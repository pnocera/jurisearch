//! Eval/benchmark command schemas (phase1/run/tune/France-LEGI/France-juris).
//!
//! Returns this domain's entries for the flat `#/schemas/*` map assembled by `compiled_schema()`.

use serde_json::{Map, Value, json};

pub(super) fn schemas() -> Map<String, Value> {
    let Value::Object(map) = json!({
        "EvalPhase1Request": {
            "properties": {
                "list": { "type": "boolean", "default": false },
                "include_dev": { "type": "boolean", "default": false },
                "mode": { "enum": ["hybrid", "bm25", "dense"], "default": "hybrid" },
                "top_k": { "type": "integer", "minimum": 1, "default": 10 }
            }
        },
        "EvalPhase1Response": {
            "properties": {
                "schema_version": { "type": "string" },
                "command": { "const": "eval phase1" },
                "action": { "enum": ["list", "run"] },
                "include_dev": { "type": "boolean" },
                "fixture_count": { "type": "integer" },
                "retrieval_mode": { "enum": ["hybrid", "bm25", "dense"] },
                "top_k": { "type": "integer" },
                "eval_fixtures": { "$ref": "#/schemas/EvalFixtureSummary" },
                "summary": {
                    "type": "object",
                    "properties": {
                        "fixture_count": { "type": "integer" },
                        "passed": { "type": "integer" },
                        "failed": { "type": "integer" },
                        "all_passed": { "type": "boolean" }
                    }
                },
                "fixtures": { "type": "array", "items": { "type": "object" } },
                "results": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": { "type": "string" },
                            "tier": { "enum": ["dev", "release_gating"] },
                            "category": { "type": "string" },
                            "query": { "type": "string" },
                            "as_of": { "type": ["string", "null"], "format": "date" },
                            "expected_ids": { "type": "array", "items": { "type": "string" } },
                            "allowed_alternates": { "type": "array", "items": { "type": "string" } },
                            "status": { "enum": ["pass", "pass_allowed_alternate", "fail"] },
                            "passed": { "type": "boolean" },
                            "best_expected_rank": { "type": ["integer", "null"] },
                            "best_allowed_alternate_rank": { "type": ["integer", "null"] },
                            "matched_document_id": { "type": ["string", "null"] },
                            "candidate_count": { "type": "integer" },
                            "top_document_ids": { "type": "array", "items": { "type": "string" } },
                            "search": { "type": "object" },
                            "error": { "$ref": "#/error_object" }
                        }
                    }
                }
            }
        },
        "EvalFixtureSummary": {
            "properties": {
                "total": { "type": "integer" },
                "source_verified": { "type": "integer" },
                "release_candidates": { "type": "integer" },
                "release_gating": { "type": "integer" },
                "hierarchy_sensitive": { "type": "integer" },
                "categories": { "type": "object" }
            }
        },
        "EvalFranceLegiRequest": {
            "properties": {
                "known_item": { "type": "integer", "minimum": 0, "default": 60 },
                "temporal": { "type": "integer", "minimum": 0, "default": 12 },
                "cross_reference": { "type": "integer", "minimum": 0, "default": 120 },
                "source_revision": { "type": ["string", "null"] },
                "out": {
                    "type": ["string", "null"],
                    "description": "Path to write the phase1_france_legi_benchmark artifact (also printed to stdout)."
                }
            }
        },
        "EvalFranceLegiResponse": {
            "description": "phase1_france_legi_benchmark artifact (also written to --out when given).",
            "properties": {
                "schema_version": { "const": 1 },
                "kind": { "const": "phase1_france_legi_benchmark" },
                "state": { "enum": ["passed", "failed"] },
                "jurisdiction": { "type": "string" },
                "claim_scope": { "type": "string" },
                "source": { "type": "string" },
                "retriever": { "type": "string" },
                "embedding": {
                    "type": "object",
                    "properties": {
                        "fingerprint_model": { "type": "string" },
                        "dimension": { "type": "integer" },
                        "normalize": { "type": "boolean" }
                    }
                },
                "thresholds": { "type": "object" },
                "categories": {
                    "type": "object",
                    "description": "Per-category results; each is a FranceLegiCategory.",
                    "properties": {
                        "structured_citation_resolution": { "$ref": "#/schemas/FranceLegiCategory" },
                        "temporal_version_pinning": { "$ref": "#/schemas/FranceLegiCategory" },
                        "semantic_retrieval": { "$ref": "#/schemas/FranceLegiCategory" }
                    }
                },
                "provenance": { "type": "object" },
                "evidence": { "type": "array", "items": { "type": "string" } }
            }
        },
        "EvalRunRequest": {
            "required": ["questions"],
            "properties": {
                "questions": { "type": "string", "description": "Path to a JSON array of {id, query, as_of?}." },
                "qrels": { "type": ["string", "null"], "description": "Path to JSON [{query_id, document_id, label}] (XOR judge_cmd)." },
                "judge_cmd": { "type": ["string", "null"], "description": "External judge: reads a blind JSON task on stdin, writes {question_id:{key:label}} on stdout." },
                "modes": { "type": "string", "default": "bm25,dense,hybrid" },
                "metrics": { "type": "string", "default": "ndcg@10,recall@10,p@10,mrr@10" },
                "top_k": { "type": "integer", "minimum": 1, "default": 10 },
                "rel_min": { "type": "integer", "default": 1 },
                "bootstrap": { "type": "integer", "minimum": 0, "default": 0 },
                "out": { "type": ["string", "null"] }
            }
        },
        "EvalRunResponse": {
            "description": "eval_run artifact (also written to --out when given).",
            "properties": {
                "schema_version": { "type": "string" },
                "kind": { "const": "eval_run_benchmark" },
                "questions": { "type": "integer" },
                "modes": { "type": "array", "items": { "enum": ["bm25", "dense", "hybrid"] } },
                "group_by": { "const": "document" },
                "top_k": { "type": "integer" },
                "rel_min": { "type": "integer" },
                "judge": { "type": "string" },
                "pool": { "type": "object" },
                "metrics": {
                    "type": "object",
                    "description": "Per-mode metric means; recall is null when no question has a relevant pooled doc.",
                    "additionalProperties": { "type": "object" }
                },
                "bootstrap": { "type": ["object", "null"] }
            }
        },
        "EvalTuneRequest": {
            "required": ["questions", "sweep"],
            "properties": {
                "questions": { "type": "string" },
                "qrels": { "type": ["string", "null"] },
                "judge_cmd": { "type": ["string", "null"] },
                "sweep": { "type": "string", "description": "PARAM=start:stop:step; PARAM in {rrf-dense, rrf-lexical, probes}." },
                "metric": { "type": "string", "default": "ndcg@10" },
                "top_k": { "type": "integer", "minimum": 1, "default": 10 },
                "rel_min": { "type": "integer", "default": 1 },
                "out": { "type": ["string", "null"] }
            }
        },
        "EvalTuneResponse": {
            "description": "eval_tune artifact (also written to --out when given).",
            "properties": {
                "schema_version": { "type": "string" },
                "kind": { "const": "eval_tune" },
                "mode": { "const": "hybrid" },
                "sweep": { "type": "object" },
                "metric": { "type": "string" },
                "points": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "value": { "type": "number" },
                            "metric": { "type": ["number", "null"] }
                        }
                    }
                },
                "best": { "type": ["object", "null"] }
            }
        },
        "FranceLegiCategory": {
            "properties": {
                "metric_value": { "type": "number" },
                "queries": { "type": "integer" },
                "gating": { "type": "boolean" },
                "advisory": { "type": "boolean" },
                "routing_backends": { "type": "object" }
            }
        },
    }) else {
        unreachable!()
    };
    map
}
