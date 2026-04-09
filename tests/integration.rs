use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::thread::{sleep, spawn};
use std::time::Duration;

const NCAP: &str = env!("CARGO_BIN_EXE_ncap");
const NCAP_SERVER: &str = env!("CARGO_BIN_EXE_ncap-server");

struct TestServer {
    socket: PathBuf,
    _dir: tempfile::TempDir,
    child: Child,
}

impl TestServer {
    fn start() -> Self {
        let dir = tempfile::tempdir().expect("failed to create tempdir");
        let socket = dir.path().join("test.sock");

        let child = Command::new(NCAP_SERVER)
            .arg("--socket")
            .arg(&socket)
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn ncap-server");

        sleep(Duration::from_millis(100));

        Self {
            socket,
            _dir: dir,
            child,
        }
    }

    fn ncap_cmd(&self) -> Command {
        let mut cmd = Command::new(NCAP);
        cmd.arg("--socket").arg(&self.socket);
        cmd
    }

    fn kill(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        std::mem::forget(self);
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn stdout_bridging() {
    let server = TestServer::start();
    let output = server
        .ncap_cmd()
        .args(["--", "echo", "hello world"])
        .output()
        .expect("failed to run ncap");

    assert!(output.status.success());
    assert_eq!(output.stdout, b"hello world\n");
}

#[test]
fn stderr_bridging() {
    let server = TestServer::start();
    let output = server
        .ncap_cmd()
        .args(["--", "sh", "-c", "echo error stream >&2"])
        .output()
        .expect("failed to run ncap");

    assert!(output.status.success());
    assert_eq!(output.stderr, b"error stream\n");
}

#[test]
fn interleaved_stdout_stderr() {
    let server = TestServer::start();
    let output = server
        .ncap_cmd()
        .args([
            "--",
            "sh",
            "-c",
            "echo out1; echo err1 >&2; echo out2; echo err2 >&2",
        ])
        .output()
        .expect("failed to run ncap");

    assert!(output.status.success());

    assert_eq!(String::from_utf8_lossy(&output.stdout), "out1\nout2\n");
    assert_eq!(String::from_utf8_lossy(&output.stderr), "err1\nerr2\n");
}

#[test]
fn exit_code_success() {
    let server = TestServer::start();
    let output = server
        .ncap_cmd()
        .args(["--", "true"])
        .output()
        .expect("failed to run ncap");

    assert_eq!(output.status.code(), Some(0));
}

#[test]
fn exit_code_failure() {
    let server = TestServer::start();
    let output = server
        .ncap_cmd()
        .args(["--", "false"])
        .output()
        .expect("failed to run ncap");

    assert_eq!(output.status.code(), Some(1));
}

#[test]
fn exit_code_custom() {
    let server = TestServer::start();
    let output = server
        .ncap_cmd()
        .args(["--", "sh", "-c", "exit 42"])
        .output()
        .expect("failed to run ncap");

    assert_eq!(output.status.code(), Some(42));
}

#[test]
fn env_override() {
    let server = TestServer::start();
    let output = server
        .ncap_cmd()
        .args([
            "--env",
            "CAPSULE_TEST=bar",
            "--",
            "sh",
            "-c",
            "echo $CAPSULE_TEST",
        ])
        .output()
        .expect("failed to run ncap");

    assert!(output.status.success());
    assert_eq!(output.stdout, b"bar\n");
}

#[test]
fn cwd_override() {
    let server = TestServer::start();
    let output = server
        .ncap_cmd()
        .args(["--cwd", "/tmp", "--", "pwd"])
        .output()
        .expect("failed to run ncap");

    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "/tmp");
}

#[test]
fn spawn_failure() {
    let server = TestServer::start();
    let output = server
        .ncap_cmd()
        .args(["--", "this-command-does-not-exist"])
        .output()
        .expect("failed to run ncap");

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("server error"),
        "expected server error, got: {stderr}"
    );
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
        .expect("failed to run ncap");

    {
        let stdin = child.stdin.take().unwrap();
        let mut writer = BufWriter::new(stdin);
        writeln!(writer, "hello from stdin").unwrap();
        writeln!(writer, "second line").unwrap();
    }

