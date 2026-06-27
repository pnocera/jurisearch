//! The single authority for the read path's `ErrorObject` construction and storage-error mapping —
//! shared by the CLI adapters and (P4) the site query service, so both shape byte-identical error
//! responses. (CLI-only constructors that depend on `jurisearch-embed` or the readiness gate stay in
//! the CLI.)

use jurisearch_core::error::{ErrorCode, ErrorObject};
use jurisearch_storage::runtime::StorageError;
use serde_json::Value;

/// The index/dependency is present but cannot serve (lock contention, missing extension setup).
pub fn index_unavailable(message: impl Into<String>) -> ErrorObject {
    ErrorObject {
        code: ErrorCode::IndexUnavailable,
        message: message.into(),
        suggestions: vec![
            "Build or select an index before running retrieval commands.".into(),
            "Pass `--index-dir <path>` or set JURISEARCH_INDEX_DIR.".into(),
        ],
    }
}

/// A dependency (PostgreSQL, an extension, the embedding endpoint) is unavailable or returned an
/// unusable result.
pub fn dependency_unavailable(message: impl Into<String>) -> ErrorObject {
    ErrorObject {
        code: ErrorCode::DependencyUnavailable,
        message: message.into(),
        suggestions: vec![
            "Check PostgreSQL extension setup and embedding endpoint configuration.".into(),
        ],
    }
}

/// A well-formed query that legitimately matched nothing.
pub fn no_results(message: impl Into<String>) -> ErrorObject {
    ErrorObject {
        code: ErrorCode::NoResults,
        message: message.into(),
        suggestions: vec!["Try a different query, ID, or --as-of date.".into()],
    }
}

/// Map a storage-layer error to a response `ErrorObject` (lock busy → index-unavailable; everything
/// else → dependency-unavailable), preserving the storage message text.
pub fn storage_error_object(error: StorageError) -> ErrorObject {
    let message = error.to_string();
    match &error {
        StorageError::StorageLockBusy { .. } | StorageError::AdvisoryLockBusy { .. } => {
            index_unavailable(message)
        }
        _ => dependency_unavailable(message),
    }
}

/// Parse a storage-layer JSON string into a [`Value`], mapping a malformed payload to the standard
/// `dependency_unavailable` error (the storage helpers return serialized JSON strings; this is the
/// shared bridge into the `Value` world).
pub fn parse_storage_json(response: &str) -> Result<Value, ErrorObject> {
    serde_json::from_str(response).map_err(|error| dependency_unavailable(error.to_string()))
}
