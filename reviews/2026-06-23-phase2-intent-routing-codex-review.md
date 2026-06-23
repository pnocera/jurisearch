# Code Review: Phase II Intent Routing

## Findings

### [High] Prefix-only article matching can return the wrong structured article before the gold

`resolve_legi_citation_json` builds the resolver predicate as a raw contains match:

- `crates/jurisearch-storage/src/retrieval.rs:216` builds `%article {article_number}%`
- `crates/jurisearch-storage/src/retrieval.rs:250` applies that pattern to `lower(concat_ws(' ', d.citation, d.title))`

That means a query parsed as `Article 33` also matches `Article 330`, `Article 331`, `Article 33-1`, etc. The new smoke test only proves that `Article 34` is excluded; it does not cover prefix siblings.

This is not just extra noise. For temporal queries, `search_with_postgres` passes the original query including the `en vigueur au <date>` suffix as `CitationResolutionQuery.query` (`crates/jurisearch-cli/src/main.rs:1346`), so `exact_citation_match` in the resolver is false for the intended article (`crates/jurisearch-storage/src/retrieval.rs:245`). Ranking then falls back to `valid_from DESC, document_id` (`crates/jurisearch-storage/src/retrieval.rs:255`), so newer prefix siblings can be ordered ahead of the actual article. Because the resolver limits to `top_k + 1` before the CLI truncates, enough prefix siblings can push the intended document out of the returned set.

This undermines the purpose of structured citation resolution: a citation-shaped query can resolve structurally but still miss because the SQL treats the requested article number as a prefix. The fix should make the article-number predicate boundary-aware, or better, compare against a canonical article-number field if one exists. Add a regression test with `Article 33`, `Article 330`, and a temporal suffix where `Article 330` has a later `valid_from`.

### [Medium] Structured pagination reports no truncation after truncating `top_k + 1` candidates

Structured candidates intentionally have `"cursor": NULL` (`crates/jurisearch-storage/src/retrieval.rs:296`), but `search_with_postgres` still uses the same `top_k + 1` truncation logic for structured and hybrid responses (`crates/jurisearch-cli/src/main.rs:1372` through `crates/jurisearch-cli/src/main.rs:1382`). If structured resolution returns more than `top_k`, the code truncates the candidate array but cannot derive `next_cursor`, leaving `possibly_truncated` false and `cursor_supported` true (`crates/jurisearch-cli/src/main.rs:1384` through `crates/jurisearch-cli/src/main.rs:1391`).

This will not create an infinite paging loop, but the response becomes misleading: rows were dropped, yet the pagination object says there is no truncation and cursor paging is supported. Structured responses should either request exactly `top_k`, set `cursor_supported` false for `structured_citation`, or explicitly report truncation without a cursor.

## Verified Behavior

- Production visibility is correct: CodeGraph reports only two callers of `search_with_postgres`, `search_payload` and `france_legi_search_documents`, so the CLI and France-LEGI runner share the same routing path. I did not find an eval-only bypass.
- Routing is input-shape-driven. `search_with_postgres` derives `citation_intent` solely from `args.query` and `as_of` (`crates/jurisearch-cli/src/main.rs:1335`) and does not read gold labels or expected answers.
- The parser strips ` en vigueur au <date>` when `date` has the ISO `YYYY-MM-DD` shape, falls back to the caller default for non-ISO suffixes, and uses the text after the last case-insensitive `article ` as the article number.
- The ASCII case-insensitive search helpers return byte offsets, but the returned offsets are safe string slice boundaries for the current ASCII needles: any successful match starts with an ASCII byte and covers only ASCII bytes, so the start and end offsets are UTF-8 character boundaries.
- The resolver's validity filter matches the hybrid temporal prefilter shape: `valid_from <= as_of` and `valid_to > as_of`, with nulls accepted.
- `like_contains` correctly escapes `\`, `%`, and `_` before the SQL uses `ESCAPE '\'`.
- Conceptual queries and explicit `bm25` / `dense` modes still call the hybrid path. The lazy embedding closure computes embeddings only when `retrieval_mode.uses_dense()` and no detailed diagnostics block references the closure-local `embedding_fingerprint`.
- A structured miss falls back to hybrid with `fallback_path = "hybrid_fallback"`, and the closure captures are immutable, so the two possible call sites are sound.

VERDICT: FIXES_REQUIRED
