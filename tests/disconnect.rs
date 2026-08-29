//! Integration tests for disconnect cleanup (ticket 04b): a client vanishing
//! before the terminal frame must not orphan its child. Raw wire frames are
//! spoken directly to the real `ncap-server`; a vanished client cannot be
//! told anything anymore, so children announce the disconnect-TERM by writing
//! marker files from their traps.

#[path = "common/mod.rs"]
mod common;

use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use nix_capsule::protocol::{CURRENT_VERSION, Exit, FrameCodec, Message, Request};
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tokio::time::sleep;
use tokio_util::codec::Framed;

use common::Server;

/// Upper bound on one frame-collection phase; generous enough that a red run
/// fails on the assertion, never on the harness itself.
const PHASE_LIMIT: Duration = Duration::from_secs(20);

/// How long the group has to die once the client vanished: the TERM goes out
/// as soon as the child's next output trips over the dead socket. The marker
/// is the observable proxy — the trap the TERM fires writes it.
const TERM_LIMIT: Duration = Duration::from_secs(2);

/// How long reaping has to complete once the child is provably dead.
const REAP_LIMIT: Duration = Duration::from_secs(5);

/// How long the child has to keep proving it survived its full grace: the
/// post-TERM heartbeats must keep arriving — a KILL escalation would silence
/// them.
const GRACE_LIMIT: Duration = Duration::from_secs(8);

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

