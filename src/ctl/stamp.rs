//! Stamp guard: binds a project name to exactly one project root.

use std::fs;
use std::io;
use std::path::Path;

/// Read `<cache>/project`; absent means "first claim" and is written, a
/// different root is a hard error with the "set `project`" hint, the same
/// root passes silently.
pub fn guard(cache_dir: &Path, project: &str, current_root: &Path) -> io::Result<()> {
    let stamp = cache_dir.join("project");
    match fs::read_to_string(&stamp) {
        Ok(existing) => {
            let existing = existing.trim_end_matches(['\n', '\r']);
            if existing != current_root.to_string_lossy().as_ref() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!(
                        "project name `{project}` is already keyed to root `{existing}`; set `project`"
                    ),
                ));
            }
            Ok(())
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            if let Some(parent) = stamp.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&stamp, current_root.to_string_lossy().as_ref())?;
            Ok(())
        }
        Err(err) => Err(err),
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
        let stored = fs::read_to_string(cache.path().join("project")).expect("stamp file");
        assert_eq!(stored, "/tmp/my-root");
    }

    #[test]
    fn same_root_passes() {
        let cache = tempfile::tempdir().expect("tempdir");
        let root = Path::new("/tmp/my-root");
        guard(cache.path(), "proj", root).expect("first guard");
        guard(cache.path(), "proj", root).expect("second guard with same root");
    }

    #[test]
    fn different_root_is_a_hard_error_with_hint() {
        let cache = tempfile::tempdir().expect("tempdir");
        guard(cache.path(), "proj", Path::new("/tmp/root-a")).expect("first guard");
        let err = guard(cache.path(), "proj", Path::new("/tmp/root-b")).expect_err("must error");
        let msg = err.to_string();
        assert!(msg.contains("proj"), "msg={msg}");
        assert!(msg.contains("/tmp/root-a"), "msg={msg}");
        assert!(msg.contains("set `project`"), "msg={msg}");
    }
}
