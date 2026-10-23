use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use tempfile::tempdir;
use test_case::test_case;
use tokio::net::{UnixListener, UnixStream};
use tokio_util::codec::Framed;

use nix_capsule::protocol::{Exit, FrameCodec, Message, Request, SignalMsg};

#[test_case(libc::SIGINT, 2 ; "sigint_sends_one_verbatim_signal_frame")]
#[test_case(libc::SIGTERM, 15 ; "sigterm_sends_one_verbatim_signal_frame")]
#[tokio::test]
async fn host_signal_sends_one_verbatim_signal_frame(
    sig: i32,
    signal: u8,
) -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let sock = dir.path().join("ncap.sock");
    let listener = UnixListener::bind(&sock)?;
    let (mut child, mut conn) = open_session(&listener, &sock, dir.path()).await;

    kill(child.id(), sig);
    let got = recv(&mut conn).await;

    assert_eq!(got, Message::Signal(SignalMsg { signal }));
    send(&mut conn, exit(0)).await;
    child.wait()?;
    Ok(())
}

#[tokio::test]
async fn repeated_host_signals_forward_one_frame_each() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let sock = dir.path().join("ncap.sock");
    let listener = UnixListener::bind(&sock)?;
    let (mut child, mut conn) = open_session(&listener, &sock, dir.path()).await;

    kill(child.id(), libc::SIGINT);
    let first = recv(&mut conn).await;
    kill(child.id(), libc::SIGINT);
    let second = recv(&mut conn).await;

    assert_eq!(
        vec![first, second],
        vec![
            Message::Signal(SignalMsg { signal: 2 }),
            Message::Signal(SignalMsg { signal: 2 }),
        ]
    );
    send(&mut conn, exit(0)).await;
    child.wait()?;
    Ok(())
}

#[tokio::test]
async fn relay_keeps_streaming_and_exits_with_the_childs_code()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let sock = dir.path().join("ncap.sock");
    let listener = UnixListener::bind(&sock)?;
    let (mut child, mut conn) = open_session(&listener, &sock, dir.path()).await;

    kill(child.id(), libc::SIGINT);
    let _signal = recv(&mut conn).await;
    send(&mut conn, Message::Stdout(b"hello".to_vec())).await;
    send(&mut conn, exit(7)).await;

    let status = child.wait()?;
    let mut out = String::new();
    child
        .stdout
        .take()
        .expect("the fixture pipes stdout")
        .read_to_string(&mut out)?;

    assert_eq!(out, "hello");
    assert_eq!(status.code(), Some(7));
    Ok(())
}

#[tokio::test]
async fn relayed_signal_never_travels_as_a_terminal_frame() -> Result<(), Box<dyn std::error::Error>>
{
    let dir = tempdir()?;
    let sock = dir.path().join("ncap.sock");
    let listener = UnixListener::bind(&sock)?;
    let mut child = spawn_ncap(&sock, dir.path(), Stdio::null());
    let mut conn = accept_conn(&listener).await;

    let mut stream = vec![recv(&mut conn).await, recv(&mut conn).await];
    kill(child.id(), libc::SIGINT);
    stream.push(recv(&mut conn).await);
    send(&mut conn, exit(0)).await;
    stream.extend(drain(&mut conn).await);
    child.wait()?;

    let terminal: Vec<&Message> = stream
        .iter()
        .filter(|m| {
            matches!(
                m,
                Message::Exit(_) | Message::Error(_) | Message::ServerStopping
            )
        })
        .collect();

    assert!(
        terminal.is_empty(),
        "expected the Client to send no terminal frames, got {terminal:?}"
    );
    Ok(())
}

#[tokio::test]
async fn signal_frame_send_failure_exits_1() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let sock = dir.path().join("ncap.sock");
    let listener = UnixListener::bind(&sock)?;
    let (mut child, conn) = open_session(&listener, &sock, dir.path()).await;

    // Half-close our read side: the Client's `Signal` send fails with EPIPE
    // while its own read side sees no EOF — the clean-close 143 race is closed.
    unsafe {
        libc::shutdown(conn.get_ref().as_raw_fd(), libc::SHUT_RD);
    }
    kill(child.id(), libc::SIGINT);

    let status = child.wait()?;

    assert_eq!(status.code(), Some(1));
    // `conn` is held past the wait so the Client's read side never sees EOF.
    drop(conn);
    Ok(())
}

