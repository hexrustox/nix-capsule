//! One [`Server`] per test: the real `ncap-server` binary by default, or a
//! scripted stand-in once the builder sets [`respond`](ServerBuilder::respond).

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use nix_capsule::protocol::{FrameCodec, Message, Request};
use tempfile::TempDir;
use tokio::net::{UnixListener, UnixStream};
use tokio::time::sleep;
use tokio_util::codec::Framed;

use super::client::{bin_path, wait_bounded};

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
            socket_path: None,
            timeout: None,
            log_level: None,
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
    pub fn client(&self) -> super::client::Client<'_> {
        super::client::Client::at(&self.socket)
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

    /// Everything the real server has written to stderr since `snapshot` —
    /// pass a string [`Self::stderr`] returned earlier in the test. The log
    /// file only grows by append, so slicing at the snapshot's byte length
    /// is exact. Empty for a scripted stand-in.
    pub fn stderr_since(&self, snapshot: &str) -> String {
        self.stderr()[snapshot.len()..].to_string()
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

    /// Deliver one signal to a real server process — not its group — without
    /// waiting; pair with [`Server::wait_for_exit`]. Panics for a scripted
    /// stand-in, which owns no signalable process.
    pub fn signal(&mut self, sig: i32) {
        let ServerProc::Real(child) = &mut self.handle else {
            panic!("signal to a scripted stand-in");
        };
        let sent = unsafe { libc::kill(child.id() as libc::pid_t, sig) };
        assert_eq!(sent, 0, "kill({sig}) to server {}", child.id());
    }

    /// Wait for a real server to exit and return its status. Bounded: a
    /// server that never exits is killed and the test fails with a named
    /// panic. `None` for a scripted stand-in.
    pub fn wait_for_exit(&mut self) -> Option<ExitStatus> {
        let ServerProc::Real(child) = &mut self.handle else {
            return None;
        };
        Some(wait_bounded(child, super::client::WAIT_LIMIT, "server"))
    }

    /// Deliver `sig` to a real server and await its exit; `None` for a
    /// scripted stand-in.
    pub fn terminate(&mut self, sig: i32) -> Option<ExitStatus> {
        self.signal(sig);
        self.wait_for_exit()
    }
}

pub struct ServerBuilder {
    log_dir: Option<PathBuf>,
    socket_path: Option<PathBuf>,
    timeout: Option<u64>,
    log_level: Option<String>,
    respond: Option<Vec<Message>>,
}

impl ServerBuilder {
    /// Drain-grace seconds handed to a real server, matching what `ncap-ctl`
    /// emits.
    const TIMEOUT_SECS: u64 = 10;

    /// Override where a real server writes its logs; defaults to `<path>/logs`.
    pub fn log_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.log_dir = Some(dir.into());
        self
    }

    /// Override the socket a real server binds; defaults to `<path>/ncap.sock`.
    pub fn socket_path(mut self, socket: impl Into<PathBuf>) -> Self {
        self.socket_path = Some(socket.into());
        self
    }

    /// Drain-grace seconds handed to a real server; defaults to
    /// [`Self::TIMEOUT_SECS`].
    pub fn timeout(mut self, seconds: u64) -> Self {
        self.timeout = Some(seconds);
        self
    }

    /// Minimum severity handed to a real server; defaults to `debug` so
    /// tests observe the full stream unless they narrow it.
    pub fn log_level(mut self, level: &str) -> Self {
        self.log_level = Some(level.into());
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
        let socket = self.socket_path.unwrap_or_else(|| path.join("ncap.sock"));
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
                    .arg(self.timeout.unwrap_or(Self::TIMEOUT_SECS).to_string())
                    .arg("--log-level")
                    .arg(self.log_level.unwrap_or_else(|| "debug".to_owned()))
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

/// A tempdir whose socket path nothing listens on, for tests of the client
/// reacting to an absent server. The tempdir must outlive the socket use.
pub fn missing_socket() -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("missing.sock");
    (dir, socket)
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
