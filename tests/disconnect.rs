//! Integration tests for disconnect cleanup (ticket 04b): a client vanishing
//! before the terminal frame must not orphan its child. Raw wire frames are
//! spoken directly to the real `ncap-server`; a vanished client cannot be
//! told anything anymore, so children announce the disconnect-TERM by writing
//! marker files from their traps.

mod common;

use std::fs;

use futures_util::SinkExt;
use test_case::test_case;
use tokio::io::AsyncWriteExt;

use nix_capsule::protocol::Message;
use common::{
    Server,
    child::{
        WAIT_PHASE, WAIT_ROOMY, WAIT_TIGHT, poll_until, request_and_vanish,
        second_connection_succeeds, vanish_and_confirm_gone, wait_for_marker, zombies_under,
    },
    probe::{assert_clean_exit, read_until_terminal, send_request, stdout_of},
    script::{group_trap_script, surviving_trap_script, trapping_ticker_script},
};

#[tokio::test(flavor = "multi_thread")]
async fn full_close_terms_group_and_next_connection_succeeds() {
    let server = Server::builder().start().await;
    let marker = server.path().join("term-marker");
    vanish_and_confirm_gone(
        &server,
        &trapping_ticker_script(&marker),
        "READY",
        &marker,
        WAIT_TIGHT,
    )
    .await;

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
async fn disconnect_terms_whole_group_including_grandchild() {
    let server = Server::builder().start().await;
    let marker = server.path().join("group-marker");
    // Both shells announce their own death from a TERM trap; the grandchild's
    // line can only come from a TERM it received itself — a kill of the child
    // alone would leave the grandchild to die silently of SIGPIPE instead.
    let script = group_trap_script(&marker);
    request_and_vanish(&server, &script, "READY").await;

    let child_gone = wait_for_marker(&marker, "child-gone", WAIT_TIGHT).await;
    let grandchild_gone = wait_for_marker(&marker, "grandchild-gone", WAIT_TIGHT).await;
    let recorded = fs::read_to_string(&marker).unwrap_or_default();
    server.stop();

    assert!(child_gone, "the child was not TERMed: marker={recorded:?}");
    assert!(
        grandchild_gone,
        "the grandchild was not TERMed with the group: marker={recorded:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn disconnect_termed_child_is_reaped_leaving_no_zombie() {
    let server = Server::builder().start().await;
    let marker = server.path().join("reap-marker");
    vanish_and_confirm_gone(
        &server,
        &trapping_ticker_script(&marker),
        "READY",
        &marker,
        WAIT_TIGHT,
    )
    .await;

    let server_pid = server.pid().expect("real server has a pid");
    let reaped = poll_until(WAIT_ROOMY, || zombies_under(server_pid).is_empty()).await;
    let left_behind = zombies_under(server_pid);
    server.stop();

    assert!(
        reaped,
        "zombies remain under the server (pid {server_pid}): {left_behind:?}"
    );
}

/// How stdin EOF reaches the child: the write half closes while the read
/// half stays open, or an empty `Stdin` frame arrives keeping the connection
/// open for later `Signal` frames.
enum StdinEof {
    WriteHalfShutdown,
    EmptyFrame,
}

#[test_case(StdinEof::WriteHalfShutdown ; "write_half_shutdown")]
#[test_case(StdinEof::EmptyFrame ; "empty_frame")]
#[tokio::test(flavor = "multi_thread")]
async fn stdin_eof_is_never_a_disconnect_and_lets_the_child_finish(eof_style: StdinEof) {
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
    match eof_style {
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

#[tokio::test(flavor = "multi_thread")]
async fn term_trapping_child_holds_only_own_connection_and_others_keep_working() {
    let server = Server::builder().start().await;
    let marker = server.path().join("trap-marker");
    // The trap runs on TERM but does not exit, and silences stdout
    // (`exec 1>/dev/null`) so the remaining ticks cannot SIGPIPE the child
    // away once the server drops the pipe. The heartbeats it then stamps
    // into the marker are the no-escalation witness: a server that KILLed
    // after the TERM would stop them early.
    let script = surviving_trap_script(&marker);
    request_and_vanish(&server, &script, "A-READY").await;

    assert!(
        wait_for_marker(&marker, "trapped", WAIT_TIGHT).await,
        "the child never received (or never survived) the disconnect TERM"
    );

    // The first child is still alive here, its connection task holding.
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
    let full_grace = wait_for_marker(&marker, "alive-5", WAIT_PHASE).await;
    let recorded = fs::read_to_string(&marker).unwrap_or_default();
    server.stop();

    assert!(
        full_grace,
        "the child's grace was cut short — the server escalated past TERM: marker={recorded:?}"
    );
}
