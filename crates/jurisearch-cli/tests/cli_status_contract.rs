//! status/doctor/model/setup health + embedding-config contract tests.

mod support;
use support::*;

#[test]
fn status_returns_json_without_index() {
    let output = jurisearch_command_without_embedding_env()
        .env_remove("JURISEARCH_INDEX_DIR")
        .env("JURISEARCH_CONFIG", "none")
        .arg("status")
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["schema_version"], "1");
    assert_eq!(json["index"]["state"], "not_configured");
    assert_eq!(json["index"]["query_ready"], false);
    assert_eq!(json["ingest_health"]["state"], "pending");
    assert_eq!(json["embedding"]["provider"], "openai_compatible");
    assert_eq!(json["embedding"]["base_url_class"], "local_loopback");
    assert_eq!(json["embedding"]["model"], "bge-m3");
    assert_eq!(json["embedding"]["dimension"], 1024);
    assert_eq!(json["embedding"]["pooling"], "cls");
    assert_eq!(json["embedding"]["max_input_chars"], 20_000);
    assert_eq!(json["embedding"]["max_estimated_tokens"], 8_192);
    assert_eq!(json["embedding"]["estimated_chars_per_token"], 4);
    assert_eq!(json["embedding"]["token_count_method"], "estimated_chars");
    assert!(json["embedding"]["tokenizer_path"].is_null());
    assert_eq!(json["embedding"]["provisional"], true);
    assert_eq!(json["embedding"]["reembeddable"], true);
    assert!(json["embedding"]["config_path"].is_null());
    assert_eq!(json["embedding"]["config_loaded"], false);
    assert!(json["embedding"]["config_error"].is_null());
    assert_eq!(json["phase1_gate"]["state"], "not_ready");
    assert_eq!(json["phase1_gate"]["claim_allowed"], false);
    assert_eq!(json["phase1_gate"]["scope"], "phase1_legi_statutory_search");
    // Phase 2 full-juridic gate is fail-closed without a jurisprudence corpus or eval benchmark.
    assert_eq!(json["phase2_gate"]["state"], "not_ready");
    assert_eq!(json["phase2_gate"]["claim_allowed"], false);
    assert_eq!(json["phase2_gate"]["scope"], "phase2_full_french_juridic_search");
    assert_eq!(json["phase2_gate"]["benchmark"]["state"], "pending");
    assert_eq!(json["phase1_gate"]["eval_fixtures"]["total"], 6);
    assert_eq!(json["phase1_gate"]["eval_fixtures"]["source_verified"], 6);
    assert_eq!(
        json["phase1_gate"]["eval_fixtures"]["release_candidates"],
        4
    );
    assert_eq!(json["phase1_gate"]["eval_fixtures"]["release_gating"], 0);
    assert!(
        json["phase1_gate"]["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|check| check["name"] == "external_expert_annotated_eval"
                && check["status"] == "pending")
    );
    assert_eq!(
        json["phase1_gate"]["external_benchmark"]["state"],
        "pending"
    );
    assert_eq!(
        json["phase1_gate"]["external_benchmark"]["primary_candidate"],
        "maastrichtlawtech/bsard"
    );
    assert_eq!(
        json["phase1_gate"]["external_benchmark"]["jurisdiction"],
        "belgium"
    );
    assert_eq!(
        json["phase1_gate"]["external_benchmark"]["usage_scope"],
        "eval_only"
    );
    assert_eq!(
        json["phase1_gate"]["external_benchmark"]["source"],
        "not_configured"
    );
    assert!(json["phase1_gate"]["external_benchmark"]["artifact_path"].is_null());
}

