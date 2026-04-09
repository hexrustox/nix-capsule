use std::io::Read;
use std::process::ExitCode;

use clap::Parser;
use color_eyre::{
    Result, Section,
    eyre::{self, Context},
};
use futures::{SinkExt, StreamExt};
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio_util::codec::{FramedRead, FramedWrite};

use nix_capsule::protocol::{ErrorMessage, Exit, Frame, FrameCodec, FrameType, Request};

#[derive(Parser)]
#[command(name = "ncap", about = "Execute commands inside a capsule container")]
struct Cli {
    /// Unix socket path
    #[arg(short, long, env = "NCAP_SOCKET")]
    socket: String,

    /// Environment overrides (KEY=VALUE)
    #[arg(short, long, value_name = "KEY=VALUE")]
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
            .wrap_err("Failed to get current working directory")?
            .to_string_lossy()
            .into_owned(),
    };

    let (command, args) = cli.command.split_first().unwrap();

    let request = Request {
        command: command.clone(),
        args: args.to_vec(),
        cwd,
        env: cli.env,
    };

    let stream = UnixStream::connect(&cli.socket)
        .await
        .wrap_err(format!("Failed to connect to socket: {}", cli.socket))
        .suggestion("Make sure ncap-server is running")?;

    let (read_half, write_half) = stream.into_split();
    let mut framed_read = FramedRead::new(read_half, FrameCodec);
    let mut framed_write = FramedWrite::new(write_half, FrameCodec);

    let request_payload = serde_json::to_vec(&request).wrap_err("Failed to serialize request")?;

    framed_write
        .send(Frame {
            frame_type: FrameType::Request,
            payload: request_payload,
        })
        .await
        .wrap_err("Failed to send request to server")?;

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
        let mut exit_code: i32 = 1;
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
                        .wrap_err("Failed to write stdout")?;
                    stdout.flush().await.wrap_err("Failed to flush stdout")?;
                }
                Some(Ok(Frame {
                    frame_type: FrameType::Stderr,
                    payload,
                })) => {
                    stderr
                        .write_all(&payload)
                        .await
                        .wrap_err("Failed to write stderr")?;
                    stderr.flush().await.wrap_err("Failed to flush stderr")?;
                }
                Some(Ok(Frame {
                    frame_type: FrameType::Exit,
                    payload,
                })) => {
                    let exit: Exit = serde_json::from_slice(&payload)
                        .wrap_err("Failed to parse exit response from server")?;
                    exit_code = exit.exit_code;
                    break;
                }
                Some(Ok(Frame {
                    frame_type: FrameType::Error,
                    payload,
                })) => {
                    let msg: ErrorMessage = serde_json::from_slice(&payload)
                        .wrap_err("Failed to parse error response from server")?;
                    return Err(eyre::Report::msg(msg.error)).wrap_err("Server error");
                }
                Some(Ok(frame)) => {
                    panic!("Unexpected frame: {:?}", frame);
                }
                Some(Err(e)) => {
                    return Err(eyre::Report::from(e))
                        .wrap_err("Socket read error")
                        .note("The server may have crashed or been killed unexpectedly");
                }
                None => {
                    break;
                }
            }
        }
        Ok(exit_code)
    });

    let exit_code = response_task.await.unwrap()?;

    Ok(ExitCode::from(exit_code as u8))
}
