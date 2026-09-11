use std::ffi::OsString;
use std::path::PathBuf;

use clap::{CommandFactory, Parser, Subcommand};

/// Execute commands inside the container shell over the project socket
#[derive(Parser)]
#[command(name = "ncap", version, about)]
struct Cli {
    #[command(subcommand)]
    subcommand: Option<Cmd>,

    /// Unix socket path of the project's server
    #[arg(short, long, value_name = "PATH", env = "NCAP_SOCKET")]
    socket: Option<PathBuf>,

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

#[derive(Subcommand)]
enum Cmd {
    /// Print shell completions
    Completions {
        /// Shell to generate completions for
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

fn main() {
    let cli = Cli::parse();
    if let Some(Cmd::Completions { shell }) = cli.subcommand {
        let mut cmd = Cli::command();
        clap_complete::generate(shell, &mut cmd, "ncap", &mut std::io::stdout());
        return;
    }
    let Some(socket) = cli.socket else {
        Cli::command()
            .error(
                clap::error::ErrorKind::MissingRequiredArgument,
                "the following required arguments were not provided:\n  --socket <PATH>\n",
            )
            .exit();
    };
    if cli.command.is_empty() {
        Cli::command()
            .error(
                clap::error::ErrorKind::MissingRequiredArgument,
                "the following required arguments were not provided:\n  <COMMAND>...\n",
            )
            .exit();
    }
    let runtime = tokio::runtime::Runtime::new().expect("spawn tokio runtime");
    let code = runtime.block_on(nix_capsule::client::run(
        &socket,
        cli.cwd,
        cli.env,
        cli.command,
    ));
    std::process::exit(code);
}
