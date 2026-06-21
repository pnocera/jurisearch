use serde_json::{Value, json};

use crate::{SCHEMA_VERSION, contract::COMMANDS};

pub fn compiled_schema() -> Value {
    json!({
        "schema_version": SCHEMA_VERSION,
        "commands": COMMANDS,
        "exit_codes": {
            "0": "success",
            "2": "user input, no-results, strict citation, or validation failure",
            "3": "local index/configuration unavailable",
            "4": "local dependency or implementation unavailable",
            "5": "upstream official API or provider failure"
        },
        "error_object": {
            "type": "object",
            "required": ["code", "message"],
            "properties": {
                "code": {
                    "type": "string",
                    "enum": [
                        "bad_input",
                        "no_results",
                        "index_unavailable",
                        "dependency_unavailable",
                        "upstream",
                        "not_implemented",
                        "internal"
                    ]
                },
                "message": { "type": "string" },
                "suggestions": { "type": "array", "items": { "type": "string" } }
            }
        },
        "session_envelope": {
            "request": {
                "type": "object",
                "required": ["command"],
                "properties": {
                    "id": { "type": ["string", "number", "null"] },
                    "command": { "type": "string" },
                    "args": { "type": "object" }
                }
            },
            "success_response": {
                "type": "object",
                "required": ["ok", "result"],
                "properties": {
                    "id": { "type": ["string", "number", "null"] },
                    "ok": { "const": true },
                    "result": { "type": "object" }
                }
            },
            "error_response": {
                "type": "object",
                "required": ["ok", "error"],
                "properties": {
                    "id": { "type": ["string", "number", "null"] },
                    "ok": { "const": false },
                    "error": { "$ref": "#/error_object" }
                }
            }
        },
        "common_enums": {
            "kind": {
                "description": "CLI kind `code` maps to result kind `article`.",
                "values": ["code", "decision", "all"]
            },
            "search_mode": {
                "description": "Retrieval ablation mode. `hybrid` fuses BM25 and dense candidates.",
                "values": ["hybrid", "bm25", "dense"]
            },
            "citation_state": ["exact", "normalized", "ambiguous", "stale_version", "not_found", "source_unavailable"],
            "response_format": ["concise", "detailed"]
        },
        "schemas": {
            "SearchRequest": {
                "required": ["query"],
                "properties": {
                    "query": { "type": "string" },
                    "kind": { "enum": ["code", "decision", "all"], "default": "all" },
                    "mode": { "enum": ["hybrid", "bm25", "dense"], "default": "hybrid" },
                    "top_k": { "type": "integer", "minimum": 1, "default": 10 },
                    "as_of": { "type": "string", "format": "date" }
                }
            },
            "SearchResponse": {
                "properties": {
                    "query": { "type": "string" },
                    "retrieval_mode": { "enum": ["hybrid", "bm25", "dense"] },
                    "as_of": { "type": "string", "format": "date" },
                    "limit": { "type": "integer" },
                    "candidates": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "chunk_id": { "type": "string" },
                                "document_id": { "type": "string" },
                                "source": { "type": "string" },
                                "kind": { "type": "string" },
                                "citation": { "type": ["string", "null"] },
                                "title": { "type": ["string", "null"] },
                                "source_url": { "type": ["string", "null"] },
                                "snippet": { "type": "string" },
                                "validity": { "type": "object" },
                                "scores": { "type": "object" },
                                "cursor": { "type": "string" }
                            }
                        }
                    }
                }
            },
            "FetchResponse": {
                "properties": {
                    "documents": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "document_id": { "type": "string" },
                                "source": { "type": "string" },
                                "kind": { "type": "string" },
                                "source_uid": { "type": "string" },
                                "version_group": { "type": ["string", "null"] },
                                "citation": { "type": ["string", "null"] },
                                "title": { "type": ["string", "null"] },
                                "body": { "type": "string" },
                                "validity": { "type": "object" },
                                "source_url": { "type": ["string", "null"] },
                                "source_payload_hash": { "type": "string" },
                                "chunks": { "type": "array" }
                            }
                        }
                    }
                }
            },
            "ContextRequest": {
                "required": ["id"],
                "properties": {
                    "id": { "type": "string" },
                    "siblings": { "type": "boolean", "default": false },
                    "as_of": { "type": "string", "format": "date" }
                }
            },
            "ContextResponse": {
                "properties": {
                    "id": { "type": "string" },
                    "as_of": { "type": ["string", "null"], "format": "date" },
                    "requested_as_of": { "type": ["string", "null"], "format": "date" },
                    "target": { "type": ["object", "null"] },
                    "ancestry": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "depth": { "type": "integer" },
                                "title": { "type": "string" }
                            }
                        }
                    },
                    "siblings": { "type": "array" },
                    "sibling_count": { "type": "integer" },
                    "sibling_limit": { "type": "integer" },
                    "sibling_truncated": { "type": "boolean" }
                }
            },
            "ExpandRequest": {
                "required": ["query"],
                "properties": {
                    "query": { "type": "string" }
                }
            },
            "ExpandResponse": {
                "properties": {
                    "query": { "type": "string" },
                    "seed_version": { "type": "string" },
                    "expanded_terms": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "term": { "type": "string" },
                                "matched_terms": {
                                    "type": "array",
                                    "items": { "type": "string" }
                                },
                                "source_seed_id": { "type": "string" },
                                "source_label": { "type": "string" },
                                "source_citation": { "type": "string" },
                                "review_status": { "type": "string" },
                                "reviewer": { "type": "string" },
                                "rationale": { "type": "string" }
                            }
                        }
                    }
                }
            },
            "StatusResponse": {
                "properties": {
                    "schema_version": { "type": "string" },
                    "index": { "type": "object" },
                    "embedding": {
                        "type": "object",
                        "properties": {
                            "provider": { "enum": ["openai_compatible", "in_process"] },
                            "base_url": { "type": "string" },
                            "base_url_class": { "enum": ["local_loopback", "hosted", "in_process"] },
                            "model": { "type": "string" },
                            "dimension": { "type": "integer" },
                            "normalize": { "type": "boolean" },
                            "pooling": { "type": "string" },
                            "provisional": { "type": "boolean" },
                            "reembeddable": { "type": "boolean" }
                        }
                    },
                    "ingest_health": { "type": "object" }
                }
            },
            "IngestResponse": {
                "properties": {
                    "plan": { "type": "object" },
                    "skipped": { "type": "array" }
                }
            }
        }
    })
}
