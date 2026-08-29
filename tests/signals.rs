//! Integration tests for process groups and signal delivery: ticket 04a
//! speaks raw wire frames to the real `ncap-server`; ticket 04c spawns the
//! real `ncap` client, delivers host signals to it mid-run, and awaits.

#[path = "common/mod.rs"]
mod common;

use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use nix_capsule::protocol::{CURRENT_VERSION, Exit, FrameCodec, Message, Request, SignalMsg};
use test_case::test_case;
use tokio::net::UnixStream;
use tokio::time::timeout;
use tokio_util::codec::Framed;

use common::Server;

/// Upper bound on one frame-collection phase; generous enough that a red run
/// fails on the assertion, never on the harness itself.
const PHASE_LIMIT: Duration = Duration::from_secs(20);

/// Bound for group-wide delivery: the signal must clear the whole group well
/// before a survivor's own 30-second `sleep` would end on its own.
const GROUP_LIMIT: Duration = Duration::from_secs(10);

type Raw = Framed<UnixStream, FrameCodec>;

async fn send_request(framed: &mut Raw, cwd: &Path, script: &str) {
    let request = Request {
        command: "sh".into(),
        args: vec!["-c".into(), script.into()],
        cwd: cwd.to_string_lossy().into_owned(),
        env: Vec::new(),
        version: Some(CURRENT_VERSION.into()),
    };
    framed
        .send(
            Message::Request(request)
                .into_frame()
                .expect("encode request"),
        )
        .await
        .expect("send request");
}

/// Send one `Signal` frame with `number`.
async fn send_signal(framed: &mut Raw, number: u8) {
    framed
        .send(
            Message::Signal(SignalMsg { signal: number })
                .into_frame()
                .expect("encode signal"),
        )
        .await
        .expect("send signal");
}

/// Read frames until `done` matches one (which is included) or `limit`
/// elapses; returns everything seen. A timeout shows up as a short list, so
/// assertions name the missing frame instead of hanging the suite.
async fn read_frames_until(
    framed: &mut Raw,
    limit: Duration,
    mut done: impl FnMut(&Message) -> bool,
) -> Vec<Message> {
    let mut frames = Vec::new();
    let _ = timeout(limit, async {
        while let Some(frame) = framed.next().await {
            let message = Message::from_frame(frame.expect("frame transport")).expect("decode");
            let finished = done(&message);
            frames.push(message);
            if finished {
                break;
            }
        }
    })
    .await;
    frames
}

/// All stdout bytes carried by `frames`.
fn stdout_of(frames: &[Message]) -> String {
    frames
        .iter()
        .filter_map(|message| match message {
            Message::Stdout(bytes) => Some(String::from_utf8_lossy(bytes).into_owned()),
            _ => None,
        })
        .collect()
}

/// The terminal frame, if one arrived.
fn terminal_of(frames: &[Message]) -> Option<&Message> {
    frames
        .iter()
        .find(|message| matches!(message, Message::Exit(_) | Message::Error(_)))
}

/// A failed kill or a vanished child must never surface as an `Error` frame.
fn assert_no_error_frames(frames: &[Message]) {
    assert!(
        !frames
            .iter()
            .any(|message| matches!(message, Message::Error(_))),
        "unexpected Error frames: {frames:?}"
    );
}

// ------------------------------------------------------------- process groups

#[tokio::test(flavor = "multi_thread")]
async fn child_runs_as_its_own_process_group_leader() {
    let server = Server::builder().start().await;
    let mut framed = server.raw().await;
    send_request(
        &mut framed,
        server.path(),
        "read -r _ _ _ _ pgrp _ < /proc/self/stat; echo pid=$$ pgrp=$pgrp",
    )
    .await;
    let frames = read_frames_until(&mut framed, PHASE_LIMIT, |message| {
        matches!(message, Message::Exit(_))
    })
    .await;
    server.stop();

    assert_no_error_frames(&frames);
    assert_eq!(
        terminal_of(&frames),
        Some(&Message::Exit(Exit {
            code: Some(0),
            signal: None,
        })),
        "frames={frames:?}"
    );
    let line = stdout_of(&frames).trim().to_string();
    let (pid, pgrp) = line
        .split_once(" pgrp=")
        .map(|(head, tail)| {
            (
                head.trim_start_matches("pid=").to_string(),
                tail.to_string(),
            )
        })
        .unwrap_or_else(|| panic!("child reported neither pid nor pgrp: `{line}`"));
    assert_eq!(pid, pgrp, "child must lead its own process group: `{line}`");
}

// ---------------------------------------------------------- signal forwarding

