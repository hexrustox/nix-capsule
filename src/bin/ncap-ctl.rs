use clap::Parser;

use nix_capsule::ctl::config::Cmd;

/// Manage the project's container lifecycle
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

fn main() {
    let cli = Cli::parse();
    let code = block_on(cli.command);
    std::process::exit(code);
}

fn block_on(cmd: Cmd) -> i32 {
    let rt = tokio::runtime::Runtime::new().expect("spawn tokio runtime");
    rt.block_on(nix_capsule::ctl::run(cmd))
}
