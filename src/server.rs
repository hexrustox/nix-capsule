use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Result, anyhow};
use clap::Parser;
use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;
use tokio::process::Command;
use tokio::sync::{broadcast, mpsc};
use tokio_util::codec::{FramedRead, FramedWrite};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

use nix_capsule::protocol::{
    ErrorMessage, Exit, Frame, FrameCodec, FrameType, Request, ServerStopping,
};

#[derive(Parser)]
#[command(version, about = "Capsule container server")]
struct Cli {
    /// Unix socket path to listen on
    #[arg(short, long, env = "NCAP_SOCKET")]
    socket: String,

    /// Directory for log files (default: same directory as socket)
    #[arg(short, long, env = "NCAP_LOG_DIR")]
    log_dir: Option<String>,
}

fn init_logging(log_dir: &Path) -> WorkerGuard {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        .to_string();
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

    let (shutdown_tx, _) = broadcast::channel::<()>(1);
    let mut accept_shutdown = shutdown_tx.subscribe();

    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok((stream, _)) => {
                        let tx = shutdown_tx.clone();
                        let rx = shutdown_tx.subscribe();
                        tokio::spawn(async move {
                            if let Err(e) = handle_connection(stream, rx, tx).await {
                                tracing::error!("{e}");
                            }
                        });
                    }
                    Err(e) => {
                        tracing::error!("Failed to accept connection: {}", e);
                    }
                }
            }
            _ = accept_shutdown.recv() => {
                break;
            }
        }
    }

    Ok(())
}

async fn handle_connection(
    stream: tokio::net::UnixStream,
    mut shutdown_rx: broadcast::Receiver<()>,
    shutdown_tx: broadcast::Sender<()>,
) -> Result<()> {
    let (read_half, write_half) = stream.into_split();
    let mut framed_read = FramedRead::new(read_half, FrameCodec);
    let mut framed_write = FramedWrite::new(write_half, FrameCodec);

    let first_frame = tokio::select! {
        frame = framed_read.next() => {
            frame
                .transpose()
                .map_err(|e| anyhow!("Failed to read first frame: {}", e))?
                .ok_or_else(|| anyhow!("Connection closed before request"))?
        }
        _ = shutdown_rx.recv() => {
            return Ok(());
        }
    };

    if first_frame.frame_type == FrameType::RequestShutdown {
        tracing::info!("Received shutdown request from init process");
        tracing::info!("Shutting down");
        let _ = shutdown_tx.send(());
        return Ok(());
    }

    if first_frame.frame_type != FrameType::Request {
        return Ok(());
    }

    let request: Request = serde_json::from_slice(&first_frame.payload)
        .map_err(|e| anyhow!("Failed to parse request: {}", e))?;

    tracing::debug!("Received request: {:?} {:?}", request.command, request.args);

    let mut cmd = Command::new(&request.command);
    cmd.args(&request.args);
    cmd.current_dir(&request.cwd);
    cmd.envs(request.env.iter().filter_map(|e| e.split_once('=')));

    cmd.stdin(Stdio::piped());
    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            return send_error(
                &mut framed_write,
                e.to_string(),
                Some(&format!(
                    "Failed to run \"{}\" in {}", request.command_line(), request.cwd
                )),
            )
            .await;
        }
    };

    tracing::info!("Executing: {} {:?}", request.command, request.args);

    let mut child_stdin = child.stdin.take().expect("stdin configured as piped");
    let child_stdout = child.stdout.take().expect("stdout configured as piped");
    let child_stderr = child.stderr.take().expect("stderr configured as piped");

    let (tx, mut rx) = mpsc::channel::<Frame>(64);
    let tx_stdout = tx.clone();
    let tx_stderr = tx;

    let stdout_task = tokio::spawn(forward_output(
        child_stdout,
        FrameType::Stdout,
        tx_stdout,
        "child stdout",
    ));

    let stderr_task = tokio::spawn(forward_output(
        child_stderr,
        FrameType::Stderr,
        tx_stderr,
        "child stderr",
    ));

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
        loop {
            tokio::select! {
                frame = rx.recv() => {
                    match frame {
                        Some(f) => {
                            if framed_write.send(f).await.is_err() {
                                break;
                            }
                        }
                        None => break,
                    }
                }
                _ = shutdown_rx.recv() => {
                    let _ = send_server_stopping(&mut framed_write, None).await;
                    break;
                }
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
                serde_json::to_vec(&exit).map_err(|e| anyhow!("Failed to serialize Exit: {e}"))?;
            if let Err(e) = framed_write
                .send(Frame {
                    frame_type: FrameType::Exit,
                    payload: exit_payload,
                })
                .await
            {
                tracing::error!("Failed to send Exit: {e}");
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
                Some(&format!("Failed to wait on \"{}\"", request.command_line())),
            )
            .await?;
        }
    };

    Ok(())
}

async fn forward_output(
    mut src: impl AsyncRead + Unpin,
    frame_type: FrameType,
    tx: mpsc::Sender<Frame>,
    label: &'static str,
) {
    let mut buf = [0u8; 8192];
    loop {
        match src.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                let frame = Frame {
                    frame_type,
                    payload: buf[..n].to_vec(),
                };
                if tx.send(frame).await.is_err() {
                    break;
                }
            }
            Err(e) => {
                tracing::error!("Failed to read {label}: {e}");
                break;
            }
        }
    }
}

async fn send_error<S>(sink: &mut S, error: String, context: Option<&str>) -> Result<()>
where
    S: SinkExt<Frame> + Unpin,
    S::Error: std::fmt::Display,
{
    let error = ErrorMessage {
        error,
        cause: context.map(|s| s.to_string()),
    };
    let payload =
        serde_json::to_vec(&error).map_err(|e| anyhow!("Failed to serialize ErrorMessage: {e}"))?;
    sink.send(Frame {
        frame_type: FrameType::Error,
        payload,
    })
    .await
    .map_err(|e| anyhow!("Failed to send ErrorMessage: {e}"))?;
    Ok(())
}

async fn send_server_stopping<S>(sink: &mut S, reason: Option<&str>) -> Result<()>
where
    S: SinkExt<Frame> + Unpin,
    S::Error: std::fmt::Display,
{
    let msg = ServerStopping {
        reason: reason.map(|s| s.to_string()),
    };
    let payload =
        serde_json::to_vec(&msg).map_err(|e| anyhow!("Failed to serialize ServerStopping: {e}"))?;
    sink.send(Frame {
        frame_type: FrameType::ServerStopping,
        payload,
    })
    .await
    .map_err(|e| anyhow!("Failed to send ServerStopping: {e}"))?;
    Ok(())
}
