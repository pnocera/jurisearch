# Code Review: refactoring-plan resync r5

## BLOCKER

- `Phase 3b` omits the shared official-API archive helpers that the proposed `enrichment/` split needs. `archive_exchange` is defined in `crates/jurisearch-cli/src/main.rs:4332` and is called from both sides of the proposed split: `enrich_legislation_citations_payload` calls it at `main.rs:4778`, while `enrich_decision_from_judilibre_with_client` calls it for Judilibre `/search` and `/decision` at `main.rs:4892` and `main.rs:4908`. It also depends on `sha256_hex` at `main.rs:4321`. The Judilibre path additionally calls `archive_local_unsupported` at `main.rs:4878`, defined at `main.rs:4372`. The plan assigns `enrich_legislation_citations_payload` to `enrichment/legislation.rs` and `enrich_decision_from_judilibre_with_client` to `enrichment/judilibre_zones.rs`, but does not assign these archive helpers anywhere. Implementing the plan literally would either leave enrichment submodules calling back into `main.rs`, create an avoidable sibling dependency, or duplicate archive semantics.
  - Concrete fix: add an explicit `enrichment/archive.rs` or `enrichment/mod.rs` shared helper bucket in the module map and Phase 3b move list for `sha256_hex` and `archive_exchange`. Put `archive_local_unsupported` either in `judilibre_zones.rs` or in the same archive helper module with a Judilibre-only note. Also update the Phase 3b dependency text to say both legislation resolution and Judilibre zone enrichment share the durable `official_api_responses` archive helper.

## WARN

- None.

## NIT

- None.

## Verified Resolutions

- Q1 is source-accurate: `fetch_payload` parses `DecisionPart` directly via `DecisionPart::parse` at `crates/jurisearch-cli/src/main.rs:4059`, then calls `annotate_fetched_parts`; `annotate_fetched_parts` reaches `official_decision_part`, `zone_cache_action`, and `part_block_from_cached_zones`, so keeping generic fetch in `retrieval.rs` with a small public decision-part parsing surface is coherent.
- Q2 is source-accurate: the France-LEGI `emit_eval` branch and `emit_artifact` both pretty-serialize the same `Value`, create the parent directory when needed, write `format!("{rendered}\n")`, then print through `write_json`; `write_json` uses pretty JSON plus one trailing newline.
- Q3 is source-accurate: `compiled_schema()` is a hand-maintained `json!({ ... })` literal in `crates/jurisearch-core/src/schema.rs`, and the existing invariant test is `every_command_schema_name_resolves`. The 36-commit churn count since 2026-06-20 also matches `git log`.
- Q4 is source-accurate: the `Session*Args` structs derive `Deserialize`/`Default` rather than `clap::Args`, use serde defaults, and are consumed by `serde_json::from_value` in the session wrappers only.

VERDICT: FIXES_REQUIRED
