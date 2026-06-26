//! The consumer-side error type. Carries a machine-readable [`RejectCode`] where the design defines
//! one (§6.3), so a refusal is explainable ("not subscribed to corpus X", "schema ahead") rather than
//! a generic failure.

use jurisearch_package::RejectCode;

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("storage error: {0}")]
    Storage(#[from] jurisearch_storage::runtime::StorageError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("postgres error: {0}")]
    Postgres(#[from] postgres::Error),
    /// A contract refusal with a machine-readable reject code (design §6.3).
    #[error("package rejected ({code:?}): {message}")]
    Reject { code: RejectCode, message: String },
}

impl SyncError {
    /// Build a [`SyncError::Reject`] with a reject code + human message.
    #[must_use]
    pub fn reject(code: RejectCode, message: impl Into<String>) -> Self {
        Self::Reject {
            code,
            message: message.into(),
        }
    }
}