/// Read frames until `done` matches one (which is included) or `limit`
/// elapses; returns everything seen. A timeout shows up as a short list, so
/// assertions name the missing frame instead of hanging the suite.
async fn read_frames_until(
    framed: &mut Raw,
    limit: Duration,
    mut done: impl FnMut(&Message) -> bool,
) -> Vec<Message> {
    let mut frames = Vec::new();
    let _ = tokio::time::timeout(limit, async {
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

/// Poll a synchronous predicate every 25 ms until it holds or `limit`
/// elapses; `false` means the deadline passed with the predicate still
/// failing.
async fn poll_until(limit: Duration, mut predicate: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + limit;
    loop {
        if predicate() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        sleep(Duration::from_millis(25)).await;
    }
}

/// Poll `marker` until it contains `needle`, via [`poll_until`].
async fn wait_for_marker(marker: &Path, needle: &str, limit: Duration) -> bool {
    poll_until(limit, || {
        fs::read_to_string(marker).is_ok_and(|content| content.contains(needle))
    })
    .await
}

/// The script for a child that announces READY, ticks every 300 ms, and
/// announces its own death in `marker` from the TERM trap the disconnect
/// fires, then exits.
fn trapping_ticker_script(marker: &Path) -> String {
    format!(
        "trap 'echo gone >> {}; exit 0' TERM; echo READY; \
         while true; do echo tick; sleep 0.3; done",
        marker.display()
    )
}

/// Send `script` as a request, wait for the child to announce `ready` on
/// stdout, then drop the connection abruptly — the client vanishes before
/// any terminal frame.
async fn request_and_vanish(server: &Server, script: &str, ready: &str) {
    let mut framed = server.raw().await;
    send_request(&mut framed, server.path(), script).await;
    read_frames_until(&mut framed, PHASE_LIMIT, |message| {
        matches!(message, Message::Stdout(bytes) if String::from_utf8_lossy(bytes).contains(ready))
    })
    .await;
    drop(framed);
}

/// Pids of zombie processes whose parent is `server_pid`, scanned straight
/// from `/proc`: a child the server never reaped stays visible here in state
/// `Z` forever, so an empty result means nothing is left to reap.
fn zombies_under(server_pid: u32) -> Vec<u32> {
    let mut zombies = Vec::new();
    let Ok(entries) = fs::read_dir("/proc") else {
        return zombies;
    };
    for entry in entries.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let Ok(pid) = name.parse::<u32>() else {
            continue;
        };
        let Ok(stat) = fs::read_to_string(Path::new("/proc").join(&name).join("stat")) else {
            continue;
        };
        // `comm` may carry spaces and parens; the fixed fields resume after
        // the last `)`. State is field 3, ppid field 4.
        let Some((_, rest)) = stat.rsplit_once(')') else {
            continue;
        };
        let mut fields = rest.split_whitespace();
        let state = fields.next().unwrap_or_default();
        let ppid = fields.next().unwrap_or_default();
        if state == "Z" && ppid == server_pid.to_string() {
            zombies.push(pid);
        }
    }
    zombies
}

// ------------------------------------------------------------ abrupt disconnect

#[tokio::test(flavor = "multi_thread")]
async fn abrupt_full_close_terms_the_group_and_the_next_connection_still_works() {
    let server = Server::builder().start().await;
    let marker = server.path().join("term-marker");
    request_and_vanish(&server, &trapping_ticker_script(&marker), "READY").await;

    assert!(
        wait_for_marker(&marker, "gone", TERM_LIMIT).await,
        "the group outlived {TERM_LIMIT:?} after the client vanished"
    );

    let mut second = server.raw().await;
    send_request(&mut second, server.path(), "echo next").await;
    let frames = read_frames_until(&mut second, PHASE_LIMIT, |message| {
        matches!(message, Message::Exit(_) | Message::Error(_))
    })
    .await;
    server.stop();

    assert_eq!(
        terminal_of(&frames),
        Some(&Message::Exit(Exit {
            code: Some(0),
            signal: None,
        })),
        "the next connection must work untouched: frames={frames:?}"
    );
    assert!(stdout_of(&frames).contains("next"), "frames={frames:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn disconnect_takes_down_the_whole_group_including_a_spawned_grandchild() {
    let server = Server::builder().start().await;
    let marker = server.path().join("group-marker");
    // Both shells announce their own death from a TERM trap; the grandchild's
    // line can only come from a TERM it received itself — a kill of the child
    // alone would leave the grandchild to die silently of SIGPIPE instead.
    let script = format!(
        "trap 'echo child-gone >> {}; exit 0' TERM; \
         ( trap 'echo grandchild-gone >> {}; exit 0' TERM; \
           while true; do echo tick-gc; sleep 0.3; done ) & \
         echo READY; wait",
        marker.display(),
        marker.display()
    );
    request_and_vanish(&server, &script, "READY").await;

    let child_gone = wait_for_marker(&marker, "child-gone", TERM_LIMIT).await;
    let grandchild_gone = wait_for_marker(&marker, "grandchild-gone", TERM_LIMIT).await;
    let recorded = fs::read_to_string(&marker).unwrap_or_default();
    server.stop();

    assert!(child_gone, "the child was not TERMed: marker={recorded:?}");
    assert!(
        grandchild_gone,
        "the grandchild was not TERMed with the group: marker={recorded:?}"
    );
}

// ----------------------------------------------------------------------- reaping

#[tokio::test(flavor = "multi_thread")]
async fn the_disconnect_termed_child_is_reaped_leaving_no_zombie_under_the_server() {
    let server = Server::builder().start().await;
    let marker = server.path().join("reap-marker");
    request_and_vanish(&server, &trapping_ticker_script(&marker), "READY").await;

    assert!(
        wait_for_marker(&marker, "gone", TERM_LIMIT).await,
        "the group outlived {TERM_LIMIT:?} after the client vanished"
    );

    let server_pid = server.pid().expect("real server has a pid");
    let reaped = poll_until(REAP_LIMIT, || zombies_under(server_pid).is_empty()).await;
    let left_behind = zombies_under(server_pid);
    server.stop();

    assert!(
        reaped,
        "zombies remain under the server (pid {server_pid}): {left_behind:?}"
    );
}

// ------------------------------------------------- EOF is never a disconnect

#[tokio::test(flavor = "multi_thread")]
async fn write_half_only_close_is_stdin_eof_and_lets_the_child_finish() {
    let server = Server::builder().start().await;
    let mut framed = server.raw().await;
    send_request(&mut framed, server.path(), "cat; echo done").await;
    framed
        .send(
            Message::Stdin(b"hello\n".to_vec())
                .into_frame()
                .expect("encode stdin"),
        )
        .await
        .expect("send stdin");
    // Write-half close only: stdin EOF for the child, the read half stays
    // open to receive the child's remainder.
    framed
        .get_mut()
        .shutdown()
        .await
        .expect("shutdown write half");

    let frames = read_frames_until(&mut framed, PHASE_LIMIT, |message| {
        matches!(message, Message::Exit(_) | Message::Error(_))
    })
    .await;
    server.stop();

    assert_eq!(
        terminal_of(&frames),
        Some(&Message::Exit(Exit {
            code: Some(0),
            signal: None,
        })),
        "EOF must never kill the child: frames={frames:?}"
    );
    let stdout = stdout_of(&frames);
    assert!(stdout.contains("hello"), "stdout={stdout:?}");
    assert!(stdout.contains("done"), "stdout={stdout:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn empty_stdin_frame_is_stdin_eof_and_keeps_the_connection_open() {
    let server = Server::builder().start().await;
    let mut framed = server.raw().await;
    send_request(&mut framed, server.path(), "cat; echo done").await;
    for payload in [b"hello\n".to_vec(), Vec::new()] {
        framed
            .send(Message::Stdin(payload).into_frame().expect("encode stdin"))
            .await
            .expect("send stdin");
    }
    // No write-half close here: the empty frame alone must deliver the EOF,
    // leaving the connection open for later `Signal` frames (ticket 04c).

    let frames = read_frames_until(&mut framed, PHASE_LIMIT, |message| {
        matches!(message, Message::Exit(_) | Message::Error(_))
    })
    .await;
    server.stop();

    assert_eq!(
        terminal_of(&frames),
        Some(&Message::Exit(Exit {
            code: Some(0),
            signal: None,
        })),
        "the empty Stdin frame must deliver stdin EOF: frames={frames:?}"
    );
    let stdout = stdout_of(&frames);
    assert!(stdout.contains("hello"), "stdout={stdout:?}");
    assert!(stdout.contains("done"), "stdout={stdout:?}");
}

// --------------------------------------------------- TERM-trapping survivors

#[tokio::test(flavor = "multi_thread")]
async fn a_term_trapping_child_holds_only_its_own_connection_and_others_keep_working() {
    let server = Server::builder().start().await;
    let marker = server.path().join("trap-marker");
    // The trap runs on TERM but does not exit, and silences stdout
    // (`exec 1>/dev/null`) so the remaining ticks cannot SIGPIPE the child
    // away once the server drops the pipe. The heartbeats it then stamps
    // into the marker are the no-escalation witness: a server that KILLed
    // after the TERM would stop them early.
    let script = format!(
        "trap 'echo trapped >> {}; exec 1>/dev/null' TERM; echo A-READY; \
         for i in 1 2 3 4 5 6 7 8 9 10; do echo tick-a; sleep 0.3; done; \
         for i in 1 2 3 4 5; do sleep 0.5; echo alive-$i >> {}; done",
        marker.display(),
        marker.display()
    );
    request_and_vanish(&server, &script, "A-READY").await;

    assert!(
        wait_for_marker(&marker, "trapped", TERM_LIMIT).await,
        "the child never received (or never survived) the disconnect TERM"
    );

    // The first child is still alive here, its connection task holding; the
    // server must still serve other connections.
    let mut second = server.raw().await;
    send_request(&mut second, server.path(), "echo hello").await;
    let frames = read_frames_until(&mut second, PHASE_LIMIT, |message| {
        matches!(message, Message::Exit(_) | Message::Error(_))
    })
    .await;
    // Heartbeats stamped well past the TERM prove the server never
    // escalated to SIGKILL: the grace after the TERM is the child's. The
    // wait and read precede `server.stop()`, whose teardown takes the
    // tempdir — marker included — with it.
    let full_grace = wait_for_marker(&marker, "alive-5", GRACE_LIMIT).await;
    let recorded = fs::read_to_string(&marker).unwrap_or_default();
    server.stop();

    assert_eq!(
        terminal_of(&frames),
        Some(&Message::Exit(Exit {
            code: Some(0),
            signal: None,
        })),
        "other connections must be unaffected: frames={frames:?}"
    );
    assert!(stdout_of(&frames).contains("hello"), "frames={frames:?}");

    assert!(
        full_grace,
        "the child's grace was cut short — the server escalated past TERM: marker={recorded:?}"
    );
}
