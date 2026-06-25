//! PisteClient (PISTE/Judilibre + Legifrance exchanges), token cache/auth, and exchange/outcome shaping.

use crate::*;

#[derive(Debug)]
pub struct PisteClient {
    config: OfficialApiConfig,
    agent: ureq::Agent,
    legifrance_token: Option<CachedToken>,
    retry: RetryPolicy,
}

impl PisteClient {
    pub fn new(config: OfficialApiConfig) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(CONNECT_TIMEOUT)
            .timeout_read(READ_TIMEOUT)
            .build();
        Self {
            config,
            agent,
            legifrance_token: None,
            retry: RetryPolicy::from_env(),
        }
    }

    /// Override the retry/backoff policy (e.g. `RetryPolicy::immediate(0)` to disable retries).
    #[must_use]
    pub fn with_retry_policy(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    pub fn judilibre_search(&self) -> Result<Value, OfficialApiError> {
        self.judilibre_get("/cassation/judilibre/v1.0/search")
    }

    pub fn judilibre_transactional_history(&self) -> Result<Value, OfficialApiError> {
        self.judilibre_get("/cassation/judilibre/v1.0/transactionalhistory")
    }

    pub fn judilibre_get(&self, path: &str) -> Result<Value, OfficialApiError> {
        self.judilibre_get_with_query(path, &[])
    }

    /// Judilibre `/search` with query parameters (e.g. `query`, `operator`, `page_size`). ureq
    /// percent-encodes each value.
    pub fn judilibre_search_params(
        &self,
        params: &[(&str, &str)],
    ) -> Result<Value, OfficialApiError> {
        self.judilibre_get_with_query("/cassation/judilibre/v1.0/search", params)
    }

    /// Judilibre `/decision?id=…` — full decision text, official `zones`, and `visa`.
    pub fn judilibre_decision(
        &self,
        provider_id: &str,
        query: Option<&str>,
    ) -> Result<Value, OfficialApiError> {
        let mut params: Vec<(&str, &str)> =
            vec![("id", provider_id), ("resolve_references", "false")];
        if let Some(query) = query {
            params.push(("query", query));
        }
        self.judilibre_get_with_query("/cassation/judilibre/v1.0/decision", &params)
    }

    /// `production` / `sandbox` — for the durable archive's `api_environment` column.
    #[must_use]
    pub fn api_environment(&self) -> &'static str {
        match self.config.environment {
            PisteEnvironment::Production => "production",
            PisteEnvironment::Sandbox => "sandbox",
        }
    }

    /// Judilibre `/search` as an archivable exchange — captures the raw response (success OR error) so
    /// it can be persisted to `official_api_responses`. Never returns an `Err`: a missing credential or
    /// transport failure becomes an `UpstreamError` exchange with the reason in `error`.
    pub fn judilibre_search_params_exchange(&self, params: &[(&str, &str)]) -> OfficialApiExchange {
        let endpoint = "/cassation/judilibre/v1.0/search".to_owned();
        let url = join_url(&self.config.api_base_url, &endpoint);
        let request_json = json!({ "params": query_params_json(params) });
        let fingerprint = query_fingerprint(params);
        let Some(key_id) = self
            .config
            .judilibre_key_id
            .as_deref()
            .filter(|key| !key.trim().is_empty())
        else {
            return missing_credential_exchange(
                "judilibre",
                endpoint,
                "GET",
                url,
                request_json,
                fingerprint,
            );
        };
        let result = send_with_retry(self.retry, || {
            let mut request = self
                .agent
                .get(&url)
                .set("Accept", "application/json")
                .set("KeyId", key_id);
            for (key, value) in params {
                request = request.query(key, value);
            }
            request.call()
        });
        build_exchange(
            "judilibre",
            endpoint,
            "GET",
            url,
            request_json,
            None,
            fingerprint,
            result,
        )
    }

    /// Judilibre `/decision?id=…` as an archivable exchange (see [`Self::judilibre_search_params_exchange`]).
    pub fn judilibre_decision_exchange(
        &self,
        provider_id: &str,
        query: Option<&str>,
    ) -> OfficialApiExchange {
        let endpoint = "/cassation/judilibre/v1.0/decision".to_owned();
        let url = join_url(&self.config.api_base_url, &endpoint);
        let mut params: Vec<(&str, &str)> =
            vec![("id", provider_id), ("resolve_references", "false")];
        if let Some(query) = query {
            params.push(("query", query));
        }
        let request_json = json!({ "params": query_params_json(&params) });
        let fingerprint = query_fingerprint(&params);
        let Some(key_id) = self
            .config
            .judilibre_key_id
            .as_deref()
            .filter(|key| !key.trim().is_empty())
        else {
            return missing_credential_exchange(
                "judilibre",
                endpoint,
                "GET",
                url,
                request_json,
                fingerprint,
            );
        };
        let result = send_with_retry(self.retry, || {
            let mut request = self
                .agent
                .get(&url)
                .set("Accept", "application/json")
                .set("KeyId", key_id);
            for (key, value) in &params {
                request = request.query(key, value);
            }
            request.call()
        });
        build_exchange(
            "judilibre",
            endpoint,
            "GET",
            url,
            request_json,
            None,
            fingerprint,
            result,
        )
    }

    fn judilibre_get_with_query(
        &self,
        path: &str,
        params: &[(&str, &str)],
    ) -> Result<Value, OfficialApiError> {
        let key_id = self
            .config
            .judilibre_key_id
            .as_deref()
            .filter(|key| !key.trim().is_empty())
            .ok_or(OfficialApiError::MissingCredential {
                names: judilibre_credential_names(self.config.environment),
            })?;
        let url = join_url(&self.config.api_base_url, path);
        let policy = self.retry;
        let response = send_with_retry(policy, || {
            let mut request = self
                .agent
                .get(&url)
                .set("Accept", "application/json")
                .set("KeyId", key_id);
            for (key, value) in params {
                request = request.query(key, value);
            }
            request.call()
        })
        .map_err(official_api_error)?;
        response_json(response)
    }

    pub fn legifrance_search(&mut self, body: &Value) -> Result<Value, OfficialApiError> {
        let token = self.legifrance_bearer_token()?;
        let url = join_url(
            &self.config.api_base_url,
            "/dila/legifrance/lf-engine-app/search",
        );
        let policy = self.retry;
        let response = send_with_retry(policy, || {
            self.agent
                .post(&url)
                .set("Accept", "application/json")
                .set("Content-Type", "application/json")
                .set("Authorization", &format!("Bearer {token}"))
                .send_json(body)
        })
        .map_err(official_api_error)?;
        response_json(response)
    }

    /// Legifrance search as an archivable exchange (see [`Self::judilibre_search_params_exchange`]).
    /// POSTs the body with the OAuth bearer token; captures the raw response on success OR error. A
    /// missing OAuth credential / token failure becomes an `UpstreamError` exchange (never an `Err`).
    pub fn legifrance_search_exchange(&mut self, body: &Value) -> OfficialApiExchange {
        let endpoint = "/dila/legifrance/lf-engine-app/search".to_owned();
        let url = join_url(&self.config.api_base_url, &endpoint);
        let request_body = body.to_string();
        let fingerprint = legifrance_search_fingerprint(&request_body);
        let token = match self.legifrance_bearer_token() {
            Ok(token) => token,
            Err(error) => {
                return OfficialApiExchange {
                    provider: "legifrance",
                    endpoint,
                    http_method: "POST",
                    request_url: url,
                    request_json: body.clone(),
                    request_body: Some(request_body),
                    request_fingerprint: fingerprint,
                    http_status: None,
                    response_body: String::new(),
                    response_json: None,
                    outcome: OfficialApiOutcome::UpstreamError,
                    error: Some(error.to_string()),
                };
            }
        };
        let result = send_with_retry(self.retry, || {
            self.agent
                .post(&url)
                .set("Accept", "application/json")
                .set("Content-Type", "application/json")
                .set("Authorization", &format!("Bearer {token}"))
                .send_json(body)
        });
        build_exchange(
            "legifrance",
            endpoint,
            "POST",
            url,
            body.clone(),
            Some(request_body),
            fingerprint,
            result,
        )
    }

    pub fn legifrance_bearer_token(&mut self) -> Result<String, OfficialApiError> {
        if let Some(token) = self
            .legifrance_token
            .as_ref()
            .filter(|token| token.is_valid())
        {
            return Ok(token.access_token.clone());
        }

        let client_id = self
            .config
            .legifrance_client_id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .ok_or(OfficialApiError::MissingCredential {
                names: legifrance_client_id_credential_names(self.config.environment),
            })?;
        let client_secret = self
            .config
            .legifrance_client_secret
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .ok_or(OfficialApiError::MissingCredential {
                names: legifrance_client_secret_credential_names(self.config.environment),
            })?;
        let url = join_url(&self.config.oauth_base_url, "/api/oauth/token");
        let policy = self.retry;
        let response = send_with_retry(policy, || {
            self.agent.post(&url).send_form(&[
                ("grant_type", "client_credentials"),
                ("scope", "openid"),
                ("client_id", client_id),
                ("client_secret", client_secret),
            ])
        })
        .map_err(official_api_error)?;
        let token_response = response
            .into_json::<TokenResponse>()
            .map_err(|error| OfficialApiError::InvalidResponse(error.to_string()))?;
        if token_response.access_token.trim().is_empty() {
            return Err(OfficialApiError::InvalidResponse(
                "OAuth token response did not include access_token".to_owned(),
            ));
        }
        let now = Instant::now();
        let expires_at = token_response.expires_in.map(|seconds| {
            now.checked_add(Duration::from_secs(seconds).saturating_sub(LEGIFRANCE_TOKEN_SKEW))
                .unwrap_or(now)
        });
        let token = CachedToken {
            access_token: token_response.access_token,
            expires_at,
        };
        let access_token = token.access_token.clone();
        self.legifrance_token = Some(token);
        Ok(access_token)
    }

    #[must_use]
    pub fn config(&self) -> &OfficialApiConfig {
        &self.config
    }
}

