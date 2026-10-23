//! Server lifecycle over the real `ncap-server` binary: startup socket
//! ownership, shutdown drain, and the per-run log (spec/server.md § Startup,
//! § Shutdown, § Logging; spec/paths.md § Server log files).

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStderr, Command, ExitStatus, Stdio};
use std::thread::sleep;
use std::time::{Duration, Instant};

use bytes::BytesMut;
use libc::{SIGTERM, kill};
use tempfile::tempdir;
use tokio_util::codec::{Decoder, Encoder};

use nix_capsule::protocol::{Exit, FrameCodec, Message, Request, SignalMsg};

#[test]
fn second_server_refuses_connectable_socket_leaving_owner_serving()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let socket = dir.path().join("ncap.sock");
    let log_dir = dir.path().join("logs");
    let mut owner = spawn_server(&socket, &log_dir, 5, "error")?;
    wait_until_serving(&socket)?;

    let output = server_command(&socket, &log_dir, 5, "error").output()?;
    assert!(
        !output.status.success(),
        "second server exited {}",
        output.status
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(&*socket.to_string_lossy()),
        "error does not name the socket path: {stderr}"
    );

    wait_until_serving(&socket)?;
    owner.signal(SIGTERM);
    owner.wait_for_exit(Duration::from_secs(10))?;
    Ok(())
}

#[test]
fn replaces_stale_socket_and_binds() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let socket = dir.path().join("ncap.sock");
    let log_dir = dir.path().join("logs");
    let stale = std::os::unix::net::UnixListener::bind(&socket)?;
    drop(stale);

    let mut server = spawn_server(&socket, &log_dir, 5, "error")?;
    wait_until_serving(&socket)?;
    server.signal(SIGTERM);
    server.wait_for_exit(Duration::from_secs(10))?;
    Ok(())
}

#[test]
fn sigterm_delivers_server_stopping_to_every_live_connection()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let socket = dir.path().join("ncap.sock");
    let log_dir = dir.path().join("logs");
    let mut server = spawn_server(&socket, &log_dir, 5, "error")?;
    wait_until_serving(&socket)?;

    let mut idle = connect(&socket)?;
    let mut busy = connect(&socket)?;
    busy.send(exec_request("echo up; sleep 30", dir.path()))?;
    wait_for_child_up(&mut busy)?;

    server.signal(SIGTERM);
    assert_eq!(
        idle.recv()?,
        Some(Message::ServerStopping),
        "connection waiting for its first frame got no ServerStopping"
    );
    assert_eq!(
        busy.recv()?,
        Some(Message::ServerStopping),
        "connection with a running child got no ServerStopping"
    );
    server.wait_for_exit(Duration::from_secs(10))?;
    Ok(())
}

#[test]
fn sigint_delivers_server_stopping_to_idle_connection() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let socket = dir.path().join("ncap.sock");
    let log_dir = dir.path().join("logs");
    let mut server = spawn_server(&socket, &log_dir, 5, "error")?;
    wait_until_serving(&socket)?;

    let mut idle = connect(&socket)?;
    server.signal(libc::SIGINT);
    assert_eq!(
        idle.recv()?,
        Some(Message::ServerStopping),
        "SIGINT did not announce ServerStopping to the idle connection"
    );
    server.wait_for_exit(Duration::from_secs(10))?;
    Ok(())
}

#[test]
fn shutdown_terms_every_running_child() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let socket = dir.path().join("ncap.sock");
    let log_dir = dir.path().join("logs");
    let mut server = spawn_server(&socket, &log_dir, 5, "error")?;
    wait_until_serving(&socket)?;

    let mut first = connect(&socket)?;
    let mut second = connect(&socket)?;
    first.send(exec_request("echo up; sleep 30", dir.path()))?;
    second.send(exec_request("echo up; sleep 30", dir.path()))?;
    wait_for_child_up(&mut first)?;
    wait_for_child_up(&mut second)?;

    server.signal(SIGTERM);
    let (_, first_exit) = recv_until_terminal(&mut first)?;
    let (_, second_exit) = recv_until_terminal(&mut second)?;
    assert_eq!(
        first_exit,
        Message::Exit(Exit {
            code: None,
            signal: Some(15)
        }),
        "first child was not TERMed"
    );
    assert_eq!(
        second_exit,
        Message::Exit(Exit {
            code: None,
            signal: Some(15)
        }),
        "second child was not TERMed"
    );
    server.wait_for_exit(Duration::from_secs(10))?;
    Ok(())
}

