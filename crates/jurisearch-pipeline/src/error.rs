//! Pipeline error vocabulary.
//!
//! The producer-facing entrypoints (`ingest_archives`, `enrich_zones`, `embed_documents`) return
//! the typed errors [`IngestError`], [`EnrichError`], and [`EmbedError`]. Internally the extracted
//! logic still constructs [`jurisearch_core::error::ErrorObject`] (the shared core protocol type) via
//! the small constructors below, so error TEXT and CODES stay byte-identical to the CLI's historical
//! output; the typed wrappers carry that `ErrorObject` across the library boundary and the thin CLI
//! consumer unwraps it to preserve its exact error emission.
//!
//! These `ErrorObject` constructors are byte-identical re-implementations of the ones owned by
//! `jurisearch-query` (`dependency_unavailable`/`index_unavailable`/`no_results`/`storage_error_object`);
//! they are duplicated here so `jurisearch-pipeline` keeps the dependency set the contract specifies
//! (core/embed/ingest/official-api/storage) without taking a `jurisearch-query` edge.

use jurisearch_core::error::{ErrorCode, ErrorObject};
use jurisearch_storage::runtime::StorageError;

pub(crate) fn index_unavailable(message: impl Into<String>) -> ErrorObject {
    ErrorObject {
        code: ErrorCode::IndexUnavailable,
        message: message.into(),
        suggestions: vec![
            "Build or select an index before running retrieval commands.".into(),
            "Pass `--index-dir <path>` or set JURISEARCH_INDEX_DIR.".into(),
        ],
    }
}

pub(crate) fn dependency_unavailable(message: impl Into<String>) -> ErrorObject {
    ErrorObject {
        code: ErrorCode::DependencyUnavailable,
        message: message.into(),
        suggestions: vec![
            "Check PostgreSQL extension setup and embedding endpoint configuration.".into(),
        ],
    }
}

pub(crate) fn no_results(message: impl Into<String>) -> ErrorObject {
    ErrorObject {
        code: ErrorCode::NoResults,
        message: message.into(),
        suggestions: vec!["Try a different query, ID, or --as-of date.".into()],
    }
}

pub(crate) fn storage_error_object(error: StorageError) -> ErrorObject {
    let message = error.to_string();
    match &error {
        StorageError::StorageLockBusy { .. } | StorageError::AdvisoryLockBusy { .. } => {
            index_unavailable(message)
        }
        _ => dependency_unavailable(message),
    }
}

pub(crate) fn upstream_unavailable(message: impl Into<String>) -> ErrorObject {
    ErrorObject {
        code: ErrorCode::Upstream,
        message: message.into(),
        suggestions: vec!["Check the configured OpenAI-compatible embeddings endpoint.".into()],
    }
}

pub(crate) fn embedding_error_object(error: jurisearch_embed::EmbeddingError) -> ErrorObject {
    let message = error.to_string();
    match &error {
        jurisearch_embed::EmbeddingError::InputTooLong(_) => ErrorObject::bad_input(message),
        jurisearch_embed::EmbeddingError::Endpoint(_)
        | jurisearch_embed::EmbeddingError::InvalidResponse(_)
        | jurisearch_embed::EmbeddingError::EmptyResponse
        | jurisearch_embed::EmbeddingError::BatchSizeMismatch { .. } => {
            upstream_unavailable(message)
        }
        _ => dependency_unavailable(message),
    }
}

pub(crate) fn embedding_error_object_with_context(
    error: jurisearch_embed::EmbeddingError,
    chunk_id: &str,
) -> ErrorObject {
    let mut object = embedding_error_object(error);
    object.message = format!("embedding chunk `{chunk_id}` failed: {}", object.message);
    object
}

/// A pgvector array literal (`[v0,v1,...]`) at the producer's fixed 8-decimal precision.
pub(crate) fn pgvector_literal(values: &[f32]) -> String {
    let values = values
        .iter()
        .map(|value| format!("{value:.8}"))
        .collect::<Vec<_>>()
        .join(",");
    format!("[{values}]")
}

macro_rules! typed_error {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone)]
        pub struct $name(pub ErrorObject);

        impl $name {
            /// The wrapped [`ErrorObject`] (code/message/suggestions), borrowed.
            #[must_use]
            pub fn error_object(&self) -> &ErrorObject {
                &self.0
            }

            /// Consume the typed error, yielding the wrapped [`ErrorObject`].
            #[must_use]
            pub fn into_error_object(self) -> ErrorObject {
                self.0
            }
        }

        impl From<ErrorObject> for $name {
            fn from(object: ErrorObject) -> Self {
                Self(object)
            }
        }

        impl From<$name> for ErrorObject {
            fn from(error: $name) -> Self {
                error.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str(&self.0.message)
            }
        }

        impl std::error::Error for $name {}
    };
}

typed_error!(
    /// Failure of [`crate::ingest_archives`] (archive read / parse / accounting / DB error).
    IngestError
);
typed_error!(
    /// Failure of [`crate::enrich_zones`] (Judilibre zone backfill / DB error).
    EnrichError
);
typed_error!(
    /// Failure of [`crate::embed_documents`] (embedding endpoint / DB / fingerprint error).
    EmbedError
);
