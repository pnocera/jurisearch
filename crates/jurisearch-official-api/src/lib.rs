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
            env::var("JURISEARCH_PISTE_API_BASE_URL").unwrap_or(config.api_base_url);
        config.oauth_base_url =
            env::var("JURISEARCH_PISTE_OAUTH_BASE_URL").unwrap_or(config.oauth_base_url);
        config.judilibre_key_id = first_nonempty_env(if environment == PisteEnvironment::Sandbox {
            &["JURISEARCH_PISTE_JUDILIBRE_KEY_ID", "PISTE_SANDBOX_API_KEY"]
        } else {
            &["JURISEARCH_PISTE_JUDILIBRE_KEY_ID", "PISTE_API_KEY"]
        });
        config.legifrance_client_id =
            first_nonempty_env(if environment == PisteEnvironment::Sandbox {
                &[
                    "JURISEARCH_PISTE_LEGIFRANCE_CLIENT_ID",
                    "PISTE_SANDBOX_OAUTH_CLIENT_ID",
                ]
            } else {
                &[
                    "JURISEARCH_PISTE_LEGIFRANCE_CLIENT_ID",
                    "PISTE_OAUTH_CLIENT_ID",
                ]
            });
        config.legifrance_client_secret =
            first_nonempty_env(if environment == PisteEnvironment::Sandbox {
                &[
                    "JURISEARCH_PISTE_LEGIFRANCE_CLIENT_SECRET",
                    "PISTE_SANDBOX_OAUTH_CLIENT_SECRET",
                ]
            } else {
                &[
                    "JURISEARCH_PISTE_LEGIFRANCE_CLIENT_SECRET",
                    "PISTE_OAUTH_CLIENT_SECRET",
                ]
            });
        config
    }
}

#[derive(Debug)]
pub struct PisteClient {
    config: OfficialApiConfig,
    agent: ureq::Agent,
    legifrance_token: Option<CachedToken>,
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
        }
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
                name: "PISTE_API_KEY",
            })?;
        let url = join_url(&self.config.api_base_url, path);
        let response = self
            .agent
            .get(&url)
            .set("Accept", "application/json")
            .set("KeyId", key_id)
            .call()
            .map_err(official_api_error)?;
        response_json(response)
    }

    pub fn legifrance_search(&mut self, body: &Value) -> Result<Value, OfficialApiError> {
        let token = self.legifrance_bearer_token()?;
        let url = join_url(
            &self.config.api_base_url,
            "/dila/legifrance/lf-engine-app/search",
        );
        let response = self
            .agent
            .post(&url)
            .set("Accept", "application/json")
            .set("Content-Type", "application/json")
            .set("Authorization", &format!("Bearer {token}"))
            .send_json(body)
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
                name: "PISTE_OAUTH_CLIENT_ID",
            })?;
        let client_secret = self
            .config
            .legifrance_client_secret
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .ok_or(OfficialApiError::MissingCredential {
                name: "PISTE_OAUTH_CLIENT_SECRET",
            })?;
        let url = join_url(&self.config.oauth_base_url, "/api/oauth/token");
        let response = self
            .agent
            .post(&url)
            .send_form(&[
                ("grant_type", "client_credentials"),
                ("scope", "openid"),
                ("client_id", client_id),
                ("client_secret", client_secret),
            ])
            .map_err(official_api_error)?;
        let token_response = response
            .into_json::<TokenResponse>()
            .map_err(|error| OfficialApiError::InvalidResponse(error.to_string()))?;
        if token_response.access_token.trim().is_empty() {
            return Err(OfficialApiError::InvalidResponse(
                "OAuth token response did not include access_token".to_owned(),
            ));
        }
        let expires_at = token_response.expires_in.map(|seconds| {
            Instant::now() + Duration::from_secs(seconds).saturating_sub(LEGIFRANCE_TOKEN_SKEW)
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
    #[error("missing official API credential `{name}`")]
    MissingCredential { name: &'static str },
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
            Self::MissingCredential { name } => ErrorObject {
                code: ErrorCode::DependencyUnavailable,
                message: self.to_string(),
                suggestions: vec![format!(
                    "Set `{name}` in the environment or configure an OS keyring entry."
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
            let body = response.into_string().unwrap_or_default();
            OfficialApiError::UpstreamStatus { status, body }
        }
        other => OfficialApiError::Transport(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::TcpListener,
        thread,
    };

    use serde_json::json;

    use super::{OfficialApiConfig, OfficialApiError, PisteClient, PisteEnvironment};
    use jurisearch_core::error::ErrorCode;

    #[test]
    fn config_redacts_secrets_in_debug_output() {
        let mut config = OfficialApiConfig::production();
        config.judilibre_key_id = Some("secret-key".to_owned());
        config.legifrance_client_id = Some("client-id".to_owned());
        config.legifrance_client_secret = Some("client-secret".to_owned());

        let debug = format!("{config:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("secret-key"));
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
    fn rate_limit_maps_to_upstream_error_object() {
        let base_url = spawn_server(1, |_request| {
            "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 2\r\nContent-Length: 17\r\n\r\nrate limited body"
                .to_owned()
        });
        let mut config = OfficialApiConfig::production();
        config.api_base_url = base_url;
        config.judilibre_key_id = Some("test-key".to_owned());
        let client = PisteClient::new(config);

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
    fn missing_credentials_are_dependency_errors() {
        let client = PisteClient::new(OfficialApiConfig::for_environment(
            PisteEnvironment::Production,
        ));

        let error = client.judilibre_search().unwrap_err();
        assert!(matches!(error, OfficialApiError::MissingCredential { .. }));
        assert_eq!(
            error.to_error_object().code,
            ErrorCode::DependencyUnavailable
        );
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
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            body.len(),
            body
        )
    }
}
