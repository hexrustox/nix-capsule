//! Shared helpers for the exec integration tests: one [`Server`] and one
//! [`Client`], assembled fluently — the Server spawns the real binaries against
//! a tempdir socket (or stands in with scripted frames), and the Client drives
//! the real `ncap` binary or speaks the raw wire protocol.

#![allow(dead_code)]

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Instant;

use futures_util::{SinkExt, StreamExt};
use nix_capsule::protocol::{FrameCodec, Message, Request};
use tempfile::TempDir;
use tokio::net::{UnixListener, UnixStream};
use tokio::time::{Duration, sleep};
use tokio_util::codec::Framed;

/// Upper bound on one client-wait phase; a red run fails on the assertion,
/// never on the harness itself.
const WAIT_LIMIT: Duration = Duration::from_secs(30);

/// What a running [`Server`] holds: a real binary child or a scripted task.
enum ServerProc {
    Real(Child),
    Fake(tokio::task::JoinHandle<()>),
}

/// One Server per test: the real `ncap-server` binary by default, or a
/// scripted stand-in once the builder sets [`respond`](ServerBuilder::respond).
pub struct Server {
    path: PathBuf,
    socket: PathBuf,
    _dir: TempDir,
    handle: ServerProc,
    captured: Arc<Mutex<Option<Request>>>,
}

impl Server {
    /// A builder for a server with a fresh tempdir and socket.
    pub fn builder() -> ServerBuilder {
        ServerBuilder {
            log_dir: None,
            respond: None,
        }
    }

    /// The tempdir root backing this server; create extra test files here.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The socket this server is reachable on.
    pub fn socket(&self) -> &Path {
        &self.socket
    }

    /// A Client pre-bound to this server's socket.
    pub fn client(&self) -> Client<'_> {
        Client::at(&self.socket)
    }

    /// The `Request` the scripted stand-in received; `None` for a real server.
    pub fn captured_request(&self) -> Option<Request> {
        self.captured.lock().expect("capture lock").clone()
    }

    /// Everything the real server has written to stderr so far; empty for a
    /// scripted stand-in.
    pub fn stderr(&self) -> String {
        fs::read_to_string(self.path.join("server-stderr.log")).unwrap_or_default()
    }

    /// The real server's process id, for `/proc` inspection; `None` for a
    /// scripted stand-in.
    pub fn pid(&self) -> Option<u32> {
        match &self.handle {
            ServerProc::Real(child) => Some(child.id()),
            ServerProc::Fake(_) => None,
        }
    }

    /// A raw wire-protocol connection to this server, for tests that must speak
    /// the frames themselves rather than via the client binary.
    pub async fn raw(&self) -> Framed<UnixStream, FrameCodec> {
        let stream = UnixStream::connect(&self.socket)
            .await
            .expect("connect to server");
        Framed::new(stream, FrameCodec)
    }

    /// Kill a real server or drop a scripted one; the tempdir goes with it.
    pub fn stop(self) {
        match self.handle {
            ServerProc::Real(mut child) => {
                let _ = child.kill();
                let _ = child.wait();
            }
            ServerProc::Fake(task) => task.abort(),
        }
    }
}

pub struct ServerBuilder {
    log_dir: Option<PathBuf>,
    respond: Option<Vec<Message>>,
}

impl ServerBuilder {
    /// Drain-grace seconds handed to a real server, matching what `ncap-ctl`
    /// emits.
    const TIMEOUT_SECS: &str = "10";

    /// Override where a real server writes its logs; defaults to `<path>/logs`.
    pub fn log_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.log_dir = Some(dir.into());
        self
    }

    /// Switch to the scripted stand-in: accept one connection, ignore the
    /// client's Request, send `respond`, and close.
    pub fn respond(mut self, respond: Vec<Message>) -> Self {
        self.respond = Some(respond);
        self
    }

    /// Bind the socket, spawn the chosen mode, and — for a real server — wait
    /// until the socket accepts connections.
    pub async fn start(self) -> Server {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().to_path_buf();
        let socket = path.join("ncap.sock");
        let captured = Arc::new(Mutex::new(None));
        let handle = match self.respond {
            None => {
                let log_dir = self.log_dir.unwrap_or_else(|| path.join("logs"));
                let stderr_log = fs::File::create(path.join("server-stderr.log"))
                    .expect("create server stderr capture");
                let child = Command::new(bin_path("ncap-server"))
                    .arg("--socket")
                    .arg(&socket)
                    .arg("--log-dir")
                    .arg(log_dir)
                    .arg("--timeout")
                    .arg(Self::TIMEOUT_SECS)
                    .stdout(Stdio::null())
                    .stderr(stderr_log)
                    .spawn()
                    .expect("spawn ncap-server");
                wait_for_socket(&socket).await;
                ServerProc::Real(child)
            }
            Some(respond) => {
                let listener = UnixListener::bind(&socket).expect("bind fake socket");
                let store = Arc::clone(&captured);
                let task = tokio::spawn(async move {
                    let (stream, _) = listener.accept().await.expect("accept");
                    let mut framed = Framed::new(stream, FrameCodec);
                    if let Some(Ok(frame)) = framed.next().await
                        && let Ok(Message::Request(request)) = Message::from_frame(frame)
                    {
                        *store.lock().expect("capture lock") = Some(request);
                    }
                    for m in respond {
                        framed
                            .send(m.into_frame().expect("frame"))
                            .await
                            .expect("send frame");
                    }
                });
                ServerProc::Fake(task)
            }
        };
        Server {
            path,
            socket,
            _dir: dir,
            handle,
            captured,
        }
    }
}

