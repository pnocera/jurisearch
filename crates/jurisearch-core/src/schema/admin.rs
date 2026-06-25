//! Admin/introspection command schemas (status/model/setup/doctor/stats/inspect/versions/diff/sync/help/serve/ingest/session).
//!
//! Returns this domain's entries for the flat `#/schemas/*` map assembled by `compiled_schema()`.

use serde_json::{Map, Value, json};

pub(super) fn schemas() -> Map<String, Value> {
    let Value::Object(map) = json!({
        "StatusRequest": {
            "properties": {
                "index_dir": { "type": "string" },
                "deep": {
                    "type": "boolean",
                    "default": false,
                    "description": "When true, recompute and cache full replay snapshot signatures; default status reads cached signatures only."
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
                "corpus_sources": { "type": ["object", "null"] },
                "phase1_gate": { "$ref": "#/schemas/Phase1GateResponse" },
                "phase2_gate": { "$ref": "#/schemas/Phase2GateResponse" }
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
        "DoctorRequest": {
            "properties": {
                "index_dir": { "type": "string" }
            }
        },
        "StatsRequest": {
            "properties": {
                "index_dir": { "type": "string" }
            }
        },
        "StatsResponse": {
            "properties": {
                "schema_version": { "type": "string" },
                "stats": {
                    "type": "object",
                    "properties": {
                        "documents": { "type": "integer" },
                        "documents_by_kind": { "type": "object" },
                        "documents_by_source": { "type": "object" },
                        "chunks": { "type": "integer" },
                        "chunk_embeddings": { "type": "integer" },
                        "graph_edges": { "type": "integer" },
                        "graph_edges_by_kind": { "type": "object" },
                        "graph_edges_by_source": { "type": "object" }
                    }
                }
            }
        },
        "InspectRequest": {
            "required": ["id"],
            "properties": {
                "id": { "type": "string" }
            }
        },
        "InspectResponse": {
            "properties": {
                "document": { "type": ["object", "null"], "description": "Full documents row incl. canonical_json." },
                "chunk_count": { "type": "integer" },
                "outgoing_edges": { "type": "integer" }
            }
        },
        "VersionsRequest": {
            "required": ["id"],
            "properties": { "id": { "type": "string" } }
        },
        "VersionsResponse": {
            "properties": {
                "id": { "type": "string" },
                "count": { "type": "integer" },
                "versions": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "document_id": { "type": "string" },
                            "source_uid": { "type": "string" },
                            "citation": { "type": ["string", "null"] },
                            "title": { "type": ["string", "null"] },
                            "validity": { "type": "object" },
                            "is_target": { "type": "boolean" }
                        }
                    }
                }
            }
        },
        "DiffRequest": {
            "required": ["id", "from", "to"],
            "properties": {
                "id": { "type": "string" },
                "from": { "type": "string", "format": "date" },
                "to": { "type": "string", "format": "date" }
            }
        },
        "DiffResponse": {
            "properties": {
                "id": { "type": "string" },
                "from": { "type": "string", "format": "date" },
                "to": { "type": "string", "format": "date" },
                "family_count": { "type": "integer" },
                "from_version": { "type": ["object", "null"] },
                "to_version": { "type": ["object", "null"] },
                "missing_from": { "type": "boolean", "description": "No version in force on `from`." },
                "missing_to": { "type": "boolean", "description": "No version in force on `to`." },
                "changed": { "type": ["boolean", "null"] }
            }
        },
        "DoctorResponse": {
            "properties": {
                "schema_version": { "type": "string" },
                "ready": { "type": "boolean" },
                "note": { "type": "string" },
                "checks": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "name": { "type": "string" },
                            "status": { "enum": ["pass", "warn", "fail", "not_required"] },
                            "detail": {}
                        }
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
        },
        "IngestRequest": {
            "description": "Ingestion is one-shot CLI only (not available over the session protocol).",
            "properties": {
                "subcommand": {
                    "enum": ["plan-archives", "legi-archives", "embed-chunks", "backfill-legi-hierarchy"]
                },
                "args": { "type": "object" }
            }
        },
        "SyncRequest": {
            "properties": {
                "source": { "type": ["string", "null"] },
                "since": { "type": ["string", "null"] }
            }
        },
        "SyncResponse": {
            "description": "STUB — not yet implemented.",
            "properties": {
                "source": { "type": ["string", "null"] },
                "since": { "type": ["string", "null"] }
            }
        },
        "ServeRequest": {
            "description": "Bind one transport, then speak the session_envelope protocol over the socket.",
            "properties": {
                "tcp": { "type": ["string", "null"], "description": "host:port (XOR socket)." },
                "socket": { "type": ["string", "null"], "description": "Unix socket path (XOR tcp)." }
            }
        },
        "ServeResponse": {
            "description": "serve does not return a JSON document; each socket connection speaks the JSONL session protocol (request = session_envelope.request, response = success/error). Capability discovery via {\"command\":\"help schema\"}."
        },
        "SessionRequest": { "$ref": "#/session_envelope/request" },
        "SessionResponse": {
            "oneOf": [
                { "$ref": "#/session_envelope/success_response" },
                { "$ref": "#/session_envelope/error_response" }
            ]
        },
        "HelpAgentRequest": { "properties": {} },
        "HelpAgentResponse": {
            "properties": {
                "text": { "type": "string", "description": "The compiled agent contract as Markdown." }
            }
        },
        "HelpSchemaRequest": {
            "properties": { "json": { "type": "boolean", "default": false } }
        },
        "HelpSchemaResponse": {
            "description": "This document — the compiled schema (commands, exit_codes, error_object, session_envelope, common_enums, schemas)."
        },
    }) else {
        unreachable!()
    };
    map
}
