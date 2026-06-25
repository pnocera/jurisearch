# Authority-Aware Jurisprudence Ranking - Implementation Plan

Date: 2026-06-24
Status: IMPLEMENTATION PLAN. Builds
`05-ranking/2026-06-24-authority-aware-ranking-design.md`.
Target index: `/mnt/models/jurisearch-index/phase2-full-juridic` unless a review gate specifies a
clone. Current schema head is v17.

This is the build order. It does not run anything; each phase is independently reviewable before
execution. The controlling invariant is unchanged from the design: when `authority_weight` is unset or
effectively `0.0`, default search, session search, pagination, Phase 2 gate inputs, and zone retrieval
must remain byte-identical to today's output.

---

## 0. Preconditions and current-source adjustments

- The shipped zone subsystem already exists: `SearchArgs.zone`, `SessionSearchArgs.zone`,
  `zone_search_payload`, `zone_candidates_json`, zone readiness, and `eval france-juris-zones` are live.
  The old helper-extraction prerequisite from the zone plan is complete; `effective_rrf_weights`,
  `effective_probes`, and `DecisionFilters::predicate` are already shared from `retrieval.rs`.
- v1 uses no migration. It reads `documents.source` plus `canonical_json->>'publication'` only inside
  the already-fetched authority window. The deferred v18 projection/PBRI work remains out of scope.
- v1 exposes only `authority_weight`. Keep `authority_band` as a fixed constant in the authority module
  unless R5 proves it needs tuning. One user knob is enough to validate the layer.
- Generic `eval tune` currently evaluates article qrels through `kind_filter: Some("article")`. Do not
  attach authority tuning to that path unless a decision-corpus mode is added. Authority tuning belongs
  in the new France-juris authority benchmark first. This intentionally supersedes design §7.3: the
  generic tune path is statute-oriented today, so an authority sweep there would be inert or misleading.
- Existing untracked work under other implementation directories is unrelated and must not be touched.

## 1. Sequencing

```text
A1 authority model/helper
  -> A2 gated candidate projection
  -> A3 CLI/session config and validation
  -> A4 main + zone wiring, widened window, first-page pagination
  -> A5 contract/integration tests
  -> A6 measured-only authority benchmark and sweep

A7 optional/deferred: v18 authority projection and Judilibre PBRI refinement
```

A1-A3 can land without changing any default retrieval output. A4 is the first phase with observable
behavior, and only behind `authority_weight > 0.0`.

---

## A1 - Authority model and pure rerank helper

Goal: add the legal authority model and deterministic reranker with no retrieval wiring.

Tasks:

- Add `crates/jurisearch-storage/src/authority.rs` and export it from `lib.rs`.
- Define:
  - `AuthorityOrder::{Judicial, Administrative}`
  - `AuthorityTier { order, tier, tier_max, marker_absent }`
  - `AUTHORITY_DEFAULT_BAND: f64 = 0.05`
  - `AUTHORITY_RERANK_WINDOW: u32 = 8`
  - `effective_authority_weight(options: &RetrievalOptions) -> Option<f64>`
  - `authority_tier(source: &str, publication: Option<&str>) -> Option<AuthorityTier>`
  - `authority_rerank(candidates: &mut [serde_json::Value], weight: f64, band: f64)`
- Extend `RetrievalOptions` with `authority_weight: Option<f64>`. Do not add any environment fallback.
- `effective_authority_weight` is the load-bearing ON/OFF primitive: it returns `None` when the field is
  unset, non-finite, or `<= 0.0`, and `Some(w)` only for finite `w > 0.0`. The CLI validator rejects
  non-finite values, but the helper must still be defensive so `rerank_on = effective_weight.is_some()`
  can never treat `0.0` as ON.
- Keep the model exactly as the design specifies:
  - judicial: `cass+publication=oui -> 3`, unpublished `cass -> 2`, `inca -> 1`, `capp -> 0` with
    `marker_absent=true`
  - administrative: `jade` publication class `A -> 2`, `B -> 1`, other/absent -> `0`, absent marked
  - unknown/non-decision sources return `None`
- `authority_rerank` must:
  - assume candidates are already sorted by relevance
  - read `scores.rrf`, `source`, optional `publication`, and document/chunk id
  - reorder only same-order candidates inside the relative relevance band
  - never use a cross-order authority number
  - annotate ON-path candidates with an `authority` block
  - preserve deterministic fallback order by existing id when adjusted scores tie
  - be a defensive no-op for `weight <= 0.0`

