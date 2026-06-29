//! Embedding runtime: the bulk endpoint pool (`pool`), model-cache / endpoint status probes
//! (`status`), and the shared endpoint identity (`endpoint`). The query-time `PreparedQueryEmbedder`
//! and the env/TOML config loader stay in `jurisearch-cli` (they are query/runtime concerns, not part
//! of the producer pipeline); they consume these types.

use crate::*;

pub mod endpoint;
mod pool;
mod status;

pub use endpoint::EmbeddingPoolEndpoint;
pub use pool::*;
pub use status::*;

/// Ensure the embedding runtime is ready for `embedding_config` (the in-process model cache must be
/// present unless `allow_download`). A network probe for hosted providers is intentionally NOT done
/// here — the bulk pool surfaces endpoint failures per batch.
pub fn ensure_embedding_runtime_ready(
    embedding_config: &EmbeddingConfig,
    allow_download: bool,
) -> Result<(), ErrorObject> {
    let model_cache = model_cache_status(embedding_config);
    embedding_config
        .ensure_in_process_ready(model_cache.model_present(), allow_download)
        .map_err(embedding_error_object)
}
