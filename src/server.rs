use std::process::Stdio;

use anyhow::{Context, Result};
use clap::Parser;
use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio_util::codec::{FramedRead, FramedWrite};

use nix_capsule::protocol::{ErrorMessage, Exit, Frame, FrameCodec, FrameType, Request};

#[derive(Parser)]
#[command(name = "ncap-server", about = "Capsule container server")]
struct Cli {
    /// Unix socket path to listen on
    #[arg(short, long, env = "NCAP_SOCKET")]
    socket: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let _ = std::fs::remove_file(&cli.socket);

    let listener = UnixListener::bind(&cli.socket)
        .with_context(|| format!("failed to bind socket: {}", cli.socket))?;

    eprintln!("listening on {}", cli.socket);

    loop {
        let (stream, _) = listener
            .accept()
            .await
            .context("failed to accept connection")?;

        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream).await {
                eprintln!("connection error: {e:?}");
            }
        });
    }
}

async fn handle_connection(stream: tokio::net::UnixStream) -> Result<()> {
    let (read_half, write_half) = stream.into_split();
    let mut framed_read = FramedRead::new(read_half, FrameCodec);
    let mut framed_write = FramedWrite::new(write_half, FrameCodec);

    let first_frame = framed_read
        .next()
        .await
        .transpose()
        .context("failed to read frame")?
        .context("connection closed before request")?;

    if first_frame.frame_type != FrameType::Request {
        send_error(&mut framed_write, "expected Request frame").await;
        return Ok(());
    }

    let request: Request =
        serde_json::from_slice(&first_frame.payload).context("failed to parse request")?;

    let mut cmd = Command::new(&request.command);
    cmd.args(&request.args);
    cmd.current_dir(&request.cwd);
    cmd.envs(request.env.iter().filter_map(|e| {
        let mut parts = e.splitn(2, '=');
        let key = parts.next()?;
        let val = parts.next().unwrap_or("");
        Some((key, val))
    }));

    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            send_error(&mut framed_write, &format!("failed to spawn: {e}")).await;
            return Ok(());
        }
    };

    let mut child_stdin = child.stdin.take().context("missing child stdin")?;
    let mut child_stdout = child.stdout.take().context("missing child stdout")?;
    let mut child_stderr = child.stderr.take().context("missing child stderr")?;

    // Channel for streaming output frames to the writer task
    let (tx, mut rx) = mpsc::channel::<Frame>(64);
    let tx_stdout = tx.clone();
    let tx_stderr = tx;

    let stdout_task = tokio::spawn(async move {
        let mut buf = [0u8; 8192];
        loop {
            let n = child_stdout.read(&mut buf).await.unwrap_or(0);
            if n == 0 {
                break;
            }
            let frame = Frame {
                frame_type: FrameType::Stdout,
                payload: buf[..n].to_vec(),
            };
            if tx_stdout.send(frame).await.is_err() {
                break;
            }
        }
    });

    let stderr_task = tokio::spawn(async move {
        let mut buf = [0u8; 8192];
        loop {
            let n = child_stderr.read(&mut buf).await.unwrap_or(0);
            if n == 0 {
                break;
            }
            let frame = Frame {
                frame_type: FrameType::Stderr,
                payload: buf[..n].to_vec(),
            };
            if tx_stderr.send(frame).await.is_err() {
                break;
            }
        }
    });

    let stdin_task = tokio::spawn(async move {
        while let Some(Ok(Frame {
            frame_type: FrameType::Stdin,
            payload,
        })) = framed_read.next().await
        {
            if child_stdin.write_all(&payload).await.is_err() {
                break;
            }
        }
    });

    // Writer task: drains the channel and sends frames to the client immediately
    let writer_task = tokio::spawn(async move {
        while let Some(frame) = rx.recv().await {
            if framed_write.send(frame).await.is_err() {
                break;
            }
        }
        framed_write
    });

    let status = child.wait().await.context("failed to wait for child")?;

    // Child no longer needs input
    stdin_task.abort();

    // Wait for output tasks to finish (they see pipe EOF)
    let _ = stdout_task.await;
    let _ = stderr_task.await;
    // tx_stdout and tx_stderr dropped here → channel closes

    // Writer drains any remaining frames, then returns the framed_write
    let mut framed_write = writer_task.await.context("writer task panicked")?;

    let exit = Exit {
        exit_code: status.code().unwrap_or(1),
    };
    let exit_payload = serde_json::to_vec(&exit).context("failed to serialize exit")?;
    let _ = framed_write
        .send(Frame {
            frame_type: FrameType::Exit,
            payload: exit_payload,
        })
        .await;

    Ok(())
}

async fn send_error<S>(sink: &mut S, msg: &str)
where
    S: SinkExt<Frame> + Unpin,
{
    let error = ErrorMessage {
        error: msg.to_string(),
    };
    if let Ok(payload) = serde_json::to_vec(&error) {
        let _ = sink
            .send(Frame {
                frame_type: FrameType::Error,
                payload,
            })
            .await;
    }
}
