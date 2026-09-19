//! Integration tests for the first exec path (ticket 02): a real `ncap`
//! client talking to a real `ncap-server` over a tempdir socket, plus raw
//! wire-protocol tests for the frames the CLI cannot drive directly.

mod common;

use std::{fs, os::unix::fs::PermissionsExt};

use futures_util::SinkExt;
use nix_capsule::protocol::{
    CURRENT_VERSION, Exit, Frame, FrameType, Message, Request, VersionMsg,
};
use proptest::prelude::*;
use test_case::test_case;

use common::{
    Client, Server,
    assert::assert_exit_and_stdout,
    missing_socket,
    probe::{
        SHELL_HOLD, assert_clean_exit, read_until_stdout_contains, read_until_terminal, request,
        run_request, send_request, send_request_version, send_signal,
    },
};

#[test_case(
    None, &["sh", "-c", "printf out; printf err >&2; exit 7"], Some("out"), Some("err"), 7
    ; "copies_stdout_stderr_and_exit_7"
)]
#[test_case(
    None, &["sh", "-c", "kill -TERM $$"], None, None, 143
    ; "signal_reports_128_plus_signal"
)]
#[test_case(
    None, &["no-such-binary-xyz"], None,
    Some("ncap: no-such-binary-xyz: command not found\n"), 127
    ; "enoent_yields_127_with_synthesized_stderr"
)]
#[test_case(
    Some("/nonexistent-xyz-abc-123"), &["echo", "hi"], None, None, 1
    ; "bad_cwd_yields_exit_1"
)]
#[tokio::test(flavor = "multi_thread")]
async fn exec_maps_child_result_to_stdio_and_exit_code(
    cwd: Option<&str>,
    args: &[&str],
    stdout: Option<&str>,
    stderr: Option<&str>,
    code: i32,
) {
    let server = Server::builder().start().await;
    let mut client = server.client();
    if let Some(cwd_override) = cwd {
        client = client.cwd(std::path::Path::new(cwd_override));
    }
    let out = client.run(args);
    server.stop();

    assert_exit_and_stdout(&out, code, stdout);
    if let Some(want) = stderr {
        assert_eq!(out.stderr, want);
    }
}

#[test_case(None ; "defaults_to_client_current_dir")]
#[test_case(Some("work") ; "override_uses_given_dir")]
#[tokio::test(flavor = "multi_thread")]
async fn pwd_reports_effective_cwd(override_dir: Option<&str>) {
    let server = Server::builder().start().await;
    let work = server.path().join("work");
    if override_dir.is_some() {
        fs::create_dir(&work).unwrap();
    }
    let mut client = server.client();
    if override_dir.is_some() {
        client = client.cwd(&work);
    }
    let out = client.run(&["sh", "-c", "pwd"]);
    server.stop();

    let expected = match override_dir {
        Some(_) => work.to_str().unwrap().to_owned(),
        None => std::env::current_dir()
            .unwrap()
            .to_string_lossy()
            .into_owned(),
    };
    assert_eq!(out.stdout.trim_end(), expected);
    assert_eq!(out.status.code(), Some(0));
}

#[tokio::test(flavor = "multi_thread")]
async fn piped_stdin_reaches_child_and_closing_write_half_gives_eof() {
    let server = Server::builder().start().await;
    let out = server
        .client()
        .stdin(b"hello\n")
        .run(&["sh", "-c", "cat; echo EOF-REACHED"]);
    server.stop();

    assert!(out.stdout.contains("hello"), "stdout={}", out.stdout);
    assert!(out.stdout.contains("EOF-REACHED"), "stdout={}", out.stdout);
    assert_eq!(out.status.code(), Some(0));
}

