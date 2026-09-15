use clap::Parser;

use nix_capsule::ctl::config::Cmd;
use nix_capsule::ctl::run;

/// Manage the project's container lifecycle
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

fn main() {
    let cli = Cli::parse();
    let (message, code) = block_on(cli.command);
    if let Some(message) = message {
        eprintln!("{}: {message}", env!("CARGO_BIN_NAME"));
    }
    std::process::exit(code);
}

fn block_on(cmd: Cmd) -> (Option<String>, i32) {
    let rt = tokio::runtime::Runtime::new().expect("spawn tokio runtime");
    rt.block_on(run(cmd))
}
