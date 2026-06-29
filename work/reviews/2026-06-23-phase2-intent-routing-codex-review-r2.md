# Code Review: Phase II Resolver Fixes r2

## Findings

- [None] I found no severity-tagged defects in `ec861e7`.

## Review Notes

- The structured resolver now compares `lower(btrim(d.title))` to the exact parsed article title (`article <number>`), which closes the prefix-sibling miss from the prior review. A query for `Article 33` can no longer match documents titled `Article 330` or `Article 33-1` through the article-number predicate.
- The title predicate is consistent with the current LEGI ingest path: `RawArticle::into_document` derives article titles from `META_ARTICLE/NUM` as `Article {num}`, and the France-LEGI known-item and temporal qrels are generated from stored document citations, so the parser's article number matches the stored title form for those citation-derived queries.
- The temporal suffix fix is sound: `legi_citation_routing` strips ` en vigueur au <date>` into `citation_query`, keeps the date as the structured `as_of`, and `search_with_postgres` passes the stripped query to `CitationResolutionQuery.query`. That restores `exact_citation_match` for temporal citation queries while preserving the as-of validity filter.
- Structured pagination is now truthful for the resolver path: it requests exactly `top_k`, structured candidates have no usable cursor, and the response reports `cursor_supported: false`, `possibly_truncated: false`, and `next_cursor: null`. The hybrid path still over-fetches `top_k + 1` and derives a cursor from the displayed tail row.
- The new storage smoke test adds a later-valid prefix sibling (`Article 330`) and asserts it is excluded when resolving `Article 33`; the parser test now asserts the stripped `citation_query`.

## Residual Risk

- I did not rerun `cargo test` in this pass because the review request limited file modifications to the review artifact, and a local test run can update build artifacts. I reviewed the changed code and tests directly against `git show ec861e7` and the current source.

VERDICT: GO