#[test]
fn status_consumes_external_benchmark_artifact_from_env() {
    let artifact = tempfile::NamedTempFile::new().unwrap();
    fs::write(
        artifact.path(),
        serde_json::json!({
            "schema_version": 1,
            "kind": "phase1_external_expert_benchmark",
            "state": "passed",
            "dataset": {
                "id": "maastrichtlawtech/bsard",
                "revision": "contract-test",
                "question_split": "test",
                "jurisdiction": "belgium",
                "usage_scope": "eval_only",
                "license": "cc-by-nc-sa-4.0",
                "corpus_documents": 22633,
                "questions": 222,
                "limit_corpus": null,
                "limit_questions": null
            },
            "claim_scope": "external expert-annotated French-language statutory retrieval benchmark",
            "applicability": "Belgian statutory questions are a French-language statutory retrieval proxy, not France-LEGI human-reviewed gold.",
            "embedding": {
                "fingerprint_model": "bge-m3",
                "request_model": "baai/bge-m3",
                "dimension": 1024,
                "normalize": true
            },
            "thresholds": {
                "hybrid_recall_at_20_min": 0.8,
                "hybrid_ndcg_at_20_min": 0.6,
                "hybrid_mrr_at_20_min": 0.5
            },
            "metrics": {
                "hybrid": {
                    "recall_at_20": 0.86,
                    "ndcg_at_20": 0.72,
                    "mrr_at_20": 0.58
                }
            },
            "evidence": [
                "work/03-implementation/02-evidence/phase1-external-benchmark.json"
            ]
        })
        .to_string(),
    )
    .unwrap();

    let output = jurisearch_command_without_embedding_env()
        .env_remove("JURISEARCH_INDEX_DIR")
        .env("JURISEARCH_CONFIG", "none")
        .env("JURISEARCH_PHASE1_EXTERNAL_BENCHMARK", artifact.path())
        .args(["status"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    let external_benchmark = &json["phase1_gate"]["external_benchmark"];
    assert_eq!(external_benchmark["state"], "passed");
    assert_eq!(external_benchmark["dataset"]["revision"], "contract-test");
    assert_eq!(external_benchmark["artifact_error"], Value::Null);
    assert!(
        json["phase1_gate"]["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|check| check["name"] == "external_expert_annotated_eval"
                && check["status"] == "pass")
    );
    assert_eq!(json["phase1_gate"]["claim_allowed"], false);
}

#[test]
fn status_reports_embedding_budget_env_overrides() {
    let output = jurisearch_command_without_embedding_env()
        .env_remove("JURISEARCH_INDEX_DIR")
        .env("JURISEARCH_CONFIG", "none")
        .env("JURISEARCH_EMBED_MAX_INPUT_CHARS", "0")
        .env("JURISEARCH_EMBED_MAX_ESTIMATED_TOKENS", "none")
        .env("JURISEARCH_EMBED_ESTIMATED_CHARS_PER_TOKEN", "3")
        .env(
            "JURISEARCH_EMBED_TOKENIZER_JSON",
            "/tmp/jurisearch-tokenizer.json",
        )
        .arg("status")
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert!(json["embedding"]["max_input_chars"].is_null());
    assert!(json["embedding"]["max_estimated_tokens"].is_null());
    assert_eq!(json["embedding"]["estimated_chars_per_token"], 3);
    assert_eq!(json["embedding"]["token_count_method"], "tokenizer");
    assert_eq!(
        json["embedding"]["tokenizer_path"],
        "/tmp/jurisearch-tokenizer.json"
    );
}

#[test]
fn status_loads_embedding_config_file_and_redacts_secrets() {
    let config_home = tempfile::Builder::new()
        .prefix("jurisearch-cli-config.")
        .tempdir()
        .unwrap();
    let config_dir = config_home.path().join("jurisearch");
    fs::create_dir_all(&config_dir).unwrap();
    let config_path = config_dir.join("config.toml");
    fs::write(
        &config_path,
        r#"
[embedding]
provider = "openai_compatible"
base_url = "https://embeddings.example.test/v1"
base_urls = ["https://embeddings-1.example.test/v1", "https://embeddings-2.example.test/v1"]
api_key = "file-secret-token"
model = "custom-embed"
dimension = 768
normalize = false
pooling = "mean"
max_input_chars = 1234
max_estimated_tokens = 567
estimated_chars_per_token = 6
tokenizer_json = "/tmp/config-tokenizer.json"
provisional = false
reembeddable = false

[[embedding.pool]]
base_url = "https://openrouter.ai/api/v1"
request_model = "baai/bge-m3"
api_key_env = "POOL_API_KEY"
"#,
    )
    .unwrap();

    let output = jurisearch_command_without_embedding_env()
        .env_remove("JURISEARCH_INDEX_DIR")
        .env("XDG_CONFIG_HOME", config_home.path())
        .env("POOL_API_KEY", "file-pool-secret")
        .arg("status")
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output.clone()).unwrap();
    assert!(!stdout.contains("file-secret-token"));
    assert!(!stdout.contains("file-pool-secret"));
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["embedding"]["provider"], "openai_compatible");
    assert_eq!(
        json["embedding"]["base_url"],
        "https://embeddings.example.test/v1"
    );
    assert_eq!(
        json["embedding"]["base_urls"],
        serde_json::json!([
            "https://embeddings-1.example.test/v1",
            "https://embeddings-2.example.test/v1"
        ])
    );
    assert_eq!(json["embedding"]["base_url_class"], "hosted");
    assert_eq!(json["embedding"]["model"], "custom-embed");
    assert_eq!(json["embedding"]["dimension"], 768);
    assert_eq!(json["embedding"]["normalize"], false);
    assert_eq!(json["embedding"]["pooling"], "mean");
    assert_eq!(json["embedding"]["max_input_chars"], 1234);
    assert_eq!(json["embedding"]["max_estimated_tokens"], 567);
    assert_eq!(json["embedding"]["estimated_chars_per_token"], 6);
    assert_eq!(json["embedding"]["token_count_method"], "tokenizer");
    assert_eq!(
        json["embedding"]["tokenizer_path"],
        "/tmp/config-tokenizer.json"
    );
    assert_eq!(json["embedding"]["provisional"], false);
    assert_eq!(json["embedding"]["reembeddable"], false);
    let pool = json["embedding"]["pool"].as_array().unwrap();
    assert_eq!(pool.len(), 1);
    assert_eq!(json["embedding"]["pool_overrides_base_urls"], true);
    assert_eq!(pool[0]["base_url"], "https://openrouter.ai/api/v1");
    assert_eq!(pool[0]["request_model"], "baai/bge-m3");
    assert_eq!(pool[0]["api_key_env"], "POOL_API_KEY");
    assert_eq!(pool[0]["api_key_configured"], true);
    assert_eq!(
        json["embedding"]["config_path"],
        config_path.display().to_string()
    );
    assert_eq!(json["embedding"]["config_loaded"], true);
    assert!(json["embedding"]["config_error"].is_null());
    assert_eq!(json["embedding"]["endpoint"]["state"], "not_checked");
}

