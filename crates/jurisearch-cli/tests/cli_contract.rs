use std::{
    fs::{self, File},
    io::{Cursor, Read, Write},
    net::TcpListener,
    path::Path,
    thread,
    time::Duration,
};

use assert_cmd::Command;
use flate2::{Compression, write::GzEncoder};
use jurisearch_embed::{EmbeddingConfig, OpenAiCompatibleClient};
use jurisearch_storage::{
    ingest_accounting::{
        IngestCompatibility, IngestMemberInput, IngestMemberStatus, IngestRunInput,
        finish_ingest_run, record_ingest_member, start_ingest_run,
    },
    runtime::{ManagedPostgres, PgConfig, StorageError},
};
use predicates::prelude::*;
use serde_json::Value;
use tar::{Builder, Header};

fn jurisearch_command_without_embedding_env() -> Command {
    let mut command = Command::cargo_bin("jurisearch").unwrap();
    for name in [
        "JURISEARCH_CONFIG",
        "XDG_CONFIG_HOME",
        "JURISEARCH_EMBED_PROVIDER",
        "JURISEARCH_EMBED_BASE_URL",
        "JURISEARCH_EMBED_BASE_URLS",
        "JURISEARCH_EMBED_POOL",
        "JURISEARCH_EMBED_API_KEY",
        "JURISEARCH_EMBED_MODEL",
        "JURISEARCH_EMBED_DIMENSION",
        "JURISEARCH_EMBED_NORMALIZE",
        "JURISEARCH_EMBED_POOLING",
        "JURISEARCH_EMBED_MAX_INPUT_CHARS",
        "JURISEARCH_EMBED_MAX_ESTIMATED_TOKENS",
        "JURISEARCH_EMBED_ESTIMATED_CHARS_PER_TOKEN",
        "JURISEARCH_EMBED_TOKENIZER_JSON",
        "JURISEARCH_PHASE1_EXTERNAL_BENCHMARK",
        "JURISEARCH_PHASE1_FRANCE_LEGI_BENCHMARK",
        "JURISEARCH_MODEL_DIR",
        "OPENROUTER_API_KEY",
        "XDG_CACHE_HOME",
    ] {
        command.env_remove(name);
    }
    command
}

#[test]
fn help_agent_works_without_index() {
    let mut command = Command::cargo_bin("jurisearch").unwrap();
    command
        .args(["help", "agent"])
        .assert()
        .success()
        .stdout(predicate::str::contains("jurisearch agent contract"))
        .stdout(predicate::str::contains("search"))
        .stdout(predicate::str::contains("help schema --json"));
}

#[test]
fn help_schema_json_is_valid_and_lists_commands() {
    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .args(["help", "schema", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["schema_version"], "1");
    assert!(json["commands"].as_array().unwrap().iter().any(|command| {
        command["name"] == "search"
            && command["status"] == "implemented"
            && command["request_schema"] == "SearchRequest"
    }));
    assert!(json["commands"].as_array().unwrap().iter().any(|command| {
        command["name"] == "expand"
            && command["status"] == "implemented"
            && command["response_schema"] == "ExpandResponse"
    }));
    assert_eq!(json["common_enums"]["kind"]["values"][0], "code");
    assert_eq!(json["common_enums"]["search_mode"]["values"][0], "hybrid");
    assert_eq!(
        json["schemas"]["SearchRequest"]["properties"]["mode"]["default"],
        "hybrid"
    );
    assert_eq!(
        json["schemas"]["SearchRequest"]["properties"]["format"]["default"],
        "concise"
    );
    assert_eq!(
        json["schemas"]["SearchRequest"]["properties"]["group_by"]["default"],
        "chunk"
    );
    assert_eq!(
        json["schemas"]["SearchRequest"]["properties"]["group_by"]["enum"][1],
        "document"
    );
    assert_eq!(
        json["schemas"]["SearchRequest"]["properties"]["cursor"]["type"],
        "string"
    );
    assert_eq!(
        json["schemas"]["SearchResponse"]["properties"]["format"]["enum"][1],
        "detailed"
    );
    assert_eq!(
        json["schemas"]["SearchResponse"]["properties"]["diagnostics"]["properties"]["retrieval"]["properties"]
            ["lexical_limit"]["type"],
        "integer"
    );
    assert_eq!(
        json["schemas"]["SearchResponse"]["properties"]["expanded_terms"]["type"],
        "array"
    );
    assert_eq!(
        json["schemas"]["SearchResponse"]["properties"]["expansion_seed_version"]["type"],
        "string"
    );
    assert_eq!(
        json["schemas"]["SearchResponse"]["properties"]["pagination"]["type"],
        "object"
    );
    assert_eq!(
        json["schemas"]["SearchResponse"]["properties"]["pagination"]["properties"]["cursor_note"]
            ["type"],
        "string"
    );
    assert_eq!(
        json["schemas"]["ExpandResponse"]["properties"]["expanded_terms"]["type"],
        "array"
    );
    assert!(json["commands"].as_array().unwrap().iter().any(|command| {
        command["name"] == "cite"
            && command["status"] == "implemented"
            && command["response_schema"] == "CiteResponse"
    }));
    assert!(json["commands"].as_array().unwrap().iter().any(|command| {
        command["name"] == "model fetch"
            && command["status"] == "implemented"
            && command["response_schema"] == "ModelFetchResponse"
    }));
    assert!(json["commands"].as_array().unwrap().iter().any(|command| {
        command["name"] == "setup"
            && command["status"] == "implemented"
            && command["response_schema"] == "SetupResponse"
    }));
    assert!(json["commands"].as_array().unwrap().iter().any(|command| {
        command["name"] == "eval phase1"
            && command["status"] == "implemented"
            && command["response_schema"] == "EvalPhase1Response"
    }));
    assert_eq!(
        json["schemas"]["CiteRequest"]["properties"]["as_of"]["format"],
        "date"
    );
    assert_eq!(
        json["schemas"]["CiteResponse"]["properties"]["state"]["enum"][0],
        "exact"
    );
    assert_eq!(
        json["schemas"]["StatusResponse"]["properties"]["embedding"]["properties"]["model_cache"]["$ref"],
        "#/schemas/ModelCacheStatus"
    );
    assert_eq!(
        json["schemas"]["StatusRequest"]["properties"]["deep"]["default"],
        false
    );
    assert_eq!(
        json["schemas"]["StatusResponse"]["properties"]["phase1_gate"]["$ref"],
        "#/schemas/Phase1GateResponse"
    );
    assert_eq!(
        json["schemas"]["Phase1GateResponse"]["properties"]["checks"]["items"]["$ref"],
        "#/schemas/Phase1GateCheck"
    );
    assert_eq!(
        json["schemas"]["Phase1GateResponse"]["properties"]["reranker_decision"]["$ref"],
        "#/schemas/RerankerDecision"
    );
    assert_eq!(
        json["schemas"]["Phase1GateResponse"]["properties"]["external_benchmark"]["$ref"],
        "#/schemas/ExternalBenchmarkGate"
    );
    assert_eq!(
        json["schemas"]["ExternalBenchmarkGate"]["properties"]["state"]["enum"][0],
        "pending"
    );
    assert_eq!(
        json["schemas"]["ExternalBenchmarkGate"]["properties"]["claim_scope"]["type"],
        "string"
    );
    assert_eq!(
        json["schemas"]["ExternalBenchmarkGate"]["properties"]["artifact_path"]["type"][0],
        "string"
    );
    assert_eq!(
        json["schemas"]["RerankerDecision"]["properties"]["provider"]["enum"][0],
        "disabled"
    );
    assert_eq!(
        json["schemas"]["RerankerDecision"]["properties"]["evidence"]["items"]["type"],
        "string"
    );
    assert_eq!(
        json["schemas"]["EvalFixtureSummary"]["properties"]["release_candidates"]["type"],
        "integer"
    );
    assert_eq!(
        json["schemas"]["EvalPhase1Request"]["properties"]["mode"]["default"],
        "hybrid"
    );
    assert_eq!(
        json["schemas"]["EvalPhase1Response"]["properties"]["eval_fixtures"]["$ref"],
        "#/schemas/EvalFixtureSummary"
    );
    assert_eq!(
        json["schemas"]["ModelFetchRequest"]["properties"]["allow_download"]["default"],
        false
    );
    assert_eq!(
        json["schemas"]["SetupResponse"]["properties"]["embedding"]["properties"]["model_cache"]["$ref"],
        "#/schemas/ModelCacheStatus"
    );
}

#[test]
fn expand_returns_curated_terms_with_review_metadata_without_index() {
    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env_remove("JURISEARCH_INDEX_DIR")
        .args(["expand", "faute dommage"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["query"], "faute dommage");
    assert_eq!(json["seed_version"], "legal-vocabulary-seed:v1");
    let article_1240 = json["expanded_terms"]
        .as_array()
        .unwrap()
        .iter()
        .find(|term| {
            term["term"] == "article 1240"
                && term["source_seed_id"] == "civil-liability-fault-damage"
                && term["source_citation"] == "Code civil, articles 1240 et 1241"
        });
    let article_1240 = article_1240.expect("article 1240 expansion is present");
    assert_eq!(
        article_1240["review_status"],
        "dev_seed_pending_legal_review"
    );
    assert_eq!(article_1240["reviewer"], "pending_legal_domain_review");
    assert_eq!(
        article_1240["matched_terms"],
        serde_json::json!(["faute", "dommage"])
    );
}

#[test]
fn expand_rejects_empty_query_in_cli_and_session() {
    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env_remove("JURISEARCH_INDEX_DIR")
        .args(["expand", "   "])
        .assert()
        .code(2)
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["code"], "bad_input");
    assert_eq!(json["error"]["message"], "expand query must not be empty");

    let input = concat!(
        "{\"id\":\"bad-expand\",\"command\":\"expand\",\"args\":{\"query\":\"\"}}\n",
        "{\"id\":\"done\",\"command\":\"exit\"}\n",
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
    assert_eq!(values[0]["id"], "bad-expand");
    assert_eq!(values[0]["ok"], false);
    assert_eq!(values[0]["error"]["code"], "bad_input");
    assert_eq!(
        values[0]["error"]["message"],
        "expand query must not be empty"
    );
    assert_eq!(values[1]["result"]["bye"], true);
}

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
fn eval_phase1_list_reports_release_candidates_without_index() {
    let output = jurisearch_command_without_embedding_env()
        .env_remove("JURISEARCH_INDEX_DIR")
        .env("JURISEARCH_CONFIG", "none")
        .args(["eval", "phase1", "--list"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["schema_version"], "1");
    assert_eq!(json["command"], "eval phase1");
    assert_eq!(json["action"], "list");
    assert_eq!(json["include_dev"], false);
    assert_eq!(json["fixture_count"], 4);
    assert_eq!(json["eval_fixtures"]["total"], 6);
    assert_eq!(json["eval_fixtures"]["release_candidates"], 4);
    assert_eq!(json["eval_fixtures"]["release_gating"], 0);
    assert_eq!(json["fixtures"].as_array().unwrap().len(), 4);
    assert!(json["fixtures"].as_array().unwrap().iter().all(|fixture| {
        fixture["tier"] == "release_gating"
            && fixture["review_status"] == "official_source_checked"
            && fixture["reviewer"].is_null()
            && fixture["as_of"].is_string()
    }));
}

#[test]
fn session_eval_phase1_list_preserves_jsonl_envelope() {
    let input = format!(
        "{}\n{}\n",
        serde_json::json!({
            "id": "eval-list",
            "command": "eval phase1",
            "args": { "list": true, "include_dev": true }
        }),
        serde_json::json!({"id": "done", "command": "exit"})
    );
    let output = jurisearch_command_without_embedding_env()
        .env_remove("JURISEARCH_INDEX_DIR")
        .env("JURISEARCH_CONFIG", "none")
        .args(["session", "--jsonl"])
        .write_stdin(input)
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let values = String::from_utf8(output)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(values.len(), 2);
    assert_eq!(values[0]["id"], "eval-list");
    assert_eq!(values[0]["ok"], true);
    assert_eq!(values[0]["result"]["action"], "list");
    assert_eq!(values[0]["result"]["include_dev"], true);
    assert_eq!(values[0]["result"]["fixture_count"], 6);
    assert_eq!(values[1]["result"]["bye"], true);
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

#[test]
fn retrieval_commands_reject_incomplete_projection_coverage() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("CLI retrieval projection gate")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-cli-retrieval-projection-gate.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let document_id = "legi:LEGIARTI000006419320@1804-02-21";

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
                 '1804-02-21', 'sha256:article-1240', '{\"official\":true}');",
        )?;
    }

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
    assert_json_error_contains(
        &output,
        "index_unavailable",
        "projection coverage gate is incomplete",
    );

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .args(["fetch", document_id])
        .assert()
        .code(3)
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    assert_json_error_contains(
        &output,
        "index_unavailable",
        "projection coverage gate is incomplete",
    );
    Ok(())
}