#[tokio::test(flavor = "multi_thread")]
async fn signal_term_runs_a_trap_and_the_child_exits_on_its_own() {
    let server = Server::builder().start().await;
    let mut framed = server.raw().await;
    // The trailing `sleep` bounds the red run: without signal forwarding the
    // script still ends, just without having run the trap.
    send_request(
        &mut framed,
        server.path(),
        "trap 'echo TRAPPED; exit 0' TERM; echo READY; sleep 8 & wait $!",
    )
    .await;
    read_frames_until(&mut framed, PHASE_LIMIT, |message| {
        matches!(message, Message::Stdout(bytes) if String::from_utf8_lossy(bytes).contains("READY"))
    })
    .await;
    send_signal(&mut framed, 15).await;
    let frames = read_frames_until(&mut framed, PHASE_LIMIT, |message| {
        matches!(message, Message::Exit(_) | Message::Error(_))
    })
    .await;
    server.stop();

    let stdout = stdout_of(&frames);
    assert!(
        stdout.contains("TRAPPED"),
        "trap never ran: stdout={stdout:?}"
    );
    assert_no_error_frames(&frames);
    assert_eq!(
        terminal_of(&frames),
        Some(&Message::Exit(Exit {
            code: Some(0),
            signal: None,
        })),
        "the child must end with its own exit, not signal death: frames={frames:?}"
    );
}

/// The Exit status of a shell killed by a signal, or reporting `128 + signal`
/// as its code — shells differ in how they report their own signal death.
fn died_from_signal(exit: &Exit, signal: u8) -> bool {
    exit.signal == Some(signal) || exit.code == Some(128 + signal)
}

