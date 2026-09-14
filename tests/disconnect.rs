//! Integration tests for disconnect cleanup (ticket 04b): a client vanishing
//! before the terminal frame must not orphan its child. Raw wire frames are
//! spoken directly to the real `ncap-server`; a vanished client cannot be
//! told anything anymore, so children announce the disconnect-TERM by writing
//! marker files from their traps.

mod common;

use std::fs;

use futures_util::SinkExt;
use nix_capsule::protocol::Message;
use test_case::test_case;
use tokio::io::AsyncWriteExt;

use common::Server;
use common::probe::{
    DISCONNECT_TERM_LIMIT, GRACE_LIMIT, REAP_LIMIT, Raw, assert_clean_exit, poll_until,
    read_until_terminal, request_and_vanish, second_connection_succeeds, send_request, stdout_of,
    wait_for_marker, zombies_under,
};
use common::script::{group_trap_script, trapping_ticker_script};

// ------------------------------------------------------------ abrupt disconnect

#[tokio::test(flavor = "multi_thread")]
async fn abrupt_full_close_terms_the_group_and_the_next_connection_still_works() {
    let server = Server::builder().start().await;
    let marker = server.path().join("term-marker");
    request_and_vanish(&server, &trapping_ticker_script(&marker), "READY").await;

    assert!(
        wait_for_marker(&marker, "gone", DISCONNECT_TERM_LIMIT).await,
        "the group outlived {DISCONNECT_TERM_LIMIT:?} after the client vanished"
    );

    second_connection_succeeds(
        &server,
        "echo next",
        "next",
        "the next connection must work untouched",
    )
    .await;
    server.stop();
}
#[tokio::test(flavor = "multi_thread")]
async fn disconnect_takes_down_the_whole_group_including_a_spawned_grandchild() {
    let server = Server::builder().start().await;
    let marker = server.path().join("group-marker");
    // Both shells announce their own death from a TERM trap; the grandchild's
    // line can only come from a TERM it received itself — a kill of the child
    // alone would leave the grandchild to die silently of SIGPIPE instead.
    let script = group_trap_script(&marker);
    request_and_vanish(&server, &script, "READY").await;

    let child_gone = wait_for_marker(&marker, "child-gone", DISCONNECT_TERM_LIMIT).await;
    let grandchild_gone = wait_for_marker(&marker, "grandchild-gone", DISCONNECT_TERM_LIMIT).await;
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
        wait_for_marker(&marker, "gone", DISCONNECT_TERM_LIMIT).await,
        "the group outlived {DISCONNECT_TERM_LIMIT:?} after the client vanished"
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

/// How stdin EOF reaches the child.
enum StdinEof {
    /// The write half closes; the read half stays open to receive the
    /// child's remainder.
    WriteHalfShutdown,
    /// An empty Stdin frame arrives; no write-half close, so the connection
    /// stays open for later `Signal` frames (ticket 04c).
    EmptyFrame,
}

#[test_case(StdinEof::WriteHalfShutdown ; "write_half_only_close_is_stdin_eof_and_lets_the_child_finish")]
#[test_case(StdinEof::EmptyFrame ; "empty_stdin_frame_is_stdin_eof_and_keeps_the_connection_open")]
#[tokio::test(flavor = "multi_thread")]
async fn stdin_eof_is_never_a_disconnect_and_lets_the_child_finish(style: StdinEof) {
    let server = Server::builder().start().await;
    let mut framed: Raw = server.raw().await;
    send_request(&mut framed, server.path(), "cat; echo done").await;
    framed
        .send(
            Message::Stdin(b"hello\n".to_vec())
                .into_frame()
                .expect("encode stdin"),
        )
        .await
        .expect("send stdin");
    match style {
        StdinEof::WriteHalfShutdown => {
            framed
                .get_mut()
                .shutdown()
                .await
                .expect("shutdown write half");
        }
        StdinEof::EmptyFrame => {
            framed
                .send(
                    Message::Stdin(Vec::new())
                        .into_frame()
                        .expect("encode stdin"),
                )
                .await
                .expect("send empty stdin");
        }
    }

    let frames = read_until_terminal(&mut framed).await;
    server.stop();

    assert_clean_exit(&frames, "EOF must never kill the child");
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
        wait_for_marker(&marker, "trapped", DISCONNECT_TERM_LIMIT).await,
        "the child never received (or never survived) the disconnect TERM"
    );

    // The first child is still alive here, its connection task holding; the
    // server must still serve other connections.
    second_connection_succeeds(
        &server,
        "echo hello",
        "hello",
        "other connections must be unaffected",
    )
    .await;
    // Heartbeats stamped well past the TERM prove the server never
    // escalated to SIGKILL: the grace after the TERM is the child's. The
    // wait and read precede `server.stop()`, whose teardown takes the
    // tempdir — marker included — with it.
    let full_grace = wait_for_marker(&marker, "alive-5", GRACE_LIMIT).await;
    let recorded = fs::read_to_string(&marker).unwrap_or_default();
    server.stop();

    assert!(
        full_grace,
        "the child's grace was cut short — the server escalated past TERM: marker={recorded:?}"
    );
}