#[test]
fn status_env_overrides_embedding_config_file_and_redacts_env_secret() {
    let config_home = tempfile::Builder::new()
        .prefix("jurisearch-cli-config-env.")
        .tempdir()
        .unwrap();
    let config_dir = config_home.path().join("jurisearch");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(
        config_dir.join("config.toml"),
        r#"
[embedding]
base_url = "https://embeddings.example.test/v1"
api_key = "file-secret-token"
model = "file-model"
dimension = 768
"#,
    )
    .unwrap();

    let output = jurisearch_command_without_embedding_env()
        .env_remove("JURISEARCH_INDEX_DIR")
        .env("XDG_CONFIG_HOME", config_home.path())
        .env("JURISEARCH_EMBED_BASE_URL", "http://127.0.0.1:9/v1")
        .env(
            "JURISEARCH_EMBED_BASE_URLS",
            "http://127.0.0.1:9/v1, http://127.0.0.1:10/v1",
        )
        .env("JURISEARCH_EMBED_API_KEY", "env-secret-token")
        .env("JURISEARCH_EMBED_MODEL", "env-model")
        .env("JURISEARCH_EMBED_DIMENSION", "1024")
        .arg("status")
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output.clone()).unwrap();
    assert!(!stdout.contains("file-secret-token"));
    assert!(!stdout.contains("env-secret-token"));
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["embedding"]["base_url"], "http://127.0.0.1:9/v1");
    assert_eq!(
        json["embedding"]["base_urls"],
        serde_json::json!(["http://127.0.0.1:9/v1", "http://127.0.0.1:10/v1"])
    );
    assert_eq!(json["embedding"]["base_url_class"], "local_loopback");
    assert_eq!(json["embedding"]["model"], "env-model");
    assert_eq!(json["embedding"]["dimension"], 1024);
    assert_eq!(json["embedding"]["config_loaded"], true);
    assert!(json["embedding"]["config_error"].is_null());
}

#[test]
fn status_reports_embedding_pool_without_leaking_endpoint_keys() {
    let output = jurisearch_command_without_embedding_env()
        .env_remove("JURISEARCH_INDEX_DIR")
        .env("JURISEARCH_CONFIG", "none")
        .env(
            "JURISEARCH_EMBED_POOL",
            "http://127.0.0.1:9/v1;https://openrouter.ai/api/v1|baai/bge-m3|OPENROUTER_API_KEY",
        )
        .env("OPENROUTER_API_KEY", "openrouter-secret-token")
        .arg("status")
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output.clone()).unwrap();
    assert!(!stdout.contains("openrouter-secret-token"));
    let json: Value = serde_json::from_slice(&output).unwrap();
    let pool = json["embedding"]["pool"].as_array().unwrap();
    assert_eq!(pool.len(), 2);
    assert_eq!(json["embedding"]["pool_overrides_base_urls"], true);
    assert_eq!(pool[0]["base_url"], "http://127.0.0.1:9/v1");
    assert!(pool[0]["request_model"].is_null());
    assert!(pool[0]["api_key_env"].is_null());
    assert_eq!(pool[0]["api_key_configured"], false);
    assert_eq!(pool[1]["base_url"], "https://openrouter.ai/api/v1");
    assert_eq!(pool[1]["request_model"], "baai/bge-m3");
    assert_eq!(pool[1]["api_key_env"], "OPENROUTER_API_KEY");
    assert_eq!(pool[1]["api_key_configured"], true);
}

