//! OpenAI-compatible embedding HTTP client + vectors/input-stats + URL classification.

use super::*;

pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

pub(crate) const READ_TIMEOUT: Duration = Duration::from_secs(30);

pub(crate) const NORMALIZED_L2_TOLERANCE: f32 = 0.01;

pub(crate) const INVALID_RESPONSE_BODY_LIMIT: usize = 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BaseUrlClass {
    LocalLoopback,
    Hosted,
    InProcess,
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
    pub(crate) config: EmbeddingConfig,
    pub(crate) agent: ureq::Agent,
    pub(crate) tokenizer: Option<Tokenizer>,
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
pub(crate) struct OpenAiEmbeddingResponse {
    pub(crate) data: Vec<OpenAiEmbeddingData>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct OpenAiEmbeddingErrorResponse {
    pub(crate) error: serde_json::Value,
}

#[derive(Debug, Deserialize)]
pub(crate) struct OpenAiEmbeddingData {
    pub(crate) embedding: Vec<f32>,
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

pub(crate) fn endpoint_error(error: ureq::Error) -> EmbeddingError {
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

pub(crate) fn truncate_response_body(body: &str) -> String {
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
