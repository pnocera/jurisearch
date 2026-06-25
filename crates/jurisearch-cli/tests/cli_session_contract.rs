//! JSONL session/batch protocol contract tests.

mod support;
use support::*;

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