Acceptance:

- Unit tests cover every tier, missing publication, case-insensitive markers, unknown sources, and the
  `marker_absent` honesty flag.
- Unit tests prove `effective_authority_weight(None) == None`,
  `effective_authority_weight(Some(0.0)) == None`, and positive finite weights return `Some`.
- Unit tests prove `weight=0.0` preserves input order and adds no misleading authority boost.
- Unit tests prove out-of-band rows do not move, cross-order rows do not move by authority, and same-band
  same-order rows can move when the higher tier is close enough.
- No call site invokes the helper yet.

Review gate A1:

- Scope is the new module, the `RetrievalOptions` field, and tests only.
- Verify separate judicial/administrative scales, no env fallback, deterministic ordering, and no
  retrieval behavior change.

---

## A2 - Gated candidate projection

Goal: make `publication` available to the ON path without changing OFF SQL or OFF payload shape.

Tasks:

- Add a `project_authority: bool` field to `HybridCandidateQuery` and `ZoneCandidateQuery`.
- In `hybrid_candidates_json`, conditionally include publication only when `project_authority=true`:
  - chunk path `limited` SELECT: `d.canonical_json->>'publication' AS publication`
  - document path `scored` SELECT: `d.canonical_json->>'publication' AS publication`
  - candidate `jsonb_build_object`: add `'publication', publication`
- In `zone_candidates_json`, conditionally include publication only when `project_authority=true`:
  - `scored` SELECT: `d.canonical_json->>'publication' AS publication`
  - candidate JSON: add `'publication', publication`
- Build the SQL with two explicit fragments or two explicit format branches. The false branch must emit
  today's SQL text, not `NULL AS publication` or an extra JSON key.
- Set all existing call sites to `project_authority: false` until A4 wires the ON path.

Acceptance:

- Storage SQL tests assert that `project_authority=false` output is unchanged for chunk, document, and
  zone builders.
- Storage tests assert that `project_authority=true` includes `publication` in returned candidates.
- Default CLI/session search output still has no `publication` or `authority` block.

Review gate A2:

- Scope is query struct fields, SQL projection branches, and tests.
- Verify OFF SQL/payload identity and that the zone path mirrors the main path.

---

## A3 - CLI/session config and validation

Goal: expose the explicit knob on both public request surfaces while keeping `0.0` an exact OFF path.

Tasks:

- Add `authority_weight: Option<f64>` to `SearchArgs` and `SessionSearchArgs`.
- Add `--authority-weight` help text: decision-only authority rerank, valid `[0.0, 1.0]`, default off,
  `0.0` treated as off.
- Thread the field through `SearchArgs::retrieval_options()` and `session_search_payload`.
- Update `validate_retrieval_options`:
  - reject non-finite values
  - reject values outside `[0.0, 1.0]`
  - allow `0.0`
- Add a small helper at the CLI boundary, or use the storage helper, to compute:
  `rerank_on = effective_authority_weight(&args.retrieval_options()).is_some()`.
- Add validation rules:
  - non-zone main search: `rerank_on` requires `--kind decision`; reject `code` and `all`
  - zone search: `--kind code` remains rejected; `all` is acceptable because `--zone` is already a
    case-law route
  - `rerank_on` plus inbound cursor is rejected in both main and zone paths
  - `--authority-weight 0.0` must bypass all authority-specific rejections except numeric validation

Acceptance:

- CLI contract tests cover valid unset, valid `0.0`, valid positive weight, negative, >1.0, NaN/inf if
  representable, `--kind code`, `--kind all`, and cursor+positive-weight rejection.
- Session search accepts and validates the same field.
- `RetrievalOptions::default()` remains inert.

Review gate A3:

- Scope is argument/session/schema/validation wiring only.
- Verify no environment fallback and that `0.0` is indistinguishable from unset for routing.

---

## A4 - Window widening, rerank wiring, and first-page pagination

Goal: enable authority ranking behind `authority_weight > 0.0` in the whole-decision and zone paths.

Tasks:

