//! Env-contract resolution: uniform demands, derivation chains, and
//! defaults. Every command demands the full set — a missing
//! var is named in the error.

use std::path::{Path, PathBuf};

use crate::ctl::paths;

/// A `ncap-ctl` subcommand: the variant selects the control flow and the
/// demand set.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::Subcommand)]
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
    /// Print container status
    Status,
    /// Show the latest server log
    Log,
    /// Wipe all project state: cache, state dir, runtime dir
    Clean,
    /// Print the expanded runtime adapter options
    ShowOptions,
}

/// Resolved configuration for one command. Fields a command does not use are
/// `None`; the flows unwrap what they demanded upfront, so an `expect`
/// there is a programmer bug, not a user error.
#[derive(Debug)]
pub struct Config {
    pub root: PathBuf,
    pub project: String,
    pub container: String,
    pub socket: PathBuf,
    pub cache_dir: PathBuf,
    pub log_dir: PathBuf,
    pub runtime: String,
    pub timeout: u64,
    pub watch_files: Vec<String>,
    pub run_opts: Vec<String>,
    pub harden: bool,
    pub image: String,
    pub server: PathBuf,
    pub nix: PathBuf,
    pub bash: PathBuf,
    pub devshell: String,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("required `{var}` is not set")]
    Missing { var: &'static str },
    #[error("cannot derive a project name from root `{root}`; set `project`")]
    EmptyProjectName { root: String },
    #[error("`{var}` is not a JSON array of strings: {source}")]
    NotJsonArray {
        var: &'static str,
        #[source]
        source: serde_json::Error,
    },
    #[error("`NCAP_TIMEOUT` is not a number of seconds: {source}")]
    BadTimeout {
        #[source]
        source: std::num::ParseIntError,
    },
    #[error("`NCAP_RUNTIME` must be `podman`, `docker`, got `{value}`")]
    BadRuntime { value: String },
    #[error("`NCAP_HARDEN` must be `true` or `false`, got `{value}`")]
    BadHarden { value: String },
    #[error(transparent)]
    NoHome(#[from] paths::NoHome),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

fn lookup_non_empty(lookup: &dyn Fn(&str) -> Option<String>, var: &str) -> Option<String> {
    lookup(var).and_then(|value| if value.is_empty() { None } else { Some(value) })
}

fn demand(lookup: &dyn Fn(&str) -> Option<String>, var: &'static str) -> Result<String, Error> {
    lookup_non_empty(lookup, var).ok_or(Error::Missing { var })
}

fn parse_watch_files(lookup: &dyn Fn(&str) -> Option<String>) -> Result<Vec<String>, Error> {
    let raw = demand(lookup, "NCAP_WATCH_FILES")?;
    serde_json::from_str(&raw).map_err(|source| Error::NotJsonArray {
        var: "NCAP_WATCH_FILES",
        source,
    })
}

fn parse_runtime(lookup: &dyn Fn(&str) -> Option<String>) -> Result<String, Error> {
    let raw = demand(lookup, "NCAP_RUNTIME")?;
    if raw == "podman" || raw == "docker" {
        Ok(raw)
    } else {
        Err(Error::BadRuntime { value: raw })
    }
}

fn parse_timeout(lookup: &dyn Fn(&str) -> Option<String>) -> Result<u64, Error> {
    let raw = demand(lookup, "NCAP_TIMEOUT")?;
    raw.parse().map_err(|source| Error::BadTimeout { source })
}

fn parse_run_opts(lookup: &dyn Fn(&str) -> Option<String>) -> Result<Vec<String>, Error> {
    let raw = demand(lookup, "NCAP_RUN_OPTS")?;
    serde_json::from_str(&raw).map_err(|source| Error::NotJsonArray {
        var: "NCAP_RUN_OPTS",
        source,
    })
}

fn parse_harden(lookup: &dyn Fn(&str) -> Option<String>) -> Result<bool, Error> {
    let raw = demand(lookup, "NCAP_HARDEN")?;
    match raw.as_str() {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(Error::BadHarden { value: raw }),
    }
}

/// Sanitize a basename into a project name: every non-ASCII-alphanumeric
/// character joins the surrounding run into a single `-`, leading and
/// trailing `-` are stripped. `None` when nothing survives — the caller
/// turns that into the hard "set `project`" error.
fn sanitize(basename: &str) -> Option<String> {
    let mut name = String::with_capacity(basename.len());
    let mut run = false;
    for ch in basename.chars() {
        if ch.is_ascii_alphanumeric() {
            if run && !name.is_empty() {
                name.push('-');
            }
            name.push(ch);
            run = false;
        } else {
            run = true;
        }
    }
    let trimmed = name.trim_matches('-').to_owned();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

fn resolve_project(
    lookup: &dyn Fn(&str) -> Option<String>,
    root: Option<&Path>,
) -> Result<String, Error> {
    if let Some(project) = lookup_non_empty(lookup, "NCAP_PROJECT") {
        return Ok(project);
    }
    let root = match root {
        Some(root) => root,
        None => {
            return Err(Error::Missing {
                var: "NCAP_PROJECT_ROOT",
            });
        }
    };
    let root_str = root.to_string_lossy().into_owned();
    let basename = root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    match sanitize(basename) {
        Some(name) => Ok(name),
        None => Err(Error::EmptyProjectName { root: root_str }),
    }
}

/// Resolve the full configuration from `lookup`. All commands share one
/// demand set (normally fully populated by `lib.nix`).
pub fn resolve(lookup: &dyn Fn(&str) -> Option<String>) -> Result<Config, Error> {
    // Runtime/timeout are demanded on every command (missing => error naming
    // the var). JSON-array vars consumed by ctl and harden are validated and
    // demanded on every command (missing or malformed => error naming the
    // var); `NCAP_ENV_FORWARD` is validated by the Client only.
    let runtime = parse_runtime(lookup)?;
    let timeout = parse_timeout(lookup)?;
    // Eagerly validate the JSON-array vars and harden on every command so
    // malformed values error even where a command does not consume them.
    let watch_files = parse_watch_files(lookup)?;
    let run_opts = parse_run_opts(lookup)?;
    let harden = parse_harden(lookup)?;

    let root_str = demand(lookup, "NCAP_PROJECT_ROOT")?;
    let root = PathBuf::from(&root_str);
    let project = resolve_project(lookup, Some(&root))?;
    let container =
        lookup_non_empty(lookup, "NCAP_CONTAINER").unwrap_or_else(|| format!("ncap-{project}"));
    let socket = if let Some(socket) = lookup_non_empty(lookup, "NCAP_SOCKET") {
        PathBuf::from(socket)
    } else {
        paths::socket_path(&project)
    };
    let cache_dir = match lookup_non_empty(lookup, "NCAP_CACHE_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => paths::cache_dir(&project)?,
    };
    let log_dir = match lookup_non_empty(lookup, "NCAP_LOG_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => paths::log_dir(&project)?,
    };
    let devshell = demand(lookup, "NCAP_DEVSHELL")?;
    let nix = demand(lookup, "NCAP_NIX")?;
    let image = demand(lookup, "NCAP_IMAGE")?;
    let server = demand(lookup, "NCAP_SERVER")?;
    let bash = demand(lookup, "NCAP_BASH")?;
    Ok(Config {
        root,
        project,
        container,
        socket,
        cache_dir,
        log_dir,
        runtime,
        timeout,
        watch_files,
        run_opts,
        harden,
        image,
        server: PathBuf::from(server),
        nix: PathBuf::from(nix),
        bash: PathBuf::from(bash),
        devshell,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use test_case::test_case;

    /// An owned lookup; when a var appears twice the last pair wins, so
    /// `full(extra)` can override the base.
    fn env(pairs: Vec<(&str, &str)>) -> impl Fn(&str) -> Option<String> {
        let pairs: Vec<(String, String)> = pairs
            .into_iter()
            .map(|(var, value)| (var.to_owned(), value.to_owned()))
            .collect();
        move |var| {
            pairs
                .iter()
                .rev()
                .find(|(name, _)| name == var)
                .map(|(_, value)| value.clone())
        }
    }

    fn single(var: &str, value: Option<&str>) -> impl Fn(&str) -> Option<String> {
        env(match value {
            Some(value) => vec![(var, value)],
            None => Vec::new(),
        })
    }

    #[test_case(None => matches Err(Error::Missing { var: "NCAP_HARDEN" }) ; "unset_is_missing")]
    #[test_case(Some("true") => matches Ok(true) ; "true_sets_harden")]
    #[test_case(Some("false") => matches Ok(false) ; "false_clears_harden")]
    #[test_case(Some("true ") => matches Err(Error::BadHarden { value }) if value == "true " ; "trailing_space_is_rejected")]
    #[test_case(Some("yes") => matches Err(Error::BadHarden { value }) if value == "yes" ; "yes_is_not_a_boolean")]
    #[test_case(Some("True") => matches Err(Error::BadHarden { value }) if value == "True" ; "capitalized_true_is_rejected")]
    fn harden_is_exact(raw: Option<&str>) -> Result<bool, Error> {
        parse_harden(&single("NCAP_HARDEN", raw))
    }

    #[test_case(None => matches Err(Error::Missing { var: "NCAP_TIMEOUT" }) ; "missing_is_named")]
    #[test_case(Some("") => matches Err(Error::Missing { .. }) ; "empty_string_counts_as_missing")]
    #[test_case(Some("30") => matches Ok(30) ; "seconds_parse")]
    #[test_case(Some("0") => matches Ok(0) ; "zero_is_a_number")]
    #[test_case(Some("-1") => matches Err(Error::BadTimeout { .. }) ; "negative_is_rejected")]
    #[test_case(Some("1.5") => matches Err(Error::BadTimeout { .. }) ; "fraction_is_rejected")]
    #[test_case(Some("abc") => matches Err(Error::BadTimeout { .. }) ; "not_a_number_is_rejected")]
    fn timeout_is_a_number_of_seconds(raw: Option<&str>) -> Result<u64, Error> {
        parse_timeout(&single("NCAP_TIMEOUT", raw))
    }

    #[test_case(Some("podman") => matches Ok(value) if value == "podman" ; "podman_is_valid")]
    #[test_case(Some("docker") => matches Ok(value) if value == "docker" ; "docker_is_valid")]
    #[test_case(None => matches Err(Error::Missing { var: "NCAP_RUNTIME" }) ; "missing_is_named")]
    #[test_case(Some("") => matches Err(Error::Missing { .. }) ; "empty_string_counts_as_missing")]
    #[test_case(Some("hello-docker") => matches Err(Error::BadRuntime { value }) if value == "hello-docker" ; "substring_is_not_enough")]
    fn runtime_is_exact(raw: Option<&str>) -> Result<String, Error> {
        parse_runtime(&single("NCAP_RUNTIME", raw))
    }

    #[test_case(None => matches Err(Error::Missing { var: "NCAP_WATCH_FILES" }) ; "unset_is_missing")]
    #[test_case(Some("") => matches Err(Error::Missing { .. }) ; "empty_string_counts_as_unset")]
    #[test_case(Some(r#"["a","b"]"#) => matches Ok(list) if list == ["a".to_owned(), "b".to_owned()] ; "list_parses")]
    #[test_case(Some(r#"["  spaces  "]"#) => matches Ok(list) if list == ["  spaces  ".to_owned()] ; "entries_are_taken_verbatim")]
    #[test_case(Some("not json") => matches Err(Error::NotJsonArray { var: "NCAP_WATCH_FILES", .. }) ; "malformed_json_names_the_var")]
    #[test_case(Some(r#"{"a": 1}"#) => matches Err(Error::NotJsonArray { .. }) ; "object_is_not_an_array")]
    #[test_case(Some(r#"["k", 1]"#) => matches Err(Error::NotJsonArray { .. }) ; "non_string_entry_is_rejected")]
    fn watch_files_is_a_json_array_of_strings(raw: Option<&str>) -> Result<Vec<String>, Error> {
        parse_watch_files(&single("NCAP_WATCH_FILES", raw))
    }

    #[test_case(None => matches Err(Error::Missing { var: "NCAP_RUN_OPTS" }) ; "unset_is_missing")]
    #[test_case(Some("") => matches Err(Error::Missing { .. }) ; "empty_string_counts_as_unset")]
    #[test_case(Some(r#"["--rm"]"#) => matches Ok(list) if list == ["--rm".to_owned()] ; "list_parses")]
    #[test_case(Some("[1]") => matches Err(Error::NotJsonArray { var: "NCAP_RUN_OPTS", .. }) ; "non_string_entry_names_the_var")]
    fn run_opts_is_a_json_array_of_strings(raw: Option<&str>) -> Result<Vec<String>, Error> {
        parse_run_opts(&single("NCAP_RUN_OPTS", raw))
    }

    #[test_case(Some("explicit"), Some(Path::new("/###")) => matches Ok(name) if name == "explicit" ; "explicit_project_wins_over_root")]
    #[test_case(Some(""), Some(Path::new("/tmp/my_project")) => matches Ok(name) if name == "my-project" ; "empty_project_falls_back_to_the_root_basename")]
    #[test_case(None, Some(Path::new("/tmp/my_project")) => matches Ok(name) if name == "my-project" ; "root_basename_is_sanitized")]
    #[test_case(None, Some(Path::new("/tmp/My.App")) => matches Ok(name) if name == "My-App" ; "root_basename_case_is_preserved")]
    #[test_case(None, Some(Path::new("/")) => matches Err(Error::EmptyProjectName { root }) if root == "/" ; "unsanitizable_root_is_an_error")]
    #[test_case(None, None => matches Err(Error::Missing { var: "NCAP_PROJECT_ROOT" }) ; "without_root_the_project_var_is_demanded")]
    fn project_name_is_derived(
        project: Option<&str>,
        root: Option<&Path>,
    ) -> Result<String, Error> {
        resolve_project(&single("NCAP_PROJECT", project), root)
    }

    #[test_case("hello" => matches Some(name) if name == "hello" ; "clean_basename_passes_through")]
    #[test_case("my_project" => matches Some(name) if name == "my-project" ; "underscore_run_collapses_to_one_dash")]
    #[test_case("a..b" => matches Some(name) if name == "a-b" ; "dot_run_collapses_to_one_dash")]
    #[test_case("a.-.b" => matches Some(name) if name == "a-b" ; "mixed_run_collapses_to_one_dash")]
    #[test_case("v1.2" => matches Some(name) if name == "v1-2" ; "version_dot_collapses_to_one_dash")]
    #[test_case("-lead-" => matches Some(name) if name == "lead" ; "leading_and_trailing_dashes_are_stripped")]
    #[test_case(".dot." => matches Some(name) if name == "dot" ; "leading_and_trailing_dots_are_stripped")]
    #[test_case("My.App" => matches Some(name) if name == "My-App" ; "case_is_preserved")]
    #[test_case("münchen" => matches Some(name) if name == "m-nchen" ; "non_ascii_letters_are_not_alphanumeric")]
    #[test_case("###" => matches None ; "nothing_surviving_is_none")]
    #[test_case("" => matches None ; "empty_basename_is_none")]
    #[test_case("---" => matches None ; "dashes_only_is_none")]
    fn sanitize_cases(basename: &str) -> Option<String> {
        sanitize(basename)
    }
}
