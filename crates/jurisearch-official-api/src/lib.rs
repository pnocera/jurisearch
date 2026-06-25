use std::{
    env, fmt,
    time::{Duration, Instant},
};

use jurisearch_core::error::{ErrorCode, ErrorObject};
use serde::Deserialize;
use serde_json::{Value, json};
use thiserror::Error;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const READ_TIMEOUT: Duration = Duration::from_secs(30);
const LEGIFRANCE_TOKEN_SKEW: Duration = Duration::from_secs(30);
const UPSTREAM_BODY_LIMIT: usize = 500;
const DEFAULT_MAX_RETRIES: u32 = 3;
const DEFAULT_RETRY_BASE_DELAY: Duration = Duration::from_millis(500);
const DEFAULT_RETRY_MAX_DELAY: Duration = Duration::from_secs(30);
const PROD_JUDILIBRE_CREDENTIALS: &[&str] = &["JURISEARCH_PISTE_JUDILIBRE_KEY_ID", "PISTE_API_KEY"];
const SANDBOX_JUDILIBRE_CREDENTIALS: &[&str] =
    &["JURISEARCH_PISTE_JUDILIBRE_KEY_ID", "PISTE_SANDBOX_API_KEY"];
const PROD_LEGIFRANCE_CLIENT_ID_CREDENTIALS: &[&str] = &[
    "JURISEARCH_PISTE_LEGIFRANCE_CLIENT_ID",
    "PISTE_OAUTH_CLIENT_ID",
];
const SANDBOX_LEGIFRANCE_CLIENT_ID_CREDENTIALS: &[&str] = &[
    "JURISEARCH_PISTE_LEGIFRANCE_CLIENT_ID",
    "PISTE_SANDBOX_OAUTH_CLIENT_ID",
];
const PROD_LEGIFRANCE_CLIENT_SECRET_CREDENTIALS: &[&str] = &[
    "JURISEARCH_PISTE_LEGIFRANCE_CLIENT_SECRET",
    "PISTE_OAUTH_CLIENT_SECRET",
];
const SANDBOX_LEGIFRANCE_CLIENT_SECRET_CREDENTIALS: &[&str] = &[
    "JURISEARCH_PISTE_LEGIFRANCE_CLIENT_SECRET",
    "PISTE_SANDBOX_OAUTH_CLIENT_SECRET",
];

/// Retry/backoff policy for transient upstream failures (HTTP 429 and 5xx). Only safe,
/// read-style PISTE requests are issued through it; transport errors are not retried.
#[derive(Debug, Clone, Copy)]
pub struct RetryPolicy {
    /// Maximum number of retries AFTER the initial attempt.
    pub max_retries: u32,
    /// Base delay for exponential backoff (`base * 2^attempt`).
    pub base_delay: Duration,
    /// Upper bound on any single backoff wait; also caps a `Retry-After` value.
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_retries: DEFAULT_MAX_RETRIES,
            base_delay: DEFAULT_RETRY_BASE_DELAY,
            max_delay: DEFAULT_RETRY_MAX_DELAY,
        }
    }
}

impl RetryPolicy {
    /// A policy that retries `max_retries` times without sleeping — for deterministic tests.
    #[must_use]
    pub fn immediate(max_retries: u32) -> Self {
        Self {
            max_retries,
            base_delay: Duration::ZERO,
            max_delay: Duration::ZERO,
        }
    }

    /// Default policy, with `max_retries` overridable via `JURISEARCH_PISTE_MAX_RETRIES`
    /// (e.g. set to `0` to disable retries deterministically in tests/probes).
    #[must_use]
    pub fn from_env() -> Self {
        let mut policy = Self::default();
        if let Ok(value) = env::var("JURISEARCH_PISTE_MAX_RETRIES") {
            if let Ok(max_retries) = value.trim().parse::<u32>() {
                policy.max_retries = max_retries;
            }
        }
        policy
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PisteEnvironment {
    Production,
    Sandbox,
}

impl PisteEnvironment {
    #[must_use]
    pub fn api_base_url(self) -> &'static str {
        match self {
            Self::Production => "https://api.piste.gouv.fr",
            Self::Sandbox => "https://sandbox-api.piste.gouv.fr",
        }
    }

    #[must_use]
    pub fn oauth_base_url(self) -> &'static str {
        match self {
            Self::Production => "https://oauth.piste.gouv.fr",
            Self::Sandbox => "https://sandbox-oauth.piste.gouv.fr",
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct OfficialApiConfig {
    pub environment: PisteEnvironment,
    pub api_base_url: String,
    pub oauth_base_url: String,
    pub judilibre_key_id: Option<String>,
    pub legifrance_client_id: Option<String>,
    pub legifrance_client_secret: Option<String>,
}

impl fmt::Debug for OfficialApiConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OfficialApiConfig")
            .field("environment", &self.environment)
            .field("api_base_url", &self.api_base_url)
            .field("oauth_base_url", &self.oauth_base_url)
            .field(
                "judilibre_key_id",
                &self.judilibre_key_id.as_ref().map(|_| "<redacted>"),
            )
            .field(
                "legifrance_client_id",
                &self.legifrance_client_id.as_ref().map(|_| "<redacted>"),
            )
            .field(
                "legifrance_client_secret",
                &self.legifrance_client_secret.as_ref().map(|_| "<redacted>"),
            )
            .finish()
    }
}

impl OfficialApiConfig {
    #[must_use]
    pub fn production() -> Self {
        Self::for_environment(PisteEnvironment::Production)
    }

