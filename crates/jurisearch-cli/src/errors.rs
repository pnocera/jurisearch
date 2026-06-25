//! Definitive owner of `ErrorObject` construction and storage/embedding error mapping. Every
//! command module builds its error responses through these constructors; `output.rs` only
//! serializes/emits the resulting `ErrorObject`.

use jurisearch_core::error::{ErrorCode, ErrorObject};
use jurisearch_storage::ingest_accounting::CoverageMetric;
use jurisearch_storage::runtime::StorageError;

use crate::QueryReadinessGate;

pub(crate) fn index_not_query_ready(
    gate: QueryReadinessGate,
    reason: &str,
    projection_coverage: &CoverageMetric,
    embedding_coverage: Option<&CoverageMetric>,
) -> ErrorObject {
    let embedding_coverage = embedding_coverage
        .map(|metric| format!("{}/{}", metric.covered, metric.total))
        .unwrap_or_else(|| "not checked".to_owned());
    ErrorObject {
        code: ErrorCode::IndexUnavailable,
        message: format!(
            "index is not query-ready for `{}`: {reason}; projection coverage {}/{}, embedding coverage {embedding_coverage}",
            gate.command(),
            projection_coverage.covered,
            projection_coverage.total,
        ),
        suggestions: vec![
            "Run `jurisearch status` to inspect ingest health and coverage gates.".into(),
            "Run `jurisearch ingest legi-archives` and `jurisearch ingest embed-chunks` before retrieval commands.".into(),
        ],
    }
}

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

pub(crate) fn upstream_unavailable(message: impl Into<String>) -> ErrorObject {
    ErrorObject {
        code: ErrorCode::Upstream,
        message: message.into(),
        suggestions: vec!["Check the configured OpenAI-compatible embeddings endpoint.".into()],
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
