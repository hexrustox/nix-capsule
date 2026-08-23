mod common;

use std::io::{BufReader, BufWriter, Write};
use std::process::{Command, Stdio};
use std::thread::{sleep, spawn};
use std::time::Duration;

use common::*;

#[test]
fn stdout_bridging() {
    let server = TestServer::start();
    let output = server.run(&["--", "echo", "hello world"]);

    assert!(output.status.success());
    assert_eq!(output.stdout, b"hello world\n");
}

#[test]
fn stderr_bridging() {
    let server = TestServer::start();
    let output = server.run(&["--", "sh", "-c", "echo error stream >&2"]);

    assert!(output.status.success());
    assert_eq!(output.stderr, b"error stream\n");
}

#[test]
fn interleaved_stdout_stderr() {
    let server = TestServer::start();
    let output = server.run(&[
        "--",
        "sh",
        "-c",
        "echo out1; echo err1 >&2; echo out2; echo err2 >&2",
    ]);

    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "out1\nout2\n");
    assert_eq!(String::from_utf8_lossy(&output.stderr), "err1\nerr2\n");
}

#[test]
fn exit_code_success() {
    let server = TestServer::start();
    let output = server.run(&["--", "true"]);
    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn exit_code_failure() {
    let server = TestServer::start();
    let output = server.run(&["--", "false"]);
    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn exit_code_custom() {
    let server = TestServer::start();
    let output = server.run(&["--", "sh", "-c", "exit 42"]);
    assert_eq!(output.status.code(), Some(42));
}

#[test]
fn env_override() {
    let server = TestServer::start();
    let output = server.run(&[
        "--env",
        "CAPSULE_TEST=bar",
        "--",
        "sh",
        "-c",
        "echo $CAPSULE_TEST",
    ]);

    assert!(output.status.success());
    assert_eq!(output.stdout, b"bar\n");
}

#[test]
fn cwd_override() {
    let server = TestServer::start();
    let output = server.run(&["--cwd", "/tmp", "--", "pwd"]);

    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "/tmp");
}

#[test]
fn spawn_failure() {
    let server = TestServer::start();
    let output = server.run(&["--", "this-command-does-not-exist"]);
    assert!(!output.status.success());
}

#[test]
fn stdin_roundtrip() {
    let server = TestServer::start();
    let mut child = server
        .ncap_cmd()
        .args(["--", "cat"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();

    {
        let stdin = child.stdin.take().unwrap();
        let mut writer = BufWriter::new(stdin);
        writeln!(writer, "hello from stdin").unwrap();
        writeln!(writer, "second line").unwrap();
    }

    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "hello from stdin\nsecond line\n"
    );
}

#[test]
fn large_stdin() {
    let server = TestServer::start();
    let data: Vec<u8> = (0..1024 * 1024).map(|i| (i % 256) as u8).collect();

    let mut child = server
        .ncap_cmd()
        .args(["--", "wc", "-c"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    {
        let stdin = child.stdin.as_mut().unwrap();
        stdin.write_all(&data).unwrap();
    }

    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let count = String::from_utf8_lossy(&output.stdout).trim().to_string();
    assert_eq!(count, "1048576", "expected 1048576 bytes, got: {count}");
}

#[test]
fn large_stdout() {
    let server = TestServer::start();
    let output = server.run(&["--", "seq", "1", "10000"]);

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 10000);
    assert_eq!(lines[0], "1");
    assert_eq!(lines[9999], "10000");
}

#[test]
fn streaming_stdout() {
    let server = TestServer::start();

    let mut child = server
        .ncap_cmd()
        .args([
            "--",
            "sh",
            "-c",
            "echo line1; sleep 0.1; echo line2; sleep 0.1; echo line3; sleep 10",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();

    let mut reader = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();

    assert_line(&mut reader, &mut line, "line1");
    assert_line(&mut reader, &mut line, "line2");
    assert_line(&mut reader, &mut line, "line3");

    child.kill().unwrap();
    let _ = child.wait();
}

#[test]
fn concurrent_connections() {
    let server = TestServer::start();

    let socket = server.socket.clone();
    let handles: Vec<_> = (0..3)
        .map(|i| {
            let socket = socket.clone();
            spawn(move || {
                let output = Command::new(NCAP)
                    .args([
                        "--socket",
                        socket.to_str().unwrap(),
                        "--",
                        "sh",
                        "-c",
                        &format!("echo task-{i}"),
                    ])
                    .output()
                    .unwrap();
                assert!(output.status.success(), "task {i} failed");
                String::from_utf8(output.stdout).unwrap()
            })
        })
        .collect();

    let mut results: Vec<String> = handles.into_iter().map(|h| h.join().unwrap()).collect();

    results.sort();
    assert_eq!(results, vec!["task-0\n", "task-1\n", "task-2\n"]);
}

#[test]
fn rapid_sequential_commands() {
    let server = TestServer::start();

    for i in 0..20 {
        let output = server.run(&["--", "sh", "-c", &format!("echo cmd-{i}")]);
        assert!(output.status.success(), "command {i} failed");
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            format!("cmd-{i}")
        );
    }
}

#[test]
fn server_crash_during_execution() {
    let server = TestServer::start();

    let child = server
        .ncap_cmd()
        .args(["--", "sleep", "30"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    sleep(Duration::from_millis(100));
    server.kill();

    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success());
}

#[test]
fn bidirectional_interleaved() {
    let server = TestServer::start();

    let mut child = server
        .ncap_cmd()
        .args([
            "--",
            "sh",
            "-c",
            "i=1; while [ $i -le 3 ]; do echo tick-$i; sleep 0.1; i=$((i+1)); done & while read line; do echo ack-$line; done",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();

    assert_line(&mut reader, &mut line, "tick-1");
    write_line(&mut stdin, "a");
    assert_line(&mut reader, &mut line, "ack-a");
    assert_line(&mut reader, &mut line, "tick-2");
    write_line(&mut stdin, "b");
    assert_line(&mut reader, &mut line, "ack-b");
    assert_line(&mut reader, &mut line, "tick-3");
    write_line(&mut stdin, "c");
    assert_line(&mut reader, &mut line, "ack-c");

    drop(stdin);
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
}

#[test]
fn idle_connection() {
    let server = TestServer::start();

    let mut child = server
        .ncap_cmd()
        .args([
            "--",
            "sh",
            "-c",
            "while read line; do echo $line; sleep 0.05; done",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();

    let mut stdin = child.stdin.take().unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();

    write_line(&mut stdin, "first");
    assert_line(&mut reader, &mut line, "first");

    sleep(Duration::from_secs(1));

    write_line(&mut stdin, "second");
    assert_line(&mut reader, &mut line, "second");

    sleep(Duration::from_secs(1));

    write_line(&mut stdin, "third");
    assert_line(&mut reader, &mut line, "third");

    drop(stdin);
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
}

#[test]
fn server_refuses_after_shutdown() {
    let server = TestServer::start();

    Command::new("kill")
        .arg("-TERM")
        .arg(server.pid().to_string())
        .output()
        .unwrap();

    sleep(Duration::from_millis(100));

    let output = server.run(&["--", "echo", "should fail"]);
    assert!(!output.status.success());
}

#[test]
fn client_receives_server_stopping() {
    let server = TestServer::start();

    let client = server
        .ncap_cmd()
        .args(["--", "sleep", "10"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    sleep(Duration::from_millis(100));

    Command::new("kill")
        .arg("-TERM")
        .arg(server.pid().to_string())
        .output()
        .unwrap();

    let output = client.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(143));
}
