//! eval phase1 contract tests.

mod support;
use support::*;

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
