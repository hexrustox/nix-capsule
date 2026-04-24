use chrono::Local;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Result, anyhow};
use clap::Parser;
use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio_util::codec::{FramedRead, FramedWrite};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

use nix_capsule::protocol::{ErrorMessage, Exit, Frame, FrameCodec, FrameType, Request};

#[derive(Parser)]
#[command(name = "ncap-server", about = "Capsule container server")]
struct Cli {
    /// Unix socket path to listen on
    #[arg(short, long, env = "NCAP_SOCKET")]
    socket: String,

    /// Directory for log files (default: same directory as socket)
    #[arg(short, long, env = "NCAP_LOG_DIR")]
    log_dir: Option<String>,
}

fn init_logging(log_dir: &Path) -> WorkerGuard {
    let timestamp = Local::now().format("%Y-%m-%d-%H%M%S").to_string();
    let log_filename = format!("ncap-server-{}.log", timestamp);

    let file_appender = tracing_appender::rolling::never(log_dir, log_filename);
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_writer(non_blocking).with_ansi(false))
        .init();

    guard
}

fn get_log_dir(socket: &str, cli_log_dir: Option<&str>) -> PathBuf {
    if let Some(dir) = cli_log_dir {
        PathBuf::from(dir)
    } else {
        PathBuf::from(socket)
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("/tmp"))
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let log_dir = get_log_dir(&cli.socket, cli.log_dir.as_deref());
    let _guard = init_logging(&log_dir);

    let _ = std::fs::remove_file(&cli.socket);

    let listener = UnixListener::bind(&cli.socket).inspect_err(|e| {
        tracing::error!("Failed to bind socket {}: {}", cli.socket, e);
    })?;

    tracing::info!("Listening on {}", cli.socket);

    loop {
        let (stream, _) = match listener.accept().await {
            Ok((stream, addr)) => (stream, addr),
            Err(e) => {
                tracing::error!("Failed to accept connection: {}", e);
                continue;
            }
        };

        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream).await {
                tracing::error!("{e}");
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
        .map_err(|e| anyhow!("Failed to read first frame: {}", e))?
        .ok_or_else(|| anyhow!("Connection closed before request"))?;

    if first_frame.frame_type != FrameType::Request {
        return Ok(());
    }

    let request: Request = serde_json::from_slice(&first_frame.payload)
        .map_err(|e| anyhow!("Failed to parse request: {}", e))?;

    tracing::debug!("Received request: {:?} {:?}", request.command, request.args);

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
            send_error(
                &mut framed_write,
                e.to_string(),
                Some(&format!(
                    "Failed to run command \"{}\"{}",
                    &request.command,
                    {
                        if request.args.is_empty() {
                            "".to_string()
                        } else {
                            format!(
                                " with arg{} {}",
                                if request.args.len() > 1 { "s" } else { "" },
                                request
                                    .args
                                    .iter()
                                    .map(|a| format!("\"{a}\""))
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            )
                        }
                    }
                )),
            )
            .await;
            return Ok(());
        }
    };

    tracing::info!("Executing: {} {:?}", request.command, request.args);

    let mut child_stdin = child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("Missing child stdin"))?;
    let mut child_stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("Missing child stdout"))?;
    let mut child_stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("Missing child stderr"))?;

    let (tx, mut rx) = mpsc::channel::<Frame>(64);
    let tx_stdout = tx.clone();
    let tx_stderr = tx;

    let stdout_task = tokio::spawn(async move {
        let mut buf = [0u8; 8192];
        loop {
            match child_stdout.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    let frame = Frame {
                        frame_type: FrameType::Stdout,
                        payload: buf[..n].to_vec(),
                    };
                    if tx_stdout.send(frame).await.is_err() {
                        break;
                    }
                }
                Err(e) => {
                    tracing::error!("Error reading child stdout: {e}");
                    break;
                }
            }
        }
    });

    let stderr_task = tokio::spawn(async move {
        let mut buf = [0u8; 8192];
        loop {
            match child_stderr.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    let frame = Frame {
                        frame_type: FrameType::Stderr,
                        payload: buf[..n].to_vec(),
                    };
                    if tx_stderr.send(frame).await.is_err() {
                        break;
                    }
                }
                Err(e) => {
                    tracing::error!("Error reading child stderr: {e}");
                    break;
                }
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

    let writer_task = tokio::spawn(async move {
        while let Some(frame) = rx.recv().await {
            if framed_write.send(frame).await.is_err() {
                break;
            }
        }
        framed_write
    });

    match child.wait().await {
        Ok(status) => {
            stdin_task.abort();

            let _ = stdout_task.await;
            let _ = stderr_task.await;

            let mut framed_write = writer_task
                .await
                .map_err(|e| anyhow!("Writer task panicked: {e}"))?;

            let exit_code = status.code().unwrap_or(1);
            tracing::info!("Command finished with exit_code: {:?}", exit_code);

            let exit = Exit { exit_code };
            let exit_payload =
                serde_json::to_vec(&exit).map_err(|e| anyhow!("Failed to serialize exit: {e}"))?;
            if let Err(e) = framed_write
                .send(Frame {
                    frame_type: FrameType::Exit,
                    payload: exit_payload,
                })
                .await
            {
                tracing::error!("Failed to send exit frame: {e}");
            }
        }
        Err(e) => {
            stdin_task.abort();
            stdout_task.abort();
            stderr_task.abort();

            let mut framed_write = writer_task
                .await
                .map_err(|e| anyhow!("Writer task panicked: {e}"))?;

            send_error(
                &mut framed_write,
                e.to_string(),
                Some("Command was not running"),
            )
            .await;
        }
    };

    Ok(())
}

async fn send_error<S>(sink: &mut S, error: String, context: Option<&str>)
where
    S: SinkExt<Frame> + Unpin,
{
    let error = ErrorMessage {
        error,
        cause: context.map(|s| s.to_string()),
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
