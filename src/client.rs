//! Client (`ncap`): connect to a running `ncap-server` over a Unix socket,
//! send one `Request`, stream the child's stdio back to the terminal, and exit
//! with the child's status.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use futures_util::{SinkExt, StreamExt};
use tokio::net::UnixStream;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;
use tokio_util::codec::Framed;

use crate::protocol::{CURRENT_VERSION, Exit, FrameCodec, Message, Request, SignalMsg};

/// Exit code for an orderly server shutdown: 128 + SIGTERM. `ServerStopping`
/// and a clean close without a terminal frame both carry it.
const SHUTDOWN_EXIT: i32 = 128 + libc::SIGTERM;

/// Run `command` against the server listening on `socket`.
///
/// `cwd` overrides the working directory the server uses for the child; when
/// `None` it defaults to the client's own current directory. `env` carries the
/// `--env` flags, each a `KEY=VALUE` override or a bare `KEY` to copy from
/// this process. Returns the exit code the client process should report.
pub async fn run(
    socket: &Path,
    cwd: Option<PathBuf>,
    env: Vec<String>,
    command: Vec<String>,
) -> i32 {
    match session(socket, cwd, env, command).await {
        Ok(code) => code,
        Err(ClientError::Connect { socket, source }) => {
            eprintln!("ncap: cannot connect to socket `{socket}`: {source}");
            eprintln!("  run `ncap-ctl init` to start this project's container");
            1
        }
        Err(err) => {
            eprintln!("ncap: {err}");
            1
        }
    }
}