#[test]
fn shutdown_group_term_reaches_grandchild() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let socket = dir.path().join("ncap.sock");
    let log_dir = dir.path().join("logs");
    let mut server = spawn_server(&socket, &log_dir, 5, "error")?;
    wait_until_serving(&socket)?;

    let marker = dir.path().join("grandchild-termed");
    let armed = dir.path().join("grandchild-armed");
    let script = format!(
        "sh -c 'trap \"touch {}; exit\" TERM; touch {}; sleep 30' & wait",
        marker.display(),
        armed.display()
    );
    let mut conn = connect(&socket)?;
    conn.send(exec_request(&script, dir.path()))?;
    wait_for_path(&armed, Duration::from_secs(5));
    assert!(armed.exists(), "grandchild never armed its TERM trap");

    server.signal(SIGTERM);
    wait_for_path(&marker, Duration::from_secs(5));
    assert!(marker.exists(), "grandchild never saw the group TERM");
    server.wait_for_exit(Duration::from_secs(10))?;
    Ok(())
}

#[test]
fn child_finishing_inside_drain_grace_delivers_exit_after_server_stopping()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let socket = dir.path().join("ncap.sock");
    let log_dir = dir.path().join("logs");
    let mut server = spawn_server(&socket, &log_dir, 5, "error")?;
    wait_until_serving(&socket)?;

    let mut conn = connect(&socket)?;
    conn.send(exec_request("trap '' TERM; echo up; sleep 1", dir.path()))?;
    wait_for_child_up(&mut conn)?;

    server.signal(SIGTERM);
    let (before, terminal) = recv_until_terminal(&mut conn)?;
    assert!(
        before.contains(&Message::ServerStopping),
        "no ServerStopping before the terminal frame: {before:?}"
    );
    assert_eq!(
        terminal,
        Message::Exit(Exit {
            code: Some(0),
            signal: None
        }),
        "child finishing inside the drain grace did not deliver its exit"
    );
    server.wait_for_exit(Duration::from_secs(10))?;
    Ok(())
}

#[test]
fn server_stopping_precedes_exit_when_pipes_drained_but_child_unreaped()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let socket = dir.path().join("ncap.sock");
    let log_dir = dir.path().join("logs");
    let mut server = spawn_server(&socket, &log_dir, 30, "error")?;
    wait_until_serving(&socket)?;

    let mut conn = connect(&socket)?;
    conn.send(exec_request(
        "echo up; exec >/dev/null 2>/dev/null; sleep 30",
        dir.path(),
    ))?;
    wait_for_child_up(&mut conn)?;

    server.signal(SIGTERM);
    let (before, terminal) = recv_until_terminal(&mut conn)?;
    assert!(
        before.contains(&Message::ServerStopping),
        "no ServerStopping before the terminal frame: {before:?}"
    );
    assert_eq!(
        terminal,
        Message::Exit(Exit {
            code: None,
            signal: Some(15)
        }),
        "pipes-drained child did not deliver its exit"
    );
    server.wait_for_exit(Duration::from_secs(10))?;
    Ok(())
}

#[test]
fn deadline_drops_connection_whose_child_misses_drain_grace()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let socket = dir.path().join("ncap.sock");
    let log_dir = dir.path().join("logs");
    let mut server = spawn_server(&socket, &log_dir, 1, "error")?;
    wait_until_serving(&socket)?;

    let mut conn = connect(&socket)?;
    let pid = hang_child(&mut conn, dir.path())?;

    server.signal(SIGTERM);
    let frames = recv_until_eof(&mut conn)?;
    assert_eq!(
        frames,
        vec![Message::ServerStopping],
        "connection whose child missed the drain grace was not dropped after ServerStopping"
    );
    server.wait_for_exit(Duration::from_secs(5))?;
    kill_group(pid);
    Ok(())
}

