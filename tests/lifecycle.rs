//! Integration tests for server startup/shutdown hygiene (ticket 05): the
//! startup probe against a live or stale socket, the per-run log file, and
//! the orderly SIGTERM/SIGINT shutdown that stops clients at 143, TERMs
//! every child's process group, drains within `--timeout`, and removes the
//! socket file.

#[path = "common/mod.rs"]
mod common;

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use nix_capsule::protocol::{CURRENT_VERSION, Exit, FrameCodec, Message, Request};
use test_case::test_case;
use tokio::io::AsyncWriteExt;
use tokio::net::{UnixListener, UnixStream};
use tokio::time::sleep;
use tokio_util::codec::Framed;

use common::{Client, Server, bin_path};

/// Upper bound on one phase; a red run fails on the assertion, never on the
/// harness itself.
const PHASE_LIMIT: Duration = Duration::from_secs(20);

/// How long a group has to die once the shutdown signal landed: the TERM is
/// sent immediately, and the trap it fires writes the marker.
const TERM_LIMIT: Duration = Duration::from_secs(5);

/// How soon the client must bail once the shutdown signal lands: the
/// `ServerStopping` frame precedes the drain, so this outruns any
/// `--timeout` a test configures.
const BAIL_LIMIT: Duration = Duration::from_millis(1500);

type Raw = Framed<UnixStream, FrameCodec>;

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

/// The terminal frame, if one arrived.
fn terminal_of(frames: &[Message]) -> Option<&Message> {
    frames
        .iter()
        .find(|message| matches!(message, Message::Exit(_) | Message::Error(_)))
}

// ------------------------------------------------- client reactions (ticket 05)

#[tokio::test(flavor = "multi_thread")]
async fn a_server_stopping_frame_bails_the_client_at_143() {
    let server = Server::builder()
        .respond(vec![Message::ServerStopping])
        .start()
        .await;
    let out = server.client().run(&["echo", "hi"]);
    server.stop();

    assert_eq!(out.status.code(), Some(143), "stderr={}", out.stderr);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_clean_close_without_a_terminal_frame_bails_the_client_at_143() {
    let server = Server::builder().respond(vec![]).start().await;
    let out = server.client().run(&["echo", "hi"]);
    server.stop();

    assert_eq!(out.status.code(), Some(143), "stderr={}", out.stderr);
}

#[tokio::test(flavor = "multi_thread")]
async fn garbled_traffic_stays_a_transport_failure_at_1() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("garbled.sock");
    let listener = UnixListener::bind(&socket).expect("bind garbled listener");
    let writer = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept");
        // One header-shaped frame: an unknown tag declaring a bogus length.
        stream
            .write_all(&[0xDE, 0xAD, 0xBE, 0xEF, 0x00])
            .await
            .expect("write garbage");
        // Hold the connection briefly so the client reads the bytes before
        // the clean close could win the race.
        sleep(Duration::from_millis(200)).await;
    });
    let out = Client::at(&socket).run(&["echo", "hi"]);
    writer.await.expect("garbage writer");

    assert_eq!(out.status.code(), Some(1), "stderr={}", out.stderr);
}

// --------------------------------------------------------- startup (ticket 05)

