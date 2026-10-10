//! The Client's command-line interface: flags for `ncap`.

use std::ffi::OsString;
use std::path::PathBuf;

use clap::Parser;

/// Run COMMAND inside the project's container
#[derive(Parser)]
#[command(version)]
pub struct Cli {
    /// Unix socket path of the project's server
    #[arg(short, long, value_name = "PATH", env = "NCAP_SOCKET")]
    pub socket: PathBuf,

    /// Working directory for the command inside the container
    #[arg(short, long, value_name = "PATH")]
    pub cwd: Option<PathBuf>,

    /// Environment override: `KEY=VALUE`, or bare `KEY` copied from this
    /// process when set
    #[arg(short, long, value_name = "KEY[=VALUE]")]
    pub env: Vec<OsString>,

    /// Command and its arguments
    #[arg(trailing_var_arg = true, required = true)]
    pub command: Vec<OsString>,
}
