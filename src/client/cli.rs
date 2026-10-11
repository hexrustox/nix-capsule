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
        let err = <Cli as Parser>::try_parse_from(["ncap", "-s", "s.sock"])
            .err()
            .expect("no command must not parse");
        assert!(
            matches!(err.kind(), clap::error::ErrorKind::MissingRequiredArgument),
            "must miss the required command, got: {err}"
        );
    }

    #[test]
    fn args_after_the_first_positional_are_trailing_verbatim() {
        let cli = <Cli as Parser>::try_parse_from([
            "ncap",
            "-s",
            "s.sock",
            "echo",
            "--cwd",
            "/not-a-flag",
            "-e",
        ])
        .unwrap_or_else(|err| panic!("flag-shaped args after COMMAND must parse: {err}"));
        assert_eq!(cli.socket, PathBuf::from("s.sock"));
        assert_eq!(cli.command, owned(&["echo", "--cwd", "/not-a-flag", "-e"]));
    }

    #[test]
    fn short_flags_match_the_long_forms() {
        let cli = <Cli as Parser>::try_parse_from([
            "ncap",
            "-s",
            "/tmp/alt.sock",
            "-c",
            "/work",
            "-e",
            "LAYER=cli",
            "echo",
        ])
        .unwrap_or_else(|err| panic!("short flags must parse: {err}"));
        assert_eq!(cli.socket, PathBuf::from("/tmp/alt.sock"));
        assert_eq!(cli.cwd, Some(PathBuf::from("/work")));
        assert_eq!(cli.env, owned(&["LAYER=cli"]));
        assert_eq!(cli.command, owned(&["echo"]));
    }

    fn owned(items: &[&str]) -> Vec<OsString> {
        items.iter().map(OsString::from).collect()
    }
}
