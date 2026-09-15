//! Freshness digest: xxhash64 over the watched files, cached as lowercase
//! hex so `init` can decide between "no Nix evaluation" and "re-eval".

use std::fs::{self, File};
use std::hash::{BuildHasher, BuildHasherDefault, Hasher};
use std::io::{self, Write};
use std::path::Path;

use twox_hash::XxHash64;

use super::paths::{env_file, hash_file};

/// Whether the cached env dump still matches the watched files.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Freshness {
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
/// flips the digest; a mtime-only touch does not. File contents stream into
/// the hasher, so large watched files are never fully buffered. Returned as
/// lowercase hex without a trailing newline.
pub fn compute(root: &Path, entries: &[String]) -> io::Result<String> {
    let mut sorted: Vec<&String> = entries.iter().collect();
    sorted.sort();
    let mut hasher = BuildHasherDefault::<XxHash64>::default().build_hasher();
    for entry in sorted {
        let (exists, contents) = match File::open(root.join(entry)) {
            Ok(file) => (true, Some(file)),
            Err(err) if err.kind() == io::ErrorKind::NotFound => (false, None),
            Err(err) => return Err(err),
        };
        hasher.write(entry.as_bytes());
        hasher.write(b"\0");
        hasher.write(if exists { b"1" } else { b"0" });
        hasher.write(b"\0");
        if let Some(contents) = contents {
            io::copy(&mut { contents }, &mut HashWriter(&mut hasher))?;
        }
    }
    Ok(format!("{:016x}", hasher.finish()))
}

/// Forwards every written byte into the hasher so `io::copy` can stream
/// file contents through it.
struct HashWriter<'a>(&'a mut <BuildHasherDefault<XxHash64> as BuildHasher>::Hasher);

impl Write for HashWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Compare the computed digest against the cached `<cache>/hash`: the env
/// dump missing is `Missing`, a matching hash is `Fresh`, everything else is
/// `Stale`. The cached hash is trimmed before comparing so a trailing newline
/// (spec: stored with no trailing newline) reads as fresh, matching `status`.
pub(crate) fn check(cache_dir: &Path, root: &Path, entries: &[String]) -> Freshness {
    if !env_file(cache_dir).is_file() {
        return Freshness::Missing;
    }
    let cached = match fs::read_to_string(hash_file(cache_dir)) {
        Ok(cached) => cached,
        Err(_) => return Freshness::Stale,
    };
    match compute(root, entries) {
        Ok(digest) if cached.trim() == digest => Freshness::Fresh,
        // Computed-error (e.g. permission-denied watched file) is Stale,
        // never Fresh: an empty `unwrap_or_default()` digest must not match
        // an empty cached hash.
        _ => Freshness::Stale,
    }
}

