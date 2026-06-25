# Authority-Aware Jurisprudence Ranking - Implementation Plan (v2)

Date: 2026-06-25
Status: IMPLEMENTATION PLAN (v2). Supersedes
`05-ranking/2026-06-24-authority-aware-ranking-implementation-plan.md`. Builds
`05-ranking/2026-06-24-authority-aware-ranking-design.md`.
Target index: `/mnt/models/jurisearch-index/phase2-full-juridic` unless a review gate specifies a
clone. Current schema head is **v17** (`migrations::CURRENT_SCHEMA_VERSION = 17`), unchanged.

This is the build order. It does not run anything; each phase is independently reviewable before
execution. The controlling invariant is unchanged from the design: when `authority_weight` is unset or
effectively `0.0`, default search, session search, pagination, Phase 2 gate inputs, and zone retrieval
must remain byte-identical to today's output.

---

## Why v2 (what changed since the v1 draft)

The v1 plan (2026-06-24) was written against a monolithic `crates/jurisearch-cli/src/main.rs` and a
single `crates/jurisearch-storage/src/retrieval.rs`. Both have since been decomposed (the
`refactor/cli-module-split` work and the storage `retrieval/` split). **The design and the invariant
are unchanged**; only the file/symbol anchors and a few structural realities move. The substantive
deltas a builder must know:

- **`SessionSearchArgs` no longer exists.** Session and one-shot now share one DTO,
  `SearchRequest` (`crates/jurisearch-cli/src/request.rs`). The one-shot path builds it via
  `SearchArgs::into_request`; the session path deserializes it directly
  (`session_search_payload` → `serde_json::from_value::<SearchRequest>`). So the knob is added in **two**
  places, not three: clap `SearchArgs` and the shared `SearchRequest`. Threading and validation then
  happen once on the shared type.
- **`retrieval_options()` / `decision_filters()` moved off the args type** onto `SearchRequest`
  (`request.rs:57`, `request.rs:65`). `RetrievalOptions` itself moved to
  `crates/jurisearch-storage/src/retrieval/types.rs:62`.
- **`search_with_postgres` was decomposed into a `SearchExecution<'a>` context**
  (`crates/jurisearch-cli/src/retrieval/search.rs`): `new` computes the per-request limits,
  `run_structured_citation_or_fallback` routes, `run_hybrid_candidates` builds the
  `HybridCandidateQuery`, and `apply_search_response_envelope` does truncation / next-cursor /
  pagination / routing / diagnostics. The v1 plan described these steps as a single function; they are
  now `&self` methods, and the wiring in A4 attaches to specific methods (below).
- **The candidate SQL is split.** `hybrid_candidates_json` (the `limited`/`scored` candidate
  projection + `jsonb_build_object`) is in `crates/jurisearch-storage/src/retrieval/hybrid.rs`; the
  ranked-candidate CTEs are in `crates/jurisearch-storage/src/retrieval/sql.rs`. The publication
  projection in A2 lands in **hybrid.rs** (the `limited`/`scored` SELECTs that already project
  `d.source`), not in the ranked CTEs.
- **Benchmark scaffolding is shared.** `eval/scoring.rs` now owns `score_known_item_qrels` and
  `benchmark_search_request`; `eval/artifact.rs` owns `mean`/`floor_metric`. The new authority
  benchmark (A6) reuses these instead of re-deriving them. `benchmark_search_request` constructs a
  `SearchRequest` field-by-field, so the new `authority_weight` field must be added there too.
- **Test homes moved.** The old `crates/jurisearch-cli/tests/cli_contract.rs` is now split into
  `cli_retrieval_contract.rs`, `cli_session_contract.rs`, `cli_eval_contract.rs`.

---

## 0. Preconditions and current-source adjustments