/// Failure modes on the client's side of the connection.
#[derive(Debug, thiserror::Error)]
enum ClientError {
    #[error("cannot connect to socket `{socket}`: {source}")]
    Connect {
        socket: String,
        #[source]
        source: io::Error,
    },
    #[error("{0}")]
    Io(#[from] io::Error),
    #[error("cannot install the `{signal}` handler: {source}")]
    SignalHandler {
        signal: &'static str,
        #[source]
        source: io::Error,
    },
    #[error("`NCAP_ENV_FORWARD` is not a JSON array of variable names: {source}")]
    ForwardEnv {
        #[source]
        source: serde_json::Error,
    },
    #[error("{0}")]
    Transport(String),
}

async fn session(
    socket: &Path,
    cwd: Option<PathBuf>,
    env: Vec<String>,
    command: Vec<String>,
) -> Result<i32, ClientError> {
    let (name, args) = match command.split_first() {
        Some(split) => split,
        None => return Err(ClientError::Transport("no command given".into())),
    };

    let stream = UnixStream::connect(socket)
        .await
        .map_err(|source| ClientError::Connect {
            socket: socket.display().to_string(),
            source,
        })?;
    let mut framed = Framed::new(stream, FrameCodec);
    send(
        &mut framed,
        Message::Request(build_request(cwd, env, name, args)?),
    )
    .await?;

    // Stdin travels on a blocking thread so a silent pipe never stalls the
    // frame loop; chunks reach the loop through the channel instead.
    let (stdin_tx, mut stdin_rx) = mpsc::channel(8);
    tokio::spawn(pump_stdin(stdin_tx));

    // Relay SIGINT and SIGTERM only, one verbatim `Signal` frame per event:
    // no raw mode (the ISIG line discipline delivers Ctrl-C here as a signal,
    // never as stdin bytes), no interpretation. SIGQUIT, SIGHUP, SIGTSTP, and
    // SIGCONT keep their default dispositions — killing the client drops the
    // connection and the server TERMs the child's process group.
    let mut sigint =
        signal(SignalKind::interrupt()).map_err(|source| ClientError::SignalHandler {
            signal: "SIGINT",
            source,
        })?;
    let mut sigterm =
        signal(SignalKind::terminate()).map_err(|source| ClientError::SignalHandler {
            signal: "SIGTERM",
            source,
        })?;
    let mut sigint_open = true;
    let mut sigterm_open = true;

    let mut version_seen = false;
    let mut stdin_open = true;
    loop {
        tokio::select! {
            frame = framed.next() => match frame {
                Some(Ok(frame)) => match Message::from_frame(frame)
                    .map_err(|err| ClientError::Transport(err.to_string()))?
                {
                    Message::Version(version) => {
                        version_seen = true;
                        if version.version != CURRENT_VERSION {
                            eprintln!(
                                "ncap: version mismatch: client `{CURRENT_VERSION}`, server `{}`",
                                version.version
                            );
                        }
                    }
                    Message::Stdout(bytes) => write_stream(&mut io::stdout().lock(), &bytes)?,
                    Message::Stderr(bytes) => write_stream(&mut io::stderr().lock(), &bytes)?,
                    Message::Exit(exit) => {
                        warn_absent_version(version_seen);
                        return Ok(exit_code(&exit, name));
                    }
                    Message::Error(message) => {
                        warn_absent_version(version_seen);
                        eprintln!("ncap: {}", message.message);
                        return Ok(1);
                    }
                    Message::ServerStopping => {
                        return Ok(SHUTDOWN_EXIT);
                    }
                    // Server misuse of client-only frames carries nothing
                    // actionable.
                    Message::Request(_) | Message::Stdin(_) | Message::Signal(_) => {}
                },
                Some(Err(err)) => return Err(ClientError::Transport(err.to_string())),
                None => {
                    // A clean close without a terminal frame is the server's
                    // orderly-shutdown signature: bail with 128 + SIGTERM,
                    // never a transport failure.
                    return Ok(SHUTDOWN_EXIT);
                }
            },
            chunk = stdin_rx.recv(), if stdin_open => match chunk {
                Some(Some(bytes)) => send(&mut framed, Message::Stdin(bytes)).await?,
                Some(None) | None => {
                    // EOF on host stdin: one empty `Stdin` frame marks it,
                    // keeping the write half open for a later signal frame.
                    // A failed send means the child finished first and the
                    // terminal frame is already in flight — not fatal; the
                    // loop keeps streaming either way.
                    let _ = send(&mut framed, Message::Stdin(Vec::new())).await;
                    stdin_open = false;
                }
            },
            sig = sigint.recv(), if sigint_open => match sig {
                Some(()) => {
                    send(&mut framed, Message::Signal(SignalMsg { signal: libc::SIGINT as u8 }))
                        .await?
                }
                None => sigint_open = false,
            },
            sig = sigterm.recv(), if sigterm_open => match sig {
                Some(()) => {
                    send(&mut framed, Message::Signal(SignalMsg { signal: libc::SIGTERM as u8 }))
                        .await?
                }
                None => sigterm_open = false,
            },
        }
    }
}

fn build_request(
    cwd: Option<PathBuf>,
    env: Vec<String>,
    name: &str,
    args: &[String],
) -> Result<Request, ClientError> {
    let cwd = match cwd {
        Some(cwd) => cwd,
        None => std::env::current_dir()?,
    };
    let forward = std::env::var("NCAP_ENV_FORWARD").ok();
    let env = build_env(&env, forward.as_deref(), |name| std::env::var(name).ok())?;
    Ok(Request {
        command: name.to_string(),
        args: args.to_vec(),
        cwd: cwd.to_string_lossy().into_owned(),
        env,
        version: Some(CURRENT_VERSION.into()),
    })
}

/// Merge the request env: every name in `NCAP_ENV_FORWARD` (a JSON array of
/// variable names) resolved from this process first, then the `--env` flags —
/// later-wins by key, deduplicated, unset entries silently omitted. A forward
/// list that is not a JSON array of names is an error.
fn build_env(
    cli: &[String],
    forward: Option<&str>,
    lookup: impl Fn(&str) -> Option<String>,
) -> Result<Vec<String>, ClientError> {
    let names: Vec<String> = match forward {
        Some(raw) => {
            serde_json::from_str(raw).map_err(|source| ClientError::ForwardEnv { source })?
        }
        None => Vec::new(),
    };
    let mut entries: Vec<(String, String)> = Vec::new();
    for name in &names {
        if let Some(value) = lookup(name) {
            apply_entry(&mut entries, name, value);
        }
    }
    for flag in cli {
        if let Some((key, value)) = resolve_flag(flag, &lookup) {
            apply_entry(&mut entries, &key, value);
        }
    }
    Ok(entries
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect())
}

/// One `--env` flag: `KEY=VALUE` carries an explicit value, bare `KEY` copies
/// from this process when set. An empty key is silently omitted.
fn resolve_flag(flag: &str, lookup: impl Fn(&str) -> Option<String>) -> Option<(String, String)> {
    match flag.split_once('=') {
        Some(("", _)) => None,
        Some((key, value)) => Some((key.to_string(), value.to_string())),
        None => lookup(flag).map(|value| (flag.to_string(), value)),
    }
}

/// Set `key` to `value`, replacing in place when the key already arrived —
/// later-wins with the first occurrence's position kept.
fn apply_entry(entries: &mut Vec<(String, String)>, key: &str, value: String) {
    match entries.iter_mut().find(|(existing, _)| existing == key) {
        Some((_, slot)) => *slot = value,
        None => entries.push((key.to_string(), value)),
    }
}

fn pump_stdin(tx: mpsc::Sender<Option<Vec<u8>>>) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        let mut stdin = io::stdin().lock();
        let mut buf = vec![0u8; 8 * 1024];
        loop {
            match stdin.read(&mut buf) {
                // A relayed signal interrupts the blocking read (EINTR);
                // retry — the stream keeps going, the frame loop relays.
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.blocking_send(Some(buf[..n].to_vec())).is_err() {
                        return; // session over; nothing left to feed
                    }
                }
            }
        }
        let _ = tx.blocking_send(None);
    })
}

