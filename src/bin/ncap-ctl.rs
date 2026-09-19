use clap::Parser;

use nix_capsule::ctl::{cli::Cli, run};

fn main() {
    let cli = Cli::parse();
    let rt = tokio::runtime::Runtime::new().expect("spawn tokio runtime");
    let outcome = rt.block_on(run(cli.command));
    if let Some(warning) = outcome.warning {
        eprintln!("{}: {warning}", env!("CARGO_BIN_NAME"));
    }
    if let Some(message) = outcome.error {
        eprintln!("{}: {message}", env!("CARGO_BIN_NAME"));
        std::process::exit(1);
    }
}
