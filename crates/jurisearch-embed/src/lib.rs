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

pub const PHASE0_EMBEDDING_MODEL: &str = "bge-m3";
pub const PHASE0_EMBEDDING_DIMENSION: usize = 1024;
pub const PHASE0_EMBEDDING_POOLING: &str = "cls";
// Keep the default character ceiling below the rough bge-m3 token ceiling; a
// configured tokenizer can apply the endpoint-specific token budget exactly.
pub const PHASE0_EMBEDDING_MAX_INPUT_CHARS: usize = 20_000;
pub const PHASE0_EMBEDDING_MAX_ESTIMATED_TOKENS: usize = 8_192;
pub const PHASE0_EMBEDDING_ESTIMATED_CHARS_PER_TOKEN: usize = 4;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const READ_TIMEOUT: Duration = Duration::from_secs(30);
const NORMALIZED_L2_TOLERANCE: f32 = 0.01;
const INVALID_RESPONSE_BODY_LIMIT: usize = 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmbeddingProvider {
    #[serde(rename = "openai_compatible")]
    OpenAiCompatible,
    InProcess,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BaseUrlClass {
    LocalLoopback,
    Hosted,
    InProcess,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EmbeddingTokenCountMethod {
    EstimatedChars,
    Tokenizer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingFingerprint {
    pub provider: EmbeddingProvider,
    pub base_url_class: BaseUrlClass,
    pub model: String,
    pub dimension: usize,
    pub normalize: bool,
    pub pooling: String,
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

impl EmbeddingFingerprint {
    #[must_use]
    pub fn storage_embedding_fingerprint(&self) -> String {
        format!(
            "{}:{}:normalize:{}",
            self.model, self.dimension, self.normalize
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EmbeddingVector {
    pub values: Vec<f32>,
    pub fingerprint: EmbeddingFingerprint,
}

impl EmbeddingVector {
    #[must_use]
    pub fn l2_norm(&self) -> f32 {
        self.values
            .iter()
            .map(|value| value * value)
            .sum::<f32>()
            .sqrt()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmbeddingInputStats {
    pub chars: usize,
    pub tokens: usize,
    pub estimated_tokens: usize,
    pub token_count_method: EmbeddingTokenCountMethod,
    pub chars_per_token: usize,
    pub max_chars: Option<usize>,
    pub max_estimated_tokens: Option<usize>,
}

impl fmt::Display for EmbeddingInputStats {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.token_count_method {
            EmbeddingTokenCountMethod::EstimatedChars => {
                write!(
                    formatter,
                    "{} chars, estimated {} tokens using {} chars/token",
                    self.chars, self.estimated_tokens, self.chars_per_token
                )?;
            }
            EmbeddingTokenCountMethod::Tokenizer => {
                write!(
                    formatter,
                    "{} chars, tokenizer-counted {} tokens",
                    self.chars, self.tokens
                )?;
            }
        }
        if let Some(max_chars) = self.max_chars {
            write!(formatter, ", max chars {max_chars}")?;
        }
        if let Some(max_tokens) = self.max_estimated_tokens {
            match self.token_count_method {
                EmbeddingTokenCountMethod::EstimatedChars => {
                    write!(formatter, ", max estimated tokens {max_tokens}")?;
                }
                EmbeddingTokenCountMethod::Tokenizer => {
                    write!(formatter, ", max tokens {max_tokens}")?;
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct OpenAiCompatibleClient {
    config: EmbeddingConfig,
    agent: ureq::Agent,
    tokenizer: Option<Tokenizer>,
}

impl OpenAiCompatibleClient {
    pub fn new(config: EmbeddingConfig) -> Result<Self, EmbeddingError> {
        if config.provider != EmbeddingProvider::OpenAiCompatible {
            return Err(EmbeddingError::UnsupportedProvider {
                provider: config.provider,
            });
        }
        if config
            .base_url
            .as_deref()
            .unwrap_or_default()
            .trim()
            .is_empty()
        {
            return Err(EmbeddingError::MissingBaseUrl);
        }
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(CONNECT_TIMEOUT)
            .timeout_read(READ_TIMEOUT)
            .build();
        let tokenizer = load_tokenizer(config.tokenizer_path.as_deref())?;
        Ok(Self {
            config,
            agent,
            tokenizer,
        })
    }

    pub fn embed_query(
        &self,
        input: &str,
        expected: &EmbeddingFingerprint,
    ) -> Result<EmbeddingVector, EmbeddingError> {
        let mut embeddings = self.embed_batch(&[input], expected)?;
        embeddings.pop().ok_or(EmbeddingError::EmptyResponse)
    }

    pub fn embed_batch(
        &self,
        inputs: &[&str],
        expected: &EmbeddingFingerprint,
    ) -> Result<Vec<EmbeddingVector>, EmbeddingError> {
        if inputs.is_empty() {
            return Ok(Vec::new());
        }
        self.config.ensure_matches_index(expected)?;
        for input in inputs {
            self.config
                .preflight_input_with_tokenizer(input, self.tokenizer.as_ref())?;
        }
        let url = format!(
            "{}/embeddings",
            self.config
                .base_url
                .as_deref()
                .expect("base_url checked by constructor")
                .trim_end_matches('/')
        );
        let mut request = self
            .agent
            .post(&url)
            .set("Content-Type", "application/json");
        if let Some(api_key) = self
            .config
            .api_key
            .as_deref()
            .filter(|key| !key.is_empty() && *key != "no-key")
        {
            request = request.set("Authorization", &format!("Bearer {api_key}"));
        }

        let response_body = request
            .send_json(json!({
                "model": self.config.request_model(),
                "input": inputs,
            }))
            .map_err(endpoint_error)?
            .into_string()
            .map_err(|error| EmbeddingError::InvalidResponse(error.to_string()))?;
        if let Ok(error_response) =
            serde_json::from_str::<OpenAiEmbeddingErrorResponse>(&response_body)
            && !error_response.error.is_null()
        {
            return Err(EmbeddingError::Endpoint(format!(
                "endpoint error response: {}",
                truncate_response_body(&response_body)
            )));
        }
        let response =
            serde_json::from_str::<OpenAiEmbeddingResponse>(&response_body).map_err(|error| {
                EmbeddingError::InvalidResponse(format!(
                    "{error}: {}",
                    truncate_response_body(&response_body)
                ))
            })?;
        if response.data.is_empty() {
            return Err(EmbeddingError::EmptyResponse);
        }
        if response.data.len() != inputs.len() {
            return Err(EmbeddingError::BatchSizeMismatch {
                expected: inputs.len(),
                actual: response.data.len(),
            });
        }

        let fingerprint = self.config.fingerprint();
        response
            .data
            .into_iter()
            .map(|data| {
                if data.embedding.len() != expected.dimension {
                    return Err(EmbeddingError::DimensionMismatch {
                        model: self.config.model.clone(),
                        expected: expected.dimension,
                        actual: data.embedding.len(),
                    });
                }

                let vector = EmbeddingVector {
                    values: data.embedding,
                    fingerprint: fingerprint.clone(),
                };
                if expected.normalize && (vector.l2_norm() - 1.0).abs() > NORMALIZED_L2_TOLERANCE {
                    return Err(EmbeddingError::NormalizationMismatch {
                        model: self.config.model.clone(),
                        norm: vector.l2_norm(),
                    });
                }

                Ok(vector)
            })
            .collect()
    }
}

#[derive(Debug, Deserialize)]
struct OpenAiEmbeddingResponse {
    data: Vec<OpenAiEmbeddingData>,
}

#[derive(Debug, Deserialize)]
struct OpenAiEmbeddingErrorResponse {
    error: serde_json::Value,
}

#[derive(Debug, Deserialize)]
struct OpenAiEmbeddingData {
    embedding: Vec<f32>,
}

pub fn base_url_class(base_url: &str) -> BaseUrlClass {
    let Ok(url) = url::Url::parse(base_url) else {
        return BaseUrlClass::Hosted;
    };
    match url.host() {
        Some(Host::Domain(host)) if host.eq_ignore_ascii_case("localhost") => {
            BaseUrlClass::LocalLoopback
        }
        Some(Host::Ipv4(address)) if address.is_loopback() => BaseUrlClass::LocalLoopback,
        Some(Host::Ipv6(address)) if address.is_loopback() => BaseUrlClass::LocalLoopback,
        _ => BaseUrlClass::Hosted,
    }
}

fn endpoint_error(error: ureq::Error) -> EmbeddingError {
    match error {
        ureq::Error::Status(code, response) => {
            let body = response.into_string().unwrap_or_default();
            let body = truncate_response_body(&body);
            if body.is_empty() {
                EmbeddingError::Endpoint(format!("http status {code}"))
            } else {
                EmbeddingError::Endpoint(format!("http status {code}: {body}"))
            }
        }
        other => EmbeddingError::Endpoint(other.to_string()),
    }
}

fn truncate_response_body(body: &str) -> String {
    let body = body.trim();
    let mut end = body.len();
    let mut chars = 0usize;
    for (index, _) in body.char_indices() {
        if chars == INVALID_RESPONSE_BODY_LIMIT {
            end = index;
            break;
        }
        chars += 1;
    }
    let mut truncated = body[..end].to_owned();
    if chars == INVALID_RESPONSE_BODY_LIMIT && end < body.len() {
        truncated.push_str("...");
    }
    truncated
}

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

fn load_tokenizer(path: Option<&Path>) -> Result<Option<Tokenizer>, EmbeddingError> {
    let Some(path) = path else {
        return Ok(None);
    };
    let mut tokenizer =
        Tokenizer::from_file(path).map_err(|error| EmbeddingError::TokenizerLoad {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
    tokenizer
        .with_truncation(None)
        .map_err(|error| EmbeddingError::TokenizerLoad {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
    tokenizer.with_padding(None);
    Ok(Some(tokenizer))
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        path::Path,
        sync::{Arc, Mutex},
        thread,
    };

    use super::{
        BaseUrlClass, EmbeddingConfig, EmbeddingError, EmbeddingTokenCountMethod,
        OpenAiCompatibleClient, base_url_class,
    };
    use tokenizers::{
        Tokenizer, TruncationParams, models::wordlevel::WordLevel,
        pre_tokenizers::whitespace::Whitespace,
    };

    #[test]
    fn loopback_endpoint_is_classified_as_configured_remote_provider() {
        let config = EmbeddingConfig::phase0_bge_m3("http://127.0.0.1:8097/v1", None);
        let fingerprint = config.fingerprint();
        assert_eq!(fingerprint.base_url_class, BaseUrlClass::LocalLoopback);
        assert_eq!(
            base_url_class("https://embeddings.example.invalid/v1"),
            BaseUrlClass::Hosted
        );
        assert_eq!(
            base_url_class("http://localhost.attacker.example/v1"),
            BaseUrlClass::Hosted
        );
        assert_eq!(
            base_url_class("http://127.0.0.1.attacker.example/v1"),
            BaseUrlClass::Hosted
        );
    }

    #[test]
    fn openai_compatible_client_embeds_and_validates_fingerprint() {
        let base_url = spawn_embedding_server(r#"{"data":[{"embedding":[0.6,0.8,0.0]}]}"#);
        let config = EmbeddingConfig::openai_compatible(base_url, None, "bge-m3", 3, true, "cls");
        let expected = config.fingerprint();
        let client = OpenAiCompatibleClient::new(config).unwrap();

        let embedding = client
            .embed_query("responsabilite civile", &expected)
            .unwrap();
        assert_eq!(embedding.values, vec![0.6, 0.8, 0.0]);
        assert!((embedding.l2_norm() - 1.0).abs() < 0.001);
    }

    #[test]
    fn openai_compatible_client_embeds_batches_in_response_order() {
        let base_url = spawn_embedding_server(
            r#"{"data":[{"embedding":[1.0,0.0,0.0]},{"embedding":[0.0,1.0,0.0]}]}"#,
        );
        let config = EmbeddingConfig::openai_compatible(base_url, None, "bge-m3", 3, true, "cls");
        let expected = config.fingerprint();
        let client = OpenAiCompatibleClient::new(config).unwrap();

        let embeddings = client
            .embed_batch(&["article", "decision"], &expected)
            .unwrap();
        assert_eq!(embeddings.len(), 2);
        assert_eq!(embeddings[0].values, vec![1.0, 0.0, 0.0]);
        assert_eq!(embeddings[1].values, vec![0.0, 1.0, 0.0]);
    }

    #[test]
    fn request_model_alias_does_not_change_stored_fingerprint() {
        let request_log = Arc::new(Mutex::new(None::<String>));
        let request_log_clone = Arc::clone(&request_log);
        let base_url = spawn_embedding_server_with_request_check(
            "200 OK",
            r#"{"data":[{"embedding":[0.6,0.8,0.0]}]}"#,
            move |request| {
                *request_log_clone.lock().unwrap() = Some(request.to_owned());
            },
        );
        let mut config =
            EmbeddingConfig::openai_compatible(base_url, None, "bge-m3", 3, true, "cls");
        config.request_model = Some("baai/bge-m3".to_owned());
        let expected = config.fingerprint();
        let client = OpenAiCompatibleClient::new(config).unwrap();

        let embedding = client
            .embed_query("responsabilite civile", &expected)
            .unwrap();

        assert_eq!(embedding.fingerprint.model, "bge-m3");
        assert_eq!(embedding.values, vec![0.6, 0.8, 0.0]);
        let request = request_log.lock().unwrap().take().unwrap();
        assert!(request.contains(r#""model":"baai/bge-m3""#));
    }

    #[test]
    fn wrong_dimension_is_actionable_error() {
        let base_url = spawn_embedding_server(r#"{"data":[{"embedding":[0.0,1.0]}]}"#);
        let config = EmbeddingConfig::openai_compatible(base_url, None, "bge-m3", 3, true, "cls");
        let expected = config.fingerprint();
        let client = OpenAiCompatibleClient::new(config).unwrap();

        let error = client
            .embed_query("responsabilite civile", &expected)
            .unwrap_err();
        assert!(matches!(
            error,
            EmbeddingError::DimensionMismatch {
                expected: 3,
                actual: 2,
                ..
            }
        ));
        assert!(error.to_string().contains("rebuild/re-embed"));
    }

    #[test]
    fn unnormalized_vector_is_rejected_when_fingerprint_requires_normalized() {
        let base_url = spawn_embedding_server(r#"{"data":[{"embedding":[1.0,1.0,0.0]}]}"#);
        let config = EmbeddingConfig::openai_compatible(base_url, None, "bge-m3", 3, true, "cls");
        let expected = config.fingerprint();
        let client = OpenAiCompatibleClient::new(config).unwrap();

        let error = client
            .embed_query("responsabilite civile", &expected)
            .unwrap_err();
        assert!(matches!(
            error,
            EmbeddingError::NormalizationMismatch { .. }
        ));
    }

    #[test]
    fn oversized_input_is_rejected_before_endpoint_call() {
        let mut config = EmbeddingConfig::openai_compatible(
            "http://127.0.0.1:9/v1",
            None,
            "bge-m3",
            3,
            true,
            "cls",
        );
        config.max_input_chars = Some(4);
        config.max_estimated_tokens = None;
        let expected = config.fingerprint();
        let client = OpenAiCompatibleClient::new(config).unwrap();

        let error = client.embed_query("abcde", &expected).unwrap_err();
        match &error {
            EmbeddingError::InputTooLong(stats) => {
                assert_eq!(stats.chars, 5);
                assert_eq!(stats.max_chars, Some(4));
            }
            other => panic!("expected input-too-long error, got {other:?}"),
        }
        assert!(error.to_string().contains("5 chars"));
    }

    #[test]
    fn estimated_token_budget_is_enforced() {
        let mut config = EmbeddingConfig::openai_compatible(
            "http://127.0.0.1:9/v1",
            None,
            "bge-m3",
            3,
            true,
            "cls",
        );
        config.max_input_chars = None;
        config.max_estimated_tokens = Some(2);
        config.estimated_chars_per_token = 2;

        let error = config.preflight_input("abcde").unwrap_err();
        match error {
            EmbeddingError::InputTooLong(stats) => {
                assert_eq!(stats.tokens, 3);
                assert_eq!(
                    stats.token_count_method,
                    EmbeddingTokenCountMethod::EstimatedChars
                );
                assert_eq!(stats.estimated_tokens, 3);
                assert_eq!(stats.max_estimated_tokens, Some(2));
            }
            other => panic!("expected input-too-long error, got {other:?}"),
        }
    }

    #[test]
    fn tokenizer_budget_is_enforced_when_configured() {
        let tempdir = tempfile::tempdir().unwrap();
        let tokenizer_path = tempdir.path().join("tokenizer.json");
        write_test_tokenizer(&tokenizer_path);

        let mut config = EmbeddingConfig::openai_compatible(
            "http://127.0.0.1:9/v1",
            None,
            "bge-m3",
            3,
            true,
            "cls",
        );
        config.max_input_chars = None;
        config.max_estimated_tokens = Some(2);
        config.estimated_chars_per_token = 100;
        config.tokenizer_path = Some(tokenizer_path);
        let expected = config.fingerprint();
        let client = OpenAiCompatibleClient::new(config).unwrap();

        let error = client
            .embed_query("alpha beta gamma", &expected)
            .unwrap_err();
        match error {
            EmbeddingError::InputTooLong(stats) => {
                assert_eq!(stats.tokens, 3);
                assert_eq!(stats.estimated_tokens, 1);
                assert_eq!(
                    stats.token_count_method,
                    EmbeddingTokenCountMethod::Tokenizer
                );
                assert_eq!(stats.max_estimated_tokens, Some(2));
            }
            other => panic!("expected tokenizer-backed input-too-long error, got {other:?}"),
        }
    }

    #[test]
    fn tokenizer_preflight_ignores_embedded_truncation() {
        let tempdir = tempfile::tempdir().unwrap();
        let tokenizer_path = tempdir.path().join("tokenizer.json");
        write_truncating_test_tokenizer(&tokenizer_path, 2);

        let mut config = EmbeddingConfig::openai_compatible(
            "http://127.0.0.1:9/v1",
            None,
            "bge-m3",
            3,
            true,
            "cls",
        );
        config.max_input_chars = None;
        config.max_estimated_tokens = Some(2);
        config.estimated_chars_per_token = 100;
        config.tokenizer_path = Some(tokenizer_path);
        let expected = config.fingerprint();
        let client = OpenAiCompatibleClient::new(config).unwrap();

        let error = client
            .embed_query("alpha beta gamma", &expected)
            .unwrap_err();
        match error {
            EmbeddingError::InputTooLong(stats) => {
                assert_eq!(stats.tokens, 3);
                assert_eq!(
                    stats.token_count_method,
                    EmbeddingTokenCountMethod::Tokenizer
                );
            }
            other => panic!("expected full tokenizer count to exceed budget, got {other:?}"),
        }
    }

    #[test]
    fn tokenizer_load_error_names_path() {
        let mut config = EmbeddingConfig::openai_compatible(
            "http://127.0.0.1:9/v1",
            None,
            "bge-m3",
            3,
            true,
            "cls",
        );
        config.tokenizer_path = Some(Path::new("/tmp/jurisearch-missing-tokenizer.json").into());

        let error = OpenAiCompatibleClient::new(config).unwrap_err();
        assert!(matches!(error, EmbeddingError::TokenizerLoad { .. }));
        assert!(
            error
                .to_string()
                .contains("jurisearch-missing-tokenizer.json")
        );
    }

    #[test]
    fn http_status_error_preserves_endpoint_body() {
        let base_url =
            spawn_embedding_server_with_status("400 Bad Request", r#"{"error":"model not found"}"#);
        let config = EmbeddingConfig::openai_compatible(base_url, None, "bge-m3", 3, true, "cls");
        let expected = config.fingerprint();
        let client = OpenAiCompatibleClient::new(config).unwrap();

        let error = client
            .embed_query("responsabilite civile", &expected)
            .unwrap_err();
        assert!(error.to_string().contains("model not found"));
    }

    #[test]
    fn success_status_error_json_is_reported_as_endpoint_error() {
        let base_url = spawn_embedding_server(
            r#"{"error":{"message":"maximum context length is 8192 tokens","code":400}}"#,
        );
        let config = EmbeddingConfig::openai_compatible(base_url, None, "bge-m3", 3, true, "cls");
        let expected = config.fingerprint();
        let client = OpenAiCompatibleClient::new(config).unwrap();

        let error = client
            .embed_query("responsabilite civile", &expected)
            .unwrap_err();

        assert!(matches!(error, EmbeddingError::Endpoint(_)));
        assert!(error.to_string().contains("maximum context length"));
    }

    #[test]
    fn fingerprint_mismatch_fails_before_endpoint_call() {
        let config = EmbeddingConfig::phase0_bge_m3("http://127.0.0.1:9/v1", None);
        let mut expected = config.fingerprint();
        expected.model = "other-model".to_owned();
        let client = OpenAiCompatibleClient::new(config).unwrap();

        let error = client
            .embed_query("responsabilite civile", &expected)
            .unwrap_err();
        assert!(matches!(error, EmbeddingError::FingerprintMismatch { .. }));
    }

    #[test]
    fn in_process_mode_refuses_missing_model_without_explicit_permission() {
        let config = EmbeddingConfig::in_process("bge-m3", 1024);
        let error = config.ensure_in_process_ready(false, false).unwrap_err();
        assert!(matches!(error, EmbeddingError::MissingLocalModel { .. }));
        assert!(config.ensure_in_process_ready(false, true).is_ok());
        assert!(config.ensure_in_process_ready(true, false).is_ok());
    }

    fn spawn_embedding_server(response_body: &'static str) -> String {
        spawn_embedding_server_with_status("200 OK", response_body)
    }

    fn spawn_embedding_server_with_status(
        status: &'static str,
        response_body: &'static str,
    ) -> String {
        spawn_embedding_server_with_request_check(status, response_body, |_| {})
    }

    fn spawn_embedding_server_with_request_check(
        status: &'static str,
        response_body: &'static str,
        check_request: impl FnOnce(&str) + Send + 'static,
    ) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_http_request(&mut stream);
            assert!(request.starts_with("POST /v1/embeddings "));
            check_request(&request);
            let response = format!(
                "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                response_body.len(),
                response_body
            );
            stream.write_all(response.as_bytes()).unwrap();
        });
        format!("http://{address}/v1")
    }

    fn read_http_request(stream: &mut std::net::TcpStream) -> String {
        let mut bytes = Vec::new();
        let mut buffer = [0; 4096];
        loop {
            let read = stream.read(&mut buffer).unwrap();
            if read == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..read]);
            if request_is_complete(&bytes) {
                break;
            }
        }
        String::from_utf8_lossy(&bytes).into_owned()
    }

    fn request_is_complete(bytes: &[u8]) -> bool {
        let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
            return false;
        };
        let headers = String::from_utf8_lossy(&bytes[..header_end]);
        let content_length = headers.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.eq_ignore_ascii_case("Content-Length") {
                value.trim().parse::<usize>().ok()
            } else {
                None
            }
        });
        let Some(content_length) = content_length else {
            return true;
        };
        bytes.len() >= header_end + 4 + content_length
    }

    fn write_test_tokenizer(path: &Path) {
        write_test_tokenizer_with_truncation(path, None);
    }

    fn write_truncating_test_tokenizer(path: &Path, max_length: usize) {
        write_test_tokenizer_with_truncation(path, Some(max_length));
    }

    fn write_test_tokenizer_with_truncation(path: &Path, max_length: Option<usize>) {
        let vocab = [
            ("[UNK]".to_owned(), 0u32),
            ("alpha".to_owned(), 1),
            ("beta".to_owned(), 2),
            ("gamma".to_owned(), 3),
        ]
        .into_iter()
        .collect();
        let model = WordLevel::builder()
            .vocab(vocab)
            .unk_token("[UNK]".to_owned())
            .build()
            .unwrap();
        let mut tokenizer = Tokenizer::new(model);
        tokenizer.with_pre_tokenizer(Some(Whitespace));
        if let Some(max_length) = max_length {
            tokenizer
                .with_truncation(Some(TruncationParams {
                    max_length,
                    ..TruncationParams::default()
                }))
                .unwrap();
        }
        tokenizer.save(path, false).unwrap();
    }
}
