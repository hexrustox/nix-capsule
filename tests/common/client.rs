//! One [`Client`] per test: drives the real `ncap` binary against a Server
//! socket, one command per connection, streaming stdio back as a
//! [`ClientOutput`].

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

/// Upper bound on one client-wait phase; a red run fails on the assertion,
/// never on the harness itself.
pub const WAIT_LIMIT: Duration = Duration::from_secs(30);

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

    /// Non-blocking exit poll: `Some(status)` once the client has exited.
    pub fn try_wait(&mut self) -> Option<ExitStatus> {
        self.child.try_wait().expect("poll client")
    }

    /// Wait for the client to exit within `limit` and collect its output;
    /// the drain deadline must beat this bound. Bounded: a client that never
    /// exits is killed and the test fails with a named panic.
    pub fn wait_within(mut self, limit: Duration, what: &str) -> ClientOutput {
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

        let status = wait_bounded(&mut self.child, limit, what);
        let (stdout, stderr) = drained.join().expect("drain client pipes");
        ClientOutput {
            status,
            stdout: String::from_utf8_lossy(&stdout).into_owned(),
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        }
    }

    /// Wait for the client to exit and collect its output. Bounded: a client
    /// that never exits is killed and the test fails with a named panic.
    pub fn wait(self) -> ClientOutput {
        self.wait_within(WAIT_LIMIT, "client")
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
pub fn bin_path(name: &str) -> PathBuf {
    let var = format!("CARGO_BIN_EXE_{name}");
    std::env::var(&var)
        .unwrap_or_else(|_| panic!("CARGO_BIN_EXE not set for binary {name}"))
        .into()
}

/// Poll `child` until it exits or `limit` elapses, then return its status;
/// a child that outlives the limit is killed and the test fails with a named
/// panic. The shared shape behind [`super::server::Server::wait_for_exit`],
/// [`ClientProc::wait`], and tests that spawn an extra process by hand.
pub fn wait_bounded(child: &mut Child, limit: Duration, what: &str) -> ExitStatus {
    let deadline = Instant::now() + limit;
    loop {
        match child.try_wait().expect("poll child") {
            Some(status) => return status,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("{what} did not exit within {limit:?}");
            }
            None => thread::sleep(Duration::from_millis(20)),
        }
    }
}