- In `search_with_postgres`:
  - compute `rerank_on` once from the effective weight
  - when OFF, keep current `query_limit = top_k + 1`
  - when ON, set `W_eff = min(AUTHORITY_RERANK_WINDOW, pool_multiplier)` where the pool multiplier is
    `4` for chunk grouping and `20` for document grouping
  - set ON `query_limit = top_k * W_eff + 1` using saturating arithmetic
  - pass `project_authority: rerank_on` into `HybridCandidateQuery`
  - after JSON parse and before truncation, call `authority_rerank` on `response["candidates"]` only
    when the chosen backend is the hybrid candidate path; structured citation responses bypass
    authority entirely
  - truncate to `top_k` after reranking
  - when ON and the hybrid candidate path ran, force `next_cursor = null` and
    `cursor_supported = false`
  - add routing/diagnostic fields: `authority.enabled`, `authority.weight`, `authority.window_factor`,
    and `authority.paging = "first_page_only"`
- In `zone_search_payload`:
  - same `rerank_on`, window factor, gated projection, rerank before truncation, and first-page-only
    pagination
  - keep the existing zone scope block and zone readiness logic unchanged
- Update `search_pagination_value` to support an authority-specific note, or mutate the note after
  constructing the pagination object. The ON note must say that authority rerank is first-page-only in
  v1 and that cursor paging is disabled for this response.
- Do not modify `parse_search_cursor` or introduce an authority cursor tag.
- Do not move authority into SQL `ORDER BY`; the final SQL relevance expression remains unchanged.
- Keep the helper's sort stable over the already-SQL-ordered window. If an explicit fallback id is
  needed, use `chunk_id` for chunk grouping and `document_id` for document/zone grouping.

Acceptance:

- Unset and `0.0`:
  - `query_limit` diagnostics remain today's value
  - `cursor_supported` and `next_cursor` behavior remain today's behavior
  - no `publication` key, no `authority` block, no authority routing block that changes existing
    golden output unless the test explicitly asks for detailed diagnostics after a positive weight
- Positive weight:
  - query limit widens and is clamped by grouping
  - candidates include `publication` while the helper runs
  - displayed candidates include an `authority` block
  - `pagination.next_cursor` is null and `cursor_supported=false`
  - inbound cursor is rejected before query execution
  - main and zone paths use the same rerank helper
- Structured citation results remain unaffected; authority only applies to hybrid candidate responses
  over decisions, and structured responses keep their existing exact-result pagination behavior.

Review gate A4:

- Scope is the main/zone runtime wiring, first-page pagination contract, and diagnostics.
- Verify OFF code path and JSON are unchanged, ON never emits a legacy cursor, the window cannot outrun
  the candidate arm pool, and the zone path is not treated as a second implementation.

---

## A5 - Contract and regression tests

Goal: lock the invariant and the user-visible behavior before any benchmark work.

Tasks:

- Storage tests:
  - `authority_tier` and `authority_rerank` unit tests from A1
  - `hybrid_candidates_json` false projection SQL/payload identity tests
  - `zone_candidates_json` false projection SQL/payload identity tests
  - true projection tests for main chunk, main document, and zone candidates
- CLI contract tests:
  - default `search` output for an existing fixture remains unchanged
  - `--authority-weight 0.0` produces the same output as unset for the same query
  - `--authority-weight > 0` on a decision search disables pagination and emits authority metadata
  - positive weight with `--cursor` fails with `bad_input`
  - positive weight with main `--kind code` and `--kind all` fails with `bad_input`
  - session search mirrors the CLI behavior
  - zone search with positive weight disables pagination and carries authority metadata
- Phase 2 guard tests:
  - ensure the Phase 2 artifact validator still accepts the unchanged `eval france-juris` artifact
  - ensure gate re-derivation ignores any authority benchmark artifact because it is a separate kind
  - ensure the Phase 2 gate command itself stays knob-free; `eval france-juris` does not gain or read
    `--authority-weight`

Suggested commands:

```bash
cargo test -p jurisearch-storage authority
cargo test -p jurisearch-storage retrieval
cargo test -p jurisearch-storage zone
cargo test -p jurisearch-cli cli_contract
cargo test -p jurisearch-cli france_juris
```

Acceptance:

- All focused tests pass.
- A committed automated golden/contract test shows unset and explicit `0.0` are identical for CLI and
  session search, and default output remains stable.
- No schema migration is required.

Review gate A5:

- Scope is tests and any tiny testability hooks.
- Verify the tests would fail if `0.0` takes the ON path, if OFF projection leaks `publication`, or if
  ON emits a legacy cursor.

---

## A6 - Measured-only authority benchmark and sweep

Goal: measure authority ordering without changing Phase 2 gate claims.

Tasks:

- Add a separate `eval france-juris-authority` subcommand and artifact kind
  `phase2_authority_benchmark`.
