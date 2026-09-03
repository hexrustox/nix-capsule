//! Freshness digest: xxhash64 over the watched files, cached as lowercase
//! hex so `init` can decide between "no Nix evaluation" and "re-eval".

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// Whether the cached env dump still matches the watched files.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Freshness {
    /// Hash file present and equal to the computed digest.
    Fresh,
    /// Env dump present but the hash differs or is unreadable.
    Stale,
    /// No env dump in the cache at all.
    Missing,
}

/// xxhash64 (seed 0) over one record per entry, entries sorted by relative
/// path: `(relative path, NUL, exists flag, NUL, contents-or-empty)`. Missing
/// files contribute their absence flag, so a file appearing or disappearing
/// flips the digest; a mtime-only touch does not. Returned as lowercase hex
/// without a trailing newline.
pub fn of(root: &Path, entries: &[String]) -> io::Result<String> {
    let mut sorted: Vec<&String> = entries.iter().collect();
    sorted.sort();
    let mut records = Vec::new();
    for entry in sorted {
        let path = root.join(entry);
        let (exists, contents) = match fs::read(&path) {
            Ok(contents) => (true, contents),
            Err(err) if err.kind() == io::ErrorKind::NotFound => (false, Vec::new()),
            Err(err) => return Err(err),
        };
        records.extend_from_slice(entry.as_bytes());
        records.push(b'\0');
        records.extend_from_slice(if exists { b"1" } else { b"0" });
        records.push(b'\0');
        records.extend_from_slice(&contents);
    }
    Ok(format!("{:016x}", twox_hash::XxHash64::oneshot(0, &records)))
}

/// Compare the computed digest against the cached `<cache>/hash`: the env
/// dump missing is `Missing`, a matching hash is `Fresh`, everything else is
/// `Stale`.
pub fn check(cache_dir: &Path, root: &Path, entries: &[String]) -> Freshness {
    if !cache_dir.join("env").is_file() {
        return Freshness::Missing;
    }
    match fs::read_to_string(cache_dir.join("hash")) {
        Ok(cached) if cached == of(root, entries).unwrap_or_default() => Freshness::Fresh,
        _ => Freshness::Stale,
    }
}

/// Write `digest` to `<cache>/hash`, lowercase hex with no trailing newline.
pub fn store(cache_dir: &Path, digest: &str) -> io::Result<()> {
    fs::write(cache_dir.join("hash"), digest)
}

/// The cached env dump the freshness state gates.
pub fn env_file(cache_dir: &Path) -> PathBuf {
    cache_dir.join("env")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| item.to_string()).collect()
    }

    fn write(path: &Path, contents: &[u8]) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create parent");
        }
        fs::write(path, contents).expect("write fixture");
    }

    #[test]
    fn empty_watch_list_is_the_empty_input_xxhash64_vector() {
        let root = tempfile::tempdir().expect("tempdir");
        let digest = of(root.path(), &[]).expect("digest");
        assert_eq!(digest, "ef46db3751d8e999");
    }

    #[test]
    fn digest_is_independent_of_entry_order() {
        let root = tempfile::tempdir().expect("tempdir");
        write(&root.path().join("a.txt"), b"alpha");
        write(&root.path().join("b.txt"), b"beta");
        let one = of(root.path(), &entries(&["a.txt", "b.txt"])).expect("digest");
        let two = of(root.path(), &entries(&["b.txt", "a.txt"])).expect("digest");
        assert_eq!(one, two);
    }

    #[test]
    fn a_content_change_flips_the_digest() {
        let root = tempfile::tempdir().expect("tempdir");
        write(&root.path().join("w.txt"), b"before");
        let before = of(root.path(), &entries(&["w.txt"])).expect("digest");
        write(&root.path().join("w.txt"), b"after");
        let after = of(root.path(), &entries(&["w.txt"])).expect("digest");
        assert_ne!(before, after);
    }

    #[test]
    fn a_file_appearing_or_disappearing_flips_the_digest() {
        let root = tempfile::tempdir().expect("tempdir");
        let absent = of(root.path(), &entries(&["w.txt"])).expect("digest");
        write(&root.path().join("w.txt"), b"");
        let empty_but_present = of(root.path(), &entries(&["w.txt"])).expect("digest");
        assert_ne!(absent, empty_but_present);
    }

    #[test]
    fn a_mtime_only_touch_keeps_the_digest() {
        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path().join("w.txt");
        write(&path, b"same");
        let first = of(root.path(), &entries(&["w.txt"])).expect("digest");
        write(&path, b"same");
        let second = of(root.path(), &entries(&["w.txt"])).expect("digest");
        assert_eq!(first, second);
    }

    #[test]
    fn nested_entries_hash_under_their_relative_path() {
        let root = tempfile::tempdir().expect("tempdir");
        write(&root.path().join("dir/w.txt"), b"same");
        let flat = of(root.path(), &entries(&["dir/w.txt"])).expect("digest");
        write(&root.path().join("other/w.txt"), b"same");
        let moved = of(root.path(), &entries(&["other/w.txt"])).expect("digest");
        assert_ne!(flat, moved, "the relative path is part of the record");
    }

    #[test]
    fn digest_is_lowercase_hex_without_a_newline() {
        let root = tempfile::tempdir().expect("tempdir");
        write(&root.path().join("w.txt"), b"contents");
        let digest = of(root.path(), &entries(&["w.txt"])).expect("digest");
        assert_eq!(digest.len(), 16, "digest={digest}");
        assert!(
            digest.bytes().all(|byte| byte.is_ascii_hexdigit()
                && !byte.is_ascii_uppercase()),
            "digest={digest}"
        );
    }

    #[test]
    fn freshness_is_missing_without_an_env_dump() {
        let cache = tempfile::tempdir().expect("tempdir");
        let root = tempfile::tempdir().expect("tempdir");
        write(&root.path().join("w.txt"), b"x");
        let digest = of(root.path(), &entries(&["w.txt"])).expect("digest");
        store(cache.path(), &digest).expect("store hash");
        assert_eq!(
            check(cache.path(), root.path(), &entries(&["w.txt"])),
            Freshness::Missing
        );
    }

    #[test]
    fn freshness_matches_the_cached_hash_when_the_dump_exists() {
        let cache = tempfile::tempdir().expect("tempdir");
        let root = tempfile::tempdir().expect("tempdir");
        write(&root.path().join("w.txt"), b"x");
        let digest = of(root.path(), &entries(&["w.txt"])).expect("digest");
        fs::write(env_file(cache.path()), b"export FOO=bar").expect("env dump");
        assert_eq!(
            check(cache.path(), root.path(), &entries(&["w.txt"])),
            Freshness::Stale,
            "no hash file yet"
        );
        store(cache.path(), &digest).expect("store hash");
        assert_eq!(
            check(cache.path(), root.path(), &entries(&["w.txt"])),
            Freshness::Fresh
        );
        write(&root.path().join("w.txt"), b"y");
        assert_eq!(
            check(cache.path(), root.path(), &entries(&["w.txt"])),
            Freshness::Stale
        );
    }

    #[test]
    fn the_stored_hash_has_no_trailing_newline() {
        let cache = tempfile::tempdir().expect("tempdir");
        store(cache.path(), "0123456789abcdef").expect("store");
        let raw = fs::read(cache.path().join("hash")).expect("hash file");
        assert_eq!(raw, b"0123456789abcdef");
    }
}
