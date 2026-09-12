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
