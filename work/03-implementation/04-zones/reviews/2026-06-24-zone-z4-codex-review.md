# Code Review: Zone Retrieval Z4

Reviewed commit `2ef7b42f4e3846062b70af4eeae5ec8c53661b68` (`Zone retrieval Z4: zone_candidates_json + search --zone`) against `origin/main`.

## Findings

### 1. `search --zone --as-of` silently ignores the requested temporal cutoff

- Location: `crates/jurisearch-cli/src/main.rs:2979-3040`, `crates/jurisearch-storage/src/zone_retrieval.rs:226-253`
- Severity: High

`SearchArgs` still exposes the global `--as-of` option for `search --zone`, but the zone path never reads `args.as_of` and `ZoneCandidateQuery` has no `as_of` field. The main search path applies `as_of` to retrieval (`valid_from <= as_of` and `valid_to > as_of`) before ranking; the zone path only joins `documents` after ranking and applies no temporal predicate. A user can therefore run a zone search with an historical `--as-of` and receive Cassation decisions dated after that cutoff, even though the shared search help says only versions in force on that date match.

Actionable fix: either reject `--as-of` when `--zone` is present with a `bad_input` message that tells users to use `--decided-from/--decided-to`, or thread `as_of` through `ZoneCandidateQuery` and apply the same validity predicate as `hybrid_candidates_json` before candidates are ranked and limited.

### 2. Zone and decision filters are applied after limited candidate pools, causing false negatives

- Location: `crates/jurisearch-storage/src/zone_retrieval.rs:87-107`, `crates/jurisearch-storage/src/zone_retrieval.rs:166-186`, `crates/jurisearch-storage/src/zone_retrieval.rs:237-240`
- Severity: High

The BM25 arm scopes only by `u.zone`, and the dense arm first takes a global `dense_pool` from all `zone_unit_embeddings`, limits it, and only then joins `zone_units` to filter by zone. Court/formation/publication/date filters are applied even later in `scored`. This means high-scoring units from the wrong zone or from decisions that fail the requested decision filters can consume the fixed `lexical_limit`/`dense_pool_limit`, after which valid in-scope units are never considered. The CLI can return `no_results` or a short page even when matching in-scope zone units exist below the pre-filter limit.

This also contradicts the new module contract that candidates are "ranked within `zone`": dense rank is currently assigned in a global embedding pool, then post-filtered.

Actionable fix: push all scope predicates into each candidate arm before ranking and limiting. For lexical, join `documents d` in the lexical CTE and apply `u.zone = ...` plus `DecisionFilters::predicate()` there. For dense, join `zone_units u` and `documents d` inside the distance query before `ORDER BY distance LIMIT ...`, so dense candidates are selected from the requested zone and requested decision-filter scope. Add regression coverage with many out-of-zone or out-of-filter high scorers ahead of one valid in-scope hit.

### 3. Dense readiness can pass for stale zone embeddings that retrieval will not use

- Location: `crates/jurisearch-cli/src/main.rs:2955-2972`, `crates/jurisearch-storage/src/zone_units.rs:576-584`, `crates/jurisearch-storage/src/zone_retrieval.rs:92-95`, `crates/jurisearch-storage/src/zone_retrieval.rs:171-174`
- Severity: Medium

`ensure_zone_retrieval_readiness` checks only that at least one embedding exists and that no zone unit is missing an embedding row. It does not verify that those rows match the current query embedder fingerprint, model, or dimension. The retrieval SQL filters dense rows by the fingerprint returned from `PreparedQueryEmbedder::embed`; if the index was finalized under an older or different `zone_embedding` manifest, readiness still passes, then dense/hybrid zone retrieval sees no matching rows and reports no results.

Actionable fix: compare the current storage fingerprint (and model/dimension if available) with `coverage["embedding_manifest"]` before allowing dense/hybrid zone search, or make `zone_retrieval_coverage_json` report pending/mismatched counts for the expected fingerprint and have the readiness gate fail on mismatches. Add a regression test where `zone_unit_embeddings` are complete for fingerprint A, the query uses fingerprint B, and the command fails with `index_unavailable` instead of returning an empty result set.

## Verification

Static review only. I did not run the test suite because the request prohibited modifying files other than the review artifact, and Cargo test/check would write build outputs.

VERDICT: FIXES_REQUIRED
