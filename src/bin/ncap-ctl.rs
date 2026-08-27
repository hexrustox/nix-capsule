use clap::{Parser, Subcommand};

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
    let _cli = Cli::parse();
}