#[test]
fn status_reports_in_process_embedding_config_file() {
    let config_home = tempfile::Builder::new()
        .prefix("jurisearch-cli-config-local.")
        .tempdir()
        .unwrap();
    let config_dir = config_home.path().join("jurisearch");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(
        config_dir.join("config.toml"),
        r#"
[embedding]
provider = "local"
api_key = "unused-local-secret"
model = "local-bge-m3"
dimension = 1024
max_input_chars = 0
max_estimated_tokens = 0
"#,
    )
    .unwrap();

    let output = jurisearch_command_without_embedding_env()
        .env_remove("JURISEARCH_INDEX_DIR")
        .env("XDG_CONFIG_HOME", config_home.path())
        .env("JURISEARCH_MODEL_DIR", config_home.path().join("models"))
        .arg("status")
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output.clone()).unwrap();
    assert!(!stdout.contains("unused-local-secret"));
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["embedding"]["provider"], "in_process");
    assert_eq!(json["embedding"]["base_url"], "");
    assert_eq!(json["embedding"]["base_url_class"], "in_process");
    assert_eq!(json["embedding"]["model"], "local-bge-m3");
    assert_eq!(json["embedding"]["dimension"], 1024);
    assert!(json["embedding"]["max_input_chars"].is_null());
    assert!(json["embedding"]["max_estimated_tokens"].is_null());
    assert_eq!(json["embedding"]["config_loaded"], true);
    assert!(json["embedding"]["config_error"].is_null());
    assert_eq!(json["embedding"]["endpoint"]["state"], "not_applicable");
    assert_eq!(json["embedding"]["model_cache"]["required"], true);
    assert_eq!(json["embedding"]["model_cache"]["state"], "missing");
    assert_eq!(json["embedding"]["model_cache"]["model_present"], false);
    assert_eq!(
        json["embedding"]["model_cache"]["missing_files"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    let model_path = config_home
        .path()
        .join("models")
        .join("embeddings")
        .join("local-bge-m3");
    fs::create_dir_all(&model_path).unwrap();
    fs::write(model_path.join("model.onnx"), b"placeholder").unwrap();
    fs::write(model_path.join("tokenizer.json"), b"{}").unwrap();

    let output = jurisearch_command_without_embedding_env()
        .env_remove("JURISEARCH_INDEX_DIR")
        .env("XDG_CONFIG_HOME", config_home.path())
        .env("JURISEARCH_MODEL_DIR", config_home.path().join("models"))
        .arg("status")
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["embedding"]["model_cache"]["state"], "ready");
    assert_eq!(json["embedding"]["model_cache"]["model_present"], true);
    assert!(
        json["embedding"]["model_cache"]["missing_files"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn status_env_in_process_provider_clears_unused_api_key() {
    let output = jurisearch_command_without_embedding_env()
        .env_remove("JURISEARCH_INDEX_DIR")
        .env("JURISEARCH_CONFIG", "none")
        .env("JURISEARCH_EMBED_PROVIDER", "in_process")
        .env("JURISEARCH_EMBED_API_KEY", "unused-env-local-secret")
        .arg("status")
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output.clone()).unwrap();
    assert!(!stdout.contains("unused-env-local-secret"));
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["embedding"]["provider"], "in_process");
    assert_eq!(json["embedding"]["base_url"], "");
    assert_eq!(json["embedding"]["base_url_class"], "in_process");
    assert!(json["embedding"]["config_error"].is_null());
}

#[test]
fn status_reports_loopback_endpoint_reachability() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let output = jurisearch_command_without_embedding_env()
        .env_remove("JURISEARCH_INDEX_DIR")
        .env("JURISEARCH_CONFIG", "none")
        .env(
            "JURISEARCH_EMBED_BASE_URL",
            format!("http://127.0.0.1:{port}/v1"),
        )
        .arg("status")
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    drop(listener);
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["embedding"]["endpoint"]["checked"], true);
    assert_eq!(json["embedding"]["endpoint"]["state"], "reachable");
    assert_eq!(json["embedding"]["endpoint"]["reachable"], true);
}

