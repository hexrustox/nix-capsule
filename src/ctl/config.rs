//! Env-contract resolution: uniform demands and defaults. Every command
//! except `SetupEnv` demands the full set — a missing var is named in the
//! error. `SetupEnv` derives the five project-scoped vars and prints them
//! as bash `export` lines for the Host shell to source.

use std::path::{Path, PathBuf};

use super::runtime::Runtime;
use super::shell::shell_escape;
use crate::ctl::paths;
use crate::server::LogLevel;

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
    /// Resolve the project-scoped envs and print them as bash `export` lines
    SetupEnv,
}

/// Resolved configuration for one command. Fields a command does not use are
/// populated anyway; the flows read what they demanded upfront.
#[derive(Debug)]
pub(crate) struct Config {
    /// Project root the container mounts and runs in.
    pub root: PathBuf,
    /// Sanitized project name scoping container, socket, cache, and logs.
    pub project: String,
    /// Container name managed by the runtime adapter.
    pub container: String,
    /// Unix socket path served by `ncap-server`.
    pub socket: PathBuf,
    /// Cache dir holding the env dump, hash, profile, and stamp.
    pub cache_dir: PathBuf,
    /// Log dir holding per-run `ncap-server-*.log` files.
    pub log_dir: PathBuf,
    /// OCI runtime binary (`podman` or `docker`).
    pub runtime: String,
    /// Seconds to wait for liveness before reporting not-live.
    pub timeout: u64,
    /// Project-root-relative watched files gating freshness.
    pub watch_files: Vec<String>,
    /// Extra runtime options appended after the defaults.
    pub run_opts: Vec<String>,
    /// Whether to drop capabilities and bind-mount watches read-only.
    pub harden: bool,
    /// Minimum severity the server logs at.
    pub log_level: LogLevel,
    /// Container image to launch.
    pub image: String,
    /// Server binary path executed inside the container.
    pub server: PathBuf,
    /// Nix binary used for `print-dev-env`.
    pub nix: PathBuf,
    /// Bash binary used for launch and exec commands.
    pub bash: PathBuf,
    /// Devshell attribute evaluated by `print-dev-env`.
    pub devshell: String,
}

impl Config {
    /// A [`Runtime`] scoped to this config's container: the executable name,
    /// container name, and bash path cloned from the config each call.
    pub(crate) fn runtime(&self) -> Runtime {
        Runtime::new(
            self.runtime.clone(),
            self.container.clone(),
            self.bash.clone(),
        )
    }
}

#[derive(Debug, thiserror::Error)]
/// Failures resolving the `NCAP_*` env contract into a [`Config`].
pub(crate) enum ConfigError {
    #[error("required `{var}` is not set")]
    Missing { var: &'static str },
    #[error("cannot derive a project name from root `{root}`")]
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
    #[error("`NCAP_WATCH_FILES` entry `{entry}` is not a project-root-relative path")]
    NotRelativeWatchFile { entry: String },
    #[error("`NCAP_WATCH_FILES` entry `{entry}` is not a file")]
    WatchFileNotFile { entry: String },
    #[error("`NCAP_RUNTIME` must be `podman` or `docker`, got `{value}`")]
    BadRuntime { value: String },
    #[error("`NCAP_HARDEN` must be `true` or `false`, got `{value}`")]
    BadHarden { value: String },
    #[error("`NCAP_LOG_LEVEL` must be `debug`, `info`, `warning`, or `error`, got `{value}`")]
    BadLevel { value: String },
    #[error(transparent)]
    NoHome(#[from] paths::NoHomeError),
}

/// Resolve the full configuration from `lookup`. All commands except
/// `SetupEnv` share one demand set (normally fully populated by `lib.nix`
/// plus a sourced `setup-env`): derived vars are demanded non-empty, never
/// derived here.
pub(crate) fn resolve(lookup: &dyn Fn(&str) -> Option<String>) -> Result<Config, ConfigError> {
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
    let log_level = parse_log_level(lookup)?;

    let root_str = demand(lookup, "NCAP_PROJECT_ROOT")?;
    let root = PathBuf::from(&root_str);
    // Relative-path and file checks need the root; still eager on every
    // command since `setup-env` never calls this function.
    validate_watch_files(&root, &watch_files)?;
    // Derived vars are populated by `setup-env` in the shellHook; an empty
    // value here is a missing var naming it, not a derivation request.
    let project = demand(lookup, "NCAP_PROJECT")?;
    let container = demand(lookup, "NCAP_CONTAINER")?;
    let socket = PathBuf::from(demand(lookup, "NCAP_SOCKET")?);
    let cache_dir = PathBuf::from(demand(lookup, "NCAP_CACHE_DIR")?);
    let log_dir = PathBuf::from(demand(lookup, "NCAP_LOG_DIR")?);
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
        log_level,
        image,
        server: PathBuf::from(server),
        nix: PathBuf::from(nix),
        bash: PathBuf::from(bash),
        devshell,
    })
}