#[tokio::test(flavor = "multi_thread")]
async fn eacces_yields_126_with_synthesized_stderr() {
    let server = Server::builder().start().await;
    let work = server.path().join("work");
    fs::create_dir(&work).unwrap();
    let blocked = work.join("blocked");
    fs::write(&blocked, b"#!/bin/sh\necho hi\n").unwrap();
    fs::set_permissions(&blocked, fs::Permissions::from_mode(0o644)).unwrap();
    let out = server.client().cwd(&work).run(&["./blocked"]);
    server.stop();

    assert_eq!(out.status.code(), Some(126));
    assert_eq!(out.stderr, "ncap: ./blocked: permission denied\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn explicit_env_flag_reaches_child_environment() {
    let server = Server::builder().start().await;
    let out = server.client().env_flag("NCAP_TEST_LAYER=from-flag").run(&[
        "sh",
        "-c",
        "printf %s \"$NCAP_TEST_LAYER\"",
    ]);
    server.stop();

    assert_eq!(out.stdout, "from-flag");
    assert_eq!(out.status.code(), Some(0));
}

#[test_case(
    Some("host-value") ; "bare_key_copies_when_set_on_host"
)]
#[test_case(
    None ; "bare_key_is_omitted_when_unset_on_host"
)]
#[tokio::test(flavor = "multi_thread")]
async fn bare_env_flag_copies_or_omits_host_value(host: Option<&str>) {
    let server = Server::builder().start().await;
    let mut client = server.client().env_flag("NCAP_TEST_BARE");
    if let Some(value) = host {
        client = client.env("NCAP_TEST_BARE", value);
    }
    let out = client.run(&[
        "sh",
        "-c",
        "printf %s \"${NCAP_TEST_BARE:-unset-fallback}\"",
    ]);
    server.stop();

    assert_eq!(out.stdout, host.unwrap_or("unset-fallback"));
}

#[tokio::test(flavor = "multi_thread")]
async fn forwarded_name_resolves_fresh_value_per_invocation() {
    let server = Server::builder().start().await;
    let forward = r#"["NCAP_TEST_FRESH"]"#;
    let first = server
        .client()
        .env("NCAP_ENV_FORWARD", forward)
        .env("NCAP_TEST_FRESH", "first")
        .run(&["sh", "-c", "printf %s \"$NCAP_TEST_FRESH\""]);
    let second = server
        .client()
        .env("NCAP_ENV_FORWARD", forward)
        .env("NCAP_TEST_FRESH", "second")
        .run(&["sh", "-c", "printf %s \"$NCAP_TEST_FRESH\""]);
    server.stop();

    assert_eq!(first.stdout, "first");
    assert_eq!(second.stdout, "second");
}