/// Transport-level outcome of an archived official-API exchange. The DECISION-level status
/// (not_found / invalid_offsets / unsupported) is determined by the caller after parsing and lives in
/// `decision_zones`; this is purely "did we get a parseable HTTP response".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OfficialApiOutcome {
    /// HTTP success with a JSON-parseable body.
    Ok,
    /// HTTP error (4xx/5xx, incl. 429 after retries) or a transport failure.
    UpstreamError,
    /// HTTP success but the body did not parse as JSON.
    ParseError,
}

impl OfficialApiOutcome {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::UpstreamError => "upstream_error",
            Self::ParseError => "parse_error",
        }
    }
}

/// A captured official-API exchange, preserved on BOTH success and error so the durable archive
/// (`official_api_responses`, migration v16) is complete — including raw error bodies the rich
/// `OfficialApiError` summarization would otherwise truncate or drop.
#[derive(Debug, Clone)]
pub struct OfficialApiExchange {
    pub provider: &'static str,
    pub endpoint: String,
    pub http_method: &'static str,
    pub request_url: String,
    pub request_json: Value,
    pub request_body: Option<String>,
    pub request_fingerprint: String,
    pub http_status: Option<u16>,
    pub response_body: String,
    pub response_json: Option<Value>,
    pub outcome: OfficialApiOutcome,
    pub error: Option<String>,
}

