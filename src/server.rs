//! Server (`ncap-server`): the per-project server inside the container. It
//! serves one child per connection, streaming the child's stdio back to the
//! client over the project socket. Startup probes the socket (refusing a
//! live server, replacing a stale file) and logs per-run; SIGTERM/SIGINT
//! stop every connection orderly within the drain grace.

use std::fs::OpenOptions;
use std::io::{ErrorKind, Write};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio_util::codec::Framed;

use crate::ctl::fs_error::FsError;
use crate::ctl::paths::server_log_path;
use crate::protocol::{
    CURRENT_VERSION, DecodeError, ErrorMsg, Exit, FrameCodec, FrameType, Message, VersionMsg,
};

/// Bind `socket` and serve connections until the process is stopped. A
/// SIGTERM or SIGINT starts the orderly shutdown: `ServerStopping` to every
/// live connection, a group TERM for every child, a drain bounded by
/// `drain`, then the socket file's removal.
pub async fn run(
    socket: PathBuf,
    log_dir: PathBuf,
    drain: Duration,
    log_level: LogLevel,
) -> Result<(), ServerError> {
    let log = Arc::new(Log::start(&log_dir, log_level)?);
    log.info(&format!("server started (pid {})", std::process::id()));
    probe_socket(&socket).await?;
    let listener = UnixListener::bind(&socket).map_err(|source| ServerError::Bind {
        socket: socket.display().to_string(),
        source,
    })?;
    log.info(&format!("socket bound at `{}`", socket.display()));

    let (stop_tx, stop_rx) = watch::channel(false);
    let connections: Arc<Mutex<Vec<JoinHandle<()>>>> = Arc::new(Mutex::new(Vec::new()));
    let acceptor = tokio::spawn(accept_loop(
        listener,
        stop_rx,
        connections.clone(),
        log.clone(),
    ));

    let mut sigterm =
        signal(SignalKind::terminate()).map_err(|source| ServerError::SignalHandler {
            signal: "SIGTERM",
            source,
        })?;
    let mut sigint =
        signal(SignalKind::interrupt()).map_err(|source| ServerError::SignalHandler {
            signal: "SIGINT",
            source,
        })?;
    let received = tokio::select! {
        _ = sigterm.recv() => "SIGTERM",
        _ = sigint.recv() => "SIGINT",
    };
    log.debug(&format!("{received} received notifying live connections"));
    let _ = stop_tx.send(true);
    acceptor.abort();
    // Await the cancelled acceptor so every handle it pushed is visible
    // before the drain snapshots them — a connection accepted on the way
    // out still gets its ServerStopping and group TERM.
    let _ = acceptor.await;

    let handles: Vec<JoinHandle<()>> = connections
        .lock()
        .expect("connection lock")
        .drain(..)
        .collect();
    log.info(&format!("drain begun within {}s", drain.as_secs()));
    if drain > Duration::from_secs(0) {
        let _ = tokio::time::timeout(drain, async {
            for handle in handles {
                let _ = handle.await;
            }
        })
        .await;
    }
    let _ = std::fs::remove_file(&socket);
    log.info("socket removed and server exited");
    Ok(())
}

/// Failures of the Server's startup path: context variants name the failed
/// operation and its object with the raw cause riding as `#[source]`; the
/// shared filesystem variants pass through transparent. Socket and
/// signal-handler io errors are not filesystem operations and carry their
/// context here.
#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    /// Filesystem failures shared with the ctl flows.
    #[error(transparent)]
    Fs(#[from] FsError),
    /// The socket file is owned by a live server; refusing to disturb it.
    #[error("socket `{socket}` is owned by a live server")]
    SocketInUse {
        /// The socket path owned by the live server.
        socket: String,
    },
    /// Binding the socket file failed.
    #[error("cannot bind socket `{socket}`: {source}")]
    Bind {
        /// The socket path that failed to bind.
        socket: String,
        /// The underlying bind failure.
        #[source]
        source: std::io::Error,
    },
    /// Installing a shutdown signal handler failed.
    #[error("cannot install the `{signal}` handler: {source}")]
    SignalHandler {
        /// The signal whose handler failed to install.
        signal: &'static str,
        /// The underlying handler-installation failure.
        #[source]
        source: std::io::Error,
    },
}

