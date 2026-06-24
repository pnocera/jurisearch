# Review: authority-aware ranking design r3

## Findings

### WARN: zero-weight is normalized in the main config path, but two design clauses still treat the raw flag as authority-ON

The r2 warning is mostly resolved: D8 now says `authority_weight <= 0.0` is normalized to inert, §3.4 says `0.0` never enters the ON path, §4.4 derives `effective_weight = options.authority_weight.filter(|w| *w > 0.0)`, §4.6 repeats that `0.0` is a clean OFF baseline, §6.1 says explicit `--authority-weight 0.0` keeps `top_k+1`, legacy cursor, no `authority` block, and R3 acceptance requires unset or `0.0` to be byte-identical (`work/03-implementation/05-ranking/2026-06-24-authority-aware-ranking-design.md:31`, `:155-158`, `:235-243`, `:309-314`, `:363-369`, `:483-488`).

However, two nearby clauses still key behavior off the raw presence of `--authority-weight`, not the normalized `effective_weight`. §4.2 says to gate `publication` projection by emitting it "only ... when `authority_weight` is set" (`:204-208`). Under the r3 contract, `--authority-weight 0.0` is set but must still be byte-identical and must not project authority-only fields. D6/§4.5 also say an inbound `--cursor` plus `--authority-weight` is rejected (`:29`, `:277-278`), without qualifying that rejection to the effective ON case. That contradicts the same r3 acceptance line that `--authority-weight 0.0` preserves the full legacy `next_cursor` and should therefore allow normal legacy cursor paging.

Concrete fix: make every authority-side-effect gate use the normalized effective weight, not raw option presence. For example: `project_authority = rerank_on`, `reject_cursor_with_authority = args.cursor.is_some() && rerank_on`, and update D6/§4.2/§4.5 wording to say inbound cursor rejection and authority-only projection apply only when `effective_weight.is_some()` / `--authority-weight > 0.0`. Keep the raw `[0.0, 1.0]` validation language, but do not use "authority_weight is set" as a synonym for ON.

## Prior Items

- r2 WARN (`--authority-weight 0.0` as degenerate ON path): mostly resolved in the core config, window, inertness, and R3 acceptance text. The remaining issue is the raw-flag wording above in projection and cursor rejection.
- r1 BLOCKER (post-SQL rerank cursor): still resolved by first-page-only authority ranking, `next_cursor=null` only when authority is ON, inbound cursor rejection for the ON path, and no `auth:` cursor tag.
- r1 WARN (env fallback): still resolved. The design removes `JURISEARCH_AUTHORITY_WEIGHT` from v1 and requires env-absent golden tests.
- r1 WARN (kind gating): still resolved for effective ON: `--authority-weight > 0` with `code`/`all` is rejected, and zone implies decisions.
- r1 WARN (pairwise-lift pairs): still resolved. The metric remains within-order, limited to OFF widened-window candidates in the same pre-rerank relevance band, excludes `marker_absent` rows from pair formation, reports coverage/score-gap distribution, and remains measured-only.

## Checks Performed

- Read the governing r3 brief at `/tmp/claude-1000/-home-pierre-Work-jurisearch/721d7412-0c39-4102-9dca-b3e97989f03c/scratchpad/codex-ranking-design-review-r3.md`.
- Reviewed the r3 design artifact `work/03-implementation/05-ranking/2026-06-24-authority-aware-ranking-design.md`.
- Compared against the prior reviews `work/03-implementation/05-ranking/reviews/2026-06-24-ranking-design-codex-review-r1.md` and `work/03-implementation/05-ranking/reviews/2026-06-24-ranking-design-codex-review-r2.md`.
- Checked the live structural surfaces with CodeGraph: `SearchArgs`, `zone_search_payload`, `zone_candidates_json`, `RetrievalOptions`, `validate_retrieval_options`, `parse_search_cursor`, `hybrid_candidates_json`, and `zone_candidates_json`.

VERDICT: FIXES_REQUIRED
