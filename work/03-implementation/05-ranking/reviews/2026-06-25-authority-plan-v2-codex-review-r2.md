# Code Review: Authority-Aware Ranking Implementation Plan v2 Re-Review

## Findings

No remaining blockers found in the r2 scope.

## R1 Blocker Resolution

### R1 BLOCKER: A2 construction-site list was incomplete

Resolved.

The updated plan now names the three CLI `HybridCandidateQuery` construction sites and the two CLI `ZoneCandidateQuery` construction sites under A2:

- `work/03-implementation/05-ranking/2026-06-25-authority-aware-ranking-implementation-plan-v2.md:172` states every `HybridCandidateQuery` / `ZoneCandidateQuery` literal must add `project_authority`.
- `work/03-implementation/05-ranking/2026-06-25-authority-aware-ranking-implementation-plan-v2.md:175` lists the `HybridCandidateQuery` sites.
- `work/03-implementation/05-ranking/2026-06-25-authority-aware-ranking-implementation-plan-v2.md:176` names `crates/jurisearch-cli/src/retrieval/search.rs:277`.
- `work/03-implementation/05-ranking/2026-06-25-authority-aware-ranking-implementation-plan-v2.md:178` names `crates/jurisearch-cli/src/retrieval/compare.rs:48`.
- `work/03-implementation/05-ranking/2026-06-25-authority-aware-ranking-implementation-plan-v2.md:179` names `crates/jurisearch-cli/src/eval/generic.rs:352`.
- `work/03-implementation/05-ranking/2026-06-25-authority-aware-ranking-implementation-plan-v2.md:180` lists the `ZoneCandidateQuery` sites.
- `work/03-implementation/05-ranking/2026-06-25-authority-aware-ranking-implementation-plan-v2.md:181` names `crates/jurisearch-cli/src/retrieval/zone.rs:104`.
- `work/03-implementation/05-ranking/2026-06-25-authority-aware-ranking-implementation-plan-v2.md:182` names `crates/jurisearch-cli/src/eval/zones.rs:136`.
- `work/03-implementation/05-ranking/2026-06-25-authority-aware-ranking-implementation-plan-v2.md:183` names the storage integration-test files.

I grepped the Rust tree for current struct literals. The CLI-crate construction sites are exactly the ones the plan now lists:

- `crates/jurisearch-cli/src/retrieval/search.rs:277` constructs `HybridCandidateQuery`.
- `crates/jurisearch-cli/src/retrieval/compare.rs:48` constructs `HybridCandidateQuery`.
- `crates/jurisearch-cli/src/eval/generic.rs:352` constructs `HybridCandidateQuery`.
- `crates/jurisearch-cli/src/retrieval/zone.rs:104` constructs `ZoneCandidateQuery`.
- `crates/jurisearch-cli/src/eval/zones.rs:136` constructs `ZoneCandidateQuery`.

The remaining literals are storage tests only, and they are covered by the plan's storage-test list:

- `crates/jurisearch-storage/tests/decision_projection.rs:140`, `166`, `245`, `336` construct `HybridCandidateQuery`.
- `crates/jurisearch-storage/tests/retrieval_smoke.rs:204`, `241`, `269`, `295`, `323`, `353`, `381` construct `HybridCandidateQuery`.
- `crates/jurisearch-storage/tests/target_spike_corpus.rs:40` constructs `HybridCandidateQuery`.
- `crates/jurisearch-storage/tests/legi_canonical_retrieval.rs:135`, `172` construct `HybridCandidateQuery`.
- `crates/jurisearch-storage/tests/zone_units.rs:415`, `440`, `453`, `469`, `478` construct `ZoneCandidateQuery`.

No `HybridCandidateQuery` or `ZoneCandidateQuery` construction site remains missing from A2.

### R1 BLOCKER: A3 missed a field-by-field `SearchRequest` literal

Resolved.

The updated plan now names both field-by-field `SearchRequest` literals that need `authority_weight: None` beyond `SearchArgs::into_request`:

- `work/03-implementation/05-ranking/2026-06-25-authority-aware-ranking-implementation-plan-v2.md:215` requires `authority_weight: None` on every field-by-field `SearchRequest` literal.
- `work/03-implementation/05-ranking/2026-06-25-authority-aware-ranking-implementation-plan-v2.md:217` names `crates/jurisearch-cli/src/eval/scoring.rs:72` / `benchmark_search_request`.
- `work/03-implementation/05-ranking/2026-06-25-authority-aware-ranking-implementation-plan-v2.md:219` names `crates/jurisearch-cli/src/eval/generic.rs:725` / `eval_phase1_fixture_result`.

