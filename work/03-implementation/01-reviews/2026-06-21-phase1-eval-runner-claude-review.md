I have everything I need. Here is my review.

---

# Review — Phase 1.7 Eval Runner (`jurisearch eval phase1`)

**Date:** 2026-06-21
**Reviewer:** Claude (Opus 4.8), automated code review
**Scope:** Uncommitted diff in `/home/pierre/Work/jurisearch` adding the executable Phase 1 eval slice — CLI `eval phase1` (list + execute), session JSONL `eval phase1`, contract/schema registration, and the implementation-plan status update. Files: `crates/jurisearch-cli/src/main.rs`, `crates/jurisearch-cli/tests/cli_contract.rs`, `crates/jurisearch-core/src/contract.rs`, `crates/jurisearch-core/src/schema.rs`, `work/03-implementation/IMPLEMENTATION_PLAN.md`.

I inspected the live diff and the surrounding code; I did not modify any files. Codex's local verification (fmt, core+cli tests, clippy `-D warnings`, full workspace tests) is taken as given for compile/lint/test-pass status — my focus is correctness, contract/output shape, gate safety, JSONL behavior, and coverage.

---

## Findings (ordered by severity)

### 1. No direct test of the core rank-computation logic — *Medium, non-blocking*
`eval_phase1_fixture_search_result` (`main.rs:635`) is the most intricate new code: it computes `best_expected_rank`, `best_allowed_alternate_rank`, `matched_document_id`, `status`, and `passed` from a search payload. It is a **pure function** (`fixture + search Value → Value`), yet none of the new tests exercise it — the four added tests cover only list mode, session list mode, zero-`top_k` rejection, and index-unavailable. The happy execution path (pass / fail / `pass_allowed_alternate` / rank values) is untested because no real index exists in CI.

This is consistent with the plan ("run the executable fixtures against a real completed LEGI index" is listed as *remaining*), so it does not block the slice. **Recommendation:** add an in-crate unit test in the existing `mod tests` (`main.rs:3708`) feeding a synthetic `search` `Value` with crafted `candidates`, asserting rank/status/`matched_document_id` for expected-hit, alternate-only-hit, and miss cases. This pins down the logic the integration tests cannot reach without a database.

### 2. Rank vs `top_document_ids` inconsistency when a candidate lacks `document_id` — *Low*
In the loop at `main.rs:635`, candidates without a string `document_id` are `continue`d, but `rank = index + 1` is derived from the **enumeration index over all candidates**, while `top_document_ids` only collects the ones with an id. If any candidate lacked `document_id`, a reported rank could exceed the position implied by `top_document_ids`, and `candidate_count` (`candidates.len()`) would exceed `top_document_ids.len()`. Harmless in practice — every storage candidate carries `document_id` — but the two counters can diverge. Consider deriving rank from `top_document_ids.len() + 1` after the push, or filtering id-less candidates up front.

### 3. Hierarchy expectations are not exercised by execution — *Low / scoping note*
With `--include-dev`, the two dev fixtures carry rich `hierarchy` expectations (ancestry titles, required/forbidden siblings), but the runner scores them **only on the expected-ID rank** (which equals the `context_document_id`). The `hierarchy` block is ignored and absent from the result. This is acceptable for an expected-ID-rank slice, but worth one line in the plan/help so a reader doesn't mistake `eval phase1` for full hierarchy validation.

### 4. Stale "Phase 0 scaffold" message for bare `eval` — *Cosmetic*
`emit_eval` (`main.rs:520`) and the session dispatch both return `ErrorObject::not_implemented("eval")` for a bare `eval` with no subcommand. That helper's message (`error.rs:38`) says the command "is not implemented in this Phase 0 scaffold yet" — misleading now that `eval phase1` is implemented. Pre-existing wording; only surfaces on a no-subcommand invocation. Optional to address.

### 5. `search_payload` does not validate `as_of` — *Informational*
Unlike `cite_payload`/`context_payload`, `search_payload` (`main.rs:699`) never calls `validate_as_of` (`main.rs:3129`); the fixture `as_of` flows straight to SQL via the storage layer. Not exploitable here — all built-in fixtures use well-formed `YYYY-MM-DD` strings — but the eval runner inherits this gap. No action needed for this slice.

---

## Positive observations (verified, not blockers)

- **Gate safety is preserved.** The eval runner is fully independent of `phase1_gate`. `phase1_eval_fixture_summary()` still reports `release_gating = 0`, and the gate's `release_gating_eval_fixtures`/`reranker_decision` checks remain `pending` (`main.rs:3760-3764`). A consumer cannot turn `"all_passed": true` into a satisfied Phase 1 claim — the gate stays fail-closed. The plan edit correctly keeps "promote release candidates only after named human legal-domain review" and "real-index benchmark evidence" as remaining work.
- **Error handling split is sound.** Per-fixture `NoResults` is captured as a fixture failure (`status:"fail"`, `passed:false`) and the run continues (`main.rs:593`), while genuinely environmental errors (index unavailable, embedding runtime) propagate and abort the whole run — appropriate, since those are global, not per-fixture. Exit codes map correctly: `bad_input → 2`, `index_unavailable → 3`, both asserted in the new tests.
- **Contract/schema are coherent.** `eval phase1` is registered `Implemented` with `EvalPhase1Request`/`EvalPhase1Response` (`contract.rs:151`); both schemas exist (`schema.rs:312`, `:320`); `$ref` targets resolve (`#/schemas/EvalFixtureSummary` at `schema.rs:412`, `#/error_object` at `:16`). The loose response schema validly covers both the list shape (`fixtures`, top-level `fixture_count`) and the run shape (`results`, `summary`, `retrieval_mode`, `top_k`).
- **Output shape is well-formed.** `Detailed` format is hardcoded (`main.rs:607`) so `diagnostics.retrieval` is populated for the compact per-fixture `search` block; `retrieval_mode` genuinely exists at the top of the storage payload (`retrieval.rs:111`), so the extraction is not a silent null. `kind=Code` + the article filter aligns with the `legi:LEGIARTI…` expected IDs.
- **JSONL parity holds.** `session_eval_phase1_payload` (`main.rs:2612`) reuses `eval_phase1_payload` through `SessionEvalPhase1Args` (which adds `index_dir`, consistent with other session arg structs), and the envelope test confirms `ok:true` + preserved `action`/`include_dev`/`fixture_count` and the trailing `bye`.
- **`top_k` guard placement is correct** — rejected only when executing (`!args.list && top_k == 0`), so `--list` is unaffected, matching the schema's `minimum: 1`.

---

## Recommendations

1. (Non-blocking, recommended before/with commit) Add an in-crate unit test for `eval_phase1_fixture_search_result` covering expected-hit, alternate-only-hit, and miss — this is the one piece of new logic with no coverage.
2. (Optional) Make rank and `top_document_ids` derive from the same filtered sequence (Finding 2).
3. (Optional) Note in help/plan that execution checks expected-ID rank only, not hierarchy structure (Finding 3).
4. (Optional) Refresh the `not_implemented` wording or special-case bare `eval` (Finding 4).

None of these are correctness, contract, gate-safety, output-shape, or JSONL defects. The slice does what it claims, keeps the Phase 1 gate fail-closed, and is internally consistent with the existing search/session machinery.

---

**Verdict: GO** — acceptable to commit; address Recommendation 1 (unit test for the rank-computation function) as the highest-value non-blocking follow-up.