- The shipped zone subsystem already exists: `SearchArgs.zone`, the shared `SearchRequest.zone`,
  `zone_search_payload` (`retrieval/zone.rs`), `zone_candidates_json` (`zone_retrieval.rs`), zone
  readiness (`ensure_zone_retrieval_readiness`), and `eval france-juris-zones` (`eval/zones.rs`) are
  live. The helper-extraction prerequisite from the zone plan is complete: `effective_rrf_weights`,
  `effective_probes`, `format_sql_f64`, `document_cursor_predicate`, and `DecisionFilters::predicate`
  are already shared from `retrieval/types.rs` + `retrieval/sql.rs` and re-used by `zone_retrieval.rs`.
- v1 uses no migration. It reads `documents.source` plus `canonical_json->>'publication'` only inside
  the already-fetched authority window. The deferred v18 projection/PBRI work remains out of scope.
- v1 exposes only `authority_weight`. Keep `authority_band` as a fixed constant in the authority module
  unless R5 proves it needs tuning. One user knob is enough to validate the layer.
- Generic `eval tune` (`eval/generic.rs::eval_tune_payload`) sweeps `rrf-dense`/`rrf-lexical`/`probes`
  via `--sweep PARAM=start:stop:step` over caller-supplied question/qrel files; it is statute/question
  oriented and has no decision-corpus mode. Do not attach authority tuning to that path. Authority
  tuning belongs in the new France-juris authority benchmark first. This intentionally supersedes
  design §7.3.
- Existing untracked work under other implementation directories is unrelated and must not be touched.

## 1. Sequencing

```text
A1 authority model/helper
  -> A2 gated candidate projection
  -> A3 CLI/session config and validation
  -> A4 SearchExecution + zone wiring, widened window, first-page pagination
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

- Add `crates/jurisearch-storage/src/authority.rs` and export it from `lib.rs` (top-level module, a
  sibling of `retrieval`/`zone_retrieval`).
- Define:
  - `AuthorityOrder::{Judicial, Administrative}`
  - `AuthorityTier { order, tier, tier_max, marker_absent }`
  - `AUTHORITY_DEFAULT_BAND: f64 = 0.05`
  - `AUTHORITY_RERANK_WINDOW: u32 = 8`
  - `effective_authority_weight(options: &RetrievalOptions) -> Option<f64>`
  - `authority_tier(source: &str, publication: Option<&str>) -> Option<AuthorityTier>`
  - `authority_rerank(candidates: &mut [serde_json::Value], weight: f64, band: f64)`
- Extend `RetrievalOptions` (now at `crates/jurisearch-storage/src/retrieval/types.rs:62`) with
  `authority_weight: Option<f64>`. Keep the struct `Copy + Default`. Do not add any environment
  fallback. Note: `RetrievalOptions` is constructed in `SearchRequest::retrieval_options`
  (`request.rs:57`) and defaulted via `RetrievalOptions::default()` in several eval call sites
  (`eval/mod.rs`, `eval/generic.rs`); `Default` keeps all those inert.
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
  - read `scores.rrf`, `source`, optional `publication`, and document/chunk id from each candidate JSON
    object (the shape emitted by `hybrid_candidates_json` / `zone_candidates_json`)
  - reorder only same-order candidates inside the relative relevance band
  - never use a cross-order authority number
  - annotate ON-path candidates with an `authority` block
  - preserve deterministic fallback order by existing id when adjusted scores tie
  - be a defensive no-op for `weight <= 0.0`

Acceptance:

- Unit tests (inline `#[cfg(test)]` in `authority.rs`, matching the crate convention) cover every tier,
  missing publication, case-insensitive markers, unknown sources, and the `marker_absent` honesty flag.
- Unit tests prove `effective_authority_weight(None) == None`,
  `effective_authority_weight(Some(0.0)) == None`, and positive finite weights return `Some`.
- Unit tests prove `weight=0.0` preserves input order and adds no misleading authority boost.
- Unit tests prove out-of-band rows do not move, cross-order rows do not move by authority, and same-band
  same-order rows can move when the higher tier is close enough.