#[test]
fn retrieval_commands_reject_empty_initialized_index() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("CLI retrieval empty index gate")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-cli-retrieval-empty-gate.")
        .tempdir()
        .map_err(StorageError::Io)?;

    {
        let _postgres =
            jurisearch_storage::runtime::ManagedPostgres::start_durable(pg_config, root.path())?;
    }

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
    assert_json_error_contains(
        &output,
        "index_unavailable",
        "projection coverage gate is incomplete",
    );

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .args(["fetch", "legi:LEGIARTI000006419320@1804-02-21"])
        .assert()
        .code(3)
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    assert_json_error_contains(
        &output,
        "index_unavailable",
        "projection coverage gate is incomplete",
    );
    Ok(())
}

#[test]
fn bad_input_is_json_and_uses_exit_code_2() {
    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env_remove("JURISEARCH_INDEX_DIR")
        .args(["search", ""])
        .assert()
        .code(2)
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["code"], "bad_input");

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env_remove("JURISEARCH_INDEX_DIR")
        .args(["search", "!!!", "--mode", "dense"])
        .assert()
        .code(2)
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["code"], "bad_input");
    assert!(
        json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("at least one searchable token")
    );

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env_remove("JURISEARCH_INDEX_DIR")
        .args([
            "search",
            "responsabilite civile",
            "--cursor",
            "not-a-cursor",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["code"], "bad_input");
    assert!(
        json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("cursor value returned by a previous search candidate")
    );
}

#[test]
fn retrieval_command_without_index_is_json_and_uses_exit_code_3() {
    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env_remove("JURISEARCH_INDEX_DIR")
        .args(["fetch", "legi:LEGIARTI000000000000@2020-01-01"])
        .assert()
        .code(3)
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["code"], "index_unavailable");
}

#[test]
fn eval_phase1_rejects_zero_top_k_before_opening_index() {
    let output = jurisearch_command_without_embedding_env()
        .env_remove("JURISEARCH_INDEX_DIR")
        .env("JURISEARCH_CONFIG", "none")
        .args(["eval", "phase1", "--top-k", "0"])
        .assert()
        .code(2)
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["code"], "bad_input");
    assert!(
        json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("eval phase1 --top-k")
    );
}

#[test]
fn eval_phase1_without_index_is_json_and_uses_exit_code_3() {
    let output = jurisearch_command_without_embedding_env()
        .env_remove("JURISEARCH_INDEX_DIR")
        .env("JURISEARCH_CONFIG", "none")
        .args(["eval", "phase1", "--mode", "bm25"])
        .assert()
        .code(3)
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["code"], "index_unavailable");
}

#[test]
fn ingest_embed_chunks_rejects_zero_limit_before_opening_index() {
    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env_remove("JURISEARCH_INDEX_DIR")
        .args(["ingest", "embed-chunks", "--limit", "0"])
        .assert()
        .code(2)
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["code"], "bad_input");
}

#[test]
fn ingest_embed_chunks_rejects_zero_index_lists_before_opening_index() {
    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env_remove("JURISEARCH_INDEX_DIR")
        .args(["ingest", "embed-chunks", "--index-lists", "0"])
        .assert()
        .code(2)
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["code"], "bad_input");
}

#[test]
fn ingest_embed_chunks_rejects_zero_pool_knobs_before_opening_index() {
    for args in [
        ["ingest", "embed-chunks", "--batch-size", "0"],
        ["ingest", "embed-chunks", "--pool-concurrency", "0"],
    ] {
        let output = Command::cargo_bin("jurisearch")
            .unwrap()
            .env_remove("JURISEARCH_INDEX_DIR")
            .args(args)
            .assert()
            .code(2)
            .stderr(predicate::str::is_empty())
            .get_output()
            .stdout
            .clone();

        let json: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(json["ok"], false);
        assert_eq!(json["error"]["code"], "bad_input");
    }
}

