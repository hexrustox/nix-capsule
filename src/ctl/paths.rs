//! XDG layout with the `$TMPDIR` fallback for the runtime dir.

use std::env;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// Error deriving a cache or log dir when neither the XDG var nor `HOME` is set.
#[derive(Debug, thiserror::Error)]
#[error("cannot derive the {what}: neither `{var}` nor `HOME` is set")]
pub struct NoHome {
    pub what: &'static str,
    pub var: &'static str,
}

/// The per-project runtime dir that holds the socket: `$XDG_RUNTIME_DIR`
/// (absolute, per the XDG spec), else `$TMPDIR` — defaulting to `/tmp` —
/// per the flat fallback of ADR-0001. `dirs` knows nothing of `$TMPDIR`,
/// so the fallback stays here.
pub fn socket_path(project: &str) -> PathBuf {
    let runtime_dir = match dirs::runtime_dir() {
        Some(dir) => dir.join("nix-capsule").join(project),
        None => {
            let base = env::var_os("TMPDIR")
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| OsString::from("/tmp"));
            Path::new(&base).join("nix-capsule").join(project)
        }
    };
    runtime_dir.join("ncap.sock")
}

/// The per-project cache dir: `$XDG_CACHE_HOME`, else `$HOME/.cache`.
pub fn cache_dir(project: &str) -> Result<PathBuf, NoHome> {
    dirs::cache_dir()
        .map(|dir| dir.join("nix-capsule").join(project))
        .ok_or(NoHome {
            what: "cache dir",
            var: "XDG_CACHE_HOME",
        })
}

/// The per-project log dir: `$XDG_STATE_HOME`, else `$HOME/.local/state`.
pub fn log_dir(project: &str) -> Result<PathBuf, NoHome> {
    dirs::state_dir()
        .map(|dir| dir.join("nix-capsule").join(project).join("logs"))
        .ok_or(NoHome {
            what: "log dir",
            var: "XDG_STATE_HOME",
        })
}

/// The cached env dump the freshness state gates.
pub fn env_file(cache_dir: &Path) -> PathBuf {
    cache_dir.join("env")
}

/// The cached freshness digest, lowercase hex with no trailing newline.
pub fn hash_file(cache_dir: &Path) -> PathBuf {
    cache_dir.join("hash")
}

/// The nix profile `print-dev-env` writes; history pruned after each eval.
pub fn profile_file(cache_dir: &Path) -> PathBuf {
    cache_dir.join("profile")
}

/// The stamp file binding a project name to exactly one project root.
pub fn project_stamp_file(cache_dir: &Path) -> PathBuf {
    cache_dir.join("project")
}

/// Filename stem shared by [`server_log_path`] and [`parse_server_log_epoch`].
const SERVER_LOG_PREFIX: &str = "ncap-server-";
const SERVER_LOG_SUFFIX: &str = ".log";

/// The per-run server log file `<log-dir>/ncap-server-<epoch-millis>.log`.
/// Millisecond epochs keep runs started in the same second apart.
pub fn server_log_path(log_dir: &Path, epoch_millis: u128) -> PathBuf {
    log_dir.join(format!(
        "{SERVER_LOG_PREFIX}{epoch_millis}{SERVER_LOG_SUFFIX}"
    ))
}

/// Parse the epoch stamp out of a server log filename: `None` for anything
/// that is not `ncap-server-<digits>.log`.
pub fn parse_server_log_epoch(name: &str) -> Option<u64> {
    let rest = name.strip_prefix(SERVER_LOG_PREFIX)?;
    let epoch = rest.strip_suffix(SERVER_LOG_SUFFIX)?;
    epoch.parse().ok()
}

/// The newest server log in `log_dir` by epoch stamp; `None` when there is
/// no log file in the dir.
pub fn newest_server_log_path(log_dir: &Path) -> Option<PathBuf> {
    let dir = fs::read_dir(log_dir).ok()?;
    let mut entries: Vec<(u64, PathBuf)> = Vec::new();
    for entry in dir.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(epoch) = parse_server_log_epoch(&name) {
            entries.push((epoch, entry.path()));
        }
    }
    entries.sort_by_key(|(epoch, _)| *epoch);
    entries.pop().map(|(_, path)| path)
}

/// Ensure `dir` exists, creating it with mode 0700 when it is newly created.
/// An existing directory is left untouched.
pub fn ensure_dir_0700(dir: &Path) -> io::Result<()> {
    if dir.exists() {
        return Ok(());
    }
    fs::create_dir_all(dir)?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    Ok(())
}
