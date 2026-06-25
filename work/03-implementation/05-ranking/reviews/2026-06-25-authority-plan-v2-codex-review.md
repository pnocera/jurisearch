# Code Review: Authority-Aware Ranking Implementation Plan v2

## Findings

### BLOCKER: A2's "all existing call sites" list misses live query-struct construction sites

Plan location: A2, lines 172-178, says all existing `HybridCandidateQuery` / `ZoneCandidateQuery` construction sites to update are `retrieval/search.rs`, `retrieval/zone.rs`, and storage integration tests.

What is wrong vs. source:

- `crates/jurisearch-cli/src/retrieval/compare.rs:46-63` constructs a `HybridCandidateQuery` for the `compare` command. After adding `project_authority: bool`, this literal must set `project_authority: false`; otherwise the CLI crate will not compile.
- `crates/jurisearch-cli/src/eval/generic.rs:350-367` constructs a `HybridCandidateQuery` in generic `eval run` / `eval tune`. This must also set `project_authority: false` to preserve the existing statute/question-oriented eval payload shape.
- `crates/jurisearch-cli/src/eval/zones.rs:134-150` constructs a `ZoneCandidateQuery` for the measured-only zone benchmark. This must set `project_authority: false` until the authority benchmark explicitly opts into its own projection.

Concrete fix:

Extend A2's construction-site list to include:

- `crates/jurisearch-cli/src/retrieval/compare.rs:48` - `HybridCandidateQuery { ... project_authority: false, ... }`
- `crates/jurisearch-cli/src/eval/generic.rs:352` - `HybridCandidateQuery { ... project_authority: false, ... }`
- `crates/jurisearch-cli/src/eval/zones.rs:136` - `ZoneCandidateQuery { ... project_authority: false, ... }`

The storage test list is otherwise directionally correct: `retrieval_smoke.rs`, `decision_projection.rs`, `legi_canonical_retrieval.rs`, `target_spike_corpus.rs`, and `zone_units.rs` all contain query literals that need the new field.

### BLOCKER: A3 misses another field-by-field `SearchRequest` construction site

Plan location: A3, lines 209-211, calls out `benchmark_search_request` as a field-by-field `SearchRequest` constructor that must add `authority_weight: None`.

What is wrong vs. source:

- `crates/jurisearch-cli/src/eval/scoring.rs:65-92` is correctly identified: `benchmark_search_request` constructs `SearchRequest` field-by-field.
- But `crates/jurisearch-cli/src/eval/generic.rs:725-744` also constructs a `SearchRequest` literal in `eval_phase1_fixture_result`. Adding `SearchRequest.authority_weight` without updating this literal will break compilation. It should stay on the OFF path with `authority_weight: None`.

Concrete fix:

Update A3 to say the new field must be added to both field-by-field constructors:

- `crates/jurisearch-cli/src/eval/scoring.rs:72` - add `authority_weight: None`
- `crates/jurisearch-cli/src/eval/generic.rs:725` - add `authority_weight: None`

## Confirmed Anchors

- `RetrievalOptions` and `HybridCandidateQuery` are in `crates/jurisearch-storage/src/retrieval/types.rs:61-84`; `RetrievalOptions` is `Debug, Clone, Copy, Default`, so `authority_weight: Option<f64>` fits the current construction/defaulting pattern.
- The A2 main-path projection target is `crates/jurisearch-storage/src/retrieval/hybrid.rs`, not `retrieval/sql.rs`: chunk `limited` projects `d.source` at `hybrid.rs:33-40` and builds candidate JSON at `hybrid.rs:52-60`; document `scored` projects `d.source` at `hybrid.rs:74-83` and builds candidate JSON at `hybrid.rs:104-113`.
- `ZoneCandidateQuery` is in `crates/jurisearch-storage/src/zone_retrieval.rs:24-42`, and `zone_candidates_json` has the `scored` CTE and candidate JSON target at `zone_retrieval.rs:225-273`.
- `migrations::CURRENT_SCHEMA_VERSION` is 17 at `crates/jurisearch-storage/src/migrations.rs:3`.
- `SessionSearchArgs` is gone; `session_search_payload` deserializes `SearchRequest` directly at `crates/jurisearch-cli/src/session.rs:33-37`.
- `SearchRequest`, `retrieval_options()`, `decision_filters()`, and `SearchArgs::into_request` are in `crates/jurisearch-cli/src/request.rs:18-101`; `SearchArgs` is in `crates/jurisearch-cli/src/args.rs:145-200`.
- `validate_retrieval_options` is in `crates/jurisearch-cli/src/query_support.rs:22-40`, takes `&RetrievalOptions`, and is called once by `search_payload` at `crates/jurisearch-cli/src/retrieval/search.rs:47`. The plan's split is correct: numeric authority validation can live there, while kind/zone/cursor rejections must live in route-aware payload builders.
- The `search_payload` zone dispatch happens before main-path parsing/routing at `crates/jurisearch-cli/src/retrieval/search.rs:47-53`, so the plan's caution to avoid rejecting `--zone --kind all` in the non-zone path is correctly placed.
- `SearchExecution<'a>` and its methods match the A4 anchors: `search_with_postgres` delegates through `SearchExecution::new` at `search.rs:181-191`; the struct starts at `search.rs:198`; `new` computes `pool_multiplier`, `lexical_limit`, `dense_limit`, and `query_limit` at `search.rs:221-260`; `run_hybrid_candidates` builds `HybridCandidateQuery` at `search.rs:265-291`; `run_structured_citation_or_fallback` is at `search.rs:301-349`; `apply_search_response_envelope` owns truncation, cursor, pagination, routing, and diagnostics at `search.rs:354-420`.
- `zone_search_payload` is still a flat mirror path at `crates/jurisearch-cli/src/retrieval/zone.rs:52-197`; it computes its own limits at `zone.rs:97-100`, builds `ZoneCandidateQuery` at `zone.rs:102-118`, truncates at `zone.rs:141-150`, and calls shared `search_pagination_value` at `zone.rs:153-159`.
- The test homes named in A5 exist: `crates/jurisearch-cli/tests/cli_retrieval_contract.rs`, `cli_session_contract.rs`, `cli_eval_contract.rs`, and storage tests `retrieval_smoke.rs`, `decision_projection.rs`, `zone_units.rs`.
- A6 eval/gate anchors are current: `EvalSubcommand` is in `args.rs:434-464`; `emit_eval` dispatch is in `eval/mod.rs:21-67`; `eval/zones.rs` is measured-only; `score_known_item_qrels` and `benchmark_search_request` are in `eval/scoring.rs:28-92`; `mean` and `floor_metric` are in `eval/artifact.rs:3-18`; `phase2_gate_payload` reads the benchmark through `JURISEARCH_PHASE2_BENCHMARK` at `gates/phase2.rs:5-11` and `gates/phase2.rs:85-97`; `eval_tune_payload` is the question/qrel sweep path at `eval/generic.rs:555-656`.

## Coherence

The A1 -> A6 order is still sound after the refactor, and the OFF-path invariant is preserved by the described design if the omitted construction sites above are added. The main problem is completeness: a builder following the current A2/A3 lists would miss live struct literals in CLI compare/eval code, causing compile failures and leaving OFF-path projection intent under-specified outside `search` and `zone`.

VERDICT: FIXES_REQUIRED
