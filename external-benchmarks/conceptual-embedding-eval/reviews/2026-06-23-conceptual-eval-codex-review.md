# Code Review: Conceptual Embedding Evaluation

## Findings

### BLOCKER: CLI failures are still converted into empty result sets

`run_retrieval.py` inherits the full environment, which fixes the prior stripped-env problem, but it still does not fail the benchmark when the search command fails. At `external-benchmarks/conceptual-embedding-eval/run_retrieval.py:40-49`, the code parses stdout and then reads `d.get("candidates", [])`. The production CLI emits parseable JSON error objects on failure (`crates/jurisearch-cli/src/main.rs:593-597` calls `emit_error` on `search_payload` errors), so failures such as readiness errors, missing embeddings, no candidates, or dependency failures can become `[]` and be counted as "no results". Even the non-JSON path only logs to stderr and returns `[]`.

This directly reopens the class of bug the review instructions call out: a failed search run can still produce a completed `retrieval.json` with empty or partial pools.

Actionable fix:

- In `search()`, treat any non-zero `returncode`, JSON parse failure, CLI error object (`ok == false` or an `error` key), missing `candidates`, or empty candidates as a hard exception.
- Let the exception abort the run with a non-zero exit, including mode, qid/query, command, return code, stderr tail, and stdout tail.
- Optionally distinguish true no-results only if the benchmark intentionally wants to measure no-results; for this benchmark, the CLI source currently turns empty candidates into an error at `crates/jurisearch-cli/src/main.rs:1498-1505`, so the runner should not reinterpret that as success.

### BLOCKER: Document-level metrics are computed over duplicate document IDs from chunk-level results

`run_retrieval.py` stores `top_uids` by mapping every returned candidate chunk to its source article UID at `external-benchmarks/conceptual-embedding-eval/run_retrieval.py:85-89`, but it does not dedupe the ordered list. `score.py` then treats that list as the top-k ranked items at `external-benchmarks/conceptual-embedding-eval/score.py:67-74`. The pool and keymap are document-level (`source_uid`), while the top-k sequence is effectively chunk-level with duplicate source UIDs.

The shipped `retrieval.json` already contains duplicates in the scored top-10 lists: BM25 has duplicate source UIDs for 7/24 questions, dense for 5/24, and hybrid for 5/24. That makes P@10, recall@10, and nDCG@10 inconsistent with the judged unit. A relevant article can receive multiple gains in nDCG and multiple hits in the numerator, while an irrelevant duplicate can consume rank budget differently from a unique document. The `rel-only-*` set counts are document-level, so they are not distorted the same way, which makes the reported metric families internally inconsistent.

Actionable fix:

- Decide the evaluation unit and make every stage use it consistently.
- If this is a document/article benchmark, dedupe each mode's candidates by `source_uid` while preserving rank order before writing `top_uids`, and keep collecting until there are `k` unique documents if possible.
- If this is a chunk benchmark, key the judge pool by stable chunk identity, expose one snippet per chunk, map labels by chunk, and stop deduping by `source_uid`.
- Add a scoring assertion that `len(top_uids) == len(set(top_uids))` for document-level runs, so this cannot silently regress.

### WARN: Candidate order leaks retrieval provenance and invites position bias in the blind judge

`build_judge_input.py` assigns keys in the existing pool order at `external-benchmarks/conceptual-embedding-eval/build_judge_input.py:32-40`. That pool order is created by iterating modes in `["bm25", "dense", "hybrid"]` at `external-benchmarks/conceptual-embedding-eval/run_retrieval.py:22,84-98`, inserting first-seen candidates. As a result, BM25 candidates tend to occupy the earliest opaque keys, followed by dense-only and then hybrid-only additions.

The judge input does not explicitly reveal retriever attribution or seed IDs, so the blinding is mostly good. But the ordering is not neutral: an LLM judge may have position bias, and anyone who knows or guesses the generation procedure can infer that earlier keys are more likely lexical. This is especially important because the benchmark compares retrievers using labels from the pooled list.

Actionable fix:

- Shuffle the per-question candidate list before assigning `c01`, `c02`, etc.
- Use a deterministic seed derived from `question_id` and a recorded benchmark salt so the run is reproducible.
- Store the shuffled keymap as today, and optionally store an audit-only private field with original mode membership outside `judge_input.json`.

### WARN: The retrieval artifact does not persist routing/mode diagnostics needed to audit the core ablation

