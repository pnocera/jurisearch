//! The query embedder seam (DIP): the builders depend on this trait, never on a concrete embedding
//! runtime. The site service injects a local llama.cpp bge-m3 embedder; the CLI injects its existing
//! `PreparedQueryEmbedder`; tests inject a stub. The thin client never references it.

use jurisearch_core::error::ErrorObject;

/// A query embedding ready for dense retrieval: a pgvector literal (`[v0,v1,…]`) and the fingerprint
/// it was produced under (the dense-compatibility key checked against the active generation).
#[derive(Debug, Clone)]
pub struct QueryEmbedding {
    /// The pgvector literal embedding of the query text.
    pub literal: String,
    /// The embedding fingerprint (model:dim:pooling:normalize), compared with the active generation's.
    pub fingerprint: String,
}

/// Embed query text for dense retrieval. The builders call it ONLY when the retrieval mode uses dense,
/// so a lexical-only request never constructs or invokes an embedder.
pub trait QueryEmbedder {
    /// Embed `text` into a pgvector literal + its fingerprint, mapping failures to a response
    /// `ErrorObject` (the same shaping the CLI used).
    fn embed(&self, text: &str) -> Result<QueryEmbedding, ErrorObject>;
}