- No call site invokes the helper yet.

Review gate A1:

- Scope is the new `authority.rs` module, the `RetrievalOptions` field, and tests only.
- Verify separate judicial/administrative scales, no env fallback, deterministic ordering, and no
  retrieval behavior change.

---

## A2 - Gated candidate projection

Goal: make `publication` available to the ON path without changing OFF SQL or OFF payload shape.

Tasks:

- Add a `project_authority: bool` field to `HybridCandidateQuery`
  (`crates/jurisearch-storage/src/retrieval/types.rs:69`) and `ZoneCandidateQuery`
  (`crates/jurisearch-storage/src/zone_retrieval.rs:25`).
- In `hybrid_candidates_json` (`crates/jurisearch-storage/src/retrieval/hybrid.rs`), conditionally
  include publication only when `project_authority=true`:
  - chunk path `limited` SELECT: add `d.canonical_json->>'publication' AS publication`
  - document path `scored` SELECT: add `d.canonical_json->>'publication' AS publication`
  - candidate `jsonb_build_object` (both branches): add `'publication', publication`
  - (These are the SELECTs that already project `d.source`, `d.kind`, etc. — the ranked CTEs in
    `retrieval/sql.rs` do NOT change.)
- In `zone_candidates_json` (`crates/jurisearch-storage/src/zone_retrieval.rs`), conditionally include
  publication only when `project_authority=true`:
  - `scored` SELECT: add `d.canonical_json->>'publication' AS publication`
  - candidate `jsonb_build_object`: add `'publication', publication`
- Build the SQL with two explicit fragments or two explicit format branches. The false branch must emit
  today's SQL text, not `NULL AS publication` or an extra JSON key.
- Set all existing call sites to `project_authority: false` until A4 wires the ON path. **Every**
  `HybridCandidateQuery` / `ZoneCandidateQuery` literal must add the field or the crate will not compile.
  Current construction sites (verified):
  - `HybridCandidateQuery`:
    - `crates/jurisearch-cli/src/retrieval/search.rs:277` — `SearchExecution::run_hybrid_candidates`
      (this is the ON-path site A4 flips to `rerank_on`; `false` here until then)
    - `crates/jurisearch-cli/src/retrieval/compare.rs:48` — `compare` command
    - `crates/jurisearch-cli/src/eval/generic.rs:352` — generic `eval run`/`tune` runner
  - `ZoneCandidateQuery`:
    - `crates/jurisearch-cli/src/retrieval/zone.rs:104` — `zone_search_payload` (ON-path site for A4)
    - `crates/jurisearch-cli/src/eval/zones.rs:136` — measured-only zone benchmark
  - storage integration tests that build these structs:
    `crates/jurisearch-storage/tests/{retrieval_smoke,decision_projection,legi_canonical_retrieval,target_spike_corpus,zone_units}.rs`

Acceptance:

- Storage SQL tests assert that `project_authority=false` output is unchanged for chunk, document, and
  zone builders.
- Storage tests assert that `project_authority=true` includes `publication` in returned candidates.
- Default CLI/session search output still has no `publication` or `authority` block.

Review gate A2:

- Scope is the two query-struct fields, the SQL projection branches in `hybrid.rs` + `zone_retrieval.rs`,
  and tests.
- Verify OFF SQL/payload identity and that the zone path mirrors the main path.

---

## A3 - CLI/session config and validation

Goal: expose the explicit knob on both public request surfaces while keeping `0.0` an exact OFF path.

Tasks:

- Add `authority_weight: Option<f64>` to:
  - `SearchArgs` (`crates/jurisearch-cli/src/args.rs:146`) as a clap `#[arg(long)]`.
  - `SearchRequest` (`crates/jurisearch-cli/src/request.rs:18`) with `#[serde(default)]` so the JSONL
    session path stays optional and matches the one-shot default.
