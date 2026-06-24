# Code Review: authority-aware ranking analysis

## Findings

### BLOCKER: Option (b) overstates the candidate pool available to a Rust post-SQL rerank

The analysis says the post-retrieval rerank can run in `search_with_postgres` before the `truncate(top_k)` and then describes the visible pool as the existing `pool_multiplier` overfetch (`work/03-implementation/05-ranking/2026-06-24-authority-aware-ranking-analysis.md:158-165`). That is not what the current code exposes to Rust.

`search_with_postgres` does set `lexical_limit` / `dense_limit` to `top_k * 4` or `top_k * 20`, but it separately passes `query_limit = top_k + 1` into `hybrid_candidates_json` (`crates/jurisearch-cli/src/main.rs:3691-3697`, `:3711-3727`). Storage then applies that `limit` in the final SQL page (`crates/jurisearch-storage/src/retrieval.rs:366-371`, and chunk mode at `:317-319`). Only after that does Rust truncate from at most `top_k + 1` to `top_k` (`crates/jurisearch-cli/src/main.rs:3777-3784`). The zone path has the same shape: `query_limit = top_k + 1` (`main.rs:3422-3441`) and only then truncates (`main.rs:3466-3474`).

This is load-bearing because a design could choose option (b) assuming it can rerank the already-overfetched `4x/20x` candidate pool without changing SQL. In reality, as written, a Rust-only rerank sees only the displayed page plus the pagination sentinel. It cannot promote an authoritative result from rank 15 into the top 10 unless the SQL output limit is explicitly increased or the rerank is moved before the final `LIMIT`.

Concrete fix: rewrite option (b) and its caveat to distinguish three pools: lexical/dense arm limits, final SQL output limit, and returned `top_k`. State that a useful Rust rerank requires increasing the SQL output limit or adding a separate rerank window, and then reworking cursor semantics for that wider window. Do the same for the zone path.

### WARN: The eval section incorrectly says authority reranking cannot improve recall@10

Section 4.3 says authority reranking "cannot improve this benchmark" and can only hurt it (`work/03-implementation/05-ranking/2026-06-24-authority-aware-ranking-analysis.md:138-141`). That is too strong. With one gold document per query, reranking cannot prove authority quality, but it can still improve recall@10 if a gold document currently sits just below rank 10 and an authority-aware rerank moves it into the first 10. It can also regress recall@10 by moving a current hit out of the first 10.

Concrete fix: replace the "cannot improve" claim with: "The current gate only observes whether the single known-item gold crosses the top-10 boundary. It can catch regressions and may show incidental recall gains, but it is not a metric for 'most authoritative relevant result first'." Keep the conclusion that a new ordering-quality metric is needed.

## Notes

The other load-bearing codebase claims I spot-checked matched the current source: RRF scoring and weights, score/cursor keying, `DecisionFilters.publication`, source/publication storage, lack of a publication index, v16 `official_api_responses` versus v17 legislation citations, zone retrieval's shared filters/RRF shape, and the Phase 2 / zone benchmark separation.

I did not find a blocker in the legal model. The analysis correctly treats authority as secondary to relevance, keeps judicial and administrative publication vocabularies separate, and avoids treating PBRI/Lebon markers as commensurable without a design-time mapping.

VERDICT: FIXES_REQUIRED
