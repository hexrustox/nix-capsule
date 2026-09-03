use clap::{Parser, Subcommand};

use nix_capsule::ctl::config::Cmd as CtlCmd;

/// Manage the project's container lifecycle
#[derive(Parser)]
#[command(version, about)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Evaluate the devshell, cache it, and start/restart the container
    Init,
    /// Start the container from a cached env dump
    Start,
    /// Stop the running container
    Stop,
    /// Stop and restart the container
    Restart,
    /// Enter an interactive shell inside the container
    Enter,
    /// Print container status
    Status,
    /// Show the latest server log
    Log,
    /// Wipe all project state: cache, state dir, runtime dir
    Clean,
    /// Print the expanded runtime adapter options
    ShowOptions,
}

fn main() {
    let cli = Cli::parse();
    let code = match cli.command {
        Cmd::Init => block_on(CtlCmd::Init),
        Cmd::Start => block_on(CtlCmd::Start),
        Cmd::Stop => block_on(CtlCmd::Stop),
        Cmd::Restart => block_on(CtlCmd::Restart),
        Cmd::Status => block_on(CtlCmd::Status),
        Cmd::Enter | Cmd::Log | Cmd::Clean | Cmd::ShowOptions => {
            eprintln!("ncap-ctl: not implemented yet (ticket 10)");
            1
        }
    };
    std::process::exit(code);
}

fn block_on(cmd: CtlCmd) -> i32 {
    let rt = tokio::runtime::Runtime::new().expect("spawn tokio runtime");
    rt.block_on(nix_capsule::ctl::run(cmd))
}
