//! Release-gate schemas (Phase 1/Phase 2 gate + benchmark-gate support).
//!
//! Returns this domain's entries for the flat `#/schemas/*` map assembled by `compiled_schema()`.

use serde_json::{Map, Value, json};

pub(crate) fn schemas() -> Map<String, Value> {
    let Value::Object(map) = json!({
        "Phase2GateResponse": {
            "properties": {
                "state": { "enum": ["ready", "not_ready"] },
                "claim_allowed": { "type": "boolean" },
                "scope": { "type": "string" },
                "checks": {
                    "type": "array",
                    "items": { "$ref": "#/schemas/Phase1GateCheck" }
                },
                "jurisprudence_corpus_sources": {
                    "type": "array",
                    "items": { "type": "string" }
                },
                "benchmark": { "$ref": "#/schemas/Phase2BenchmarkGate" }
            }
        },
        "Phase2BenchmarkGate": {
            "properties": {
                "state": { "enum": ["pending", "passed", "failed"] },
                "source": { "type": "string" },
                "artifact_path": { "type": ["string", "null"] },
                "artifact_error": { "type": ["string", "null"] },
                "artifact_reported_state": { "type": ["string", "null"] },
                "jurisdiction": { "type": "string" },
                "fingerprint": { "type": "string" },
                "claim_scope": { "type": "string" },
                "required_evidence": { "type": "array", "items": { "type": "string" } },
                "floors": { "type": "object" },
                "categories": { "type": ["object", "null"] },
                "provenance": { "type": ["object", "null"] },
                "evidence": { "type": "array" },
                "reason": { "type": "string" },
                "artifact": { "type": ["object", "null"] }
            }
        },
        "Phase1GateResponse": {
            "properties": {
                "state": { "enum": ["ready", "not_ready"] },
                "claim_allowed": { "type": "boolean" },
                "scope": { "type": "string" },
                "checks": {
                    "type": "array",
                    "items": { "$ref": "#/schemas/Phase1GateCheck" }
                },
                "eval_fixtures": { "$ref": "#/schemas/EvalFixtureSummary" },
                "external_benchmark": { "$ref": "#/schemas/ExternalBenchmarkGate" },
                "france_legi_benchmark": { "$ref": "#/schemas/FranceLegiGate" },
                "reranker_decision": { "$ref": "#/schemas/RerankerDecision" }
            }
        },
        "Phase1GateCheck": {
            "properties": {
                "name": { "type": "string" },
                "status": { "enum": ["pass", "pending", "fail"] },
                "message": { "type": "string" },
                "gating": { "type": "boolean" }
            }
        },
        "ExternalBenchmarkGate": {
            "properties": {
                "state": { "enum": ["pending", "passed", "failed"] },
                "source": { "type": "string" },
                "artifact_path": { "type": ["string", "null"] },
                "artifact_error": { "type": ["string", "null"] },
                "decision_date": { "type": "string", "format": "date" },
                "primary_candidate": { "type": "string" },
                "claim_scope": { "type": "string" },
                "jurisdiction": { "type": "string" },
                "usage_scope": { "type": "string" },
                "dataset": { "type": ["object", "null"] },
                "metrics": { "type": ["object", "null"] },
                "thresholds": { "type": ["object", "null"] },
                "artifact": { "type": ["object", "null"] },
                "required_evidence": { "type": "array", "items": { "type": "string" } },
                "evidence": { "type": "array", "items": { "type": "string" } },
                "candidate_datasets": { "type": "array", "items": { "type": "object" } },
                "non_gating_inputs": { "type": "array", "items": { "type": "object" } },
                "reason": { "type": "string" }
            }
        },
        "FranceLegiGate": {
            "properties": {
                "state": { "enum": ["pending", "passed", "failed"] },
                "source": { "type": "string" },
                "artifact_path": { "type": ["string", "null"] },
                "artifact_error": { "type": ["string", "null"] },
                "decision_date": { "type": "string", "format": "date" },
                "claim_scope": { "type": "string" },
                "jurisdiction": { "type": "string" },
                "retriever": { "type": "string" },
                "categories": { "type": ["object", "null"] },
                "thresholds": { "type": ["object", "null"] },
                "provenance": { "type": ["object", "null"] },
                "artifact": { "type": ["object", "null"] },
                "required_evidence": { "type": "array", "items": { "type": "string" } },
                "evidence": { "type": "array", "items": { "type": "string" } },
                "reason": { "type": "string" }
            }
        },
        "RerankerDecision": {
            "properties": {
                "state": { "enum": ["deferred", "adopted"] },
                "provider": { "enum": ["disabled", "http", "local_onnx"] },
                "adopted": { "type": "boolean" },
                "decision_date": { "type": "string", "format": "date" },
                "model_candidate": { "type": "string" },
                "evidence": { "type": "array", "items": { "type": "string" } },
                "reason": { "type": "string" },
                "future_adoption_gate": { "type": "string" }
            }
        },
    }) else {
        unreachable!()
    };
    map
}
