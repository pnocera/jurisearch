Inspection complete. All data shapes verified against their definitions, both test suites pass locally, and the schema additions match the existing style. Here is the review.

---

# Code Review — Phase 1.7 Phase 1 Gate Status

**Date:** 2026-06-21
**Reviewer:** Claude (Opus 4.8), automated diff review
**Scope:** First Phase 1.7 evaluation/migration-gate slice — fail-closed `phase1_gate` object in `jurisearch status`, machine-readable `phase1_eval_fixture_summary`, schema + implementation-plan updates. Files: `crates/jurisearch-core/src/eval.rs`, `crates/jurisearch-core/src/schema.rs`, `crates/jurisearch-cli/src/main.rs`, `crates/jurisearch-cli/tests/cli_contract.rs`, `work/03-implementation/IMPLEMENTATION_PLAN.md`.

## Summary

The slice adds a composed, fail-closed Phase 1 readiness gate to `status` and a machine-readable fixture summary to the core eval module. I verified every field the gate reads against its producer struct, traced all status branches, and re-ran the targeted test suites — `cargo test -p jurisearch-core eval` (5 passed) and `cargo test -p jurisearch-cli --test cli_contract` (36 passed, 2 ignored). The code is correct, panic-free, fail-closed by construction, and the schema additions are consistent with the surrounding style. Findings below are all non-blocking.

### Field-shape verification (all correct)

The gate reads `ingest_health` / `index` / `EmbeddingManifest` fields that all match their producers:
- `ingest_health["state"]` = `"available"` is set by `ingest_health_payload` (`main.rs:2732`); `pending`/`unavailable` handled elsewhere — gate treats both as not-`available`. ✅
- `latest_completed_run` re-inserted as `Option<String>` (`main.rs:2736`); `.is_string()` correctly distinguishes a completed run from `null`. ✅
- `failed_members`, `projection_coverage{covered,total}`, `embedding_coverage`, `replay_snapshot_status` all match `IngestHealthReport` (`ingest_accounting.rs:133–149`); `replay_snapshot_status` is exactly `"empty"`/`"available"` (`ingest_accounting.rs:544–552`), and the gate keys on `"available"`. ✅
- `EmbeddingManifest.provisional` exists (`jurisearch-embed/src/lib.rs:61`). ✅
- `index["query_ready"]` is itself derived from the same coverage completeness (`main.rs:2685–2691`), so it can never contradict the separate coverage checks. ✅

## Findings (ordered by severity)

### 1. (Low) Pass/fail branches of the gate are executed but never asserted
The only value-level assertions on `phase1_gate` are in the no-index, all-`pending` path (`cli_contract.rs:289–303`). The populated-index test `status_reports_ingest_health_from_existing_index` *executes* the pass branches (`coverage_value_complete`→true, `failed_members==0`, `latest_completed_run` is_string, `replay_snapshot=="available"`) but asserts nothing about `phase1_gate`, so a mis-mapping of a real pass→pending/fail would not be caught. The `failed_members > 0` → `"fail"` branch (`main.rs:2550–2553`) is never exercised by any test.

This is mitigated — the branches do run without panic in the existing test, and `coverage_value_complete` (`main.rs:2651`) mirrors the already-tested `coverage_complete` — so it is not a blocker. **Recommendation:** add a cheap unit test (in a `#[cfg(test)]` module in `main.rs`) that calls `phase1_gate_payload` with synthetic `Value`s and a non-provisional `EmbeddingManifest` to assert the pass-mapping and the `failed_members > 0` fail-mapping. No Postgres needed since the function takes plain `Value`s.

### 2. (Low) `reranker_decision` is a hardcoded `"pending"` with no data source — the gate is currently unconditionally closed
`reranker_decision` is always `"pending"` (`main.rs:2597–2601`), so `claim_allowed` (`main.rs:2602–2604`) can never be `true` regardless of system state. This is the intended fail-closed behavior for this slice and is documented in the plan's "Remaining" section, so it is acceptable. **Recommendation:** leave a `// TODO(phase1):` marker on the reranker check so the eventual wiring to real benchmark-gate state is not forgotten; until then no deployment — however ready — can flip the gate open, which is the desired conservative posture but worth making explicit in the code.

### 3. (Low) `final_embedding_model` trusts a self-declared config flag, not stored evidence
The check passes purely on `!embedding_manifest.provisional` (`main.rs:2588–2596`). A user could flip the gate's intent by setting `provisional: false` in an embedding config without any Phase 1 retrieval-metric evidence. The always-pending reranker check currently masks this (the gate can't open anyway). **Recommendation:** when the gate is later wired to open, back `final_embedding_model` (and `release_gating_eval_fixtures`) with recorded benchmark/selection evidence rather than a config boolean alone.

### 4. (Nit) Inconsistent message semantics across checks
`index_query_ready` emits a status-specific message (`main.rs`, two-arm conditional), while every other check emits a single requirement-style message regardless of outcome (e.g. `failed_members` says "must be zero" even when it passes). Harmless for machine consumers reading `status`, but inconsistent. Optional: standardize on requirement-style messages, or make all status-specific.

### 5. (Nit) Minor duplication
`index["query_ready"].as_bool().unwrap_or(false)` is evaluated twice for the same check (status arg + message arg) at `main.rs:2530–2542`. A `let query_ready = …;` binding would read cleaner. Cosmetic.

## Positive notes
- Gate is genuinely fail-closed: any missing/`pending`/serialization-error input degrades to non-`pass`, and a `"fail"` (failed members) also blocks. No `unwrap`/index-panic paths — all reads use `as_bool`/`as_i64`/`is_string` with safe fallbacks.
- `Phase1GateStatus` (`main.rs:2624–2649`) is a tidy `From<bool>`/`From<&'static str>` abstraction; bool→`pass`/`pending` mapping is correct for the coverage checks.
- Schema additions (`schema.rs:339–366`) match the existing convention exactly (no top-level `"type": "object"`, bare `"enum"` arrays), and the contract test pins the `$ref` wiring.
- `EvalFixtureSummary` uses `BTreeMap` for deterministic category ordering — good for stable JSON output.
- The plan's "Current status" block (`IMPLEMENTATION_PLAN.md:719–724`) accurately states the gate is fail-closed and that built-in fixtures are dev (non-gating), matching the code (`release_gating: 0`).
- No secret/PII exposure — the gate only composes already-published status fields.

## Recommendations (all non-blocking)
1. Add a unit test for `phase1_gate_payload` covering pass and `failed_members`-fail mappings (Finding 1).
2. Add a `TODO(phase1)` marker on the hardcoded `reranker_decision` check (Finding 2).
3. When the gate is later allowed to open, back `final_embedding_model` / `release_gating_eval_fixtures` with stored evidence (Finding 3).
4. Optionally harmonize check messages and de-duplicate the `query_ready` evaluation (Findings 4–5).

## Verdict: GO

The slice is correct, fail-closed, schema-consistent, and verified green by both targeted test suites. All findings are non-blocking; it is acceptable to commit after optionally applying the recommendations above (the unit test in Finding 1 being the most valuable follow-up).
