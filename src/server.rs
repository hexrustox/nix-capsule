//! Server (`ncap-server`): the per-project server inside the container. It
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
                        &format!("failed to spawn `{}`: {err}", request.command),
                    )
                    .await;
                }
            }
            return;
        }
    };
    let pgid = child.id().expect("freshly spawned child has a pid");

    if bridge(&mut framed, &mut child, pgid).await {
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
        // The connection ended before the terminal frame — the only notice
        // is the failed send that ended the bridge (an undecodable frame
        // lands here too). TERM the group and then await the child, however
        // long it takes: the grace after the TERM is the child's, not the
        // server's, so there is no KILL escalation.
        //
        // Accepted limitation: a silent child — no output, stdin already
        // EOF'd — of a vanished client runs to completion; nothing
        // observable triggers detection.
        let _ = signal_group(pgid, libc::SIGTERM as u8);
        let _ = child.wait().await;
    }
}

/// Pump both directions until the child's pipes close (returns true) or the
/// connection dies (returns false, caller TERMs the group and reaps).
async fn bridge(
    framed: &mut Framed<UnixStream, FrameCodec>,
    child: &mut tokio::process::Child,
    pgid: u32,
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
                    Ok(Message::Signal(signal_msg)) => {
                        // A failed kill (already-exited group, out-of-range
                        // number) is one warning line, never an Error frame —
                        // the connection continues to its normal terminal.
                        if let Err(err) = signal_group(pgid, signal_msg.signal) {
                            eprintln!("ncap-server: kill(-{pgid}, {}): {err}", signal_msg.signal);
                        }
                    }
                    Ok(_) => {}
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

#[cfg(test)]
mod tests {
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

        let err = signal_group(pgid, 15).expect_err("a reaped group cannot be signalled");

        assert_eq!(err.raw_os_error(), Some(libc::ESRCH), "err={err}");
    }
}
