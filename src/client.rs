//! Client (`ncap`): connect to a running `ncap-server` over a Unix socket,
//! send one `Request`, stream the child's stdio back to the terminal, and exit
//! with the child's status.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use futures_util::{SinkExt, StreamExt};
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio_util::codec::Framed;

use crate::protocol::{CURRENT_VERSION, Exit, FrameCodec, Message, Request};

/// Run `command` against the server listening on `socket`.
///
/// `cwd` overrides the working directory the server uses for the child; when
/// `None` it defaults to the client's own current directory. Returns the exit
/// code the client process should report.
pub async fn run(socket: &Path, cwd: Option<PathBuf>, command: Vec<String>) -> i32 {
    match session(socket, cwd, command).await {
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
    #[error("{0}")]
    Transport(String),
}

async fn session(
    socket: &Path,
    cwd: Option<PathBuf>,
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
        Message::Request(build_request(cwd, name, args)?),
    )
    .await?;

    // Stdin travels on a blocking thread so a silent pipe never stalls the
    // frame loop; chunks reach the loop through the channel instead.
    let (stdin_tx, mut stdin_rx) = mpsc::channel(8);
    tokio::spawn(pump_stdin(stdin_tx));

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
                    // ServerStopping (ticket 05) and server misuse of
                    // client-only frames carry nothing actionable yet.
                    Message::Request(_)
                    | Message::Stdin(_)
                    | Message::Signal(_)
                    | Message::ServerStopping => {}
                },
                Some(Err(err)) => return Err(ClientError::Transport(err.to_string())),
                None => {
                    return Err(ClientError::Transport(
                        "connection closed without a terminal frame".into(),
                    ))
                }
            },
            chunk = stdin_rx.recv(), if stdin_open => match chunk {
                Some(Some(bytes)) => send(&mut framed, Message::Stdin(bytes)).await?,
                Some(None) | None => {
                    // EOF on host stdin: closing our write half gives the
                    // child EOF without hanging up the connection.
                    framed.get_mut().shutdown().await?;
                    stdin_open = false;
                }
            },
        }
    }
}

fn build_request(
    cwd: Option<PathBuf>,
    name: &str,
    args: &[String],
) -> Result<Request, ClientError> {
    let cwd = match cwd {
        Some(cwd) => cwd,
        None => std::env::current_dir()?,
    };
    Ok(Request {
        command: name.to_string(),
        args: args.to_vec(),
        cwd: cwd.to_string_lossy().into_owned(),
        env: Vec::new(),
        version: Some(CURRENT_VERSION.into()),
    })
}

fn pump_stdin(tx: mpsc::Sender<Option<Vec<u8>>>) -> tokio::task::JoinHandle<()> {
    tokio::task::spawn_blocking(move || {
        let mut stdin = io::stdin().lock();
        let mut buf = vec![0u8; 8 * 1024];
        loop {
            match stdin.read(&mut buf) {
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
