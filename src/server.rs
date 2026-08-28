//! Server (`ncap-server`): the per-project daemon inside the container. It
//! serves one child per connection, streaming the child's stdio back to the
//! client over the project socket.

use std::io::ErrorKind;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tokio_util::codec::Framed;

use crate::protocol::{CURRENT_VERSION, ErrorMsg, Exit, FrameCodec, Message, VersionMsg};

/// Bind `socket` and serve connections until the process is stopped.
pub async fn run(socket: PathBuf) -> std::io::Result<()> {
    // Ticket 02 unlinks blindly; the live-vs-stale probe lands with ticket 05.
    match std::fs::remove_file(&socket) {
        Err(err) if err.kind() != ErrorKind::NotFound => return Err(err),
        _ => {}
    }
    let listener = UnixListener::bind(&socket)?;
    loop {
        let (stream, _) = listener.accept().await?;
        tokio::spawn(handle_conn(stream));
    }
}

async fn handle_conn(stream: UnixStream) {
    let mut framed = Framed::new(stream, FrameCodec);

    let request = match framed.next().await {
        Some(Ok(frame)) => match Message::from_frame(frame) {
            Ok(Message::Request(request)) => request,
            Ok(other) => {
                send_error(
                    &mut framed,
                    &format!("expected a Request frame, got {:?}", other.frame_type()),
                )
                .await;
                return;
            }
            Err(err) => {
                send_error(&mut framed, &err.to_string()).await;
                return;
            }
        },
        Some(Err(err)) => {
            send_error(&mut framed, &err.to_string()).await;
            return;
        }
        None => return, // client left before its first frame
    };

    let version = Message::Version(VersionMsg {
        version: CURRENT_VERSION.into(),
    });
    if !send(&mut framed, version).await {
        return;
    }

    if !Path::new(&request.cwd).is_dir() {
        send_error(
            &mut framed,
            &format!("cwd is not a directory: `{}`", request.cwd),
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
                send_error(
                    &mut framed,
                    &format!("invalid env entry `{entry}`: expected `KEY=VALUE`"),
                )
                .await;
                return;
            }
        }
    }
    command
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
                        &format!("failed to spawn `{}`: {err}", request.command),
                    )
                    .await;
                }
            }
            return;
        }
    };

    if bridge(&mut framed, &mut child).await {
        let status = match child.wait().await {
            Ok(status) => status,
            Err(err) => {
                send_error(
                    &mut framed,
                    &format!("failed to wait for `{}`: {err}", request.command),
                )
                .await;
                return;
            }
        };
        let exit = Exit {
            code: status.code().map(|code| code as u8),
            signal: status.signal().map(|signal| signal as u8),
        };
        send(&mut framed, Message::Exit(exit)).await;
    } else {
        let _ = child.start_kill();
    }
}

/// Pump both directions until the child's pipes close (returns true) or the
/// connection dies (returns false, caller kills the child).
async fn bridge(
    framed: &mut Framed<UnixStream, FrameCodec>,
    child: &mut tokio::process::Child,
) -> bool {
    let mut stdin = child.stdin.take();
    let stdout = child.stdout.take().expect("child stdout is piped");
    let stderr = child.stderr.take().expect("child stderr is piped");

    let (tx, mut rx) = mpsc::channel(64);
    tokio::spawn(pump_output(stdout, Message::Stdout, tx.clone()));
    tokio::spawn(pump_output(stderr, Message::Stderr, tx));

    let mut client_writes_open = true;
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
                        let pipe_failed = match stdin.as_mut() {
                            Some(pipe) => pipe.write_all(&bytes).await.is_err(),
                            None => false,
                        };
                        if pipe_failed {
                            stdin = None;
                        }
                    }
                    Ok(_) => {} // Signal forwarding lands with ticket 04
                    Err(err) => {
                        send_error(framed, &err.to_string()).await;
                        return false;
                    }
                },
                Some(Err(err)) => {
                    send_error(framed, &err.to_string()).await;
                    return false;
                }
                None => {
                    // Write-half close is stdin EOF, not a disconnect — a real
                    // disconnect surfaces as a failed send on the rx branch.
                    stdin = None;
                    client_writes_open = false;
                }
            },
        }
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

async fn send_error(framed: &mut Framed<UnixStream, FrameCodec>, message: &str) {
    send(
        framed,
        Message::Error(ErrorMsg {
            message: message.to_string(),
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
