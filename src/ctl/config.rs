//! Env-contract resolution: uniform demands, derivation chains, and
//! defaults. Every command demands the full set — a missing
//! var is named in the error.

use std::path::{Path, PathBuf};

use crate::ctl::{names, paths};

/// Which ctl command is being resolved — the demand set depends on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cmd {
    Init,
    Start,
    Stop,
    Restart,
    Status,
    Enter,
    Log,
    Clean,
    ShowOptions,
}

/// Resolved configuration for one command. Fields a command does not use are
/// `None`; the flows unwrap what they demanded upfront, so an `expect`
/// there is a programmer bug, not a user error.
#[derive(Debug)]
pub struct Config {
    pub root: Option<PathBuf>,
    pub project: Option<String>,
    pub container: String,
    pub socket: Option<PathBuf>,
    pub cache_dir: Option<PathBuf>,
    pub log_dir: Option<PathBuf>,
    pub runtime: String,
    pub timeout: u64,
    pub watch_files: Vec<String>,
    pub run_opts: Vec<String>,
    pub harden: bool,
    pub image: Option<String>,
    pub server: Option<PathBuf>,
    pub nix: Option<PathBuf>,
    pub bash: Option<PathBuf>,
    pub devshell: Option<String>,
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
    match lookup_non_empty(lookup, "NCAP_WATCH_FILES") {
        None => Ok(Vec::new()),
        Some(raw) => serde_json::from_str(&raw).map_err(|source| Error::NotJsonArray {
            var: "NCAP_WATCH_FILES",
            source,
        }),
    }
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
    match lookup_non_empty(lookup, "NCAP_TIMEOUT") {
        None => Err(Error::Missing {
            var: "NCAP_TIMEOUT",
        }),
        Some(raw) => raw.parse().map_err(|source| Error::BadTimeout { source }),
    }
}

fn parse_run_opts(lookup: &dyn Fn(&str) -> Option<String>) -> Result<Vec<String>, Error> {
    match lookup_non_empty(lookup, "NCAP_RUN_OPTS") {
        None => Ok(Vec::new()),
        Some(raw) => serde_json::from_str(&raw).map_err(|source| Error::NotJsonArray {
            var: "NCAP_RUN_OPTS",
            source,
        }),
    }
}

