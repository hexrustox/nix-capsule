//! The Ctl's command-line interface: the subcommand dispatch of `ncap-ctl`.

use clap::Parser;

use super::config::Cmd;

/// The `ncap-ctl` command line, parsed by the binary; kept here so the
/// completion generator can describe it too.
#[derive(Parser)]
#[command(version, about)]
pub struct Cli {
    /// The subcommand controlling the flow
    #[command(subcommand)]
    pub command: Cmd,
}
