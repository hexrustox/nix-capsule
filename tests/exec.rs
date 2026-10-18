//! Client exec-path integration tests: every test drives the real `ncap`
//! binary against a scripted stand-in for the Server over the wire protocol
//! (spec/protocol.md), pinning the Client behaviors of spec/client.md.

use std::ffi::OsStr;
use std::fs;
use std::io::{self, Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::net::UnixListener;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tempfile::tempdir;
use test_case::test_case;

#[test]
fn fails_when_no_socket_is_configured() -> TestResult {
    let mut child = ncap().args(["printf", "hi"]).spawn()?;
    let (status, _, _) = finish(&mut child)?;
    assert!(
        !status.success(),
        "expected a usage failure, exited {status}"
    );
    Ok(())
}

#[test]
fn reads_the_socket_path_from_the_environment() -> TestResult {
    let holder = tempdir()?;
    let sock = holder.path().join("ncap.sock");
    let server = serve(&sock, |mut wire| {
        wire.read_frame().expect("request frame arrives");
        wire.write_frame(EXIT, br#"{"code":0}"#)
            .expect("exit frame sends");
    })?;
    let mut child = ncap()
        .env("NCAP_SOCKET", &sock)
        .args(["printf", "hi"])
        .spawn()?;
    let (status, _, _) = finish(&mut child)?;
    server.result();
    assert_eq!(status.code(), Some(0));
    Ok(())
}

#[test]
fn exits_1_hinting_the_socket_and_init_when_connect_fails() -> TestResult {
    let holder = tempdir()?;
    let sock = holder.path().join("ncap.sock");
    let mut child = ncap()
        .arg("--socket")
        .arg(&sock)
        .args(["printf", "hi"])
        .spawn()?;
    let (status, _, stderr) = finish(&mut child)?;
    assert_eq!(status.code(), Some(1));
    assert!(
        stderr.contains(sock.to_string_lossy().as_ref()),
        "hint should name the socket path, stderr was: {stderr}"
    );
    assert!(
        stderr.contains("ncap-ctl init"),
        "hint should suggest `ncap-ctl init`, stderr was: {stderr}"
    );
    Ok(())
}

#[test]
fn rejects_an_empty_command_before_building_a_request() -> TestResult {
    let holder = tempdir()?;
    let sock = holder.path().join("ncap.sock");
    let listener = UnixListener::bind(&sock)?;
    let mut child = ncap().arg("--socket").arg(&sock).spawn()?;
    let (status, _, _) = finish(&mut child)?;
    assert!(!status.success(), "expected rejection, exited {status}");
    assert_no_connection(&listener)?;
    Ok(())
}

#[test]
fn fails_locally_on_an_unresolvable_current_directory() -> TestResult {
    let holder = tempdir()?;
    let sock = holder.path().join("ncap.sock");
    let listener = UnixListener::bind(&sock)?;
    let doomed = tempdir()?;
    let mut child = Command::new("/bin/sh")
        .env_clear()
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .arg("-c")
        .arg("cd \"$1\" && /bin/rm -rf \"$1\" && exec \"$2\" --socket \"$3\" printf gone")
        .arg("doomed")
        .arg(doomed.path())
        .arg(env!("CARGO_BIN_EXE_ncap"))
        .arg(&sock)
        .spawn()?;
    let (status, _, _) = finish(&mut child)?;
    assert_eq!(status.code(), Some(1));
    assert_no_connection(&listener)?;
    Ok(())
}

#[test]
fn opens_the_exec_connection_with_a_request_carrying_the_invocation() -> TestResult {
    let holder = tempdir()?;
    let sock = holder.path().join("ncap.sock");
    let workdir = tempdir()?;
    let server = serve(&sock, |mut wire| {
        wire.read_frame().expect("request frame arrives")
    })?;
    let mut child = ncap()
        .arg("--socket")
        .arg(&sock)
        .current_dir(workdir.path())
        .args(["printf", "hello", "world"])
        .spawn()?;
    let (_, _, _) = finish(&mut child)?;
    let (tag, payload) = server.result();
    assert_eq!(tag, REQUEST);
    let request: Value = serde_json::from_slice(&payload)?;
    assert_eq!(request["command"], "printf");
    assert_eq!(request["args"], json!(["hello", "world"]));
    let expected_cwd = fs::canonicalize(workdir.path())?
        .to_string_lossy()
        .into_owned();
    assert_eq!(request["cwd"], expected_cwd.as_str());
    assert_eq!(request["env"], json!([]));
    Ok(())
}

#[test]
fn passes_the_cwd_flag_through_to_the_request() -> TestResult {
    let holder = tempdir()?;
    let sock = holder.path().join("ncap.sock");
    let workdir = tempdir()?;
    let cwd = fs::canonicalize(workdir.path())?
        .to_string_lossy()
        .into_owned();
    let server = serve(&sock, |mut wire| {
        wire.read_frame().expect("request frame arrives")
    })?;
    let mut child = ncap()
        .arg("--socket")
        .arg(&sock)
        .arg("--cwd")
        .arg(&cwd)
        .args(["printf", "hi"])
        .spawn()?;
    let (_, _, _) = finish(&mut child)?;
    let (_, payload) = server.result();
    let request: Value = serde_json::from_slice(&payload)?;
    assert_eq!(request["cwd"], cwd.as_str());
    Ok(())
}

#[test]
fn accepts_the_short_flag_forms_for_socket_env_and_cwd() -> TestResult {
    let holder = tempdir()?;
    let sock = holder.path().join("ncap.sock");
    let workdir = tempdir()?;
    let cwd = fs::canonicalize(workdir.path())?
        .to_string_lossy()
        .into_owned();
    let server = serve(&sock, |mut wire| {
        wire.read_frame().expect("request frame arrives")
    })?;
    let mut child = ncap()
        .arg("-s")
        .arg(&sock)
        .arg("-e")
        .arg("SHORT_FLAG=short-value")
        .arg("-c")
        .arg(&cwd)
        .args(["printf", "hi"])
        .spawn()?;
    let (_, _, _) = finish(&mut child)?;
    let (_, payload) = server.result();
    let request: Value = serde_json::from_slice(&payload)?;
    assert_eq!(request["env"], json!(["SHORT_FLAG=short-value"]));
    assert_eq!(request["cwd"], cwd.as_str());
    Ok(())
}

#[test]
fn passes_args_after_the_separator_verbatim() -> TestResult {
    let holder = tempdir()?;
    let sock = holder.path().join("ncap.sock");
    let server = serve(&sock, |mut wire| {
        wire.read_frame().expect("request frame arrives")
    })?;
    let mut child = ncap()
        .arg("--socket")
        .arg(&sock)
        .arg("--")
        .args(["grep", "-e", "needle", "haystack.txt"])
        .spawn()?;
    let (_, _, _) = finish(&mut child)?;
    let (_, payload) = server.result();
    let request: Value = serde_json::from_slice(&payload)?;
    assert_eq!(request["command"], "grep");
    assert_eq!(request["args"], json!(["-e", "needle", "haystack.txt"]));
    Ok(())
}

#[test]
fn converts_non_utf8_command_line_values_lossily() -> TestResult {
    let holder = tempdir()?;
    let sock = holder.path().join("ncap.sock");
    let server = serve(&sock, |mut wire| {
        wire.read_frame().expect("request frame arrives")
    })?;
    let mut child = ncap()
        .arg("--socket")
        .arg(&sock)
        .arg("--env")
        .arg(OsStr::from_bytes(b"BAD\xff=v\xfe"))
        .arg("printf")
        .arg(OsStr::from_bytes(b"lossy\xff"))
        .spawn()?;
    let (_, _, _) = finish(&mut child)?;
    let (_, payload) = server.result();
    let request: Value = serde_json::from_slice(&payload)?;
    assert_eq!(request["args"], json!(["lossy\u{FFFD}"]));
    assert_eq!(request["env"], json!(["BAD\u{FFFD}=v\u{FFFD}"]));
    Ok(())
}

#[test]
fn resolves_forwarded_names_from_the_client_environment() -> TestResult {
    let holder = tempdir()?;
    let sock = holder.path().join("ncap.sock");
    let server = serve(&sock, |mut wire| {
        wire.read_frame().expect("request frame arrives")
    })?;
    let mut child = ncap()
        .arg("--socket")
        .arg(&sock)
        .env("NCAP_ENV_FORWARD", r#"["FWD_SET", "FWD_MISSING"]"#)
        .env("FWD_SET", "alpha")
        .args(["printf", "hi"])
        .spawn()?;
    let (_, _, _) = finish(&mut child)?;
    let (_, payload) = server.result();
    let request: Value = serde_json::from_slice(&payload)?;
    assert_eq!(request["env"], json!(["FWD_SET=alpha"]));
    Ok(())
}

#[test]
fn copies_bare_env_flags_from_the_client_environment() -> TestResult {
    let holder = tempdir()?;
    let sock = holder.path().join("ncap.sock");
    let server = serve(&sock, |mut wire| {
        wire.read_frame().expect("request frame arrives")
    })?;
    let mut child = ncap()
        .arg("--socket")
        .arg(&sock)
        .arg("--env")
        .arg("BARE_SET")
        .arg("--env")
        .arg("BARE_MISSING")
        .env("BARE_SET", "bare-value")
        .args(["printf", "hi"])
        .spawn()?;
    let (_, _, _) = finish(&mut child)?;
    let (_, payload) = server.result();
    let request: Value = serde_json::from_slice(&payload)?;
    assert_eq!(request["env"], json!(["BARE_SET=bare-value"]));
    Ok(())
}

#[test]
fn passes_empty_and_equals_bearing_flag_values_verbatim() -> TestResult {
    let holder = tempdir()?;
    let sock = holder.path().join("ncap.sock");
    let server = serve(&sock, |mut wire| {
        wire.read_frame().expect("request frame arrives")
    })?;
    let mut child = ncap()
        .arg("--socket")
        .arg(&sock)
        .arg("--env")
        .arg("EMPTY=")
        .arg("--env")
        .arg("EQ=a=b")
        .args(["printf", "hi"])
        .spawn()?;
    let (_, _, _) = finish(&mut child)?;
    let (_, payload) = server.result();
    let request: Value = serde_json::from_slice(&payload)?;
    assert_eq!(request["env"], json!(["EMPTY=", "EQ=a=b"]));
    Ok(())
}

#[test]
fn lets_later_layers_win_by_key_without_moving_position() -> TestResult {
    let holder = tempdir()?;
    let sock = holder.path().join("ncap.sock");
    let server = serve(&sock, |mut wire| {
        wire.read_frame().expect("request frame arrives")
    })?;
    let mut child = ncap()
        .arg("--socket")
        .arg(&sock)
        .env("NCAP_ENV_FORWARD", r#"["LAYER_A", "LAYER_B", "LAYER_C"]"#)
        .env("LAYER_A", "forwarded-a")
        .env("LAYER_B", "forwarded-b")
        .env("LAYER_C", "forwarded-c")
        .arg("--env")
        .arg("LAYER_B=flag-b")
        .args(["printf", "hi"])
        .spawn()?;
    let (_, _, _) = finish(&mut child)?;
    let (_, payload) = server.result();
    let request: Value = serde_json::from_slice(&payload)?;
    assert_eq!(
        request["env"],
        json!([
            "LAYER_A=forwarded-a",
            "LAYER_B=flag-b",
            "LAYER_C=forwarded-c"
        ])
    );
    Ok(())
}

#[test]
fn lets_a_later_flag_replace_an_earlier_flag_by_key() -> TestResult {
    let holder = tempdir()?;
    let sock = holder.path().join("ncap.sock");
    let server = serve(&sock, |mut wire| {
        wire.read_frame().expect("request frame arrives")
    })?;
    let mut child = ncap()
        .arg("--socket")
        .arg(&sock)
        .arg("--env")
        .arg("FIRST=one")
        .arg("--env")
        .arg("DUP=first")
        .arg("--env")
        .arg("DUP=second")
        .args(["printf", "hi"])
        .spawn()?;
    let (_, _, _) = finish(&mut child)?;
    let (_, payload) = server.result();
    let request: Value = serde_json::from_slice(&payload)?;
    assert_eq!(request["env"], json!(["FIRST=one", "DUP=second"]));
    Ok(())
}

#[test_case("not json at all"; "not json")]
#[test_case(r#"["A", 7]"#; "not an array of names")]
fn fails_locally_on_a_malformed_env_forward_list(raw: &str) -> TestResult {
    let holder = tempdir()?;
    let sock = holder.path().join("ncap.sock");
    let listener = UnixListener::bind(&sock)?;
    let mut child = ncap()
        .arg("--socket")
        .arg(&sock)
        .env("NCAP_ENV_FORWARD", raw)
        .args(["printf", "hi"])
        .spawn()?;
    let (status, _, _) = finish(&mut child)?;
    assert_eq!(status.code(), Some(1));
    assert_no_connection(&listener)?;
    Ok(())
}

#[test_case("=VALUE"; "key is empty with a value")]
#[test_case(""; "flag is the empty string")]
fn fails_locally_on_an_env_flag_with_an_empty_key(flag_value: &str) -> TestResult {
    let holder = tempdir()?;
    let sock = holder.path().join("ncap.sock");
    let listener = UnixListener::bind(&sock)?;
    let mut child = ncap()
        .arg("--socket")
        .arg(&sock)
        .arg("--env")
        .arg(flag_value)
        .args(["printf", "hi"])
        .spawn()?;
    let (status, _, _) = finish(&mut child)?;
    assert_eq!(status.code(), Some(1));
    assert_no_connection(&listener)?;
    Ok(())
}

#[test]
fn pumps_stdin_frames_until_host_stdin_eof() -> TestResult {
    let holder = tempdir()?;
    let sock = holder.path().join("ncap.sock");
    let server = serve(&sock, |mut wire| {
        wire.read_frame().expect("request frame arrives");
        let first = wire.read_frame().expect("stdin data frame arrives");
        let eof = wire.read_frame().expect("stdin eof frame arrives");
        wire.write_frame(EXIT, br#"{"code":0}"#)
            .expect("exit frame sends");
        vec![first, eof]
    })?;
    let mut child = ncap().arg("--socket").arg(&sock).args(["cat"]).spawn()?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    stdin.write_all(b"feed me")?;
    drop(stdin);
    let (_, _, _) = finish(&mut child)?;
    let frames = server.result();
    assert_eq!(
        frames,
        vec![(STDIN, b"feed me".to_vec()), (STDIN, Vec::new())]
    );
    Ok(())
}

#[test]
fn sends_a_lone_empty_stdin_frame_when_stdin_is_immediately_eof() -> TestResult {
    let holder = tempdir()?;
    let sock = holder.path().join("ncap.sock");
    let server = serve(&sock, |mut wire| {
        wire.read_frame().expect("request frame arrives");
        let frame = wire.read_frame().expect("stdin eof frame arrives");
        wire.write_frame(EXIT, br#"{"code":0}"#)
            .expect("exit frame sends");
        frame
    })?;
    let mut child = ncap()
        .arg("--socket")
        .arg(&sock)
        .args(["cat"])
        .stdin(Stdio::null())
        .spawn()?;
    let (_, _, _) = finish(&mut child)?;
    let frame = server.result();
    assert_eq!(frame, (STDIN, Vec::new()));
    Ok(())
}

#[test]
fn chunks_large_stdin_across_multiple_frames() -> TestResult {
    let holder = tempdir()?;
    let sock = holder.path().join("ncap.sock");
    let payload = "0123456789abcdef".repeat(6 * 1024).into_bytes();
    let total = payload.len();
    let server = serve(&sock, move |mut wire| {
        wire.read_frame().expect("request frame arrives");
        let mut received = Vec::new();
        let mut frames = 0;
        while received.len() < total {
            let (tag, data) = wire.read_frame().expect("stdin frame arrives");
            assert_eq!(tag, STDIN, "only stdin frames carry the pump");
            received.extend_from_slice(&data);
            frames += 1;
        }
        (received, frames)
    })?;
    let mut child = ncap().arg("--socket").arg(&sock).args(["cat"]).spawn()?;
    let mut stdin = child.stdin.take().expect("piped stdin");
    stdin.write_all(&payload)?;
    drop(stdin);
    let (_, _, _) = finish(&mut child)?;
    let (received, frames) = server.result();
    assert_eq!(received, payload);
    assert!(
        frames >= 2,
        "expected the pump to chunk around 8 KiB at a time, got {frames} frames for {total} bytes"
    );
    Ok(())
}

#[test]
fn treats_a_host_stdin_read_error_as_eof() -> TestResult {
    let holder = tempdir()?;
    let sock = holder.path().join("ncap.sock");
    // a directory fd makes the client's stdin read fail with EISDIR, the read
    // error the pump must fold into EOF
    let stdin_directory = fs::File::open(holder.path())?;
    let server = serve(&sock, |mut wire| {
        wire.read_frame().expect("request frame arrives");
        let frame = wire.read_frame().expect("stdin eof frame arrives");
        wire.write_frame(EXIT, br#"{"code":0}"#)
            .expect("exit frame sends");
        frame
    })?;
    let mut child = ncap()
        .arg("--socket")
        .arg(&sock)
        .args(["cat"])
        .stdin(Stdio::from(stdin_directory))
        .spawn()?;
    let (_, _, _) = finish(&mut child)?;
    let frame = server.result();
    assert_eq!(frame, (STDIN, Vec::new()));
    Ok(())
}

#[test]
fn keeps_the_child_exit_code_when_the_child_finishes_before_stdin_eof() -> TestResult {
    let holder = tempdir()?;
    let sock = holder.path().join("ncap.sock");
    let server = serve(&sock, |mut wire| {
        wire.read_frame().expect("request frame arrives");
        wire.write_frame(EXIT, br#"{"code":7}"#)
            .expect("exit frame sends");
    })?;
    // stdin stays open, so the client's eof frame races the closed socket and
    // its failed send must not turn the exit into 1
    let mut child = ncap().arg("--socket").arg(&sock).args(["cat"]).spawn()?;
    let (status, _, _) = finish(&mut child)?;
    server.result();
    assert_eq!(status.code(), Some(7));
    Ok(())
}

#[test]
fn streams_child_stdout_and_stderr_to_the_terminal() -> TestResult {
    let holder = tempdir()?;
    let sock = holder.path().join("ncap.sock");
    let server = serve(&sock, |mut wire| {
        wire.read_frame().expect("request frame arrives");
        wire.write_frame(STDOUT, b"stream-out")
            .expect("stdout frame sends");
        wire.write_frame(STDERR, b"stream-err")
            .expect("stderr frame sends");
        wire.write_frame(EXIT, br#"{"code":0}"#)
            .expect("exit frame sends");
    })?;
    let mut child = ncap().arg("--socket").arg(&sock).args(["tee"]).spawn()?;
    let (_, stdout, stderr) = finish(&mut child)?;
    server.result();
    assert_eq!(stdout, "stream-out");
    assert_eq!(stderr, "stream-err");
    Ok(())
}

#[test]
fn reassembles_chunked_output_frames_per_stream_in_order() -> TestResult {
    let holder = tempdir()?;
    let sock = holder.path().join("ncap.sock");
    let server = serve(&sock, |mut wire| {
        wire.read_frame().expect("request frame arrives");
        wire.write_frame(STDOUT, b"chunk-")
            .expect("stdout frame sends");
        wire.write_frame(STDOUT, b"one")
            .expect("stdout frame sends");
        wire.write_frame(STDERR, b"err-1")
            .expect("stderr frame sends");
        wire.write_frame(STDERR, b"err-2")
            .expect("stderr frame sends");
        wire.write_frame(EXIT, br#"{"code":0}"#)
            .expect("exit frame sends");
    })?;
    let mut child = ncap().arg("--socket").arg(&sock).args(["sh"]).spawn()?;
    let (_, stdout, stderr) = finish(&mut child)?;
    server.result();
    assert_eq!(stdout, "chunk-one");
    assert_eq!(stderr, "err-1err-2");
    Ok(())
}

#[test]
fn exits_with_the_child_code_carried_by_the_exit_frame() -> TestResult {
    let holder = tempdir()?;
    let sock = holder.path().join("ncap.sock");
    let server = serve(&sock, |mut wire| {
        wire.read_frame().expect("request frame arrives");
        wire.write_frame(EXIT, br#"{"code":42}"#)
            .expect("exit frame sends");
    })?;
    let mut child = ncap().arg("--socket").arg(&sock).args(["false"]).spawn()?;
    let (status, _, _) = finish(&mut child)?;
    server.result();
    assert_eq!(status.code(), Some(42));
    Ok(())
}

#[test]
fn exits_with_128_plus_the_child_signal() -> TestResult {
    let holder = tempdir()?;
    let sock = holder.path().join("ncap.sock");
    let server = serve(&sock, |mut wire| {
        wire.read_frame().expect("request frame arrives");
        wire.write_frame(EXIT, br#"{"signal":9}"#)
            .expect("exit frame sends");
    })?;
    let mut child = ncap().arg("--socket").arg(&sock).args(["sleep"]).spawn()?;
    let (status, _, _) = finish(&mut child)?;
    server.result();
    assert_eq!(status.code(), Some(137));
    Ok(())
}

#[test_case(127; "code 127")]
#[test_case(126; "code 126")]
fn exits_the_spawn_failure_codes_with_a_stderr_notice(code: u8) -> TestResult {
    let holder = tempdir()?;
    let sock = holder.path().join("ncap.sock");
    let payload = format!(r#"{{"code":{code}}}"#);
    let server = serve(&sock, move |mut wire| {
        wire.read_frame().expect("request frame arrives");
        wire.write_frame(EXIT, payload.as_bytes())
            .expect("exit frame sends");
    })?;
    let mut child = ncap().arg("--socket").arg(&sock).args(["sh"]).spawn()?;
    let (status, _, stderr) = finish(&mut child)?;
    server.result();
    assert_eq!(status.code(), Some(code as i32));
    assert!(
        !stderr.is_empty(),
        "the client should print a notice naming the cause"
    );
    Ok(())
}

#[test]
fn exits_1_with_a_notice_when_the_exit_frame_carries_no_fields() -> TestResult {
    let holder = tempdir()?;
    let sock = holder.path().join("ncap.sock");
    let server = serve(&sock, |mut wire| {
        wire.read_frame().expect("request frame arrives");
        wire.write_frame(EXIT, b"{}").expect("exit frame sends");
    })?;
    let mut child = ncap().arg("--socket").arg(&sock).args(["sh"]).spawn()?;
    let (status, _, stderr) = finish(&mut child)?;
    server.result();
    assert_eq!(status.code(), Some(1));
    assert!(
        !stderr.is_empty(),
        "the client should print a notice naming the cause"
    );
    Ok(())
}

#[test]
fn exits_1_naming_the_cause_when_the_terminal_frame_is_error() -> TestResult {
    let holder = tempdir()?;
    let sock = holder.path().join("ncap.sock");
    let server = serve(&sock, |mut wire| {
        wire.read_frame().expect("request frame arrives");
        wire.write_frame(ERROR, br#"{"message":"no such directory"}"#)
            .expect("error frame sends");
    })?;
    let mut child = ncap().arg("--socket").arg(&sock).args(["sh"]).spawn()?;
    let (status, _, stderr) = finish(&mut child)?;
    server.result();
    assert_eq!(status.code(), Some(1));
    assert!(
        stderr.contains("no such directory"),
        "the notice should name the cause, stderr was: {stderr}"
    );
    Ok(())
}

#[test_case(EXIT, br#"{"code":1,"signal":9}"#; "exit frame with both code and signal set")]
#[test_case(EXIT, b"not-json"; "exit payload that is not json")]
#[test_case(SERVER_STOPPING, b"unexpected"; "server stopping frame with a non-empty payload")]
fn exits_1_on_terminal_frames_the_protocol_rejects(tag: u8, payload: &'static [u8]) -> TestResult {
    let holder = tempdir()?;
    let sock = holder.path().join("ncap.sock");
    let server = serve(&sock, move |mut wire| {
        wire.read_frame().expect("request frame arrives");
        wire.write_frame(tag, payload)
            .expect("rejected frame sends");
    })?;
    let mut child = ncap().arg("--socket").arg(&sock).args(["sh"]).spawn()?;
    let (status, _, _) = finish(&mut child)?;
    server.result();
    assert_eq!(status.code(), Some(1));
    Ok(())
}

#[test]
fn exits_1_on_a_frame_declaring_more_than_the_payload_cap() -> TestResult {
    let holder = tempdir()?;
    let sock = holder.path().join("ncap.sock");
    let server = serve(&sock, |mut wire| {
        wire.read_frame().expect("request frame arrives");
        // 16 MiB is the cap; this Stdout header declares one byte more and never sends a payload
        wire.stream
            .write_all(&[STDOUT, 0x01, 0x00, 0x00, 0x01])
            .expect("oversized header sends");
    })?;
    let mut child = ncap().arg("--socket").arg(&sock).args(["sh"]).spawn()?;
    let (status, _, _) = finish(&mut child)?;
    server.result();
    assert_eq!(status.code(), Some(1));
    Ok(())
}

#[test]
fn exits_1_on_an_unknown_tag_byte() -> TestResult {
    let holder = tempdir()?;
    let sock = holder.path().join("ncap.sock");
    let server = serve(&sock, |mut wire| {
        wire.read_frame().expect("request frame arrives");
        // 0x7f matches no tag in the protocol's frame table
        wire.write_frame(0x7F, b"?")
            .expect("unknown tag frame sends");
    })?;
    let mut child = ncap().arg("--socket").arg(&sock).args(["sh"]).spawn()?;
    let (status, _, _) = finish(&mut child)?;
    server.result();
    assert_eq!(status.code(), Some(1));
    Ok(())
}

#[test]
fn ignores_the_reserved_tag_byte_wherever_it_arrives() -> TestResult {
    let holder = tempdir()?;
    let sock = holder.path().join("ncap.sock");
    let server = serve(&sock, |mut wire| {
        wire.read_frame().expect("request frame arrives");
        // the payload is the ServerVersion shape so the pin stays about
        // ignoring the reserved tag, not about decode failures
        wire.write_frame(VERSION, br#"{"version":"stray"}"#)
            .expect("reserved frame sends");
        wire.write_frame(STDOUT, b"after-reserved")
            .expect("stdout frame sends");
        wire.write_frame(EXIT, br#"{"code":0}"#)
            .expect("exit frame sends");
    })?;
    let mut child = ncap().arg("--socket").arg(&sock).args(["sh"]).spawn()?;
    let (status, stdout, _) = finish(&mut child)?;
    server.result();
    assert_eq!(status.code(), Some(0));
    assert_eq!(stdout, "after-reserved");
    Ok(())
}

#[test]
fn ignores_misdirected_frames_from_the_server() -> TestResult {
    let holder = tempdir()?;
    let sock = holder.path().join("ncap.sock");
    let server = serve(&sock, |mut wire| {
        wire.read_frame().expect("request frame arrives");
        wire.write_frame(STDIN, b"misdirected")
            .expect("misdirected frame sends");
        wire.write_frame(STDOUT, b"kept-streaming")
            .expect("stdout frame sends");
        wire.write_frame(EXIT, br#"{"code":0}"#)
            .expect("exit frame sends");
    })?;
    let mut child = ncap().arg("--socket").arg(&sock).args(["sh"]).spawn()?;
    let (status, stdout, _) = finish(&mut child)?;
    server.result();
    assert_eq!(status.code(), Some(0));
    assert_eq!(stdout, "kept-streaming");
    Ok(())
}

#[test]
fn bails_with_143_and_stops_streaming_on_server_stopping() -> TestResult {
    let holder = tempdir()?;
    let sock = holder.path().join("ncap.sock");
    let server = serve(&sock, |mut wire| {
        wire.read_frame().expect("request frame arrives");
        wire.write_frame(STDOUT, b"before-stopping")
            .expect("stdout frame sends");
        wire.write_frame(SERVER_STOPPING, b"")
            .expect("stopping frame sends");
        // the client bails on ServerStopping, so later sends may fail; either
        // way the frames must not be processed
        let _ = wire.write_frame(STDOUT, b"after-stopping");
        let _ = wire.write_frame(EXIT, br#"{"code":0}"#);
    })?;
    let mut child = ncap().arg("--socket").arg(&sock).args(["sh"]).spawn()?;
    let (status, stdout, _) = finish(&mut child)?;
    server.result();
    assert_eq!(status.code(), Some(143));
    assert_eq!(stdout, "before-stopping");
    Ok(())
}

#[test]
fn exits_143_when_the_socket_closes_without_a_terminal_frame() -> TestResult {
    let holder = tempdir()?;
    let sock = holder.path().join("ncap.sock");
    let server = serve(&sock, |mut wire| {
        wire.read_frame().expect("request frame arrives");
    })?;
    let mut child = ncap().arg("--socket").arg(&sock).args(["sh"]).spawn()?;
    let (status, _, _) = finish(&mut child)?;
    server.result();
    assert_eq!(status.code(), Some(143));
    Ok(())
}

#[test]
fn opens_no_version_probe_connection() -> TestResult {
    let holder = tempdir()?;
    let sock = holder.path().join("ncap.sock");
    let listener = UnixListener::bind(&sock)?;
    let tag = thread::scope(|scope| -> io::Result<u8> {
        let handled = scope.spawn(|| {
            let (stream, _) = listener.accept().expect("the client connects");
            let mut wire = Wire { stream };
            let frame = wire.read_frame().expect("the request frame arrives");
            wire.write_frame(EXIT, br#"{"code":0}"#)
                .expect("exit frame sends");
            frame
        });
        let mut child = ncap()
            .arg("--socket")
            .arg(&sock)
            .args(["printf", "hi"])
            .spawn()?;
        let (status, _, _) = finish(&mut child)?;
        assert_eq!(status.code(), Some(0));
        let (tag, _) = handled.join().expect("the server script finishes");
        Ok(tag)
    })?;
    assert_eq!(tag, REQUEST, "the only connection carries the exec request");
    assert_no_connection(&listener)?;
    Ok(())
}

#[test]
fn sends_a_changed_forwarded_value_on_the_next_invocation() -> TestResult {
    let holder = tempdir()?;
    let sock = holder.path().join("ncap.sock");
    let server = serve(&sock, |mut wire| {
        wire.read_frame().expect("request frame arrives")
    })?;
    let mut child = ncap()
        .arg("--socket")
        .arg(&sock)
        .env("NCAP_ENV_FORWARD", r#"["FWD_SET"]"#)
        .env("FWD_SET", "alpha")
        .args(["printf", "hi"])
        .spawn()?;
    let (_, _, _) = finish(&mut child)?;
    let (_, payload) = server.result();
    let request: Value = serde_json::from_slice(&payload)?;
    assert_eq!(request["env"], json!(["FWD_SET=alpha"]));
    fs::remove_file(&sock)?;

    let server = serve(&sock, |mut wire| {
        wire.read_frame().expect("request frame arrives")
    })?;
    let mut child = ncap()
        .arg("--socket")
        .arg(&sock)
        .env("NCAP_ENV_FORWARD", r#"["FWD_SET"]"#)
        .env("FWD_SET", "beta")
        .args(["printf", "hi"])
        .spawn()?;
    let (_, _, _) = finish(&mut child)?;
    let (_, payload) = server.result();
    let request: Value = serde_json::from_slice(&payload)?;
    assert_eq!(request["env"], json!(["FWD_SET=beta"]));
    Ok(())
}

#[test]
fn converts_a_non_utf8_cwd_lossily() -> TestResult {
    let holder = tempdir()?;
    let sock = holder.path().join("ncap.sock");
    let workdir = tempdir()?;
    let odd = workdir.path().join(OsStr::from_bytes(b"dir\xff"));
    fs::create_dir(&odd)?;
    let server = serve(&sock, |mut wire| {
        wire.read_frame().expect("request frame arrives")
    })?;
    let mut child = ncap()
        .arg("--socket")
        .arg(&sock)
        .arg("--cwd")
        .arg(&odd)
        .args(["printf", "hi"])
        .spawn()?;
    let (_, _, _) = finish(&mut child)?;
    let (_, payload) = server.result();
    let request: Value = serde_json::from_slice(&payload)?;
    let expected = format!("{}/dir\u{FFFD}", workdir.path().to_string_lossy());
    assert_eq!(request["cwd"], expected.as_str());
    Ok(())
}

// Frame tags from spec/protocol.md § Frames.
const REQUEST: u8 = 0x01;
const STDIN: u8 = 0x02;
const STDOUT: u8 = 0x03;
const STDERR: u8 = 0x04;
const EXIT: u8 = 0x05;
const ERROR: u8 = 0x06;
const SERVER_STOPPING: u8 = 0x07;
const VERSION: u8 = 0x08;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn ncap() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ncap"));
    // the request env is built from the client's own environment, so tests
    // start from a clean one
    command
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

struct Wire {
    stream: std::os::unix::net::UnixStream,
}

impl Wire {
    fn read_frame(&mut self) -> io::Result<(u8, Vec<u8>)> {
        let mut head = [0u8; 5];
        self.stream.read_exact(&mut head)?;
        let len = u32::from_be_bytes(head[1..].try_into().expect("the length slice is 4 bytes"));
        let mut payload = vec![0u8; len as usize];
        self.stream.read_exact(&mut payload)?;
        Ok((head[0], payload))
    }

    fn write_frame(&mut self, tag: u8, payload: &[u8]) -> io::Result<()> {
        let mut frame = Vec::with_capacity(5 + payload.len());
        frame.push(tag);
        frame.extend_from_slice(&(payload.len() as u32).to_be_bytes());
        frame.extend_from_slice(payload);
        self.stream.write_all(&frame)
    }
}

fn serve<T, F>(sock: &std::path::Path, script: F) -> io::Result<Server<T>>
where
    T: Send + 'static,
    F: FnOnce(Wire) -> T + Send + 'static,
{
    let listener = UnixListener::bind(sock)?;
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        // catch_unwind surfaces script panics through Server::result instead
        // of stalling it on a dropped channel
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let (stream, _) = listener.accept().expect("the client connects");
            script(Wire { stream })
        }));
        let _ = tx.send(outcome);
    });
    Ok(Server { rx })
}

struct Server<T> {
    rx: mpsc::Receiver<thread::Result<T>>,
}

impl<T> Server<T> {
    fn result(self) -> T {
        match self.rx.recv_timeout(Duration::from_secs(30)) {
            Ok(Ok(value)) => value,
            Ok(Err(panic)) => std::panic::resume_unwind(panic),
            Err(_) => panic!("the server script did not finish within 30s"),
        }
    }
}

fn finish(
    child: &mut std::process::Child,
) -> io::Result<(std::process::ExitStatus, String, String)> {
    let deadline = Instant::now() + Duration::from_secs(30);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() > deadline {
            let _ = child.kill();
            panic!("ncap did not exit within 30s");
        }
        thread::sleep(Duration::from_millis(10));
    };
    let stdout = drain(child.stdout.take())?;
    let stderr = drain(child.stderr.take())?;
    Ok((status, stdout, stderr))
}

fn drain<R: Read>(pipe: Option<R>) -> io::Result<String> {
    let mut collected = String::new();
    if let Some(mut pipe) = pipe {
        pipe.read_to_string(&mut collected)?;
    }
    Ok(collected)
}

fn assert_no_connection(listener: &UnixListener) -> io::Result<()> {
    listener.set_nonblocking(true)?;
    match listener.accept() {
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(()),
        other => panic!("expected no connection attempt, accept says {other:?}"),
    }
}