- Reuse the France-juris decision qrel/gold discipline: official indexed fields only, no LLM, no human
  labels, no archive re-parse.
- Build pairwise authority-lift data from a benchmark-only widened, `project_authority=true`,
  un-reranked fetch. This fetch is not the production OFF path; it exists so the benchmark can derive
  the pair set, `authority_lift_off` from natural relevance order, and `authority_lift_on` after applying
  `authority_rerank` to the same window.
  - candidates must be in the same order, judicial or administrative
  - candidates must both be in the benchmark widened window for the query
  - candidates must be inside the same pre-rerank relevance band
  - candidates must have different authority tiers
  - candidates with `marker_absent=true` are excluded from pair formation
- Report:
  - `authority_lift_off`
  - `authority_lift_on`
  - `authority_lift_delta`
  - pair coverage
  - per-order/per-source breakdown
  - score-gap distribution
  - recall@10 OFF vs ON for judicial and administrative categories
  - zone recall@10 OFF vs ON if `--include-zones` is set
- Add an authority sweep to this new benchmark, not to generic article-qrel `eval tune`:
  `--authority-weights 0.0,0.1,0.25,0.5`
- If generic `eval tune` is later extended, first add an explicit decision-corpus mode; do not reuse
  its current article path for authority.
- The mandatory design §7.1 recall regression guard is realized inside this measured-only benchmark,
  not by adding authority knobs to the Phase 2 gate command. Recompute recall@10 with the same gold
  recipe and grouping as `eval france-juris`, then compare OFF vs ON there.

Acceptance:

- Artifact state is `measured`, never `passed`/`failed`.
- Artifact is written separately under the requested `--out`; it is not consumed by the Phase 2 gate.
- For every tested positive weight, recall@10, computed with the same gold recipe and grouping as the
  Phase 2 gate, does not regress below the OFF measurement or the Phase 2 floor.
- Coverage is reported prominently so a tiny pair set cannot look conclusive.

Review gate A6:

- Scope is benchmark code, artifact schema, and sweep logic.
- Verify the metric is within-order only, publisher-field-derived, measured-only, and cannot inflate the
  existing Phase 2 corpus claim.

---

## A7 - Deferred authority data enhancements

Do not build this in v1.

Tasks if/when approved:

- Add schema v18 only if authority moves into SQL or needs large-scale filtering:
  - `documents.authority_publication`
  - `(source, authority_publication)` index for decisions
  - idempotent backfill from `canonical_json->>'publication'`
  - `index_manifest` schema-version upsert
- Add Judilibre PBRI projection only behind a sub-knob:
  - Cassation-only, from `official_api_responses.response_json`
  - no invented PBRI signal for `capp` or `jade`
  - benchmark and rollback plan separate from v1
- Revisit deep authority pagination only after a concrete product need:
  - either stateful cursor carrying displayed/window state
  - or SQL-native authority ordering with a matching keyset predicate

Acceptance:

- A7 has its own design/review. It is not a prerequisite for A1-A6.

---

## Rollback

v1 leaves no schema state. Operational rollback is to stop passing `--authority-weight`. Code rollback is
limited to the authority module, the query projection flag, the CLI/session field, and the ON-path
wiring. Because OFF projection and OFF pagination are locked by tests, removing the knob should restore
the prior behavior without data repair.

---

## Source anchors checked while drafting

- `crates/jurisearch-storage/src/retrieval.rs`: `RetrievalOptions`, `HybridCandidateQuery`,
  `hybrid_candidates_json`, shared RRF/probes helpers, candidate JSON projection.
- `crates/jurisearch-storage/src/zone_retrieval.rs`: `ZoneCandidateQuery`, `zone_candidates_json`.
- `crates/jurisearch-cli/src/main.rs`: `SearchArgs`, `SessionSearchArgs`,
  `validate_retrieval_options`, `search_payload`, `zone_search_payload`, `search_with_postgres`,
  `session_search_payload`, `eval_france_juris_payload`, `eval_france_juris_zones_payload`,
  `eval_tune_payload`.
- `crates/jurisearch-storage/src/france_juris.rs`: France-juris gold builders.
- `crates/jurisearch-storage/src/migrations.rs`: schema head v17.
- `crates/jurisearch-cli/tests/cli_contract.rs`, `crates/jurisearch-storage/tests/retrieval_smoke.rs`,
  `crates/jurisearch-storage/tests/decision_projection.rs`, and
  `crates/jurisearch-storage/tests/zone_units.rs`: likely contract/integration test homes.