#[test]
fn timeout_zero_skips_awaiting_hung_child() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let socket = dir.path().join("ncap.sock");
    let log_dir = dir.path().join("logs");
    let mut server = spawn_server(&socket, &log_dir, 0, "error")?;
    wait_until_serving(&socket)?;

    let mut conn = connect(&socket)?;
    let pid = hang_child(&mut conn, dir.path())?;

    server.signal(SIGTERM);
    server.wait_for_exit(Duration::from_secs(3))?;
    assert!(!socket.exists(), "socket file left behind");
    let frames = recv_until_eof(&mut conn)?;
    assert!(
        !frames
            .iter()
            .any(|frame| matches!(frame, Message::Exit(_) | Message::Error(_))),
        "dropped connection delivered a terminal frame: {frames:?}"
    );
    kill_group(pid);
    Ok(())
}

#[test]
fn clean_shutdown_removes_socket_file_keeping_parent_dir() -> Result<(), Box<dyn std::error::Error>>
{
    let dir = tempdir()?;
    let socket_dir = dir.path().join("run");
    std::fs::create_dir(&socket_dir)?;
    let socket = socket_dir.join("ncap.sock");
    let log_dir = dir.path().join("logs");
    let mut server = spawn_server(&socket, &log_dir, 5, "error")?;
    wait_until_serving(&socket)?;

    server.signal(SIGTERM);
    server.wait_for_exit(Duration::from_secs(10))?;
    assert!(!socket.exists(), "socket file left behind");
    assert!(socket_dir.exists(), "socket parent dir was removed");
    Ok(())
}

#[test]
fn writes_one_log_file_per_run_matching_server_log_pattern()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let socket = dir.path().join("ncap.sock");
    let log_dir = dir.path().join("logs");
    let mut server = spawn_server(&socket, &log_dir, 5, "debug")?;
    wait_until_serving(&socket)?;
    server.signal(SIGTERM);
    server.wait_for_exit(Duration::from_secs(10))?;

    let entries = dir_entries(&log_dir)?;
    assert_eq!(entries.len(), 1, "one log file per run: {entries:?}");
    assert!(
        is_server_log_name(&entries[0]),
        "log file name is not `ncap-server-<digits>.log`: {:?}",
        entries[0].file_name()
    );
    Ok(())
}

#[test]
fn runs_started_in_same_second_write_distinct_log_files() -> Result<(), Box<dyn std::error::Error>>
{
    let dir = tempdir()?;
    let log_dir = dir.path().join("logs");
    let mut first = spawn_server(&dir.path().join("a.sock"), &log_dir, 5, "error")?;
    wait_until_serving(&dir.path().join("a.sock"))?;
    let mut second = spawn_server(&dir.path().join("b.sock"), &log_dir, 5, "error")?;
    wait_until_serving(&dir.path().join("b.sock"))?;

    first.signal(SIGTERM);
    second.signal(SIGTERM);
    first.wait_for_exit(Duration::from_secs(10))?;
    second.wait_for_exit(Duration::from_secs(10))?;

    let entries = dir_entries(&log_dir)?;
    assert_eq!(entries.len(), 2, "two runs, two log files: {entries:?}");
    assert_ne!(
        entries[0], entries[1],
        "runs started in the same second share a log file name"
    );
    Ok(())
}

#[test]
fn creates_missing_log_dir() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let socket = dir.path().join("ncap.sock");
    let log_dir = dir.path().join("missing/nested/logs");
    let mut server = spawn_server(&socket, &log_dir, 5, "error")?;
    wait_until_serving(&socket)?;

    server.signal(SIGTERM);
    server.wait_for_exit(Duration::from_secs(10))?;
    assert!(log_dir.is_dir(), "log dir was not created");
    let entries = dir_entries(&log_dir)?;
    assert_eq!(entries.len(), 1, "run wrote no log file: {entries:?}");
    Ok(())
}

