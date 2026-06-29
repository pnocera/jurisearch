//! Crate-level error type for the fetch engine.

use std::{io, path::PathBuf};

use thiserror::Error;

/// Errors raised while listing, downloading, or mirroring DILA archives.
///
/// Note that a failed *integrity* check is NOT modelled as a [`FetchError`]:
/// integrity failure is an expected, per-archive outcome that quarantines the
/// file and continues. A [`FetchError`] is a hard failure of the run itself
/// (listing unreachable, filesystem error, etc.).
#[derive(Debug, Error)]
pub enum FetchError {
    /// The remote directory listing could not be retrieved.
    #[error("failed to list remote directory for `{archive_source}`: {message}")]
    Listing {
        archive_source: String,
        message: String,
    },

    /// A download transport error (network, HTTP status, etc.).
    #[error("failed to download `{file_name}` for `{archive_source}`: {message}")]
    Download {
        archive_source: String,
        file_name: String,
        message: String,
    },

    /// A filesystem operation failed.
    #[error("filesystem error at `{path}`: {source}")]
    Io { path: PathBuf, source: io::Error },

    /// The persisted cursor could not be read or written.
    #[error("cursor state error at `{path}`: {message}")]
    Cursor { path: PathBuf, message: String },
}

impl FetchError {
    pub(crate) fn io(path: impl Into<PathBuf>, source: io::Error) -> Self {
        FetchError::Io {
            path: path.into(),
            source,
        }
    }
}
