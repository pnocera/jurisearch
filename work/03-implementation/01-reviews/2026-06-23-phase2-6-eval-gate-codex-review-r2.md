# Codex Review r2 - Phase 2.6 Evaluation Gate

Reviewed change: `b999938 Phase 2.6 r2: genuinely fail-closed benchmark contract (codex 2 BLOCKERs + 2 WARNs)`

## BLOCKER

1. `decision_citation.identifiers` still does not prove per-identifier benchmark coverage.

   Evidence: `crates/jurisearch-cli/src/main.rs:8247` validates a single aggregate `decision_citation` category with one `value` and one `queries` count. The new coverage check at `crates/jurisearch-cli/src/main.rs:8257` only lowercases the string values in `decision_citation.identifiers` and verifies that the names `ecli`, `pourvoi`, and `cetatext` are present. There is no per-identifier query count, metric, or result breakdown. The evidence contract likewise documents only an aggregate `decision_citation` accuracy over at least 30 queries plus an `identifiers` list at `work/03-implementation/02-evidence/2026-06-23-phase2-eval-gate.md:26`.

   Impact: an artifact can pass the gate with `queries: 30`, `value: 0.95`, and `identifiers: ["ecli", "pourvoi", "cetatext"]` even if all measured citation queries were ECLI-only and pourvoi/CETATEXT had zero evaluated examples. That still opens `phase2_gate.claim_allowed` for the full "ECLI/pourvoi/CETATEXT decision-citation verification" claim while one or two identifier families may not have been measured at all.

   Concrete fix: make identifier coverage measurable, not just declarative. For example, replace or supplement `identifiers` with `identifier_breakdown` entries keyed by `ecli`, `pourvoi`, and `cetatext`, each with `metric=decision_citation_accuracy`, `queries >= <floor>`, and `value >= <floor>`, and reject missing or below-floor entries. Add a negative test where the identifier list contains all three names but the breakdown omits `pourvoi` or gives it zero queries.

## WARN

1. The public schema still does not fully match the emitted Phase 2 benchmark payload.

   Evidence: `phase2_benchmark_payload_with_path` emits a raw `artifact` field whenever a benchmark file is parsed (`crates/jurisearch-cli/src/main.rs:8142`), but `Phase2BenchmarkGate` does not declare `artifact` in the public schema (`crates/jurisearch-core/src/schema.rs:498`). The same function copies `artifact["state"]` verbatim into `artifact_reported_state` (`crates/jurisearch-cli/src/main.rs:8150`), while the schema declares that field as only `string | null` (`crates/jurisearch-core/src/schema.rs:504`). A malformed artifact with `"state": false` or `"state": {}` will therefore produce a status payload that violates the schema even though the artifact is correctly failed.

   Concrete fix: either add `artifact: { "type": ["object", "null"] }` to `Phase2BenchmarkGate` and coerce `artifact_reported_state` to `artifact["state"].as_str().map_or(Value::Null, json)`, or stop emitting the raw artifact and make the schema reflect only the normalized diagnostic surface.

## NIT

1. The production provenance comment contradicts the validator and fixture.

   Evidence: the comment says the benchmark must have "no sampling/human/LLM gold" at `crates/jurisearch-cli/src/main.rs:8208`, but the validator only requires `sampled=false` and that `human_in_gold` / `llm_in_gold` are booleans (`crates/jurisearch-cli/src/main.rs:8221`). The valid test artifact even sets `llm_in_gold: true` at `crates/jurisearch-cli/src/main.rs:9397`.

   Concrete fix: reword the comment to match the contract, e.g. "sampled=false, with human_in_gold/llm_in_gold recorded as booleans", unless the intended policy is actually to reject human or LLM gold.

## Verification

- `git diff --check HEAD~1 HEAD` passed.
- `cargo test -p jurisearch-cli phase2_` passed: 5 tests.
- `cargo test -p jurisearch-cli --test cli_contract status_returns_json_without_index` passed.
- `cargo test -p jurisearch-core schema` passed.

VERDICT: FIXES_REQUIRED
