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
pub fn runtime_dir(
    project: &str,
    xdg_runtime_dir: Option<&str>,
    tmpdir: Option<&str>,
    uid: u32,
) -> PathBuf {
    if let Some(dir) = xdg_runtime_dir.filter(|value| !value.is_empty()) {
        Path::new(dir).join("nix-capsule").join(project)
    } else {
        let base = tmpdir.filter(|value| !value.is_empty()).unwrap_or("/tmp");
        Path::new(base)
            .join(format!("nix-capsule-{uid}"))
            .join("nix-capsule")
            .join(project)
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
        Ok(Path::new(home).join(".cache").join("nix-capsule").join(project))
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
        Ok(Path::new(dir).join("nix-capsule").join(project).join("logs"))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn uid() -> u32 {
        unsafe { libc::getuid() }
    }

    #[test]
    fn runtime_dir_prefers_xdg_runtime_dir() {
        let dir = runtime_dir("proj", Some("/run/user/1000"), Some("/tmp"), uid());
        assert_eq!(dir, Path::new("/run/user/1000/nix-capsule/proj"));
    }

    #[test]
    fn runtime_dir_falls_back_to_tmpdir_with_uid() {
        let dir = runtime_dir("proj", None, Some("/tmp/foo"), uid());
        assert_eq!(
            dir,
            Path::new(&format!("/tmp/foo/nix-capsule-{}/nix-capsule/proj", uid()))
        );
    }

    #[test]
    fn runtime_dir_falls_back_to_slash_tmp_when_tmpdir_unset() {
        let dir = runtime_dir("proj", None, None, 42);
        assert_eq!(dir, Path::new("/tmp/nix-capsule-42/nix-capsule/proj"));
    }

    #[test]
    fn socket_path_appends_ncap_sock() {
        let dir = Path::new("/run/user/1000/nix-capsule/proj");
        assert_eq!(socket_path(dir), dir.join("ncap.sock"));
    }

    #[test]
    fn cache_dir_prefers_xdg_cache_home() {
        let dir = cache_dir("proj", Some("/cache"), Some("/home/u")).expect("cache dir");
        assert_eq!(dir, Path::new("/cache/nix-capsule/proj"));
    }

    #[test]
    fn cache_dir_falls_back_to_home_dot_cache() {
        let dir = cache_dir("proj", None, Some("/home/u")).expect("cache dir");
        assert_eq!(dir, Path::new("/home/u/.cache/nix-capsule/proj"));
    }

    #[test]
    fn cache_dir_errors_without_xdg_or_home() {
        let err = cache_dir("proj", None, None).expect_err("must error");
        assert!(err.to_string().contains("XDG_CACHE_HOME"));
    }

    #[test]
    fn log_dir_prefers_xdg_state_home() {
        let dir = log_dir("proj", Some("/state"), Some("/home/u")).expect("log dir");
        assert_eq!(dir, Path::new("/state/nix-capsule/proj/logs"));
    }

    #[test]
    fn log_dir_falls_back_to_home_dot_local_state() {
        let dir = log_dir("proj", None, Some("/home/u")).expect("log dir");
        assert_eq!(
            dir,
            Path::new("/home/u/.local/state/nix-capsule/proj/logs")
        );
    }

    #[test]
    fn log_dir_errors_without_xdg_or_home() {
        let err = log_dir("proj", None, None).expect_err("must error");
        assert!(err.to_string().contains("XDG_STATE_HOME"));
    }

    #[test]
    fn ensure_dir_0700_creates_with_0700() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("a").join("b");
        ensure_dir_0700(&dir).expect("ensure");
        let mode = fs::metadata(&dir).expect("metadata").permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[test]
    fn ensure_dir_0700_leaves_existing_perms() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path().join("existing");
        fs::create_dir_all(&dir).expect("create");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).expect("chmod");
        ensure_dir_0700(&dir).expect("ensure");
        let mode = fs::metadata(&dir).expect("metadata").permissions().mode() & 0o777;
        assert_eq!(mode, 0o755);
    }
}
