//! Shared io error context: one variant per filesystem operation, naming the
//! failed operation and its object with the raw cause riding as `#[source]`.
//! Constructed by any ctl module that touches the filesystem.

use std::io;

#[derive(Debug, thiserror::Error)]
pub enum FsError {
    #[error("cannot create `{dir}`: {source}")]
    CreateDir {
        dir: String,
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
