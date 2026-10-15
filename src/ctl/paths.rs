//! XDG layout with the `$TMPDIR` fallback for the runtime dir.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// Error deriving a cache or log dir when neither the XDG var nor `HOME` is set.
#[derive(Debug, thiserror::Error)]
#[error("cannot derive the {what}: neither `{var}` nor `HOME` is set")]
pub(crate) struct NoHomeError {
    what: &'static str,
    var: &'static str,
}

/// The per-project runtime dir that holds the socket: `$XDG_RUNTIME_DIR`
/// (absolute, per the XDG spec), else `$TMPDIR` — defaulting to `/tmp` —
/// per the flat fallback of ADR-0001. `dirs` knows nothing of `$TMPDIR`,
/// so the fallback stays here.
pub(crate) fn socket_path(project: &str) -> PathBuf {
    let runtime_dir = dirs::runtime_dir();
    let tmpdir = std::env::var_os("TMPDIR");
    let base = match (
        runtime_dir.as_deref(),
        tmpdir.as_deref().filter(|value| !value.is_empty()),
    ) {
        (Some(dir), _) => dir.join("nix-capsule").join(project),
        (None, Some(tmpdir)) => Path::new(tmpdir)
            .to_path_buf()
            .join("nix-capsule")
            .join(project),
        (None, None) => Path::new("/tmp")
            .to_path_buf()
            .join("nix-capsule")
            .join(project),
    };
    base.join("ncap.sock")
}

/// The per-project cache dir: `$XDG_CACHE_HOME`, else `$HOME/.cache`.
pub(super) fn cache_dir(project: &str) -> Result<PathBuf, NoHomeError> {
    dirs::cache_dir()
        .map(|dir| dir.join("nix-capsule").join(project))
        .ok_or(NoHomeError {
            what: "cache dir",
            var: "XDG_CACHE_HOME",
        })
}

/// The per-project log dir: `$XDG_STATE_HOME`, else `$HOME/.local/state`.
pub(super) fn log_dir(project: &str) -> Result<PathBuf, NoHomeError> {
    dirs::state_dir()
        .map(|dir| dir.join("nix-capsule").join(project).join("logs"))
        .ok_or(NoHomeError {
            what: "log dir",
            var: "XDG_STATE_HOME",
        })
}

/// The cached env dump the freshness state gates.
pub(super) fn env_file(cache_dir: &Path) -> PathBuf {
    cache_dir.join("env")
}

/// The cached freshness digest, lowercase hex with no trailing newline.
pub(super) fn hash_file(cache_dir: &Path) -> PathBuf {
    cache_dir.join("hash")
}

/// The nix profile `print-dev-env` writes; history pruned after each eval.
pub(super) fn profile_file(cache_dir: &Path) -> PathBuf {
    cache_dir.join("profile")
}

/// The stamp file binding a project name to exactly one project root.
pub(super) fn project_stamp_file(cache_dir: &Path) -> PathBuf {
    cache_dir.join("project")
}

/// Filename stem shared by [`server_log_path`] and [`parse_server_log_epoch`].
const SERVER_LOG_PREFIX: &str = "ncap-server-";
const SERVER_LOG_SUFFIX: &str = ".log";

/// The per-run server log file `<log-dir>/ncap-server-<epoch-millis>.log`.
/// Millisecond epochs keep runs started in the same second apart.
pub(crate) fn server_log_path(log_dir: &Path, epoch_millis: u128) -> PathBuf {
    log_dir.join(format!(
        "{SERVER_LOG_PREFIX}{epoch_millis}{SERVER_LOG_SUFFIX}"
    ))
}

/// Parse the epoch stamp out of a server log filename: `None` for anything
/// that is not `ncap-server-<digits>.log` — digits only, so a leading `+`
/// (which `u64::from_str` would accept) does not pass.
pub(super) fn parse_server_log_epoch(name: &str) -> Option<u64> {
    let rest = name.strip_prefix(SERVER_LOG_PREFIX)?;
    let epoch = rest.strip_suffix(SERVER_LOG_SUFFIX)?;
    if epoch.is_empty() || !epoch.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    epoch.parse().ok()
}

/// One Server log file found by the dir scan: its epoch stamp and path.
/// The pair travels together through every selection and the follow
/// loop, so the position a stream holds never lands on another file.
#[derive(Clone, Debug)]
pub(super) struct ServerLog {
    pub(super) epoch: u64,
    pub(super) path: PathBuf,
}

/// The log dir scanned (spec/paths.md § Server log files): every server
/// log file, sorted by ascending epoch — numerically, never lexically.
/// Foreign entries and a missing dir read as no log files. The
/// "highest-epoch wins" newest selection is this scan's last entry.
pub(super) fn sorted_server_logs(log_dir: &Path) -> Vec<ServerLog> {
    let Ok(dir) = fs::read_dir(log_dir) else {
        return Vec::new();
    };
    let mut entries = Vec::new();
    for entry in dir.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(epoch) = parse_server_log_epoch(&name) {
            entries.push(ServerLog {
                epoch,
                path: entry.path(),
            });
        }
    }
    entries.sort_by_key(|log| log.epoch);
    entries
}