The CLI source does distinguish the modes correctly. `RetrievalMode::{Bm25,Dense,Hybrid}` is distinct at `crates/jurisearch-storage/src/retrieval.rs:64-87`; BM25 builds only the lexical CTE at `crates/jurisearch-storage/src/retrieval.rs:429-456`; dense builds only the vector CTE at `crates/jurisearch-storage/src/retrieval.rs:457-506`; hybrid builds both and fuses them at `crates/jurisearch-storage/src/retrieval.rs:345-427`. Citation routing is only allowed to replace hybrid mode when `legi_citation_routing()` detects citation intent and structured resolution returns candidates (`crates/jurisearch-cli/src/main.rs:1398-1431`). So, from the source, the CLI path is capable of the intended ablation.

However, `run_retrieval.py` discards the CLI response's `routing` field and does not request or store detailed retrieval diagnostics. After the fact, `retrieval.json` cannot prove that every hybrid query used `chosen_backend == "hybrid"` and `query_type == "semantic"`, nor that each result row came from the requested mode. This is an auditability gap for the central claim of the benchmark.

Actionable fix:

- For every query/mode, persist `retrieval_mode`, `routing`, `pagination`, and ideally detailed `diagnostics.retrieval`.
- Assert the expected mode invariants while running: BM25 uses lexical only, dense uses dense only, hybrid conceptual questions have `routing.query_type == "semantic"` and `routing.chosen_backend == "hybrid"`.
- Fail loudly if any hybrid benchmark question routes to `structured_citation`; either remove/rephrase that question or report it in a separate citation-routed slice.

### WARN: The reported design needs uncertainty and scope limits before supporting broad claims

The benchmark structure is directionally reasonable: 12 structurally sampled seeds, 24 questions, codex/authored slices, real CLI retrieval, blind pooled LLM judgments, and separate seed-recall reporting. The seed-recall metric is correctly separated from the blind judged metrics; because codex questions were written from seed text, seed-recall should be treated as known-item evidence, not as an unbiased relevance metric.

The current scoring output, though, prints only point estimates. With 24 questions over 12 seed topics, conclusions like "dense outperformed BM25 on this sampled conceptual benchmark" or "hybrid underperformed dense in this run" are legitimate after the blockers above are fixed. Claims like "embeddings generally help French legal retrieval" or "hybrid is worse than dense" would overclaim without a larger sample, paired uncertainty, and sensitivity analysis.

Actionable fix:

- Add per-question paired deltas and bootstrap confidence intervals, preferably resampling by seed/topic rather than by individual phrasing.
- Report the `codex` and `authored` slices, but make the primary aggregate seed-clustered so the two phrasings for the same seed are not treated as fully independent.
- State in the generated score output that pooled recall is recall within the union of depth-10 retrievers, not absolute corpus recall.

### NIT: Missing or malformed judgment labels should be validation errors, not implicit irrelevance

`score.py` defaults missing judgments to zero at `external-benchmarks/conceptual-embedding-eval/score.py:58-63`. The current `judge_output.json` covers all 436 `(question,candidate)` pairs with labels in `{0,1,2}`, so this is not affecting the checked-in output. But the scoring script should enforce that completeness instead of silently treating an omitted LLM label as irrelevant.

Actionable fix:

- Before scoring, validate that every `(question_id, candidate_key)` in `judge_input.json`/`judge_keymap.json` has exactly one label in `judge_output.json`.
- Fail on missing, extra, non-integer, or out-of-range labels.
- Keep any "treat missing as 0" behavior behind an explicit `--allow-missing-as-zero` flag if it is ever useful for exploratory work.

## Positive Checks

- The reviewed files are scoped to the requested benchmark and do call the production `jurisearch search` CLI rather than reimplementing retrieval in Python.
- The environment handling in `run_retrieval.py` now preserves the full inherited environment and only overrides `JURISEARCH_PISTE_MAX_RETRIES`, which is the right direction for embedded Postgres startup.
- The CLI source confirms that `--mode bm25`, `--mode dense`, and `--mode hybrid` are not collapsed into one backend.
- The 12 seeds are unique, and `questions.json` has exactly two questions per seed with one `codex` and one `authored` source.
- `score.py` uses the intended graded nDCG gain formula (`2^label - 1`) and excludes no-relevant-in-pool questions from the recall mean by using `None`; the shipped judgments happen to have at least one relevant pooled document for all 24 questions.

## Methodological Bottom Line

After fixing the hard failure handling and the duplicate-document scoring issue, this benchmark can support a directional, sample-bound statement about whether bge-m3 dense retrieval adds value over BM25 on these 24 conceptual French legal questions, and whether this specific hybrid fusion beats either constituent on the same pooled judgments. It cannot by itself support a broad product-level or corpus-level claim without more topics, uncertainty estimates, and a clearer distinction between pooled recall and absolute recall.

VERDICT: FIXES_REQUIRED
