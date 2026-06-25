//! Unit tests for the official-API client (config/token/retry/error), moved out of lib.rs verbatim.

use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::{Mutex, MutexGuard},
    thread,
};

use serde_json::json;

use super::{OfficialApiConfig, OfficialApiError, PisteClient, PisteEnvironment, RetryPolicy};
use crate::client::legifrance_search_fingerprint;
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
        "HTTP/1.1 404 Not Found\r\nConnection: close\r\nContent-Length: 9\r\n\r\nnot found"
            .to_owned()
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
    assert_eq!(
        super::retry_delay(None, 10, policy),
        Duration::from_secs(30)
    );
}

#[test]
fn retry_after_from_error_reads_header_for_429_and_5xx() {
    let parse = |raw: &str| raw.parse::<ureq::Response>().unwrap();

    // Both 429 and 5xx carry Retry-After; both must be honored.
    let r429 =
        parse("HTTP/1.1 429 Too Many Requests\r\nRetry-After: 12\r\nContent-Length: 0\r\n\r\n");
    assert_eq!(
        super::retry_after_from_error(&ureq::Error::Status(429, r429)),
        Some(Duration::from_secs(12))
    );
    let r503 =
        parse("HTTP/1.1 503 Service Unavailable\r\nRetry-After: 30\r\nContent-Length: 0\r\n\r\n");
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
fn legifrance_search_exchange_archives_missing_credential_as_upstream_error() {
    // Slice-2 review fix: a missing OAuth credential must NOT short-circuit; the exchange must be a
    // durable UpstreamError (no panic, no network) so the caller can archive every attempt uniformly.
    let mut client = PisteClient::new(OfficialApiConfig::for_environment(
        PisteEnvironment::Sandbox,
    ));
    // Real-contract body (no top-level `query` field) — exactly what the enrichment now sends.
    let body = json!({
        "fond": "CODE_DATE",
        "recherche": { "champs": [{ "typeChamp": "ALL", "criteres": [
            { "typeRecherche": "TOUS_LES_MOTS_DANS_UN_CHAMP", "valeur": "609 code de procédure civile" }
        ]}]}
    });
    let exchange = client.legifrance_search_exchange(&body);
    assert_eq!(exchange.provider, "legifrance");
    assert_eq!(exchange.http_method, "POST");
    assert!(matches!(
        exchange.outcome,
        super::OfficialApiOutcome::UpstreamError
    ));
    assert!(exchange.http_status.is_none(), "no HTTP request was made");
    assert!(exchange.response_json.is_none());
    assert!(
        exchange.error.is_some(),
        "the missing-credential reason is recorded"
    );
    // Regression: the fingerprint must be a non-empty, body-derived sha256 — NOT the old empty
    // `legifrance-search:` that resulted from reading a now-absent top-level `query` field.
    assert!(
        exchange
            .request_fingerprint
            .starts_with("legifrance-search:sha256:")
    );
    assert_ne!(exchange.request_fingerprint, "legifrance-search:");
    // Stable & body-sensitive: same body -> same fingerprint; different body -> different fingerprint.
    assert_eq!(
        exchange.request_fingerprint,
        legifrance_search_fingerprint(&body.to_string())
    );
    let other = json!({ "fond": "CODE_DATE", "recherche": { "champs": [{ "typeChamp": "ALL",
        "criteres": [{ "typeRecherche": "TOUS_LES_MOTS_DANS_UN_CHAMP", "valeur": "1147 code civil" }]}]}});
    assert_ne!(
        exchange.request_fingerprint,
        legifrance_search_fingerprint(&other.to_string()),
        "distinct queries must produce distinct fingerprints"
    );
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