/// Failure modes that end a Connection with a terminal `Error` frame; the
/// message rides to the Client as the frame's `message` payload. Separate
/// from the startup `ServerError`: this enum renders on the wire, not on
/// the Server's stderr.
#[derive(Debug, thiserror::Error)]
enum Rejection {
    #[error("expected a `Request` frame first, got `{got:?}`")]
    ExpectedRequest { got: FrameType },
    #[error(transparent)]
    Decode(#[from] DecodeError),
    #[error("`cwd` is not a directory: `{cwd}`")]
    NotADirectory { cwd: String },
    #[error("invalid env entry `{entry}`")]
    BadEnvEntry { entry: String },
    #[error("cannot spawn `{command}`: {source}")]
    Spawn {
        command: String,
        #[source]
        source: std::io::Error,
    },
    #[error("cannot wait for `{command}`: {source}")]
    Wait {
        command: String,
        #[source]
        source: std::io::Error,
    },
}

/// The minimum severity the Server logs at — exactly `debug`, `info`,
/// `warning`, `error` (spec/server.md § Logging). Ordering is
/// `debug` < `info` < `warning` < `error`; the single source the Ctl
/// contract validates against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogLevel {
    /// Finest-grained detail: connection opens, request lines, signals.
    Debug,
    /// Lifecycle milestones: start, bind, drain, exit.
    Info,
    /// Version skew and failed kills; the connection continues.
    Warning,
    /// Accept, decode, and spawn failures ending a connection.
    Error,
}

impl LogLevel {
    /// The tag string the log writer emits for this level.
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            LogLevel::Debug => "debug",
            LogLevel::Info => "info",
            LogLevel::Warning => "warning",
            LogLevel::Error => "error",
        }
    }

    /// Fixed rank for the minimum-severity comparison: `debug` 0 through
    /// `error` 3.
    pub(crate) fn rank(&self) -> u8 {
        match self {
            LogLevel::Debug => 0,
            LogLevel::Info => 1,
            LogLevel::Warning => 2,
            LogLevel::Error => 3,
        }
    }

    /// Exact match against the four tag strings; anything else is `None`.
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "debug" => Some(LogLevel::Debug),
            "info" => Some(LogLevel::Info),
            "warning" => Some(LogLevel::Warning),
            "error" => Some(LogLevel::Error),
            _ => None,
        }
    }
}

impl std::fmt::Display for LogLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Rejection of a `--log-level` value outside the four tags.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LogLevelParseError {
    /// The rejected `--log-level` value.
    #[error("must be `debug`, `info`, `warning`, or `error`, got `{value}`")]
    BadLevel {
        /// The off-vocabulary value that was rejected.
        value: String,
    },
}

impl std::str::FromStr for LogLevel {
    type Err = LogLevelParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        LogLevel::parse(value).ok_or_else(|| LogLevelParseError::BadLevel {
            value: value.to_owned(),
        })
    }
}

/// Probe an existing socket file before binding: a connectable socket is
/// owned by a live server — refuse rather than disturb it. A connect failure
/// means the file is stale (the previous server crashed) and is removed so
/// the bind can succeed.
async fn probe_socket(socket: &Path) -> Result<(), ServerError> {
    if !socket.exists() {
        return Ok(());
    }
    match UnixStream::connect(socket).await {
        Ok(_) => Err(ServerError::SocketInUse {
            socket: socket.display().to_string(),
        }),
        Err(_) => {
            std::fs::remove_file(socket).map_err(|source| FsError::Remove {
                path: socket.display().to_string(),
                source,
            })?;
            Ok(())
        }
    }
}

/// Accept connections until cancelled, registering each task so the
/// shutdown can drain them.
async fn accept_loop(
    listener: UnixListener,
    stopping: watch::Receiver<bool>,
    connections: Arc<Mutex<Vec<JoinHandle<()>>>>,
    log: Arc<Log>,
) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let handle = tokio::spawn(handle_conn(stream, stopping.clone(), log.clone()));
                connections.lock().expect("connection lock").push(handle);
            }
            Err(err) => {
                log.error(&format!("accept failed: {err}"));
                return;
            }
        }
    }
}

