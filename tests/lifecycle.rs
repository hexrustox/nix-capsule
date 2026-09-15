//! Integration tests for server startup/shutdown hygiene (ticket 05): the
//! startup probe against a live or stale socket, the per-run log file, and
//! the orderly SIGTERM/SIGINT shutdown that stops clients at 143, TERMs
//! every child's process group, drains within `--timeout`, and removes the
//! socket file.

mod common;

use std::fs;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use nix_capsule::protocol::Message;
use test_case::test_case;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixListener;
use tokio::time::sleep;

use common::assert::{assert_announced, assert_orderly_shutdown};
use common::probe::{
    BAIL_LIMIT, DRAIN_DEADLINE, PHASE_LIMIT, SHELL_BODY, SHUTDOWN_TERM_LIMIT, assert_clean_exit,
    read_frames_until, read_until_stdout_contains, read_until_terminal, send_request,
    wait_for_flag, wait_for_marker,
};
use common::script::shutdown_group_trap_script;
use common::{Client, Server, WAIT_LIMIT, bin_path, wait_bounded};

// ------------------------------------------------- client reactions (ticket 05)

#[test_case(vec![Message::ServerStopping] ; "a_server_stopping_frame_bails_the_client_at_143")]
#[test_case(vec![] ; "a_clean_close_without_a_terminal_frame_bails_the_client_at_143")]
#[tokio::test(flavor = "multi_thread")]
async fn a_terminal_response_bails_the_client_at_143(respond: Vec<Message>) {
    let server = Server::builder().respond(respond).start().await;
    let out = server.client().run(&["echo", "hi"]);
    server.stop();

    assert_eq!(out.status.code(), Some(143), "stderr={}", out.stderr);
}

#[tokio::test(flavor = "multi_thread")]
async fn garbled_traffic_stays_a_transport_failure_at_1() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("garbled.sock");
    let listener = UnixListener::bind(&socket).expect("bind garbled listener");
    // The writer holds the connection open until the client has exited, so
    // the garbage is parsed before any clean close could win the race; the
    // client-side bounded poll below is the deadline.
    let (release, released) = tokio::sync::oneshot::channel::<()>();
    let writer = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept");
        // One header-shaped frame: an unknown tag declaring a bogus length.
        stream
            .write_all(&[0xDE, 0xAD, 0xBE, 0xEF, 0x00])
            .await
            .expect("write garbage");
        let _ = tokio::time::timeout(WAIT_LIMIT, released).await;
    });
    let mut client = Client::at(&socket).spawn(&["echo", "hi"]);
    let deadline = Instant::now() + WAIT_LIMIT;
    while client.try_wait().is_none() {
        assert!(
            Instant::now() < deadline,
            "the garbled client did not exit within {WAIT_LIMIT:?}"
        );
        sleep(Duration::from_millis(20)).await;
    }
    release.send(()).expect("release writer");
    writer.await.expect("garbage writer");
    let out = client.wait();

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
        .arg("--log-level")
        .arg("debug")
        .stdout(Stdio::null())
        .stderr(stderr_log)
        .spawn()
        .expect("spawn second server");
    let status = wait_bounded(&mut second, PHASE_LIMIT, "the second server");
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
    let mut server = Server::builder()
        .timeout(DRAIN_DEADLINE.as_secs())
        .start()
        .await;
    // The child ignores TERM so a client that wrongly waited for its child
    // would only be freed by the drain deadline, never by the child itself.
    let client = server.client().cwd(server.path()).spawn(&[
        "sh",
        "-c",
        &format!("trap '' TERM; touch ready.flag; exec sleep {SHELL_BODY}"),
    ]);
    wait_for_flag(&server, "ready.flag").await;
    server.signal(sig);

    let client_status = client.wait_within(BAIL_LIMIT, "the client to bail").status;
    let server_status = server.wait_for_exit().expect("real server");
    let socket_gone = !server.socket().exists();
    server.stop();

    assert_eq!(
        client_status.code(),
        Some(143),
        "the client must bail at 143"
    );
    assert_orderly_shutdown(&server_status, socket_gone);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_connection_finishing_inside_the_grace_window_completes_normally() {
    let mut server = Server::builder().start().await;
    let mut framed = server.raw().await;
    send_request(
        &mut framed,
        server.path(),
        &format!("trap 'exit 0' TERM; echo READY; sleep {SHELL_BODY}"),
    )
    .await;
    read_until_stdout_contains(&mut framed, "READY").await;

    let status = server.terminate(libc::SIGTERM).expect("real server");
    // The terminal frames are already buffered on this socket: the
    // connection ran to completion before the drain closed it.
    let frames = read_until_terminal(&mut framed).await;
    let closed_cleanly = framed.next().await.is_none();
    let socket_gone = !server.socket().exists();
    server.stop();

    assert_orderly_shutdown(&status, socket_gone);
    assert_announced(&frames, "the shutdown must be announced");
    assert_clean_exit(&frames, "the child's own exit must complete normally");
    assert!(
        closed_cleanly,
        "the server must close cleanly after the terminal frame"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_terms_the_whole_group_including_grandchildren() {
    let mut server = Server::builder().start().await;
    let marker = server.path().join("shutdown-marker");
    // Both shells announce their own death from a TERM trap; the
    // grandchild's line can only come from a TERM it received itself — a
    // kill of the child alone would leave the grandchild running.
    let script = shutdown_group_trap_script(&marker);
    let mut framed = server.raw().await;
    send_request(&mut framed, server.path(), &script).await;
    read_until_stdout_contains(&mut framed, "READY").await;

    let status = server.terminate(libc::SIGTERM).expect("real server");
    let child_gone = wait_for_marker(&marker, "child-gone", SHUTDOWN_TERM_LIMIT).await;
    let grandchild_gone = wait_for_marker(&marker, "grandchild-gone", SHUTDOWN_TERM_LIMIT).await;
    let recorded = fs::read_to_string(&marker).unwrap_or_default();
    let socket_gone = !server.socket().exists();
    server.stop();

    assert_orderly_shutdown(&status, socket_gone);
    assert!(child_gone, "the child was not TERMed: marker={recorded:?}");
    assert!(
        grandchild_gone,
        "the grandchild was not TERMed with the group: marker={recorded:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_connection_past_the_deadline_is_dropped_when_the_drain_expires() {
    let mut server = Server::builder()
        .timeout(DRAIN_DEADLINE.as_secs())
        .start()
        .await;
    let mut framed = server.raw().await;
    // The child ignores TERM, so the connection can only end when the
    // drain deadline expires.
    send_request(
        &mut framed,
        server.path(),
        &format!("trap '' TERM; touch ready.flag; exec sleep {SHELL_BODY}"),
    )
    .await;
    wait_for_flag(&server, "ready.flag").await;

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

    assert_orderly_shutdown(&status, socket_gone);
    assert!(elapsed >= DRAIN_DEADLINE);
    assert!(
        elapsed < DRAIN_DEADLINE + Duration::from_secs(1),
        "the drain must expire at the deadline, not wait out the child: {elapsed:?}"
    );
    assert_announced(&announced, "the shutdown must be announced before the drop");
    assert!(dropped, "the overdue connection must be dropped");
}
