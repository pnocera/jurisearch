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
            "citation_state": ["exact", "normalized", "ambiguous", "stale_version", "not_found", "source_unavailable"],
            "response_format": ["concise", "detailed"]
        },
        "schemas": {
            "SearchRequest": {
                "required": ["query"],
                "properties": {
                    "query": { "type": "string" },
                    "kind": { "enum": ["code", "decision", "all"], "default": "all" },
                    "top_k": { "type": "integer", "minimum": 1, "default": 10 },
                    "as_of": { "type": "string", "format": "date" }
                }
            },
            "SearchResponse": {
                "properties": {
                    "query": { "type": "string" },
                    "results": { "type": "array" }
                }
            },
            "StatusResponse": {
                "properties": {
                    "schema_version": { "type": "string" },
                    "index": { "type": "object" },
                    "embedding": { "type": "object" },
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