async fn handle_conn(stream: UnixStream, stopping: watch::Receiver<bool>, log: Arc<Log>) {
    log.info("connection opened");
    let mut framed = Framed::new(stream, FrameCodec);

    let request = tokio::select! {
        frame = framed.next() => match frame {
            Some(Ok(frame)) => match Message::from_frame(frame) {
                Ok(Message::Request(request)) => request,
                Ok(other) => {
                    send_error(
                        &mut framed,
                        &Rejection::ExpectedRequest {
                            got: other.frame_type(),
                        },
                    )
                    .await;
                    return;
                }
                Err(err) => {
                    log.error(&format!("first frame undecodable: {err}"));
                    send_error(&mut framed, &Rejection::from(err)).await;
                    return;
                }
            },
            Some(Err(err)) => {
                log.error(&format!("first frame undecodable: {err}"));
                send_error(&mut framed, &Rejection::from(err)).await;
                return;
            }
            None => return, // client left before its first frame
        },
        _ = stopping_signalled(stopping.clone()) => {
            // Every live connection learns of the shutdown, even one still
            // waiting for its first frame.
            send(&mut framed, Message::ServerStopping).await;
            return;
        }
    };

    let version = Message::Version(VersionMsg {
        version: CURRENT_VERSION.into(),
    });
    if !send(&mut framed, version).await {
        return;
    }
    log.debug(&format!(
        "exec request for `{}` in `{}`",
        request.command, request.cwd
    ));

    // Version is advisory, never a rejection: a differing (or missing)
    // request version is one warning line, then the connection continues to
    // cwd validation. Comparison is exact string equality.
    match &request.version {
        Some(peer) if peer == CURRENT_VERSION => {}
        Some(peer) => {
            log.warning(&format!(
                "connection declared version `{peer}` against `{CURRENT_VERSION}`"
            ));
        }
        None => {
            log.warning(&format!(
                "connection declared no version against `{CURRENT_VERSION}`"
            ));
        }
    }

    if !Path::new(&request.cwd).is_dir() {
        send_error(
            &mut framed,
            &Rejection::NotADirectory {
                cwd: request.cwd.clone(),
            },
        )
        .await;
        return;
    }
    let mut command = tokio::process::Command::new(&request.command);
    command.args(&request.args).current_dir(&request.cwd);
    // Request env layers over the environment the server inherited from the
    // sourced env dump; entries are additive, never clearing inherited keys.
    for entry in &request.env {
        match entry.split_once('=') {
            Some((key, value)) => {
                command.env(key, value);
            }
            None => {
                log.error(&format!("invalid env entry `{entry}`"));
                send_error(
                    &mut framed,
                    &Rejection::BadEnvEntry {
                        entry: entry.clone(),
                    },
                )
                .await;
                return;
            }
        }
    }
    // The child leads a fresh process group, so signal delivery below can
    // reach its whole group via `kill(-pgid, …)` and grandchildren die with
    // their progenitor.
    command
        .process_group(0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            match err.kind() {
                ErrorKind::NotFound => {
                    send_exit(
                        &mut framed,
                        Exit {
                            code: Some(127),
                            signal: None,
                        },
                    )
                    .await;
                }
                ErrorKind::PermissionDenied => {
                    send_exit(
                        &mut framed,
                        Exit {
                            code: Some(126),
                            signal: None,
                        },
                    )
                    .await;
                }
                _ => {
                    send_error(
                        &mut framed,
                        &Rejection::Spawn {
                            command: request.command.clone(),
                            source: err,
                        },
                    )
                    .await;
                }
            }
            return;
        }
    };
    let pgid = child.id().expect("freshly spawned child has a pid");

    if bridge(&mut framed, &mut child, pgid, stopping.clone(), &log).await {
        // Shutdown may land in the post-drain wait window (pipes drained,
        // child not yet reaped). Select on `stopping` so every live
        // Connection still gets `ServerStopping` before its terminal `Exit`
        // (spec/server.md § Shutdown).
        let status = tokio::select! {
            status = child.wait() => status,
            _ = stopping_signalled(stopping.clone()) => {
                // Best-effort: the client may already be gone.
                // TERM the group so a pipes-drained-but-running child
                // cannot hang the drain (spec/server.md § Shutdown).
                send(&mut framed, Message::ServerStopping).await;
                let _ = signal_group(pgid, libc::SIGTERM as u8);
                child.wait().await
            }
        };
        let status = match status {
            Ok(status) => status,
            Err(err) => {
                send_error(
                    &mut framed,
                    &Rejection::Wait {
                        command: request.command.clone(),
                        source: err,
                    },
                )
                .await;
                return;
            }
        };
        let exit = Exit {
            code: status.code().map(|code| code as u8),
            signal: status.signal().map(|signal| signal as u8),
        };
        match (exit.code, exit.signal) {
            (Some(code), _) => log.debug(&format!("child exited with code {code}")),
            (_, Some(signal)) => log.debug(&format!("child exited with signal {signal}")),
            (None, None) => log.debug("child exited with an unknowable status"),
        }
        send(&mut framed, Message::Exit(exit)).await;
    } else {
        // The connection ended before the terminal frame — the only notice
        // is the failed send that ended the bridge (an undecodable frame
        // lands here too). TERM the group and then await the child, however
        // long it takes: the grace after the TERM is the child's, not the
        // server's, so there is no KILL escalation.
        //
        // Accepted limitation: a silent child — no output, stdin already
        // EOF'd — of a vanished client runs to completion; nothing
        // observable triggers detection.
        log.warning("client vanished before the terminal frame: TERMed and awaited the child's process group");
        let _ = signal_group(pgid, libc::SIGTERM as u8);
        let _ = child.wait().await;
    }
}

