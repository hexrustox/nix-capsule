//! Shared helpers for `#[serial]` env tests and pair-lookup fixtures.

use std::ffi::{OsStr, OsString};

/// Owns one env var for a `#[serial]` test: process-wide by nature, so
/// it restores (or removes) the original on drop.
pub(crate) struct EnvGuard {
    var: &'static str,
    original: Option<OsString>,
}

impl EnvGuard {
    pub(crate) fn set(var: &'static str, value: impl AsRef<OsStr>) -> Self {
        let original = std::env::var_os(var);
        unsafe { std::env::set_var(var, value) };
        Self { var, original }
    }

    pub(crate) fn unset(var: &'static str) -> Self {
        let original = std::env::var_os(var);
        unsafe { std::env::remove_var(var) };
        Self { var, original }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.original {
            Some(value) => unsafe { std::env::set_var(self.var, value) },
            None => unsafe { std::env::remove_var(self.var) },
        }
    }
}

/// A lookup over literal pairs, standing in for the process environment.
/// Absent names yield `None`, the same signal `expand_one` gets from
/// `std::env::var(..).ok()`.
pub(crate) fn lookup_of<'a>(
    pairs: &'a [(&'a str, &'a str)],
) -> impl Fn(&str) -> Option<String> + 'a {
    move |name| find_value(pairs, name).map(str::to_owned)
}

/// The `OsString` variant for Client env tests, standing in for the
/// process environment.
pub(crate) fn os_lookup_of<'a>(
    pairs: &'a [(&'a str, &'a str)],
) -> impl Fn(&str) -> Option<OsString> + 'a {
    move |name| find_value(pairs, name).map(OsString::from)
}

fn find_value<'a>(pairs: &'a [(&'a str, &'a str)], name: &str) -> Option<&'a str> {
    pairs
        .iter()
        .find(|(key, _)| *key == name)
        .map(|(_, value)| *value)
}
