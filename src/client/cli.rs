//! The Client's command-line interface: flags for `ncap`.

use std::ffi::OsString;
use std::path::PathBuf;

use clap::Parser;

/// Run COMMAND inside the project's container
#[derive(Parser)]
#[command(version)]
pub struct Cli {
    /// Unix socket path of the project's server
    #[arg(short, long, value_name = "PATH", env = "NCAP_SOCKET")]
    pub socket: PathBuf,

    /// Working directory for the command inside the container
    #[arg(short, long, value_name = "PATH")]
    pub cwd: Option<PathBuf>,

    /// Environment override: `KEY=VALUE`, or bare `KEY` copied from this
    /// process when set
    #[arg(short, long, value_name = "KEY[=VALUE]")]
    pub env: Vec<OsString>,

    /// Command and its arguments
    #[arg(trailing_var_arg = true, required = true)]
    pub command: Vec<OsString>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_command_is_a_usage_error() {
        let err = Cli::try_parse_from(["ncap", "-s", "s.sock"])
            .err()
            .expect("no command must not parse");
        assert!(
            matches!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument),
            "must miss the required command, got: {err}"
        );
    }

    #[test]
    fn args_after_the_first_positional_are_trailing_verbatim() -> Result<(), clap::Error> {
        let cli =
            Cli::try_parse_from(["ncap", "-s", "s.sock", "echo", "--cwd", "/not-a-flag", "-e"])?;
        assert_eq!(cli.command, owned(&["echo", "--cwd", "/not-a-flag", "-e"]));
        Ok(())
    }

    fn owned(items: &[&str]) -> Vec<OsString> {
        items.iter().map(OsString::from).collect()
    }
}