#[test]
fn ingest_legi_archives_rejects_zero_limit_before_opening_index() {
    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env_remove("JURISEARCH_INDEX_DIR")
        .args([
            "ingest",
            "legi-archives",
            "--archives-dir",
            "/tmp",
            "--limit-members",
            "0",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["code"], "bad_input");
}

#[test]
fn ingest_legi_archives_records_accounting_and_quarantines_failures()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(pg_config) = discover_pg_config("CLI LEGI archive ingest")? else {
        return Ok(());
    };
    let index = tempfile::Builder::new()
        .prefix("jurisearch-cli-legi-ingest.")
        .tempdir()?;
    let archives = tempfile::Builder::new()
        .prefix("jurisearch-cli-legi-archives.")
        .tempdir()?;
    let quarantine = tempfile::Builder::new()
        .prefix("jurisearch-cli-legi-quarantine.")
        .tempdir()?;
    let archive_path = archives
        .path()
        .join("Freemium_legi_global_20250101-000000.tar.gz");
    let article = article_fixture().replace(
        "  </LIENS>",
        r#"    <LIEN_SECTION_TA id="LEGISCTA000006089696" debut="1804-03-21" fin="2999-01-01"/>
  </LIENS>"#,
    );
    write_tar_gz(
        archive_path.as_path(),
        &[
            ("legi/articles/LEGIARTI000006419320.xml", article.as_bytes()),
            (
                "legi/sections/SECTION.xml",
                br#"<SECTION_TA>
  <ID>LEGISCTA000006089696</ID>
  <TITRE_TA>Titre preliminaire</TITRE_TA>
  <CONTEXTE>
    <TEXTE cid="LEGITEXT000006070721">
      <TITRE_TXT debut="1804-03-21" fin="2999-01-01">Code civil</TITRE_TXT>
      <TM>
        <TITRE_TM>Livre III : Des differentes manieres dont on acquiert la propriete</TITRE_TM>
        <TM>
          <TITRE_TM>Titre IV : Des engagements qui se forment sans convention</TITRE_TM>
        </TM>
      </TM>
    </TEXTE>
  </CONTEXTE>
</SECTION_TA>"#,
            ),
            (
                "legi/textes/LEGITEXT000049371154.xml",
                br#"<TEXTE_VERSION>
  <META>
    <META_COMMUN>
      <ID>LEGITEXT000049371154</ID>
      <URL>/id/LEGITEXT000049371154</URL>
      <NATURE/>
    </META_COMMUN>
    <META_SPEC>
      <META_TEXTE_VERSION>
        <TITRE>Arrete du 12 avril 1956</TITRE>
        <ETAT>VIGUEUR</ETAT>
        <DATE_DEBUT>1956-04-12</DATE_DEBUT>
        <DATE_FIN>2999-01-01</DATE_FIN>
      </META_TEXTE_VERSION>
    </META_SPEC>
  </META>
</TEXTE_VERSION>"#,
            ),
            (
                "legi/textelr/LEGITEXT000006070721.xml",
                br#"<TEXTELR>
  <META>
    <META_COMMUN>
      <ID>LEGITEXT000006070721</ID>
      <URL>/id/LEGITEXT000006070721</URL>
      <NATURE>CODE</NATURE>
    </META_COMMUN>
    <META_SPEC>
      <META_TEXTE_CHRONICLE>
        <CID>LEGITEXT000006070721</CID>
        <NUM>civil</NUM>
        <DATE_PUBLI>1804-03-21</DATE_PUBLI>
        <DATE_TEXTE>1804-03-21</DATE_TEXTE>
      </META_TEXTE_CHRONICLE>
    </META_SPEC>
  </META>
  <STRUCT>
    <LIEN_TXT id="LEGITEXT000006070721" debut="1804-03-21"/>
  </STRUCT>
</TEXTELR>"#,
            ),
            ("legi/articles/BROKEN.xml", b"<ARTICLE><META/></ARTICLE>"),
        ],
    )?;

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .arg("--index-dir")
        .arg(index.path())
        .args(["ingest", "legi-archives", "--archives-dir"])
        .arg(archives.path())
        .args([
            "--run-id",
            "run-cli",
            "--limit-members",
            "5",
            "--quarantine-dir",
        ])
        .arg(quarantine.path())
        .arg("--safe-mode")
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["command"], "ingest legi-archives");
    assert_eq!(json["run_id"], "run-cli");
    assert_eq!(json["run_status"], "failed");
    assert_eq!(json["safe_mode"], true);
    assert_eq!(json["visited_members"], 5);
    assert_eq!(json["inserted_documents"], 1);
    assert_eq!(json["parsed_metadata_members"], 3);
    assert_eq!(json["persisted_metadata_members"], 3);
    assert_eq!(json["hierarchy_backfill_scoped_documents"], 1);
    assert_eq!(json["hierarchy_backfill_scoped_sections"], 1);
    assert_eq!(json["hierarchy_backfill_scoped_texts"], 1);
    assert_eq!(json["hierarchy_backfilled_documents"], 1);
    assert_eq!(json["hierarchy_backfill_invalidated_embeddings"], 0);
    assert_eq!(json["skipped_members"], 3);
    assert_eq!(json["failed_members"], 1);
    assert_eq!(json["quarantined_payloads"], 1);
    assert_eq!(json["parsed_metadata_roots"]["SECTION_TA"], 1);
    assert_eq!(json["parsed_metadata_roots"]["TEXTELR"], 1);
    assert_eq!(json["parsed_metadata_roots"]["TEXTE_VERSION"], 1);
    assert_eq!(json["manifest"]["source"], "legi");
    assert_eq!(json["manifest"]["dataset"], "LEGI");
    assert_eq!(json["manifest"]["run_status"], "failed");
    assert_eq!(json["manifest"]["complete"], false);
    assert_eq!(json["manifest"]["source_version"], "20250101-000000");
    assert_eq!(
        json["manifest"]["freshness"]["latest_archive"],
        "Freemium_legi_global_20250101-000000.tar.gz"
    );
    assert_eq!(json["manifest"]["coverage"]["visited_members"], 5);
    assert_eq!(
        json["manifest"]["coverage"]["hierarchy_backfill_scoped_documents"],
        1
    );
    assert_eq!(
        json["manifest"]["coverage"]["hierarchy_backfill_scoped_sections"],
        1
    );
    assert_eq!(
        json["manifest"]["coverage"]["hierarchy_backfill_scoped_texts"],
        1
    );
    assert_eq!(
        json["manifest"]["coverage"]["hierarchy_backfilled_documents"],
        1
    );
    assert!(json["unsupported_roots"].as_object().unwrap().is_empty());

    let quarantine_entries =
        fs::read_dir(quarantine.path().join("run-cli"))?.collect::<Result<Vec<_>, _>>()?;
    assert_eq!(quarantine_entries.len(), 1);

    let postgres = ManagedPostgres::start_durable(pg_config.clone(), index.path())?;
    assert_eq!(
        postgres.execute_sql("SELECT count(*)::text FROM documents;")?,
        "1"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT string_agg(status || ':' || member_count::text, ',' ORDER BY status) \
             FROM (SELECT status, count(*) AS member_count FROM ingest_member GROUP BY status) s;",
        )?,
        "failed:1,inserted:1,skipped:3"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT source_entity FROM ingest_member \
             WHERE member_path = 'legi/sections/SECTION.xml';",
        )?,
        "LEGISCTA000006089696"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT source_entity FROM ingest_member \
             WHERE member_path = 'legi/textes/LEGITEXT000049371154.xml';",
        )?,
        "LEGITEXT000049371154"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT string_agg(root_kind || ':' || source_uid, ',' ORDER BY root_kind, source_uid) \
             FROM legi_metadata_roots;",
        )?,
        "SECTION_TA:LEGISCTA000006089696,TEXTELR:LEGITEXT000006070721,TEXTE_VERSION:LEGITEXT000049371154"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT coalesce(canonical_json->>'nature', 'absent') \
             FROM legi_metadata_roots \
             WHERE root_kind = 'TEXTE_VERSION';",
        )?,
        "absent"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT canonical_json->>'num' \
             FROM legi_metadata_roots \
             WHERE root_kind = 'TEXTELR';",
        )?,
        "civil"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT canonical_json->'hierarchy_path'->>3 \
             FROM documents \
             WHERE document_id = 'legi:LEGIARTI000006419320@1804-02-21';",
        )?,
        "Titre preliminaire"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT (canonical_json->'chunks'->0->>'contextualized_body') \
                    LIKE '%Titre preliminaire%Article 1240%' \
             FROM documents \
             WHERE document_id = 'legi:LEGIARTI000006419320@1804-02-21';",
        )?,
        "t"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT contextualized_body LIKE '%Titre preliminaire%Article 1240%', \
                    hierarchy_path->>3, \
                    chunking || ':' || boundary \
             FROM chunks \
             WHERE document_id = 'legi:LEGIARTI000006419320@1804-02-21';",
        )?,
        "t|Titre preliminaire|structural:article"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT string_agg(error_code, ',' ORDER BY error_code) FROM ingest_error;"
        )?,
        "validation_missing_required_field"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT count(*)::text FROM pg_class \
             WHERE relkind = 'i' AND relname = 'chunks_bm25_idx';"
        )?,
        "1"
    );
    drop(postgres);

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .arg("--index-dir")
        .arg(index.path())
        .arg("status")
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["ingest_health"]["latest_manifest"]["source"], "legi");
    assert_eq!(
        json["ingest_health"]["latest_manifest"]["run_status"],
        "failed"
    );
    assert_eq!(json["ingest_health"]["latest_manifest"]["complete"], false);
    assert_eq!(
        json["ingest_health"]["latest_manifest"]["freshness"]["latest_archive_timestamp"],
        "20250101-000000"
    );
    assert_eq!(
        json["ingest_health"]["latest_manifest"]["coverage"]["persisted_metadata_members"],
        3
    );

    {
        let postgres = ManagedPostgres::start_durable(pg_config.clone(), index.path())?;
        postgres.execute_sql(
            "UPDATE documents \
             SET canonical_json = jsonb_set(canonical_json, '{hierarchy_path}', '[\"Code civil\"]'::jsonb), \
                 hierarchy_path = '[\"Code civil\"]'::jsonb \
             WHERE document_id = 'legi:LEGIARTI000006419320@1804-02-21'; \
             UPDATE chunks \
             SET contextualized_body = body, hierarchy_path = '[]'::jsonb \
             WHERE document_id = 'legi:LEGIARTI000006419320@1804-02-21';",
        )?;
    }

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .arg("--index-dir")
        .arg(index.path())
        .args(["ingest", "legi-archives", "--archives-dir"])
        .arg(archives.path())
        .args(["--run-id", "run-cli-resume", "--limit-members", "1"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["run_status"], "completed");
    assert_eq!(json["visited_members"], 1);
    assert_eq!(json["inserted_documents"], 0);
    assert_eq!(json["skipped_members"], 1);
    assert_eq!(json["skipped_compatible_members"], 1);
    assert_eq!(json["hierarchy_backfill_scoped_documents"], 0);
    assert_eq!(json["hierarchy_backfilled_documents"], 1);

    {
        let postgres = ManagedPostgres::start_durable(pg_config.clone(), index.path())?;
        assert_eq!(
            postgres.execute_sql(
                "SELECT canonical_json->'hierarchy_path'->>3 \
                 FROM documents \
                 WHERE document_id = 'legi:LEGIARTI000006419320@1804-02-21';",
            )?,
            "Titre preliminaire"
        );
        assert_eq!(
            postgres.execute_sql(
                "SELECT contextualized_body LIKE '%Titre preliminaire%Article 1240%', \
                        hierarchy_path->>3 \
                 FROM chunks \
                 WHERE document_id = 'legi:LEGIARTI000006419320@1804-02-21';",
            )?,
            "t|Titre preliminaire"
        );
        assert_eq!(
            postgres.execute_sql(
                "SELECT count(*)::text FROM pg_class \
                 WHERE relkind = 'i' AND relname = 'chunks_bm25_idx';"
            )?,
            "1"
        );
    }

    write_tar_gz(
        archive_path.as_path(),
        &[("legi/articles/BROKEN.xml", b"<ARTICLE><META/></ARTICLE>")],
    )?;
    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .arg("--index-dir")
        .arg(index.path())
        .args(["ingest", "legi-archives", "--archives-dir"])
        .arg(archives.path())
        .args(["--run-id", "run-cli-retry", "--limit-members", "1"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["run_status"], "failed");
    assert_eq!(json["visited_members"], 1);
    assert_eq!(json["failed_members"], 1);

    let postgres = ManagedPostgres::start_durable(pg_config.clone(), index.path())?;
    assert_eq!(
        postgres.execute_sql(
            "SELECT status FROM ingest_member \
             WHERE run_id = 'run-cli-retry' AND member_path = 'legi/articles/BROKEN.xml';",
        )?,
        "failed"
    );
    assert_eq!(
        postgres
            .execute_sql("SELECT error_code FROM ingest_error WHERE run_id = 'run-cli-retry';")?,
        "validation_missing_required_field"
    );
    drop(postgres);

    let mutated_article =
        article_fixture().replace("Tout fait quelconque", "Tout autre fait quelconque");
    write_tar_gz(
        archive_path.as_path(),
        &[(
            "legi/articles/LEGIARTI000006419320.xml",
            mutated_article.as_bytes(),
        )],
    )?;
    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .arg("--index-dir")
        .arg(index.path())
        .args(["ingest", "legi-archives", "--archives-dir"])
        .arg(archives.path())
        .args([
            "--run-id",
            "run-cli-incompatible",
            "--limit-members",
            "1",
            "--quarantine-dir",
        ])
        .arg(quarantine.path())
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["run_status"], "failed");
    assert_eq!(json["visited_members"], 1);
    assert_eq!(json["failed_members"], 1);
    assert_eq!(json["quarantined_payloads"], 1);

    let postgres = ManagedPostgres::start_durable(pg_config, index.path())?;
    assert_eq!(
        postgres.execute_sql(
            "SELECT status FROM ingest_member \
             WHERE run_id = 'run-cli-incompatible' \
               AND member_path = 'legi/articles/LEGIARTI000006419320.xml';",
        )?,
        "failed"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT error_class || ':' || error_code \
             FROM ingest_error \
             WHERE run_id = 'run-cli-incompatible';",
        )?,
        "validation_error:compatibility_mismatch"
    );

    Ok(())
}

