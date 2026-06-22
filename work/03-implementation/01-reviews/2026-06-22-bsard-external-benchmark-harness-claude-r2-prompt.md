Please re-review the current uncommitted diff in /home/pierre/Work/jurisearch after fixes for:

Prior review:
- `work/03-implementation/01-reviews/2026-06-22-bsard-external-benchmark-harness-claude-review.md`
- Prior verdict: `VERDICT: FIXES_REQUIRED`

Fixes applied:
- Rust status gate now enforces policy floors instead of trusting artifact `state` alone:
  - `thresholds.hybrid_recall_at_20_min >= 0.75`
  - `thresholds.hybrid_ndcg_at_20_min >= 0.60`
  - `thresholds.hybrid_mrr_at_20_min >= 0.50`
- Rust status gate re-derives pass by checking `metrics.hybrid.* >= thresholds.*`.
- Rust status gate validates:
  - `kind == phase1_external_expert_benchmark`
  - `schema_version == 1`
  - `dataset.revision != unknown`
  - full-size BSARD evidence (`corpus_documents >= 22000`, `questions >= 200`)
  - no corpus/question limits
  - locked embedding fingerprint model `bge-m3`, dimension `1024`, normalize `true`
- Python runner now computes true recall@k and also reports `success_at_k`.
- Python runner now hard-fails if it cannot resolve a pinned dataset revision.
- Python runner includes base URL and input truncation policy in the embedding cache key.
- Python runner records `embedding.fingerprint_model`, `request_model`, `dimension`, `normalize`, and `max_input_chars`.
- Python runner retries transient 429/5xx endpoint failures.
- Dense ranking tie-break is deterministic by document id.
- Evidence/README/plan updated to record enforced floors.
- Limited smoke rerun with the fixed runner; status rejects it for limits and zero thresholds.

Validation already run after fixes:
- `python3 -m py_compile external-benchmarks/bsard_benchmark.py`
- `cargo fmt --all`
- `cargo test -p jurisearch-cli external_benchmark`
- `cargo test -p jurisearch-cli --test cli_contract`
- Limited r2 smoke against OpenRouter `baai/bge-m3`:
  - `--limit-corpus 1000 --limit-questions 2`
  - artifact `/tmp/jurisearch-bsard-smoke-r2.json`
  - status rejected it with `external_check=fail` because limits, minimum full-size counts, and zero thresholds fail policy.

Please review only whether the FIXES_REQUIRED findings are resolved and whether the remaining diff is safe to commit.

Output structure:
- Findings first, ordered by severity.
- Open questions/risks.
- Verification notes.
- Final verdict line exactly one of:
  - VERDICT: GO
  - VERDICT: FIXES_REQUIRED
