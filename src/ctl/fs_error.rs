//! Shared io error context: one variant per filesystem operation, naming the
//! failed operation and its object with the raw cause riding as `#[source]`.
//! Constructed by any module that touches the filesystem — ctl flows and the
//! Server alike (socket and signal-handler io errors carry their own context
//! in `server::ServerError`; these are not filesystem operations).

use std::io;

#[derive(Debug, thiserror::Error)]
/// Filesystem operation failures with the failed path riding as data.
pub enum FsError {
    /// Creating a directory failed.
    #[error("cannot create `{dir}`: {source}")]
    CreateDir {
        /// The directory that failed creation.
        dir: String,
        /// The underlying creation failure.
        #[source]
        source: io::Error,
    },
    /// Opening a file failed.
    #[error("cannot open `{path}`: {source}")]
    Open {
        /// The path that failed to open.
        path: String,
        /// The underlying open failure.
        #[source]
        source: io::Error,
    },
    /// Reading a file failed.
    #[error("cannot read `{path}`: {source}")]
    Read {
        /// The path that failed to read.
        path: String,
        /// The underlying read failure.
        #[source]
        source: io::Error,
    },
    /// Writing a file failed.
    #[error("cannot write `{path}`: {source}")]
    Write {
        /// The path that failed to write.
        path: String,
        /// The underlying write failure.
        #[source]
        source: io::Error,
    },
    /// Listing a directory failed.
    #[error("cannot read `{dir}`: {source}")]
    ReadDir {
        /// The directory that failed to list.
        dir: String,
        /// The underlying listing failure.
        #[source]
        source: io::Error,
    },
    /// Removing a file failed.
    #[error("cannot remove `{path}`: {source}")]
    Remove {
        /// The path that failed to remove.
        path: String,
        /// The underlying removal failure.
        #[source]
        source: io::Error,
    },
}