/// One Client per test: drives the real `ncap` binary against a Server socket,
/// one command per connection, streaming stdio back as a [`ClientOutput`].
pub struct Client<'a> {
    socket: &'a Path,
    cwd: Option<&'a Path>,
    stdin: Option<&'a [u8]>,
    env_flags: Vec<&'a str>,
    process_env: Vec<(&'a str, &'a str)>,
}

impl<'a> Client<'a> {
    /// A Client pointed at `socket`, with no cwd override and no stdin.
    pub fn at(socket: &'a Path) -> Self {
        Client {
            socket,
            cwd: None,
            stdin: None,
            env_flags: Vec::new(),
            process_env: Vec::new(),
        }
    }

    /// Override the working directory the client reports to the server.
    pub fn cwd(mut self, cwd: &'a Path) -> Self {
        self.cwd = Some(cwd);
        self
    }

    /// Feed `stdin` to the client; the write half closes right after, giving
    /// the child EOF.
    pub fn stdin(mut self, stdin: &'a [u8]) -> Self {
        self.stdin = Some(stdin);
        self
    }

    /// Set a variable in the client process's own environment — the view that
    /// bare `--env` flags and `NCAP_ENV_FORWARD` resolve against.
    pub fn env(mut self, name: &'a str, value: &'a str) -> Self {
        self.process_env.push((name, value));
        self
    }

    /// Pre-fill one `--env` flag, as a wrapper script would.
    pub fn env_flag(mut self, spec: &'a str) -> Self {
        self.env_flags.push(spec);
        self
    }

    /// Spawn the client binary end to end with `args` as the exec command
    /// without waiting, for tests that must deliver a signal mid-run; await
    /// the outcome with [`ClientProc::wait`].
    pub fn spawn(self, args: &[&str]) -> ClientProc {
        let mut cmd = Command::new(bin_path("ncap"));
        cmd.arg("--socket").arg(self.socket);
        if let Some(c) = self.cwd {
            cmd.arg("--cwd").arg(c);
        }
        for spec in &self.env_flags {
            cmd.arg("--env").arg(spec);
        }
        for (name, value) in &self.process_env {
            cmd.env(name, value);
        }
        cmd.args(args);
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
        if self.stdin.is_some() {
            cmd.stdin(Stdio::piped());
        } else {
            cmd.stdin(Stdio::null());
        }
        let mut child = cmd.spawn().expect("spawn ncap client");
        if let Some(input) = self.stdin {
            let mut handle = child.stdin.take().expect("client stdin");
            handle.write_all(input).expect("write stdin");
            drop(handle);
        }
        ClientProc { child }
    }

    /// Run the client binary end to end with `args` as the exec command.
    pub fn run(self, args: &[&str]) -> ClientOutput {
        self.spawn(args).wait()
    }
}

/// A running client process, spawned so a test can deliver a signal mid-run.
pub struct ClientProc {
    child: Child,
}

impl ClientProc {
    /// The client process's id, for `/proc` inspection or direct signaling.
    pub fn id(&self) -> u32 {
        self.child.id()
    }

    /// Deliver one signal to the client process itself — not its process
    /// group, so a container child never receives it directly.
    pub fn signal(&self, sig: i32) {
        let sent = unsafe { libc::kill(self.child.id() as libc::pid_t, sig) };
        assert_eq!(sent, 0, "kill({sig}) to client {}", self.child.id());
    }

    /// Wait for the client to exit and collect its output. Bounded: a client
    /// that never exits is killed and the test fails with a named panic.
    pub fn wait(mut self) -> ClientOutput {
        // Drain the pipes on a helper thread: a client blocked writing to a
        // full pipe could never exit for the bounded poll below.
        let stdout_pipe = self.child.stdout.take();
        let stderr_pipe = self.child.stderr.take();
        let drained = thread::spawn(move || {
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            if let Some(mut pipe) = stdout_pipe {
                let _ = pipe.read_to_end(&mut stdout);
            }
            if let Some(mut pipe) = stderr_pipe {
                let _ = pipe.read_to_end(&mut stderr);
            }
            (stdout, stderr)
        });

        let deadline = Instant::now() + WAIT_LIMIT;
        let status = loop {
            match self.child.try_wait().expect("poll client") {
                Some(status) => break status,
                None if Instant::now() >= deadline => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    panic!("client did not exit within {WAIT_LIMIT:?}");
                }
                None => thread::sleep(Duration::from_millis(20)),
            }
        };
        let (stdout, stderr) = drained.join().expect("drain client pipes");
        ClientOutput {
            status,
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        }
    }
}

/// What the client reported: exit status plus captured stdio.
pub struct ClientOutput {
    pub status: ExitStatus,
    pub stdout: String,
    pub stderr: String,
}

/// Absolute path to a compiled binary target, resolved at runtime from the
/// `CARGO_BIN_EXE_*` env var Cargo sets for integration tests.
fn bin_path(name: &str) -> PathBuf {
    let var = format!("CARGO_BIN_EXE_{name}");
    std::env::var(&var)
        .unwrap_or_else(|_| panic!("CARGO_BIN_EXE not set for binary {name}"))
        .into()
}

/// Poll the socket until the server accepts connections (or we give up).
async fn wait_for_socket(socket: &Path) {
    for _ in 0..200 {
        if UnixStream::connect(socket).await.is_ok() {
            return;
        }
        sleep(Duration::from_millis(20)).await;
    }
    panic!("server never bound {socket:?}");
}