#[tokio::test(flavor = "multi_thread")]
async fn signal_term_reaches_the_whole_group_including_grandchildren() {
    let server = Server::builder().start().await;
    let mut framed = server.raw().await;
    // The background `sleep` inherits the shell's pipes, so the server only
    // sees EOF — and can only report `Exit` — once the whole group is gone.
    // A bounded phase turns a survivor into a short, readable failure.
    //
    // This uses TERM, not INT: a non-interactive shell starts its background
    // jobs with INT and QUIT ignored, and that ignore survives exec and
    // cannot be reset by the job, so no shell-spawned grandchild can ever
    // die from INT. TERM proves the same `kill(-pgid)` delivery.
    send_request(&mut framed, server.path(), "sleep 30 & echo READY; wait").await;
    read_frames_until(&mut framed, PHASE_LIMIT, |message| {
        matches!(message, Message::Stdout(bytes) if String::from_utf8_lossy(bytes).contains("READY"))
    })
    .await;
    send_signal(&mut framed, 15).await;
    let frames = read_frames_until(&mut framed, GROUP_LIMIT, |message| {
        matches!(message, Message::Exit(_) | Message::Error(_))
    })
    .await;
    server.stop();

    assert_no_error_frames(&frames);
    match terminal_of(&frames) {
        Some(Message::Exit(exit)) => assert!(
            died_from_signal(exit, 15),
            "the shell must die from the group TERM: frames={frames:?}"
        ),
        other => panic!("no Exit within {GROUP_LIMIT:?} — the group survived: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn signal_int_runs_a_trap_and_the_child_exits_on_its_own() {
    let server = Server::builder().start().await;
    let mut framed = server.raw().await;
    send_request(
        &mut framed,
        server.path(),
        "trap 'echo TRAPPED; exit 0' INT; echo READY; sleep 8 & wait $!",
    )
    .await;
    read_frames_until(&mut framed, PHASE_LIMIT, |message| {
        matches!(message, Message::Stdout(bytes) if String::from_utf8_lossy(bytes).contains("READY"))
    })
    .await;
    send_signal(&mut framed, 2).await;
    let frames = read_frames_until(&mut framed, PHASE_LIMIT, |message| {
        matches!(message, Message::Exit(_) | Message::Error(_))
    })
    .await;
    server.stop();

    let stdout = stdout_of(&frames);
    assert!(
        stdout.contains("TRAPPED"),
        "trap never ran: stdout={stdout:?}"
    );
    assert_eq!(
        terminal_of(&frames),
        Some(&Message::Exit(Exit {
            code: Some(0),
            signal: None,
        })),
        "the child must end with its own exit, not signal death: frames={frames:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn out_of_range_signal_is_forwarded_verbatim_and_warns_without_error_frame() {
    let server = Server::builder().start().await;
    let mut framed = server.raw().await;
    // The child stays alive while the out-of-range number arrives, then ends
    // on its own — an invalid signal must not disturb it.
    send_request(&mut framed, server.path(), "echo READY; sleep 2").await;
    read_frames_until(&mut framed, PHASE_LIMIT, |message| {
        matches!(message, Message::Stdout(bytes) if String::from_utf8_lossy(bytes).contains("READY"))
    })
    .await;
    send_signal(&mut framed, 200).await;
    let frames = read_frames_until(&mut framed, PHASE_LIMIT, |message| {
        matches!(message, Message::Exit(_) | Message::Error(_))
    })
    .await;
    let stderr = server.stderr();
    server.stop();

    assert!(
        !frames
            .iter()
            .any(|message| matches!(message, Message::Error(_))),
        "a failed kill must never surface as an Error frame: {frames:?}"
    );
    assert_eq!(
        terminal_of(&frames),
        Some(&Message::Exit(Exit {
            code: Some(0),
            signal: None,
        })),
        "the connection must continue to the child's normal exit: frames={frames:?}"
    );
    assert!(
        stderr.contains("kill(-") && stderr.contains("200"),
        "the EINVAL kill must warn on the server's stderr: {stderr:?}"
    );
}

// ---------------------------------------------------- client relay (ticket 04c)

/// Poll until `name` exists in the server's tempdir — the child's cwd — or
/// panic; children write flag files as observable progress markers.
fn wait_for_flag(server: &Server, name: &str) {
    let flag = server.path().join(name);
    let deadline = Instant::now() + PHASE_LIMIT;
    while !flag.exists() {
        assert!(Instant::now() < deadline, "{name} never appeared");
        thread::sleep(Duration::from_millis(20));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn sigint_to_the_client_runs_a_trapping_childs_cleanup_and_exits_with_its_code() {
    let server = Server::builder().start().await;
    let client = server.client().cwd(server.path()).spawn(&[
        "sh",
        "-c",
        "trap 'echo CLEANUP; exit 0' INT; touch ready.flag; sleep 30",
    ]);
    wait_for_flag(&server, "ready.flag");
    client.signal(libc::SIGINT);
    let out = client.wait();
    server.stop();

    assert_eq!(out.status.code(), Some(0), "stderr={}", out.stderr);
    assert!(out.stdout.contains("CLEANUP"), "stdout={}", out.stdout);
}

#[tokio::test(flavor = "multi_thread")]
async fn sigint_with_a_non_trapping_child_dies_by_signal_and_the_client_exits_130() {
    let server = Server::builder().start().await;
    let client =
        server
            .client()
            .cwd(server.path())
            .spawn(&["sh", "-c", "touch ready.flag; sleep 30"]);
    wait_for_flag(&server, "ready.flag");
    client.signal(libc::SIGINT);
    let out = client.wait();
    server.stop();

    assert_eq!(out.status.code(), Some(130), "stderr={}", out.stderr);
}

#[test_case(true, 0 ; "trapping_child_exits_with_its_own_code")]
#[test_case(false, 143 ; "non_trapping_child_dies_by_signal")]
#[tokio::test(flavor = "multi_thread")]
async fn sigterm_is_relayed_like_sigint(traps: bool, expected: i32) {
    let server = Server::builder().start().await;
    let script = if traps {
        "trap 'echo TERM-CLEANUP; exit 0' TERM; touch ready.flag; sleep 30"
    } else {
        "touch ready.flag; sleep 30"
    };
    let client = server
        .client()
        .cwd(server.path())
        .spawn(&["sh", "-c", script]);
    wait_for_flag(&server, "ready.flag");
    client.signal(libc::SIGTERM);
    let out = client.wait();
    server.stop();

    assert_eq!(out.status.code(), Some(expected), "stderr={}", out.stderr);
    if traps {
        assert!(out.stdout.contains("TERM-CLEANUP"), "stdout={}", out.stdout);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn repeated_sigints_forward_one_frame_each() {
    let server = Server::builder().start().await;
    let client = server.client().cwd(server.path()).spawn(&[
        "sh",
        "-c",
        "trap 'c=$((c+1)); echo COUNT=$c; touch count-$c.flag' INT; touch ready.flag; while :; do sleep 0.2; done",
    ]);
    wait_for_flag(&server, "ready.flag");
    client.signal(libc::SIGINT);
    wait_for_flag(&server, "count-1.flag");
    client.signal(libc::SIGINT);
    wait_for_flag(&server, "count-2.flag");
    // End the run: the child has no TERM trap, so it dies by signal.
    client.signal(libc::SIGTERM);
    let out = client.wait();
    server.stop();

    assert_eq!(out.status.code(), Some(143), "stderr={}", out.stderr);
    assert!(out.stdout.contains("COUNT=1"), "stdout={}", out.stdout);
    assert!(out.stdout.contains("COUNT=2"), "stdout={}", out.stdout);
}

#[tokio::test(flavor = "multi_thread")]
async fn output_produced_after_the_signal_still_streams_before_the_terminal_frame() {
    let server = Server::builder().start().await;
    let client = server.client().cwd(server.path()).spawn(&[
        "sh",
        "-c",
        "trap 'echo AFTER-1; sleep 1; echo AFTER-2; exit 0' INT; touch ready.flag; sleep 30",
    ]);
    wait_for_flag(&server, "ready.flag");
    client.signal(libc::SIGINT);
    let out = client.wait();
    server.stop();

    assert_eq!(out.status.code(), Some(0), "stderr={}", out.stderr);
    assert!(out.stdout.contains("AFTER-1"), "stdout={}", out.stdout);
    assert!(out.stdout.contains("AFTER-2"), "stdout={}", out.stdout);
}
