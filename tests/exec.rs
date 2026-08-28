//! Integration tests for the first exec path (ticket 02): a real `ncap`
//! client talking to a real `ncap-server` over a tempdir socket, plus raw
//! wire-protocol tests for the frames the CLI cannot drive directly.

#[path = "common/mod.rs"]
mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use futures_util::{SinkExt, StreamExt};
use nix_capsule::protocol::{CURRENT_VERSION, ErrorMsg, Exit, Message, Request, VersionMsg};
use proptest::prelude::*;
use test_case::test_case;

use common::{Client, Server};

// --------------------------------------------- stdio & exit codes (real server)

#[test_case(
    None, &["sh", "-c", "printf out; printf err >&2; exit 7"], Some("out"), Some("err"), 7
    ; "streams_stdio_and_exit_code"
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
async fn exec_reports_the_expected_stdio_and_exit_code(
    cwd: Option<&str>,
    args: &[&str],
    stdout: Option<&str>,
    stderr: Option<&str>,
    code: i32,
) {
    let server = Server::builder().start().await;
    let mut client = server.client();
    if let Some(c) = cwd {
        client = client.cwd(Path::new(c));
    }
    let out = client.run(args);
    server.stop();

    assert_eq!(out.status.code(), Some(code));
    if let Some(want) = stdout {
        assert_eq!(out.stdout, want);
    }
    if let Some(want) = stderr {
        assert_eq!(out.stderr, want);
    }
}

// ------------------------------------------------------------------ cwd (real)

#[test_case(None ; "defaults_to_client_current_dir")]
#[test_case(Some("work") ; "override_is_honored")]
#[tokio::test(flavor = "multi_thread")]
async fn pwd_reports_the_effective_cwd(override_dir: Option<&str>) {
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

// ---------------------------------------------------------------- stdin (real)

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

// ------------------------------------------------- synthesized errors (real)

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

// ----------------------------------------------------------- raw wire protocol

#[tokio::test(flavor = "multi_thread")]
async fn server_applies_request_env_over_inherited() {
    let server = Server::builder().start().await;
    let mut framed = server.raw().await;
    let req = Request {
        command: "sh".into(),
        args: vec!["-c".into(), "printf %s \"$NCAP_TEST_VAR\"".into()],
        cwd: server.path().to_string_lossy().into_owned(),
        env: vec!["NCAP_TEST_VAR=hello".into()],
        version: Some(CURRENT_VERSION.into()),
    };
    framed
        .send(Message::Request(req).into_frame().unwrap())
        .await
        .unwrap();

    let mut stdout = String::new();
    let mut code = None;
    while let Some(frame) = framed.next().await {
        match Message::from_frame(frame.unwrap()).unwrap() {
            Message::Version(_) => {}
            Message::Stdout(b) => stdout.push_str(&String::from_utf8_lossy(&b)),
            Message::Stderr(_) => {}
            Message::Exit(e) => {
                code = e.code;
                break;
            }
            other => panic!("unexpected frame {other:?}"),
        }
    }
    server.stop();

    assert_eq!(stdout, "hello");
    assert_eq!(code, Some(0));
}

#[tokio::test(flavor = "multi_thread")]
async fn non_request_first_frame_is_error_and_close() {
    let server = Server::builder().start().await;
    let mut framed = server.raw().await;
    framed
        .send(Message::Stdout(b"hi".to_vec()).into_frame().unwrap())
        .await
        .unwrap();
    let resp = framed.next().await.unwrap().unwrap();
    let msg = Message::from_frame(resp).unwrap();
    server.stop();

    assert!(
        matches!(msg, Message::Error(_)),
        "expected Error, got {msg:?}"
    );
}

// ------------------------------------------------------------ client behaviors

#[test_case(
    vec![
        Message::Version(VersionMsg {
            version: "9.9.9".into(),
        }),
        Message::Stdout(b"ok".to_vec()),
        Message::Exit(Exit {
            code: Some(0),
            signal: None,
        }),
    ],
    Some("ok"), 0, &["version", "9.9.9"]
    ; "version_mismatch_warns_but_command_succeeds"
)]
#[test_case(
    vec![
        Message::Stdout(b"ok".to_vec()),
        Message::Exit(Exit {
            code: Some(0),
            signal: None,
        }),
    ],
    Some("ok"), 0, &["version"]
    ; "version_absent_warns_but_command_succeeds"
)]
#[test_case(
    vec![Message::Exit(Exit {
        code: None,
        signal: None,
    })],
    None, 1, &["status", "unknown"]
    ; "exit_null_null_warns_and_exits_1"
)]
#[test_case(
    vec![Message::Error(ErrorMsg {
        message: "boom".into(),
    })],
    None, 1, &["boom"]
    ; "error_frame_exits_1_with_message_on_stderr"
)]
#[tokio::test(flavor = "multi_thread")]
async fn client_reports_what_the_server_frames_imply(
    respond: Vec<Message>,
    stdout: Option<&str>,
    code: i32,
    stderr_any_of: &[&str],
) {
    let server = Server::builder().respond(respond).start().await;
    let out = server.client().run(&["echo", "hi"]);
    server.stop();

    assert_eq!(out.status.code(), Some(code));
    if let Some(want) = stdout {
        assert_eq!(out.stdout, want);
    }
    let lowered = out.stderr.to_lowercase();
    assert!(
        stderr_any_of
            .iter()
            .any(|hint| lowered.contains(&hint.to_lowercase())),
        "stderr={}",
        out.stderr
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn connect_failure_names_socket_and_suggests_init() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("missing.sock");

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

// ------------------------------------------------------------------ properties

/// A NUL-free Unicode string: full range of multibyte, newline, and control
/// characters, sized so large payloads cross the server's pipe-read chunk
/// boundaries. NUL is filtered out because execve argv cannot carry it.
fn arb_payload() -> impl Strategy<Value = String> {
    prop::collection::vec(any::<char>(), 0..2048)
        .prop_map(|chars| chars.into_iter().filter(|c| *c != '\0').collect())
}

#[tokio::test(flavor = "multi_thread")]
async fn arbitrary_stdio_round_trips_through_a_real_child() {
    let server = Server::builder().start().await;
    // Each case spawns real processes, so a reduced case count keeps the
    // suite fast while the shared server carries the expensive setup.
    proptest!(ProptestConfig::with_cases(16), |(out in arb_payload(), err in arb_payload())| {
        let result = server
            .client()
            .run(&[
                "sh",
                "-c",
                "printf %s \"$1\"; printf %s \"$2\" >&2",
                "sh",
                &out,
                &err,
            ]);
        prop_assert_eq!(result.stdout, out);
        prop_assert_eq!(result.stderr, err);
        prop_assert_eq!(result.status.code(), Some(0));
    });
    server.stop();
}

#[tokio::test(flavor = "multi_thread")]
async fn arbitrary_stdin_is_written_verbatim_to_a_child_file() {
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
