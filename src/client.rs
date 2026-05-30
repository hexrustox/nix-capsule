use std::io::Read;
use std::process::ExitCode;

use clap::Parser;
use color_eyre::{
    Result, Section,
    eyre::{Context, Report, eyre},
};
use futures::{SinkExt, StreamExt};
use serde_json::from_slice;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio_util::codec::{FramedRead, FramedWrite};

use nix_capsule::protocol::{
    ErrorMessage, Exit, Frame, FrameCodec, FrameType, Request, ServerStopping,
};

#[derive(Parser)]
#[command(version, about = "Execute commands inside a capsule container")]
struct Cli {
    /// Unix socket path
    #[arg(short, long, env = "NCAP_SOCKET")]
    socket: String,

    /// Environment overrides (KEY=VALUE or KEY to pass through from host)
    #[arg(short, long, value_name = "KEY[=VALUE]")]
    env: Vec<String>,

    /// Override working directory
    #[arg(short = 'w', long)]
    cwd: Option<String>,

    /// Command and its arguments
    #[arg(trailing_var_arg = true, required = true)]
    command: Vec<String>,
}

#[tokio::main]
async fn main() -> Result<ExitCode> {
    color_eyre::install()?;

    run().await
}

async fn run() -> Result<ExitCode> {
    let cli = Cli::parse();

    let cwd = match cli.cwd {
        Some(c) => c,
        None => std::env::current_dir()
            .wrap_err("failed to get current directory")?
            .to_string_lossy()
            .into_owned(),
    };

    let (command, args) = cli.command.split_first().expect("clap ensures at least one argument");

    let resolved_env: Vec<String> = cli
        .env
        .iter()
        .filter_map(|e| match e.find('=') {
            Some(_) => Some(e.clone()),
            None => match std::env::var(e) {
                Ok(val) => Some(format!("{e}={val}")),
                Err(_) => None,
            },
        })
        .collect();

    let request = Request {
        command: command.clone(),
        args: args.to_vec(),
        cwd,
        env: resolved_env,
    };

    let stream = UnixStream::connect(&cli.socket)
        .await
        .wrap_err(format!("failed to connect to socket: `{}`", cli.socket))
        .suggestion("make sure `ncap-server` is running")?;

    let (read_half, write_half) = stream.into_split();
    let mut framed_read = FramedRead::new(read_half, FrameCodec);
    let mut framed_write = FramedWrite::new(write_half, FrameCodec);

    let request_payload = serde_json::to_vec(&request).wrap_err("failed to serialize request")?;

    framed_write
        .send(Frame {
            frame_type: FrameType::Request,
            payload: request_payload,
        })
        .await
        .wrap_err("failed to send request")?;

    let mut stdout = tokio::io::stdout();
    let mut stderr = tokio::io::stderr();

    let (stdin_tx, mut stdin_rx) = mpsc::channel::<Vec<u8>>(32);

    std::thread::spawn(move || {
        let mut stdin = std::io::stdin();
        let mut buf = [0u8; 8192];
        loop {
            let n = match stdin.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(_) => break,
            };
            if stdin_tx.blocking_send(buf[..n].to_vec()).is_err() {
                break;
            }
        }
    });

    tokio::spawn(async move {
        let mut framed_write = framed_write;
        while let Some(data) = stdin_rx.recv().await {
            if framed_write
                .send(Frame {
                    frame_type: FrameType::Stdin,
                    payload: data,
                })
                .await
                .is_err()
            {
                break;
            }
        }
    });

    let response_task = tokio::spawn(async move {
        let exit_code: u8;
        loop {
            let frame = framed_read.next().await;
            match frame {
                Some(Ok(Frame {
                    frame_type: FrameType::Stdout,
                    payload,
                })) => {
                    stdout
                        .write_all(&payload)
                        .await
                        .wrap_err("failed to write stdout")?;
                    stdout.flush().await.wrap_err("failed to flush stdout")?;
                }
                Some(Ok(Frame {
                    frame_type: FrameType::Stderr,
                    payload,
                })) => {
                    stderr
                        .write_all(&payload)
                        .await
                        .wrap_err("failed to write stderr")?;
                    stderr.flush().await.wrap_err("failed to flush stderr")?;
                }
                Some(Ok(Frame {
                    frame_type: FrameType::Exit,
                    payload,
                })) => {
                    let exit: Exit =
                        serde_json::from_slice(&payload).wrap_err("failed to parse exit frame")?;
                    exit_code = exit.exit_code as u8;
                    break;
                }
                Some(Ok(Frame {
                    frame_type: FrameType::Error,
                    payload,
                })) => {
                    let msg: ErrorMessage =
                        from_slice(&payload).wrap_err("failed to parse error frame")?;
                    return Err(if let Some(cause) = msg.cause {
                        eyre!("{cause}: {}", msg.error)
                    } else {
                        eyre!("{}", msg.error)
                    });
                }
                Some(Ok(Frame {
                    frame_type: FrameType::ServerStopping,
                    payload,
                })) => {
                    let msg: ServerStopping =
                        from_slice(&payload).wrap_err("failed to parse server shutdown frame")?;
                    let err = if let Some(reason) = msg.reason {
                        eyre!("server is shutting down: {reason}")
                    } else {
                        eyre!("server is shutting down")
                    };
                    return Ok((143, Some(err)));
                }
                Some(Ok(frame)) => {
                    return Err(eyre!(
                        "protocol error: unexpected frame `{:?}`",
                        frame.frame_type
                    ));
                }
                Some(Err(e)) => return Err(Report::from(e)).wrap_err("failed to read from socket"),
                None => {
                    return Ok((143, Some(eyre!("server disconnected"))));
                }
            }
        }
        Ok((exit_code, None))
    });

    let (exit_code, stop_err) = response_task.await.wrap_err("response task panicked")??;

    if let Some(err) = stop_err {
        eprintln!("{err}");
    }

    Ok(ExitCode::from(exit_code))
}