/// Resolve the five project-scoped vars and render them as bash `export`
/// lines for the Host shell to source. Explicit non-empty values win;
/// empty/unset values derive per the NCAP_* contract (project from the
/// root basename, container as `ncap-<project>`, socket/cache/log from the
/// XDG layout). Fixed order: `PROJECT, CONTAINER, SOCKET, CACHE_DIR,
/// LOG_DIR`. Needs only `NCAP_PROJECT_ROOT`.
pub(crate) fn setup_env(lookup: &dyn Fn(&str) -> Option<String>) -> Result<String, ConfigError> {
    let root_str = demand(lookup, "NCAP_PROJECT_ROOT")?;
    let root = PathBuf::from(&root_str);
    let project = resolve_project(lookup, Some(&root))?;
    let container =
        lookup_non_empty(lookup, "NCAP_CONTAINER").unwrap_or_else(|| format!("ncap-{project}"));
    let socket = if let Some(socket) = lookup_non_empty(lookup, "NCAP_SOCKET") {
        socket
    } else {
        paths::socket_path(&project).to_string_lossy().into_owned()
    };
    let cache_dir = match lookup_non_empty(lookup, "NCAP_CACHE_DIR") {
        Some(dir) => dir,
        None => paths::cache_dir(&project)?.to_string_lossy().into_owned(),
    };
    let log_dir = match lookup_non_empty(lookup, "NCAP_LOG_DIR") {
        Some(dir) => dir,
        None => paths::log_dir(&project)?.to_string_lossy().into_owned(),
    };
    let pairs = [
        ("NCAP_PROJECT", project),
        ("NCAP_CONTAINER", container),
        ("NCAP_SOCKET", socket),
        ("NCAP_CACHE_DIR", cache_dir),
        ("NCAP_LOG_DIR", log_dir),
    ];
    let mut out = String::new();
    for (var, value) in pairs {
        out.push_str(&format!("export {var}='{}'\n", shell_escape(&value)));
    }
    Ok(out)
}

fn lookup_non_empty(lookup: &dyn Fn(&str) -> Option<String>, var: &str) -> Option<String> {
    lookup(var).and_then(|value| if value.is_empty() { None } else { Some(value) })
}

fn demand(
    lookup: &dyn Fn(&str) -> Option<String>,
    var: &'static str,
) -> Result<String, ConfigError> {
    lookup_non_empty(lookup, var).ok_or(ConfigError::Missing { var })
}

fn parse_watch_files(lookup: &dyn Fn(&str) -> Option<String>) -> Result<Vec<String>, ConfigError> {
    let raw = demand(lookup, "NCAP_WATCH_FILES")?;
    serde_json::from_str(&raw).map_err(|source| ConfigError::NotJsonArray {
        var: "NCAP_WATCH_FILES",
        source,
    })
}

/// Every entry must be a project-root-relative path (`Path::is_relative`,
/// no `..` component to escape the root); an entry that exists must be a
/// file — the digest hashes it with `File::open` and `harden` bind-mounts
/// it, so a directory there is an error now, not a permanently stale cache.
/// Absent entries are fine (the digest hashes their absence), and a broken
/// symlink counts as absent, matching `File::open`'s `NotFound`.
fn validate_watch_files(root: &Path, entries: &[String]) -> Result<(), ConfigError> {
    for entry in entries {
        let path = Path::new(entry);
        if !path.is_relative()
            || path
                .components()
                .any(|c| c == std::path::Component::ParentDir)
        {
            return Err(ConfigError::NotRelativeWatchFile {
                entry: entry.clone(),
            });
        }
        if root.join(path).exists() && !root.join(path).is_file() {
            return Err(ConfigError::WatchFileNotFile {
                entry: entry.clone(),
            });
        }
    }
    Ok(())
}

fn parse_runtime(lookup: &dyn Fn(&str) -> Option<String>) -> Result<String, ConfigError> {
    let raw = demand(lookup, "NCAP_RUNTIME")?;
    if raw == "podman" || raw == "docker" {
        Ok(raw)
    } else {
        Err(ConfigError::BadRuntime { value: raw })
    }
}

