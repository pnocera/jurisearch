use serde_json::{Map, Value, json};

use crate::{SCHEMA_VERSION, contract::COMMANDS};

mod admin;
mod eval;
mod gates;
mod search;

pub fn compiled_schema() -> Value {
    let mut schemas: Map<String, Value> = Map::new();
    schemas.extend(search::schemas());
    schemas.extend(admin::schemas());
    schemas.extend(eval::schemas());
    schemas.extend(gates::schemas());
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
        "schemas": Value::Object(schemas)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::COMMANDS;

    /// Invariant: every request/response schema name advertised in the command contract resolves to
    /// a schema body in `compiled_schema()`. Guards against an "implemented but unschema'd" command
    /// (how `routing` and `eval france-legi` previously slipped through).
    #[test]
    fn every_command_schema_name_resolves() {
        let schema = compiled_schema();
        let schemas = schema["schemas"]
            .as_object()
            .expect("compiled_schema must have a `schemas` object");
        let mut missing = Vec::new();
        for command in COMMANDS {
            for name in [command.request_schema, command.response_schema] {
                if !schemas.contains_key(name) {
                    missing.push(format!("{} -> {name}", command.name));
                }
            }
        }
        assert!(
            missing.is_empty(),
            "command schema names with no schema body: {missing:?}"
        );
    }

    /// Byte-identical guard for the schema/ split: the assembled `compiled_schema()` must
    /// serialize exactly as before the split. Regenerate the golden after an INTENTIONAL schema
    /// change with `cargo test -p jurisearch-core regenerate_schema_golden -- --ignored`.
    #[test]
    fn compiled_schema_matches_golden() {
        let rendered = serde_json::to_string_pretty(&compiled_schema()).unwrap() + "\n";
        assert_eq!(
            rendered,
            include_str!("../schema_golden.json"),
            "compiled_schema() output drifted from the golden fixture"
        );
    }

    #[test]
    #[ignore = "regenerates the schema golden fixture; run intentionally after a schema change"]
    fn regenerate_schema_golden() {
        let rendered = serde_json::to_string_pretty(&compiled_schema()).unwrap() + "\n";
        std::fs::write(
            concat!(env!("CARGO_MANIFEST_DIR"), "/src/schema_golden.json"),
            rendered,
        )
        .unwrap();
    }

    /// Pin the one-shot-only set (CommandSpec::session_excluded) so it cannot silently drift —
    /// this is the `eval france-legi` gap a hard-coded parallel list previously missed.
    #[test]
    fn session_excluded_set_is_exactly_the_one_shot_only_commands() {
        let excluded: std::collections::BTreeSet<&str> = COMMANDS
            .iter()
            .filter(|c| c.session_excluded)
            .map(|c| c.name)
            .collect();
        let expected: std::collections::BTreeSet<&str> = [
            "eval france-legi",
            "eval run",
            "eval tune",
            "ingest",
            "serve",
            "sync",
        ]
        .into_iter()
        .collect();
        assert_eq!(excluded, expected);
    }
}
