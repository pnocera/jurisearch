# Code Review: Refactoring Plan Resync R2

Round-1 fixes verified: the Phase 3a/3b/3c ordering, suggested commit plan, and risk-area wording now agree that enrichment is extracted before `fetch_payload`; the `decision_zones` versus legislation-citation storage split matches source; the embedding-pool wrappers are kept with `embedding_runtime.rs`; the working-tree wording now excludes the plan document; the variant and `cli_contract.rs` counts are current; and the official-api Legifrance split is scoped to the generic exchange client.

## BLOCKER

None.

## WARN

- `work/06-refactoring/refactoring-plan.md:191-193` says the citation parsing/state helpers listed for the Phase 3a retrieval split are "used only by cite/related", but `parse_citation_target` is also used by the eval path: `france_juris_cite_documents` at `crates/jurisearch-cli/src/main.rs:2709` calls it before `citation_lookup_json`, while `cite_payload` at `crates/jurisearch-cli/src/main.rs:5180` is the other caller. Moving it as a retrieval-only helper would either make the later `eval.rs` split depend back on `retrieval.rs` for a shared parser, or require a second move.
  - Recommended fix: amend Phase 3a to distinguish the helpers. Keep `classify_citation_state` and `annotate_valid_matches` with `cite_payload`, but either place `parse_citation_target` in a small shared citation helper module used by both retrieval and eval, or explicitly document the intended eval-to-retrieval dependency.

## NIT

None.

VERDICT: FIXES_REQUIRED
