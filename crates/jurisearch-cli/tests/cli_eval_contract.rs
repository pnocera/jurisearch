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

#[test]
fn eval_france_juris_stays_knob_free_and_rejects_authority_weight() {
    // Phase 2 gate invariant: the authority re-rank is a SEPARATE measured-only benchmark (A6) and must
    // never be wired into the gating `eval france-juris` command. clap rejects the unknown flag, so the
    // gate's recall is always measured on the production OFF path.
    Command::cargo_bin("jurisearch")
        .unwrap()
        .args(["eval", "france-juris", "--authority-weight", "0.5"])
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("--authority-weight")
                .or(predicate::str::contains("unexpected argument")),
        );
}

#[test]
fn eval_france_juris_authority_validates_weights_before_index_and_is_measured_only() {
    // Bad sweep weights are rejected as bad_input BEFORE any index access (parse runs first).
    Command::cargo_bin("jurisearch")
        .unwrap()
        .env_remove("JURISEARCH_INDEX_DIR")
        .args([
            "eval",
            "france-juris-authority",
            "--authority-weights",
            "0.1,1.5",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::is_empty())
        .get_output();

    // Valid weights but no index → index_unavailable (the benchmark is a real indexed run).
    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env_remove("JURISEARCH_INDEX_DIR")
        .args([
            "eval",
            "france-juris-authority",
            "--authority-weights",
            "0.0,0.5",
        ])
        .assert()
        .code(3)
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["error"]["code"], "index_unavailable");
}