/// Build an exchange envelope from the (post-retry) transport result, consuming the response to capture
/// its raw body. Classifies into `Ok` (parseable JSON) / `ParseError` (success, non-JSON body) /
/// `UpstreamError` (HTTP error or transport failure); a parsed `response_json` is kept whenever the body
/// is valid JSON, even on error.
#[allow(clippy::too_many_arguments)]
fn build_exchange(
    provider: &'static str,
    endpoint: String,
    http_method: &'static str,
    request_url: String,
    request_json: Value,
    request_body: Option<String>,
    request_fingerprint: String,
    result: Result<ureq::Response, ureq::Error>,
) -> OfficialApiExchange {
    let (http_status, response_body, transport_error) = match result {
        Ok(response) => {
            let status = response.status();
            (
                Some(status),
                response.into_string().unwrap_or_default(),
                None,
            )
        }
        Err(ureq::Error::Status(status, response)) => (
            Some(status),
            response.into_string().unwrap_or_default(),
            Some(format!("upstream returned HTTP {status}")),
        ),
        Err(other) => (None, String::new(), Some(other.to_string())),
    };
    let parsed = serde_json::from_str::<Value>(&response_body).ok();
    let (outcome, error) = if let Some(error) = transport_error {
        (OfficialApiOutcome::UpstreamError, Some(error))
    } else if parsed.is_some() {
        (OfficialApiOutcome::Ok, None)
    } else {
        (
            OfficialApiOutcome::ParseError,
            Some("upstream response body was not valid JSON".to_owned()),
        )
    };
    OfficialApiExchange {
        provider,
        endpoint,
        http_method,
        request_url,
        request_json,
        request_body,
        request_fingerprint,
        http_status,
        response_body,
        response_json: parsed,
        outcome,
        error,
    }
}