#[tokio::test]
async fn sighup_kills_the_client_outright() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let sock = dir.path().join("ncap.sock");
    let listener = UnixListener::bind(&sock)?;
    let (mut child, mut conn) = open_session(&listener, &sock, dir.path()).await;

    kill(child.id(), libc::SIGHUP);

    let status = child.wait()?;
    let relayed = drain(&mut conn).await;

    assert_eq!(status.signal(), Some(libc::SIGHUP));
    assert!(
        !relayed.iter().any(|m| matches!(m, Message::Signal(_))),
        "expected no relayed Signal frame, got {relayed:?}"
    );
    Ok(())
}

#[tokio::test]
async fn job_control_signals_are_not_relayed() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let sock = dir.path().join("ncap.sock");
    let listener = UnixListener::bind(&sock)?;
    let (mut child, mut conn) = open_session(&listener, &sock, dir.path()).await;

    kill(child.id(), libc::SIGTSTP);
    wait_until_stopped(&mut child).await;
    kill(child.id(), libc::SIGCONT);
    kill(child.id(), libc::SIGINT);
    let first = recv(&mut conn).await;

    send(&mut conn, exit(0)).await;
    let mut frames = vec![first];
    frames.extend(drain(&mut conn).await);
    child.wait()?;

    let signals: Vec<Message> = frames
        .into_iter()
        .filter(|m| matches!(m, Message::Signal(_)))
        .collect();

    assert_eq!(signals, vec![Message::Signal(SignalMsg { signal: 2 })]);
    Ok(())
}

#[tokio::test]
async fn relay_still_flows_after_stdin_eof() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let sock = dir.path().join("ncap.sock");
    let listener = UnixListener::bind(&sock)?;
    let mut child = spawn_ncap(&sock, dir.path(), Stdio::null());
    let mut conn = accept_conn(&listener).await;
    let _request = recv(&mut conn).await;

    let eof = recv(&mut conn).await;

    kill(child.id(), libc::SIGINT);
    let signal = recv(&mut conn).await;

    assert_eq!(
        vec![eof, signal],
        vec![
            Message::Stdin(vec![]),
            Message::Signal(SignalMsg { signal: 2 }),
        ]
    );
    send(&mut conn, exit(0)).await;
    child.wait()?;
    Ok(())
}

#[tokio::test]
async fn host_stdin_keeps_pumping_across_host_signals() -> Result<(), Box<dyn std::error::Error>> {
    struct ChildGuard(Child);

    impl std::ops::Deref for ChildGuard {
        type Target = Child;

        fn deref(&self) -> &Child {
            &self.0
        }
    }

    impl std::ops::DerefMut for ChildGuard {
        fn deref_mut(&mut self) -> &mut Child {
            &mut self.0
        }
    }

    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    let dir = tempdir()?;
    let sock = dir.path().join("ncap.sock");
    let listener = UnixListener::bind(&sock)?;
    let mut child = ChildGuard(spawn_ncap(&sock, dir.path(), Stdio::piped()));
    let mut conn = accept_conn(&listener).await;
    let _request = recv(&mut conn).await;

    let mut host_stdin = child.stdin.take().expect("the fixture pipes stdin");
    host_stdin.write_all(b"before")?;
    host_stdin.flush()?;
    // Sync on the first chunk's arrival so "after" cannot coalesce with it.
    let first = recv(&mut conn).await;

    kill(child.id(), libc::SIGINT);
    kill(child.id(), libc::SIGINT);
    host_stdin.write_all(b"after")?;
    host_stdin.flush()?;

    let mut frames = vec![first];
    loop {
        let msg = recv(&mut conn).await;
        let arrived = matches!(msg, Message::Stdin(_));
        frames.push(msg);
        if arrived {
            break;
        }
    }
    send(&mut conn, exit(0)).await;
    frames.extend(drain(&mut conn).await);
    child.wait()?;

    let stdin_frames: Vec<Message> = frames
        .into_iter()
        .filter(|m| matches!(m, Message::Stdin(_)))
        .collect();

    assert_eq!(
        stdin_frames,
        vec![
            Message::Stdin(b"before".to_vec()),
            Message::Stdin(b"after".to_vec()),
        ]
    );
    Ok(())
}

#[tokio::test]
async fn signal_number_reaches_the_child_verbatim() -> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let sock = dir.path().join("ncap.sock");
    let log_dir = dir.path().join("logs");
    let (mut server, mut conn) = boot_server(&sock, &log_dir, "error").await;

    send(
        &mut conn,
        Message::Request(request("sleep", &["10"], dir.path())),
    )
    .await;
    send(&mut conn, Message::Signal(SignalMsg { signal: 2 })).await;
    let terminal = recv(&mut conn).await;

    assert_eq!(
        terminal,
        Message::Exit(Exit {
            code: None,
            signal: Some(2)
        })
    );
    stop_server(&mut server);
    Ok(())
}

