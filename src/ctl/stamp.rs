//! Binds a project name to exactly one project root.

use std::fs;
use std::io;
use std::path::Path;

use super::fs_error::FsError;
use super::paths::project_stamp_file;

#[derive(Debug, thiserror::Error)]
pub(crate) enum StampError {
    #[error("project name `{project}` is already keyed to root `{existing}`")]
    AlreadyClaimed { project: String, existing: String },
    #[error(transparent)]
    Fs(#[from] FsError),
}

/// Absent means "first claim" and is written; a different root is a hard
/// error, the same root passes silently.
pub(super) fn guard(
    cache_dir: &Path,
    project: &str,
    current_root: &Path,
) -> Result<(), StampError> {
    let stamp = project_stamp_file(cache_dir);
    match fs::read_to_string(&stamp) {
        Ok(existing) => {
            let existing = existing.trim_end_matches(['\n', '\r']);
            if existing != current_root.to_string_lossy().as_ref() {
                return Err(StampError::AlreadyClaimed {
                    project: project.to_owned(),
                    existing: existing.to_owned(),
                });
            }
            Ok(())
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            if let Some(parent) = stamp.parent() {
                fs::create_dir_all(parent).map_err(|source| FsError::CreateDir {
                    dir: parent.display().to_string(),
                    source,
                })?;
            }
            fs::write(&stamp, current_root.to_string_lossy().as_ref()).map_err(|source| {
                FsError::Write {
                    path: stamp.display().to_string(),
                    source,
                }
            })?;
            Ok(())
        }
        Err(err) => Err(FsError::Read {
            path: stamp.display().to_string(),
            source: err,
        }
        .into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_stamp_is_written() {
        let cache = tempfile::tempdir().expect("tempdir");
        let root = Path::new("/tmp/my-root");
        guard(cache.path(), "proj", root).expect("guard");
        let stored = fs::read_to_string(project_stamp_file(cache.path())).expect("stamp file");
        assert_eq!(stored, "/tmp/my-root");
    }

    #[test]
    fn accepts_same_root_on_second_guard() {
        let cache = tempfile::tempdir().expect("tempdir");
        let root = Path::new("/tmp/my-root");
        guard(cache.path(), "proj", root).expect("first guard");
        guard(cache.path(), "proj", root).expect("second guard with same root");
    }

    #[test]
    fn different_root_is_a_hard_error() {
        let cache = tempfile::tempdir().expect("tempdir");
        guard(cache.path(), "proj", Path::new("/tmp/root-a")).expect("first guard");
        let err = guard(cache.path(), "proj", Path::new("/tmp/root-b")).expect_err("must error");
        assert!(
            matches!(
                err,
                StampError::AlreadyClaimed { ref project, ref existing } if project == "proj" && existing == "/tmp/root-a"
            ),
            "err={err:?}"
        );
    }

    #[test]
    fn a_stamp_with_trailing_newlines_matches_the_bare_root() {
        let cache = tempfile::tempdir().expect("tempdir");
        fs::write(project_stamp_file(cache.path()), "/tmp/my-root\r\n").expect("stamp file");
        guard(cache.path(), "proj", Path::new("/tmp/my-root"))
            .expect("a trimmed stamp must match the same root");
    }

    #[test]
    fn absent_cache_dir_is_created_before_writing() {
        let cache = tempfile::tempdir().expect("tempdir");
        let nested = cache.path().join("cache");
        guard(&nested, "proj", Path::new("/tmp/my-root")).expect("guard succeeds");
        let stored = fs::read_to_string(project_stamp_file(&nested)).expect("stamp file written");
        assert_eq!(stored, "/tmp/my-root");
    }

    #[test]
    fn a_read_failure_other_than_absence_is_an_fs_error() {
        let cache = tempfile::tempdir().expect("tempdir");
        fs::create_dir(project_stamp_file(cache.path())).expect("directory at the stamp path");
        let err = guard(cache.path(), "proj", Path::new("/tmp/my-root")).expect_err("must error");
        assert!(
            matches!(err, StampError::Fs(FsError::Read { .. })),
            "a read failure that is not `NotFound` must surface as `StampError::Fs` carrying the read, got {err:?}"
        );
    }
}
