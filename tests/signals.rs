//! Integration tests for process groups and signal delivery: ticket 04a
//! speaks raw wire frames to the real `ncap-server`; ticket 04c spawns the
//! real `ncap` client, delivers host signals to it mid-run, and awaits.

mod common;

use nix_capsule::protocol::{Exit, Message};
use test_case::test_case;

use common::{
    Server,
    assert::assert_no_error_frames,
    probe::{
        GROUP_LIMIT, PHASE_LIMIT, SHELL_BODY, SHELL_HOLD, assert_clean_exit, read_frames_until,
        ready_signal_terminal, send_request, stdout_of, terminal_of, wait_for_flag,
    },
};

#[tokio::test(flavor = "multi_thread")]
async fn child_leads_own_process_group() {
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
    assert_clean_exit(&frames, "the child must exit cleanly");
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

#[test_case(libc::SIGINT, "INT" ; "on_sigint")]
#[test_case(libc::SIGTERM, "TERM" ; "on_sigterm")]
#[tokio::test(flavor = "multi_thread")]
async fn signal_runs_trap_and_child_exits_on_own(signal: i32, signal_name: &str) {
    let server = Server::builder().start().await;
    let mut framed = server.raw().await;
    // The trailing `sleep` bounds the red run: without signal forwarding the
    // script still ends, just without having run the trap.
    let script = format!(
        "trap 'echo TRAPPED; exit 0' {signal_name}; echo READY; sleep {SHELL_HOLD} & wait $!"
    );
    let frames = ready_signal_terminal(&mut framed, &server, &script, signal, PHASE_LIMIT).await;
    server.stop();

    assert_no_error_frames(&frames);
    let stdout = stdout_of(&frames);
    assert!(
        stdout.contains("TRAPPED"),
        "trap never ran: stdout={stdout:?}"
    );
    assert_clean_exit(
        &frames,
        "the child must end with its own exit, not signal death",
    );
}

// The Exit status of a shell killed by a signal, or reporting `128 + signal`
// as its code — shells differ in how they report their own signal death.
fn died_from_signal(status: &Exit, signal: i32) -> bool {
    let signal = signal as u8;
    status.signal == Some(signal) || status.code == Some(128 + signal)
}

#[tokio::test(flavor = "multi_thread")]
async fn signal_term_reaches_whole_group_including_grandchildren() {
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
    let frames = ready_signal_terminal(
        &mut framed,
        &server,
        &format!("sleep {SHELL_BODY} & echo READY; wait"),
        libc::SIGTERM,
        GROUP_LIMIT,
    )
    .await;
    server.stop();

    assert_no_error_frames(&frames);
    match terminal_of(&frames) {
        Some(Message::Exit(exit)) => assert!(
            died_from_signal(exit, libc::SIGTERM),
            "the shell must die from the group TERM: frames={frames:?}"
        ),
        other => panic!("no Exit within {GROUP_LIMIT:?} — the group survived: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn out_of_range_signal_is_forwarded_verbatim_and_warns_without_error_frame() {
    let server = Server::builder().start().await;
    let mut framed = server.raw().await;
    // The child stays alive while the out-of-range number arrives, then ends
    // on its own — an invalid signal must not disturb it.
    let frames = ready_signal_terminal(
        &mut framed,
        &server,
        &format!("echo READY; sleep {SHELL_HOLD}"),
        200,
        PHASE_LIMIT,
    )
    .await;
    let stderr = server.stderr();
    server.stop();

    assert_no_error_frames(&frames);
    assert_clean_exit(
        &frames,
        "the connection must continue to the child's normal exit",
    );
    assert!(
        stderr.contains("kill(-") && stderr.contains("200"),
        "the EINVAL kill must warn on the server's stderr: {stderr:?}"
    );
}

#[test_case(libc::SIGINT, true, 0 ; "sigint_trapping_child_runs_cleanup_and_exits_0")]
#[test_case(libc::SIGINT, false, 130 ; "sigint_non_trapping_child_exits_130")]
#[test_case(libc::SIGTERM, true, 0 ; "sigterm_trapping_child_runs_cleanup_and_exits_0")]
#[test_case(libc::SIGTERM, false, 143 ; "sigterm_non_trapping_child_exits_143")]
#[tokio::test(flavor = "multi_thread")]
async fn relayed_signal_determines_client_exit_code(
    signal: i32,
    traps_signal: bool,
    expected_code: i32,
) {
    let server = Server::builder().start().await;
    let script = if traps_signal {
        format!(
            "trap 'echo CLEANUP; exit 0' {signal_name}; touch ready.flag; sleep {SHELL_BODY}",
            signal_name = if signal == libc::SIGINT {
                "INT"
            } else {
                "TERM"
            }
        )
    } else {
        format!("touch ready.flag; sleep {SHELL_BODY}")
    };
    let client = server
        .client()
        .cwd(server.path())
        .spawn(&["sh", "-c", &script]);
    wait_for_flag(&server, "ready.flag").await;
    client.signal(signal);
    let out = client.wait();
    server.stop();

    assert_eq!(
        out.status.code(),
        Some(expected_code),
        "stderr={}",
        out.stderr
    );
    if traps_signal {
        assert!(
            out.stdout.contains("CLEANUP"),
            "child cleanup must run before exit: stdout={}",
            out.stdout
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn repeated_sigints_forward_one_frame_each() {
    let server = Server::builder().start().await;
    let client = server.client().cwd(server.path()).spawn(&[
        "sh",
        "-c",
        &format!(
            "trap 'c=$((c+1)); echo COUNT=$c; touch count-$c.flag' INT; touch ready.flag; while :; do sleep {SHELL_HOLD}; done"
        ),
    ]);
    wait_for_flag(&server, "ready.flag").await;
    client.signal(libc::SIGINT);
    wait_for_flag(&server, "count-1.flag").await;
    client.signal(libc::SIGINT);
    wait_for_flag(&server, "count-2.flag").await;
    // End the run: the child has no TERM trap, so it dies by signal.
    client.signal(libc::SIGTERM);
    let out = client.wait();
    server.stop();

    assert_eq!(out.status.code(), Some(143), "stderr={}", out.stderr);
    assert!(out.stdout.contains("COUNT=1"), "stdout={}", out.stdout);
    assert!(out.stdout.contains("COUNT=2"), "stdout={}", out.stdout);
}

#[tokio::test(flavor = "multi_thread")]
async fn post_signal_output_streams_before_terminal_frame() {
    let server = Server::builder().start().await;
    let client = server.client().cwd(server.path()).spawn(&[
        "sh",
        "-c",
        &format!(
            "trap 'echo AFTER-1; sleep {SHELL_HOLD}; echo AFTER-2; exit 0' INT; touch ready.flag; sleep {SHELL_BODY}"
        ),
    ]);
    wait_for_flag(&server, "ready.flag").await;
    client.signal(libc::SIGINT);
    let out = client.wait();
    server.stop();

    assert_eq!(out.status.code(), Some(0), "stderr={}", out.stderr);
    assert!(out.stdout.contains("AFTER-1"), "stdout={}", out.stdout);
    assert!(out.stdout.contains("AFTER-2"), "stdout={}", out.stdout);
}