#[tokio::test]
async fn signal_reaches_grandchildren_in_the_process_group()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let sock = dir.path().join("ncap.sock");
    let log_dir = dir.path().join("logs");
    let marker = dir.path().join("marker");
    let grandchild = dir.path().join("grandchild.sh");
    let child_script = dir.path().join("child.sh");
    std::fs::write(
        &grandchild,
        format!(
            "trap 'printf x > \"{}\"' TERM\nprintf 'ready\\n'\nsleep 30\n",
            marker.display()
        ),
    )?;
    std::fs::write(
        &child_script,
        format!("sh \"{}\" &\nwait\n", grandchild.display()),
    )?;
    let (mut server, mut conn) = boot_server(&sock, &log_dir, "error").await;

    send(
        &mut conn,
        Message::Request(request(
            "sh",
            &[child_script.to_str().expect("tempdir paths are UTF-8")],
            dir.path(),
        )),
    )
    .await;
    // The grandchild prints `ready` only after installing its TERM trap —
    // signaling earlier would pin the trap-install race, not group targeting.
    let mut out = Vec::new();
    while !out.windows(5).any(|w| w == b"ready") {
        if let Message::Stdout(bytes) = recv(&mut conn).await {
            out.extend_from_slice(&bytes);
        }
    }
    send(&mut conn, Message::Signal(SignalMsg { signal: 15 })).await;
    let _terminal = recv(&mut conn).await;

    let deadline = tokio::time::Instant::now() + MARKER_WAIT;
    while !marker.exists() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    assert!(
        marker.exists(),
        "expected the grandchild's TERM trap to write the marker file"
    );
    stop_server(&mut server);
    Ok(())
}

#[tokio::test]
async fn kill_failure_logs_one_warning_and_the_connection_continues()
-> Result<(), Box<dyn std::error::Error>> {
    let dir = tempdir()?;
    let sock = dir.path().join("ncap.sock");
    let log_dir = dir.path().join("logs");
    let (mut server, mut conn) = boot_server(&sock, &log_dir, "warning").await;

    send(
        &mut conn,
        Message::Request(request("sh", &["-c", "read line"], dir.path())),
    )
    .await;
    send(&mut conn, Message::Signal(SignalMsg { signal: 255 })).await;
    send(&mut conn, Message::Stdin(b"go\n".to_vec())).await;
    let terminal = recv(&mut conn).await;

    assert_eq!(
        terminal,
        Message::Exit(Exit {
            code: Some(0),
            signal: None
        })
    );
    stop_server(&mut server);

    let lines = server_log_lines(&log_dir);

    assert_eq!(
        lines.len(),
        1,
        "expected exactly one warning line, got {lines:#?}"
    );
    Ok(())
}

#[tokio::test]
async fn signal_after_stdin_eof_still_reaches_the_child() -> Result<(), Box<dyn std::error::Error>>
{
    let dir = tempdir()?;
    let sock = dir.path().join("ncap.sock");
    let log_dir = dir.path().join("logs");
    let (mut server, mut conn) = boot_server(&sock, &log_dir, "error").await;

    send(
        &mut conn,
        Message::Request(request("sleep", &["10"], dir.path())),
    )
    .await;
    send(&mut conn, Message::Stdin(vec![])).await;
    send(&mut conn, Message::Signal(SignalMsg { signal: 15 })).await;
    let terminal = recv(&mut conn).await;

    assert_eq!(
        terminal,
        Message::Exit(Exit {
            code: None,
            signal: Some(15)
        })
    );
    stop_server(&mut server);
    Ok(())
}

type Conn = Framed<UnixStream, FrameCodec>;

const FRAME_WAIT: Duration = Duration::from_secs(10);
const MARKER_WAIT: Duration = Duration::from_secs(5);

/// Spawn the Client against `sock` and hand back its Connection after the
/// handshake. The pump's immediate EOF marker is the readiness sync: the
/// frame loop — and with it the SIGINT/SIGTERM handlers — is installed
/// before that frame can be emitted, so a signal raised after it cannot
/// race the handler install.
async fn open_session(listener: &UnixListener, sock: &Path, cwd: &Path) -> (Child, Conn) {
    let child = spawn_ncap(sock, cwd, Stdio::null());
    let mut conn = accept_conn(listener).await;
    let _request = recv(&mut conn).await;
    let _eof_marker = recv(&mut conn).await;
    (child, conn)
}