#[tokio::test(flavor = "multi_thread")]
async fn a_per_run_epoch_stamped_log_file_appears_in_the_log_dir() {
    let server = Server::builder().start().await;
    let entries: Vec<String> = match std::fs::read_dir(server.path().join("logs")) {
        Ok(entries) => entries
            .map(|entry| {
                entry
                    .expect("log dir entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect(),
        Err(err) => panic!("log dir never appeared: {err}"),
    };
    server.stop();

    assert_eq!(entries.len(), 1, "entries={entries:?}");
    let epoch = entries[0]
        .strip_prefix("ncap-server-")
        .and_then(|rest| rest.strip_suffix(".log"))
        .unwrap_or_else(|| panic!("entry {} is not epoch-stamped", entries[0]));
    assert!(
        !epoch.is_empty() && epoch.bytes().all(|byte| byte.is_ascii_digit()),
        "entry {} carries a non-numeric epoch",
        entries[0]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_live_socket_refuses_startup_naming_the_path_and_leaves_the_owner_untouched() {
    let server = Server::builder().start().await;
    let socket = server.socket().to_path_buf();

    let dir = tempfile::tempdir().expect("tempdir");
    let stderr_log =
        fs::File::create(dir.path().join("second-stderr.log")).expect("stderr capture");
    let mut second = Command::new(bin_path("ncap-server"))
        .arg("--socket")
        .arg(&socket)
        .arg("--log-dir")
        .arg(dir.path().join("logs"))
        .arg("--timeout")
        .arg("10")
        .stdout(Stdio::null())
        .stderr(stderr_log)
        .spawn()
        .expect("spawn second server");
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        match second.try_wait().expect("poll second server") {
            Some(status) => break status,
            None if Instant::now() >= deadline => {
                let _ = second.kill();
                let _ = second.wait();
                panic!("the second server never refused the live socket");
            }
            None => thread::sleep(Duration::from_millis(20)),
        }
    };
    let stderr = fs::read_to_string(dir.path().join("second-stderr.log")).expect("stderr");
    let out = server.client().run(&["echo", "still-owns"]);
    server.stop();

    assert_eq!(
        status.code(),
        Some(1),
        "the second server must refuse, not serve"
    );
    assert!(
        stderr.contains(socket.to_str().expect("utf-8 socket path")),
        "the refusal must name the path: stderr={stderr}"
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "the owning server must be untouched: stderr={}",
        out.stderr
    );
    assert_eq!(out.stdout.trim_end(), "still-owns");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stale_socket_file_is_removed_and_the_bind_succeeds() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("stale.sock");
    let listener = UnixListener::bind(&socket).expect("bind stale socket");
    drop(listener); // a crashed server: the file remains, nothing listens
    assert!(
        socket.exists(),
        "the stale file must predate the new server"
    );

    let server = Server::builder().socket_path(&socket).start().await;
    let out = server.client().run(&["echo", "replaced"]);
    server.stop();

    assert_eq!(out.status.code(), Some(0), "stderr={}", out.stderr);
    assert_eq!(out.stdout.trim_end(), "replaced");
}

// -------------------------------------------------------- shutdown (ticket 05)

#[test_case(libc::SIGTERM ; "sigterm")]
#[test_case(libc::SIGINT ; "sigint")]
#[tokio::test(flavor = "multi_thread")]
async fn a_shutdown_signal_bails_the_client_at_143_immediately_and_removes_the_socket(sig: i32) {
    let mut server = Server::builder().timeout(3).start().await;
    // The child ignores TERM so a client that wrongly waited for its child
    // would only be freed by the drain deadline, never by the child itself.
    let mut client = server.client().cwd(server.path()).spawn(&[
        "sh",
        "-c",
        "trap '' TERM; touch ready.flag; exec sleep 30",
    ]);
    wait_for_flag(&server, "ready.flag");
    server.signal(sig);

    let deadline = Instant::now() + BAIL_LIMIT;
    let client_status = loop {
        if let Some(status) = client.try_wait() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "the client never bailed within {BAIL_LIMIT:?} of the signal"
        );
        thread::sleep(Duration::from_millis(20));
    };

    let server_status = server.wait_for_exit().expect("real server");
    let socket_gone = !server.socket().exists();
    server.stop();

    assert_eq!(
        client_status.code(),
        Some(143),
        "the client must bail at 143"
    );
    assert_eq!(
        server_status.code(),
        Some(0),
        "an orderly shutdown exits 0, not by signal"
    );
    assert!(socket_gone, "the socket file must be gone after exit");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_connection_finishing_inside_the_grace_window_completes_normally() {
    let mut server = Server::builder().start().await;
    let mut framed = server.raw().await;
    send_request(
        &mut framed,
        server.path(),
        "trap 'exit 0' TERM; echo READY; sleep 30",
    )
    .await;
    read_frames_until(&mut framed, PHASE_LIMIT, |message| {
        matches!(message, Message::Stdout(bytes) if String::from_utf8_lossy(bytes).contains("READY"))
    })
    .await;

    let status = server.terminate(libc::SIGTERM).expect("real server");
    // The terminal frames are already buffered on this socket: the
    // connection ran to completion before the drain closed it.
    let frames = read_frames_until(&mut framed, PHASE_LIMIT, |message| {
        matches!(message, Message::Exit(_) | Message::Error(_))
    })
    .await;
    let closed_cleanly = framed.next().await.is_none();
    let socket_gone = !server.socket().exists();
    server.stop();

    assert_eq!(
        status.code(),
        Some(0),
        "an orderly shutdown exits 0, not by signal"
    );
    assert!(
        frames
            .iter()
            .any(|message| matches!(message, Message::ServerStopping)),
        "the shutdown must be announced: frames={frames:?}"
    );
    assert_eq!(
        terminal_of(&frames),
        Some(&Message::Exit(Exit {
            code: Some(0),
            signal: None,
        })),
        "the child's own exit must complete normally: frames={frames:?}"
    );
    assert!(
        closed_cleanly,
        "the server must close cleanly after the terminal frame"
    );
    assert!(socket_gone, "the socket file must be gone after exit");
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_terms_the_whole_group_including_grandchildren() {
    let mut server = Server::builder().start().await;
    let marker = server.path().join("shutdown-marker");
    // Both shells announce their own death from a TERM trap; the
    // grandchild's line can only come from a TERM it received itself — a
    // kill of the child alone would leave the grandchild running.
    let script = format!(
        "trap 'echo child-gone >> {}; exit 0' TERM; \
         ( trap 'echo grandchild-gone >> {}; exit 0' TERM; \
           while true; do echo tick; sleep 0.3; done ) & \
         echo READY; wait",
        marker.display(),
        marker.display()
    );
    let mut framed = server.raw().await;
    send_request(&mut framed, server.path(), &script).await;
    read_frames_until(&mut framed, PHASE_LIMIT, |message| {
        matches!(message, Message::Stdout(bytes) if String::from_utf8_lossy(bytes).contains("READY"))
    })
    .await;

    let status = server.terminate(libc::SIGTERM).expect("real server");
    let child_gone = wait_for_marker(&marker, "child-gone", TERM_LIMIT).await;
    let grandchild_gone = wait_for_marker(&marker, "grandchild-gone", TERM_LIMIT).await;
    let recorded = fs::read_to_string(&marker).unwrap_or_default();
    let socket_gone = !server.socket().exists();
    server.stop();

    assert_eq!(
        status.code(),
        Some(0),
        "an orderly shutdown exits 0, not by signal"
    );
    assert!(child_gone, "the child was not TERMed: marker={recorded:?}");
    assert!(
        grandchild_gone,
        "the grandchild was not TERMed with the group: marker={recorded:?}"
    );
    assert!(socket_gone, "the socket file must be gone after exit");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_connection_past_the_deadline_is_dropped_when_the_drain_expires() {
    let mut server = Server::builder().timeout(1).start().await;
    let mut framed = server.raw().await;
    // The child ignores TERM, so the connection can only end when the
    // drain deadline expires.
    send_request(
        &mut framed,
        server.path(),
        "trap '' TERM; touch ready.flag; exec sleep 30",
    )
    .await;
    wait_for_flag(&server, "ready.flag");

    let started = Instant::now();
    let status = server.terminate(libc::SIGTERM).expect("real server");
    let elapsed = started.elapsed();
    let announced = read_frames_until(&mut framed, PHASE_LIMIT, |message| {
        matches!(message, Message::ServerStopping)
    })
    .await;
    let dropped = framed.next().await.is_none();
    let socket_gone = !server.socket().exists();
    server.stop();

    assert_eq!(
        status.code(),
        Some(0),
        "an orderly shutdown exits 0, not by signal"
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "the drain must expire at the deadline, not wait out the child: {elapsed:?}"
    );
    assert!(
        announced
            .iter()
            .any(|message| matches!(message, Message::ServerStopping)),
        "the shutdown must be announced before the drop: frames={announced:?}"
    );
    assert!(dropped, "the overdue connection must be dropped");
    assert!(socket_gone, "the socket file must be gone after exit");
}