    #[must_use]
    pub fn sandbox() -> Self {
        Self::for_environment(PisteEnvironment::Sandbox)
    }

    #[must_use]
    pub fn for_environment(environment: PisteEnvironment) -> Self {
        Self {
            environment,
            api_base_url: environment.api_base_url().to_owned(),
            oauth_base_url: environment.oauth_base_url().to_owned(),
            judilibre_key_id: None,
            legifrance_client_id: None,
            legifrance_client_secret: None,
        }
    }

    #[must_use]
    pub fn from_env() -> Self {
        let environment = match env::var("JURISEARCH_PISTE_ENV")
            .or_else(|_| env::var("PISTE_ENV"))
            .unwrap_or_else(|_| "production".to_owned())
            .to_ascii_lowercase()
            .as_str()
        {
            "sandbox" => PisteEnvironment::Sandbox,
            _ => PisteEnvironment::Production,
        };
        let mut config = Self::for_environment(environment);
        config.api_base_url =
            nonempty_env_or_default("JURISEARCH_PISTE_API_BASE_URL", config.api_base_url);
        config.oauth_base_url =
            nonempty_env_or_default("JURISEARCH_PISTE_OAUTH_BASE_URL", config.oauth_base_url);
        config.judilibre_key_id = first_nonempty_env(if environment == PisteEnvironment::Sandbox {
            SANDBOX_JUDILIBRE_CREDENTIALS
        } else {
            PROD_JUDILIBRE_CREDENTIALS
        });
        config.legifrance_client_id =
            first_nonempty_env(if environment == PisteEnvironment::Sandbox {
                SANDBOX_LEGIFRANCE_CLIENT_ID_CREDENTIALS
            } else {
                PROD_LEGIFRANCE_CLIENT_ID_CREDENTIALS
            });
        config.legifrance_client_secret =
            first_nonempty_env(if environment == PisteEnvironment::Sandbox {
                SANDBOX_LEGIFRANCE_CLIENT_SECRET_CREDENTIALS
            } else {
                PROD_LEGIFRANCE_CLIENT_SECRET_CREDENTIALS
            });
        config
    }
}

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
        let mut params: Vec<(&str, &str)> = vec![("id", provider_id), ("resolve_references", "false")];
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
            return missing_credential_exchange("judilibre", endpoint, "GET", url, request_json, fingerprint);
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
        build_exchange("judilibre", endpoint, "GET", url, request_json, None, fingerprint, result)
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
            return missing_credential_exchange("judilibre", endpoint, "GET", url, request_json, fingerprint);
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
        build_exchange("judilibre", endpoint, "GET", url, request_json, None, fingerprint, result)
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

#[derive(Debug, Error)]
pub enum OfficialApiError {
    #[error("missing official API credential; checked {names:?}")]
    MissingCredential { names: &'static [&'static str] },
    #[error("official API rate limited the request with status 429")]
    RateLimited {
        retry_after: Option<String>,
        body: String,
    },
    #[error("official API returned HTTP status {status}: {body}")]
    UpstreamStatus { status: u16, body: String },
    #[error("official API transport failed: {0}")]
    Transport(String),
    #[error("official API response was invalid: {0}")]
    InvalidResponse(String),
}

