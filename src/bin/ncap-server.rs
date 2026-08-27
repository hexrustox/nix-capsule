use std::path::PathBuf;

use clap::Parser;

/// Serve exec requests from clients inside the container, streaming their stdio
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Unix socket path to bind
    #[arg(long, value_name = "PATH")]
    socket: PathBuf,

    /// Directory for per-run server logs
    #[arg(long, value_name = "DIR")]
    log_dir: Option<PathBuf>,

    /// Drain grace for live connections at shutdown, seconds
    #[arg(long)]
    timeout: Option<u64>,
}

fn main() {
    let _cli = Cli::parse();
}
