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
    /// The apply/switch advisory lock was held by another writer — a TRANSIENT contention the caller
    /// (the work/09 P5 daemon) should back off and RETRY, not treat as a permanent reject. Explicit so
    /// retry classification never parses error text (a lock-busy was previously masked as a
    /// `WrongGeneration` reject, which ALSO signals real cursor/generation conflicts).
    #[error("apply/switch advisory lock is busy: {message}")]
    LockBusy { message: String },
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

    /// Build a [`SyncError::LockBusy`] (transient apply/switch-lock contention; retry).
    #[must_use]
    pub fn lock_busy(message: impl Into<String>) -> Self {
        Self::LockBusy {
            message: message.into(),
        }
    }

    /// Whether the error is a TRANSIENT condition the daemon should back off and retry (apply/switch
    /// lock contention, or a transient IO/fetch blip — incl. a manifest observed before its artifacts
    /// during a non-atomic publish), rather than a permanent contract reject or a fatal fault.
    /// Classification is by TYPE, never by parsing message text.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        use jurisearch_storage::runtime::StorageError;
        match self {
            SyncError::LockBusy { .. } => true,
            SyncError::Storage(
                StorageError::ApplyLockBusy { .. }
                | StorageError::AdvisoryLockBusy { .. }
                | StorageError::StorageLockBusy { .. },
            ) => true,
            // A transient fetch/IO blip (a missing-but-coming artifact, an fs/network hiccup) is retryable.
            SyncError::Io(_) => true,
            _ => false,
        }
    }
}