- Thread the field through `SearchArgs::into_request` (`request.rs:80`) and surface it in
  `SearchRequest::retrieval_options` (`request.rs:57`, into `RetrievalOptions.authority_weight`).
- Add `--authority-weight` help text: decision-only authority rerank, valid `[0.0, 1.0]`, default off,
  `0.0` treated as off.
- Add `authority_weight: None` to **every** field-by-field `SearchRequest` literal so the crate still
  compiles and every existing path stays OFF (verified construction sites beyond `into_request`):
  - `crates/jurisearch-cli/src/eval/scoring.rs:72` — `benchmark_search_request` (the France-LEGI /
    France-juris / zone benchmark runners)
  - `crates/jurisearch-cli/src/eval/generic.rs:725` — `eval_phase1_fixture_result` (the Phase 1 LEGI
    fixture runner)
- Update `validate_retrieval_options` (`crates/jurisearch-cli/src/query_support.rs:22`) — it already
  receives `&RetrievalOptions`, so it sees `authority_weight`:
  - reject non-finite values
  - reject values outside `[0.0, 1.0]`
  - allow `0.0`
  - (this runs once in `search_payload` (`retrieval/search.rs:47`), covering both one-shot and session.)
- Add the routing rejections where the route is known — `authority_weight` numeric validation is shared,
  but kind/zone/cursor interactions are not visible to `validate_retrieval_options`. Compute
  `rerank_on = effective_authority_weight(&req.retrieval_options()).is_some()` in the payload builders
  and reject there:
  - `search_payload` (non-zone main path, `retrieval/search.rs:38`): `rerank_on` requires
    `req.kind == decision`; reject `code` and `all`. `rerank_on` plus an inbound `req.cursor` is rejected
    before query execution.
  - `zone_search_payload` (`retrieval/zone.rs:52`): `--kind code` remains rejected (existing); `all` is
    acceptable because `--zone` is already a case-law route. `rerank_on` plus an inbound `req.cursor` is
    rejected.
  - `--authority-weight 0.0` must bypass all authority-specific rejections except numeric validation
    (because `rerank_on` is `false` for `0.0`).
  - Note `search_payload` dispatches to `zone_search_payload` when `req.zone.is_some()` (search.rs:51),
    so put the main-path kind rejection AFTER the zone dispatch, or scope it to the non-zone branch, to
    avoid rejecting `--zone … --kind all`.

Acceptance:

- CLI contract tests cover valid unset, valid `0.0`, valid positive weight, negative, >1.0, NaN/inf if
  representable, `--kind code`, `--kind all`, and cursor+positive-weight rejection.
- Session search (`session_search_payload`) accepts and validates the same field through the shared
  `SearchRequest` deserialization.
- `RetrievalOptions::default()` remains inert.

Review gate A3:

- Scope is `SearchArgs`/`SearchRequest`/`into_request`/`retrieval_options` field wiring (including the
  `benchmark_search_request` and `eval_phase1_fixture_result` literals), the validator numeric rule, and
  the payload-builder routing rejections.
- Verify no environment fallback and that `0.0` is indistinguishable from unset for routing.

---

## A4 - SearchExecution wiring, window widening, and first-page pagination

Goal: enable authority ranking behind `authority_weight > 0.0` in the whole-decision and zone paths.

The single function from v1 is now a `SearchExecution<'a>` context
(`crates/jurisearch-cli/src/retrieval/search.rs:198`). Attach the wiring to its methods:

- In `SearchExecution::new` (`search.rs:222`, where `lexical_limit`/`dense_limit`/`query_limit` are
  computed today):
  - compute `rerank_on` once from `effective_authority_weight(&req.retrieval_options())`
  - when OFF, keep current `query_limit = top_k.saturating_add(1)`
  - when ON, set `W_eff = min(AUTHORITY_RERANK_WINDOW, pool_multiplier)` where `pool_multiplier` is the
    existing `4` for chunk grouping / `20` for document grouping (search.rs:240-243)
  - set ON `query_limit = top_k.saturating_mul(W_eff).saturating_add(1)`
  - store `rerank_on` (and the effective weight / window factor) on the struct for the later steps
