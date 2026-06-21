use serde::{Deserialize, Serialize};
use serde_json::json;
use thiserror::Error;

pub const PHASE0_EMBEDDING_MODEL: &str = "bge-m3";
pub const PHASE0_EMBEDDING_DIMENSION: usize = 1024;
pub const PHASE0_EMBEDDING_POOLING: &str = "cls";

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
    pub api_key: Option<String>,
    pub model: String,
    pub dimension: usize,
    pub normalize: bool,
    pub pooling: String,
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
        Self {
            provider: EmbeddingProvider::OpenAiCompatible,
            base_url: Some(base_url.into()),
            api_key,
            model: model.into(),
            dimension,
            normalize,
            pooling: pooling.into(),
            provisional: false,
            reembeddable: true,
        }
    }

    pub fn phase0_bge_m3(base_url: impl Into<String>, api_key: Option<String>) -> Self {
        Self {
            provider: EmbeddingProvider::OpenAiCompatible,
            base_url: Some(base_url.into()),
            api_key,
            model: PHASE0_EMBEDDING_MODEL.to_owned(),
            dimension: PHASE0_EMBEDDING_DIMENSION,
            normalize: true,
            pooling: PHASE0_EMBEDDING_POOLING.to_owned(),
            provisional: true,
            reembeddable: true,
        }
    }

    pub fn in_process(model: impl Into<String>, dimension: usize) -> Self {
        Self {
            provider: EmbeddingProvider::InProcess,
            base_url: None,
            api_key: None,
            model: model.into(),
            dimension,
            normalize: true,
            pooling: PHASE0_EMBEDDING_POOLING.to_owned(),
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

#[derive(Debug, Clone)]
pub struct OpenAiCompatibleClient {
    config: EmbeddingConfig,
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
        Ok(Self { config })
    }

    pub fn embed_query(
        &self,
        input: &str,
        expected: &EmbeddingFingerprint,
    ) -> Result<EmbeddingVector, EmbeddingError> {
        self.config.ensure_matches_index(expected)?;
        let url = format!(
            "{}/embeddings",
            self.config
                .base_url
                .as_deref()
                .expect("base_url checked by constructor")
                .trim_end_matches('/')
        );
        let mut request = ureq::post(&url).set("Content-Type", "application/json");
        if let Some(api_key) = self
            .config
            .api_key
            .as_deref()
            .filter(|key| !key.is_empty() && *key != "no-key")
        {
            request = request.set("Authorization", &format!("Bearer {api_key}"));
        }

        let response = request
            .send_json(json!({
                "model": self.config.model,
                "input": input,
            }))
            .map_err(|error| EmbeddingError::Endpoint(error.to_string()))?;
        let response = response
            .into_json::<OpenAiEmbeddingResponse>()
            .map_err(|error| EmbeddingError::InvalidResponse(error.to_string()))?;
        let embedding = response
            .data
            .into_iter()
            .next()
            .ok_or(EmbeddingError::EmptyResponse)?
            .embedding;

        if embedding.len() != expected.dimension {
            return Err(EmbeddingError::DimensionMismatch {
                model: self.config.model.clone(),
                expected: expected.dimension,
                actual: embedding.len(),
            });
        }

        Ok(EmbeddingVector {
            values: embedding,
            fingerprint: self.config.fingerprint(),
        })
    }
}

#[derive(Debug, Deserialize)]
struct OpenAiEmbeddingResponse {
    data: Vec<OpenAiEmbeddingData>,
}

#[derive(Debug, Deserialize)]
struct OpenAiEmbeddingData {
    embedding: Vec<f32>,
}

pub fn base_url_class(base_url: &str) -> BaseUrlClass {
    let lower = base_url.to_ascii_lowercase();
    if lower.starts_with("http://127.")
        || lower.starts_with("http://localhost")
        || lower.starts_with("http://[::1]")
        || lower.starts_with("https://127.")
        || lower.starts_with("https://localhost")
        || lower.starts_with("https://[::1]")
    {
        BaseUrlClass::LocalLoopback
    } else {
        BaseUrlClass::Hosted
    }
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
    #[error(
        "embedding endpoint returned {actual} dimensions for model `{model}`, expected {expected}; use an endpoint/model matching the index fingerprint or rebuild/re-embed the index"
    )]
    DimensionMismatch {
        model: String,
        expected: usize,
        actual: usize,
    },
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
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    use super::{
        BaseUrlClass, EmbeddingConfig, EmbeddingError, OpenAiCompatibleClient, base_url_class,
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
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0; 4096];
            let read = stream.read(&mut request).unwrap();
            let request = String::from_utf8_lossy(&request[..read]);
            assert!(request.starts_with("POST /v1/embeddings "));
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
                response_body.len(),
                response_body
            );
            stream.write_all(response.as_bytes()).unwrap();
        });
        format!("http://{address}/v1")
    }
}
