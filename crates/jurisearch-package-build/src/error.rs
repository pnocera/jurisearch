//! Producer build errors.

#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error("storage error: {0}")]
    Storage(#[from] jurisearch_storage::runtime::StorageError),
    #[error("postgres error: {0}")]
    Postgres(#[from] postgres::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("canonicalisation error: {0}")]
    Canonical(#[from] jurisearch_package::canonical::CanonicalError),
    #[error("signing error: {0}")]
    Sign(#[from] jurisearch_package::crypto::SignError),
    #[error("attribution error: {0}")]
    Attribution(#[from] jurisearch_package::corpus::AttributionError),
}
