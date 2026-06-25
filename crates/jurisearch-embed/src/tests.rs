//! Unit tests for the embedding config/fingerprint/client/tokenizer, moved out of lib.rs verbatim.

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
    let mut config = EmbeddingConfig::openai_compatible(base_url, None, "bge-m3", 3, true, "cls");
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
    let mut config =
        EmbeddingConfig::openai_compatible("http://127.0.0.1:9/v1", None, "bge-m3", 3, true, "cls");
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
    let mut config =
        EmbeddingConfig::openai_compatible("http://127.0.0.1:9/v1", None, "bge-m3", 3, true, "cls");
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

    let mut config =
        EmbeddingConfig::openai_compatible("http://127.0.0.1:9/v1", None, "bge-m3", 3, true, "cls");
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

    let mut config =
        EmbeddingConfig::openai_compatible("http://127.0.0.1:9/v1", None, "bge-m3", 3, true, "cls");
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
    let mut config =
        EmbeddingConfig::openai_compatible("http://127.0.0.1:9/v1", None, "bge-m3", 3, true, "cls");
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

fn spawn_embedding_server_with_status(status: &'static str, response_body: &'static str) -> String {
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
