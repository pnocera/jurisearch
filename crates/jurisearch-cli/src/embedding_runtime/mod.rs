//! Embedding runtime hub: the query-time `PreparedQueryEmbedder` and the env/TOML config loader
//! (`config`). The bulk endpoint pool, model-cache/endpoint status probes, the endpoint identity, and
//! `ensure_embedding_runtime_ready` now live in `jurisearch-pipeline` (work/10 M1-C); they are
//! re-exported here so the CLI's status/doctor/serve surfaces keep their `crate::<fn>` references.

use crate::*;

mod config;

pub(crate) use config::*;
pub(crate) use jurisearch_pipeline::embedding::*;

/// A query-embedding client built once and reused across many searches. Building an
/// `OpenAiCompatibleClient` loads a tokenizer and a fresh HTTP agent, and `ensure_embedding_runtime_ready`
/// is a network probe — paying both per query in a batch sweep is wasteful. The index is static during a
/// run, so one prepared embedder serves all.
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

/// The work/09 P3B query-embedder seam: the CLI injects its prepared embedder into the side-effect-free
/// response builders (which depend only on the `QueryEmbedder` trait, never on the CLI runtime).
impl jurisearch_query::QueryEmbedder for PreparedQueryEmbedder {
    fn embed(&self, text: &str) -> Result<jurisearch_query::QueryEmbedding, ErrorObject> {
        let (literal, fingerprint) = PreparedQueryEmbedder::embed(self, text)?;
        Ok(jurisearch_query::QueryEmbedding {
            literal,
            fingerprint,
        })
    }
}
