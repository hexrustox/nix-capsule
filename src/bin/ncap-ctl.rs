use clap::Parser;

use nix_capsule::ctl::cli::Cli;
use nix_capsule::ctl::run;

fn main() {
    let cli = Cli::parse();
    let rt = tokio::runtime::Runtime::new().expect("spawn tokio runtime");
    let message = rt.block_on(run(cli.command));
    if let Some(message) = message {
        eprintln!("{}: {message}", env!("CARGO_BIN_NAME"));
        std::process::exit(1);
    }
}