/// Pump both directions until the child's pipes close (returns true) or the
/// connection dies (returns false, caller TERMs the group and reaps). The
/// first shutdown broadcast announces `ServerStopping` to the client, TERMs
/// the child's group, and leaves the bridge running so a child finishing
/// inside the drain grace still completes normally.
async fn bridge(
    framed: &mut Framed<UnixStream, FrameCodec>,
    child: &mut tokio::process::Child,
    pgid: u32,
    stopping: watch::Receiver<bool>,
    log: &Log,
) -> bool {
    let mut stdin = child.stdin.take();
    let stdout = child.stdout.take().expect("child stdout is piped");
    let stderr = child.stderr.take().expect("child stderr is piped");

    let (tx, mut rx) = mpsc::channel(64);
    tokio::spawn(pump_output(stdout, Message::Stdout, tx.clone()));
    tokio::spawn(pump_output(stderr, Message::Stderr, tx));

    let mut client_writes_open = true;
    let mut stopping_open = true;
    loop {
        tokio::select! {
            message = rx.recv() => match message {
                Some(message) => {
                    if !send(framed, message).await {
                        return false;
                    }
                }
                None => return true, // both pipes drained: child is done
            },
            frame = framed.next(), if client_writes_open => match frame {
                Some(Ok(frame)) => match Message::from_frame(frame) {
                    Ok(Message::Stdin(bytes)) => {
                        if bytes.is_empty() {
                            // Empty `Stdin` frame is stdin EOF: dropping the
                            // pipe gives the child EOF while the connection
                            // stays open for `Signal` frames.
                            stdin = None;
                            log.debug("stdin EOF: dropping the child's stdin pipe");
                        } else {
                            let pipe_failed = match stdin.as_mut() {
                                Some(pipe) => pipe.write_all(&bytes).await.is_err(),
                                None => false,
                            };
                            if pipe_failed {
                                stdin = None;
                                log.debug("write to the child's stdin failed: dropping the pipe");
                            }
                        }
                    }
                    Ok(Message::Signal(signal_msg)) => {
                        // A failed kill (already-exited group, out-of-range
                        // number) is one warning line, never an Error frame —
                        // the connection continues to its normal terminal.
                        if let Err(err) = signal_group(pgid, signal_msg.signal) {
                            log.warning(&format!("kill(-{pgid}, {}): {err}", signal_msg.signal));
                        }
                    }
                    Ok(_) => {}
                    Err(err) => {
                        send_error(framed, &Rejection::from(err)).await;
                        return false;
                    }
                },
                Some(Err(err)) => {
                    send_error(framed, &Rejection::from(err)).await;
                    return false;
                }
                None => {
                    // Write-half close is stdin EOF, not a disconnect — a real
                    // disconnect surfaces as a failed send on the rx branch.
                    stdin = None;
                    client_writes_open = false;
                    log.debug("client write half closed: dropping the child's stdin pipe");
                }
            },
            _ = stopping_signalled(stopping.clone()), if stopping_open => {
                stopping_open = false;
                // The client learns first, then the whole group gets the
                // TERM. The bridge keeps running so a child that dies
                // (or traps and exits) inside the drain grace still
                // delivers its terminal frame.
                send(framed, Message::ServerStopping).await;
                let _ = signal_group(pgid, libc::SIGTERM as u8);
                log.info("sent `ServerStopping` and TERMed the child's process group");
            }
        }
    }
}

