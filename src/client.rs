//! Client (`ncap`): connect to a running `ncap-server` over a Unix socket,
//! send one `Request`, stream the child's stdio back to the terminal, and exit
//! with the child's status.

use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use futures_util::{SinkExt, StreamExt};
use tokio::net::UnixStream;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;
use tokio_util::codec::Framed;

use crate::protocol::{
    CURRENT_VERSION, DecodeError, EncodeError, Exit, FrameCodec, Message, Request, SignalMsg,
};

/// Exit code for an orderly server shutdown: 128 + SIGTERM. `ServerStopping`
/// and a clean close without a terminal frame both carry it.
const SHUTDOWN_EXIT: i32 = 128 + libc::SIGTERM;

/// Outcome of a session: the exit code the client process reports plus any
/// notice the entry point renders on stderr before exiting.
struct Outcome {
    code: i32,
    notice: Option<String>,
}

impl Outcome {
    fn just(code: i32) -> Self {
        Self { code, notice: None }
    }

    fn with_notice(code: i32, notice: String) -> Self {
        Self {
            code,
            notice: Some(notice),
        }
    }
}

/// Run `command` against the server listening on `socket`.
///
/// `cwd` overrides the working directory the server uses for the child; when
/// `None` it defaults to the client's own current directory. `env` carries the
/// `--env` flags, each a `KEY=VALUE` override or a bare `KEY` to copy from
/// this process. Non-UTF-8 bytes in `env`/`command` convert lossily
/// (`U+FFFD`) at this boundary — never rejected. Returns the exit code the
/// client process should report and the message to print on stderr, if any
/// (unprefixed, may be multi-line).
pub async fn run(
    socket: &Path,
    cwd: Option<PathBuf>,
    env: Vec<OsString>,
    command: Vec<OsString>,
) -> (i32, Option<String>) {
    match session(socket, cwd, env, command).await {
        Ok(outcome) => (outcome.code, outcome.notice),
        Err(err @ ClientError::Connect { .. }) => (
            1,
            Some(format!(
                "{err}\nrun `ncap-ctl init` to start this project's container"
            )),
        ),
        Err(err) => (1, Some(err.to_string())),
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
    #[error("cannot resolve the current directory: {source}")]
    CurrentDir {
        #[source]
        source: io::Error,
    },
    #[error("cannot write to stdout: {source}")]
    WriteStdout {
        #[source]
        source: io::Error,
    },
    #[error("cannot write to stderr: {source}")]
    WriteStderr {
        #[source]
        source: io::Error,
    },
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
    #[error("`--env` `{flag}` has an empty key")]
    EnvFlag { flag: String },
    #[error("`Exit` frame carries neither `code` nor `signal`")]
    MissingExitStatus,
    #[error("cannot read a frame from the socket: {source}")]
    Receive {
        #[source]
        source: DecodeError,
    },
    #[error("cannot send a frame to the socket: {source}")]
    Send {
        #[source]
        source: EncodeError,
    },
    #[error("server error: {message}")]
    ServerError { message: String },
}

async fn session(
    socket: &Path,
    cwd: Option<PathBuf>,
    env: Vec<OsString>,
    command: Vec<OsString>,
) -> Result<Outcome, ClientError> {
    // Unreachable: the only caller is the `ncap` binary, whose `command`
    // argument carries `required = true` (src/bin/ncap.rs:27), so clap
    // rejects an empty command before `run` is ever invoked — keeping
    // `build_request`'s `expect` below from ever firing.
    if command.is_empty() {
        unreachable!();
    }

    // Local errors — malformed `NCAP_ENV_FORWARD`, invalid `--env`, an
    // unresolvable cwd — fire before any connection attempt, so they fail
    // identically whether or not the Server is up.
    let request = build_request(cwd, &env, &command)?;
    let command_name = request.command.clone();

    let stream = UnixStream::connect(socket)
        .await
        .map_err(|source| ClientError::Connect {
            socket: socket.display().to_string(),
            source,
        })?;
    let mut framed = Framed::new(stream, FrameCodec);
    send(&mut framed, Message::Request(request)).await?;

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
                    .map_err(|source| ClientError::Receive { source })?
                {
                    Message::Version(version) => {
                        version_seen = true;
                        if version.version != CURRENT_VERSION {
                            // TODO
                        }
                    }
                    Message::Stdout(bytes) => {
                        write_stream(Stream::Stdout, &bytes)?;
                    }
                    Message::Stderr(bytes) => {
                        write_stream(Stream::Stderr, &bytes)?;
                    }
                    Message::Exit(exit) => {
                        warn_absent_version(version_seen);
                        return exit_outcome(&exit, &command_name);
                    }
                    Message::Error(err) => {
                        warn_absent_version(version_seen);
                        return Err(ClientError::ServerError { message: err.message });
                    }
                    Message::ServerStopping => {
                        return Ok(Outcome::just(SHUTDOWN_EXIT));
                    }
                    // Server misuse of client-only frames carries nothing
                    // actionable.
                    Message::Request(_) | Message::Stdin(_) | Message::Signal(_) => {}
                },
                Some(Err(source)) => return Err(ClientError::Receive { source }),
                None => {
                    // A clean close without a terminal frame is the server's
                    // orderly-shutdown signature: bail with 128 + SIGTERM,
                    // never a transport failure.
                    return Ok(Outcome::just(SHUTDOWN_EXIT));
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
    env: &[OsString],
    command: &[OsString],
) -> Result<Request, ClientError> {
    let cwd = match cwd {
        Some(cwd) => cwd,
        None => std::env::current_dir().map_err(|source| ClientError::CurrentDir { source })?,
    };
    // `NCAP_ENV_FORWARD` itself travels through `var_os`: present-but-non-Unicode
    // is lossy-decoded (then fails as malformed JSON → exit 1), not treated as absent.
    let forward =
        std::env::var_os("NCAP_ENV_FORWARD").map(|raw| raw.to_string_lossy().into_owned());
    let env = build_env(env, forward.as_deref(), |name| std::env::var_os(name))?;
    let mut lossy = command.iter().map(|arg| arg.to_string_lossy().into_owned());
    let name = lossy.next().expect("session rejects an empty command");
    Ok(Request {
        command: name,
        args: lossy.collect(),
        cwd: cwd.to_string_lossy().into_owned(),
        env,
        version: Some(CURRENT_VERSION.into()),
    })
}

/// Merge the request env: every name in `NCAP_ENV_FORWARD` (a JSON array of
/// variable names) resolved from this process first, then the `--env` flags —
/// later-wins by key, deduplicated, unset entries silently omitted, an empty
/// `--env` key an error. A forward list that is not a JSON array of names is
/// an error. Present-but-non-Unicode values forward lossily (`U+FFFD`); only
/// unset names are omitted.
fn build_env(
    cli: &[OsString],
    forward: Option<&str>,
    lookup: impl Fn(&str) -> Option<OsString>,
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
            apply_entry(&mut entries, name, value.to_string_lossy().into_owned());
        }
    }
    for flag in cli {
        if let Some((key, value)) = resolve_flag(flag, &lookup)? {
            apply_entry(&mut entries, &key, value);
        }
    }
    Ok(entries
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect())
}

