use clap::Parser;
use futures::SinkExt;
use tokio::net::UnixStream;
use tokio::signal::unix::{SignalKind, signal};
use tokio_util::codec::FramedWrite;

use nix_capsule::protocol::{Frame, FrameCodec, FrameType};

#[derive(Parser)]
#[command(name = "ncap-init", about = "Container init process")]
struct Cli {
    /// Unix socket path
    #[arg(short, long, env = "NCAP_SOCKET")]
    socket: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sigint = signal(SignalKind::interrupt())?;

    tokio::select! {
        _ = sigterm.recv() => {}
        _ = sigint.recv() => {}
    }

    if let Ok(stream) = UnixStream::connect(&cli.socket).await {
        let mut framed = FramedWrite::new(stream, FrameCodec);
        let _ = framed
            .send(Frame {
                frame_type: FrameType::RequestShutdown,
                payload: vec![],
            })
            .await;
    }

    Ok(())
}