fn parse_harden(lookup: &dyn Fn(&str) -> Option<String>) -> Result<bool, Error> {
    match lookup_non_empty(lookup, "NCAP_HARDEN") {
        None => Ok(false),
        Some(raw) => match raw.as_str() {
            "true" => Ok(true),
            "false" => Ok(false),
            _ => Err(Error::BadHarden { value: raw }),
        },
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
    match names::sanitize(basename) {
        Some(name) => Ok(name),
        None => Err(Error::EmptyProjectName { root: root_str }),
    }
}

/// Resolve the full configuration from `lookup`. All commands share one
/// demand set (normally fully populated by `lib.nix`).
pub fn resolve(lookup: &dyn Fn(&str) -> Option<String>) -> Result<Config, Error> {
    // Runtime/timeout are demanded on every command (missing => error naming
    // the var). JSON-array vars consumed by ctl and harden are validated on
    // every command (missing => default, malformed => error naming the var);
    // `NCAP_ENV_FORWARD` is validated by the Client only.
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
        let xdg = lookup_non_empty(lookup, "XDG_RUNTIME_DIR");
        let tmpdir = lookup_non_empty(lookup, "TMPDIR");
        let dir = paths::runtime_dir(&project, xdg.as_deref(), tmpdir.as_deref());
        paths::socket_path(&dir)
    };
    let cache_dir = if let Some(dir) = lookup_non_empty(lookup, "NCAP_CACHE_DIR") {
        PathBuf::from(dir)
    } else {
        let xdg = lookup_non_empty(lookup, "XDG_CACHE_HOME");
        let home = lookup_non_empty(lookup, "HOME");
        paths::cache_dir(&project, xdg.as_deref(), home.as_deref())?
    };
    let log_dir = if let Some(dir) = lookup_non_empty(lookup, "NCAP_LOG_DIR") {
        PathBuf::from(dir)
    } else {
        let xdg = lookup_non_empty(lookup, "XDG_STATE_HOME");
        let home = lookup_non_empty(lookup, "HOME");
        paths::log_dir(&project, xdg.as_deref(), home.as_deref())?
    };
    let devshell = demand(lookup, "NCAP_DEVSHELL")?;
    let nix = demand(lookup, "NCAP_NIX")?;
    let image = demand(lookup, "NCAP_IMAGE")?;
    let server = demand(lookup, "NCAP_SERVER")?;
    let bash = demand(lookup, "NCAP_BASH")?;
    Ok(Config {
        root: Some(root),
        project: Some(project),
        container,
        socket: Some(socket),
        cache_dir: Some(cache_dir),
        log_dir: Some(log_dir),
        runtime,
        timeout,
        watch_files,
        run_opts,
        harden,
        image: Some(image),
        server: Some(PathBuf::from(server)),
        nix: Some(PathBuf::from(nix)),
        bash: Some(PathBuf::from(bash)),
        devshell: Some(devshell),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn lookup_of(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect()
    }

    fn resolve_with(pairs: &[(&str, &str)]) -> Result<Config, Error> {
        let map = lookup_of(pairs);
        resolve(&|var| map.get(var).cloned())
    }

    fn full_init_env() -> Vec<(&'static str, &'static str)> {
        vec![
            ("NCAP_PROJECT_ROOT", "/tmp/myproj"),
            ("NCAP_IMAGE", "alpine:latest"),
            ("NCAP_SERVER", "/nix/store/server/bin/ncap-server"),
            ("NCAP_NIX", "/nix/store/nix/bin/nix"),
            ("NCAP_BASH", "/nix/store/bash/bin/bash"),
            ("NCAP_DEVSHELL", ".#container"),
            ("NCAP_CONTAINER", "ncap-myproj"),
            ("NCAP_SOCKET", "/run/user/1000/nix-capsule/myproj/ncap.sock"),
            ("NCAP_CACHE_DIR", "/tmp/cache/myproj"),
            ("NCAP_LOG_DIR", "/tmp/logs/myproj"),
            ("NCAP_RUNTIME", "podman"),
            ("NCAP_TIMEOUT", "10"),
            ("NCAP_WATCH_FILES", "[]"),
            ("HOME", "/home/user"),
            ("XDG_RUNTIME_DIR", "/run/user/1000"),
            ("XDG_CACHE_HOME", "/tmp/cache-home"),
            ("XDG_STATE_HOME", "/tmp/state-home"),
        ]
    }

    #[test]
    fn init_with_full_env_resolves() {
        let pairs = full_init_env();
        let cfg = resolve_with(&pairs).expect("resolve");
        assert_eq!(cfg.container, "ncap-myproj");
        assert_eq!(cfg.timeout, 10);
    }

    #[test]
    fn derived_project_from_root_basename() {
        let mut pairs = full_init_env();
        pairs.retain(|(key, _)| *key != "NCAP_PROJECT" && *key != "NCAP_CONTAINER");
        // NCAP_PROJECT absent, NCAP_CONTAINER absent → derive both
        let cfg = resolve_with(&pairs).expect("resolve");
        assert_eq!(cfg.project.as_deref(), Some("myproj"));
        assert_eq!(cfg.container, "ncap-myproj");
    }

    #[test]
    fn explicit_project_overrides_derivation() {
        let mut pairs = full_init_env();
        pairs.push(("NCAP_PROJECT", "custom"));
        pairs.retain(|(key, _)| *key != "NCAP_CONTAINER");
        let cfg = resolve_with(&pairs).expect("resolve");
        assert_eq!(cfg.project.as_deref(), Some("custom"));
        assert_eq!(cfg.container, "ncap-custom");
    }

    #[test]
    fn explicit_container_overrides_derivation() {
        let pairs = full_init_env();
        let cfg = resolve_with(&pairs).expect("resolve");
        assert_eq!(cfg.container, "ncap-myproj");
    }

    #[test]
    fn socket_derived_via_xdg_runtime_dir() {
        let mut pairs = full_init_env();
        pairs.retain(|(key, _)| *key != "NCAP_SOCKET");
        let cfg = resolve_with(&pairs).expect("resolve");
        assert_eq!(
            cfg.socket.as_deref(),
            Some(Path::new("/run/user/1000/nix-capsule/myproj/ncap.sock"))
        );
    }

    #[test]
    fn socket_falls_back_to_tmpdir_flat() {
        let mut pairs = full_init_env();
        pairs.retain(|(key, _)| *key != "NCAP_SOCKET" && *key != "XDG_RUNTIME_DIR");
        pairs.push(("TMPDIR", "/tmp/foo"));
        let cfg = resolve_with(&pairs).expect("resolve");
        assert_eq!(
            cfg.socket.as_deref(),
            Some(Path::new("/tmp/foo/nix-capsule/myproj/ncap.sock"))
        );
    }

    #[test]
    fn cache_dir_falls_back_to_home() {
        let mut pairs = full_init_env();
        pairs.retain(|(key, _)| *key != "NCAP_CACHE_DIR" && *key != "XDG_CACHE_HOME");
        let cfg = resolve_with(&pairs).expect("resolve");
        assert_eq!(
            cfg.cache_dir.as_deref(),
            Some(Path::new("/home/user/.cache/nix-capsule/myproj"))
        );
    }

    #[test]
    fn missing_runtime_is_an_error_naming_it() {
        let mut pairs = full_init_env();
        pairs.retain(|(key, _)| *key != "NCAP_RUNTIME");
        let err = resolve_with(&pairs).expect_err("must error");
        assert!(err.to_string().contains("NCAP_RUNTIME"), "err={err}");
    }

    #[test]
    fn invalid_runtime_is_rejected() {
        let mut pairs = full_init_env();
        for (key, value) in &mut pairs {
            if *key == "NCAP_RUNTIME" {
                *value = "nerdctl";
            }
        }
        let err = resolve_with(&pairs).expect_err("must error");
        assert!(err.to_string().contains("NCAP_RUNTIME"), "err={err}");
    }

    #[test]
    fn absolute_runtime_path_is_rejected() {
        let mut pairs = full_init_env();
        for (key, value) in &mut pairs {
            if *key == "NCAP_RUNTIME" {
                *value = "/usr/local/bin/podman";
            }
        }
        let err = resolve_with(&pairs).expect_err("absolute path must error");
        assert!(err.to_string().contains("NCAP_RUNTIME"), "err={err}");
    }

    #[test]
    fn missing_timeout_is_an_error_naming_it() {
        let mut pairs = full_init_env();
        pairs.retain(|(key, _)| *key != "NCAP_TIMEOUT");
        let err = resolve_with(&pairs).expect_err("must error");
        assert!(err.to_string().contains("NCAP_TIMEOUT"), "err={err}");
    }

    #[test]
    fn empty_project_name_is_a_hard_error() {
        let mut pairs = full_init_env();
        pairs.retain(|(key, _)| *key != "NCAP_PROJECT" && *key != "NCAP_CONTAINER");
        // Root whose basename sanitizes to empty
        for (key, value) in &mut pairs {
            if *key == "NCAP_PROJECT_ROOT" {
                *value = "/tmp/###";
            }
        }
        let err = resolve_with(&pairs).expect_err("must error");
        assert!(err.to_string().contains("set `project`"), "err={err}");
    }

    #[test]
    fn init_demands_ncap_project_root() {
        let mut pairs = full_init_env();
        pairs.retain(|(key, _)| *key != "NCAP_PROJECT_ROOT");
        let err = resolve_with(&pairs).expect_err("must error");
        assert!(err.to_string().contains("NCAP_PROJECT_ROOT"), "err={err}");
    }

    #[test]
    fn init_demands_ncap_image() {
        let mut pairs = full_init_env();
        pairs.retain(|(key, _)| *key != "NCAP_IMAGE");
        let err = resolve_with(&pairs).expect_err("must error");
        assert!(err.to_string().contains("NCAP_IMAGE"), "err={err}");
    }

    #[test]
    fn init_demands_ncap_server() {
        let mut pairs = full_init_env();
        pairs.retain(|(key, _)| *key != "NCAP_SERVER");
        let err = resolve_with(&pairs).expect_err("must error");
        assert!(err.to_string().contains("NCAP_SERVER"), "err={err}");
    }

    #[test]
    fn init_demands_ncap_nix() {
        let mut pairs = full_init_env();
        pairs.retain(|(key, _)| *key != "NCAP_NIX");
        let err = resolve_with(&pairs).expect_err("must error");
        assert!(err.to_string().contains("NCAP_NIX"), "err={err}");
    }

    #[test]
    fn init_demands_ncap_bash() {
        let mut pairs = full_init_env();
        pairs.retain(|(key, _)| *key != "NCAP_BASH");
        let err = resolve_with(&pairs).expect_err("must error");
        assert!(err.to_string().contains("NCAP_BASH"), "err={err}");
    }

    #[test]
    fn init_demands_ncap_devshell() {
        let mut pairs = full_init_env();
        pairs.retain(|(key, _)| *key != "NCAP_DEVSHELL");
        let err = resolve_with(&pairs).expect_err("must error");
        assert!(err.to_string().contains("NCAP_DEVSHELL"), "err={err}");
    }

    #[test]
    fn uniform_resolve_demands_nix_and_devshell() {
        let mut pairs = full_init_env();
        pairs.retain(|(key, _)| *key != "NCAP_NIX" && *key != "NCAP_DEVSHELL");
        let err = resolve_with(&pairs).expect_err("uniform demands must error");
        assert!(
            err.to_string().contains("NCAP_NIX") || err.to_string().contains("NCAP_DEVSHELL"),
            "err={err}"
        );
    }

    #[test]
    fn stop_with_only_container_demands_full_env() {
        let err = resolve_with(&[
            ("NCAP_CONTAINER", "ncap-foo"),
            ("NCAP_RUNTIME", "podman"),
            ("NCAP_TIMEOUT", "10"),
        ])
        .expect_err("uniform resolve needs PROJECT_ROOT");
        assert!(err.to_string().contains("NCAP_PROJECT_ROOT"), "err={err}");
    }

    #[test]
    fn stop_without_anything_demands_runtime_first() {
        let err = resolve_with(&[]).expect_err("must error");
        assert!(err.to_string().contains("NCAP_RUNTIME"), "err={err}");
    }

    #[test]
    fn stop_without_container_demands_project_root() {
        let err = resolve_with(&[("NCAP_RUNTIME", "podman"), ("NCAP_TIMEOUT", "10")])
            .expect_err("must error");
        assert!(err.to_string().contains("NCAP_PROJECT_ROOT"), "err={err}");
    }

    #[test]
    fn stop_with_project_still_demands_project_root() {
        let err = resolve_with(&[
            ("NCAP_PROJECT", "myproj"),
            ("NCAP_RUNTIME", "podman"),
            ("NCAP_TIMEOUT", "10"),
        ])
        .expect_err("uniform resolve needs PROJECT_ROOT");
        assert!(err.to_string().contains("NCAP_PROJECT_ROOT"), "err={err}");
    }

    #[test]
    fn uniform_resolve_validates_ctl_json_vars() {
        for var in ["NCAP_WATCH_FILES", "NCAP_RUN_OPTS"] {
            let mut pairs = full_init_env();
            pairs.retain(|(key, _)| *key != var);
            pairs.push((var, "not json"));
            let err = resolve_with(&pairs).expect_err("must error");
            assert!(err.to_string().contains(var), "var={var} err={err}");
        }
    }

    #[test]
    fn stop_with_bad_harden_errors() {
        let mut pairs = full_init_env();
        pairs.retain(|(key, _)| *key != "NCAP_HARDEN");
        pairs.push(("NCAP_HARDEN", "yes"));
        let err = resolve_with(&pairs).expect_err("must error");
        assert!(err.to_string().contains("NCAP_HARDEN"), "err={err}");
    }

    #[test]
    fn status_with_minimal_env_demands_full_env() {
        let err = resolve_with(&[
            ("NCAP_CONTAINER", "ncap-foo"),
            ("NCAP_SOCKET", "/tmp/sock"),
            ("NCAP_CACHE_DIR", "/tmp/cache"),
            ("NCAP_WATCH_FILES", "[]"),
            ("NCAP_RUNTIME", "podman"),
            ("NCAP_TIMEOUT", "10"),
        ])
        .expect_err("uniform resolve needs full env");
        assert!(err.to_string().contains("NCAP_PROJECT_ROOT"), "err={err}");
    }

    #[test]
    fn devshell_bare_name_passes_through_raw() {
        let mut pairs = full_init_env();
        for (key, value) in &mut pairs {
            if *key == "NCAP_DEVSHELL" {
                *value = "container";
            }
        }
        let cfg = resolve_with(&pairs).expect("resolve");
        assert_eq!(cfg.devshell.as_deref(), Some("container"));
    }

    #[test]
    fn devshell_uris_pass_through() {
        for uri in [
            ".#container",
            "./flake#container",
            "../other",
            "/abs/path",
            "github:foo/bar",
            "nixpkgs#hello",
            ".",
        ] {
            let mut pairs = full_init_env();
            // Replace devshell value via rebuild to satisfy borrow rules.
            pairs.retain(|(key, _)| *key != "NCAP_DEVSHELL");
            let owned_uri = uri.to_owned();
            let leaked: &'static str = Box::leak(owned_uri.into_boxed_str());
            pairs.push(("NCAP_DEVSHELL", leaked));
            let cfg = resolve_with(&pairs).expect("resolve");
            assert_eq!(cfg.devshell.as_deref(), Some(uri), "uri={uri}");
        }
    }

    #[test]
    fn harden_strict_true_false() {
        let mut pairs = full_init_env();
        pairs.push(("NCAP_HARDEN", "true"));
        let cfg = resolve_with(&pairs).expect("resolve");
        assert!(cfg.harden);
        let mut pairs = full_init_env();
        pairs.push(("NCAP_HARDEN", "false"));
        let cfg = resolve_with(&pairs).expect("resolve");
        assert!(!cfg.harden);
        for bad in ["1", "yes", "0", "TRUE", ""] {
            // Empty string counts as unset => false, not an error.
            if bad.is_empty() {
                continue;
            }
            let mut pairs = full_init_env();
            pairs.push(("NCAP_HARDEN", bad));
            let err = resolve_with(&pairs).expect_err("must error");
            assert!(
                err.to_string().contains("NCAP_HARDEN"),
                "bad={bad} err={err}"
            );
        }
    }

    #[test]
    fn harden_missing_defaults_to_false() {
        let pairs = full_init_env();
        let cfg = resolve_with(&pairs).expect("resolve");
        assert!(!cfg.harden);
    }

    #[test]
    fn malformed_env_forward_is_ignored_by_ctl() {
        let mut pairs = full_init_env();
        pairs.push(("NCAP_ENV_FORWARD", "not json"));
        // Client-only validation: ctl resolves fine.
        resolve_with(&pairs).expect("ctl ignores ENV_FORWARD");
    }

    #[test]
    fn malformed_watch_files_is_an_error() {
        let mut pairs = full_init_env();
        for (key, value) in &mut pairs {
            if *key == "NCAP_WATCH_FILES" {
                *value = "not json";
            }
        }
        let err = resolve_with(&pairs).expect_err("must error");
        assert!(err.to_string().contains("NCAP_WATCH_FILES"), "err={err}");
    }

    #[test]
    fn malformed_timeout_is_an_error() {
        let mut pairs = full_init_env();
        for (key, value) in &mut pairs {
            if *key == "NCAP_TIMEOUT" {
                *value = "ten";
            }
        }
        let err = resolve_with(&pairs).expect_err("must error");
        assert!(err.to_string().contains("NCAP_TIMEOUT"), "err={err}");
    }
}