- In `SearchExecution::run_hybrid_candidates` (`search.rs:265`): pass `project_authority: self.rerank_on`
  into the `HybridCandidateQuery`.
- In `SearchExecution::apply_search_response_envelope` (`search.rs:354`), which already owns truncation +
  next-cursor + pagination + routing + diagnostics:
  - the routed set carries `chosen_backend` (`RoutedSearch`); structured citation responses set
    `chosen_backend == "structured_citation"`. Gate authority on
    `self.rerank_on && chosen_backend != "structured_citation"` — the same predicate the existing
    `cursor_supported` line uses (search.rs:385). Structured citation responses bypass authority entirely.
  - call `authority_rerank` on `response["candidates"]` BEFORE the existing `top_k` truncation block
    (search.rs:373-381).
  - when authority ran, force `next_cursor = None` and `cursor_supported = false` for the pagination
    block (search.rs:386-392), regardless of the displayed-row cursor.
  - add routing/diagnostic fields: `authority.enabled`, `authority.weight`, `authority.window_factor`,
    and `authority.paging = "first_page_only"`.
- In `zone_search_payload` (`retrieval/zone.rs:52`), which is still a flat function (not a
  `SearchExecution`): apply the same `rerank_on`, the same widened `query_limit` (it computes its own at
  zone.rs:98-100), `project_authority: rerank_on` on the `ZoneCandidateQuery`, the `authority_rerank`
  call before its truncation block (zone.rs:143-150), and the same first-page-only pagination override
  (zone.rs:153-159). Keep the existing zone scope block and zone readiness logic unchanged.
- `search_pagination_value` (`retrieval/search.rs:14`) is shared by both surfaces. Either extend it with
  an authority-specific note parameter, or mutate `response["pagination"]["cursor_note"]` after
  constructing the block. The ON note must say authority rerank is first-page-only in v1 and that cursor
  paging is disabled for this response.
- Do not modify `parse_search_cursor` / `ParsedSearchCursor` or introduce an authority cursor tag.
- Do not move authority into SQL `ORDER BY`; the final SQL relevance expression in
  `hybrid.rs`/`zone_retrieval.rs` remains unchanged.
- Keep the helper's sort stable over the already-SQL-ordered window. If an explicit fallback id is
  needed, use `chunk_id` for chunk grouping and `document_id` for document/zone grouping.

Acceptance:

- Unset and `0.0`:
  - `query_limit` diagnostics remain today's value (`top_k + 1`)
  - `cursor_supported` and `next_cursor` behavior remain today's behavior
  - no `publication` key, no `authority` block, no authority routing block that changes existing
    golden output unless the test explicitly asks for detailed diagnostics after a positive weight
- Positive weight:
  - `query_limit` widens and is clamped by grouping (`W_eff`)
  - candidates include `publication` while the helper runs
  - displayed candidates include an `authority` block
  - `pagination.next_cursor` is null and `cursor_supported=false`
  - inbound cursor is rejected before query execution (A3)
  - main and zone paths use the same `authority_rerank` helper
- Structured citation results remain unaffected; authority only applies to hybrid candidate responses
  over decisions, and structured responses keep their existing exact-result pagination behavior.

Review gate A4:

- Scope is `SearchExecution` (new/run_hybrid_candidates/apply_search_response_envelope), the
  `zone_search_payload` mirror, the first-page pagination contract, and diagnostics.
- Verify OFF code path and JSON are unchanged, ON never emits a legacy cursor, the window cannot outrun
  the candidate arm pool (`W_eff <= pool_multiplier`), and the zone path is not a second implementation
  of the rerank itself.

---

## A5 - Contract and regression tests

Goal: lock the invariant and the user-visible behavior before any benchmark work.