fn exit_code(exit: &Exit, command: &str) -> i32 {
    match (exit.code, exit.signal) {
        (Some(127), _) => {
            eprintln!("ncap: {command}: command not found");
            127
        }
        (Some(126), _) => {
            eprintln!("ncap: {command}: permission denied");
            126
        }
        (Some(code), _) => i32::from(code),
        (None, Some(signal)) => i32::from(signal) + 128,
        (None, None) => {
            eprintln!("ncap: child status unknowable");
            1
        }
    }
}

fn warn_absent_version(seen: bool) {
    if !seen {
        eprintln!("ncap: server did not send a version");
    }
}

fn write_stream(stream: &mut impl Write, bytes: &[u8]) -> Result<(), ClientError> {
    stream.write_all(bytes)?;
    stream.flush()?;
    Ok(())
}

async fn send(
    framed: &mut Framed<UnixStream, FrameCodec>,
    message: Message,
) -> Result<(), ClientError> {
    let frame = message
        .into_frame()
        .map_err(|err| ClientError::Transport(err.to_string()))?;
    framed
        .send(frame)
        .await
        .map_err(|err| ClientError::Transport(err.to_string()))
}

#[cfg(test)]
mod tests {
    use super::build_env;
    use test_case::test_case;

    /// A lookup over literal pairs, standing in for the process environment.
    fn lookup_of<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.to_string())
        }
    }

    fn owned(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    #[test_case(&["K=V"], None, &[], &["K=V"] ; "explicit_key_value_passes_through")]
    #[test_case(&["K"], None, &[("K", "host")], &["K=host"] ; "bare_key_copies_when_set")]
    #[test_case(&["K"], None, &[], &[] ; "bare_key_omitted_when_unset")]
    #[test_case(&["K="], None, &[], &["K="] ; "empty_value_is_explicit")]
    #[test_case(&["K=a=b"], None, &[], &["K=a=b"] ; "value_may_carry_equals")]
    #[test_case(&["=V"], None, &[], &[] ; "empty_key_is_omitted")]
    #[test_case(&[], Some(r#"["K"]"#), &[("K", "host")], &["K=host"] ; "forwarded_name_resolves")]
    #[test_case(&[], Some(r#"["K"]"#), &[], &[] ; "forwarded_unset_name_is_omitted")]
    #[test_case(&[], Some("[]"), &[("K", "host")], &[] ; "empty_forward_list_yields_nothing")]
    #[test_case(&[], None, &[("K", "host")], &[] ; "absent_forward_yields_nothing")]
    #[test_case(&["K=cli"], Some(r#"["K"]"#), &[("K", "host")], &["K=cli"] ; "cli_flag_wins_over_forwarded")]
    #[test_case(&["K=first", "K=second"], None, &[], &["K=second"] ; "later_flag_wins")]
    #[test_case(
        &["A=cli", "B=flag"],
        Some(r#"["A", "B", "C"]"#),
        &[("A", "host"), ("B", "host"), ("C", "host")],
        &["A=cli", "B=flag", "C=host"] ; "merged_list_dedups_forwarded_first_cli_wins"
    )]
    fn build_env_resolves_omits_and_dedups(
        cli: &[&str],
        forward: Option<&str>,
        host: &[(&str, &str)],
        expected: &[&str],
    ) {
        let merged = build_env(&owned(cli), forward, lookup_of(host)).expect("merge succeeds");
        assert_eq!(merged, owned(expected));
    }

    #[test_case("not json" ; "malformed_json")]
    #[test_case(r#"{"a": 1}"# ; "object_is_not_an_array")]
    #[test_case(r#"["K", 1]"# ; "non_string_entry")]
    fn malformed_forward_is_an_error(forward: &str) {
        let err =
            build_env(&[], Some(forward), lookup_of(&[])).expect_err("malformed forward errors");
        assert!(err.to_string().contains("NCAP_ENV_FORWARD"), "error={err}");
    }
}
