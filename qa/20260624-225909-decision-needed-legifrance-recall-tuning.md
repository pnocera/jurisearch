# Q&A — 20260624-225909

## Question

# Decision needed: Legifrance recall tuning scope

Repo: /home/pierre/Work/jurisearch. Context: the legislation-citation enrichment resolves deduped
"article N of code C" citations (parsed from Cassation `visa`) against Legifrance `lf-engine-app/search`.
A handoff told me to "tune the query for higher recall" (current resolution ~25-35%), hypothesizing that
**separate champs** (`NUM_ARTICLE` + `TITLE`) would beat the current single-champ `ALL` query in
`legifrance_code_search_body` (crates/jurisearch-cli/src/main.rs:4562).

I ran a **read-only live experiment** over a fixed random sample of 120 real unique citations from the
clone DB (4 query variants, ~480 live `/search` calls). Metric = **precise match**: a result whose
`sections[].extracts[].num` (or `num`) == the requested article, within top-5 by `PERTINENCE`
(`totalResultNumber` is a weak signal — caps at 200, large for any code-wide match).

Findings:
- **Current query (`champ=ALL`, `TOUS_LES_MOTS_DANS_UN_CHAMP`, valeur="<art> <code>") is the BEST**:
  59.6% precise on clean citations, 25.8% overall.
- Separate champs `NUM_ARTICLE`+`TITLE` are all WORSE: `EXACTE` 38.5% (EXACTE returns 0 on lettered
  articles like "L.121-1" — dot format), `TOUS_LES_MOTS_DANS_UN_CHAMP`/`UN_DES_MOTS` 53.8%. None beat ALL.
- **~57% of sampled citations are garbage from parsing** (`parse_visa_citation` + normalization):
  multi-article concatenations ("S832-1ET1476", "SL.4523-5ETR.4523-3"), `+` suffix / `S` prefix junk
  ("S885+", "SL.165-1+" — the `+` even triggers HTTP 500), prose contamination in the code name
  ("code de procédure civile ensemble l'obligation pour le juge…"), degenerate "du même code"→"code".
  Garbage resolves ~0% under every variant → caps overall recall near the clean fraction (~43%).
