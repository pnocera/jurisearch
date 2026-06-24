# Review: authority-aware ranking design r4

## Findings

No remaining findings.

The r3 warning is resolved. The design now consistently gates authority side effects on the normalized effective weight / `rerank_on`, not raw `--authority-weight` presence:

- §4.2 specifies `project_authority = rerank_on`, with `--authority-weight 0.0` producing `rerank_on = false`, no `publication` projection, and byte-identical OFF payload shape (`work/03-implementation/05-ranking/2026-06-24-authority-aware-ranking-design.md:202-214`).
- D6 and §4.5 reject inbound `--cursor` only for an effectively-ON weight (`rerank_on`, `> 0.0`), while `--authority-weight 0.0` plus `--cursor` pages normally on the legacy path (`:29`, `:279-285`).
- §4.6 gates the decision-only rejection on an effectively-ON weight, with `0.0` explicitly inert and not rejected for non-decision searches (`:307-319`).
- §6.1 locks the invariant that `None` or `<= 0.0` yields `effective_weight = None`, `rerank_on = false`, `project_authority = false`, legacy `top_k+1`, legacy cursor, and no `authority` block (`:368-374`).

The earlier r1/r2 fixes also remain intact and mutually consistent:

- First-page-only pagination is still the v1 contract for authority ON: ON returns `next_cursor = null`, rejects inbound cursor paging, introduces no `auth:` cursor tag, and leaves the legacy parser untouched (`:270-291`, `:481-483`, `:523`).
- The env fallback remains removed for v1; the design keeps authority request-scoped and requires env-absent golden tests, preserving `RetrievalOptions::default()` as inert (`:295-303`, `:321-323`, `:488-490`).
- `--authority-weight 0.0` remains normalized to OFF rather than a degenerate ON path (`:31`, `:237-245`, `:314-319`, `:368-374`, `:488-490`).
- The kind gate remains decision-only and effectively-ON only, so the jurisprudence knob cannot alter `code`/`all` paging when inert (`:307-313`, `:492-493`).
- Pairwise authority-lift remains measured-only, within-order, limited to OFF widened-window pairs in the same pre-rerank relevance band, excludes `marker_absent` rows from pair formation, and reports coverage/score-gap distribution (`:400-431`, `:507-513`).

## Checks Performed

- Read the governing r4 brief at `/tmp/claude-1000/-home-pierre-Work-jurisearch/721d7412-0c39-4102-9dca-b3e97989f03c/scratchpad/codex-ranking-design-review-r4.md`.
- Reviewed the r4 design artifact `work/03-implementation/05-ranking/2026-06-24-authority-aware-ranking-design.md`.
- Compared against r3 and the earlier r1/r2 review findings.
- Checked the relevant live structural surfaces with CodeGraph: `RetrievalOptions`, `SearchArgs`, `validate_retrieval_options`, `search_with_postgres`, `zone_search_payload`, `hybrid_candidates_json`, `zone_candidates_json`, and `parse_search_cursor`.
- Searched the design for remaining raw-presence wording around `authority_weight`, `--authority-weight`, cursor rejection, projection, kind gating, env fallback, and pairwise-lift construction.

VERDICT: GO