/// Resolve once the server is shutting down; the `watch::Ref` never escapes
/// this future, keeping it `Send` for the select loops.
async fn stopping_signalled(mut rx: watch::Receiver<bool>) {
    let _ = rx.wait_for(|stopping| *stopping).await;
}

/// Forward one signal number to the child's process group, verbatim — the
/// server is a relay, not a policy: whatever number arrives goes out as it
/// is. `Err` carries the `kill` failure (ESRCH for an already-exited group,
/// EINVAL for an out-of-range number).
fn signal_group(pgid: u32, signal: u8) -> std::io::Result<()> {
    let failed = unsafe { libc::kill(-(pgid as libc::pid_t), signal as libc::c_int) } != 0;
    if failed {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

async fn pump_output(
    pipe: impl tokio::io::AsyncRead + Unpin,
    wrap: impl Fn(Vec<u8>) -> Message,
    tx: mpsc::Sender<Message>,
) {
    let mut pipe = pipe;
    let mut buf = vec![0u8; 8 * 1024];
    loop {
        match pipe.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                if tx.send(wrap(buf[..n].to_vec())).await.is_err() {
                    return; // connection gone; frames have nowhere to go
                }
            }
        }
    }
}

async fn send_error(framed: &mut Framed<UnixStream, FrameCodec>, rejection: &Rejection) {
    send(
        framed,
        Message::Error(ErrorMsg {
            message: rejection.to_string(),
        }),
    )
    .await;
}

async fn send_exit(framed: &mut Framed<UnixStream, FrameCodec>, exit: Exit) {
    send(framed, Message::Exit(exit)).await;
}

/// Send one frame; false means the client is unreachable and the connection
/// should be torn down.
async fn send(framed: &mut Framed<UnixStream, FrameCodec>, message: Message) -> bool {
    let frame = match message.into_frame() {
        Ok(frame) => frame,
        Err(_) => return false,
    };
    framed.send(frame).await.is_ok()
}

/// The per-run log file `<log-dir>/ncap-server-<epoch>.log`. Every line is
/// mirrored to stderr so the container runtime captures the same stream.
/// One emit-time severity gate sits before both sinks, so the file and the
/// mirror never disagree.
struct Log {
    file: Mutex<std::fs::File>,
    min_level: LogLevel,
}

impl Log {
    /// Create `dir` when missing and open this run's epoch-stamped log file.
    fn start(dir: &Path, min_level: LogLevel) -> Result<Self, FsError> {
        std::fs::create_dir_all(dir).map_err(|source| FsError::CreateDir {
            dir: dir.display().to_string(),
            source,
        })?;
        let path = server_log_path(dir, epoch_millis());
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|source| FsError::Open {
                path: path.display().to_string(),
                source,
            })?;
        Ok(Self {
            file: Mutex::new(file),
            min_level,
        })
    }

    /// Append one stamped, level-tagged line to the log file and stderr;
    /// logging is best-effort and never disturbs the connection it reports
    /// on. A line whose level ranks below the minimum is written to neither
    /// sink — filtering happens here at emit time, before stamping, against
    /// the single `LogLevel` ordering.
    fn emit_line(&self, level: LogLevel, message: &str) {
        if level.rank() < self.min_level.rank() {
            return;
        }
        let line = format!("[{}] {level}: {message}\n", rfc3339_utc());
        if let Ok(mut file) = self.file.lock() {
            let _ = file.write_all(line.as_bytes());
        }
        #[cfg(not(test))]
        let _ = std::io::stderr().write_all(line.as_bytes());
    }

    fn debug(&self, message: &str) {
        self.emit_line(LogLevel::Debug, message);
    }

    fn info(&self, message: &str) {
        self.emit_line(LogLevel::Info, message);
    }

    fn warning(&self, message: &str) {
        self.emit_line(LogLevel::Warning, message);
    }

    fn error(&self, message: &str) {
        self.emit_line(LogLevel::Error, message);
    }
}

