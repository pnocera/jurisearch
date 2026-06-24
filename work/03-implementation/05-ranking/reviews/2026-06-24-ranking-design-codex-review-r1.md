# Review: authority-aware ranking design

## Findings

### BLOCKER: `auth:` cursor design will duplicate and skip rows once display order diverges from SQL order

The central cursor recommendation in §4.5 is not correct against the live keyset path. Today the storage predicates page strictly in SQL order: chunk mode resumes with `round(r.fused_score, 8) < score OR (= AND chunk_id > id)` and document/zone mode resumes with `cursor_score < score OR (= AND document_id > id)` (`crates/jurisearch-storage/src/retrieval.rs:536-567`). The design proposes re-ranking a widened window in Rust, emitting `auth:<fused_score>:<id>` for the **last displayed row**, then using the same fused-score predicate plus a "seen-id fence".

That does not define a monotone cursor over the displayed order. Example with `top_k=2`, SQL order `A(1.00), B(0.99), C(0.98), D(0.97)`, and an in-band authority rerank displaying `C, A`: if the cursor is based on last displayed row `A`, the next SQL page starts after `A`, so it can fetch `B, C, D...` and duplicate `C` unless the cursor carries every displayed id. If the cursor instead anchors on the deepest SQL-consumed row, it skips not-yet-displayed rows from the first widened window unless the cursor also carries buffered candidates or re-fetches from an earlier anchor. A single `(score, id)` fence cannot represent the set of displayed-but-SQL-later rows or the set of fetched-but-not-displayed rows.

The proposed "bounded look-back of `band`" is also underspecified in code terms. The existing predicate only fetches rows **after** the cursor; looking back before the cursor requires a new predicate/window query, despite §4.5 saying there is "no new SQL predicate". It also still does not solve arbitrary in-window permutations unless the cursor state tracks enough ids or the server recomputes from a stable page origin.

Concrete fix: choose one implementable pagination contract before R3:

- Lowest-risk v1: allow authority rerank only on the first page and return no `next_cursor` while authority is ON, explicitly labeling deep paging unsupported for the experimental rerank.
- Correct deep-paging option: define an authority cursor that carries a page origin plus a bounded displayed-id set (or a consumed-window anchor plus buffered remainder) and add tests for promoted rows that are later in SQL order than the cursor anchor.
- Alternative: move the authority ordering into SQL so the keyset predicate and `ORDER BY` share the same total order, accepting that this is no longer the low-risk post-SQL design.

Until this is fixed, the design would mislead implementation on the highest-risk part of option (b).

### WARN: the env fallback weakens the "unset means byte-identical" contract unless it is treated as an explicit ON knob

The design says `RetrievalOptions { authority_weight: Option<f64> }` defaults to `None` and that the default path is byte-identical when `authority_weight` is unset, but D8 also specifies `JURISEARCH_AUTHORITY_WEIGHT` as an env fallback following the existing RRF helper pattern. In this codebase, `RetrievalOptions::default()` is used by eval helpers and many internal call sites (`main.rs:2628-2646`, `retrieval.rs:63-68`); an `effective_authority_weight(None)` that reads process env would make those call sites rerank without any request-level field being set.

That may be acceptable only if the environment variable is formally defined as an explicit deployment knob. Otherwise it contradicts the review brief's "default ranking is byte-identical when `authority_weight` is unset" invariant.

Concrete fix: either drop the env fallback in v1, or specify the invariant as `effective_authority_weight == None` and require the byte-identical golden tests to run with `JURISEARCH_AUTHORITY_WEIGHT` absent. Also record in diagnostics whether authority was enabled by request field vs env so eval artifacts cannot accidentally compare an env-mutated "OFF" run.

### WARN: the design should explicitly gate authority to decision retrieval or make non-decision behavior inert

The scope is jurisprudence (`kind='decision'`) across `cass` / `inca` / `capp` / `jade`, but the proposed CLI/session knob is added to the generic `search` surface. The live main search path can run with `kind=all` or `kind=code` (`SearchArgs.kind`, `search_with_postgres` kind filter at `main.rs:3682-3687`). If `--authority-weight` is accepted for statutes, the implementation could still widen `query_limit`, alter cursor shape, and run the ON pagination path even though `authority_tier` returns `None` for articles.

Concrete fix: R3 should either reject `--authority-weight` unless the effective kind is `decision` (and for zone, zone already implies decisions), or explicitly define authority ON for non-decision searches as a no-op that also preserves the old `top_k+1` limit and legacy cursor. Do not let a jurisprudence-only knob change statute/all search paging merely because the option is set.

### WARN: pairwise authority-lift needs a stricter pair-construction rule to avoid measuring lexical co-occurrence instead of comparable relevance

The eval plan is correctly measured-only and keeps pairs within an order, with no human/LLM gold. However, §7.2 currently says to form pairs from decisions that "match" and are both retrieved in the candidate window. That is not enough to prove the two decisions are comparably relevant to the query; for known-item headnote queries, another higher-authority decision can share terms without answering the same issue. The metric would still be useful as a smoke signal, but the design overstates it as an ordering-quality metric unless pair construction is tightened.

Concrete fix: define `match` mechanically and conservatively. For example, only count pairs that are both inside the same pre-rerank relevance band for the same query and both present in the OFF widened window; report coverage and score-gap distribution. Keep it measured-only as specified, and avoid any wording that treats the metric as graded relevance gold.

### NIT: cursor tag shape should preserve grouping if the cursor survives R3

The current parser distinguishes chunk cursors from document cursors with the `doc:` prefix and rejects cross-grouping misuse (`main.rs:11694-11755`). The proposed `auth:<fused_score>:<id>` tag loses that grouping information unless a separate parser enum carries it. Zone always uses document grouping, but main search supports both chunk and document grouping.

Concrete fix: if authority deep paging remains in scope, use explicit grouped variants such as `auth:chunk:<score>:<chunk_id>:...` and `auth:doc:<score>:<document_id>:...`, and add mismatch tests mirroring the current `doc:` behavior.

## Checks Performed

- Read the governing review brief at `/tmp/claude-1000/-home-pierre-Work-jurisearch/721d7412-0c39-4102-9dca-b3e97989f03c/scratchpad/codex-ranking-design-review.md`.
- Reviewed the design artifact `work/03-implementation/05-ranking/2026-06-24-authority-aware-ranking-design.md` and its analysis predecessor.
- Inspected the live search, zone, SQL builder, cursor, config, schema, and eval anchors cited by the design:
  - `crates/jurisearch-cli/src/main.rs:296-439`, `3373-3521`, `3659-3805`, `5242-5274`, `11694-11755`
  - `crates/jurisearch-storage/src/retrieval.rs:60-68`, `281-390`, `536-567`, `636-646`
  - `crates/jurisearch-storage/src/zone_retrieval.rs:24-42`, `198-277`
  - `crates/jurisearch-storage/src/migrations.rs:3`, `27-44`, `582-701`, `773-776`
  - `crates/jurisearch-ingest/src/juri/mod.rs:439-443`, `680-683`
  - `crates/jurisearch-ingest/src/juri/tests.rs:89`, `190-194`

Most of the isolation plan is sound: keeping `query_limit = top_k+1`, legacy candidate JSON, legacy truncate, and legacy fused-score cursors on the OFF path can preserve the default path; the window cap `W_eff = min(W, pool_multiplier)` is conservative against the arm limits; and the per-order authority model is honest about the stored fields. The remaining blocking issue is that the ON-path cursor design is not a valid keyset cursor for a post-SQL reranked window.

VERDICT: FIXES_REQUIRED