#[test]
fn stamps_every_log_line_with_utc_rfc3339() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let socket = dir.path().join("ncap.sock");
    let log_dir = dir.path().join("logs");
    let mut server = spawn_server(&socket, &log_dir, 5, "debug")?;
    wait_until_serving(&socket)?;
    server.signal(SIGTERM);
    server.wait_for_exit(Duration::from_secs(10))?;

    let lines = all_log_lines(&log_dir)?;
    assert!(!lines.is_empty(), "run wrote no log lines at debug");
    assert!(
        lines.iter().all(|line| has_rfc3339_stamp(line)),
        "log line without an `[YYYY-MM-DDTHH:MM:SSZ]` stamp: {lines:#?}"
    );
    Ok(())
}

#[test]
fn mirrors_log_lines_to_stderr() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let socket = dir.path().join("ncap.sock");
    let log_dir = dir.path().join("logs");
    let mut server = spawn_server(&socket, &log_dir, 5, "debug")?;
    wait_until_serving(&socket)?;
    server.signal(SIGTERM);
    server.wait_for_exit(Duration::from_secs(10))?;

    let file_lines = all_log_lines(&log_dir)?;
    let stderr_lines: Vec<String> = server.stderr_text().lines().map(str::to_string).collect();
    assert!(!file_lines.is_empty(), "run wrote no log lines at debug");
    assert_eq!(
        stderr_lines, file_lines,
        "stderr mirror and log file disagree"
    );
    Ok(())
}

#[test]
fn version_probe_line_written_at_debug_level() -> Result<(), Box<dyn std::error::Error>> {
    let (with_file, with_stderr) =
        run_first_frame_scenario(Some(Message::RequestVersion), "debug")?;
    let (without_file, without_stderr) = run_first_frame_scenario(None, "debug")?;
    assert!(
        with_file > without_file,
        "debug log file: probe run wrote {with_file} lines, silent run {without_file}"
    );
    assert!(
        with_stderr > without_stderr,
        "debug stderr: probe run wrote {with_stderr} lines, silent run {without_stderr}"
    );
    Ok(())
}

#[test]
fn version_probe_line_suppressed_at_info_level() -> Result<(), Box<dyn std::error::Error>> {
    let (with_file, with_stderr) = run_first_frame_scenario(Some(Message::RequestVersion), "info")?;
    let (without_file, without_stderr) = run_first_frame_scenario(None, "info")?;
    assert_eq!(
        with_file, without_file,
        "info log file: probe run wrote {with_file} lines, silent run {without_file}"
    );
    assert_eq!(
        with_stderr, without_stderr,
        "info stderr: probe run wrote {with_stderr} lines, silent run {without_stderr}"
    );
    Ok(())
}

#[test]
fn kill_failure_warning_written_at_warning_level() -> Result<(), Box<dyn std::error::Error>> {
    let out_of_range = SignalMsg { signal: 255 };
    let kills_child = SignalMsg { signal: 15 };
    let (with_file, with_stderr) =
        run_signal_scenario(&[out_of_range.clone(), kills_child.clone()], "warning")?;
    let (without_file, without_stderr) = run_signal_scenario(&[kills_child], "warning")?;
    assert_eq!(
        with_file,
        without_file + 1,
        "warning log file: failing kill wrote {with_file} lines, clean run {without_file}"
    );
    assert_eq!(
        with_stderr,
        without_stderr + 1,
        "warning stderr: failing kill wrote {with_stderr} lines, clean run {without_stderr}"
    );
    Ok(())
}

