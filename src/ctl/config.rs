//! Env-contract resolution: per-command demands, derivation chains, and
//! defaults. Every command "demands exactly the vars it uses" — a missing
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
}

impl Cmd {
    fn name(self) -> &'static str {
        match self {
            Cmd::Init => "init",
            Cmd::Start => "start",
            Cmd::Stop => "stop",
            Cmd::Restart => "restart",
            Cmd::Status => "status",
        }
    }
}

/// Resolved configuration for one command. Fields a command does not use are
/// `None`; the flows unwrap what they demanded upfront, so an `expect`
/// there is a programmer bug, not a user error.
#[derive(Debug)]
pub struct Config {
    pub cmd: Cmd,
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
    #[error("`{command}` requires `{var}`, which is not set")]
    Missing {
        command: &'static str,
        var: &'static str,
    },
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
    #[error(transparent)]
    NoHome(#[from] paths::NoHome),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

fn lookup_non_empty(lookup: &dyn Fn(&str) -> Option<String>, var: &str) -> Option<String> {
    lookup(var).and_then(|value| {
        if value.is_empty() {
            None
        } else {
            Some(value)
        }
    })
}

fn demand(
    lookup: &dyn Fn(&str) -> Option<String>,
    cmd: Cmd,
    var: &'static str,
) -> Result<String, Error> {
    lookup_non_empty(lookup, var).ok_or(Error::Missing {
        command: cmd.name(),
        var,
    })
}

fn parse_watch_files(
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<Vec<String>, Error> {
    match lookup_non_empty(lookup, "NCAP_WATCH_FILES") {
        None => Ok(Vec::new()),
        Some(raw) => serde_json::from_str(&raw).map_err(|source| Error::NotJsonArray {
            var: "NCAP_WATCH_FILES",
            source,
        }),
    }
}

fn parse_timeout(lookup: &dyn Fn(&str) -> Option<String>) -> Result<u64, Error> {
    match lookup_non_empty(lookup, "NCAP_TIMEOUT") {
        None => Ok(10),
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

fn parse_harden(lookup: &dyn Fn(&str) -> Option<String>) -> bool {
    match lookup_non_empty(lookup, "NCAP_HARDEN") {
        Some(raw) => {
            let lower = raw.to_ascii_lowercase();
            lower == "1" || lower == "true" || lower == "yes"
        }
        None => false,
    }
}

fn resolve_project(
    lookup: &dyn Fn(&str) -> Option<String>,
    cmd: Cmd,
    root: Option<&Path>,
) -> Result<String, Error> {
    if let Some(project) = lookup_non_empty(lookup, "NCAP_PROJECT") {
        return Ok(project);
    }
    let root = match root {
        Some(root) => root,
        None => {
            return Err(Error::Missing {
                command: cmd.name(),
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

/// Resolve the full configuration for `cmd` from `lookup`.
pub fn resolve(
    cmd: Cmd,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<Config, Error> {
    // Runtime and timeout have defaults and are needed by every command that
    // touches the runtime (init/start/restart/stop/status). Resolve them for
    // all commands.
    let runtime = lookup_non_empty(lookup, "NCAP_RUNTIME").unwrap_or_else(|| "podman".to_owned());
    let timeout = parse_timeout(lookup)?;

    match cmd {
        Cmd::Init | Cmd::Restart => {
            let root_str = demand(lookup, cmd, "NCAP_PROJECT_ROOT")?;
            let root = PathBuf::from(&root_str);
            let project = resolve_project(lookup, cmd, Some(&root))?;
            let container = lookup_non_empty(lookup, "NCAP_CONTAINER")
                .unwrap_or_else(|| format!("ncap-{project}"));
            let socket = if let Some(socket) = lookup_non_empty(lookup, "NCAP_SOCKET") {
                PathBuf::from(socket)
            } else {
                let xdg = lookup_non_empty(lookup, "XDG_RUNTIME_DIR");
                let tmpdir = lookup_non_empty(lookup, "TMPDIR");
                let uid = unsafe { libc::getuid() };
                let dir = paths::runtime_dir(&project, xdg.as_deref(), tmpdir.as_deref(), uid);
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
            let watch_files = parse_watch_files(lookup)?;
            let run_opts = parse_run_opts(lookup)?;
            let harden = parse_harden(lookup);
            let devshell = demand(lookup, cmd, "NCAP_DEVSHELL")?;
            let nix = demand(lookup, cmd, "NCAP_NIX")?;
            let image = demand(lookup, cmd, "NCAP_IMAGE")?;
            let server = demand(lookup, cmd, "NCAP_SERVER")?;
            let bash = demand(lookup, cmd, "NCAP_BASH")?;
            Ok(Config {
                cmd,
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
        Cmd::Start => {
            let root_str = demand(lookup, cmd, "NCAP_PROJECT_ROOT")?;
            let root = PathBuf::from(&root_str);
            let project = resolve_project(lookup, cmd, Some(&root))?;
            let container = lookup_non_empty(lookup, "NCAP_CONTAINER")
                .unwrap_or_else(|| format!("ncap-{project}"));
            let socket = if let Some(socket) = lookup_non_empty(lookup, "NCAP_SOCKET") {
                PathBuf::from(socket)
            } else {
                let xdg = lookup_non_empty(lookup, "XDG_RUNTIME_DIR");
                let tmpdir = lookup_non_empty(lookup, "TMPDIR");
                let uid = unsafe { libc::getuid() };
                let dir = paths::runtime_dir(&project, xdg.as_deref(), tmpdir.as_deref(), uid);
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
            let watch_files = parse_watch_files(lookup)?;
            let run_opts = parse_run_opts(lookup)?;
            let harden = parse_harden(lookup);
            let image = demand(lookup, cmd, "NCAP_IMAGE")?;
            let server = demand(lookup, cmd, "NCAP_SERVER")?;
            let bash = demand(lookup, cmd, "NCAP_BASH")?;
            Ok(Config {
                cmd,
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
                nix: None,
                bash: Some(PathBuf::from(bash)),
                devshell: None,
            })
        }
        Cmd::Stop => {
            // Stop only needs the container name (plus runtime). Derive the
            // container when NCAP_CONTAINER is absent.
            let container = if let Some(container) = lookup_non_empty(lookup, "NCAP_CONTAINER") {
                container
            } else {
                // Need project; project may need root.
                let root_opt = lookup_non_empty(lookup, "NCAP_PROJECT_ROOT")
                    .map(PathBuf::from);
                let project = resolve_project(lookup, cmd, root_opt.as_deref())?;
                format!("ncap-{project}")
            };
            Ok(Config {
                cmd,
                root: None,
                project: None,
                container,
                socket: None,
                cache_dir: None,
                log_dir: None,
                runtime,
                timeout,
                watch_files: Vec::new(),
                run_opts: Vec::new(),
                harden: false,
                image: None,
                server: None,
                nix: None,
                bash: None,
                devshell: None,
            })
        }
        Cmd::Status => {
            let watch_files = parse_watch_files(lookup)?;
            let root_opt = lookup_non_empty(lookup, "NCAP_PROJECT_ROOT").map(PathBuf::from);

            let needs_project = lookup_non_empty(lookup, "NCAP_CONTAINER").is_none()
                || lookup_non_empty(lookup, "NCAP_SOCKET").is_none()
                || lookup_non_empty(lookup, "NCAP_CACHE_DIR").is_none()
                || !watch_files.is_empty();

            let project = if needs_project {
                Some(resolve_project(lookup, cmd, root_opt.as_deref())?)
            } else {
                lookup_non_empty(lookup, "NCAP_PROJECT")
            };

            let container = if let Some(container) = lookup_non_empty(lookup, "NCAP_CONTAINER") {
                container
            } else {
                let project = project.clone().ok_or(Error::Missing {
                    command: cmd.name(),
                    var: "NCAP_PROJECT_ROOT",
                })?;
                format!("ncap-{project}")
            };

            let socket = if let Some(socket) = lookup_non_empty(lookup, "NCAP_SOCKET") {
                Some(PathBuf::from(socket))
            } else {
                let project = project.clone().ok_or(Error::Missing {
                    command: cmd.name(),
                    var: "NCAP_PROJECT_ROOT",
                })?;
                let xdg = lookup_non_empty(lookup, "XDG_RUNTIME_DIR");
                let tmpdir = lookup_non_empty(lookup, "TMPDIR");
                let uid = unsafe { libc::getuid() };
                let dir = paths::runtime_dir(&project, xdg.as_deref(), tmpdir.as_deref(), uid);
                Some(paths::socket_path(&dir))
            };

            let cache_dir = if let Some(dir) = lookup_non_empty(lookup, "NCAP_CACHE_DIR") {
                Some(PathBuf::from(dir))
            } else {
                let project = project.clone().ok_or(Error::Missing {
                    command: cmd.name(),
                    var: "NCAP_PROJECT_ROOT",
                })?;
                let xdg = lookup_non_empty(lookup, "XDG_CACHE_HOME");
                let home = lookup_non_empty(lookup, "HOME");
                Some(paths::cache_dir(&project, xdg.as_deref(), home.as_deref())?)
            };

            let log_dir = if let Some(dir) = lookup_non_empty(lookup, "NCAP_LOG_DIR") {
                Some(PathBuf::from(dir))
            } else if project.is_some() {
                let project = project.clone().unwrap();
                let xdg = lookup_non_empty(lookup, "XDG_STATE_HOME");
                let home = lookup_non_empty(lookup, "HOME");
                Some(paths::log_dir(&project, xdg.as_deref(), home.as_deref())?)
            } else {
                None
            };

            // Status uses the root for hashing only when watch files are
            // non-empty.
            let root = if !watch_files.is_empty() {
                let root_str = demand(lookup, cmd, "NCAP_PROJECT_ROOT")?;
                Some(PathBuf::from(root_str))
            } else {
                root_opt
            };

            let run_opts = parse_run_opts(lookup)?;
            let harden = parse_harden(lookup);
            Ok(Config {
                cmd,
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
                image: None,
                server: None,
                nix: None,
                bash: None,
                devshell: None,
            })
        }
    }
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

    fn resolve_with(cmd: Cmd, pairs: &[(&str, &str)]) -> Result<Config, Error> {
        let map = lookup_of(pairs);
        resolve(cmd, &|var| map.get(var).cloned())
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
        let cfg = resolve_with(Cmd::Init, &pairs).expect("resolve");
        assert_eq!(cfg.container, "ncap-myproj");
        assert_eq!(cfg.timeout, 10);
    }

    #[test]
    fn derived_project_from_root_basename() {
        let mut pairs = full_init_env();
        pairs.retain(|(key, _)| *key != "NCAP_PROJECT" && *key != "NCAP_CONTAINER");
        // NCAP_PROJECT absent, NCAP_CONTAINER absent → derive both
        let cfg = resolve_with(Cmd::Init, &pairs).expect("resolve");
        assert_eq!(cfg.project.as_deref(), Some("myproj"));
        assert_eq!(cfg.container, "ncap-myproj");
    }

    #[test]
    fn explicit_project_overrides_derivation() {
        let mut pairs = full_init_env();
        pairs.push(("NCAP_PROJECT", "custom"));
        pairs.retain(|(key, _)| *key != "NCAP_CONTAINER");
        let cfg = resolve_with(Cmd::Init, &pairs).expect("resolve");
        assert_eq!(cfg.project.as_deref(), Some("custom"));
        assert_eq!(cfg.container, "ncap-custom");
    }

    #[test]
    fn explicit_container_overrides_derivation() {
        let pairs = full_init_env();
        let cfg = resolve_with(Cmd::Init, &pairs).expect("resolve");
        assert_eq!(cfg.container, "ncap-myproj");
    }

    #[test]
    fn socket_derived_via_xdg_runtime_dir() {
        let mut pairs = full_init_env();
        pairs.retain(|(key, _)| *key != "NCAP_SOCKET");
        let cfg = resolve_with(Cmd::Init, &pairs).expect("resolve");
        assert_eq!(
            cfg.socket.as_deref(),
            Some(Path::new("/run/user/1000/nix-capsule/myproj/ncap.sock"))
        );
    }

    #[test]
    fn socket_falls_back_to_tmpdir_with_uid() {
        let mut pairs = full_init_env();
        pairs.retain(|(key, _)| *key != "NCAP_SOCKET" && *key != "XDG_RUNTIME_DIR");
        pairs.push(("TMPDIR", "/tmp/foo"));
        let cfg = resolve_with(Cmd::Init, &pairs).expect("resolve");
        let uid = unsafe { libc::getuid() };
        let expected =
            PathBuf::from(format!("/tmp/foo/nix-capsule-{uid}/nix-capsule/myproj/ncap.sock"));
        assert_eq!(cfg.socket.as_deref(), Some(expected.as_path()));
    }

    #[test]
    fn cache_dir_falls_back_to_home() {
        let mut pairs = full_init_env();
        pairs.retain(|(key, _)| *key != "NCAP_CACHE_DIR" && *key != "XDG_CACHE_HOME");
        let cfg = resolve_with(Cmd::Init, &pairs).expect("resolve");
        assert_eq!(
            cfg.cache_dir.as_deref(),
            Some(Path::new("/home/user/.cache/nix-capsule/myproj"))
        );
    }

    #[test]
    fn runtime_defaults_to_podman() {
        let mut pairs = full_init_env();
        pairs.retain(|(key, _)| *key != "NCAP_RUNTIME");
        let cfg = resolve_with(Cmd::Init, &pairs).expect("resolve");
        assert_eq!(cfg.runtime, "podman");
    }

    #[test]
    fn timeout_defaults_to_ten() {
        let mut pairs = full_init_env();
        pairs.retain(|(key, _)| *key != "NCAP_TIMEOUT");
        let cfg = resolve_with(Cmd::Init, &pairs).expect("resolve");
        assert_eq!(cfg.timeout, 10);
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
        let err = resolve_with(Cmd::Init, &pairs).expect_err("must error");
        assert!(err.to_string().contains("set `project`"), "err={err}");
    }

    #[test]
    fn init_demands_ncap_project_root() {
        let mut pairs = full_init_env();
        pairs.retain(|(key, _)| *key != "NCAP_PROJECT_ROOT");
        let err = resolve_with(Cmd::Init, &pairs).expect_err("must error");
        assert!(err.to_string().contains("NCAP_PROJECT_ROOT"), "err={err}");
    }

    #[test]
    fn init_demands_ncap_image() {
        let mut pairs = full_init_env();
        pairs.retain(|(key, _)| *key != "NCAP_IMAGE");
        let err = resolve_with(Cmd::Init, &pairs).expect_err("must error");
        assert!(err.to_string().contains("NCAP_IMAGE"), "err={err}");
    }

    #[test]
    fn init_demands_ncap_server() {
        let mut pairs = full_init_env();
        pairs.retain(|(key, _)| *key != "NCAP_SERVER");
        let err = resolve_with(Cmd::Init, &pairs).expect_err("must error");
        assert!(err.to_string().contains("NCAP_SERVER"), "err={err}");
    }

    #[test]
    fn init_demands_ncap_nix() {
        let mut pairs = full_init_env();
        pairs.retain(|(key, _)| *key != "NCAP_NIX");
        let err = resolve_with(Cmd::Init, &pairs).expect_err("must error");
        assert!(err.to_string().contains("NCAP_NIX"), "err={err}");
    }

    #[test]
    fn init_demands_ncap_bash() {
        let mut pairs = full_init_env();
        pairs.retain(|(key, _)| *key != "NCAP_BASH");
        let err = resolve_with(Cmd::Init, &pairs).expect_err("must error");
        assert!(err.to_string().contains("NCAP_BASH"), "err={err}");
    }

    #[test]
    fn init_demands_ncap_devshell() {
        let mut pairs = full_init_env();
        pairs.retain(|(key, _)| *key != "NCAP_DEVSHELL");
        let err = resolve_with(Cmd::Init, &pairs).expect_err("must error");
        assert!(err.to_string().contains("NCAP_DEVSHELL"), "err={err}");
    }

    #[test]
    fn start_does_not_demand_nix_or_devshell() {
        let mut pairs = full_init_env();
        pairs.retain(|(key, _)| *key != "NCAP_NIX" && *key != "NCAP_DEVSHELL");
        resolve_with(Cmd::Start, &pairs).expect("start without nix/devshell");
    }

    #[test]
    fn stop_with_only_container_succeeds() {
        let cfg = resolve_with(Cmd::Stop, &[("NCAP_CONTAINER", "ncap-foo")]).expect("resolve");
        assert_eq!(cfg.container, "ncap-foo");
    }

    #[test]
    fn stop_without_anything_demands_project_root() {
        let err = resolve_with(Cmd::Stop, &[]).expect_err("must error");
        assert!(err.to_string().contains("NCAP_PROJECT_ROOT"), "err={err}");
    }

    #[test]
    fn stop_with_project_derives_container_without_root() {
        let cfg = resolve_with(Cmd::Stop, &[("NCAP_PROJECT", "myproj")]).expect("resolve");
        assert_eq!(cfg.container, "ncap-myproj");
    }

    #[test]
    fn status_without_watch_files_and_full_paths_needs_no_root() {
        let cfg = resolve_with(
            Cmd::Status,
            &[
                ("NCAP_CONTAINER", "ncap-foo"),
                ("NCAP_SOCKET", "/tmp/sock"),
                ("NCAP_CACHE_DIR", "/tmp/cache"),
                ("NCAP_WATCH_FILES", "[]"),
            ],
        )
        .expect("resolve");
        assert_eq!(cfg.container, "ncap-foo");
    }

    #[test]
    fn malformed_watch_files_is_an_error() {
        let mut pairs = full_init_env();
        for (key, value) in &mut pairs {
            if *key == "NCAP_WATCH_FILES" {
                *value = "not json";
            }
        }
        let err = resolve_with(Cmd::Init, &pairs).expect_err("must error");
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
        let err = resolve_with(Cmd::Init, &pairs).expect_err("must error");
        assert!(err.to_string().contains("NCAP_TIMEOUT"), "err={err}");
    }
}
