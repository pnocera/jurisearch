//! OpenAI-compatible embedding client and configuration. This crate root keeps the shared
//! external imports and re-exports the public API; the config/fingerprint/client/tokenizer/
//! error detail lives in submodules that pull the shared scope via `use super::*`.

use std::{
    fmt,
    path::{Path, PathBuf},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;
use tokenizers::Tokenizer;
use url::Host;

mod client;
mod config;
mod error;
mod fingerprint;
mod tokenizer;

use self::tokenizer::*;

pub use client::{
    BaseUrlClass, EmbeddingInputStats, EmbeddingVector, OpenAiCompatibleClient, base_url_class,
};
pub use config::{
    EmbeddingConfig, EmbeddingManifest, EmbeddingProvider, EmbeddingTokenCountMethod,
    PHASE0_EMBEDDING_DIMENSION, PHASE0_EMBEDDING_ESTIMATED_CHARS_PER_TOKEN,
    PHASE0_EMBEDDING_MAX_ESTIMATED_TOKENS, PHASE0_EMBEDDING_MAX_INPUT_CHARS,
    PHASE0_EMBEDDING_MODEL, PHASE0_EMBEDDING_POOLING,
};
pub use error::EmbeddingError;
pub use fingerprint::EmbeddingFingerprint;

#[cfg(test)]
mod tests;