/// The current UTC instant as `YYYY-MM-DDTHH:MM:SSZ` (compact RFC 3339,
/// second precision). Saturates at the epoch for a clock set before it.
fn rfc3339_utc() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default();
    format_utc(secs)
}

/// Format Unix seconds as `YYYY-MM-DDTHH:MM:SSZ`. Days-to-date is the
/// inverse of Howard Hinnant's `days_from_civil`, which stays correct
/// across leap years.
fn format_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let secs_of_day = secs % 86_400;

    // Shift the civil era so the era math sees only positive values.
    let days_since_civil = days + 719_468;
    let era_index = days_since_civil / 146_097;
    let day_of_era = days_since_civil % 146_097; // [0, 146096]
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365; // [0, 399]
    let year_index = year_of_era + era_index * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100); // [0, 365]
    let month_prime = (5 * day_of_year + 2) / 153; // [0, 11]
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1; // [1, 31]
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    }; // [1, 12]
    let year = if month <= 2 {
        year_index + 1
    } else {
        year_index
    };

    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        year,
        month,
        day,
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60,
    )
}

/// Milliseconds since the Unix epoch, saturating at 0 like [`epoch_secs`].
fn epoch_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;
    use std::os::unix::process::CommandExt;

    use super::*;

    #[test]
    fn signal_to_a_reaped_group_maps_esrch() {
        let mut child = std::process::Command::new("true")
            .process_group(0)
            .spawn()
            .expect("spawn");
        let pgid = child.id();
        assert!(child.wait().expect("wait").success());

        let err = signal_group(pgid, libc::SIGTERM as u8)
            .expect_err("a reaped group cannot be signalled");

        assert_eq!(err.raw_os_error(), Some(libc::ESRCH), "err={err}");
    }

    #[test_case::test_case(0, "1970-01-01T00:00:00Z"; "epoch")]
    #[test_case::test_case(86_399, "1970-01-01T23:59:59Z"; "end of day one")]
    #[test_case::test_case(951_782_400, "2000-02-29T00:00:00Z"; "leap day 2000")]
    fn rfc3339_utc_formats_known_instants(secs: u64, expected: &str) {
        assert_eq!(format_utc(secs), expected);
    }

    fn arb_log_level() -> impl Strategy<Value = LogLevel> {
        prop_oneof![
            Just(LogLevel::Debug),
            Just(LogLevel::Info),
            Just(LogLevel::Warning),
            Just(LogLevel::Error),
        ]
    }

    proptest! {
        #[test]
        fn log_emit_keeps_lines_at_or_above_the_minimum(min in arb_log_level()) {
            let dir = tempfile::tempdir().expect("tempdir");
            let log = Log::start(dir.path(), min).expect("log starts");
            log.debug("debug line");
            log.info("info line");
            log.warning("warning line");
            log.error("error line");
            let entries: Vec<PathBuf> = std::fs::read_dir(dir.path())
                .expect("log dir")
                .map(|entry| entry.expect("log dir entry").path())
                .collect();
            let logged = std::fs::read_to_string(&entries[0]).expect("read log file");
            for (level, marker) in [
                (LogLevel::Debug, "debug line"),
                (LogLevel::Info, "info line"),
                (LogLevel::Warning, "warning line"),
                (LogLevel::Error, "error line"),
            ] {
                if level.rank() >= min.rank() {
                    prop_assert!(logged.contains(&format!("{level}: {marker}")),
                        "must keep {marker} at min {min}: {logged:?}");
                } else {
                    prop_assert!(!logged.contains(marker),
                        "must drop {marker} below min {min}: {logged:?}");
                }
            }
        }
    }
}