#[tokio::test(flavor = "multi_thread")]
async fn forwarded_name_unset_on_host_is_silently_omitted() {
    let server = Server::builder().start().await;
    let out = server
        .client()
        .env("NCAP_ENV_FORWARD", r#"["NCAP_TEST_MISSING"]"#)
        .run(&[
            "sh",
            "-c",
            "printf %s \"${NCAP_TEST_MISSING:-unset-fallback}\"",
        ]);
    server.stop();

    assert_eq!(out.stdout, "unset-fallback");
}

#[tokio::test(flavor = "multi_thread")]
async fn duplicate_env_flags_keep_last_value() {
    let server = Server::builder().start().await;
    let out = server
        .client()
        .env_flag("NCAP_TEST_DUP=first")
        .env_flag("NCAP_TEST_DUP=second")
        .run(&["sh", "-c", "printf %s \"$NCAP_TEST_DUP\""]);
    server.stop();

    assert_eq!(out.stdout, "second");
}

#[tokio::test(flavor = "multi_thread")]
async fn merged_env_arrives_deduplicated_in_request() {
    let server = Server::builder()
        .respond(vec![Message::Exit(Exit {
            code: Some(0),
            signal: None,
        })])
        .start()
        .await;
    let out = server
        .client()
        .env(
            "NCAP_ENV_FORWARD",
            r#"["NCAP_TEST_FWD_A", "NCAP_TEST_FWD_C"]"#,
        )
        .env("NCAP_TEST_FWD_A", "host-a")
        .env("NCAP_TEST_FWD_C", "host-c")
        .env_flag("NCAP_TEST_FWD_A=flag-a")
        .env_flag("NCAP_TEST_FWD_B=flag-b")
        .env_flag("NCAP_TEST_FWD_B=flag-b2")
        .run(&["true"]);

    assert_eq!(out.status.code(), Some(0));
    let request = server.captured_request().expect("captured request");
    server.stop();
    let merged: std::collections::BTreeMap<String, String> = request
        .env
        .iter()
        .map(|entry| entry.split_once('=').expect("entry carries ="))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect();
    assert_eq!(
        merged.len(),
        request.env.len(),
        "duplicates arrived: {:?}",
        request.env
    );
    assert_eq!(
        merged.get("NCAP_TEST_FWD_A").map(String::as_str),
        Some("flag-a"),
        "env={merged:?}"
    );
    assert_eq!(
        merged.get("NCAP_TEST_FWD_B").map(String::as_str),
        Some("flag-b2"),
        "env={merged:?}"
    );
    assert_eq!(
        merged.get("NCAP_TEST_FWD_C").map(String::as_str),
        Some("host-c"),
        "env={merged:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn request_env_overrides_inherited_env() {
    let server = Server::builder().start().await;
    let run = run_request(
        &mut server.raw().await,
        Request {
            env: vec!["NCAP_TEST_VAR=hello".into()],
            ..request(server.path(), "printf %s \"$NCAP_TEST_VAR\"")
        },
    )
    .await;
    server.stop();

    assert_clean_exit(&run.frames, "request env must override the inherited env");
    assert_eq!(run.stdout, "hello");
}

#[tokio::test(flavor = "multi_thread")]
async fn non_request_first_frame_is_error_and_close() {
    let server = Server::builder().start().await;
    let mut framed = server.raw().await;
    framed
        .send(Message::Stdout(b"hi".to_vec()).into_frame().unwrap())
        .await
        .unwrap();
    let frames = common::probe::read_until_terminal(&mut framed).await;
    server.stop();

    assert!(
        matches!(common::probe::terminal_of(&frames), Some(Message::Error(_))),
        "expected Error: frames={frames:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn version_probe_serves_server_version_then_closes_without_child() {
    let server = Server::builder().start().await;
    let mut framed = server.raw().await;
    send_request_version(&mut framed).await;

    let frames =
        common::probe::read_frames_until(&mut framed, common::probe::WAIT_TIGHT, |_| false).await;

    assert_eq!(
        frames,
        vec![Message::ServerVersion(VersionMsg {
            version: CURRENT_VERSION.to_string(),
        })],
        "the probe gets exactly one ServerVersion frame: frames={frames:?}"
    );

    // The reply is terminal: whatever follows the close carries no Exit/Error.
    let after =
        common::probe::read_frames_until(&mut framed, common::probe::WAIT_TIGHT, |_| false).await;
    let logs = read_newest_server_log(server.path().join("logs"));
    server.stop();

    assert!(
        !after
            .iter()
            .any(|frame| matches!(frame, Message::Exit(_) | Message::Error(_))),
        "no terminal frame may follow the probe reply: frames={after:?}"
    );
    assert!(
        logs.contains("version probe served"),
        "the debug probe line must appear: {logs:?}"
    );
    assert!(
        !logs.contains("exec request"),
        "no Child may spawn for a probe: {logs:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn version_probe_with_non_empty_payload_is_error_and_close() {
    let server = Server::builder().start().await;
    let mut framed = server.raw().await;
    framed
        .send(Frame {
            frame_type: FrameType::RequestVersion,
            payload: b"junk".to_vec(),
        })
        .await
        .unwrap();
    let frames = common::probe::read_until_terminal(&mut framed).await;
    server.stop();

    assert!(
        matches!(common::probe::terminal_of(&frames), Some(Message::Error(_))),
        "expected Error: frames={frames:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn reserved_version_frame_mid_bridge_is_ignored() {
    let server = Server::builder().start().await;
    let mut framed = server.raw().await;
    send_request(&mut framed, server.path(), "printf ok").await;
    framed
        .send(
            Message::Version(VersionMsg {
                version: "9.9.9".into(),
            })
            .into_frame()
            .unwrap(),
        )
        .await
        .unwrap();
    let run = common::probe::read_until_terminal(&mut framed).await;
    server.stop();

    assert_clean_exit(&run, "a stray Version frame must not kill the bridge");
    assert_eq!(common::probe::stdout_of(&run), "ok");
}

#[tokio::test(flavor = "multi_thread")]
async fn old_style_request_with_version_field_still_decodes_and_runs() {
    let server = Server::builder().start().await;
    let mut framed = server.raw().await;
    let old_style = format!(
        r#"{{"command":"sh","args":["-c","printf ok"],"cwd":"{}","env":[],"version":"9.9.9"}}"#,
        server.path().display()
    );
    framed
        .send(Frame {
            frame_type: FrameType::Request,
            payload: old_style.into_bytes(),
        })
        .await
        .unwrap();
    let run = common::probe::read_until_terminal(&mut framed).await;
    server.stop();

    assert_clean_exit(&run, "an old-style versioned Request must still run");
    assert_eq!(common::probe::stdout_of(&run), "ok");
}

// A failed kill is the test vehicle only: the subject is the emit-time
// severity gate shared by the log file and the stderr mirror. The signal
// number 200 is invalid, so the failure does not race the child's life like
// an already-exited group would.
const OUT_OF_RANGE_SIGNAL: u8 = 200;

#[tokio::test(flavor = "multi_thread")]
async fn error_log_level_suppresses_kill_warning_in_both_sinks() {
    let server = Server::builder().log_level("error").start().await;
    let stderr_before = server.stderr();
    let mut framed = server.raw().await;
    // The child holds the full hold after `READY`, so the signal frame —
    // already sent — is near-certainly processed before the terminal read
    // finishes; absence of the warning is the assertion, not proof it fired.
    send_request(
        &mut framed,
        server.path(),
        &format!("echo READY; sleep {SHELL_HOLD}"),
    )
    .await;
    read_until_stdout_contains(&mut framed, "READY").await;
    send_signal(&mut framed, OUT_OF_RANGE_SIGNAL).await;
    let frames = read_until_terminal(&mut framed).await;
    let stderr = server.stderr_since(&stderr_before);
    let log_file = read_newest_server_log(server.path().join("logs"));
    server.stop();

    assert_clean_exit(&frames, "the connection must continue past the bad signal");
    assert!(
        !stderr.contains("kill(-"),
        "warning below error must not mirror to stderr: {stderr:?}"
    );
    assert!(
        !log_file.contains("kill(-"),
        "warning below error must not reach the log file: {log_file:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn debug_log_level_keeps_kill_warning_in_both_sinks() {
    let server = Server::builder().log_level("debug").start().await;
    let stderr_before = server.stderr();
    let mut framed = server.raw().await;
    send_request(
        &mut framed,
        server.path(),
        &format!("echo READY; sleep {SHELL_HOLD}"),
    )
    .await;
    read_until_stdout_contains(&mut framed, "READY").await;
    send_signal(&mut framed, OUT_OF_RANGE_SIGNAL).await;
    let frames = read_until_terminal(&mut framed).await;
    let stderr = server.stderr_since(&stderr_before);
    let log_file = read_newest_server_log(server.path().join("logs"));
    server.stop();

    assert_clean_exit(&frames, "the connection must continue past the bad signal");
    assert!(
        stderr.contains("kill(-") && stderr.contains(&OUT_OF_RANGE_SIGNAL.to_string()),
        "warning at debug must mirror to stderr: {stderr:?}"
    );
    assert!(
        log_file.contains("kill(-") && log_file.contains(&OUT_OF_RANGE_SIGNAL.to_string()),
        "warning at debug must reach the log file: {log_file:?}"
    );
}

// Read back the single per-run server log under `dir`: one run writes one log file.
fn read_newest_server_log(dir: std::path::PathBuf) -> String {
    let entries: Vec<_> = fs::read_dir(&dir)
        .expect("log dir")
        .map(|entry| entry.expect("log dir entry").path())
        .collect();
    assert_eq!(entries.len(), 1, "one run, one log file");
    fs::read_to_string(&entries[0]).expect("read log file")
}

#[test_case(
    vec![Message::Exit(Exit {
        code: None,
        signal: None,
    })],
    None, 1, "carries neither"
    ; "exit_null_null_warns_and_exits_1"
)]
#[test_case(
    vec![Message::Error(nix_capsule::protocol::ErrorMsg {
        message: "boom".into(),
    })],
    None, 1, "boom"
    ; "error_frame_exits_1_with_message_on_stderr"
)]
#[tokio::test(flavor = "multi_thread")]
async fn client_maps_terminal_frame_to_exit_code(
    respond: Vec<Message>,
    stdout: Option<&str>,
    code: i32,
    stderr: &str,
) {
    let server = Server::builder().respond(respond).start().await;
    let out = server.client().run(&["echo", "hi"]);
    server.stop();

    assert_exit_and_stdout(&out, code, stdout);
    assert!(out.stderr.contains(stderr), "stderr={}", out.stderr);
}

#[tokio::test(flavor = "multi_thread")]
async fn connect_failure_names_socket_and_suggests_init() {
    let (_dir, socket) = missing_socket();

    let out = Client::at(&socket).run(&["echo", "hi"]);

    assert_eq!(out.status.code(), Some(1));
    assert!(
        out.stderr.contains(socket.to_str().unwrap()),
        "stderr={}",
        out.stderr
    );
    assert!(
        out.stderr.contains("ncap-ctl init"),
        "stderr={}",
        out.stderr
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn empty_key_env_flag_fails_locally_without_connect_hint() {
    let (_dir, socket) = missing_socket();

    let out = Client::at(&socket).env_flag("=VALUE").run(&["echo", "hi"]);

    assert_eq!(out.status.code(), Some(1));
    assert!(
        out.stderr.contains("`--env` `=VALUE` has an empty key"),
        "stderr={}",
        out.stderr
    );
    assert!(
        !out.stderr.contains("ncap-ctl init"),
        "the connect hint must not appear: stderr={}",
        out.stderr
    );
}

// A NUL-free Unicode string: full range of multibyte, newline, and control
// characters, sized so large payloads cross the server's pipe-read chunk
// boundaries. NUL is filtered out because execve argv cannot carry it.
fn arbitrary_payload() -> impl Strategy<Value = String> {
    prop::collection::vec(any::<char>(), 0..2048)
        .prop_map(|chars| chars.into_iter().filter(|c| *c != '\0').collect())
}

#[tokio::test(flavor = "multi_thread")]
async fn stdio_round_trips_verbatim_with_exit_0() {
    let server = Server::builder().start().await;
    // Each case spawns real processes, so a reduced case count keeps the
    // suite fast while the shared server carries the expensive setup.
    proptest!(ProptestConfig::with_cases(16), |(stdout_text in arbitrary_payload(), stderr_text in arbitrary_payload())| {
        let result = server
            .client()
            .run(&[
                "sh",
                "-c",
                "printf %s \"$1\"; printf %s \"$2\" >&2",
                "sh",
                &stdout_text,
                &stderr_text,
            ]);
        prop_assert_eq!(result.stdout, stdout_text);
        prop_assert_eq!(result.stderr, stderr_text);
        prop_assert_eq!(result.status.code(), Some(0));
    });
    server.stop();
}

#[tokio::test(flavor = "multi_thread")]
async fn stdin_writes_verbatim_to_child_file() {
    let server = Server::builder().start().await;
    // Each case spawns real processes, so a reduced case count keeps the
    // suite fast while the shared server carries the expensive setup.
    proptest!(ProptestConfig::with_cases(16), |(bytes in prop::collection::vec(any::<u8>(), 0..20000))| {
        let file = tempfile::NamedTempFile::new_in(server.path()).unwrap();
        let result = server
            .client()
            .stdin(&bytes)
            .run(&["sh", "-c", "cat > \"$1\"", "sh", file.path().to_str().unwrap()]);
        prop_assert_eq!(result.status.code(), Some(0));
        prop_assert_eq!(fs::read(file.path()).unwrap(), bytes);
    });
    server.stop();
}