fn parse_timeout(lookup: &dyn Fn(&str) -> Option<String>) -> Result<u64, ConfigError> {
    let raw = demand(lookup, "NCAP_TIMEOUT")?;
    raw.parse()
        .map_err(|source| ConfigError::BadTimeout { source })
}

fn parse_run_opts(lookup: &dyn Fn(&str) -> Option<String>) -> Result<Vec<String>, ConfigError> {
    let raw = demand(lookup, "NCAP_RUN_OPTS")?;
    serde_json::from_str(&raw).map_err(|source| ConfigError::NotJsonArray {
        var: "NCAP_RUN_OPTS",
        source,
    })
}

fn parse_harden(lookup: &dyn Fn(&str) -> Option<String>) -> Result<bool, ConfigError> {
    let raw = demand(lookup, "NCAP_HARDEN")?;
    match raw.as_str() {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(ConfigError::BadHarden { value: raw }),
    }
}

/// The Server's level vocabulary is the contract: exact match against the
/// four tag strings the log writer emits, mirroring how the runtime adapter
/// and the harden flag are parsed.
fn parse_log_level(lookup: &dyn Fn(&str) -> Option<String>) -> Result<LogLevel, ConfigError> {
    let raw = demand(lookup, "NCAP_LOG_LEVEL")?;
    LogLevel::parse(&raw).ok_or(ConfigError::BadLevel { value: raw })
}