#[test]
fn model_fetch_and_setup_report_in_process_model_cache() {
    let model_root = tempfile::Builder::new()
        .prefix("jurisearch-cli-model-cache.")
        .tempdir()
        .unwrap();
    let missing_output = jurisearch_command_without_embedding_env()
        .env_remove("JURISEARCH_INDEX_DIR")
        .env("JURISEARCH_CONFIG", "none")
        .env("JURISEARCH_MODEL_DIR", model_root.path())
        .env("JURISEARCH_EMBED_PROVIDER", "in_process")
        .args(["model", "fetch", "local-bge-m3"])
        .assert()
        .code(2)
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    assert_json_error_contains(&missing_output, "bad_input", "missing required cache files");

    let output = jurisearch_command_without_embedding_env()
        .env_remove("JURISEARCH_INDEX_DIR")
        .env("JURISEARCH_CONFIG", "none")
        .env("JURISEARCH_MODEL_DIR", model_root.path())
        .env("JURISEARCH_EMBED_PROVIDER", "in_process")
        .env("JURISEARCH_EMBED_MODEL", "local-bge-m3")
        .arg("setup")
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["ready"], false);
    assert_eq!(json["embedding"]["model_cache"]["state"], "missing");

    let model_path = model_root.path().join("embeddings").join("local-bge-m3");
    fs::create_dir_all(&model_path).unwrap();
    fs::write(model_path.join("model.onnx"), b"placeholder").unwrap();
    fs::write(model_path.join("tokenizer.json"), b"{}").unwrap();

    let output = jurisearch_command_without_embedding_env()
        .env_remove("JURISEARCH_INDEX_DIR")
        .env("JURISEARCH_CONFIG", "none")
        .env("JURISEARCH_MODEL_DIR", model_root.path())
        .env("JURISEARCH_EMBED_PROVIDER", "in_process")
        .args(["model", "fetch", "local-bge-m3"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["action"], "already_cached");
    assert_eq!(json["model_cache"]["state"], "ready");

    let input = concat!(
        "{\"id\":\"setup\",\"command\":\"setup\"}\n",
        "{\"id\":\"fetch\",\"command\":\"model fetch\",\"args\":{\"model\":\"local-bge-m3\"}}\n",
    );
    let output = jurisearch_command_without_embedding_env()
        .env_remove("JURISEARCH_INDEX_DIR")
        .env("JURISEARCH_CONFIG", "none")
        .env("JURISEARCH_MODEL_DIR", model_root.path())
        .env("JURISEARCH_EMBED_PROVIDER", "in_process")
        .env("JURISEARCH_EMBED_MODEL", "local-bge-m3")
        .args(["session", "--jsonl"])
        .write_stdin(input)
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let lines = String::from_utf8(output).unwrap();
    let values = lines
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(values.len(), 2);
    assert_eq!(values[0]["id"], "setup");
    assert_eq!(values[0]["ok"], true);
    assert_eq!(values[0]["result"]["ready"], true);
    assert_eq!(values[1]["id"], "fetch");
    assert_eq!(values[1]["ok"], true);
    assert_eq!(values[1]["result"]["action"], "already_cached");
}

#[test]
fn status_malformed_embedding_config_does_not_leak_api_key() {
    let config_home = tempfile::Builder::new()
        .prefix("jurisearch-cli-config-malformed.")
        .tempdir()
        .unwrap();
    let config_dir = config_home.path().join("jurisearch");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(
        config_dir.join("config.toml"),
        r#"
[embedding]
base_url = "https://embeddings.example.test/v1"
api_key = "super-secret-leaky-token
model = "custom-embed"
"#,
    )
    .unwrap();

    let output = jurisearch_command_without_embedding_env()
        .env_remove("JURISEARCH_INDEX_DIR")
        .env("XDG_CONFIG_HOME", config_home.path())
        .arg("status")
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output.clone()).unwrap();
    assert!(!stdout.contains("super-secret-leaky-token"));
    assert!(!stdout.contains("api_key"));
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["embedding"]["config_loaded"], false);
    assert!(
        json["embedding"]["config_error"]
            .as_str()
            .unwrap()
            .contains("TOML syntax error at line")
    );

    let output = jurisearch_command_without_embedding_env()
        .env_remove("JURISEARCH_INDEX_DIR")
        .env("XDG_CONFIG_HOME", config_home.path())
        .args(["session", "--jsonl"])
        .write_stdin("{\"id\":\"status\",\"command\":\"status\"}\n")
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output.clone()).unwrap();
    assert!(!stdout.contains("super-secret-leaky-token"));
    assert!(!stdout.contains("api_key"));
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["id"], "status");
    assert_eq!(json["ok"], true);
    assert_eq!(json["result"]["embedding"]["config_loaded"], false);
    assert!(
        json["result"]["embedding"]["config_error"]
            .as_str()
            .unwrap()
            .contains("TOML syntax error at line")
    );
}

#[test]
fn status_unknown_embedding_config_key_fails_without_source_echo() {
    let config_home = tempfile::Builder::new()
        .prefix("jurisearch-cli-config-unknown-key.")
        .tempdir()
        .unwrap();
    let config_dir = config_home.path().join("jurisearch");
    fs::create_dir_all(&config_dir).unwrap();
    fs::write(
        config_dir.join("config.toml"),
        r#"
[embedding]
api_key = "unknown-key-secret"
dimention = 768
"#,
    )
    .unwrap();

    let output = jurisearch_command_without_embedding_env()
        .env_remove("JURISEARCH_INDEX_DIR")
        .env("XDG_CONFIG_HOME", config_home.path())
        .arg("status")
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let stdout = String::from_utf8(output.clone()).unwrap();
    assert!(!stdout.contains("unknown-key-secret"));
    assert!(!stdout.contains("dimention"));
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["embedding"]["config_loaded"], false);
    assert!(
        json["embedding"]["config_error"]
            .as_str()
            .unwrap()
            .contains("TOML syntax error at line")
    );
}