/// Ensure `dir` exists, creating it with mode 0700 when it is newly created.
/// An existing directory is left untouched.
pub(super) fn ensure_dir_0700(dir: &Path) -> std::io::Result<()> {
    if dir.exists() {
        return Ok(());
    }
    fs::create_dir_all(dir)?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use serial_test::serial;
    use test_case::test_case;

    use crate::ctl::test_helper::EnvGuard;

    use super::*;

    #[test_case("ncap-server-1729512345678.log" => matches Some(1729512345678) ; "plain_epoch_parses")]
    #[test_case("ncap-server-007.log" => matches Some(7) ; "leading_zero_digits_stay_digits")]
    #[test_case("ncap-server-+5.log" => matches None ; "leading_plus_is_not_digits")]
    #[test_case("ncap-server-.log" => matches None ; "empty_body_is_not_digits")]
    #[test_case("ncap-server-12x.log" => matches None ; "trailing_letter_is_not_digits")]
    #[test_case("ncap-server-99999999999999999999999.log" => matches None ; "overflow_body_is_not_an_epoch")]
    #[test_case("other.log" => matches None ; "wrong_prefix_is_not_a_server_log")]
    #[test_case("ncap-server-1.log.bak" => matches None ; "wrong_suffix_is_not_a_server_log")]
    fn parses_exactly_digit_epochs(name: &str) -> Option<u64> {
        parse_server_log_epoch(name)
    }

    #[test_case(&["ncap-server-500.log", "ncap-server-70.log", "ncap-server-5.log"] => matches Ok(v) if v == vec![5u64, 70, 500] ; "numerically_not_lexically_sorted_ascending")]
    #[test_case(&["ncap-server-999.log", "ncap-server-100.log"] => matches Ok(v) if v == vec![100u64, 999] ; "two_epochs_sort_ascending")]
    #[test_case(&["ncap-server-7.log"] => matches Ok(v) if v == vec![7u64] ; "one_epoch_sorts_as_itself")]
    #[test_case(&["not-a-server-log.txt", "ncap-server-3.log.bak", "sub"] => matches Ok(v) if v.is_empty() ; "foreign_entries_are_ignored")]
    #[test_case(&[] => matches Ok(v) if v.is_empty() ; "no_log_files_is_empty")]
    fn scans_log_files_epoch_ascending_ignoring_foreigners(
        seeded: &[&str],
    ) -> Result<Vec<u64>, Box<dyn std::error::Error>> {
        scanned_epochs(seeded)
    }

    /// Seed each filename into a fresh log dir (bodies never matter) and
    /// return the epochs `sorted_server_logs` lists ascending.
    fn scanned_epochs(seeded: &[&str]) -> Result<Vec<u64>, Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        for name in seeded {
            fs::write(dir.path().join(name), "line")?;
        }
        Ok(sorted_server_logs(dir.path())
            .into_iter()
            .map(|log| log.epoch)
            .collect())
    }

    #[test]
    fn scan_of_a_missing_dir_is_empty_not_an_error() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let entries = sorted_server_logs(&dir.path().join("absent"));
        assert!(
            entries.is_empty(),
            "a missing log dir reads as no log files: {entries:?}"
        );
        Ok(())
    }

    #[test]
    fn a_non_utf8_filename_is_not_counted_as_a_server_log() -> Result<(), Box<dyn std::error::Error>>
    {
        use std::os::unix::ffi::OsStrExt;
        let dir = tempfile::tempdir()?;
        let name = OsStr::from_bytes(b"ncap-server-\xff\xfe.log");
        fs::write(dir.path().join(name), "line")?;
        let logs = sorted_server_logs(dir.path());
        assert!(
            logs.is_empty(),
            "lossy-decoded invalid bytes are not digit epochs, scanned: {logs:?}"
        );
        Ok(())
    }

    #[test]
    fn creates_nested_dir_with_mode_0700() -> Result<(), Box<dyn std::error::Error>> {
        let outer = tempfile::tempdir()?;
        let nested = outer.path().join("a").join("b");
        ensure_dir_0700(&nested)?;
        let mode = fs::metadata(&nested)?.permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "created dir mode: {mode:o}");
        Ok(())
    }

    #[test]
    fn existing_dir_keeps_its_permissions() -> Result<(), Box<dyn std::error::Error>> {
        let outer = tempfile::tempdir()?;
        let existing = outer.path().join("existing");
        fs::create_dir(&existing)?;
        fs::set_permissions(&existing, fs::Permissions::from_mode(0o755))?;

        ensure_dir_0700(&existing)?;

        let mode = fs::metadata(&existing)?.permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o755,
            "existing dir mode after ensure: {mode:o}"
        );
        Ok(())
    }

    #[test]
    fn log_paths_point_at_the_seeded_files() -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        for name in [
            "ncap-server-70.log",
            "ncap-server-500.log",
            "ncap-server-5.log",
        ] {
            fs::write(dir.path().join(name), "line")?;
        }

        let got: Vec<(u64, PathBuf)> = sorted_server_logs(dir.path())
            .into_iter()
            .map(|log| (log.epoch, log.path))
            .collect();
        let expected: Vec<(u64, PathBuf)> = [5, 70, 500]
            .map(|epoch| (epoch, dir.path().join(format!("ncap-server-{epoch}.log"))))
            .into_iter()
            .collect();
        assert_eq!(got, expected, "scanned logs: {got:?}");
        Ok(())
    }

    #[test]
    fn server_log_path_round_trips_through_the_parser() {
        let dir = Path::new("/state/logs");
        for epoch in [1u128, 1729512345678] {
            let path = server_log_path(dir, epoch);
            assert_eq!(path, dir.join(format!("ncap-server-{epoch}.log")));
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .expect("file name is plain ascii");
            assert_eq!(
                parse_server_log_epoch(name),
                Some(epoch.try_into().expect("epoch fits u64")),
                "epoch {epoch} round-trips: {name}"
            );
        }
    }

    #[test]
    #[serial]
    fn socket_prefers_the_xdg_runtime_dir() {
        let _xdg = EnvGuard::set("XDG_RUNTIME_DIR", "/run/user/1000");
        for tmpdir in ["", "/var/tmp"] {
            let _tmp = EnvGuard::set("TMPDIR", tmpdir);
            assert_eq!(
                socket_path("demo"),
                PathBuf::from("/run/user/1000/nix-capsule/demo/ncap.sock"),
                "TMPDIR {tmpdir:?}"
            );
        }
    }

    #[test]
    #[serial]
    fn socket_falls_back_to_tmpdir() {
        let _xdg = EnvGuard::unset("XDG_RUNTIME_DIR");
        let _tmp = EnvGuard::set("TMPDIR", "/var/tmp");
        assert_eq!(
            socket_path("demo"),
            PathBuf::from("/var/tmp/nix-capsule/demo/ncap.sock")
        );
    }

    #[test]
    #[serial]
    fn socket_treats_an_empty_tmpdir_as_unset() {
        let _xdg = EnvGuard::unset("XDG_RUNTIME_DIR");
        let _tmp = EnvGuard::set("TMPDIR", "");
        assert_eq!(
            socket_path("demo"),
            PathBuf::from("/tmp/nix-capsule/demo/ncap.sock")
        );
    }

    #[test]
    #[serial]
    fn socket_defaults_to_tmp_when_nothing_is_set() {
        let _xdg = EnvGuard::unset("XDG_RUNTIME_DIR");
        let _tmp = EnvGuard::unset("TMPDIR");
        assert_eq!(
            socket_path("demo"),
            PathBuf::from("/tmp/nix-capsule/demo/ncap.sock")
        );
    }

    #[test]
    #[serial]
    fn cache_dir_honors_xdg_cache_home_then_home() -> Result<(), Box<dyn std::error::Error>> {
        let home = tempfile::tempdir()?;
        let cache = home.path().join("cache");

        {
            let _xdg = EnvGuard::set("XDG_CACHE_HOME", cache.as_os_str());
            assert_eq!(cache_dir("demo")?, cache.join("nix-capsule").join("demo"));
        }

        // `dirs` computes the `$HOME/.cache` fallback internally, so what a
        // test can pin through env is only that the fallback is passed
        // through joined with the project name.
        {
            let _xdg = EnvGuard::unset("XDG_CACHE_HOME");
            let _home = EnvGuard::set("HOME", home.path());
            assert_eq!(
                cache_dir("demo")?,
                home.path().join(".cache").join("nix-capsule").join("demo")
            );
        }
        Ok(())
    }

    #[test]
    #[serial]
    fn log_dir_honors_xdg_state_home_then_home() -> Result<(), Box<dyn std::error::Error>> {
        let home = tempfile::tempdir()?;
        let state = home.path().join("state");

        {
            let _xdg = EnvGuard::set("XDG_STATE_HOME", state.as_os_str());
            assert_eq!(
                log_dir("demo")?,
                state.join("nix-capsule").join("demo").join("logs")
            );
        }

        {
            let _xdg = EnvGuard::unset("XDG_STATE_HOME");
            let _home = EnvGuard::set("HOME", home.path());
            assert_eq!(
                log_dir("demo")?,
                home.path()
                    .join(".local/state")
                    .join("nix-capsule")
                    .join("demo")
                    .join("logs")
            );
        }
        Ok(())
    }
}