- **Residual clean failures (21/52) are almost all articles missing their mandatory L./R./D. prefix**
  (CSS / Code du travail / CPCE / Code de commerce all use L/R/D): "511-8"→R.511-8, "353-1"→L.353-1,
  "2323-12"→L.2323-12. Unresolvable via query alone (we don't know the prefix).

Conclusion: **query tuning is exhausted — the current `ALL` body is already optimal.** The real recall
lever is PARSE quality (split multi-article visas, strip S/+ junk + prose tails, handle prefixes), which
requires re-running `collect-legislation-citations` (re-derives `citation_key`s) — a separate, bigger
change. The legislation enrichment is a **non-blocking cache** (not on the zone-retrieval critical path
of the promotion).

## Proposed plan for THIS promotion

1. **Do NOT change the query body** — keep variant A (already best).
2. Fix the 2 open codex WARNs only:
   (a) `legifrance_search_exchange` fingerprint reads `recherche.champs[*].criteres[*].valeur`
       (or a stable body hash) instead of the now-absent top-level `body["query"]`
       (crates/jurisearch-official-api/src/lib.rs:397); + regression test.
   (b) `cite --online` (`apply_online_citation_confirmation`, main.rs:11683) uses the shared
       real-contract body builder instead of `{query,pageSize}`; + test.
3. **Minimal input hygiene** in the body builder so junk values yield an honest `not_found`/clean call
   instead of HTTP 500 (cleaner archive, fewer wasted retries) — e.g. strip control chars / a trailing
   `+`. (Open question: worth doing, or leave junk to surface as upstream_error?)
4. Run the full pass with `--retry-errors`; accept ~25-35% resolution; record parse-quality as the real
   lever for a future enrichment-quality follow-up (out of promotion scope).

## Questions
1. Do you AGREE we should NOT change the query and proceed with (2)+(3) only for this promotion,
   deferring the parser improvements? Or do you think the parser fix belongs in-scope now?
2. Any objection to the minimal sanitization in (3), or would you prefer junk citations surface as
   `upstream_error` (HTTP 500) untouched so they're visible/auditable?
3. For the fingerprint fix (2a): prefer reading the first criterion `valeur`, or a hash of the whole
   serialized body? The fingerprint is stored per resolution row and used for audit/retry dedup.

## Answer

# Decision: Legifrance Recall Tuning Scope

## Recommendation

Agree with the proposed plan: **do not change the Legifrance query body for this promotion**. Keep the current `fond=CODE_DATE` + `typeChamp=ALL` + `TOUS_LES_MOTS_DANS_UN_CHAMP` request built by `legifrance_code_search_body` in `crates/jurisearch-cli/src/main.rs`.

Your live experiment is enough to close the query-tuning branch for now:

- The current `ALL` body is empirically best on precise top-5 matches.
- `NUM_ARTICLE` + `TITLE` variants underperform, especially on lettered articles.
- Overall recall is capped mainly by parser/normalization garbage, not by Legifrance search syntax.
- Prefix recovery (`511-8` -> `R.511-8`, etc.) is a parser/resolution problem, not a query-body problem.

So for this promotion, implement only:

1. Fix the request fingerprint.
2. Make `cite --online` use the shared real-contract body builder.
3. Add minimal, auditable input hygiene to avoid known-bad upstream 500s.
4. Run the full pass with `--retry-errors`, accept the measured 25-35%, and document parser quality as the next lever.

## Parser Scope

Do **not** pull parser improvements into this promotion.

Reason: parser fixes change `citation_key` derivation and therefore force a `collect-legislation-citations` re-run and alter the deduped resolution corpus. That is a legitimate follow-up, but it is broader than a promotion hardening patch. It also needs its own evaluation sample because the failure modes are diverse:

- multi-article splitting,
- `S` prefix artifacts,
- trailing `+`,
- prose tails in code names,
- `du même code`,
- missing `L.` / `R.` / `D.` prefixes.

Those are real recall improvements, but they are not needed for the zone-retrieval promotion path. The legislation enrichment is a non-blocking cache/evidence layer; do not risk the promotion by changing the citation identity function now.

## Minimal Sanitization

Do add minimal sanitization, but keep it deliberately narrow and visible.

Recommended behavior:

- Strip ASCII/control whitespace.
- Trim leading/trailing whitespace.
- Strip a trailing `+` from the final query string or criterion value.
- Optionally reject an empty/too-short sanitized query as `not_found`/`parse_error` without calling Legifrance.
- Do **not** rewrite article prefixes, split multi-article citations, trim prose tails, or infer `L.`/`R.`/`D.` in this slice.

This is not hiding bad data; it is preventing malformed extracted values from becoming noisy `upstream_error` rows and wasted retries. Keep auditability by storing both:

- the original `canonical_query` in `legislation_citation_resolutions`,
- the actual sanitized request body in `official_api_responses.request_json` / `request_body`.

If a query was sanitized, include a small marker in the archived request JSON or resolution error/provenance, for example:

```json
{
  "jurisearch_input_hygiene": {
    "applied": true,
    "original_query": "SL.165-1+ code ...",
    "sanitized_query": "SL.165-1 code ..."
  }
}
```

Do not let sanitization turn garbage into a false positive. It should only remove characters known to break the API, not broaden matching.

## Fingerprint

Use a **stable hash of the whole serialized request body**, not the first criterion `valeur`.

Reason:

- The request fingerprint is audit/retry dedup metadata, not a display label.
- The full body captures `fond`, field type, operator, page size, sort, and future request-shape changes.
- Reading the first `valeur` would repeat the current bug class: it silently assumes one body shape and one criterion.
- Full-body hashing avoids collisions when two request bodies share a query string but differ in search semantics.

Recommended implementation:

```rust
fn request_body_fingerprint(provider: &str, endpoint: &str, body: &Value) -> String {
    format!("{provider}:{endpoint}:sha256:{}", sha256_hex(&canonical_json_string(body)))
}
```

If the codebase does not already have canonical JSON serialization, `serde_json::to_string(body)` is acceptable for bodies constructed by `json!` with deterministic field insertion in this code path. Better still: write a small recursive canonicalizer if you want this to be robust for arbitrary `Value` objects.

Also keep a human-readable query in the resolution row:

- `canonical_query` remains the operator-facing value.
- `legifrance_request_fingerprint` becomes the full-body fingerprint.

## Required Fixes For This Promotion

1. `legifrance_search_exchange` in `crates/jurisearch-official-api/src/lib.rs`
   - Replace `body.get("query")...` fingerprinting with full-body hash.
   - Update the missing-credential regression test so it asserts the fingerprint is non-empty/stable and no longer expects the query string inside it.

2. `apply_online_citation_confirmation` in `crates/jurisearch-cli/src/main.rs`
   - Replace the stale `{ "query": query, "pageSize": 1 }` body with `legifrance_code_search_body(query)`.
   - Add a test that `cite --online` uses a body without top-level `query` and with `fond=CODE_DATE`.

3. `legifrance_code_search_body`
   - Keep `typeChamp=ALL`.
   - Add the narrow query sanitization before putting the value in `criteres[0].valeur`.
   - Test at least trailing `+` and control whitespace.

4. Full run
   - Run `ingest enrich-legislation-citations --retry-errors`.
   - Accept the measured low resolution rate as honest current coverage.
   - Record in the promotion notes that parse quality, not query tuning, is the next recall lever.

## Go/No-Go

Go after the two WARN fixes and minimal hygiene tests pass.

Do not block promotion on parser recall. A parser rework should be a follow-up with its own before/after sample, because it changes citation identity and requires recollection/re-resolution.
