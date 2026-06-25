//! Embedding configuration: provider/token-count method, PHASE0 defaults, config + manifest.

use super::*;

pub const PHASE0_EMBEDDING_MODEL: &str = "bge-m3";

pub const PHASE0_EMBEDDING_DIMENSION: usize = 1024;

pub const PHASE0_EMBEDDING_POOLING: &str = "cls";

// Keep the default character ceiling below the rough bge-m3 token ceiling; a
// configured tokenizer can apply the endpoint-specific token budget exactly.
pub const PHASE0_EMBEDDING_MAX_INPUT_CHARS: usize = 20_000;

pub const PHASE0_EMBEDDING_MAX_ESTIMATED_TOKENS: usize = 8_192;

pub const PHASE0_EMBEDDING_ESTIMATED_CHARS_PER_TOKEN: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmbeddingProvider {
    #[serde(rename = "openai_compatible")]
    OpenAiCompatible,
    InProcess,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmbeddingTokenCountMethod {
    EstimatedChars,
    Tokenizer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingManifest {
    pub fingerprint: EmbeddingFingerprint,
    pub provisional: bool,
    pub reembeddable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingConfig {
    pub provider: EmbeddingProvider,
    pub base_url: Option<String>,
    pub base_urls: Vec<String>,
    pub api_key: Option<String>,
    pub model: String,
    pub request_model: Option<String>,
    pub dimension: usize,
    pub normalize: bool,
    pub pooling: String,
    pub max_input_chars: Option<usize>,
    pub max_estimated_tokens: Option<usize>,
    pub estimated_chars_per_token: usize,
    pub tokenizer_path: Option<PathBuf>,
    pub provisional: bool,
    pub reembeddable: bool,
}

impl EmbeddingConfig {
    pub fn openai_compatible(
        base_url: impl Into<String>,
        api_key: Option<String>,
        model: impl Into<String>,
        dimension: usize,
        normalize: bool,
        pooling: impl Into<String>,
    ) -> Self {
        let base_url = base_url.into();
        Self {
            provider: EmbeddingProvider::OpenAiCompatible,
            base_url: Some(base_url.clone()),
            base_urls: vec![base_url],
            api_key,
            model: model.into(),
            request_model: None,
            dimension,
            normalize,
            pooling: pooling.into(),
            max_input_chars: Some(PHASE0_EMBEDDING_MAX_INPUT_CHARS),
            max_estimated_tokens: Some(PHASE0_EMBEDDING_MAX_ESTIMATED_TOKENS),
            estimated_chars_per_token: PHASE0_EMBEDDING_ESTIMATED_CHARS_PER_TOKEN,
            tokenizer_path: None,
            provisional: false,
            reembeddable: true,
        }
    }

    pub fn phase0_bge_m3(base_url: impl Into<String>, api_key: Option<String>) -> Self {
        let base_url = base_url.into();
        Self {
            provider: EmbeddingProvider::OpenAiCompatible,
            base_url: Some(base_url.clone()),
            base_urls: vec![base_url],
            api_key,
            model: PHASE0_EMBEDDING_MODEL.to_owned(),
            request_model: None,
            dimension: PHASE0_EMBEDDING_DIMENSION,
            normalize: true,
            pooling: PHASE0_EMBEDDING_POOLING.to_owned(),
            max_input_chars: Some(PHASE0_EMBEDDING_MAX_INPUT_CHARS),
            max_estimated_tokens: Some(PHASE0_EMBEDDING_MAX_ESTIMATED_TOKENS),
            estimated_chars_per_token: PHASE0_EMBEDDING_ESTIMATED_CHARS_PER_TOKEN,
            tokenizer_path: None,
            provisional: true,
            reembeddable: true,
        }
    }

    pub fn in_process(model: impl Into<String>, dimension: usize) -> Self {
        Self {
            provider: EmbeddingProvider::InProcess,
            base_url: None,
            base_urls: Vec::new(),
            api_key: None,
            model: model.into(),
            request_model: None,
            dimension,
            normalize: true,
            pooling: PHASE0_EMBEDDING_POOLING.to_owned(),
            max_input_chars: Some(PHASE0_EMBEDDING_MAX_INPUT_CHARS),
            max_estimated_tokens: Some(PHASE0_EMBEDDING_MAX_ESTIMATED_TOKENS),
            estimated_chars_per_token: PHASE0_EMBEDDING_ESTIMATED_CHARS_PER_TOKEN,
            tokenizer_path: None,
            provisional: true,
            reembeddable: true,
        }
    }

    #[must_use]
    pub fn fingerprint(&self) -> EmbeddingFingerprint {
        EmbeddingFingerprint {
            provider: self.provider,
            base_url_class: match self.provider {
                EmbeddingProvider::OpenAiCompatible => {
                    base_url_class(self.base_url.as_deref().unwrap_or_default())
                }
                EmbeddingProvider::InProcess => BaseUrlClass::InProcess,
            },
            model: self.model.clone(),
            dimension: self.dimension,
            normalize: self.normalize,
            pooling: self.pooling.clone(),
        }
    }

    #[must_use]
    pub fn storage_embedding_fingerprint(&self) -> String {
        self.fingerprint().storage_embedding_fingerprint()
    }

    #[must_use]
    pub fn manifest(&self) -> EmbeddingManifest {
        EmbeddingManifest {
            fingerprint: self.fingerprint(),
            provisional: self.provisional,
            reembeddable: self.reembeddable,
        }
    }

    pub fn ensure_matches_index(
        &self,
        expected: &EmbeddingFingerprint,
    ) -> Result<(), EmbeddingError> {
        let configured = self.fingerprint();
        if &configured == expected {
            Ok(())
        } else {
            Err(EmbeddingError::FingerprintMismatch {
                expected: Box::new(expected.clone()),
                configured: Box::new(configured),
            })
        }
    }

    pub fn ensure_in_process_ready(
        &self,
        model_present: bool,
        allow_download: bool,
    ) -> Result<(), EmbeddingError> {
        if self.provider != EmbeddingProvider::InProcess || model_present || allow_download {
            return Ok(());
        }
        Err(EmbeddingError::MissingLocalModel {
            model: self.model.clone(),
        })
    }

    pub fn preflight_input(&self, input: &str) -> Result<EmbeddingInputStats, EmbeddingError> {
        self.preflight_input_with_tokenizer(input, None)
    }

    #[must_use]
    pub fn request_model(&self) -> &str {
        self.request_model
            .as_deref()
            .filter(|model| !model.trim().is_empty())
            .unwrap_or(self.model.as_str())
    }

    pub fn preflight_input_with_tokenizer(
        &self,
        input: &str,
        tokenizer: Option<&Tokenizer>,
    ) -> Result<EmbeddingInputStats, EmbeddingError> {
        let chars = input.chars().count();
        let chars_per_token = self.estimated_chars_per_token.max(1);
        let estimated_tokens = chars.div_ceil(chars_per_token);
        let (tokens, token_count_method) = if let Some(tokenizer) = tokenizer {
            let encoding = tokenizer
                .encode(input, true)
                .map_err(|error| EmbeddingError::TokenizerEncode(error.to_string()))?;
            (
                encoding.get_ids().len(),
                EmbeddingTokenCountMethod::Tokenizer,
            )
        } else {
            (estimated_tokens, EmbeddingTokenCountMethod::EstimatedChars)
        };
        let stats = EmbeddingInputStats {
            chars,
            tokens,
            estimated_tokens,
            token_count_method,
            chars_per_token,
            max_chars: self.max_input_chars,
            max_estimated_tokens: self.max_estimated_tokens,
        };
        if self
            .max_input_chars
            .is_some_and(|max_chars| chars > max_chars)
            || self
                .max_estimated_tokens
                .is_some_and(|max_tokens| tokens > max_tokens)
        {
            return Err(EmbeddingError::InputTooLong(stats));
        }
        Ok(stats)
    }

    #[must_use]
    pub fn configured_token_count_method(&self) -> EmbeddingTokenCountMethod {
        if self.tokenizer_path.is_some() {
            EmbeddingTokenCountMethod::Tokenizer
        } else {
            EmbeddingTokenCountMethod::EstimatedChars
        }
    }
}
