//! Filesystem op error context shared by ctl flows and the Server: the failed
//! operation and its object ride as data, the raw cause as `#[source]`.
//! Socket and signal-handler io errors are not filesystem operations and
//! carry their own context in `server::ServerError` instead.

use std::io;

/// Filesystem operation failures with the failed path riding as data.
#[derive(Debug, thiserror::Error)]
pub enum FsError {
    #[error("cannot create `{dir}`: {source}")]
    CreateDir {
        dir: String,
        #[source]
        source: io::Error,
    },
    #[error("cannot open `{path}`: {source}")]
    Open {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("cannot read `{path}`: {source}")]
    Read {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("cannot write `{path}`: {source}")]
    Write {
        path: String,
        #[source]
        source: io::Error,
    },
    #[error("cannot read `{dir}`: {source}")]
    ReadDir {
        dir: String,
        #[source]
        source: io::Error,
    },
    #[error("cannot remove `{path}`: {source}")]
    Remove {
        path: String,
        #[source]
        source: io::Error,
    },
}
