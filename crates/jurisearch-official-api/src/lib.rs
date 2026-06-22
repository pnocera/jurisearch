use std::{
    env, fmt,
    time::{Duration, Instant},
};

use jurisearch_core::error::{ErrorCode, ErrorObject};
use serde::Deserialize;
use serde_json::Value;
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
            self.agent
                .get(&url)
                .set("Accept", "application/json")
                .set("KeyId", key_id)
                .call()
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
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::{Mutex, MutexGuard},
        thread,
    };

    use serde_json::json;

    use super::{OfficialApiConfig, OfficialApiError, PisteClient, PisteEnvironment, RetryPolicy};
    use jurisearch_core::error::ErrorCode;
    use std::time::Duration;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn config_redacts_secrets_in_debug_output() {
        let mut config = OfficialApiConfig::production();
        config.judilibre_key_id = Some("secret-key".to_owned());
        config.legifrance_client_id = Some("client-id".to_owned());
        config.legifrance_client_secret = Some("client-secret".to_owned());

        let debug = format!("{config:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("secret-key"));
        assert!(!debug.contains("client-id"));
        assert!(!debug.contains("client-secret"));
    }

    #[test]
    fn judilibre_uses_key_id_header() {
        let base_url = spawn_server(1, |request| {
            assert!(request.starts_with("GET /cassation/judilibre/v1.0/search "));
            assert!(request.contains("\r\nKeyId: test-key\r\n"));
            ok_json(r#"{"total":1}"#)
        });
        let mut config = OfficialApiConfig::production();
        config.api_base_url = base_url;
        config.judilibre_key_id = Some("test-key".to_owned());
        let client = PisteClient::new(config);

        let response = client.judilibre_search().unwrap();
        assert_eq!(response["total"], 1);
    }

    #[test]
    fn judilibre_transactional_history_uses_expected_path() {
        let base_url = spawn_server(1, |request| {
            assert!(request.starts_with("GET /cassation/judilibre/v1.0/transactionalhistory "));
            assert!(request.contains("\r\nKeyId: test-key\r\n"));
            ok_json(r#"{"events":[]}"#)
        });
        let mut config = OfficialApiConfig::production();
        config.api_base_url = base_url;
        config.judilibre_key_id = Some("test-key".to_owned());
        let client = PisteClient::new(config);

        let response = client.judilibre_transactional_history().unwrap();
        assert_eq!(response["events"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn legifrance_fetches_and_reuses_bearer_token() {
        let base_url = spawn_server(3, |request| {
            if request.starts_with("POST /api/oauth/token ") {
                assert!(request.contains("grant_type=client_credentials"));
                assert!(request.contains("scope=openid"));
                assert!(request.contains("client_id=client-id"));
                assert!(request.contains("client_secret=client-secret"));
                ok_json(r#"{"access_token":"token-123","expires_in":3600}"#)
            } else {
                assert!(request.starts_with("POST /dila/legifrance/lf-engine-app/search "));
                assert!(request.contains("\r\nAuthorization: Bearer token-123\r\n"));
                assert!(request.contains(r#""query":"responsabilite""#));
                ok_json(r#"{"results":[]}"#)
            }
        });
        let mut config = OfficialApiConfig::sandbox();
        config.api_base_url = base_url.clone();
        config.oauth_base_url = base_url;
        config.legifrance_client_id = Some("client-id".to_owned());
        config.legifrance_client_secret = Some("client-secret".to_owned());
        let mut client = PisteClient::new(config);

        let response = client
            .legifrance_search(&json!({ "query": "responsabilite" }))
            .unwrap();
        assert_eq!(response["results"].as_array().unwrap().len(), 0);
        let response = client
            .legifrance_search(&json!({ "query": "responsabilite" }))
            .unwrap();
        assert_eq!(response["results"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn legifrance_refetches_short_lived_token_after_skew() {
        let mut token_count = 0;
        let base_url = spawn_server(4, move |request| {
            if request.starts_with("POST /api/oauth/token ") {
                token_count += 1;
                ok_json(&format!(
                    r#"{{"access_token":"token-{token_count}","expires_in":1}}"#
                ))
            } else {
                assert!(request.starts_with("POST /dila/legifrance/lf-engine-app/search "));
                assert!(request.contains(&format!(
                    "\r\nAuthorization: Bearer token-{token_count}\r\n"
                )));
                ok_json(r#"{"results":[]}"#)
            }
        });
        let mut config = OfficialApiConfig::sandbox();
        config.api_base_url = base_url.clone();
        config.oauth_base_url = base_url;
        config.legifrance_client_id = Some("client-id".to_owned());
        config.legifrance_client_secret = Some("client-secret".to_owned());
        let mut client = PisteClient::new(config);

        client
            .legifrance_search(&json!({ "query": "responsabilite" }))
            .unwrap();
        client
            .legifrance_search(&json!({ "query": "responsabilite" }))
            .unwrap();
    }

    #[test]
    fn rate_limit_maps_to_upstream_error_object() {
        let base_url = spawn_server(1, |_request| {
            "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 2\r\nContent-Length: 17\r\n\r\nrate limited body"
                .to_owned()
        });
        let mut config = OfficialApiConfig::production();
        config.api_base_url = base_url;
        config.judilibre_key_id = Some("test-key".to_owned());
        // No-retry policy: this test asserts 429 error MAPPING against a single-request server.
        let client = PisteClient::new(config).with_retry_policy(RetryPolicy::immediate(0));

        let error = client.judilibre_search().unwrap_err();
        assert!(matches!(
            error,
            OfficialApiError::RateLimited {
                retry_after: Some(ref retry_after),
                ..
            } if retry_after == "2"
        ));
        assert_eq!(error.to_error_object().code, ErrorCode::Upstream);
    }

    #[test]
    fn non_429_upstream_status_is_truncated_in_error_object() {
        let long_body = format!("{}{}", "x".repeat(700), "\nwith whitespace");
        let response = format!(
            "HTTP/1.1 500 Internal Server Error\r\nContent-Length: {}\r\n\r\n{}",
            long_body.len(),
            long_body
        );
        let base_url = spawn_server(1, move |_request| response.clone());
        let mut config = OfficialApiConfig::production();
        config.api_base_url = base_url;
        config.judilibre_key_id = Some("test-key".to_owned());
        // No-retry policy: this test asserts 5xx error MAPPING/truncation against a single-request server.
        let client = PisteClient::new(config).with_retry_policy(RetryPolicy::immediate(0));

        let error = client.judilibre_search().unwrap_err();
        assert!(matches!(
            error,
            OfficialApiError::UpstreamStatus { status: 500, .. }
        ));
        let object = error.to_error_object();
        assert_eq!(object.code, ErrorCode::Upstream);
        assert!(object.message.len() < 620);
        assert!(object.message.ends_with("..."));
    }

    #[test]
    fn retries_429_then_succeeds() {
        let mut call = 0;
        let base_url = spawn_server(2, move |_request| {
            call += 1;
            if call == 1 {
                "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 1\r\nConnection: close\r\nContent-Length: 7\r\n\r\nbackoff"
                    .to_owned()
            } else {
                ok_json(r#"{"total":7}"#)
            }
        });
        let mut config = OfficialApiConfig::production();
        config.api_base_url = base_url;
        config.judilibre_key_id = Some("test-key".to_owned());
        let client = PisteClient::new(config).with_retry_policy(RetryPolicy::immediate(3));

        let response = client.judilibre_search().unwrap();
        assert_eq!(response["total"], 7);
    }

    #[test]
    fn retries_5xx_then_succeeds() {
        let mut call = 0;
        let base_url = spawn_server(2, move |_request| {
            call += 1;
            if call == 1 {
                "HTTP/1.1 503 Service Unavailable\r\nConnection: close\r\nContent-Length: 4\r\n\r\nbusy"
                    .to_owned()
            } else {
                ok_json(r#"{"total":3}"#)
            }
        });
        let mut config = OfficialApiConfig::production();
        config.api_base_url = base_url;
        config.judilibre_key_id = Some("test-key".to_owned());
        let client = PisteClient::new(config).with_retry_policy(RetryPolicy::immediate(2));

        let response = client.judilibre_search().unwrap();
        assert_eq!(response["total"], 3);
    }

    #[test]
    fn exhausts_retries_and_maps_rate_limit() {
        // 1 initial attempt + 2 retries = 3 requests, all 429.
        let base_url = spawn_server(3, |_request| {
            "HTTP/1.1 429 Too Many Requests\r\nConnection: close\r\nContent-Length: 7\r\n\r\nbackoff"
                .to_owned()
        });
        let mut config = OfficialApiConfig::production();
        config.api_base_url = base_url;
        config.judilibre_key_id = Some("test-key".to_owned());
        let client = PisteClient::new(config).with_retry_policy(RetryPolicy::immediate(2));

        let error = client.judilibre_search().unwrap_err();
        assert!(matches!(error, OfficialApiError::RateLimited { .. }));
    }

    #[test]
    fn does_not_retry_non_retryable_status() {
        // Single-request server: if the client retried a 404 it would hit a closed listener and
        // surface a transport error instead of the mapped 404.
        let base_url = spawn_server(1, |_request| {
            "HTTP/1.1 404 Not Found\r\nConnection: close\r\nContent-Length: 9\r\n\r\nnot found".to_owned()
        });
        let mut config = OfficialApiConfig::production();
        config.api_base_url = base_url;
        config.judilibre_key_id = Some("test-key".to_owned());
        let client = PisteClient::new(config).with_retry_policy(RetryPolicy::immediate(3));

        let error = client.judilibre_search().unwrap_err();
        assert!(matches!(
            error,
            OfficialApiError::UpstreamStatus { status: 404, .. }
        ));
    }

    #[test]
    fn retry_delay_honors_retry_after_and_backs_off_exponentially() {
        let policy = RetryPolicy {
            max_retries: 5,
            base_delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(30),
        };
        // Retry-After wins, capped by max_delay.
        assert_eq!(
            super::retry_delay(Some(Duration::from_secs(5)), 0, policy),
            Duration::from_secs(5)
        );
        assert_eq!(
            super::retry_delay(Some(Duration::from_secs(100)), 0, policy),
            Duration::from_secs(30)
        );
        // Exponential backoff base * 2^attempt, capped by max_delay.
        assert_eq!(super::retry_delay(None, 0, policy), Duration::from_secs(1));
        assert_eq!(super::retry_delay(None, 1, policy), Duration::from_secs(2));
        assert_eq!(super::retry_delay(None, 2, policy), Duration::from_secs(4));
        assert_eq!(super::retry_delay(None, 10, policy), Duration::from_secs(30));
    }

    #[test]
    fn retry_after_from_error_reads_header_for_429_and_5xx() {
        let parse = |raw: &str| raw.parse::<ureq::Response>().unwrap();

        // Both 429 and 5xx carry Retry-After; both must be honored.
        let r429 = parse("HTTP/1.1 429 Too Many Requests\r\nRetry-After: 12\r\nContent-Length: 0\r\n\r\n");
        assert_eq!(
            super::retry_after_from_error(&ureq::Error::Status(429, r429)),
            Some(Duration::from_secs(12))
        );
        let r503 = parse("HTTP/1.1 503 Service Unavailable\r\nRetry-After: 30\r\nContent-Length: 0\r\n\r\n");
        assert_eq!(
            super::retry_after_from_error(&ureq::Error::Status(503, r503)),
            Some(Duration::from_secs(30))
        );

        // Retryable status without the header → fall back to exponential (None here).
        let r500 = parse("HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\n\r\n");
        assert_eq!(
            super::retry_after_from_error(&ureq::Error::Status(500, r500)),
            None
        );

        // Non-retryable status → ignore any Retry-After.
        let r404 = parse("HTTP/1.1 404 Not Found\r\nRetry-After: 5\r\nContent-Length: 0\r\n\r\n");
        assert_eq!(
            super::retry_after_from_error(&ureq::Error::Status(404, r404)),
            None
        );
    }

    #[test]
    fn retry_policy_from_env_reads_max_retries() {
        let _lock = ENV_LOCK.lock().unwrap();
        let previous = std::env::var("JURISEARCH_PISTE_MAX_RETRIES").ok();

        set_env_var("JURISEARCH_PISTE_MAX_RETRIES", Some("0"));
        assert_eq!(RetryPolicy::from_env().max_retries, 0);
        set_env_var("JURISEARCH_PISTE_MAX_RETRIES", Some("7"));
        assert_eq!(RetryPolicy::from_env().max_retries, 7);
        // Garbage falls back to the default.
        set_env_var("JURISEARCH_PISTE_MAX_RETRIES", Some("not-a-number"));
        assert_eq!(
            RetryPolicy::from_env().max_retries,
            super::DEFAULT_MAX_RETRIES
        );
        set_env_var("JURISEARCH_PISTE_MAX_RETRIES", None);
        assert_eq!(
            RetryPolicy::from_env().max_retries,
            super::DEFAULT_MAX_RETRIES
        );

        set_env_var("JURISEARCH_PISTE_MAX_RETRIES", previous.as_deref());
    }

    #[test]
    fn missing_credentials_are_dependency_errors() {
        let client = PisteClient::new(OfficialApiConfig::for_environment(
            PisteEnvironment::Sandbox,
        ));

        let error = client.judilibre_search().unwrap_err();
        let OfficialApiError::MissingCredential { names } = &error else {
            panic!("expected missing credential error, got {error:?}");
        };
        assert!(names.contains(&"PISTE_SANDBOX_API_KEY"));
        let object = error.to_error_object();
        assert_eq!(object.code, ErrorCode::DependencyUnavailable);
        assert!(object.suggestions[0].contains("PISTE_SANDBOX_API_KEY"));
    }

    #[test]
    fn missing_legifrance_credentials_are_dependency_errors() {
        let mut client = PisteClient::new(OfficialApiConfig::for_environment(
            PisteEnvironment::Sandbox,
        ));

        let error = client
            .legifrance_search(&json!({ "query": "test" }))
            .unwrap_err();
        let OfficialApiError::MissingCredential { names } = &error else {
            panic!("expected missing credential error, got {error:?}");
        };
        assert!(names.contains(&"PISTE_SANDBOX_OAUTH_CLIENT_ID"));
        let object = error.to_error_object();
        assert_eq!(object.code, ErrorCode::DependencyUnavailable);
        assert!(object.suggestions[0].contains("PISTE_SANDBOX_OAUTH_CLIENT_ID"));
    }

    #[test]
    fn from_env_uses_sandbox_fallbacks_and_ignores_empty_base_overrides() {
        let _env = EnvGuard::new(&[
            ("JURISEARCH_PISTE_ENV", Some("sandbox")),
            ("PISTE_ENV", Some("production")),
            ("JURISEARCH_PISTE_API_BASE_URL", Some("")),
            ("JURISEARCH_PISTE_OAUTH_BASE_URL", Some("")),
            ("JURISEARCH_PISTE_JUDILIBRE_KEY_ID", Some("unified-key")),
            ("PISTE_API_KEY", Some("prod-key")),
            ("PISTE_SANDBOX_API_KEY", Some("sandbox-key")),
            ("JURISEARCH_PISTE_LEGIFRANCE_CLIENT_ID", Some("unified-id")),
            ("PISTE_OAUTH_CLIENT_ID", Some("prod-client-id")),
            ("PISTE_SANDBOX_OAUTH_CLIENT_ID", Some("sandbox-client-id")),
            ("JURISEARCH_PISTE_LEGIFRANCE_CLIENT_SECRET", None),
            ("PISTE_OAUTH_CLIENT_SECRET", Some("prod-client-secret")),
            (
                "PISTE_SANDBOX_OAUTH_CLIENT_SECRET",
                Some("sandbox-client-secret"),
            ),
        ]);

        let config = OfficialApiConfig::from_env();

        assert_eq!(config.environment, PisteEnvironment::Sandbox);
        assert_eq!(
            config.api_base_url,
            PisteEnvironment::Sandbox.api_base_url()
        );
        assert_eq!(
            config.oauth_base_url,
            PisteEnvironment::Sandbox.oauth_base_url()
        );
        assert_eq!(config.judilibre_key_id.as_deref(), Some("unified-key"));
        assert_eq!(config.legifrance_client_id.as_deref(), Some("unified-id"));
        assert_eq!(
            config.legifrance_client_secret.as_deref(),
            Some("sandbox-client-secret")
        );
    }

    struct EnvGuard {
        _lock: MutexGuard<'static, ()>,
        previous: Vec<(&'static str, Option<String>)>,
    }

    impl EnvGuard {
        fn new(vars: &[(&'static str, Option<&str>)]) -> Self {
            let lock = ENV_LOCK.lock().unwrap();
            let previous = vars
                .iter()
                .map(|(name, _)| (*name, std::env::var(name).ok()))
                .collect::<Vec<_>>();
            for (name, value) in vars {
                set_env_var(name, *value);
            }
            Self {
                _lock: lock,
                previous,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (name, value) in self.previous.iter().rev() {
                set_env_var(name, value.as_deref());
            }
        }
    }

    fn set_env_var(name: &str, value: Option<&str>) {
        // SAFETY: Environment-mutating tests take ENV_LOCK for the full mutation/read/restore
        // window, and this crate's other tests do not access these PISTE variables.
        unsafe {
            match value {
                Some(value) => std::env::set_var(name, value),
                None => std::env::remove_var(name),
            }
        }
    }

    fn spawn_server(
        request_count: usize,
        mut handler: impl FnMut(String) -> String + Send + 'static,
    ) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        thread::spawn(move || {
            for _ in 0..request_count {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_http_request(&mut stream);
                let response = handler(request);
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        format!("http://{address}")
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

    fn ok_json(body: &str) -> String {
        // `Connection: close` keeps the hand-rolled one-request-per-accept server deterministic:
        // ureq never pools a connection the server is about to close (avoids broken-pipe reuse).
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        )
    }
}