Tasks:

- Storage tests:
  - `authority_tier` / `authority_rerank` / `effective_authority_weight` unit tests from A1 (inline in
    `authority.rs`)
  - `hybrid_candidates_json` false-projection SQL/payload identity tests
    (`crates/jurisearch-storage/tests/retrieval_smoke.rs` or `decision_projection.rs`)
  - `zone_candidates_json` false-projection SQL/payload identity tests
    (`crates/jurisearch-storage/tests/zone_units.rs`)
  - true-projection tests for main chunk, main document, and zone candidates
- CLI contract tests:
  - search authority cases in `crates/jurisearch-cli/tests/cli_retrieval_contract.rs`:
    - default `search` output for an existing fixture remains unchanged
    - `--authority-weight 0.0` produces the same output as unset for the same query
    - `--authority-weight > 0` on a decision search disables pagination and emits authority metadata
    - positive weight with `--cursor` fails with `bad_input`
    - positive weight with main `--kind code` and `--kind all` fails with `bad_input`
    - zone search with positive weight disables pagination and carries authority metadata
  - session mirror in `crates/jurisearch-cli/tests/cli_session_contract.rs`: the JSONL `search` request
    accepts/validates `authority_weight` identically to one-shot.
- Phase 2 guard tests (`crates/jurisearch-cli/tests/cli_eval_contract.rs`):
  - the Phase 2 artifact validator (`gates/phase2.rs`) still accepts the unchanged
    `phase2_france_juris_benchmark` artifact
  - gate re-derivation ignores any authority benchmark artifact because it is a separate `kind`
  - the Phase 2 gate command stays knob-free; `eval france-juris` (`eval/france_juris.rs`) does not gain
    or read `--authority-weight`

Suggested commands:

