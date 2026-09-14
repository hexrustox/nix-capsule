use std::ffi::OsString;
use std::path::PathBuf;

use clap::Parser;

/// Execute commands inside the container shell over the project socket
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Unix socket path of the project's server
    #[arg(short, long, value_name = "PATH", env = "NCAP_SOCKET")]
    socket: PathBuf,

    /// Working directory for the command inside the container
    #[arg(short, long, value_name = "PATH")]
    cwd: Option<PathBuf>,

    /// Environment override: `KEY=VALUE`, or bare `KEY` copied from this
    /// process when set
    #[arg(short, long, value_name = "KEY[=VALUE]")]
    env: Vec<OsString>,

    /// Command and its arguments
    #[arg(trailing_var_arg = true, required = true)]
    command: Vec<OsString>,
}

fn main() {
    let cli = Cli::parse();
    let runtime = tokio::runtime::Runtime::new().expect("spawn tokio runtime");
    let (message, code) = runtime.block_on(nix_capsule::client::run(
        &cli.socket,
        cli.cwd,
        cli.env,
        cli.command,
    ));
    if let Some(message) = message {
        eprintln!("{}: {message}", env!("CARGO_BIN_NAME"));
    }
    std::process::exit(code);
}
