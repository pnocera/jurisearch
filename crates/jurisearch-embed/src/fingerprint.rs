//! Embedding model fingerprint (identity + storage fingerprint).

use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingFingerprint {
    pub provider: EmbeddingProvider,
    pub base_url_class: BaseUrlClass,
    pub model: String,
    pub dimension: usize,
    pub normalize: bool,
    pub pooling: String,
}

impl EmbeddingFingerprint {
    #[must_use]
    pub fn storage_embedding_fingerprint(&self) -> String {
        format!(
            "{}:{}:normalize:{}",
            self.model, self.dimension, self.normalize
        )
    }
}
