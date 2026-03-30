use std::io::IsTerminal;
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::Parser;
use futures::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
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
async fn main() -> ExitCode {
    match run().await {
        Ok(code) => code,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<ExitCode> {
    let cli = Cli::parse();

    let cwd = match cli.cwd {
        Some(c) => c,
        None => std::env::current_dir()
            .context("failed to get current directory")?
            .to_string_lossy()
            .into_owned(),
    };

    let (command, args) = cli.command.split_first().context("command is required")?;

    let request = Request {
        command: command.clone(),
        args: args.to_vec(),
        cwd,
        env: cli.env,
    };

    let stream = UnixStream::connect(&cli.socket)
        .await
        .with_context(|| format!("failed to connect to socket: {}", cli.socket))?;

    let (read_half, write_half) = stream.into_split();
    let mut framed_read = FramedRead::new(read_half, FrameCodec);
    let mut framed_write = FramedWrite::new(write_half, FrameCodec);

    let request_payload = serde_json::to_vec(&request).context("failed to serialize request")?;

    framed_write
        .send(Frame {
            frame_type: FrameType::Request,
            payload: request_payload,
        })
        .await
        .context("failed to send request")?;

    let mut stdout = tokio::io::stdout();
    let mut stderr = tokio::io::stderr();

    // Forward stdin — owns the write half. Dropping it sends EOF to server.
    // Skip if stdin is a TTY: the blocking read() syscall can't be interrupted
    // by abort(), which would hang the client until the user presses Enter.
    let stdin_is_tty = std::io::stdin().is_terminal();

    let stdin_task = if !stdin_is_tty {
        Some(tokio::spawn(async move {
            let mut framed_write = framed_write;
            let mut stdin = tokio::io::stdin();
            let mut buf = [0u8; 8192];
            loop {
                let n = stdin.read(&mut buf).await.context("failed to read stdin")?;
                if n == 0 {
                    return Ok::<_, anyhow::Error>(());
                }
                framed_write
                    .send(Frame {
                        frame_type: FrameType::Stdin,
                        payload: buf[..n].to_vec(),
                    })
                    .await
                    .context("failed to send stdin frame")?;
            }
            // framed_write dropped here — OwnedWriteHalf shuts down, server sees EOF
        }))
    } else {
        // TTY stdin: drop write half immediately so server sees EOF.
        drop(framed_write);
        None
    };

    // Read responses — owns the read half
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
                        .context("failed to write stdout")?;
                    stdout.flush().await.context("failed to flush stdout")?;
                }
                Some(Ok(Frame {
                    frame_type: FrameType::Stderr,
                    payload,
                })) => {
                    stderr
                        .write_all(&payload)
                        .await
                        .context("failed to write stderr")?;
                    stderr.flush().await.context("failed to flush stderr")?;
                }
                Some(Ok(Frame {
                    frame_type: FrameType::Exit,
                    payload,
                })) => {
                    let exit: Exit =
                        serde_json::from_slice(&payload).context("failed to parse exit")?;
                    exit_code = exit.exit_code;
                    break;
                }
                Some(Ok(Frame {
                    frame_type: FrameType::Error,
                    payload,
                })) => {
                    let err: ErrorMessage =
                        serde_json::from_slice(&payload).context("failed to parse error")?;
                    bail!("server error: {}", err.error);
                }
                Some(Ok(frame)) => {
                    bail!("unexpected frame type: {:?}", frame.frame_type);
                }
                Some(Err(e)) => {
                    return Err(e).context("socket read error");
                }
                None => {
                    break;
                }
            }
        }
        Ok::<i32, anyhow::Error>(exit_code)
    });

    let exit_code = response_task.await.context("response task panicked")??;
    if let Some(task) = stdin_task {
        task.abort();
    }

    Ok(ExitCode::from(exit_code as u8))
}