/// Readable, bounded fingerprint of a GET query (sorted) for grouping re-fetches in the archive.
fn query_fingerprint(params: &[(&str, &str)]) -> String {
    let mut parts: Vec<String> = params
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    parts.sort();
    parts.join("&")
}

/// Stable per-request fingerprint for an archived Legifrance `/search` exchange: a sha256 over the
/// exact serialized request body. Body-shape-agnostic on purpose — the previous version read a now-absent
/// top-level `query` field, so every real-contract (`recherche.champs[*]…`) request collapsed to the same
/// empty `legifrance-search:` fingerprint, destroying the per-row audit/dedup signal.
pub(super) fn legifrance_search_fingerprint(request_body: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(request_body.as_bytes());
    let hex: String = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("legifrance-search:sha256:{hex}")
}

/// GET query params as a JSON object, for the archive's `request_json` column.
fn query_params_json(params: &[(&str, &str)]) -> Value {
    Value::Object(
        params
            .iter()
            .map(|(key, value)| ((*key).to_owned(), Value::String((*value).to_owned())))
            .collect(),
    )
}

/// An exchange that never left the process because a required credential was missing — still archived
/// as an `UpstreamError` so the attempt is durably accounted for.
fn missing_credential_exchange(
    provider: &'static str,
    endpoint: String,
    http_method: &'static str,
    request_url: String,
    request_json: Value,
    request_fingerprint: String,
) -> OfficialApiExchange {
    OfficialApiExchange {
        provider,
        endpoint,
        http_method,
        request_url,
        request_json,
        request_body: None,
        request_fingerprint,
        http_status: None,
        response_body: String::new(),
        response_json: None,
        outcome: OfficialApiOutcome::UpstreamError,
        error: Some(format!("missing {provider} (PISTE) credential")),
    }
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    expires_in: Option<u64>,
}

#[derive(Clone)]
struct CachedToken {
    access_token: String,
    expires_at: Option<Instant>,
}

impl fmt::Debug for CachedToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CachedToken")
            .field("access_token", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

impl CachedToken {
    fn is_valid(&self) -> bool {
        self.expires_at
            .is_none_or(|expires_at| Instant::now() < expires_at)
    }
}

fn join_url(base: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

fn response_json(response: ureq::Response) -> Result<Value, OfficialApiError> {
    response
        .into_json::<Value>()
        .map_err(|error| OfficialApiError::InvalidResponse(error.to_string()))
}