#[test]
fn ingest_legi_archives_same_run_resume_keeps_inserted_members_inserted()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(pg_config) = discover_pg_config("CLI LEGI same-run resume")? else {
        return Ok(());
    };
    let index = tempfile::Builder::new()
        .prefix("jurisearch-cli-legi-same-run-resume.")
        .tempdir()?;
    let archives = tempfile::Builder::new()
        .prefix("jurisearch-cli-legi-same-run-resume-archives.")
        .tempdir()?;
    let archive_path = archives
        .path()
        .join("Freemium_legi_global_20250101-000000.tar.gz");
    let article = article_fixture();
    write_tar_gz(
        archive_path.as_path(),
        &[("legi/articles/LEGIARTI000006419320.xml", article.as_bytes())],
    )?;

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .arg("--index-dir")
        .arg(index.path())
        .args(["ingest", "legi-archives", "--archives-dir"])
        .arg(archives.path())
        .args(["--run-id", "run-cli-same-resume"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["run_status"], "completed");
    assert_eq!(json["visited_members"], 1);
    assert_eq!(json["inserted_documents"], 1);

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .arg("--index-dir")
        .arg(index.path())
        .args(["ingest", "legi-archives", "--archives-dir"])
        .arg(archives.path())
        .args(["--run-id", "run-cli-same-resume"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["run_status"], "completed");
    assert_eq!(json["visited_members"], 1);
    assert_eq!(json["inserted_documents"], 0);
    assert_eq!(json["skipped_members"], 1);
    assert_eq!(json["skipped_compatible_members"], 1);

    let postgres = ManagedPostgres::start_durable(pg_config, index.path())?;
    assert_eq!(
        postgres.execute_sql(
            "SELECT status || ':' || attempt_count::text || ':' || coalesce(source_entity, 'none') \
             FROM ingest_member \
             WHERE run_id = 'run-cli-same-resume' \
               AND member_path = 'legi/articles/LEGIARTI000006419320.xml';",
        )?,
        "inserted:1:LEGIARTI000006419320"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT count(*)::text \
             FROM ingest_member \
             WHERE run_id = 'run-cli-same-resume';",
        )?,
        "1"
    );

    Ok(())
}

#[test]
fn ingest_legi_archives_skips_no_text_articles_without_failing_run()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(pg_config) = discover_pg_config("CLI LEGI no-text article skip")? else {
        return Ok(());
    };
    let index = tempfile::Builder::new()
        .prefix("jurisearch-cli-legi-no-text.")
        .tempdir()?;
    let archives = tempfile::Builder::new()
        .prefix("jurisearch-cli-legi-no-text-archives.")
        .tempdir()?;
    let quarantine = tempfile::Builder::new()
        .prefix("jurisearch-cli-legi-no-text-quarantine.")
        .tempdir()?;
    let archive_path = archives
        .path()
        .join("Freemium_legi_global_20250101-000000.tar.gz");
    let no_text_article = article_fixture_without_body();
    write_tar_gz(
        archive_path.as_path(),
        &[(
            "legi/articles/LEGIARTI000006419320.xml",
            no_text_article.as_bytes(),
        )],
    )?;

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .arg("--index-dir")
        .arg(index.path())
        .args(["ingest", "legi-archives", "--archives-dir"])
        .arg(archives.path())
        .args(["--run-id", "run-no-text", "--quarantine-dir"])
        .arg(quarantine.path())
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["command"], "ingest legi-archives");
    assert_eq!(json["run_status"], "completed");
    assert_eq!(json["visited_members"], 1);
    assert_eq!(json["inserted_documents"], 0);
    assert_eq!(json["skipped_members"], 1);
    assert_eq!(json["skipped_no_text_articles"], 1);
    assert_eq!(json["failed_members"], 0);
    assert_eq!(json["quarantined_payloads"], 0);
    assert_eq!(json["manifest"]["coverage"]["skipped_no_text_articles"], 1);
    assert!(
        json["parsed_metadata_roots"]
            .as_object()
            .unwrap()
            .is_empty()
    );
    assert!(json["unsupported_roots"].as_object().unwrap().is_empty());
    assert!(!quarantine.path().join("run-no-text").exists());

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .arg("--index-dir")
        .arg(index.path())
        .args(["ingest", "legi-archives", "--archives-dir"])
        .arg(archives.path())
        .args(["--run-id", "run-no-text-resume"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["run_status"], "completed");
    assert_eq!(json["visited_members"], 1);
    assert_eq!(json["skipped_members"], 1);
    assert_eq!(json["skipped_compatible_members"], 1);
    assert_eq!(json["skipped_no_text_articles"], 0);
    assert_eq!(json["failed_members"], 0);

    let postgres = ManagedPostgres::start_durable(pg_config, index.path())?;
    assert_eq!(
        postgres.execute_sql("SELECT count(*)::text FROM documents;")?,
        "0"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT status || ':' || coalesce(source_entity, 'none') \
             FROM ingest_member \
             WHERE run_id = 'run-no-text';",
        )?,
        "skipped:LEGIARTI000006419320"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT count(*)::text \
             FROM ingest_error \
             WHERE run_id = 'run-no-text';",
        )?,
        "0"
    );

    Ok(())
}

#[test]
fn ingest_legi_archives_keeps_non_body_article_errors_failed()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(pg_config) = discover_pg_config("CLI LEGI invalid article failure")? else {
        return Ok(());
    };
    let index = tempfile::Builder::new()
        .prefix("jurisearch-cli-legi-invalid-article.")
        .tempdir()?;
    let archives = tempfile::Builder::new()
        .prefix("jurisearch-cli-legi-invalid-article-archives.")
        .tempdir()?;
    let quarantine = tempfile::Builder::new()
        .prefix("jurisearch-cli-legi-invalid-article-quarantine.")
        .tempdir()?;
    let archive_path = archives
        .path()
        .join("Freemium_legi_global_20250101-000000.tar.gz");
    let invalid_article = article_fixture().replace(
        "<DATE_DEBUT>1804-02-21</DATE_DEBUT>",
        "<DATE_DEBUT>not-a-date</DATE_DEBUT>",
    );
    write_tar_gz(
        archive_path.as_path(),
        &[(
            "legi/articles/LEGIARTI000006419320.xml",
            invalid_article.as_bytes(),
        )],
    )?;

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .arg("--index-dir")
        .arg(index.path())
        .args(["ingest", "legi-archives", "--archives-dir"])
        .arg(archives.path())
        .args(["--run-id", "run-invalid-article", "--quarantine-dir"])
        .arg(quarantine.path())
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["run_status"], "failed");
    assert_eq!(json["visited_members"], 1);
    assert_eq!(json["skipped_members"], 0);
    assert_eq!(json["skipped_no_text_articles"], 0);
    assert_eq!(json["failed_members"], 1);
    assert_eq!(json["quarantined_payloads"], 1);

    let quarantine_entries = fs::read_dir(quarantine.path().join("run-invalid-article"))?
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(quarantine_entries.len(), 1);

    let postgres = ManagedPostgres::start_durable(pg_config, index.path())?;
    assert_eq!(
        postgres.execute_sql(
            "SELECT status \
             FROM ingest_member \
             WHERE run_id = 'run-invalid-article';",
        )?,
        "failed"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT error_code \
             FROM ingest_error \
             WHERE run_id = 'run-invalid-article';",
        )?,
        "validation_invalid_date"
    );

    Ok(())
}

#[test]
fn ingest_backfill_legi_hierarchy_updates_full_index() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("CLI LEGI hierarchy backfill")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-cli-legi-backfill.")
        .tempdir()
        .map_err(StorageError::Io)?;

    {
        let postgres = ManagedPostgres::start_durable(pg_config.clone(), root.path())?;
        postgres.execute_sql(
            r#"INSERT INTO legi_metadata_roots
                (metadata_key, root_kind, source_uid, parent_source_uid, title,
                 valid_from, valid_to, valid_to_raw, source_payload_hash,
                 canonical_version, canonical_json)
             VALUES
                ('legi:SECTION_TA:LEGISCTA000006089696@1804-03-21', 'SECTION_TA',
                 'LEGISCTA000006089696', 'LEGITEXT000006070721', 'Titre preliminaire',
                 '1804-03-21', NULL, '2999-01-01', 'sha256:section',
                 'legi_section_ta:v1',
                 '{"title":"Titre preliminaire","hierarchy_path":["Code civil","Livre III"]}');
             INSERT INTO documents
                (document_id, source, kind, source_uid, citation, title, body,
                 valid_from, source_payload_hash, canonical_json)
             VALUES
                ('legi:LEGIARTI000006419320@1804-02-21', 'legi', 'article',
                 'LEGIARTI000006419320', 'Code civil article 1240',
                 'Article 1240', 'Texte initial pour le test.',
                 '1804-02-21', 'sha256:article-1240',
                 '{"title":"Article 1240","hierarchy_path":["Code civil"],"chunks":[{"body":"Texte initial pour le test.","hierarchy_path":["Code civil"],"contextualized_body":"Code civil\nArticle 1240\nTexte initial pour le test."}]}');
             INSERT INTO chunks
                (chunk_id, document_id, chunk_index, body, contextualized_body, source_payload_hash,
                 chunk_builder_version, embedding_fingerprint)
             VALUES
                ('chunk:1240:0', 'legi:LEGIARTI000006419320@1804-02-21', 0,
                 'Texte initial pour le test.',
                 'Code civil\nArticle 1240\nTexte initial pour le test.',
                 'sha256:article-1240',
                 'chunker:v0', 'bge-m3:1024:normalize:true');
             INSERT INTO graph_edges
                (edge_id, from_document_id, edge_kind, edge_source, payload)
             VALUES
                ('edge:1240:section', 'legi:LEGIARTI000006419320@1804-02-21',
                 'structure', 'publisher',
                 '{"source_tag":"LIEN_SECTION_TA","to_source_uid":"LEGISCTA000006089696","attributes":[{"key":"debut","value":"1804-03-21"},{"key":"fin","value":"2999-01-01"}]}');"#,
        )?;
        postgres.execute_sql(&format!(
            "INSERT INTO chunk_embeddings \
                (chunk_id, embedding_fingerprint, embedding, model, dimension) \
             VALUES \
                ('chunk:1240:0', 'bge-m3:1024:normalize:true', '{}', 'bge-m3', 1024);",
            unit_vector_literal(0)
        ))?;
    }

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .arg("--index-dir")
        .arg(root.path())
        .args(["ingest", "backfill-legi-hierarchy"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["command"], "ingest backfill-legi-hierarchy");
    assert_eq!(json["scope"], "full");
    assert_eq!(json["hierarchy_backfilled_documents"], 1);
    assert_eq!(json["hierarchy_backfill_invalidated_embeddings"], 1);
    assert_eq!(json["embedding_rebuild_required"], true);
    assert_eq!(
        json["recommended_next_command"],
        "jurisearch ingest embed-chunks"
    );
    assert_eq!(json["replay_snapshot_cache"]["source"], "refreshed");
    assert_eq!(json["replay_snapshot_cache"]["status"], "available");
    assert_eq!(json["replay_snapshot_cache"]["documents"], 1);
    assert_eq!(json["replay_snapshot_cache"]["chunks"], 1);

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .arg("--index-dir")
        .arg(root.path())
        .args(["ingest", "backfill-legi-hierarchy"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["hierarchy_backfilled_documents"], 0);
    assert_eq!(json["hierarchy_backfill_invalidated_embeddings"], 0);
    assert_eq!(json["embedding_rebuild_required"], false);
    assert!(json["recommended_next_command"].is_null());

    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
    assert_eq!(
        postgres.execute_sql(
            "SELECT canonical_json->'hierarchy_path'->>2 \
             FROM documents \
             WHERE document_id = 'legi:LEGIARTI000006419320@1804-02-21';",
        )?,
        "Titre preliminaire"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT (canonical_json->'chunks'->0->>'contextualized_body') \
                    LIKE '%Titre preliminaire%Article 1240%Texte initial%' \
             FROM documents \
             WHERE document_id = 'legi:LEGIARTI000006419320@1804-02-21';",
        )?,
        "t"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT coalesce(embedding_fingerprint, 'cleared') \
             FROM chunks \
             WHERE chunk_id = 'chunk:1240:0';",
        )?,
        "cleared"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT count(*)::text \
             FROM chunk_embeddings \
             WHERE chunk_id = 'chunk:1240:0';",
        )?,
        "0"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT count(*)::text \
             FROM ingest_run \
             WHERE run_id = 'backfill-legi-hierarchy';",
        )?,
        "0"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT count(*)::text \
             FROM ingest_member;",
        )?,
        "0"
    );

    Ok(())
}

