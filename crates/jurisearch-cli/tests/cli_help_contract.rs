//! CLI help/schema + generic bad-input contract tests.

mod support;
use support::*;

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
    // T2.1 request-scoped tuning must be discoverable through the schema, not just the CLI flags.
    assert_eq!(
        json["schemas"]["SearchRequest"]["properties"]["rrf_dense_weight"]["minimum"],
        0
    );
    assert_eq!(
        json["schemas"]["SearchRequest"]["properties"]["probes"]["maximum"],
        4096
    );
    // A3: the decision-only authority re-rank knob must be discoverable through the schema with its
    // [0.0, 1.0] bounds, so JSONL/session clients can find it (not just the one-shot CLI flag).
    let authority_weight = &json["schemas"]["SearchRequest"]["properties"]["authority_weight"];
    assert_eq!(authority_weight["type"], "number");
    assert_eq!(authority_weight["minimum"], 0);
    assert_eq!(authority_weight["maximum"], 1);
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
    // Z4: the official-zone search surface must be discoverable through the schema — the `zone`
    // request field and the zone routing values (query_type=zone, backend=official_zone_retrieval).
    let zone_request_values =
        json["schemas"]["SearchRequest"]["properties"]["zone"]["enum"].as_array();
    assert!(
        zone_request_values.is_some_and(|values| values.iter().any(|value| value == "motivations")
            && values.iter().any(|value| value == "moyens")
            && values.iter().any(|value| value == "dispositif")),
        "SearchRequest.zone must advertise motivations/moyens/dispositif"
    );
    let routing = &json["schemas"]["SearchResponse"]["properties"]["routing"]["properties"];
    assert!(
        routing["query_type"]["enum"]
            .as_array()
            .is_some_and(|values| values.iter().any(|value| value == "zone")),
        "routing.query_type must include zone"
    );
    assert!(
        routing["chosen_backend"]["enum"]
            .as_array()
            .is_some_and(|values| values
                .iter()
                .any(|value| value == "official_zone_retrieval")),
        "routing.chosen_backend must include official_zone_retrieval"
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