#[test]
fn kill_failure_warning_suppressed_at_error_level() -> Result<(), Box<dyn std::error::Error>> {
    let out_of_range = SignalMsg { signal: 255 };
    let kills_child = SignalMsg { signal: 15 };
    let (with_file, with_stderr) =
        run_signal_scenario(&[out_of_range.clone(), kills_child.clone()], "error")?;
    let (without_file, without_stderr) = run_signal_scenario(&[kills_child], "error")?;
    assert_eq!(
        with_file, without_file,
        "error log file: failing kill wrote {with_file} lines, clean run {without_file}"
    );
    assert_eq!(
        with_stderr, without_stderr,
        "error stderr: failing kill wrote {with_stderr} lines, clean run {without_stderr}"
    );
    Ok(())
}

fn spawn_server(
    socket: &Path,
    log_dir: &Path,
    timeout_secs: u64,
    log_level: &str,
) -> Result<Server, Box<dyn std::error::Error>> {
    let mut child = server_command(socket, log_dir, timeout_secs, log_level).spawn()?;
    let stderr = child.stderr.take();
    Ok(Server {
        child,
        stderr,
        stderr_text: String::new(),
    })
}

fn server_command(socket: &Path, log_dir: &Path, timeout_secs: u64, log_level: &str) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ncap-server"));
    command
        .arg("--socket")
        .arg(socket)
        .arg("--log-dir")
        .arg(log_dir)
        .arg("--timeout")
        .arg(timeout_secs.to_string())
        .arg("--log-level")
        .arg(log_level)
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    command
}

/// The spawned `ncap-server` process: SIGTERM on drop so a failing test
/// still runs the server's own shutdown (its child groups die with it).
struct Server {
    child: Child,
    stderr: Option<ChildStderr>,
    stderr_text: String,
}

impl Server {
    fn signal(&self, signum: i32) {
        unsafe { kill(self.child.id() as i32, signum) };
    }

    /// Wait out `bound`, then drain the captured stderr. The bound is the
    /// test's own deadline: a server still running after it is the failure.
    fn wait_for_exit(&mut self, bound: Duration) -> Result<ExitStatus, Box<dyn std::error::Error>> {
        let deadline = Instant::now() + bound;
        loop {
            if let Some(status) = self.child.try_wait()? {
                if let Some(mut stderr) = self.stderr.take() {
                    stderr.read_to_string(&mut self.stderr_text)?;
                }
                return Ok(status);
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "server (pid {}) still running after {bound:?}",
                    self.child.id()
                )
                .into());
            }
            sleep(Duration::from_millis(20));
        }
    }

    fn stderr_text(&self) -> &str {
        &self.stderr_text
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        if self.child.try_wait().ok().flatten().is_some() {
            return;
        }
        unsafe { kill(self.child.id() as i32, SIGTERM) };
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            if self.child.try_wait().ok().flatten().is_some() {
                return;
            }
            sleep(Duration::from_millis(20));
        }
        unsafe { kill(self.child.id() as i32, libc::SIGKILL) };
        let _ = self.child.wait();
    }
}

/// One client connection with its own frame buffer: a single read may hold
/// several frames, so buffered bytes carry across `recv` calls.
struct Connection {
    stream: UnixStream,
    buffered: BytesMut,
}

impl Connection {
    fn send(&mut self, message: Message) -> Result<(), Box<dyn std::error::Error>> {
        let mut codec = FrameCodec;
        let mut bytes = BytesMut::new();
        codec.encode(message.into_frame()?, &mut bytes)?;
        self.stream.write_all(&bytes)?;
        Ok(())
    }

    fn recv(&mut self) -> Result<Option<Message>, Box<dyn std::error::Error>> {
        let mut codec = FrameCodec;
        let mut chunk = [0u8; 4096];
        loop {
            if let Some(frame) = codec.decode(&mut self.buffered)? {
                return Ok(Some(Message::from_frame(frame)?));
            }
            match self.stream.read(&mut chunk) {
                Ok(0) => return Ok(None),
                Ok(n) => self.buffered.extend_from_slice(&chunk[..n]),
                Err(err)
                    if err.kind() == std::io::ErrorKind::WouldBlock
                        || err.kind() == std::io::ErrorKind::TimedOut =>
                {
                    return Err("timed out waiting for a frame from the server".into());
                }
                Err(err) => return Err(err.into()),
            }
        }
    }
}