    let output = child.wait_with_output().expect("failed to wait for ncap");
    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(stdout, "hello from stdin\nsecond line\n");
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
        .expect("failed to run ncap");

    {
        let stdin = child.stdin.as_mut().expect("missing stdin");
        stdin.write_all(&data).expect("failed to write stdin");
    }

    let output = child.wait_with_output().expect("failed to wait for ncap");

    assert!(output.status.success());
    let count = String::from_utf8_lossy(&output.stdout).trim().to_string();
    assert_eq!(count, "1048576", "expected 1048576 bytes, got: {count}");
}

#[test]
fn large_stdout() {
    let server = TestServer::start();
    let output = server
        .ncap_cmd()
        .args(["--", "seq", "1", "10000"])
        .output()
        .expect("failed to run ncap");

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
        .expect("failed to run ncap");

    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);

    let mut line = String::new();

    reader.read_line(&mut line).unwrap();
    assert_eq!(line.trim(), "line1");

    line.clear();
    reader.read_line(&mut line).unwrap();
    assert_eq!(line.trim(), "line2");

    line.clear();
    reader.read_line(&mut line).unwrap();
    assert_eq!(line.trim(), "line3");

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
                    .expect("failed to run ncap");
                assert!(output.status.success(), "task {i} failed");
                String::from_utf8(output.stdout).unwrap()
            })
        })
        .collect();

    let mut results: Vec<String> = handles
        .into_iter()
        .map(|h| h.join().expect("thread panicked"))
        .collect();

    results.sort();
    assert_eq!(results, vec!["task-0\n", "task-1\n", "task-2\n"]);
}

#[test]
fn rapid_sequential_commands() {
    let server = TestServer::start();

    for i in 0..20 {
        let output = server
            .ncap_cmd()
            .args(["--", "sh", "-c", &format!("echo cmd-{i}")])
            .output()
            .expect("failed to run ncap");

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
        .expect("failed to run ncap");

    sleep(Duration::from_millis(200));

    server.kill();

    let output = child.wait_with_output().expect("failed to wait for ncap");
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
        .expect("failed to run ncap");

    let mut child_stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();

    reader.read_line(&mut line).unwrap();
    assert_eq!(line.trim(), "tick-1");

    writeln!(child_stdin, "a").unwrap();
    child_stdin.flush().unwrap();
    line.clear();
    reader.read_line(&mut line).unwrap();
    assert_eq!(line.trim(), "ack-a");

    line.clear();
    reader.read_line(&mut line).unwrap();
    assert_eq!(line.trim(), "tick-2");

    writeln!(child_stdin, "b").unwrap();
    child_stdin.flush().unwrap();
    line.clear();
    reader.read_line(&mut line).unwrap();
    assert_eq!(line.trim(), "ack-b");

    line.clear();
    reader.read_line(&mut line).unwrap();
    assert_eq!(line.trim(), "tick-3");

    writeln!(child_stdin, "c").unwrap();
    child_stdin.flush().unwrap();
    line.clear();
    reader.read_line(&mut line).unwrap();
    assert_eq!(line.trim(), "ack-c");

    drop(child_stdin);
    let output = child.wait_with_output().expect("failed to wait for ncap");
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
        .expect("failed to run ncap");

    let mut child_stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);

    writeln!(child_stdin, "first").unwrap();
    child_stdin.flush().unwrap();
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    assert_eq!(line.trim(), "first");

    sleep(Duration::from_secs(2));

    line.clear();
    writeln!(child_stdin, "second").unwrap();
    child_stdin.flush().unwrap();
    reader.read_line(&mut line).unwrap();
    assert_eq!(line.trim(), "second");

    sleep(Duration::from_secs(2));

    line.clear();
    writeln!(child_stdin, "third").unwrap();
    child_stdin.flush().unwrap();
    reader.read_line(&mut line).unwrap();
    assert_eq!(line.trim(), "third");

    drop(child_stdin);
    let output = child.wait_with_output().expect("failed to wait for ncap");
    assert!(output.status.success());
}
