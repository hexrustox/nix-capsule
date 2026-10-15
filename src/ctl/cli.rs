//! The Ctl's command-line interface: the subcommand dispatch of `ncap-ctl`.

use clap::{Args, Parser, Subcommand};

/// Control this project's container lifecycle
#[derive(Parser)]
#[command(version)]
pub struct Cli {
    /// The subcommand controlling the flow
    #[command(subcommand)]
    pub command: Cmd,
}

/// A `ncap-ctl` subcommand: the variant selects the control flow and the
/// demand set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Subcommand)]
pub enum Cmd {
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
    /// Print the container, socket, and cache status
    Status,
    /// Print or follow the newest server log file
    Log {
        /// The `log` flags (no flag keeps the pager behavior)
        #[command(flatten)]
        flags: LogFlags,
    },
    /// Wipe all project state: cache, log dir, socket dir
    Clean,
    /// Print the expanded runtime adapter options
    ShowOptions,
    /// Resolve the project-scoped envs and print them as bash `export` lines
    SetupEnv,
}

/// The CLI flags of the `log` subcommand (spec/ctl.md § log): three flag
/// states — no flag (pager), `--no-pager` (stdout), `--follow` (stream;
/// same stream on Server restarts, ends on Ctrl-C). The two flags are
/// mutually exclusive at parse time: the usage error names both flags.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Args)]
#[group(required = false, multiple = false)]
pub struct LogFlags {
    /// Print the newest server log file to stdout, skipping the pager
    #[arg(long)]
    pub no_pager: bool,
    /// Stream the newest server log file and follow it across Server restarts
    #[arg(long)]
    pub follow: bool,
}

#[cfg(test)]
mod tests {
    use test_case::test_case;

    use super::*;

    #[test_case(&["--no-pager", "--follow"] ; "no_pager_then_follow")]
    #[test_case(&["--follow", "--no-pager"] ; "follow_then_no_pager")]
    fn the_two_flags_together_are_a_usage_error(flags: &[&str]) {
        let err = Cli::try_parse_from([&["ncap-ctl", "log"], flags].concat())
            .err()
            .expect("both flags together must not parse");
        assert!(
            matches!(err.kind(), clap::error::ErrorKind::ArgumentConflict),
            "the two flags together must conflict, got: {err}"
        );
    }

    #[test_case("--no-pager", true, false ; "accepts_no_pager_alone")]
    #[test_case("--follow", false, true ; "accepts_follow_alone")]
    fn log_parses_with_exactly_one_flag_set(
        flag: &str,
        no_pager: bool,
        follow: bool,
    ) -> Result<(), clap::Error> {
        let flags = log_flags_of(Cli::try_parse_from(["ncap-ctl", "log", flag])?);
        assert_eq!(
            flags.no_pager, no_pager,
            "passing `{flag}` alone sets `no_pager`"
        );
        assert_eq!(flags.follow, follow, "passing `{flag}` alone sets `follow`");
        Ok(())
    }

    #[test]
    fn log_with_no_flag_yields_the_default_pager_state() -> Result<(), clap::Error> {
        let flags = log_flags_of(Cli::try_parse_from(["ncap-ctl", "log"])?);
        assert!(!flags.no_pager, "no flags keeps `no_pager` off");
        assert!(!flags.follow, "no flags keeps `follow` off");
        Ok(())
    }

    /// The `Log` subcommand's flags, panicking on any other subcommand —
    /// every test above parses only `log`.
    fn log_flags_of(cli: Cli) -> LogFlags {
        match cli.command {
            Cmd::Log { flags } => flags,
            _ => panic!("expected the `log` subcommand"),
        }
    }
}
