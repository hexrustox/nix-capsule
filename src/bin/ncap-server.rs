use std::path::PathBuf;
use std::time::Duration;

use clap::Parser;

/// Serve exec requests from clients inside the container, streaming their stdio
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Unix socket path to bind
    #[arg(long, value_name = "PATH", required = true)]
    socket: PathBuf,

    /// Directory for per-run server logs
    #[arg(long, value_name = "DIR", required = true)]
    log_dir: PathBuf,

    /// Drain grace for live connections at shutdown, seconds
    #[arg(long, required = true)]
    timeout: u64,
}

fn main() {
    let cli = Cli::parse();
    let runtime = tokio::runtime::Runtime::new().expect("spawn tokio runtime");
    let result = runtime.block_on(nix_capsule::server::run(
        cli.socket,
        cli.log_dir,
        Duration::from_secs(cli.timeout),
    ));
    if let Err(err) = result {
        eprintln!("ncap-server: {err}");
        std::process::exit(1);
    }
}