```bash
cargo test -p jurisearch-storage authority
cargo test -p jurisearch-storage retrieval
cargo test -p jurisearch-storage zone
cargo test -p jurisearch-cli --test cli_retrieval_contract
cargo test -p jurisearch-cli --test cli_session_contract
cargo test -p jurisearch-cli --test cli_eval_contract
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

- Add a separate `eval france-juris-authority` subcommand. Concretely:
  - add `FranceJurisAuthority(EvalFranceJurisAuthorityArgs)` to `EvalSubcommand`
    (`crates/jurisearch-cli/src/args.rs`), mirroring `FranceJurisZones`.
  - dispatch it in `eval/mod.rs::emit_eval` to a new payload builder, emitting via the existing
    `emit_artifact(response, out_path)`.
  - add `crates/jurisearch-cli/src/eval/authority.rs` (new submodule, registered in `eval/mod.rs`),
    mirroring `eval/zones.rs`. Artifact `kind = "phase2_authority_benchmark"`.
- Reuse the France-juris decision qrel/gold discipline: gold from `france_juris_gold_json` (official
  indexed fields only, no LLM, no human labels, no archive re-parse); score via
  `score_known_item_qrels` and `benchmark_search_request` from `eval/scoring.rs`; `mean`/`floor_metric`
  from `eval/artifact.rs`.
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
- Add the authority sweep to this new benchmark, not to generic `eval tune`:
  `--authority-weights 0.0,0.1,0.25,0.5`.
- If generic `eval tune` is later extended, first add an explicit decision-corpus mode; do not reuse
  its current question/qrel path for authority.
- The mandatory design §7.1 recall regression guard is realized inside this measured-only benchmark,
  not by adding authority knobs to the Phase 2 gate command. Recompute recall@10 with the same gold
  recipe and grouping as `eval france-juris` (`france_juris_retrieval_category`, top-10 document
  grouping), then compare OFF vs ON there.

Acceptance:

- Artifact state is `measured`, never `passed`/`failed` (mirror the `phase2_zone_benchmark` measured-only
  contract in `eval/zones.rs`).
- Artifact is written separately under the requested `--out`; it is not consumed by the Phase 2 gate
  (`gates/phase2.rs` only reads `JURISEARCH_PHASE2_BENCHMARK` / `phase2_france_juris_benchmark`).
- For every tested positive weight, recall@10, computed with the same gold recipe and grouping as the
  Phase 2 gate, does not regress below the OFF measurement or the Phase 2 floor.
- Coverage is reported prominently so a tiny pair set cannot look conclusive.

Review gate A6:

- Scope is the new `eval/authority.rs`, its args/subcommand/dispatch, the artifact schema, and the
  sweep logic.
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
  - `index_manifest` schema-version upsert (bump `migrations::CURRENT_SCHEMA_VERSION`)
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
limited to the `authority.rs` module, the two query projection flags, the `SearchArgs`/`SearchRequest`
field, and the `SearchExecution`/`zone_search_payload` ON-path wiring. Because OFF projection and OFF
pagination are locked by tests, removing the knob should restore the prior behavior without data repair.

---

## Source anchors (verified against the refactored tree, 2026-06-25)

Storage (`crates/jurisearch-storage/src/`):
- `retrieval/types.rs`: `RetrievalOptions` (:62), `HybridCandidateQuery` (:69), `DecisionFilters`,
  `effective_rrf_weights` (:159), `effective_probes` (:169), `GroupBy`, `RetrievalMode`.
- `retrieval/hybrid.rs`: `hybrid_candidates_json` — the `limited`/`scored` candidate projection +
  `jsonb_build_object` (A2 target).
- `retrieval/sql.rs`: `ranked_candidate_ctes`, `document_cursor_predicate`, `format_sql_f64`
  (ranked CTEs — NOT touched by A2).
- `retrieval.rs`: module root re-exporting the above.
- `zone_retrieval.rs`: `ZoneCandidateQuery` (:25), `zone_candidates_json` (:198).
- `migrations.rs`: `CURRENT_SCHEMA_VERSION = 17` (:3).
- `authority.rs`: NEW (A1).

CLI (`crates/jurisearch-cli/src/`):
- `args.rs`: `SearchArgs` (:146), `CliZone`, `EvalSubcommand` (:434), `EvalFranceJurisArgs`,
  `EvalFranceJurisZonesArgs`.
- `request.rs`: `SearchRequest` (:18), `SearchRequest::retrieval_options` (:57),
  `SearchRequest::decision_filters` (:65), `SearchArgs::into_request` (:80).
- `query_support.rs`: `validate_retrieval_options` (:22), `parade_query_text`.
- `retrieval/search.rs`: `search_payload` (:38), `search_with_postgres` (:158), `SearchExecution` (:198),
  `run_hybrid_candidates` (:265), `run_structured_citation_or_fallback` (:301),
  `apply_search_response_envelope` (:354), `search_pagination_value` (:14), `parse_search_cursor` (:463).
- `retrieval/zone.rs`: `zone_search_payload` (:52), `ensure_zone_retrieval_readiness` (:10).
- `session.rs`: `session_search_payload` (:33), `dispatch_session_request` (:121).
- `eval/mod.rs`: `emit_eval` dispatch. `eval/france_juris.rs`: `eval_france_juris_payload`,
  `france_juris_retrieval_category`. `eval/zones.rs`: `eval_france_juris_zones_payload` (measured-only
  template). `eval/scoring.rs`: `score_known_item_qrels`, `benchmark_search_request` (:65).
  `eval/artifact.rs`: `mean`, `floor_metric`. `eval/generic.rs`: `eval_tune_payload` (:555, do NOT extend).
- `gates/phase2.rs`: `phase2_gate_payload` (knob-free gate).

Test homes:
- `crates/jurisearch-storage/tests/{retrieval_smoke,decision_projection,zone_units}.rs`.
- `crates/jurisearch-cli/tests/{cli_retrieval_contract,cli_session_contract,cli_eval_contract}.rs`.
</content>
</invoke>
