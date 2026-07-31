use std::io::Read;
use std::process::ExitCode;

use clap::{CommandFactory, Parser, Subcommand};
use clap_complete::Shell;
use color_eyre::{
    Result, Section,
    eyre::{Context, Report, eyre},
};
use futures::{SinkExt, StreamExt};
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio_util::codec::{FramedRead, FramedWrite};

use nix_capsule::protocol::{
    CURRENT_VERSION, FrameCodec, Message, Request, Role, SIGTERM_EXIT, VersionCheck,
};

#[derive(Parser)]
#[command(version, about = "Execute commands inside a capsule")]
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
    #[arg(trailing_var_arg = true)]
    command: Vec<String>,

    /// Generate shell completions
    #[command(subcommand)]
    completions: Option<CompletionCmd>,
}

#[derive(Subcommand)]
enum CompletionCmd {
    /// Generate shell completions
    Completions {
        /// Shell to generate completions for
        shell: Shell,
    },
}

#[tokio::main]
async fn main() -> Result<ExitCode> {
    color_eyre::install()?;

    run().await
}

async fn run() -> Result<ExitCode> {
    let cli = Cli::parse();

    if let Some(CompletionCmd::Completions { shell }) = cli.completions {
        let mut cmd = Cli::command();
        let name = cmd.get_name().to_string();
        clap_complete::generate(shell, &mut cmd, name, &mut std::io::stdout());
        return Ok(ExitCode::SUCCESS);
    }

    if cli.command.is_empty() {
        return Err(eyre!("no command specified")).suggestion("run `ncap --help` for usage");
    }

    let cwd = match cli.cwd {
        Some(c) => c,
        None => std::env::current_dir()
            .wrap_err("failed to get current directory")?
            .to_string_lossy()
            .into_owned(),
    };

    let (command, args) = cli
        .command
        .split_first()
        .expect("clap ensures at least one argument");

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
        version: Some(CURRENT_VERSION.to_string()),
    };

    let stream = UnixStream::connect(&cli.socket)
        .await
        .wrap_err(format!("failed to connect to socket: `{}`", cli.socket))
        .suggestion("make sure `ncap-server` is running")?;

    let (read_half, write_half) = stream.into_split();
    let mut framed_read = FramedRead::new(read_half, FrameCodec);
    let mut framed_write = FramedWrite::new(write_half, FrameCodec);

    framed_write
        .send(
            Message::Request(request)
                .into_frame()
                .wrap_err("failed to serialize request")?,
        )
        .await
        .wrap_err("failed to send request")
        .suggestion("check that the server is still running")?;

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
            let frame = Message::Stdin(data).into_frame().expect("Stdin encode");
            if framed_write.send(frame).await.is_err() {
                break;
            }
        }
    });

    let response_task = tokio::spawn(async move {
        let exit_code: u8;
        let mut version_received = false;
        loop {
            let frame = framed_read.next().await;
            match frame {
                Some(Ok(frame)) => {
                    let msg = Message::from_frame(frame)
                        .map_err(|e| Report::from(std::io::Error::other(e.to_string())))
                        .wrap_err("failed to decode frame")?;
                    match msg {
                        Message::Stdout(payload) => {
                            stdout
                                .write_all(&payload)
                                .await
                                .wrap_err("failed to write stdout")?;
                            stdout.flush().await.wrap_err("failed to flush stdout")?;
                        }
                        Message::Stderr(payload) => {
                            stderr
                                .write_all(&payload)
                                .await
                                .wrap_err("failed to write stderr")?;
                            stderr.flush().await.wrap_err("failed to flush stderr")?;
                        }
                        Message::Version(v) => {
                            version_received = true;
                            report_version_check(VersionCheck::from(
                                Some(&v),
                                CURRENT_VERSION,
                                Role::Client,
                            ));
                        }
                        Message::Exit(exit) => {
                            exit_code = exit.exit_code as u8;
                            break;
                        }
                        Message::Error(em) => {
                            return Err(if let Some(cause) = em.cause {
                                eyre!("{cause}: {}", em.error)
                            } else {
                                eyre!("{}", em.error)
                            });
                        }
                        Message::ServerStopping(s) => {
                            let err = if let Some(reason) = s.reason {
                                eyre!("server is shutting down: {reason}")
                            } else {
                                eyre!("server is shutting down")
                            };
                            return Ok((SIGTERM_EXIT, Some(err)));
                        }
                        other => {
                            return Err(eyre!(
                                "protocol error: unexpected frame {:?}",
                                other.frame_type()
                            ));
                        }
                    }
                }
                Some(Err(e)) => {
                    return Err(Report::from(e))
                        .wrap_err("failed to read from socket")
                        .suggestion("the server may have disconnected");
                }
                None => {
                    return Ok((SIGTERM_EXIT, Some(eyre!("server disconnected"))));
                }
            }
        }
        if !version_received {
            report_version_check(VersionCheck::from(None, CURRENT_VERSION, Role::Client));
        }
        Ok((exit_code, None))
    });

    let (exit_code, stop_err) = response_task.await.wrap_err("response task panicked")??;

    if let Some(err) = stop_err {
        eprintln!("{err}");
    }

    Ok(ExitCode::from(exit_code))
}

fn report_version_check(check: VersionCheck) {
    match check {
        VersionCheck::Match => {}
        VersionCheck::Mismatch { client, server } => {
            eprintln!("warning: client/server version mismatch (client={client}, server={server})");
        }
        VersionCheck::ServerMissing { client } => {
            eprintln!("warning: server did not send version (client={client})");
        }
        VersionCheck::ClientMissing { server } => {
            eprintln!("warning: client did not send version (server={server})");
        }
    }
}