impl OfficialApiError {
    #[must_use]
    pub fn to_error_object(&self) -> ErrorObject {
        match self {
            Self::MissingCredential { names } => ErrorObject {
                code: ErrorCode::DependencyUnavailable,
                message: self.to_string(),
                suggestions: vec![format!(
                    "Set one of {} in the environment or configure an OS keyring entry.",
                    names.join(", ")
                )],
            },
            Self::RateLimited { retry_after, .. } => ErrorObject {
                code: ErrorCode::Upstream,
                message: match retry_after {
                    Some(retry_after) => {
                        format!("official API rate limited the request; retry after {retry_after}")
                    }
                    None => "official API rate limited the request".to_owned(),
                },
                suggestions: vec!["Back off and retry later; prefer bulk dumps for full builds.".into()],
            },
            Self::UpstreamStatus { .. } | Self::Transport(_) | Self::InvalidResponse(_) => {
                ErrorObject {
                    code: ErrorCode::Upstream,
                    message: self.to_string(),
                    suggestions: vec![
                        "Check official API availability, credentials, subscription, and rate limits."
                            .into(),
                    ],
                }
            }
        }
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
            (Some(status), response.into_string().unwrap_or_default(), None)
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
fn legifrance_search_fingerprint(request_body: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(request_body.as_bytes());
    let hex: String = hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect();
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

fn first_nonempty_env(names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|name| env::var(name).ok().filter(|value| !value.trim().is_empty()))
}

fn nonempty_env_or_default(name: &str, default: String) -> String {
    env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(default)
}

fn judilibre_credential_names(environment: PisteEnvironment) -> &'static [&'static str] {
    match environment {
        PisteEnvironment::Production => PROD_JUDILIBRE_CREDENTIALS,
        PisteEnvironment::Sandbox => SANDBOX_JUDILIBRE_CREDENTIALS,
    }
}

fn legifrance_client_id_credential_names(environment: PisteEnvironment) -> &'static [&'static str] {
    match environment {
        PisteEnvironment::Production => PROD_LEGIFRANCE_CLIENT_ID_CREDENTIALS,
        PisteEnvironment::Sandbox => SANDBOX_LEGIFRANCE_CLIENT_ID_CREDENTIALS,
    }
}

fn legifrance_client_secret_credential_names(
    environment: PisteEnvironment,
) -> &'static [&'static str] {
    match environment {
        PisteEnvironment::Production => PROD_LEGIFRANCE_CLIENT_SECRET_CREDENTIALS,
        PisteEnvironment::Sandbox => SANDBOX_LEGIFRANCE_CLIENT_SECRET_CREDENTIALS,
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

fn official_api_error(error: ureq::Error) -> OfficialApiError {
    match error {
        ureq::Error::Status(429, response) => {
            let retry_after = response.header("Retry-After").map(str::to_owned);
            let body = response.into_string().unwrap_or_default();
            OfficialApiError::RateLimited { retry_after, body }
        }
        ureq::Error::Status(status, response) => {
            let body = truncated_body(response.into_string().unwrap_or_default());
            OfficialApiError::UpstreamStatus { status, body }
        }
        other => OfficialApiError::Transport(other.to_string()),
    }
}

fn truncated_body(body: String) -> String {
    let body = body.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut truncated = body.chars().take(UPSTREAM_BODY_LIMIT).collect::<String>();
    if body.chars().count() > UPSTREAM_BODY_LIMIT {
        truncated.push_str("...");
    }
    truncated
}

/// Send a request, retrying transient upstream failures (HTTP 429 and 5xx) per `policy`.
/// The `send` closure rebuilds and issues the request on each attempt (ureq builders are
/// single-use). Transport errors and non-retryable statuses (e.g. 4xx) are returned immediately.
fn send_with_retry<F>(policy: RetryPolicy, mut send: F) -> Result<ureq::Response, ureq::Error>
where
    F: FnMut() -> Result<ureq::Response, ureq::Error>,
{
    let mut attempt: u32 = 0;
    loop {
        match send() {
            Ok(response) => return Ok(response),
            Err(error) => {
                if attempt >= policy.max_retries || !is_retryable_status(&error) {
                    return Err(error);
                }
                let delay = retry_delay(retry_after_from_error(&error), attempt, policy);
                if !delay.is_zero() {
                    std::thread::sleep(delay);
                }
                attempt += 1;
            }
        }
    }
}

fn is_retryable_status(error: &ureq::Error) -> bool {
    matches!(error, ureq::Error::Status(code, _) if *code == 429 || (500..=599).contains(code))
}

fn retry_after_from_error(error: &ureq::Error) -> Option<Duration> {
    match error {
        // `Retry-After` is upstream-directed for any retryable status (429 and 5xx both use it).
        ureq::Error::Status(code, response) if *code == 429 || (500..=599).contains(code) => {
            response
                .header("Retry-After")
                .and_then(parse_retry_after_seconds)
        }
        _ => None,
    }
}

/// Backoff for a given attempt (0-based): a `Retry-After` value wins (capped by `max_delay`),
/// otherwise exponential `base_delay * 2^attempt`, capped by `max_delay`.
fn retry_delay(retry_after: Option<Duration>, attempt: u32, policy: RetryPolicy) -> Duration {
    if let Some(after) = retry_after {
        return after.min(policy.max_delay);
    }
    let factor = 1u32.checked_shl(attempt).unwrap_or(u32::MAX);
    policy
        .base_delay
        .checked_mul(factor)
        .unwrap_or(policy.max_delay)
        .min(policy.max_delay)
}

fn parse_retry_after_seconds(value: &str) -> Option<Duration> {
    value.trim().parse::<u64>().ok().map(Duration::from_secs)
}

#[cfg(test)]
mod tests;
