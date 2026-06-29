//! The producer's typed error boundary.

use std::path::PathBuf;

use thiserror::Error;

/// Everything the producer can fail at, mapped to a stable exit class (see [`ProducerError::class`]).
#[derive(Debug, Error)]
pub enum ProducerError {
    #[error("failed to read config `{path}`: {source}")]
    ConfigRead {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to parse config `{path}`: {source}")]
    ConfigParse {
        path: PathBuf,
        #[source]
        source: toml::de::Error,
    },
    #[error("invalid config: {0}")]
    ConfigInvalid(String),
    #[error("secret file `{path}`: {message}")]
    Secret { path: PathBuf, message: String },
    #[error("unknown fetch group `{0}`")]
    UnknownGroup(String),
    #[error("unknown DILA source token `{0}` (expected one of legi/cass/capp/inca/jade)")]
    UnknownSource(String),
    #[error("fetch failed: {0}")]
    Fetch(#[from] jurisearch_fetch::FetchError),
    #[error("ingest failed: {0}")]
    Ingest(#[from] jurisearch_pipeline::IngestError),
    #[error("enrichment failed: {0}")]
    Enrich(#[from] jurisearch_pipeline::EnrichError),
    #[error("embedding failed: {0}")]
    Embed(#[from] jurisearch_pipeline::EmbedError),
    #[error("package build/publish failed: {0}")]
    Build(#[from] jurisearch_package_build::BuildError),
    #[error(
        "external producer database is not provisioned (run `jurisearch-producer provision-db` first): {0}"
    )]
    Unprovisioned(String),
    #[error("provisioning failed: {0}")]
    Provision(#[from] jurisearch_storage::provision::ProvisionError),
    #[error("storage error: {0}")]
    Storage(#[from] jurisearch_storage::runtime::StorageError),
    #[error(
        "the `{lock}` update lock is held by another run (skipped after waiting {waited_secs}s)"
    )]
    LockHeld { lock: String, waited_secs: u64 },
    #[error("io error at `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl ProducerError {
    /// A stable, machine-readable exit class for a timer/alert wrapper (a small subset of the M3
    /// taxonomy; M3 extends it).
    #[must_use]
    pub fn class(&self) -> &'static str {
        match self {
            ProducerError::ConfigRead { .. }
            | ProducerError::ConfigParse { .. }
            | ProducerError::ConfigInvalid(_)
            | ProducerError::Secret { .. }
            | ProducerError::UnknownGroup(_)
            | ProducerError::UnknownSource(_) => "config-invalid",
            ProducerError::Fetch(_) => "fetch-failed",
            ProducerError::Ingest(_) => "ingest-failed",
            ProducerError::Enrich(_) => "enrich-degraded",
            ProducerError::Embed(_) => "embed-failed",
            ProducerError::Build(_) => "publish-failed",
            ProducerError::Unprovisioned(_) => "producer-db-unprovisioned",
            ProducerError::Provision(_) => "provision-failed",
            ProducerError::Storage(_) => "storage-failed",
            ProducerError::LockHeld { .. } => "skipped-lock-held",
            ProducerError::Io { .. } => "io-failed",
        }
    }
}