/// Write `digest` to `<cache>/hash`, lowercase hex with no trailing newline.
pub(crate) fn store(cache_dir: &Path, digest: &str) -> io::Result<()> {
    fs::write(hash_file(cache_dir), digest)
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
        let digest = compute(root.path(), &[]).expect("digest");
        assert_eq!(digest, "ef46db3751d8e999");
    }

    #[test]
    fn digest_is_independent_of_entry_order() {
        let root = tempfile::tempdir().expect("tempdir");
        write(&root.path().join("a.txt"), b"alpha");
        write(&root.path().join("b.txt"), b"beta");
        let one = compute(root.path(), &entries(&["a.txt", "b.txt"])).expect("digest");
        let two = compute(root.path(), &entries(&["b.txt", "a.txt"])).expect("digest");
        assert_eq!(one, two);
    }

    #[test]
    fn content_change_flips_digest() {
        let root = tempfile::tempdir().expect("tempdir");
        write(&root.path().join("w.txt"), b"before");
        let before = compute(root.path(), &entries(&["w.txt"])).expect("digest");
        write(&root.path().join("w.txt"), b"after");
        let after = compute(root.path(), &entries(&["w.txt"])).expect("digest");
        assert_ne!(before, after);
    }

    #[test]
    fn appearing_or_disappearing_file_flips_digest() {
        let root = tempfile::tempdir().expect("tempdir");
        let absent = compute(root.path(), &entries(&["w.txt"])).expect("digest");
        write(&root.path().join("w.txt"), b"");
        let empty_but_present = compute(root.path(), &entries(&["w.txt"])).expect("digest");
        assert_ne!(absent, empty_but_present);
    }

    #[test]
    fn mtime_only_touch_keeps_digest() {
        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path().join("w.txt");
        write(&path, b"same");
        let first = compute(root.path(), &entries(&["w.txt"])).expect("digest");
        write(&path, b"same");
        let second = compute(root.path(), &entries(&["w.txt"])).expect("digest");
        assert_eq!(first, second);
    }

    #[test]
    fn nested_entries_hash_under_their_relative_path() {
        let root = tempfile::tempdir().expect("tempdir");
        write(&root.path().join("dir/w.txt"), b"same");
        let flat = compute(root.path(), &entries(&["dir/w.txt"])).expect("digest");
        write(&root.path().join("other/w.txt"), b"same");
        let moved = compute(root.path(), &entries(&["other/w.txt"])).expect("digest");
        assert_ne!(flat, moved, "the relative path is part of the record");
    }

    #[test]
    fn digest_is_lowercase_hex_without_a_newline() {
        let root = tempfile::tempdir().expect("tempdir");
        write(&root.path().join("w.txt"), b"contents");
        let digest = compute(root.path(), &entries(&["w.txt"])).expect("digest");
        assert_eq!(digest.len(), 16, "digest={digest}");
        assert!(
            digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
            "digest={digest}"
        );
    }

    #[test]
    fn freshness_is_missing_without_an_env_dump() {
        let cache = tempfile::tempdir().expect("tempdir");
        let root = tempfile::tempdir().expect("tempdir");
        write(&root.path().join("w.txt"), b"x");
        let digest = compute(root.path(), &entries(&["w.txt"])).expect("digest");
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
        let digest = compute(root.path(), &entries(&["w.txt"])).expect("digest");
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
    fn stored_hash_has_no_trailing_newline() {
        let cache = tempfile::tempdir().expect("tempdir");
        store(cache.path(), "0123456789abcdef").expect("store");
        let raw = fs::read(hash_file(cache.path())).expect("hash file");
        assert_eq!(raw, b"0123456789abcdef");
    }

    #[test]
    fn freshness_tolerates_a_trailing_newline_in_the_cached_hash() {
        let cache = tempfile::tempdir().expect("tempdir");
        let root = tempfile::tempdir().expect("tempdir");
        write(&root.path().join("w.txt"), b"x");
        let digest = compute(root.path(), &entries(&["w.txt"])).expect("digest");
        fs::write(env_file(cache.path()), b"export FOO=bar").expect("env dump");
        fs::write(hash_file(cache.path()), format!("{digest}\n")).expect("hash with newline");
        assert_eq!(
            check(cache.path(), root.path(), &entries(&["w.txt"])),
            Freshness::Fresh,
            "a trailing newline must read as fresh, matching `status`"
        );
    }

    #[test]
    fn computed_error_reads_stale_never_fresh() {
        let cache = tempfile::tempdir().expect("tempdir");
        let root = tempfile::tempdir().expect("tempdir");
        // A directory at a watched path makes `of()` fail with a
        // non-NotFound error even as root (permission bits are ignored
        // for root, so chmod-based fixtures would be flaky here).
        fs::create_dir_all(root.path().join("w.txt")).expect("watched dir");
        assert!(compute(root.path(), &entries(&["w.txt"])).is_err());
        fs::write(env_file(cache.path()), b"export FOO=bar").expect("env dump");
        fs::write(hash_file(cache.path()), b"").expect("empty hash");
        assert_eq!(
            check(cache.path(), root.path(), &entries(&["w.txt"])),
            Freshness::Stale,
            "computed-error plus empty cached hash must not read as fresh"
        );
    }
}