fn resolve_project(
    lookup: &dyn Fn(&str) -> Option<String>,
    root: Option<&Path>,
) -> Result<String, ConfigError> {
    if let Some(project) = lookup_non_empty(lookup, "NCAP_PROJECT") {
        return Ok(project);
    }
    let root = match root {
        Some(root) => root,
        None => {
            return Err(ConfigError::Missing {
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
        None => Err(ConfigError::EmptyProjectName { root: root_str }),
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

    #[test_case(None => matches Err(ConfigError::Missing { var: "NCAP_HARDEN" }) ; "unset_is_missing")]
    #[test_case(Some("") => matches Err(ConfigError::Missing { .. }) ; "empty_string_counts_as_missing")]
    #[test_case(Some("true") => matches Ok(true) ; "true_sets_harden")]
    #[test_case(Some("false") => matches Ok(false) ; "false_clears_harden")]
    #[test_case(Some("true ") => matches Err(ConfigError::BadHarden { value }) if value == "true " ; "trailing_space_is_rejected")]
    #[test_case(Some("yes") => matches Err(ConfigError::BadHarden { value }) if value == "yes" ; "yes_is_not_a_boolean")]
    #[test_case(Some("True") => matches Err(ConfigError::BadHarden { value }) if value == "True" ; "capitalized_true_is_rejected")]
    fn harden_is_exact(raw: Option<&str>) -> Result<bool, ConfigError> {
        parse_harden(&single("NCAP_HARDEN", raw))
    }

    #[test_case(None => matches Err(ConfigError::Missing { var: "NCAP_TIMEOUT" }) ; "missing_is_named")]
    #[test_case(Some("") => matches Err(ConfigError::Missing { .. }) ; "empty_string_counts_as_missing")]
    #[test_case(Some("30") => matches Ok(30) ; "seconds_parse")]
    #[test_case(Some("0") => matches Ok(0) ; "zero_is_a_number")]
    #[test_case(Some("-1") => matches Err(ConfigError::BadTimeout { .. }) ; "negative_is_rejected")]
    #[test_case(Some("1.5") => matches Err(ConfigError::BadTimeout { .. }) ; "fraction_is_rejected")]
    #[test_case(Some("abc") => matches Err(ConfigError::BadTimeout { .. }) ; "not_a_number_is_rejected")]
    fn timeout_is_a_number_of_seconds(raw: Option<&str>) -> Result<u64, ConfigError> {
        parse_timeout(&single("NCAP_TIMEOUT", raw))
    }

    #[test_case(None => matches Err(ConfigError::Missing { var: "NCAP_RUNTIME" }) ; "missing_is_named")]
    #[test_case(Some("") => matches Err(ConfigError::Missing { .. }) ; "empty_string_counts_as_missing")]
    #[test_case(Some("podman") => matches Ok(value) if value == "podman" ; "podman_is_valid")]
    #[test_case(Some("docker") => matches Ok(value) if value == "docker" ; "docker_is_valid")]
    #[test_case(Some("hello-docker") => matches Err(ConfigError::BadRuntime { value }) if value == "hello-docker" ; "substring_is_not_enough")]
    fn runtime_is_exact(raw: Option<&str>) -> Result<String, ConfigError> {
        parse_runtime(&single("NCAP_RUNTIME", raw))
    }

    #[test_case(None => matches Err(ConfigError::Missing { var: "NCAP_LOG_LEVEL" }) ; "missing_is_named")]
    #[test_case(Some("") => matches Err(ConfigError::Missing { .. }) ; "empty_string_counts_as_missing")]
    #[test_case(Some("debug") => matches Ok(level) if level == LogLevel::Debug ; "debug_is_valid")]
    #[test_case(Some("info") => matches Ok(level) if level == LogLevel::Info ; "info_is_valid")]
    #[test_case(Some("warning") => matches Ok(level) if level == LogLevel::Warning ; "warning_is_valid")]
    #[test_case(Some("error") => matches Ok(level) if level == LogLevel::Error ; "error_is_valid")]
    #[test_case(Some("Warning") => matches Err(ConfigError::BadLevel { value }) if value == "Warning" ; "capitalized_is_rejected")]
    #[test_case(Some("verbose") => matches Err(ConfigError::BadLevel { value }) if value == "verbose" ; "off_vocabulary_is_rejected")]
    #[test_case(Some("warn") => matches Err(ConfigError::BadLevel { value }) if value == "warn" ; "abbreviation_is_rejected")]
    #[test_case(Some("debug ") => matches Err(ConfigError::BadLevel { value }) if value == "debug " ; "trailing_space_is_rejected")]
    fn log_level_is_exact(raw: Option<&str>) -> Result<LogLevel, ConfigError> {
        parse_log_level(&single("NCAP_LOG_LEVEL", raw))
    }

    #[test_case(None => matches Err(ConfigError::Missing { var: "NCAP_WATCH_FILES" }) ; "unset_is_missing")]
    #[test_case(Some("") => matches Err(ConfigError::Missing { .. }) ; "empty_string_counts_as_unset")]
    #[test_case(Some(r#"["a","b"]"#) => matches Ok(list) if list == ["a".to_owned(), "b".to_owned()] ; "list_parses")]
    #[test_case(Some(r#"["  spaces  "]"#) => matches Ok(list) if list == ["  spaces  ".to_owned()] ; "entries_are_taken_verbatim")]
    #[test_case(Some("not json") => matches Err(ConfigError::NotJsonArray { var: "NCAP_WATCH_FILES", .. }) ; "malformed_json_names_the_var")]
    #[test_case(Some(r#"{"a": 1}"#) => matches Err(ConfigError::NotJsonArray { .. }) ; "object_is_not_an_array")]
    #[test_case(Some(r#"["k", 1]"#) => matches Err(ConfigError::NotJsonArray { .. }) ; "non_string_entry_is_rejected")]
    fn watch_files_is_a_json_array_of_strings(
        raw: Option<&str>,
    ) -> Result<Vec<String>, ConfigError> {
        parse_watch_files(&single("NCAP_WATCH_FILES", raw))
    }

    #[test_case(&[] => matches Ok(()) ; "empty_list_passes")]
    #[test_case(&["file"] => matches Ok(()) ; "existing_relative_file_passes")]
    #[test_case(&["dir/file"] => matches Ok(()) ; "nested_relative_file_passes")]
    #[test_case(&["absent"] => matches Ok(()) ; "absent_entry_hashes_its_absence")]
    #[test_case(&["link"] => matches Ok(()) ; "symlink_to_a_file_passes")]
    #[test_case(&["broken"] => matches Ok(()) ; "dangling_symlink_counts_as_absent")]
    #[test_case(&["/abs/file"] => matches Err(ConfigError::NotRelativeWatchFile { entry }) if entry == "/abs/file" ; "absolute_entry_is_rejected")]
    #[test_case(&["../escape"] => matches Err(ConfigError::NotRelativeWatchFile { entry }) if entry == "../escape" ; "dotdot_escape_is_rejected")]
    #[test_case(&["dir/../.."] => matches Err(ConfigError::NotRelativeWatchFile { entry }) if entry == "dir/../.." ; "embedded_dotdot_is_rejected")]
    #[test_case(&["dir"] => matches Err(ConfigError::WatchFileNotFile { entry }) if entry == "dir" ; "existing_directory_is_rejected")]
    fn watch_files_are_relative_files(entries: &[&str]) -> Result<(), ConfigError> {
        let root = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(root.path().join("dir")).expect("watched dir");
        std::fs::write(root.path().join("file"), b"x").expect("watched file");
        std::os::unix::fs::symlink("file", root.path().join("link")).expect("symlink");
        std::os::unix::fs::symlink("nowhere", root.path().join("broken")).expect("dangling");
        let owned: Vec<String> = entries.iter().map(|e| e.to_string()).collect();
        validate_watch_files(root.path(), &owned)
    }

    #[test_case(None => matches Err(ConfigError::Missing { var: "NCAP_RUN_OPTS" }) ; "unset_is_missing")]
    #[test_case(Some("") => matches Err(ConfigError::Missing { .. }) ; "empty_string_counts_as_unset")]
    #[test_case(Some(r#"["--rm"]"#) => matches Ok(list) if list == ["--rm".to_owned()] ; "list_parses")]
    #[test_case(Some("[1]") => matches Err(ConfigError::NotJsonArray { var: "NCAP_RUN_OPTS", .. }) ; "non_string_entry_names_the_var")]
    fn run_opts_is_a_json_array_of_strings(raw: Option<&str>) -> Result<Vec<String>, ConfigError> {
        parse_run_opts(&single("NCAP_RUN_OPTS", raw))
    }

    #[test_case(Some("explicit"), Some(Path::new("/###")) => matches Ok(name) if name == "explicit" ; "explicit_project_wins_over_root")]
    #[test_case(Some(""), Some(Path::new("/tmp/my_project")) => matches Ok(name) if name == "my-project" ; "empty_project_falls_back_to_the_root_basename")]
    #[test_case(None, Some(Path::new("/tmp/my_project")) => matches Ok(name) if name == "my-project" ; "root_basename_is_sanitized")]
    #[test_case(None, Some(Path::new("/tmp/My.App")) => matches Ok(name) if name == "My-App" ; "root_basename_case_is_preserved")]
    #[test_case(None, Some(Path::new("/")) => matches Err(ConfigError::EmptyProjectName { root }) if root == "/" ; "unsanitizable_root_is_an_error")]
    #[test_case(None, None => matches Err(ConfigError::Missing { var: "NCAP_PROJECT_ROOT" }) ; "without_root_the_project_var_is_demanded")]
    fn project_name_is_derived(
        project: Option<&str>,
        root: Option<&Path>,
    ) -> Result<String, ConfigError> {
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
    fn sanitizes_basename_with_single_dashes(basename: &str) -> Option<String> {
        sanitize(basename)
    }

    #[test]
    fn setup_env_derives_and_quotes() {
        let lookup = env(vec![
            ("NCAP_PROJECT_ROOT", "/tmp/my_project"),
            ("NCAP_CACHE_DIR", "/tmp/a'b dir"),
        ]);
        let out = setup_env(&lookup).expect("setup-env succeeds");
        assert!(
            out.contains("export NCAP_PROJECT='my-project'\n"),
            "out={out}"
        );
        assert!(
            out.contains("export NCAP_CONTAINER='ncap-my-project'\n"),
            "out={out}"
        );
        assert!(
            out.contains("export NCAP_CACHE_DIR='/tmp/a'\\''b dir'\n"),
            "out={out}"
        );
        // Fixed order: project, container, socket, cache, log.
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 5, "out={out}");
        assert!(lines[0].starts_with("export NCAP_PROJECT="), "out={out}");
        assert!(lines[1].starts_with("export NCAP_CONTAINER="), "out={out}");
        assert!(lines[2].starts_with("export NCAP_SOCKET="), "out={out}");
        assert!(lines[3].starts_with("export NCAP_CACHE_DIR="), "out={out}");
        assert!(lines[4].starts_with("export NCAP_LOG_DIR="), "out={out}");
    }

    #[test]
    fn setup_env_explicit_wins() {
        let lookup = env(vec![
            ("NCAP_PROJECT_ROOT", "/tmp/my_project"),
            ("NCAP_PROJECT", "explicit"),
            ("NCAP_CONTAINER", "custom"),
            ("NCAP_SOCKET", "/tmp/x.sock"),
            ("NCAP_CACHE_DIR", "/tmp/c"),
            ("NCAP_LOG_DIR", "/tmp/l"),
        ]);
        let out = setup_env(&lookup).expect("setup-env succeeds");
        assert!(
            out.contains("export NCAP_PROJECT='explicit'\n"),
            "out={out}"
        );
        assert!(
            out.contains("export NCAP_CONTAINER='custom'\n"),
            "out={out}"
        );
    }

    #[test]
    fn setup_env_needs_root() {
        let lookup = env(vec![]);
        let err = setup_env(&lookup).expect_err("root is demanded");
        assert!(matches!(
            err,
            ConfigError::Missing {
                var: "NCAP_PROJECT_ROOT"
            }
        ));
    }
}
