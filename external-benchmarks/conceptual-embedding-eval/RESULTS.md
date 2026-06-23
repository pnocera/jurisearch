# Conceptual-query embedding ablation — do bge-m3 embeddings add retrieval value?

**Status:** codex-reviewed (`reviews/2026-06-23-conceptual-eval-codex-review.md`, verdict
FIXES_REQUIRED). All findings applied — see "Review fixes applied" below. Numbers are the
2026-06-23 doc-level run.

## Question

The Phase-1 release gate demoted semantic retrieval to *advisory*, and a working hypothesis was
"the vector embeddings bring more noise than useful things." That came from **citation-shaped /
known-item** tasks, where an exact-title structured resolver is the right tool. This benchmark tests
the *opposite* regime: **genuinely conceptual** lay-language questions, where there is no citation to
match — exactly where embeddings should earn their keep.

## Method (codex = JUDGE only, not generator)

- **Seeds:** 12 substantive LEGI articles sampled *structurally* (no LLM) — `seeds.json`.
- **Queries:** 24 lay-French questions, two phrasings per seed, tagged by `source`: 12 written by
  **codex**, 12 **authored** by the engineer (`questions.json`). Slicing by source guards against
  "the LLM phrased the query to suit one retriever."
- **Retrieval:** the **real CLI** `jurisearch search --mode {bm25,dense,hybrid} --kind code
  --top-k 10 --as-of 2026-06-23` per question (no Python reimplementation). Unit = **document**:
  the CLI returns chunk-level hits, so we fetch 40 and dedupe by article UID to the first 10 unique
  docs per mode. Every search asserts it used the requested retriever and that the query stayed
  `routing.query_type == "semantic"` (not diverted by citation routing). Any CLI failure aborts the
  run (empty candidates is a CLI error, never silently a zero pool).
- **Judging:** **codex as a BLIND LLM judge** — only title+snippet, opaque per-question keys,
  candidate order deterministically **shuffled** (salt-seeded) so key position leaks no provenance;
  seed and retriever are hidden (pool deduped across modes). Labels 0/1/2 → `judge_output.json`.
  450 labels, all valid and complete (0:149 / 1:180 / 2:121).
- **Scoring (`score.py`):** P@10, **pooled** recall@10 (within the depth-10 union of retrievers —
  NOT absolute corpus recall), nDCG@10 (gain 2^label−1). **Primary aggregate is seed-clustered**
  (average the 2 phrasings within a seed, then across 12 seeds), and 95% CIs for between-retriever
  deltas are **bootstrapped by resampling seeds** (B=5000), since the two phrasings of a seed are
  not independent.

## Results (2026-06-23, doc-level)

### Seed-clustered primary aggregate (12 seeds)
| mode | P@10 | recall@10 (pooled) | nDCG@10 |
|---|---|---|---|
| bm25  | 0.558 | 0.446 | 0.490 |
| **dense** | **0.838** | **0.691** | **0.825** |
| hybrid| 0.617 | 0.494 | 0.590 |

Ranking is identical on every slice (all / authored / codex) and at the strict "directly answers"
threshold (rel=2: bm25 nDCG 0.490, dense 0.825, hybrid 0.590).

### Between-retriever deltas — 95% CI, seed-resampled (rel≥1; `*` = CI excludes 0)
| delta | nDCG@10 | recall@10 | P@10 |
|---|---|---|---|
| dense − bm25   | +0.336 [+0.224, +0.445] * | +0.245 [+0.100, +0.397] * | +0.279 [+0.121, +0.437] * |
| dense − hybrid | +0.235 [+0.111, +0.356] * | +0.198 [+0.064, +0.349] * | +0.221 [+0.083, +0.375] * |
| hybrid − bm25  | +0.101 [+0.059, +0.145] * | +0.047 [+0.020, +0.076] * | +0.058 [+0.025, +0.096] * |

**Every** delta is significant. Relevant docs pooled over questions (rel≥1):
**dense-only = 147, bm25-only = 80, both = 54** — dense surfaces ~1.8× as many uniquely-relevant
documents as BM25 does.

## Findings

1. **Embeddings add substantial, significant value on conceptual queries.** Dense beats BM25 on
   every metric, every slice, both thresholds, with CIs well clear of zero (nDCG +0.336). The
   "embeddings are mostly noise" reading is specific to **citation/known-item** tasks and does
   **not** generalise to conceptual retrieval.

2. **Hybrid significantly UNDER-performs dense alone** (dense − hybrid nDCG +0.235, CI excludes 0).
   The RRF fusion down-weights dense (`JURISEARCH_RRF_DENSE_WEIGHT` default 0.3 vs lexical 1.0), so
   fusion drags the *best* retriever back toward BM25 exactly when dense should dominate.
   **Actionable:** raise the dense RRF weight, or make fusion weights intent-aware (lexical-heavy for
   citation-shaped queries, dense-heavy for conceptual). This benchmark is the fixture to re-tune
   against — re-run `run_retrieval.py` + `score.py` while sweeping `JURISEARCH_RRF_DENSE_WEIGHT`.

## Scope / caveats (what this does and does NOT support)

- Supports a **directional, sample-bound** claim: on these 12 seed topics / 24 conceptual questions,
  dense retrieval significantly beats BM25, and this specific hybrid fusion significantly beats BM25
  but loses to dense.
- Does **not** support a corpus-level/product-level claim "embeddings help French legal retrieval"
  generally — that needs more topics and broader sampling. N = 12 seeds.
- **Pooled** recall (depth-10 union), so absolute recall is unknown; conclusions are *relative*.
- **Single LLM judge** (codex), blind but not multiply-adjudicated.
- Seed-recall (objective) is reported by `run_retrieval.py` but treated as **known-item** evidence
  only, since codex questions were written from seed text.

## Review fixes applied (from codex r1, FIXES_REQUIRED)

- **BLOCKER** — CLI failures no longer become empty pools: any non-zero exit / error envelope /
  missing-or-empty candidates aborts the run with diagnostics.
- **BLOCKER** — doc-level evaluation unit: fetch 40 chunks, dedupe to 10 unique articles per mode;
  `top_uids` asserted duplicate-free (was 17/72 mode-lists with dups, now 0).
- **WARN** — provenance-neutral blind judge: candidates shuffled per question (salt-seeded).
- **WARN** — routing/mode invariants asserted per query (correct backend, `semantic`, no citation
  diversion).
- **WARN** — uncertainty: seed-clustered primary aggregate + seed-resampled bootstrap CIs; pooled-vs-
  absolute recall stated in output and here.
- **NIT** — `score.py` validates judgment completeness (fails on missing/extra/out-of-range labels
  unless `--allow-missing-as-zero`).