fn connect(socket: &Path) -> Result<Connection, Box<dyn std::error::Error>> {
    let stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    Ok(Connection {
        stream,
        buffered: BytesMut::new(),
    })
}

fn exec_request(script: &str, cwd: &Path) -> Message {
    Message::Request(Request {
        command: "/bin/sh".to_string(),
        args: vec!["-c".to_string(), script.to_string()],
        cwd: cwd.to_string_lossy().into_owned(),
        env: Vec::new(),
    })
}

/// Frames before the connection's one terminal frame, plus that frame
/// (spec/protocol.md § Connection lifecycle).
fn recv_until_terminal(
    conn: &mut Connection,
) -> Result<(Vec<Message>, Message), Box<dyn std::error::Error>> {
    let mut before = Vec::new();
    loop {
        let Some(message) = conn.recv()? else {
            return Err(
                format!("connection closed without a terminal frame; saw {before:?}").into(),
            );
        };
        if matches!(message, Message::Exit(_) | Message::Error(_)) {
            return Ok((before, message));
        }
        before.push(message);
    }
}

fn recv_until_eof(conn: &mut Connection) -> Result<Vec<Message>, Box<dyn std::error::Error>> {
    let mut frames = Vec::new();
    while let Some(message) = conn.recv()? {
        frames.push(message);
    }
    Ok(frames)
}

/// Read the fixture child's `echo up` ping: its arrival proves the child
/// process is running — and any trap armed — before the test signals the
/// server, whose shutdown TERMs the group at once.
fn wait_for_child_up(conn: &mut Connection) -> Result<(), Box<dyn std::error::Error>> {
    match conn.recv()? {
        Some(Message::Stdout(_)) => Ok(()),
        other => Err(format!("expected the child's startup ping, got {other:?}").into()),
    }
}

/// Start a child that cannot die to the shutdown TERM and outlives any
/// drain grace. It echoes `$$` only after arming the trap, so the pid
/// arriving on stdout proves the child the server may leave behind; the
/// caller kills that group when done.
fn hang_child(conn: &mut Connection, cwd: &Path) -> Result<u32, Box<dyn std::error::Error>> {
    conn.send(exec_request("trap '' TERM; echo $$; exec sleep 8", cwd))?;
    let frame = conn.recv()?;
    let Some(Message::Stdout(bytes)) = frame else {
        return Err(format!("expected the child pid on stdout, got {frame:?}").into());
    };
    Ok(String::from_utf8(bytes)?.trim().parse()?)
}

fn kill_group(pid: u32) {
    unsafe { kill(-(pid as i32), libc::SIGKILL) };
}

fn wait_for_path(path: &Path, bound: Duration) {
    let deadline = Instant::now() + bound;
    while Instant::now() < deadline {
        if path.exists() {
            return;
        }
        sleep(Duration::from_millis(10));
    }
}

/// Poll a version probe until the Server answers; the same call shows a
/// running owner still serving after a refused second Server.
fn wait_until_serving(socket: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let err = match version_probe(socket) {
            Ok(()) => return Ok(()),
            Err(err) => err,
        };
        if Instant::now() >= deadline {
            return Err(format!(
                "server at {} never served a version probe: {err}",
                socket.display()
            )
            .into());
        }
        sleep(Duration::from_millis(10));
    }
}

fn version_probe(socket: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut conn = connect(socket)?;
    conn.send(Message::RequestVersion)?;
    match conn.recv()? {
        Some(Message::ServerVersion(_)) => Ok(()),
        other => Err(format!("version probe got {other:?}").into()),
    }
}

