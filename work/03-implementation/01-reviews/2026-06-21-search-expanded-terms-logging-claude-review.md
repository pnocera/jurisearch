# Review: Phase 1.3 — Search `expanded_terms` Logging

**Date:** 2026-06-21
**Reviewer:** Claude (Opus 4.8)
**Scope:** `search_payload` enriches `SearchResponse` with `expansion_seed_version` and `expanded_terms` from `expand_query` without changing ranking; schema + CLI contract tests updated; `IMPLEMENTATION_PLAN` status updated.

## Files inspected

- `crates/jurisearch-cli/src/main.rs` — `search_payload` (diff lines 501–506), plus surrounding flow (436–515) and `expand_payload` (589–595) for parity.
- `crates/jurisearch-core/src/expand.rs` — `expand_query` / `ExpandedTerm` / `ExpansionResponse`.
- `crates/jurisearch-core/src/schema.rs` — `SearchResponse` (88–134) vs `ExpandResponse` (195–219).
- `crates/jurisearch-cli/tests/cli_contract.rs` — schema assertion (62–65) and BM25 search assertion (538–546).
- `work/03-implementation/IMPLEMENTATION_PLAN.md` — status delta (611–616).

## Verification performed

- `cargo check -p jurisearch-cli -p jurisearch-core` — clean.
- `cargo test -p jurisearch-cli ... help_schema_json_is_valid_and_lists_commands` — pass.
- `cargo test -p jurisearch-core expand` — 3 pass.

## Findings

### Correctness — clean

1. **Ranking is genuinely untouched.** `expand_query(&args.query)` runs *after* `hybrid_candidates_json` returns; its output is appended to the parsed `response` value and never reaches `query_text`, `query_embedding`, the `HybridCandidateQuery`, or the SQL. The "log only, no ranking effect" claim holds. (main.rs:501–506)
2. **Raw query is the correct source.** Expansion uses `&args.query`, matching the `expand` command (`expand_payload`, main.rs:593). `expand_query` normalizes accents/punctuation internally (`normalize_for_match`), so passing the un-normalized query — rather than `query_text`/`parade_query_text` output — is intentional and consistent. ✓
3. **Schema mirrors actual output.** The new `SearchResponse.expansion_seed_version` (string) and `expanded_terms` array-of-object schema field set (`term`, `matched_terms`, `source_seed_id`, `source_label`, `source_citation`, `review_status`, `reviewer`, `rationale`) matches `ExpandedTerm` exactly. Inserted in the right object, between `retrieval_mode` and `as_of`. ✓
4. **No key collision.** Only `expansion_seed_version` and `expanded_terms` are added; the expansion's own `query` field is dropped, so it cannot clobber the search response's existing `query`. The `response[...] = ...` index-assignment is safe because the upstream payload is a known JSON object (already indexed via `response["candidates"]`). ✓
5. **Session parity.** `session_search_payload` delegates to `search_payload`, so JSONL session search emits the same fields under the same single `SearchResponse` schema. ✓
6. **Plan delta is accurate.** New "Done" line scopes it precisely ("without feeding those terms into ranking yet"); the "Remaining" line is correctly narrowed from "connect expand into search-time logging" to "feed `expanded_terms` into explicit ranking experiments."

### Minor / nits (non-blocking)

1. **Key naming diverges from `ExpandResponse`.** Search emits `expansion_seed_version`; the `expand` command emits `seed_version` for the identical value (`legal-vocabulary-seed:v1`). The `expansion_`-prefix disambiguates well within a search payload, but a consumer handling both responses must special-case the key. Deliberate and defensible — flagging for awareness, not change.
2. **Unreachable error branch.** `serde_json::to_value(expansion.expanded_terms)` cannot fail (every field is `&'static str` / `Vec<&'static str>`), so the `.map_err(dependency_unavailable)` is dead code; `dependency_unavailable` is also a semantically loose code for a serialization fault. Both mirror the existing convention in this file (`from_str` and `expand_payload` map the same way), so it's consistent — could simplify to `json!(...)` / `expect` but not required.
3. **Expansion computed on the empty-results path too.** When `candidates` is empty the function returns `Err(no_results(...))` and the just-computed `expanded_terms` is discarded. Negligible wasted work (in-memory match over 3 seeds); no correctness impact.
4. **Schema block duplicated.** The `expanded_terms` item schema is now copied verbatim in both `SearchResponse` and `ExpandResponse`. These are hand-maintained mirrors of the Rust `ExpandedTerm` struct with no compile-time link, so a future field change must touch three places. Pre-existing pattern, not a regression.

### Test coverage

- Unconditional: `help_schema_json...` asserts `SearchResponse.properties.expanded_terms.type == "array"`. ✓
- Runtime: the BM25 search test asserts `expansion_seed_version == "legal-vocabulary-seed:v1"` and that `expanded_terms` contains `article 1240` / `civil-liability-fault-damage`. Query `"responsabilite civile"` matches `match_terms` `responsabilite`, and `article 1240` is not in the query so it survives the in-query filter — assertion is sound. This test is gated behind `discover_pg_config` and is skipped without a Postgres backend.
- Gap (low priority): no test asserts the schema advertises `expansion_seed_version`, and the only runtime assertion of the new fields is PG-gated, so CI without Postgres exercises the schema field but not the live wiring.

## Recommendations

1. (Optional) Add a one-line schema assertion for `SearchResponse.properties.expansion_seed_version.type == "string"` so the new field has unconditional (non-PG) coverage alongside `expanded_terms`.
2. (Optional, follow-up) When the shared `ExpandedTerm` item schema next changes, consider hoisting it to a single `$defs`/shared literal referenced by both `SearchResponse` and `ExpandResponse` to remove the triple-maintenance risk.
3. (Optional) Drop the unreachable `.map_err` on the `to_value` of `expanded_terms`, or leave as-is for stylistic consistency — author's call.

None of the above blocks the change. Behavior is correct, scoped, schema-accurate, and ranking-neutral as claimed.

Verdict: GO