#[test]
fn fetch_returns_documents_from_existing_index() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("CLI fetch existing index")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-cli-fetch.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let document_id = "legi:LEGIARTI000006419320@1804-02-21";

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
                 '1804-02-21', 'sha256:article-1240', '{\"official\":true}'); \
             INSERT INTO chunks \
                (chunk_id, document_id, chunk_index, body, contextualized_body, source_payload_hash, \
                 chunk_builder_version, embedding_fingerprint) \
             VALUES \
                ('chunk:1240:0', 'legi:LEGIARTI000006419320@1804-02-21', 0, \
                 'responsabilite civile faute reparation dommage article 1240', \
                 'Code civil > Article 1240\nresponsabilite civile faute reparation dommage article 1240', \
                 'sha256:article-1240', 'chunker:v0', 'bge-m3:1024:normalize:true');",
        )?;
    }

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .args(["fetch", document_id])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["documents"][0]["document_id"], document_id);
    assert_eq!(
        json["documents"][0]["chunks"][0]["chunk_id"],
        "chunk:1240:0"
    );

    let input = format!(
        "{{\"id\":\"fetch-one\",\"command\":\"fetch\",\"args\":{{\"ids\":[\"{document_id}\"]}}}}\n"
    );
    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .args(["session", "--jsonl"])
        .write_stdin(input)
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["id"], "fetch-one");
    assert_eq!(json["ok"], true);
    assert_eq!(json["result"]["documents"][0]["document_id"], document_id);

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .args(["fetch", "legi:LEGIARTI999999999999@2024-01-01"])
        .assert()
        .code(2)
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["code"], "no_results");

    // `fetch --as-of` / `--part` were removed (T0.4): they only ever errored, so the flags no longer
    // exist. fetch is exact, version-pinned retrieval; date-resolved fetch is deferred to a real
    // feature (alongside `versions`/`diff`). A now-unknown `--as-of` is a clap usage error, not a
    // JSON contract error, so there is nothing more to assert here.
    Ok(())
}

