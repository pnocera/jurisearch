# Legifrance recall experiment — findings (2026-06-24)

Read-only live experiment to decide the "tune the query for higher recall" step (handoff §0.1).
Method: fixed pseudo-random sample of 250 real unique citations (`md5(citation_key)` order) from the
clone's `legislation_citation_resolutions`; 120 used for the A/B run (~480 live `/search` calls).
Metric = **precise match**: a result whose `sections[].extracts[].num` (or `num`) equals the requested
article, within the top-5 by `PERTINENCE`. (`totalResultNumber` alone is a weak signal — it caps at 200
and is large for any code-wide match, so we score the exact article extract instead.)

## Result — the CURRENT query is already the best

Clean-citation precision (52 clean of 120) / overall precision (incl. garbage):

| variant | typeRecherche | clean prec | overall prec | note |
|---|---|---|---|---|
| **A_ALL (current)** | champ=ALL, TOUS_LES_MOTS_DANS_UN_CHAMP, valeur="\<art\> \<code\>" | **59.6%** | **25.8%** | best |
| B NUM_ARTICLE+TITLE | EXACTE | 38.5% | 16.7% | EXACTE returns 0 on lettered articles ("L.121-1", dot) |
| B NUM_ARTICLE+TITLE | TOUS_LES_MOTS_DANS_UN_CHAMP | 53.8% | 23.3% | still below ALL |
| B NUM_ARTICLE+TITLE | UN_DES_MOTS | 53.8% | 23.3% | still below ALL |

The handoff hypothesis (separate champs → higher recall) is **empirically false**. Query tuning is
exhausted; ALL + TOUS_LES_MOTS_DANS_UN_CHAMP wins.

## The real ceiling is PARSE quality, not the query

- **~57% of sampled citations are garbage** from `parse_visa_citation` / normalization:
  - multi-article concatenations: `S832-1ET1476`, `SL.4523-5ETR.4523-3`, `S15,16,135ET783`
  - `+` suffix / `S` prefix junk: `S885+`, `SL.165-1+` (the `+` even triggers HTTP 500)
  - prose contamination in the code name: `code de procédure civile ensemble l'obligation pour le juge…`,
    `code du travail, dans sa rédaction issue de la loi…`
  - degenerate `du même code` → code_name="code"
  - Garbage resolves ~0% under every variant → caps overall recall near the clean fraction (~43%).
- **Residual clean failures (21/52) are almost all articles missing their mandatory L./R./D. prefix**
  (CSS, Code du travail, CPCE, Code de commerce all use L/R/D): `511-8`→R.511-8, `353-1`→L.353-1,
  `2323-12`→L.2323-12, `645-11`→L./R.645-11. Unresolvable via query alone (we don't know the prefix).

## Decision taken (for THIS promotion; legislation enrichment is a non-blocking cache)

1. **Do NOT change the query body** — variant A is already optimal.
2. Fix the 2 open WARNs only: fingerprint from `recherche.champs[*].criteres[*].valeur` (not the absent
   top-level `query`); `cite --online` shares the real-contract body builder.
3. Minimal input hygiene in the body builder so junk values yield an honest `not_found`/clean call
   instead of HTTP 500 (cleaner archive, fewer wasted retries).
4. Run the full pass with `--retry-errors`; expect ~25–35% resolution. Parse-quality improvement
   (split multi-article visas, strip S/+ junk + prose tails) is the real recall lever and is deferred to
   a separate enrichment-quality follow-up — it requires re-running `collect-legislation-citations`
   (re-derives citation_keys) and is off the promotion critical path.

(Scratchpad harness: `lf_experiment.py`, `lf_clean_failures.py`. Confirmed with codex before coding.)
