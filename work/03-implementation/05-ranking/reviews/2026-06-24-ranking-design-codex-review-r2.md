# Review: authority-aware ranking design r2

## Findings

### WARN: `--authority-weight 0.0` has contradictory semantics and would still change paging

The r2 design correctly fixes the r1 cursor blocker by making authority re-rank first-page-only when authority is ON: ON returns the re-ranked `top_k` with `pagination.next_cursor = null`, rejects inbound `--cursor` plus authority, and introduces no `auth:` cursor tag. That is consistent with the live keyset implementation, where chunk/document/zone paging is strictly tied to fused-score SQL order (`crates/jurisearch-storage/src/retrieval.rs:536-567`) and the CLI parser only understands the legacy chunk cursor plus `doc:` document cursor (`crates/jurisearch-cli/src/main.rs:11694-11755`).

However, the design still leaves one new edge case inconsistent: explicit zero weight. D8 says `--authority-weight` is validated as `[0.0, 1.0]` (`work/03-implementation/05-ranking/2026-06-24-authority-aware-ranking-design.md:31`, `:306-307`), §3.4 says `authority_weight = 0` is the inertness proof and byte-identical order (`:155-156`), and the helper contract says `weight == 0.0` is a no-op (`:182-186`). But §4.4 defines `rerank_on = options.authority_weight.is_some()` (`:233-238`), and R3 acceptance says any set value widens the window, adds the authority block, returns `next_cursor=null`, and rejects inbound `--cursor` plus authority (`:477-481`).

That means `--authority-weight 0.0` would not re-rank, but it would still take the ON path: widened query limit, first-page-only pagination, authority payload annotation, and cursor rejection. Against the live code, those are not harmless implementation details: today `search_with_postgres` and `zone_search_payload` both use `top_k + 1`, truncate, and derive `next_cursor` from the last displayed legacy cursor (`crates/jurisearch-cli/src/main.rs:3422-3425`, `:3466-3484`, `:3691-3697`, `:3775-3795`). Suppressing that cursor for an explicit no-op weight contradicts the design's own "weight 0 is inert/no-op" wording and can pollute eval/tune baselines if `0.0` is included as the OFF comparison point.

Concrete fix: pick one zero-weight contract and make D8, §4.4, §4.6, §6.1, and R3 acceptance use it consistently. Lowest-risk: normalize `authority_weight <= 0.0` to `None` at validation/options threading and define `rerank_on = effective_authority_weight.is_some_and(|w| w > 0.0)`, so `--authority-weight 0.0` preserves the legacy `top_k+1` limit, legacy cursor, and no authority block. Alternative: reject zero by validating `(0.0, 1.0]`. If explicit zero is intentionally a diagnostics-on/no-ranking mode, the design should stop calling it inert/byte-identical and tests must assert `next_cursor=null` for that explicit mode.

## Prior r1 Items

- r1 BLOCKER (cursor): resolved in the actual v1 design. D6, §2.2, §4.5, §8 R3, §9, and §10 all now specify first-page-only authority ranking, `next_cursor=null` when ON, inbound cursor rejection, no `auth:` tag, and deferred deep authority pagination. The only remaining `auth:` references are negative statements that no such cursor is introduced.
- r1 WARN (env fallback): resolved. D8/§4.6 remove `JURISEARCH_AUTHORITY_WEIGHT` from v1, restate `effective_authority_weight == None => inert`, require env-absent golden tests, and include enabled-by-request diagnostics.
- r1 WARN (kind gating): resolved. D8/§4.6/R3 require effective `kind=decision`, reject `code`/`all`, state zone implies decisions, and keep non-decision/inert paths on the legacy limit/cursor behavior.
- r1 WARN (pairwise-lift pairs): resolved. D7/§7.2 now requires within-order pairs, both candidates in the OFF widened window, both inside the same pre-rerank relevance band, different tiers, `marker_absent` excluded, plus coverage and score-gap reporting; it is framed as measured-only smoke/ordering evidence, not graded gold.
- r1 NIT (cursor tag grouping): moot as designed. There is no authority cursor tag in v1 and the legacy parser remains untouched.

## Checks Performed

- Read the r2 review brief at `/tmp/claude-1000/-home-pierre-Work-jurisearch/721d7412-0c39-4102-9dca-b3e97989f03c/scratchpad/codex-ranking-design-review-r2.md`.
- Compared r2 against the prior r1 review at `work/03-implementation/05-ranking/reviews/2026-06-24-ranking-design-codex-review-r1.md`.
- Reviewed the r2 design artifact `work/03-implementation/05-ranking/2026-06-24-authority-aware-ranking-design.md`.
- Checked the live retrieval/cursor/config surfaces that the design relies on:
  - `crates/jurisearch-cli/src/main.rs:3256-3278`, `3373-3485`, `3659-3810`, `11694-11755`
  - `crates/jurisearch-storage/src/retrieval.rs:61-68`, `536-567`
  - `crates/jurisearch-storage/src/zone_retrieval.rs:198-277`

Most r1 issues are correctly closed. The remaining design cleanup is small but should be fixed before implementation so the OFF/inert contract cannot diverge from the pagination contract.

VERDICT: FIXES_REQUIRED