/// One full run whose first connection sends `first_frame` (then closes) or
/// stays silent, followed by a version probe that syncs the accept queue,
/// then SIGTERM and a clean wait. The version probe's reply comes only after
/// its log line is written, so the returned (file, stderr) line counts
/// differ between calls by exactly the first connection's own log events.
fn run_first_frame_scenario(
    first_frame: Option<Message>,
    log_level: &str,
) -> Result<(usize, usize), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let socket = dir.path().join("ncap.sock");
    let log_dir = dir.path().join("logs");
    let mut server = spawn_server(&socket, &log_dir, 5, log_level)?;
    wait_until_serving(&socket)?;

    let mut first = connect(&socket)?;
    if let Some(frame) = first_frame {
        first.send(frame)?;
        let reply = first.recv()?;
        assert!(
            matches!(reply, Some(Message::ServerVersion(_))),
            "probe reply: {reply:?}"
        );
    }
    drop(first);

    version_probe(&socket)?;
    server.signal(SIGTERM);
    server.wait_for_exit(Duration::from_secs(10))?;
    count_lines(&log_dir, &server)
}

/// One full run that sends `signals` to a sleeping child and waits for its
/// terminal frame. The last signal must kill the child: its terminal frame
/// syncs the run, so any earlier warning line is already written before
/// the returned (file, stderr) line counts are read.
fn run_signal_scenario(
    signals: &[SignalMsg],
    log_level: &str,
) -> Result<(usize, usize), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let socket = dir.path().join("ncap.sock");
    let log_dir = dir.path().join("logs");
    let mut server = spawn_server(&socket, &log_dir, 5, log_level)?;
    wait_until_serving(&socket)?;

    let mut conn = connect(&socket)?;
    conn.send(exec_request("sleep 30", dir.path()))?;
    for signal in signals {
        conn.send(Message::Signal(signal.clone()))?;
    }
    let _ = recv_until_terminal(&mut conn)?;
    drop(conn);

    server.signal(SIGTERM);
    server.wait_for_exit(Duration::from_secs(10))?;
    count_lines(&log_dir, &server)
}

fn count_lines(
    log_dir: &Path,
    server: &Server,
) -> Result<(usize, usize), Box<dyn std::error::Error>> {
    Ok((
        all_log_lines(log_dir)?.len(),
        server.stderr_text().lines().count(),
    ))
}

fn dir_entries(dir: &Path) -> Result<Vec<PathBuf>, Box<dyn std::error::Error>> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)?
        .map(|entry| Ok(entry?.path()))
        .collect::<Result<_, Box<dyn std::error::Error>>>()?;
    entries.sort();
    Ok(entries)
}

fn is_server_log_name(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let Some(stamp) = name
        .strip_prefix("ncap-server-")
        .and_then(|name| name.strip_suffix(".log"))
    else {
        return false;
    };
    !stamp.is_empty() && stamp.bytes().all(|byte| byte.is_ascii_digit())
}

fn read_lines(path: &Path) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    Ok(std::fs::read_to_string(path)?
        .lines()
        .map(str::to_string)
        .collect())
}

fn all_log_lines(log_dir: &Path) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mut lines = Vec::new();
    for file in dir_entries(log_dir)? {
        lines.extend(read_lines(&file)?);
    }
    Ok(lines)
}

/// `[YYYY-MM-DDTHH:MM:SSZ]` — 22 bytes (spec/server.md § Logging).
fn has_rfc3339_stamp(line: &str) -> bool {
    let bytes = line.as_bytes();
    bytes.len() >= 22
        && bytes[0] == b'['
        && bytes[21] == b']'
        && bytes[20] == b'Z'
        && bytes[5] == b'-'
        && bytes[8] == b'-'
        && bytes[11] == b'T'
        && bytes[14] == b':'
        && bytes[17] == b':'
        && bytes[1..5].iter().all(u8::is_ascii_digit)
        && bytes[6..8].iter().all(u8::is_ascii_digit)
        && bytes[9..11].iter().all(u8::is_ascii_digit)
        && bytes[12..14].iter().all(u8::is_ascii_digit)
        && bytes[15..17].iter().all(u8::is_ascii_digit)
        && bytes[18..20].iter().all(u8::is_ascii_digit)
}
