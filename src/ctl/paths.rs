//! XDG layout with the `$TMPDIR` fallback for the runtime dir.

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

/// The per-project runtime dir that holds the socket.
pub fn runtime_dir(project: &str, xdg_runtime_dir: Option<&str>, tmpdir: Option<&str>) -> PathBuf {
    if let Some(dir) = xdg_runtime_dir.filter(|value| !value.is_empty()) {
        Path::new(dir).join("nix-capsule").join(project)
    } else {
        let base = tmpdir.filter(|value| !value.is_empty()).unwrap_or("/tmp");
        Path::new(base).join("nix-capsule").join(project)
    }
}

/// The socket file inside `runtime_dir`.
pub fn socket_path(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join("ncap.sock")
}

/// The per-project cache dir.
pub fn cache_dir(
    project: &str,
    xdg_cache_home: Option<&str>,
    home: Option<&str>,
) -> Result<PathBuf, NoHome> {
    if let Some(dir) = xdg_cache_home.filter(|value| !value.is_empty()) {
        Ok(Path::new(dir).join("nix-capsule").join(project))
    } else if let Some(home) = home.filter(|value| !value.is_empty()) {
        Ok(Path::new(home)
            .join(".cache")
            .join("nix-capsule")
            .join(project))
    } else {
        Err(NoHome {
            what: "cache dir",
            var: "XDG_CACHE_HOME",
        })
    }
}

/// The per-project log dir.
pub fn log_dir(
    project: &str,
    xdg_state_home: Option<&str>,
    home: Option<&str>,
) -> Result<PathBuf, NoHome> {
    if let Some(dir) = xdg_state_home.filter(|value| !value.is_empty()) {
        Ok(Path::new(dir)
            .join("nix-capsule")
            .join(project)
            .join("logs"))
    } else if let Some(home) = home.filter(|value| !value.is_empty()) {
        Ok(Path::new(home)
            .join(".local")
            .join("state")
            .join("nix-capsule")
            .join(project)
            .join("logs"))
    } else {
        Err(NoHome {
            what: "log dir",
            var: "XDG_STATE_HOME",
        })
    }
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
