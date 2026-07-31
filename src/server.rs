use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Result, anyhow};
use clap::Parser;
use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixListener;
use tokio::process::Command;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinSet;
use tokio_util::codec::{FramedRead, FramedWrite};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{EnvFilter, fmt, layer::SubscriberExt, util::SubscriberInitExt};

use nix_capsule::protocol::{
    CURRENT_VERSION, ErrorMessage, Exit, Frame, FrameCodec, FrameType, Message, Request, Role,
    ServerStopping, VersionCheck, VersionMsg, exit_code_from,
};

#[derive(Parser)]
#[command(version, about = "Server for the nix-capsule")]
struct Cli {
    /// Unix socket path to listen on
    #[arg(short, long)]
    socket: String,

    /// Directory for log files (default: same directory as socket)
    #[arg(short, long)]
    log_dir: Option<String>,

    /// Drain timeout in seconds for active connections on shutdown
    #[arg(long)]
    timeout: u64,
}

fn init_logging(log_dir: &Path) -> WorkerGuard {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let log_filename = nix_capsule::path::log_filename(secs);

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
        tracing::error!("failed to bind socket `{}`: {}", cli.socket, e);
    })?;

    tracing::info!("listening on `{}`", cli.socket);

    let (shutdown_tx, _) = broadcast::channel::<()>(1);
    let mut accept_shutdown = shutdown_tx.subscribe();

    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sigint = signal(SignalKind::interrupt())?;

    let mut join_set: JoinSet<()> = JoinSet::new();

    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok((stream, _)) => {
                        let rx = shutdown_tx.subscribe();
                        join_set.spawn(async move {
                            if let Err(e) = handle_connection(stream, rx).await {
                                tracing::error!("{e}");
                            }
                        });
                    }
                    Err(e) => {
                        tracing::error!("failed to accept connection: {}", e);
                    }
                }
            }
            _ = accept_shutdown.recv() => {
                break;
            }
            _ = sigterm.recv() => {
                tracing::info!("received SIGTERM, shutting down");
                let _ = shutdown_tx.send(());
                break;
            }
            _ = sigint.recv() => {
                tracing::info!("received SIGINT, shutting down");
                let _ = shutdown_tx.send(());
                break;
            }
        }
    }

    tracing::info!("waiting for active connections to drain");
    tokio::time::timeout(std::time::Duration::from_secs(cli.timeout), async {
        while let Some(result) = join_set.join_next().await {
            if let Err(e) = result {
                tracing::error!("connection handler panicked: {e}");
            }
        }
    })
    .await
    .unwrap_or_else(|_| tracing::warn!("timed out waiting for connections to drain"));
    tracing::info!("server shut down");

    let _ = std::fs::remove_file(&cli.socket);

    Ok(())
}

async fn handle_connection(
    stream: tokio::net::UnixStream,
    mut shutdown_rx: broadcast::Receiver<()>,
) -> Result<()> {
    let (read_half, write_half) = stream.into_split();
    let mut framed_read = FramedRead::new(read_half, FrameCodec);
    let mut framed_write = FramedWrite::new(write_half, FrameCodec);

    let first_frame = tokio::select! {
        frame = framed_read.next() => {
            frame
                .transpose()
                .map_err(|e| anyhow!("failed to read first frame: {}", e))?
                .ok_or_else(|| anyhow!("connection closed before request"))?
        }
        _ = shutdown_rx.recv() => {
            let _ = send_server_stopping(&mut framed_write, None).await;
            return Ok(());
        }
    };

    if first_frame.frame_type != FrameType::Request {
        let _ = send_error(
            &mut framed_write,
            format!("expected Request frame, got {:?}", first_frame.frame_type),
            Some("protocol violation"),
        )
        .await;
        return Ok(());
    }

    let request: Request = match Message::from_frame(first_frame) {
        Ok(Message::Request(r)) => r,
        Err(e) => return Err(anyhow!("failed to parse request: {}", e)),
        _ => unreachable!("frame_type checked above"),
    };

    tracing::debug!("received request: `{}`", request.command_line());

    framed_write
        .send(
            Message::Version(VersionMsg {
                version: CURRENT_VERSION.to_string(),
            })
            .into_frame()
            .map_err(|e| anyhow!("failed to serialize version frame: {e}"))?,
        )
        .await
        .map_err(|e| anyhow!("failed to send version frame: {e}"))?;

    let client_version_msg = request
        .version
        .as_ref()
        .map(|v| VersionMsg { version: v.clone() });
    report_version_check(VersionCheck::from(
        client_version_msg.as_ref(),
        CURRENT_VERSION,
        Role::Server,
    ));

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
                    "failed to run `{}` in `{}`",
                    request.command_line(),
                    request.cwd
                )),
            )
            .await;
        }
    };

    tracing::info!("executing `{}`", request.command_line());

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
        while let Some(frame) = rx.recv().await {
            if framed_write.send(frame).await.is_err() {
                break;
            }
        }
        framed_write
    });

    tokio::select! {
        result = child.wait() => {
            stdin_task.abort();

            let _ = stdout_task.await;
            let _ = stderr_task.await;

            let mut framed_write = writer_task
                .await
                .map_err(|e| anyhow!("writer task panicked: {e}"))?;

            match result {
                Ok(status) => {
                    let exit_code = exit_code_from(&status);
                    tracing::info!("command finished with exit_code {}", exit_code);

                    let exit = Exit { exit_code };
                    if let Err(e) = framed_write
                        .send(Message::Exit(exit).into_frame().expect("Exit encode"))
                        .await
                    {
                        tracing::error!("failed to send Exit: {e}");
                    }
                }
                Err(e) => {
                    send_error(
                        &mut framed_write,
                        e.to_string(),
                        Some(&format!("failed to wait on `{}`", request.command_line())),
                    )
                    .await?;
                }
            }
        }
        _ = shutdown_rx.recv() => {
            tracing::info!("server shutting down, terminating child");
            stdin_task.abort();
            let _ = child.kill().await;
            let _ = child.wait().await;
            let _ = stdout_task.await;
            let _ = stderr_task.await;
            let mut framed_write = writer_task
                .await
                .map_err(|e| anyhow!("writer task panicked: {e}"))?;
            let _ = send_server_stopping(&mut framed_write, None).await;
        }
    }

    Ok(())
}

async fn forward_output(
    mut src: impl AsyncRead + Unpin,
    frame_type: FrameType,
    tx: mpsc::Sender<Frame>,
    label: &str,
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
                tracing::error!("failed to read `{label}`: {e}");
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
    let msg = ErrorMessage {
        error,
        cause: context.map(|s| s.to_string()),
    };
    let frame = Message::Error(msg)
        .into_frame()
        .map_err(|e| anyhow!("failed to encode ErrorMessage: {e}"))?;
    sink.send(frame)
        .await
        .map_err(|e| anyhow!("failed to send ErrorMessage: {e}"))?;
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
    let frame = Message::ServerStopping(msg)
        .into_frame()
        .map_err(|e| anyhow!("failed to encode ServerStopping: {e}"))?;
    sink.send(frame)
        .await
        .map_err(|e| anyhow!("failed to send ServerStopping: {e}"))?;
    Ok(())
}

fn report_version_check(check: VersionCheck) {
    match check {
        VersionCheck::Match => {}
        VersionCheck::Mismatch { client, server } => {
            tracing::warn!("client/server version mismatch: client={client}, server={server}");
        }
        VersionCheck::ClientMissing { server } => {
            tracing::warn!("client did not send version (server={server})");
        }
        VersionCheck::ServerMissing { client } => {
            tracing::warn!("server did not send version (client={client})");
        }
    }
}
