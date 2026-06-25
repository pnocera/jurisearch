//! Query-time embedding runtime. `PreparedQueryEmbedder` builds the OpenAI-compatible client
//! once and reuses it across a retrieval/eval sweep. (The bulk embedding-pool runtime and config
//! loaders move here in a later phase.)

use jurisearch_embed::{EmbeddingFingerprint, OpenAiCompatibleClient};

use jurisearch_core::error::ErrorObject;

use crate::errors::embedding_error_object;
use crate::{embedding_config_from_env, ensure_embedding_runtime_ready, pgvector_literal};

/// A query-embedding client built once and reused across many searches. Building an
/// `OpenAiCompatibleClient` loads a tokenizer and a fresh HTTP agent, and `ensure_embedding_runtime_ready`
/// is a network probe — paying both per query in a batch sweep (the France-LEGI runner issues up to
/// ~192 queries) is wasteful. The index is static during a run, so one prepared embedder serves all.
pub(crate) struct PreparedQueryEmbedder {
    pub(crate) client: OpenAiCompatibleClient,
    pub(crate) expected_fingerprint: EmbeddingFingerprint,
    pub(crate) storage_fingerprint: String,
}

impl PreparedQueryEmbedder {
    pub(crate) fn from_env() -> Result<Self, ErrorObject> {
        let embedding_config = embedding_config_from_env();
        ensure_embedding_runtime_ready(&embedding_config, false)?;
        let expected_fingerprint = embedding_config.fingerprint();
        let storage_fingerprint = embedding_config.storage_embedding_fingerprint();
        let client =
            OpenAiCompatibleClient::new(embedding_config).map_err(embedding_error_object)?;
        Ok(Self {
            client,
            expected_fingerprint,
            storage_fingerprint,
        })
    }

    /// Returns `(pgvector_literal, storage_fingerprint)` for the query.
    pub(crate) fn embed(&self, query: &str) -> Result<(String, String), ErrorObject> {
        let embedding = self
            .client
            .embed_query(query, &self.expected_fingerprint)
            .map_err(embedding_error_object)?;
        Ok((
            pgvector_literal(&embedding.values),
            self.storage_fingerprint.clone(),
        ))
    }
}