#[test]
fn status_reports_not_initialized_index_without_opening_postgres() -> Result<(), StorageError> {
    let root = tempfile::Builder::new()
        .prefix("jurisearch-cli-status-empty.")
        .tempdir()
        .map_err(StorageError::Io)?;

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .arg("status")
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["index"]["state"], "not_initialized");
    assert_eq!(json["index"]["query_ready"], false);
    assert_eq!(json["ingest_health"]["state"], "pending");
    Ok(())
}

#[test]
fn status_reports_ingest_health_from_existing_index() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("CLI status ingest health")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-cli-status-health.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let compatibility = IngestCompatibility {
        parser_version: "legi-parser:v1",
        schema_version: "canonical:v1",
        code_version: "test-code-sha",
        source_payload_hash: "sha256:article-1240",
    };

    {
        let postgres = jurisearch_storage::runtime::ManagedPostgres::start_durable(
            pg_config.clone(),
            root.path(),
        )?;
        start_ingest_run(
            &postgres,
            &IngestRunInput {
                run_id: "run-status",
                source: "legi",
                parser_version: compatibility.parser_version,
                schema_version: compatibility.schema_version,
                code_version: compatibility.code_version,
                safe_mode: false,
                archive_plan_json: None,
                manifest_json: None,
            },
        )?;
        record_ingest_member(
            &postgres,
            &IngestMemberInput {
                run_id: "run-status",
                archive_name: "Freemium_legi_global_20240101-000000.tar.gz",
                member_path: "legi/articles/LEGIARTI000006419320.xml",
                source: "legi",
                source_entity: Some("LEGIARTI000006419320"),
                date_anchor: Some("1804-02-21"),
                status: IngestMemberStatus::Inserted,
                compatibility,
            },
        )?;
        postgres.execute_sql(&format!(
            "INSERT INTO documents \
                (document_id, source, kind, source_uid, citation, title, body, \
                 valid_from, source_payload_hash, canonical_json) \
             VALUES \
                ('legi:LEGIARTI000006419320@1804-02-21', 'legi', 'article', \
                 'LEGIARTI000006419320', 'Code civil article 1240', \
                 'Article 1240', 'Tout fait quelconque de l''homme oblige a reparer le dommage.', \
                 '1804-02-21', 'sha256:article-1240', '{{\"official\":true}}'); \
             INSERT INTO chunks \
                (chunk_id, document_id, chunk_index, body, contextualized_body, source_payload_hash, \
                 chunk_builder_version, embedding_fingerprint) \
             VALUES \
                ('chunk:1240:0', 'legi:LEGIARTI000006419320@1804-02-21', 0, \
                 'responsabilite civile faute reparation dommage article 1240', \
                 'Code civil > Article 1240\nresponsabilite civile faute reparation dommage article 1240', \
                 'sha256:article-1240', 'chunker:v0', 'bge-m3:1024:normalize:true'); \
             INSERT INTO chunk_embeddings \
                (chunk_id, embedding_fingerprint, embedding, model, dimension) \
             VALUES \
                ('chunk:1240:0', 'bge-m3:1024:normalize:true', '{}', 'bge-m3', 1024);",
            unit_vector_literal(0)
        ))?;
        finish_ingest_run(
            &postgres,
            "run-status",
            jurisearch_storage::ingest_accounting::IngestRunStatus::Completed,
            None,
        )?;
    }

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .arg("status")
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["index"]["state"], "ready");
    assert_eq!(json["index"]["query_ready"], true);
    assert_eq!(json["ingest_health"]["state"], "available");
    assert_eq!(json["ingest_health"]["latest_run_id"], "run-status");
    assert_eq!(json["ingest_health"]["latest_completed_run"], "run-status");
    assert_eq!(json["ingest_health"]["total_members"], 1);
    assert_eq!(json["ingest_health"]["inserted_members"], 1);
    assert_eq!(json["ingest_health"]["projection_coverage"]["covered"], 1);
    assert_eq!(json["ingest_health"]["projection_coverage"]["total"], 1);
    assert_eq!(json["ingest_health"]["embedding_coverage"]["covered"], 1);
    assert_eq!(json["ingest_health"]["embedding_coverage"]["total"], 1);
    assert_eq!(json["ingest_health"]["replay_snapshot_status"], "missing");
    assert_eq!(json["ingest_health"]["replay_snapshot_source"], "missing");
    assert_eq!(
        json["ingest_health"]["replay_snapshot"]["documents"]["count"],
        0
    );

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .args(["status", "--deep"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["ingest_health"]["replay_snapshot_status"], "available");
    assert_eq!(json["ingest_health"]["replay_snapshot_source"], "refreshed");
    assert_eq!(
        json["ingest_health"]["replay_snapshot"]["documents"]["count"],
        1
    );
    assert_eq!(
        json["ingest_health"]["replay_snapshot"]["chunks"]["count"],
        1
    );
    assert_eq!(
        json["ingest_health"]["replay_snapshot"]["embeddings"]["count"],
        1
    );
    assert_eq!(
        json["ingest_health"]["replay_snapshot"]["signature"]
            .as_str()
            .unwrap()
            .len(),
        32
    );
    let replay_signature = json["ingest_health"]["replay_snapshot"]["signature"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        json["ingest_health"]["recovery_warnings"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .arg("status")
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let cached_json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(
        cached_json["ingest_health"]["replay_snapshot_status"],
        "available"
    );
    assert_eq!(
        cached_json["ingest_health"]["replay_snapshot_source"],
        "cached"
    );
    assert_eq!(
        cached_json["ingest_health"]["replay_snapshot"]["signature"],
        replay_signature
    );

    let input = format!(
        "{{\"id\":\"status-index\",\"command\":\"status\",\"args\":{{\"index_dir\":\"{}\",\"deep\":true}}}}\n",
        root.path().to_string_lossy()
    );
    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env_remove("JURISEARCH_INDEX_DIR")
        .args(["session", "--jsonl"])
        .write_stdin(input)
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["id"], "status-index");
    assert_eq!(json["ok"], true);
    assert_eq!(json["result"]["index"]["query_ready"], true);
    assert_eq!(json["result"]["ingest_health"]["state"], "available");

    {
        let postgres = jurisearch_storage::runtime::ManagedPostgres::start_durable(
            pg_config.clone(),
            root.path(),
        )?;
        postgres.execute_sql(
            "UPDATE chunk_embeddings \
             SET embedding_fingerprint = 'stale-fingerprint' \
             WHERE chunk_id = 'chunk:1240:0';",
        )?;
    }
    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .arg("status")
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["index"]["query_ready"], false);
    assert_eq!(json["ingest_health"]["embedding_coverage"]["covered"], 0);
    assert_eq!(json["ingest_health"]["embedding_coverage"]["total"], 1);

    {
        let postgres =
            jurisearch_storage::runtime::ManagedPostgres::start_durable(pg_config, root.path())?;
        postgres.execute_sql(
            "UPDATE chunk_embeddings \
             SET embedding_fingerprint = 'bge-m3:1024:normalize:true' \
             WHERE chunk_id = 'chunk:1240:0';",
        )?;
    }
    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .arg("status")
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["index"]["query_ready"], true);
    assert_eq!(json["ingest_health"]["embedding_coverage"]["covered"], 1);
    assert_eq!(json["ingest_health"]["embedding_coverage"]["total"], 1);
    Ok(())
}

