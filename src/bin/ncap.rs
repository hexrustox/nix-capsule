use std::path::PathBuf;

use clap::Parser;

/// Execute commands inside the container shell over the project socket
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Unix socket path of the project's server
    #[arg(short, long, value_name = "PATH", env = "NCAP_SOCKET")]
    socket: Option<PathBuf>,

    /// Working directory for the command inside the container
    #[arg(short, long, value_name = "PATH")]
    cwd: Option<PathBuf>,

    /// Command and its arguments
    #[arg(trailing_var_arg = true, required = true)]
    command: Vec<String>,
}

fn main() {
    let _cli = Cli::parse();
}
