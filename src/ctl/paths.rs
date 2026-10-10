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
pub(super) fn socket_path(project: &str) -> PathBuf {
    let runtime_dir = match dirs::runtime_dir() {
        Some(dir) => dir.join("nix-capsule").join(project),
        None => {
            let base = std::env::var_os("TMPDIR")
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| std::ffi::OsString::from("/tmp"));
            Path::new(&base).join("nix-capsule").join(project)
        }
    };
    runtime_dir.join("ncap.sock")
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
    use test_case::test_case;

    use super::*;

    #[test_case("ncap-server-1729512345678.log" => matches Some(1729512345678) ; "plain_epoch_parses")]
    #[test_case("ncap-server-007.log" => matches Some(7) ; "leading_zero_digits_stay_digits")]
    #[test_case("ncap-server-+5.log" => matches None ; "leading_plus_is_not_digits")]
    #[test_case("ncap-server-.log" => matches None ; "empty_body_is_not_digits")]
    #[test_case("ncap-server-12x.log" => matches None ; "trailing_letter_is_not_digits")]
    #[test_case("other.log" => matches None ; "wrong_prefix_is_not_a_server_log")]
    #[test_case("ncap-server-1.log.bak" => matches None ; "wrong_suffix_is_not_a_server_log")]
    fn parses_exactly_digit_epochs(name: &str) -> Option<u64> {
        parse_server_log_epoch(name)
    }

    /// Seed each filename into a fresh log dir (bodies never matter) and
    /// return the epochs `sorted_server_logs` lists ascending.
    fn scanned_epochs(seeded: &[&str]) -> Vec<u64> {
        let dir = tempfile::tempdir().expect("tempdir");
        for name in seeded {
            fs::write(dir.path().join(name), "line").expect("log file");
        }
        sorted_server_logs(dir.path())
            .into_iter()
            .map(|log| log.epoch)
            .collect::<Vec<_>>()
    }

    #[test_case(&["ncap-server-500.log", "ncap-server-70.log", "ncap-server-5.log"] => vec![5u64, 70, 500] ; "numerically_not_lexically_sorted_ascending")]
    #[test_case(&["ncap-server-999.log", "ncap-server-100.log"] => vec![100u64, 999] ; "two_epochs_sort_ascending")]
    #[test_case(&["ncap-server-7.log"] => vec![7u64] ; "one_epoch_sorts_as_itself")]
    #[test_case(&["not-a-server-log.txt", "ncap-server-3.log.bak", "sub"] => vec![] as Vec<u64> ; "foreign_entries_are_ignored")]
    #[test_case(&[] => vec![] as Vec<u64> ; "no_log_files_is_empty")]
    fn scans_log_files_epoch_ascending_ignoring_foreigners(seeded: &[&str]) -> Vec<u64> {
        scanned_epochs(seeded)
    }

    #[test]
    fn scan_of_a_missing_dir_is_empty_not_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let entries = sorted_server_logs(&dir.path().join("absent"));
        assert!(
            entries.is_empty(),
            "a missing log dir reads as no log files: {entries:?}"
        );
    }
}