#[test]
fn cite_resolves_local_statutory_citations_and_strict_states() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("CLI cite existing index")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-cli-cite.")
        .tempdir()
        .map_err(StorageError::Io)?;

    {
        let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
        postgres.execute_sql(
            r#"INSERT INTO documents
                (document_id, source, kind, source_uid, citation, title, body,
                 valid_from, valid_to, valid_to_raw, source_payload_hash, canonical_json)
             VALUES
                ('legi:LEGIARTI000006419320@1804-02-21', 'legi', 'article',
                 'LEGIARTI000006419320', 'Code civil article 1240',
                 'Article 1240', 'Responsabilite civile courante.',
                 '1804-02-21', NULL, '2999-01-01', 'sha256:civil-1240',
                 '{"official":true}'),
                ('legi:LEGIARTI000000001240@2020-01-01', 'legi', 'article',
                 'LEGIARTI000000001240', 'Code des assurances article 1240',
                 'Article 1240', 'Autre article courant avec le meme numero.',
                 '2020-01-01', NULL, '2999-01-01', 'sha256:assurance-1240',
                 '{"official":true}'),
                ('legi:LEGIARTI000000121001@2020-01-01', 'legi', 'article',
                 'LEGIARTI000000121001', 'Code de la consommation article L121-1',
                 'Article L121-1', 'Article prefixe courant.',
                 '2020-01-01', NULL, '2999-01-01', 'sha256:conso-l121-1',
                 '{"official":true}'),
                ('legi:LEGIARTI000000000888@1900-01-01', 'legi', 'article',
                 'LEGIARTI000000000888', 'Code civil article 88',
                 'Article 88', 'Ancienne version historique.',
                 '1900-01-01', '2000-01-01', '2000-01-01', 'sha256:article-88-old',
                 '{"official":true}'),
                ('legi:LEGIARTI000000000888@2000-01-01', 'legi', 'article',
                 'LEGIARTI000000000888', 'Code civil article 88',
                 'Article 88', 'Version courante.',
                 '2000-01-01', NULL, '2999-01-01', 'sha256:article-88-current',
                 '{"official":true}'),
                ('legi:LEGIARTI000000000777@1900-01-01', 'legi', 'article',
                 'LEGIARTI000000000777', 'Code civil article 777',
                 'Article 777', 'Version abrogee.',
                 '1900-01-01', '2000-01-01', '2000-01-01', 'sha256:article-777-old',
                 '{"official":true}');
             INSERT INTO chunks
                (chunk_id, document_id, chunk_index, body, contextualized_body, source_payload_hash,
                 chunk_builder_version, embedding_fingerprint)
             VALUES
                ('chunk:civil-1240:0', 'legi:LEGIARTI000006419320@1804-02-21', 0,
                 'Responsabilite civile courante.', 'Code civil > Article 1240',
                 'sha256:civil-1240', 'chunker:v0', NULL),
                ('chunk:assurance-1240:0', 'legi:LEGIARTI000000001240@2020-01-01', 0,
                 'Autre article courant avec le meme numero.', 'Code des assurances > Article 1240',
                 'sha256:assurance-1240', 'chunker:v0', NULL),
                ('chunk:conso-l121-1:0', 'legi:LEGIARTI000000121001@2020-01-01', 0,
                 'Article prefixe courant.', 'Code de la consommation > Article L121-1',
                 'sha256:conso-l121-1', 'chunker:v0', NULL),
                ('chunk:article-88-old:0', 'legi:LEGIARTI000000000888@1900-01-01', 0,
                 'Ancienne version historique.', 'Code civil > Article 88',
                 'sha256:article-88-old', 'chunker:v0', NULL),
                ('chunk:article-88-current:0', 'legi:LEGIARTI000000000888@2000-01-01', 0,
                 'Version courante.', 'Code civil > Article 88',
                 'sha256:article-88-current', 'chunker:v0', NULL),
                ('chunk:article-777-old:0', 'legi:LEGIARTI000000000777@1900-01-01', 0,
                 'Version abrogee.', 'Code civil > Article 777',
                 'sha256:article-777-old', 'chunker:v0', NULL);
             INSERT INTO legi_metadata_roots
                (metadata_key, root_kind, source_uid, parent_source_uid, title,
                 valid_from, valid_to, valid_to_raw, source_payload_hash, canonical_version, canonical_json)
             VALUES
                ('legi:SECTION_TA:LEGISCTA000006089696@1804-03-21', 'SECTION_TA',
                 'LEGISCTA000006089696', 'LEGITEXT000006070721', 'Titre preliminaire',
                 '1804-03-21', NULL, '2999-01-01', 'sha256:section',
                 'legi_section_ta:v1', '{"title":"Titre preliminaire"}'),
                ('legi:TEXTELR:LEGITEXT000006070721@1804-03-21:nor', 'TEXTELR',
                 'LEGITEXT000006070721', NULL, NULL,
                 '1804-03-21', NULL, NULL, 'sha256:textelr',
                 'legi_textelr:v1', '{"text_id":"LEGITEXT000006070721","nor":"JUSC2301234L"}');"#,
        )?;
    }

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .args(["cite", "LEGIARTI000006419320", "--as-of", "2024-01-01"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["state"], "exact");
    assert_eq!(json["input_class"], "legiarti");
    assert_eq!(json["valid_match_count"], 1);
    assert_eq!(
        json["matches"][0]["document_id"],
        "legi:LEGIARTI000006419320@1804-02-21"
    );

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .args(["cite", "Code civil article 1240", "--as-of", "2024-01-01"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["state"], "normalized");
    assert_eq!(json["input_class"], "free_text_article");
    assert_eq!(json["match_count"], 1);

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .args([
            "cite",
            "Code de la consommation article L. 121-1",
            "--as-of",
            "2024-01-01",
        ])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["state"], "normalized");
    assert_eq!(json["normalized"], "l121-1");
    assert_eq!(
        json["matches"][0]["document_id"],
        "legi:LEGIARTI000000121001@2020-01-01"
    );

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .args([
            "cite",
            "Code des assurances article 1240",
            "--as-of",
            "2024-01-01",
        ])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["state"], "normalized");
    assert_eq!(
        json["matches"][0]["document_id"],
        "legi:LEGIARTI000000001240@2020-01-01"
    );

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .args(["cite", "article 1240", "--as-of", "2024-01-01"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["state"], "ambiguous");
    assert_eq!(json["valid_match_count"], 2);

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .args(["cite", "LEGIARTI000000000888", "--as-of", "1999-01-01"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["state"], "exact");
    assert!(json["matches"].as_array().unwrap().iter().any(|candidate| {
        candidate["document_id"] == "legi:LEGIARTI000000000888@1900-01-01"
            && candidate["valid_on_as_of"] == true
    }));

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .args(["cite", "LEGIARTI000000000777", "--as-of", "2024-01-01"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["state"], "stale_version");
    assert_eq!(json["valid_match_count"], 0);

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .args(["cite", "JUSC2301234L", "--as-of", "2024-01-01"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["state"], "exact");
    assert_eq!(json["input_class"], "nor");
    assert_eq!(
        json["matches"][0]["metadata_key"],
        "legi:TEXTELR:LEGITEXT000006070721@1804-03-21:nor"
    );

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env_remove("JURISEARCH_INDEX_DIR")
        .args(["cite", "not a citation"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["state"], "not_found");
    assert_eq!(json["input_class"], "malformed");

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env_remove("JURISEARCH_INDEX_DIR")
        .env_remove("JURISEARCH_PISTE_LEGIFRANCE_CLIENT_ID")
        .env_remove("JURISEARCH_PISTE_LEGIFRANCE_CLIENT_SECRET")
        .args(["cite", "not a citation", "--online"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["state"], "not_found");
    assert_eq!(json["online"]["requested"], true);
    assert_eq!(json["online"]["checked"], false);
    assert!(
        json["online"]["note"]
            .as_str()
            .is_some_and(|note| note.contains("not sent"))
    );

    let online_base_url = spawn_server(2, |request| {
        if request.starts_with("POST /api/oauth/token ") {
            assert!(request.contains("grant_type=client_credentials"));
            assert!(request.contains("scope=openid"));
            ok_json(r#"{"access_token":"token-123","expires_in":3600}"#)
        } else {
            assert!(request.starts_with("POST /dila/legifrance/lf-engine-app/search "));
            assert!(request.contains("\r\nAuthorization: Bearer token-123\r\n"));
            assert!(request.contains(r#""query":"LEGIARTI999999999999""#));
            ok_json(r#"{"results":[]}"#)
        }
    });
    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .env("JURISEARCH_PISTE_ENV", "sandbox")
        .env("JURISEARCH_PISTE_API_BASE_URL", &online_base_url)
        .env("JURISEARCH_PISTE_OAUTH_BASE_URL", &online_base_url)
        .env("JURISEARCH_PISTE_LEGIFRANCE_CLIENT_ID", "client-id")
        .env("JURISEARCH_PISTE_LEGIFRANCE_CLIENT_SECRET", "client-secret")
        .args(["cite", "LEGIARTI999999999999", "--online"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["state"], "source_unavailable");
    assert_eq!(json["local_state"], "not_found");
    assert_eq!(json["online"]["requested"], true);
    assert_eq!(json["online"]["checked"], true);
    assert_eq!(json["online"]["provider"], "legifrance");

    let failing_online_base_url = spawn_server(2, |request| {
        if request.starts_with("POST /api/oauth/token ") {
            ok_json(r#"{"access_token":"token-500","expires_in":3600}"#)
        } else {
            "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 11\r\n\r\nserver down".to_owned()
        }
    });
    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .env("JURISEARCH_PISTE_ENV", "sandbox")
        .env("JURISEARCH_PISTE_API_BASE_URL", &failing_online_base_url)
        .env("JURISEARCH_PISTE_OAUTH_BASE_URL", &failing_online_base_url)
        .env("JURISEARCH_PISTE_LEGIFRANCE_CLIENT_ID", "client-id")
        .env("JURISEARCH_PISTE_LEGIFRANCE_CLIENT_SECRET", "client-secret")
        // Probe failure must map fast: disable upstream retries so the 500 mock needs one request.
        .env("JURISEARCH_PISTE_MAX_RETRIES", "0")
        .args([
            "cite",
            "Code civil article 1240",
            "--as-of",
            "2024-01-01",
            "--online",
        ])
        .assert()
        .code(5)
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    assert_json_error_contains(&output, "upstream", "official API returned HTTP status 500");

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .args(["cite", "article 1240", "--as-of", "2024-01-01", "--strict"])
        .assert()
        .code(2)
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    assert_json_error_contains(&output, "no_results", "ambiguous");

    let input = format!(
        "{{\"id\":\"cite-one\",\"command\":\"cite\",\"args\":{{\"cite\":\"Code civil article 1240\",\"as_of\":\"2024-01-01\",\"index_dir\":\"{}\"}}}}\n",
        root.path().display()
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
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["id"], "cite-one");
    assert_eq!(json["ok"], true);
    assert_eq!(json["result"]["state"], "normalized");

    Ok(())
}

#[test]
fn cite_free_text_matches_ingested_legi_article_citation() -> Result<(), Box<dyn std::error::Error>>
{
    let Some(_pg_config) = discover_pg_config("CLI cite ingested LEGI article")? else {
        return Ok(());
    };
    let index = tempfile::Builder::new()
        .prefix("jurisearch-cli-cite-ingested.")
        .tempdir()?;
    let archives = tempfile::Builder::new()
        .prefix("jurisearch-cli-cite-archives.")
        .tempdir()?;
    let archive_path = archives
        .path()
        .join("Freemium_legi_global_20250101-000000.tar.gz");
    let article = article_fixture();
    write_tar_gz(
        archive_path.as_path(),
        &[("legi/articles/LEGIARTI000006419320.xml", article.as_bytes())],
    )?;

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .arg("--index-dir")
        .arg(index.path())
        .args(["ingest", "legi-archives", "--archives-dir"])
        .arg(archives.path())
        .args(["--run-id", "run-cite-ingested"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["run_status"], "completed");
    assert_eq!(json["inserted_documents"], 1);

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .arg("--index-dir")
        .arg(index.path())
        .args(["cite", "Code civil article 1240", "--as-of", "2024-01-01"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["state"], "normalized");
    assert_eq!(json["match_count"], 1);
    assert_eq!(
        json["matches"][0]["document_id"],
        "legi:LEGIARTI000006419320@1804-02-21"
    );

    Ok(())
}

#[test]
fn context_returns_hierarchy_and_siblings_from_existing_index() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("CLI context existing index")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-cli-context.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let document_id = "legi:LEGIARTI000006419320@1804-02-21";

    {
        let postgres =
            jurisearch_storage::runtime::ManagedPostgres::start_durable(pg_config, root.path())?;
        postgres.execute_sql(
            "INSERT INTO documents \
                (document_id, source, kind, source_uid, citation, title, body, \
                 valid_from, valid_to, source_payload_hash, hierarchy_path, canonical_json) \
             VALUES \
                ('legi:LEGIARTI000006419320@1804-02-21', 'legi', 'article', \
                 'LEGIARTI000006419320', 'Code civil article 1240', \
                 'Article 1240', 'Responsabilite civile.', '1804-02-21', NULL, \
                 'sha256:article-1240', \
                 '[\"Code civil\",\"Livre III\",\"Titre IV\"]'::jsonb, \
                 '{\"hierarchy_path\":[\"Code civil\",\"Livre III\",\"Titre IV\"]}'), \
                ('legi:LEGIARTI000006419321@1804-02-21', 'legi', 'article', \
                 'LEGIARTI000006419321', 'Code civil article 1241', \
                 'Article 1241', 'Responsabilite voisine.', '1804-02-21', NULL, \
                 'sha256:article-1241', \
                 '[\"Code civil\",\"Livre III\",\"Titre IV\"]'::jsonb, \
                 '{\"hierarchy_path\":[\"Code civil\",\"Livre III\",\"Titre IV\"]}'), \
                ('legi:LEGIARTI000006419322@2025-01-01', 'legi', 'article', \
                 'LEGIARTI000006419322', 'Code civil article futur', \
                 'Article futur', 'Future section article.', '2025-01-01', NULL, \
                 'sha256:article-future', \
                 '[\"Code civil\",\"Livre III\",\"Titre IV\"]'::jsonb, \
                 '{\"hierarchy_path\":[\"Code civil\",\"Livre III\",\"Titre IV\"]}'); \
             INSERT INTO chunks \
                (chunk_id, document_id, chunk_index, body, contextualized_body, chunking, \
                 boundary, hierarchy_path, source_payload_hash, chunk_builder_version) \
             VALUES \
                ('chunk:1240:0', 'legi:LEGIARTI000006419320@1804-02-21', 0, \
                 'Responsabilite civile.', 'Code civil > Livre III > Titre IV > Article 1240', \
                 'structural', 'article', '[\"Code civil\",\"Livre III\",\"Titre IV\"]', \
                 'sha256:article-1240', 'chunker:v1'), \
                ('chunk:1241:0', 'legi:LEGIARTI000006419321@1804-02-21', 0, \
                 'Responsabilite voisine.', 'Code civil > Livre III > Titre IV > Article 1241', \
                 'structural', 'article', '[\"Code civil\",\"Livre III\",\"Titre IV\"]', \
                 'sha256:article-1241', 'chunker:v1'), \
                ('chunk:future:0', 'legi:LEGIARTI000006419322@2025-01-01', 0, \
                 'Future section article.', 'Code civil > Livre III > Titre IV > Article futur', \
                 'structural', 'article', '[\"Code civil\",\"Livre III\",\"Titre IV\"]', \
                 'sha256:article-future', 'chunker:v1');",
        )?;
    }

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .args([
            "context",
            document_id,
            "--siblings",
            "--as-of",
            "2024-01-01",
        ])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["target"]["document_id"], document_id);
    assert_eq!(json["ancestry"][0]["title"], "Code civil");
    assert_eq!(json["sibling_count"], 1);
    assert_eq!(json["sibling_limit"], 50);
    assert_eq!(json["sibling_truncated"], false);
    assert_eq!(
        json["siblings"][0]["document_id"],
        "legi:LEGIARTI000006419321@1804-02-21"
    );

    let input = format!(
        "{{\"id\":\"context-one\",\"command\":\"context\",\"args\":{{\"id\":\"{document_id}\",\"siblings\":true,\"as_of\":\"2024-01-01\"}}}}\n"
    );
    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .args(["session", "--jsonl"])
        .write_stdin(input)
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["id"], "context-one");
    assert_eq!(json["ok"], true);
    assert_eq!(json["result"]["target"]["document_id"], document_id);

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env_remove("JURISEARCH_INDEX_DIR")
        .args(["context", document_id, "--as-of", "20240101"])
        .assert()
        .code(2)
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["code"], "bad_input");

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env_remove("JURISEARCH_INDEX_DIR")
        .args(["context", document_id, "--as-of", "2024-13-01"])
        .assert()
        .code(2)
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["code"], "bad_input");

    Ok(())
}

#[test]
fn ingest_embed_chunks_truncates_over_budget_input_and_reports_count() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("CLI embed chunk truncation")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-cli-embed-budget.")
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
                 'Article 1240', 'Texte long pour le test.', \
                 '1804-02-21', 'sha256:article-1240', \
                 '{\"chunks\":[{\"contextualized_body\":\"abcde\"}]}'); \
             INSERT INTO chunks \
                (chunk_id, document_id, chunk_index, body, contextualized_body, source_payload_hash, \
                 chunk_builder_version, embedding_fingerprint) \
             VALUES \
                ('chunk:1240:0', 'legi:LEGIARTI000006419320@1804-02-21', 0, \
                 'abcde', \
                 'abcde', \
                 'sha256:article-1240', 'chunker:v0', NULL);",
        )?;
    }

    let endpoint = spawn_server(1, |request| {
        assert!(request.starts_with("POST /v1/embeddings "));
        assert!(request.contains(r#""input":["abcd"]"#));
        assert!(!request.contains(r#""abcde""#));
        ok_json(&embedding_response_json(0))
    });

    let output = jurisearch_command_without_embedding_env()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .env("JURISEARCH_CONFIG", "none")
        .env("JURISEARCH_EMBED_BASE_URL", format!("{endpoint}/v1"))
        .env("JURISEARCH_EMBED_MAX_INPUT_CHARS", "4")
        .args(["ingest", "embed-chunks", "--index-lists", "1"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["command"], "ingest embed-chunks");
    assert_eq!(json["chunks_considered"], 1);
    assert_eq!(json["embeddings_inserted"], 1);
    assert_eq!(json["embedding_inputs_truncated"], 1);
    assert_eq!(json["endpoint_pool"]["endpoints"][0]["truncated_inputs"], 1);

    Ok(())
}

#[test]
fn ingest_embed_chunks_uses_endpoint_pool_and_finalizes_dense_index()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(pg_config) = discover_pg_config("CLI embed chunk endpoint pool")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-cli-embed-pool.")
        .tempdir()?;

    {
        let postgres = ManagedPostgres::start_durable(pg_config.clone(), root.path())?;
        postgres.execute_sql(
            "INSERT INTO documents \
                (document_id, source, kind, source_uid, citation, title, body, \
                 valid_from, source_payload_hash, canonical_json) \
             VALUES \
                ('legi:LEGIARTI000006419320@1804-02-21', 'legi', 'article', \
                 'LEGIARTI000006419320', 'Code civil article 1240', \
                 'Article 1240', 'Texte pour la projection dense.', \
                 '1804-02-21', 'sha256:article-1240', '{\"chunks\":[]}'); \
             INSERT INTO chunks \
                (chunk_id, document_id, chunk_index, body, contextualized_body, source_payload_hash, \
                 chunk_builder_version, embedding_fingerprint) \
             VALUES \
                ('chunk:pool:0', 'legi:LEGIARTI000006419320@1804-02-21', 0, \
                 'alpha', 'alpha', 'sha256:article-1240', 'chunker:v0', NULL), \
                ('chunk:pool:1', 'legi:LEGIARTI000006419320@1804-02-21', 1, \
                 'beta', 'beta', 'sha256:article-1240', 'chunker:v0', NULL);",
        )?;
    }

    let local_endpoint = spawn_server(1, |request| {
        assert!(request.starts_with("POST /v1/embeddings "));
        assert!(request.contains(r#""model":"bge-m3""#));
        assert!(request.contains(r#""input":["alph"]"#));
        assert!(!request.contains(r#""alpha""#));
        assert!(!request.to_ascii_lowercase().contains("authorization:"));
        thread::sleep(Duration::from_millis(150));
        ok_json(&embedding_response_json(0))
    });
    let mut openrouter_attempt = 0usize;
    let openrouter_endpoint = spawn_server(2, move |request| {
        openrouter_attempt += 1;
        assert!(request.starts_with("POST /api/v1/embeddings "));
        assert!(request.contains(r#""model":"baai/bge-m3""#));
        assert!(request.contains(r#""input":["beta"]"#));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer openrouter-secret-token")
        );
        if openrouter_attempt == 1 {
            ok_json(r#"{"error":{"message":"transient provider error","code":529}}"#)
        } else {
            ok_json(&embedding_response_json(1))
        }
    });
    let pool =
        format!("{local_endpoint}/v1;{openrouter_endpoint}/api/v1|baai/bge-m3|OPENROUTER_API_KEY");
    let primary_base_url = "http://127.0.0.1:1/v1";

    let output = jurisearch_command_without_embedding_env()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .env("JURISEARCH_CONFIG", "none")
        .env("JURISEARCH_EMBED_BASE_URL", primary_base_url)
        .env("JURISEARCH_EMBED_POOL", pool)
        .env("JURISEARCH_EMBED_MAX_INPUT_CHARS", "4")
        .env("OPENROUTER_API_KEY", "openrouter-secret-token")
        .args([
            "ingest",
            "embed-chunks",
            "--batch-size",
            "1",
            "--pool-concurrency",
            "2",
            "--index-lists",
            "1",
        ])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["command"], "ingest embed-chunks");
    assert_eq!(json["chunks_considered"], 2);
    assert_eq!(json["embeddings_inserted"], 2);
    assert_eq!(json["embedding_inputs_truncated"], 1);
    assert_eq!(
        json["endpoint_pool"]["strategy"],
        "least_outstanding_requests"
    );
    assert_eq!(json["endpoint_pool"]["batch_size"], 1);
    assert_eq!(json["endpoint_pool"]["pool_concurrency"], 2);
    let endpoints = json["endpoint_pool"]["endpoints"].as_array().unwrap();
    assert_eq!(endpoints.len(), 2);
    assert!(
        endpoints
            .iter()
            .all(|endpoint| endpoint["base_url"].as_str().unwrap() != primary_base_url)
    );
    assert!(endpoints.iter().any(|endpoint| {
        endpoint["base_url"].as_str().unwrap().ends_with("/v1")
            && endpoint["request_model"].is_null()
    }));
    assert!(endpoints.iter().any(|endpoint| {
        endpoint["base_url"].as_str().unwrap().ends_with("/api/v1")
            && endpoint["request_model"] == "baai/bge-m3"
    }));
    assert!(endpoints.iter().all(|endpoint| {
        endpoint["requests"].as_u64().unwrap() == 1 && endpoint["chunks"].as_u64().unwrap() == 1
    }));
    assert_eq!(
        endpoints
            .iter()
            .map(|endpoint| endpoint["truncated_inputs"].as_u64().unwrap())
            .sum::<u64>(),
        1
    );
    assert_eq!(
        endpoints
            .iter()
            .map(|endpoint| endpoint["chunks"].as_u64().unwrap())
            .sum::<u64>(),
        2
    );
    assert_eq!(json["dense_rebuild"]["chunks"], 2);
    assert_eq!(json["dense_rebuild"]["embeddings"], 2);
    assert_eq!(json["replay_snapshot_cache"]["source"], "refreshed");
    assert_eq!(json["replay_snapshot_cache"]["status"], "available");
    assert_eq!(json["replay_snapshot_cache"]["chunks"], 2);
    assert_eq!(json["replay_snapshot_cache"]["embeddings"], 2);
    assert_eq!(
        json["replay_snapshot_cache"]["signature"]
            .as_str()
            .unwrap()
            .len(),
        32
    );

    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
    assert_eq!(
        postgres.execute_sql("SELECT count(*)::text FROM chunk_embeddings;")?,
        "2"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT count(*)::text \
             FROM chunks \
             WHERE embedding_fingerprint = 'bge-m3:1024:normalize:true';",
        )?,
        "2"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT count(*)::text \
             FROM chunk_embeddings \
             WHERE model = 'bge-m3' AND embedding_fingerprint = 'bge-m3:1024:normalize:true';",
        )?,
        "2"
    );

    Ok(())
}

#[test]
#[ignore = "requires a running OpenAI-compatible bge-m3 embeddings endpoint"]
fn ingest_embed_chunks_uses_live_endpoint_and_finalizes_dense_index()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(pg_config) = discover_pg_config("CLI live embed chunks")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-cli-embed-chunks.")
        .tempdir()?;
    let embedding_base_url = std::env::var("JURISEARCH_EMBED_BASE_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8097/v1".to_owned());

    {
        let postgres = jurisearch_storage::runtime::ManagedPostgres::start_durable(
            pg_config.clone(),
            root.path(),
        )?;
        postgres.execute_sql(
            "INSERT INTO documents \
                (document_id, source, kind, source_uid, citation, title, body, \
                 valid_from, source_payload_hash, canonical_json) \
             VALUES \
                ('legi:LEGIARTI000006419320@1804-02-21', 'legi', 'article', \
                 'LEGIARTI000006419320', 'Code civil article 1240', \
                 'Article 1240', 'Tout fait quelconque de l''homme oblige a reparer le dommage.', \
                 '1804-02-21', 'sha256:article-1240', \
                 '{\"chunks\":[{\"contextualized_body\":\"Code civil > Article 1240\\nresponsabilite civile faute reparation dommage\"}]}'); \
             INSERT INTO chunks \
                (chunk_id, document_id, chunk_index, body, contextualized_body, source_payload_hash, \
                 chunk_builder_version, embedding_fingerprint) \
             VALUES \
                ('chunk:1240:0', 'legi:LEGIARTI000006419320@1804-02-21', 0, \
                 'plain fallback chunk text', \
                 'Code civil > Article 1240\nresponsabilite civile faute reparation dommage', \
                 'sha256:article-1240', 'chunker:v0', NULL);",
        )?;
    }

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .env("JURISEARCH_EMBED_BASE_URL", &embedding_base_url)
        .args(["ingest", "embed-chunks", "--index-lists", "1"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["command"], "ingest embed-chunks");
    assert_eq!(json["chunks_considered"], 1);
    assert_eq!(json["embeddings_inserted"], 1);
    assert_eq!(
        json["embedding"]["fingerprint"],
        "bge-m3:1024:normalize:true"
    );
    assert_eq!(json["dense_rebuild"]["chunks"], 1);
    assert_eq!(json["dense_rebuild"]["embeddings"], 1);
    assert_eq!(json["dense_rebuild"]["index_lists"], 1);

    let postgres =
        jurisearch_storage::runtime::ManagedPostgres::start_durable(pg_config, root.path())?;
    let stored = postgres.execute_sql(
        "SELECT concat(embedding_fingerprint, '|', model, '|', dimension::text) \
         FROM chunk_embeddings \
         WHERE chunk_id = 'chunk:1240:0';",
    )?;
    assert_eq!(stored, "bge-m3:1024:normalize:true|bge-m3|1024");
    let manifest = postgres.execute_sql(
        "SELECT value->>'embedding_fingerprint' \
         FROM index_manifest \
         WHERE key = 'embedding';",
    )?;
    assert_eq!(manifest, "bge-m3:1024:normalize:true");

    Ok(())
}

#[test]
#[ignore = "requires a running OpenAI-compatible bge-m3 embeddings endpoint"]
fn search_returns_results_from_existing_index_with_live_embeddings()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(pg_config) = discover_pg_config("CLI live search existing index")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-cli-search.")
        .tempdir()?;
    let query = "responsabilite civile faute dommage article 1240";
    let document_id = "legi:LEGIARTI000006419320@1804-02-21";
    let embedding_base_url = std::env::var("JURISEARCH_EMBED_BASE_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8097/v1".to_owned());
    let embedding_config = EmbeddingConfig::phase0_bge_m3(
        &embedding_base_url,
        std::env::var("JURISEARCH_EMBED_API_KEY").ok(),
    );
    let expected = embedding_config.fingerprint();
    let storage_fingerprint = expected.storage_embedding_fingerprint();
    let client = OpenAiCompatibleClient::new(embedding_config)?;
    let embedding = client.embed_query(query, &expected)?;
    let embedding = pgvector_literal(&embedding.values);

    {
        let postgres =
            jurisearch_storage::runtime::ManagedPostgres::start_durable(pg_config, root.path())?;
        postgres.execute_sql(&format!(
            "INSERT INTO documents \
                (document_id, source, kind, source_uid, citation, title, body, \
                 valid_from, source_payload_hash, canonical_json) \
             VALUES \
                ('{document_id}', 'legi', 'article', \
                 'LEGIARTI000006419320', 'Code civil article 1240', \
                 'Article 1240', 'Tout fait quelconque de l''homme oblige a reparer le dommage.', \
                 '1804-02-21', 'sha256:article-1240', '{{\"official\":true}}'); \
             INSERT INTO chunks \
                (chunk_id, document_id, chunk_index, body, contextualized_body, source_payload_hash, \
                 chunk_builder_version, embedding_fingerprint) \
             VALUES \
                ('chunk:1240:0', '{document_id}', 0, \
                 'responsabilite civile faute reparation dommage article 1240', \
                 'Code civil > Article 1240\nresponsabilite civile faute reparation dommage article 1240', \
                 'sha256:article-1240', 'chunker:v0', '{storage_fingerprint}'); \
             INSERT INTO chunk_embeddings \
                (chunk_id, embedding_fingerprint, embedding, model, dimension) \
             VALUES \
                ('chunk:1240:0', '{storage_fingerprint}', '{}', 'bge-m3', 1024);",
            embedding
        ))?;
    }

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .env("JURISEARCH_EMBED_BASE_URL", &embedding_base_url)
        .args(["search", query, "--as-of", "2024-01-01", "--top-k", "3"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["retrieval_mode"], "hybrid");
    assert_eq!(json["candidates"][0]["document_id"], document_id);
    assert_eq!(json["candidates"][0]["scores"]["dense_rank"], 1);

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .env("JURISEARCH_EMBED_BASE_URL", embedding_base_url)
        .args([
            "search",
            query,
            "--as-of",
            "2024-01-01",
            "--top-k",
            "3",
            "--mode",
            "dense",
        ])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["retrieval_mode"], "dense");
    assert_eq!(json["candidates"][0]["document_id"], document_id);
    assert!(json["candidates"][0]["scores"]["lexical_rank"].is_null());
    assert_eq!(json["candidates"][0]["scores"]["dense_rank"], 1);
    Ok(())
}

#[test]
fn session_jsonl_preserves_order_handles_bad_json_and_exit() {
    let input = concat!(
        "{\"id\":\"one\",\"command\":\"status\"}\n",
        "not-json\n",
        "{\"id\":\"two\",\"command\":\"help schema\"}\n",
        "{\"id\":\"three\",\"command\":\"exit\"}\n",
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

    assert_eq!(values.len(), 4);
    assert_eq!(values[0]["id"], "one");
    assert_eq!(values[0]["ok"], true);
    assert_eq!(values[1]["ok"], false);
    assert_eq!(values[1]["error"]["code"], "bad_input");
    assert_eq!(values[2]["id"], "two");
    assert_eq!(values[2]["ok"], true);
    assert_eq!(values[3]["id"], "three");
    assert_eq!(values[3]["result"]["bye"], true);
}

#[test]
fn batch_jsonl_is_finite_ordered_and_honors_fatal_malformed_input() {
    let input = concat!(
        "{\"id\":\"one\",\"command\":\"expand\",\"args\":{\"query\":\"faute dommage\"}}\n",
        "not-json\n",
        "{\"id\":\"two\",\"command\":\"help schema\"}\n",
    );

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .args(["batch", "--jsonl"])
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

    assert_eq!(values.len(), 3);
    assert_eq!(values[0]["id"], "one");
    assert_eq!(values[0]["ok"], true);
    assert_eq!(values[1]["ok"], false);
    assert_eq!(values[1]["error"]["code"], "bad_input");
    assert_eq!(values[2]["id"], "two");
    assert_eq!(values[2]["ok"], true);
    assert_eq!(values[2]["result"]["schema_version"], "1");

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .args(["batch", "--jsonl", "--fatal"])
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
    assert_eq!(values[0]["id"], "one");
    assert_eq!(values[0]["ok"], true);
    assert_eq!(values[1]["ok"], false);
    assert_eq!(values[1]["error"]["code"], "bad_input");

    let input = concat!(
        "{\"id\":\"one\",\"command\":\"expand\",\"args\":{\"query\":\"faute dommage\"}}\n",
        "{\"id\":\"bad-command\",\"command\":\"unknown\"}\n",
        "{\"id\":\"two\",\"command\":\"help schema\"}\n",
    );
    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .args(["batch", "--jsonl", "--fatal"])
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

    assert_eq!(values.len(), 3);
    assert_eq!(values[0]["ok"], true);
    assert_eq!(values[1]["id"], "bad-command");
    assert_eq!(values[1]["ok"], false);
    assert_eq!(values[1]["error"]["code"], "bad_input");
    assert_eq!(values[2]["id"], "two");
    assert_eq!(values[2]["ok"], true);

    for command in ["batch", "session"] {
        let output = Command::cargo_bin("jurisearch")
            .unwrap()
            .args([command])
            .write_stdin(input)
            .assert()
            .code(2)
            .stderr(predicate::str::is_empty())
            .get_output()
            .stdout
            .clone();
        assert_json_error_contains(&output, "bad_input", "explicit `--jsonl` flag");
    }
}

#[test]
fn session_jsonl_expand_returns_curated_terms() {
    let input = concat!(
        "{\"id\":\"expand-one\",\"command\":\"expand\",\"args\":{\"query\":\"prescription action\"}}\n",
        "{\"id\":\"done\",\"command\":\"exit\"}\n",
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
    assert_eq!(values[0]["id"], "expand-one");
    assert_eq!(values[0]["ok"], true);
    assert!(
        values[0]["result"]["expanded_terms"]
            .as_array()
            .unwrap()
            .iter()
            .any(|term| term["term"] == "article 2224"
                && term["source_seed_id"] == "civil-prescription")
    );
    assert_eq!(values[1]["result"]["bye"], true);
}

fn discover_pg_config(test_name: &str) -> Result<Option<PgConfig>, StorageError> {
    let pg_config = match PgConfig::discover() {
        Ok(pg_config) => pg_config,
        Err(error @ StorageError::MissingPgConfig { .. }) => {
            if std::env::var_os("JURISEARCH_REQUIRE_PG_EXTENSIONS").is_some() {
                return Err(error);
            }
            eprintln!("skipping {test_name}: {error}");
            return Ok(None);
        }
        Err(error) => return Err(error),
    };

    for extension in ["pg_search", "vector"] {
        if let Err(error) = pg_config.require_extension_assets(extension) {
            if std::env::var_os("JURISEARCH_REQUIRE_PG_EXTENSIONS").is_some() {
                return Err(error);
            }
            eprintln!("skipping {test_name}: {error}");
            return Ok(None);
        }
    }

    Ok(Some(pg_config))
}

fn assert_json_error_contains(output: &[u8], code: &str, message: &str) {
    let json: Value = serde_json::from_slice(output).unwrap();
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["code"], code);
    assert!(json["error"]["message"].as_str().unwrap().contains(message));
}

fn write_tar_gz(path: &Path, members: &[(&str, &[u8])]) -> Result<(), Box<dyn std::error::Error>> {
    let file = File::create(path)?;
    let encoder = GzEncoder::new(file, Compression::default());
    let mut builder = Builder::new(encoder);
    for (member_path, bytes) in members {
        let mut header = Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, member_path, Cursor::new(bytes))?;
    }
    builder.finish()?;
    Ok(())
}

fn article_fixture() -> String {
    r#"
<ARTICLE>
  <META>
    <META_COMMUN>
      <ID>LEGIARTI000006419320</ID>
      <URL>/codes/article_lc/LEGIARTI000006419320</URL>
      <NATURE>Article</NATURE>
    </META_COMMUN>
    <META_ARTICLE>
      <NUM>1240</NUM>
      <ETAT>VIGUEUR</ETAT>
      <TYPE>AUTONOME</TYPE>
      <DATE_DEBUT>1804-02-21</DATE_DEBUT>
      <DATE_FIN>2999-01-01</DATE_FIN>
    </META_ARTICLE>
  </META>
  <CONTEXTE>
    <TEXTE>
      <TITRE_TXT>Code civil</TITRE_TXT>
      <TM>
        <TITRE_TM>Livre III : Des differentes manieres dont on acquiert la propriete</TITRE_TM>
        <TM>
          <TITRE_TM>Titre IV : Des engagements qui se forment sans convention</TITRE_TM>
        </TM>
      </TM>
    </TEXTE>
  </CONTEXTE>
  <BLOC_TEXTUEL>
    <CONTENU>
      <p>Tout fait quelconque de l'homme, qui cause a autrui un dommage, oblige celui par la faute duquel il est arrive a le reparer.</p>
    </CONTENU>
  </BLOC_TEXTUEL>
  <LIENS>
    <LIEN cidtexte="JORFTEXT000000696195" id="LEGIARTI000006554637" sens="cible" typelien="MODIFICATION">Decret no 73-138 - art. 11</LIEN>
  </LIENS>
</ARTICLE>
"#
    .to_owned()
}

fn article_fixture_without_body() -> String {
    article_fixture().replace(
        r#"  <BLOC_TEXTUEL>
    <CONTENU>
      <p>Tout fait quelconque de l'homme, qui cause a autrui un dommage, oblige celui par la faute duquel il est arrive a le reparer.</p>
    </CONTENU>
  </BLOC_TEXTUEL>
"#,
        "",
    )
}

fn pgvector_literal(values: &[f32]) -> String {
    let values = values
        .iter()
        .map(|value| format!("{value:.8}"))
        .collect::<Vec<_>>()
        .join(",");
    format!("[{values}]")
}

fn unit_vector_literal(active_index: usize) -> String {
    let values = (0..1024)
        .map(|index| if index == active_index { 1.0 } else { 0.0 })
        .collect::<Vec<_>>();
    pgvector_literal(&values)
}

fn embedding_response_json(active_index: usize) -> String {
    let values = (0..1024)
        .map(|index| {
            if index == active_index {
                "1.0".to_owned()
            } else {
                "0.0".to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(r#"{{"data":[{{"embedding":[{values}]}}]}}"#)
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