I grepped the Rust tree for `SearchRequest {`. Current constructors are:

- `crates/jurisearch-cli/src/request.rs:81` in `SearchArgs::into_request`, which A3 separately threads at plan lines 211-212.
- `crates/jurisearch-cli/src/eval/scoring.rs:72` in `benchmark_search_request`, now named by A3.
- `crates/jurisearch-cli/src/eval/generic.rs:725` in `eval_phase1_fixture_result`, now named by A3.

No additional field-by-field `SearchRequest` literal remains missing from A3.

## Regression Check Against R1 Confirmed Anchors

No regression found in the r1 confirmed anchors I spot-checked against current source:

- `RetrievalOptions` and `HybridCandidateQuery` remain at `crates/jurisearch-storage/src/retrieval/types.rs:61` and `crates/jurisearch-storage/src/retrieval/types.rs:69`.
- The A2 main-path projection targets remain in `crates/jurisearch-storage/src/retrieval/hybrid.rs`: chunk projection at `hybrid.rs:33`, chunk JSON at `hybrid.rs:52`, document projection at `hybrid.rs:75`, document JSON at `hybrid.rs:104`.
- `ZoneCandidateQuery` remains at `crates/jurisearch-storage/src/zone_retrieval.rs:25`, with zone candidate projection/JSON targets at `zone_retrieval.rs:229` and `zone_retrieval.rs:260`.
- `migrations::CURRENT_SCHEMA_VERSION` remains `17` at `crates/jurisearch-storage/src/migrations.rs:3`.
- `session_search_payload` still deserializes `SearchRequest` directly at `crates/jurisearch-cli/src/session.rs:33`.
- `SearchRequest`, `retrieval_options`, `SearchArgs::into_request`, and `SearchArgs` remain at `crates/jurisearch-cli/src/request.rs:18`, `request.rs:57`, `request.rs:80`, and `args.rs:146`.
- `validate_retrieval_options` still takes `&RetrievalOptions` at `crates/jurisearch-cli/src/query_support.rs:22`, and `search_payload` still calls it before zone dispatch at `crates/jurisearch-cli/src/retrieval/search.rs:47`.
- `search_payload` still dispatches to `zone_search_payload` before non-zone parsing/routing at `crates/jurisearch-cli/src/retrieval/search.rs:51`.
- `SearchExecution` anchors still match the A4 plan: struct at `search.rs:198`, `new` at `search.rs:222`, limit computation at `search.rs:240`, `run_hybrid_candidates` at `search.rs:265`, `run_structured_citation_or_fallback` at `search.rs:301`, and `apply_search_response_envelope` at `search.rs:354`.
- `zone_search_payload` still mirrors the flat zone path at `crates/jurisearch-cli/src/retrieval/zone.rs:52`, with limit computation at `zone.rs:97`, `ZoneCandidateQuery` construction at `zone.rs:104`, truncation at `zone.rs:143`, and shared pagination at `zone.rs:153`.
- The named test homes exist: `crates/jurisearch-cli/tests/cli_retrieval_contract.rs`, `crates/jurisearch-cli/tests/cli_session_contract.rs`, `crates/jurisearch-cli/tests/cli_eval_contract.rs`, `crates/jurisearch-storage/tests/retrieval_smoke.rs`, `crates/jurisearch-storage/tests/decision_projection.rs`, and `crates/jurisearch-storage/tests/zone_units.rs`.
- A6 eval/gate anchors remain current: `EvalSubcommand` at `args.rs:435`, `emit_eval` dispatch at `eval/mod.rs:21`, `score_known_item_qrels` at `eval/scoring.rs:28`, `benchmark_search_request` at `eval/scoring.rs:65`, `mean` / `floor_metric` at `eval/artifact.rs:3`, and Phase 2 benchmark ingestion at `gates/phase2.rs:85`.

One non-blocking cleanup: A3's review-gate scope at plan lines 251-255 still names `benchmark_search_request` but not `eval_phase1_fixture_result`. The task list itself is now complete, so this is not a remaining implementation-plan gap, but adding the second constructor to the review-gate scope would make the gate text match the corrected A3 task list.

VERDICT: GO
