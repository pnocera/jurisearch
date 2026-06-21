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
                    "format": { "enum": ["concise", "detailed"], "default": "concise" },
                    "top_k": { "type": "integer", "minimum": 1, "default": 10 },
                    "cursor": { "type": "string" },
                    "as_of": { "type": "string", "format": "date" }
                }
            },
            "SearchResponse": {
                "properties": {
                    "query": { "type": "string" },
                    "retrieval_mode": { "enum": ["hybrid", "bm25", "dense"] },
                    "format": { "enum": ["concise", "detailed"] },
                    "expansion_seed_version": { "type": "string" },
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
                    },
                    "as_of": { "type": "string", "format": "date" },
                    "limit": { "type": "integer" },
                    "pagination": {
                        "type": "object",
                        "properties": {
                            "requested_top_k": { "type": "integer" },
                            "after_cursor": { "type": ["string", "null"] },
                            "returned": { "type": "integer" },
                            "possibly_truncated": { "type": "boolean" },
                            "cursor_supported": { "type": "boolean" },
                            "next_cursor": { "type": ["string", "null"] },
                            "cursor_note": { "type": "string" },
                            "guidance": { "type": ["string", "null"] }
                        }
                    },
                    "diagnostics": {
                        "type": "object",
                        "properties": {
                            "query_input": { "type": "string" },
                            "lexical_query_text": { "type": ["string", "null"] },
                            "retrieval": {
                                "type": "object",
                                "properties": {
                                    "mode": { "enum": ["hybrid", "bm25", "dense"] },
                                    "uses_lexical": { "type": "boolean" },
                                    "uses_dense": { "type": "boolean" },
                                    "lexical_limit": { "type": "integer" },
                                    "dense_limit": { "type": "integer" },
                                    "query_limit": { "type": "integer" },
                                    "embedding_fingerprint": { "type": ["string", "null"] },
                                    "kind_filter": { "type": ["string", "null"] },
                                    "after_cursor": { "type": ["string", "null"] }
                                }
                            }
                        }
                    },
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
            "CiteRequest": {
                "required": ["cite"],
                "properties": {
                    "cite": { "type": "string" },
                    "strict": { "type": "boolean", "default": false },
                    "online": { "type": "boolean", "default": false },
                    "as_of": { "type": "string", "format": "date" }
                }
            },
            "CiteResponse": {
                "properties": {
                    "query": { "type": "string" },
                    "input_class": {
                        "enum": ["document_id", "legiarti", "legitext", "legiscta", "nor", "free_text_article", "malformed"]
                    },
                    "normalized": { "type": ["string", "null"] },
                    "as_of": { "type": "string", "format": "date" },
                    "requested_as_of": { "type": ["string", "null"], "format": "date" },
                    "state": {
                        "enum": ["exact", "normalized", "ambiguous", "stale_version", "not_found", "source_unavailable"]
                    },
                    "local_state": {
                        "enum": ["exact", "normalized", "ambiguous", "stale_version", "not_found", "source_unavailable"]
                    },
                    "strict": { "type": "boolean" },
                    "online": { "type": "object" },
                    "match_count": { "type": "integer" },
                    "valid_match_count": { "type": "integer" },
                    "matches": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "target_type": { "enum": ["document", "metadata_root"] },
                                "document_id": { "type": ["string", "null"] },
                                "metadata_key": { "type": ["string", "null"] },
                                "source": { "type": "string" },
                                "kind": { "type": "string" },
                                "source_uid": { "type": ["string", "null"] },
                                "version_group": { "type": ["string", "null"] },
                                "root_kind": { "type": ["string", "null"] },
                                "parent_source_uid": { "type": ["string", "null"] },
                                "citation": { "type": ["string", "null"] },
                                "title": { "type": ["string", "null"] },
                                "nor": { "type": ["string", "null"] },
                                "validity": { "type": "object" },
                                "valid_on_as_of": { "type": "boolean" },
                                "source_url": { "type": ["string", "null"] },
                                "source_payload_hash": { "type": "string" },
                                "exact_identifier_match": { "type": "boolean" }
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
                            "reembeddable": { "type": "boolean" },
                            "config_path": { "type": ["string", "null"] },
                            "config_loaded": { "type": "boolean" },
                            "config_error": { "type": ["string", "null"] },
                            "model_cache": { "$ref": "#/schemas/ModelCacheStatus" },
                            "endpoint": { "$ref": "#/schemas/EmbeddingEndpointStatus" }
                        }
                    },
                    "ingest_health": { "type": "object" },
                    "phase1_gate": { "$ref": "#/schemas/Phase1GateResponse" }
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
                    "eval_fixtures": { "$ref": "#/schemas/EvalFixtureSummary" }
                }
            },
            "Phase1GateCheck": {
                "properties": {
                    "name": { "type": "string" },
                    "status": { "enum": ["pass", "pending", "fail"] },
                    "message": { "type": "string" }
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
            "ModelFetchRequest": {
                "properties": {
                    "model": { "type": "string" },
                    "allow_download": { "type": "boolean", "default": false }
                }
            },
            "ModelFetchResponse": {
                "properties": {
                    "schema_version": { "type": "string" },
                    "provider": { "enum": ["openai_compatible", "in_process"] },
                    "model": { "type": "string" },
                    "action": { "enum": ["not_required", "already_cached"] },
                    "allow_download": { "type": "boolean" },
                    "model_cache": { "$ref": "#/schemas/ModelCacheStatus" },
                    "message": { "type": "string" }
                }
            },
            "SetupRequest": {
                "properties": {}
            },
            "SetupResponse": {
                "properties": {
                    "schema_version": { "type": "string" },
                    "ready": { "type": "boolean" },
                    "embedding": {
                        "type": "object",
                        "properties": {
                            "provider": { "enum": ["openai_compatible", "in_process"] },
                            "model": { "type": "string" },
                            "dimension": { "type": "integer" },
                            "config_path": { "type": ["string", "null"] },
                            "config_loaded": { "type": "boolean" },
                            "config_error": { "type": ["string", "null"] },
                            "model_cache": { "$ref": "#/schemas/ModelCacheStatus" },
                            "endpoint": { "$ref": "#/schemas/EmbeddingEndpointStatus" }
                        }
                    }
                }
            },
            "ModelCacheStatus": {
                "properties": {
                    "required": { "type": "boolean" },
                    "state": { "enum": ["not_required", "ready", "missing"] },
                    "model_dir": { "type": "string" },
                    "model_cache_key": { "type": "string" },
                    "model_path": { "type": ["string", "null"] },
                    "model_present": { "type": ["boolean", "null"] },
                    "required_files": {
                        "type": "array",
                        "items": { "type": "string" }
                    },
                    "missing_files": {
                        "type": "array",
                        "items": { "type": "string" }
                    }
                }
            },
            "EmbeddingEndpointStatus": {
                "properties": {
                    "checked": { "type": "boolean" },
                    "state": { "enum": ["not_applicable", "not_checked", "reachable", "unreachable", "invalid"] },
                    "reachable": { "type": ["boolean", "null"] },
                    "message": { "type": "string" }
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