/// One `--env` flag: `KEY=VALUE` carries an explicit value, bare `KEY` copies
/// from this process when set. An empty key (`=VALUE`, or the empty string) is
/// an error. The flag is lossy-decoded first so non-UTF-8 bytes arrive as
/// `U+FFFD`, never rejected.
fn resolve_flag(
    flag: &OsString,
    lookup: impl Fn(&str) -> Option<OsString>,
) -> Result<Option<(String, String)>, ClientError> {
    let flag = flag.to_string_lossy();
    match flag.split_once('=') {
        Some(("", _)) => Err(ClientError::EnvFlag {
            flag: flag.into_owned(),
        }),
        Some((key, value)) => Ok(Some((key.to_string(), value.to_string()))),
        None if flag.is_empty() => Err(ClientError::EnvFlag {
            flag: flag.into_owned(),
        }),
        None => Ok(lookup(flag.as_ref())
            .map(|value| (flag.into_owned(), value.to_string_lossy().into_owned()))),
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

/// Classify the child's terminal status into an outcome. 127 and 126 come
/// with a notice the entry point renders; the raw codes alone say nothing
/// actionable.
fn exit_outcome(exit: &Exit, command: &str) -> Result<Outcome, ClientError> {
    match (exit.code, exit.signal) {
        (Some(127), _) => Ok(Outcome::with_notice(
            127,
            format!("{command}: command not found"),
        )),
        (Some(126), _) => Ok(Outcome::with_notice(
            126,
            format!("{command}: permission denied"),
        )),
        (Some(code), _) => Ok(Outcome::just(i32::from(code))),
        (None, Some(signal)) => Ok(Outcome::just(i32::from(signal) + 128)),
        (None, None) => Err(ClientError::MissingExitStatus),
    }
}

/// The session ended without the server ever sending a version frame.
fn warn_absent_version(seen: bool) {
    if !seen {
        // TODO
    }
}

/// Which of the client's own streams a relayed chunk goes to.
enum Stream {
    Stdout,
    Stderr,
}

fn write_stream(stream: Stream, bytes: &[u8]) -> Result<(), ClientError> {
    let result = match stream {
        Stream::Stdout => {
            let mut out = io::stdout().lock();
            out.write_all(bytes).and_then(|()| out.flush())
        }
        Stream::Stderr => {
            let mut out = io::stderr().lock();
            out.write_all(bytes).and_then(|()| out.flush())
        }
    };
    result.map_err(|source| match stream {
        Stream::Stdout => ClientError::WriteStdout { source },
        Stream::Stderr => ClientError::WriteStderr { source },
    })
}

async fn send(
    framed: &mut Framed<UnixStream, FrameCodec>,
    message: Message,
) -> Result<(), ClientError> {
    let frame = message
        .into_frame()
        .map_err(|source| ClientError::Send { source })?;
    framed
        .send(frame)
        .await
        .map_err(|source| ClientError::Send { source })
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use test_case::test_case;

    use super::build_env;

    /// A lookup over literal pairs, standing in for the process environment.
    fn lookup_of<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |name| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(value.to_string()))
        }
    }

    fn owned(items: &[&str]) -> Vec<OsString> {
        items.iter().map(OsString::from).collect()
    }

    fn expected_strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    #[test_case(&["K=V"], None, &[], &["K=V"] ; "explicit_key_value_passes_through")]
    #[test_case(&["K"], None, &[("K", "host")], &["K=host"] ; "bare_key_copies_when_set")]
    #[test_case(&["K"], None, &[], &[] ; "bare_key_omitted_when_unset")]
    #[test_case(&["K="], None, &[], &["K="] ; "empty_value_is_explicit")]
    #[test_case(&["K=a=b"], None, &[], &["K=a=b"] ; "value_may_carry_equals")]
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
        assert_eq!(merged, expected_strings(expected));
    }

    #[test_case("not json" ; "malformed_json")]
    #[test_case(r#"{"a": 1}"# ; "object_is_not_an_array")]
    #[test_case(r#"["K", 1]"# ; "non_string_entry")]
    fn malformed_forward_is_an_error(forward: &str) {
        let err =
            build_env(&[], Some(forward), lookup_of(&[])).expect_err("malformed forward errors");
        assert!(err.to_string().contains("NCAP_ENV_FORWARD"), "error={err}");
    }

    #[test_case(&["=V"], "`--env` `=V`" ; "value_only")]
    #[test_case(&["="], "`--env` `=`" ; "equals_alone")]
    #[test_case(&[""], "`--env` ``" ; "empty_string")]
    #[test_case(&["=V", "K=V"], "empty key" ; "errors_before_a_later_valid_flag_merges")]
    fn empty_key_flag_is_an_error(cli: &[&str], expected_substring: &str) {
        let err = build_env(&owned(cli), None, lookup_of(&[])).expect_err("empty key errors");
        assert!(err.to_string().contains(expected_substring), "error={err}");
    }
}