#[test]
fn status_marks_initialized_index_not_ready_when_embedding_coverage_is_incomplete()
-> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("CLI status incomplete coverage")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-cli-status-incomplete.")
        .tempdir()
        .map_err(StorageError::Io)?;

    {
        let postgres =
            jurisearch_storage::runtime::ManagedPostgres::start_durable(pg_config, root.path())?;
        postgres.execute_sql(
            "INSERT INTO documents \
                (document_id, source, kind, source_uid, citation, title, body, \
                 valid_from, source_payload_hash, canonical_json) \
             VALUES \
                ('legi:LEGIARTI000006419320@1804-02-21', 'legi', 'article', \
                 'LEGIARTI000006419320', 'Code civil article 1240', \
                 'Article 1240', 'Tout fait quelconque de l''homme oblige a reparer le dommage.', \
                 '1804-02-21', 'sha256:article-1240', '{\"official\":true}'), \
                ('legi:LEGIARTI000000000124@2024-01-01', 'legi', 'article', \
                 'LEGIARTI000000000124', 'Code civil article 1241', \
                 'Article 1241', 'Responsabilite civile complementaire pour le dommage.', \
                 '2024-01-01', 'sha256:article-1241', '{\"official\":true}'); \
             INSERT INTO chunks \
                (chunk_id, document_id, chunk_index, body, contextualized_body, source_payload_hash, \
                 chunk_builder_version, embedding_fingerprint) \
             VALUES \
                ('chunk:1240:0', 'legi:LEGIARTI000006419320@1804-02-21', 0, \
                 'responsabilite civile faute reparation dommage article 1240', \
                 'Code civil > Article 1240\nresponsabilite civile faute reparation dommage article 1240', \
                 'sha256:article-1240', 'chunker:v0', 'bge-m3:1024:normalize:true'), \
                ('chunk:1241:0', 'legi:LEGIARTI000000000124@2024-01-01', 0, \
                 'responsabilite civile dommage article 1241', \
                 'Code civil > Article 1241\nresponsabilite civile dommage article 1241', \
                 'sha256:article-1241', 'chunker:v0', 'bge-m3:1024:normalize:true');",
        )?;
    }

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .arg("status")
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["index"]["state"], "ready");
    assert_eq!(json["index"]["query_ready"], false);
    assert_eq!(json["ingest_health"]["projection_coverage"]["covered"], 2);
    assert_eq!(json["ingest_health"]["projection_coverage"]["total"], 2);
    assert_eq!(json["ingest_health"]["embedding_coverage"]["covered"], 0);
    assert_eq!(json["ingest_health"]["embedding_coverage"]["total"], 2);

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .env("JURISEARCH_EMBED_BASE_URL", "http://127.0.0.1:9/v1")
        .args(["search", "responsabilite civile"])
        .assert()
        .code(3)
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["code"], "index_unavailable");
    assert!(
        json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("embedding coverage gate is incomplete")
    );

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .env("JURISEARCH_EMBED_BASE_URL", "http://127.0.0.1:9/v1")
        .args([
            "search",
            "responsabilite civile",
            "--mode",
            "bm25",
            "--top-k",
            "1",
        ])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["format"], "concise");
    assert!(json["diagnostics"].is_null());
    assert_eq!(json["retrieval_mode"], "bm25");
    let first_page_document_id = json["candidates"][0]["document_id"]
        .as_str()
        .expect("first page candidate has a document id")
        .to_owned();
    assert!(
        [
            "legi:LEGIARTI000006419320@1804-02-21",
            "legi:LEGIARTI000000000124@2024-01-01",
        ]
        .contains(&first_page_document_id.as_str())
    );
    assert!(json["candidates"][0]["scores"]["dense_rank"].is_null());
    assert_eq!(json["expansion_seed_version"], "legal-vocabulary-seed:v1");
    assert!(
        json["expanded_terms"]
            .as_array()
            .unwrap()
            .iter()
            .any(|term| term["term"] == "article 1240"
                && term["source_seed_id"] == "civil-liability-fault-damage")
    );
    assert_eq!(json["pagination"]["requested_top_k"], 1);
    assert_eq!(json["pagination"]["returned"], 1);
    assert_eq!(json["pagination"]["possibly_truncated"], true);
    assert_eq!(json["pagination"]["cursor_supported"], true);
    let next_cursor = json["pagination"]["next_cursor"]
        .as_str()
        .expect("full first page returns a next cursor")
        .to_owned();
    assert!(
        json["pagination"]["cursor_note"]
            .as_str()
            .is_some_and(|note| note.contains("Use next_cursor"))
    );
    assert!(
        json["pagination"]["guidance"]
            .as_str()
            .is_some_and(|guidance| guidance.contains("Use next_cursor"))
    );

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .env("JURISEARCH_EMBED_BASE_URL", "http://127.0.0.1:9/v1")
        .args([
            "search",
            "responsabilite civile",
            "--mode",
            "bm25",
            "--top-k",
            "1",
            "--cursor",
            &next_cursor,
        ])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let second_page: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(second_page["pagination"]["after_cursor"], next_cursor);
    assert_eq!(second_page["pagination"]["returned"], 1);
    assert_ne!(
        second_page["candidates"][0]["document_id"],
        first_page_document_id
    );

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .env("JURISEARCH_EMBED_BASE_URL", "http://127.0.0.1:9/v1")
        .args([
            "search",
            "responsabilite civile",
            "--mode",
            "bm25",
            "--top-k",
            "10",
            "--format",
            "detailed",
        ])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["format"], "detailed");
    assert_eq!(json["pagination"]["requested_top_k"], 10);
    assert_eq!(json["pagination"]["returned"], 2);
    assert_eq!(json["pagination"]["possibly_truncated"], false);
    assert!(json["pagination"]["guidance"].is_null());
    assert_eq!(json["diagnostics"]["query_input"], "responsabilite civile");
    assert_eq!(
        json["diagnostics"]["lexical_query_text"],
        "responsabilite civile"
    );
    assert_eq!(json["diagnostics"]["retrieval"]["mode"], "bm25");
    assert_eq!(json["diagnostics"]["retrieval"]["uses_dense"], false);
    assert_eq!(json["diagnostics"]["retrieval"]["lexical_limit"], 40);
    assert_eq!(json["diagnostics"]["retrieval"]["query_limit"], 11);
    assert!(json["diagnostics"]["retrieval"]["embedding_fingerprint"].is_null());

    let input = format!(
        "{}\n{}\n",
        serde_json::json!({
            "id": "search-format",
            "command": "search",
            "args": {
                "query": "responsabilite civile",
                "mode": "bm25",
                "format": "detailed",
                "top_k": 1,
                "index_dir": root.path().to_string_lossy()
            }
        }),
        serde_json::json!({"id": "done", "command": "exit"})
    );
    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .args(["session", "--jsonl"])
        .write_stdin(input)
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let lines = String::from_utf8(output).unwrap();
    let values = lines
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(values.len(), 2);
    assert_eq!(values[0]["id"], "search-format");
    assert_eq!(values[0]["ok"], true);
    assert_eq!(values[0]["result"]["format"], "detailed");
    assert_eq!(
        values[0]["result"]["diagnostics"]["retrieval"]["mode"],
        "bm25"
    );
    assert_eq!(values[1]["result"]["bye"], true);
    Ok(())
}
