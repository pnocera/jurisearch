//! Embedding error taxonomy.

use super::*;

#[derive(Debug, Error)]
pub enum EmbeddingError {
    #[error("embedding provider {provider:?} is not supported by the OpenAI-compatible client")]
    UnsupportedProvider { provider: EmbeddingProvider },
    #[error("embedding base_url is required for provider openai_compatible")]
    MissingBaseUrl,
    #[error("embedding endpoint request failed: {0}")]
    Endpoint(String),
    #[error("embedding endpoint response was invalid: {0}")]
    InvalidResponse(String),
    #[error("embedding endpoint returned no embeddings")]
    EmptyResponse,
    #[error("embedding endpoint returned {actual} embeddings for a batch of {expected} inputs")]
    BatchSizeMismatch { expected: usize, actual: usize },
    #[error(
        "embedding input is too long: {0}; split the document chunk or adjust JURISEARCH_EMBED_MAX_INPUT_CHARS/JURISEARCH_EMBED_MAX_ESTIMATED_TOKENS for this endpoint"
    )]
    InputTooLong(EmbeddingInputStats),
    #[error(
        "embedding endpoint returned {actual} dimensions for model `{model}`, expected {expected}; use an endpoint/model matching the index fingerprint or rebuild/re-embed the index"
    )]
    DimensionMismatch {
        model: String,
        expected: usize,
        actual: usize,
    },
    #[error(
        "embedding endpoint returned a vector for model `{model}` with L2 norm {norm:.4}, but the configured fingerprint requires normalized vectors"
    )]
    NormalizationMismatch { model: String, norm: f32 },
    #[error(
        "embedding fingerprint mismatch: configured {configured:?}, index requires {expected:?}; reconfigure the endpoint or run an explicit re-embed/index migration"
    )]
    FingerprintMismatch {
        expected: Box<EmbeddingFingerprint>,
        configured: Box<EmbeddingFingerprint>,
    },
    #[error(
        "in-process embedding model `{model}` is missing; run `jurisearch model fetch {model}` or pass explicit download permission"
    )]
    MissingLocalModel { model: String },
    #[error("embedding tokenizer `{}` could not be loaded: {message}", path.display())]
    TokenizerLoad { path: PathBuf, message: String },
    #[error("embedding tokenizer failed to count input tokens: {0}")]
    TokenizerEncode(String),
}
