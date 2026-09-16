use clap::Parser;

use nix_capsule::client::cli::Cli;
use nix_capsule::client::run;

fn main() {
    let cli = Cli::parse();
    let runtime = tokio::runtime::Runtime::new().expect("spawn tokio runtime");
    let (code, message) = runtime.block_on(run(&cli.socket, cli.cwd, cli.env, cli.command));
    if let Some(message) = message {
        eprintln!("{}: {message}", env!("CARGO_BIN_NAME"));
    }
    std::process::exit(code);
}