fn spawn_ncap(sock: &Path, cwd: &Path, stdin: Stdio) -> Child {
    // `COMMAND` is required by the CLI and never spawned: the scripted
    // Server answers the Connection itself.
    Command::new(env!("CARGO_BIN_EXE_ncap"))
        .arg("--socket")
        .arg(sock)
        .current_dir(cwd)
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .args(["--", "true"])
        .env_remove("NCAP_SOCKET")
        .env_remove("NCAP_ENV_FORWARD")
        .spawn()
        .expect("the Client spawns")
}

async fn accept_conn(listener: &UnixListener) -> Conn {
    let stream = tokio::time::timeout(FRAME_WAIT, listener.accept())
        .await
        .expect("timed out waiting for the Client to connect")
        .expect("the Client connects")
        .0;
    Framed::new(stream, FrameCodec)
}

async fn recv(conn: &mut Conn) -> Message {
    let frame = tokio::time::timeout(FRAME_WAIT, conn.next())
        .await
        .expect("timed out waiting for a frame")
        .expect("the connection ended while a frame was expected")
        .expect("the frame decodes: the peer sends well-formed frames");
    Message::from_frame(frame).expect("the frame carries a message")
}

async fn send(conn: &mut Conn, msg: Message) {
    let frame = msg.into_frame().expect("the message encodes");
    conn.send(frame)
        .await
        .expect("the frame reaches the socket")
}

async fn drain(conn: &mut Conn) -> Vec<Message> {
    let mut messages = Vec::new();
    while let Some(item) = tokio::time::timeout(FRAME_WAIT, conn.next())
        .await
        .expect("timed out draining the connection")
    {
        let frame = item.expect("the frame decodes: the peer sends well-formed frames");
        messages.push(Message::from_frame(frame).expect("the frame carries a message"));
    }
    messages
}

async fn boot_server(sock: &Path, log_dir: &Path, log_level: &str) -> (Child, Conn) {
    let server = Command::new(env!("CARGO_BIN_EXE_ncap-server"))
        .arg("--socket")
        .arg(sock)
        .arg("--log-dir")
        .arg(log_dir)
        .args(["--timeout", "0"])
        .arg("--log-level")
        .arg(log_level)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("the Server spawns");
    let deadline = tokio::time::Instant::now() + FRAME_WAIT;
    let stream = loop {
        match UnixStream::connect(sock).await {
            Ok(stream) => break stream,
            Err(_) if tokio::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            Err(err) => panic!("timed out waiting for the Server socket: {err}"),
        }
    };
    (server, Framed::new(stream, FrameCodec))
}

fn kill(pid: u32, sig: i32) {
    unsafe {
        libc::kill(pid as i32, sig);
    }
}

fn stop_server(server: &mut Child) {
    kill(server.id(), libc::SIGTERM);
    server.wait().expect("the Server reaps");
}

fn exit(code: u8) -> Message {
    Message::Exit(Exit {
        code: Some(code),
        signal: None,
    })
}

fn request(command: &str, args: &[&str], cwd: &Path) -> Request {
    Request {
        command: command.to_string(),
        args: args.iter().map(|arg| (*arg).to_string()).collect(),
        cwd: cwd.to_str().expect("tempdir paths are UTF-8").to_string(),
        env: Vec::new(),
    }
}

/// WUNTRACED reports a stopped Child without consuming its exit status, so
/// `Child::wait` can still reap it after the SIGCONT.
async fn wait_until_stopped(child: &mut Child) {
    let pid = child.id() as i32;
    let deadline = tokio::time::Instant::now() + FRAME_WAIT;
    loop {
        let mut status: libc::c_int = 0;
        let reaped = unsafe { libc::waitpid(pid, &mut status, libc::WUNTRACED | libc::WNOHANG) };
        if reaped == pid {
            assert!(
                libc::WIFSTOPPED(status),
                "the Client exited instead of stopping: status {status}"
            );
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("timed out waiting for the Client to stop");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn server_log_lines(log_dir: &Path) -> Vec<String> {
    let logs: Vec<PathBuf> = std::fs::read_dir(log_dir)
        .expect("the log dir is readable")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let epoch = name
                .strip_prefix("ncap-server-")
                .and_then(|s| s.strip_suffix(".log"))
                .unwrap_or("");
            !epoch.is_empty() && epoch.bytes().all(|b| b.is_ascii_digit())
        })
        .collect();
    assert_eq!(logs.len(), 1, "expected one Server log file, got {logs:?}");
    let text = std::fs::read_to_string(&logs[0]).expect("the log file is readable");
    text.lines().map(str::to_string).collect()
}
