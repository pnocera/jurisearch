# France-LEGI Official-Evidence Benchmark — Feasibility & Gate Design

Date: 2026-06-22

## Question

Can we create/source a France-LEGI retrieval benchmark from **official Légifrance
evidence** — to replace the Belgian BSARD proxy as the Phase 1 release gate, and to do it
without waiting on scarce human annotation?

## Verdict

**Yes. Feasible, and partly proven end-to-end today.**

The DILA LEGI archive (and the equivalent Légifrance/PISTE API fields) already carries
machine-readable relevance evidence the legislator authored. Three retrieval categories can be
built from it with **no human annotator and no LLM in the gold**, which is exactly the property
BSARD cannot give us (wrong jurisdiction) and the property the LLM-draft→verify→sign-off
workflow is expensive to give us (human in the loop). One category (cross-reference) was built
and scored end-to-end through bge-m3 today; the production hybrid search path was separately
confirmed working live on the 1.85M-chunk index.

## Why not just keep BSARD

`2026-06-22-bsard-full-benchmark-result.md`: BSARD ran clean but **failed** the gate —
hybrid recall@20 `0.4683` vs floor `0.75`, nDCG `0.3297` vs `0.60`, MRR `0.3389` vs `0.50`.
Two structural problems, independent of tuning:

1. **Jurisdiction**: BSARD is Belgian statutory law. The Phase 1 claim is about French LEGI.
   The adoption note (`2026-06-22-external-expert-benchmark-gate.md`) already flags this as a
   proxy requiring an explicit "Belgian-law to French-LEGI applicability" argument.
2. **Harness**: `bsard_benchmark.py` is standalone Python BM25/RRF — *not* the production Rust
   `pg_search` + pgvector + RRF pipeline. The only production-shared component is the locked
   `bge-m3` fingerprint. So even a passing BSARD score would not exercise the shipped retriever.

A France-LEGI gate fixes both: correct jurisdiction, and it can run through the **real**
`jurisearch search` pipeline (confirmed below).

## The official-evidence insight

LEGI XML (and the Légifrance API) ship structured fields that *are* relevance judgments,
authored by the legislator/DILA, not by us:

| Official field | What it asserts | Benchmark use |
|---|---|---|
| `ID` (`LEGIARTI…`) + `NUM` + `TITRE_TXT` | this article number, in this code/text | known-item identity |
| `CID` | stable cross-version article identity | groups versions of one article |
| `DATE_DEBUT` / `DATE_FIN` | this version is in force on `[début, fin)` | temporal / as-of truth |
| `<LIEN typelien="CITATION" sens="cible" id="LEGIARTI…">` | this article officially cites that one | cross-reference truth |

Because each is a literal field in the source the State publishes under Licence Ouverte, the
qrels are **objective and reproducible** — re-deriving them from a newer archive just refreshes
the labels. No annotation budget, no LLM hallucination surface.

## Three categories (each measures a distinct capability — report separately)

### 1. Cross-reference — **BUILT & SCORED end-to-end today**
- Task: given article A, retrieve the articles A officially cites.
- Gold: A's `LIEN typelien="CITATION" sens="cible"` targets that exist in the corpus.
- Prototype: `external-benchmarks/legi_xref_proto.py`, 6,000 articles scanned from the archive
  front, 80 queries, embedded via OpenRouter `baai/bge-m3`, **dense-only** cosine retrieval.
- Result:

  | Metric | Value |
  |---|---|
  | Recall@10 (cited targets in top-10) | **0.428** |
  | MRR@10 | **0.215** |
  | hit@10 (≥1 cited target found) | **46/80 = 0.57** |

- Read this as a **lower-bound floor from a deliberately degraded harness**: dense-only (no
  BM25, no RRF), a 6k-article subset, context truncated to 6 kB, ad-hoc OpenRouter embeddings —
  none of the production hybrid retriever. The live index already holds **~1.95M LIEN edges**,
  so the same qrels over the production pipeline give a full cross-reference gate directly.
- Caveat: cross-references are navigational, not always topical — this is a citation/related-
  article signal (the `cite`/`related` pillars), not open-ended conceptual search.

### 2. Known-item — **RUN LIVE** through the production pipeline
- Task: query a code+article (or a named legal duty) → the exact article version.
- Gold: `legi:{LEGIARTI}@{DATE_DEBUT}` from `ID` + `NUM` + `TITRE_TXT` + `DATE_DEBUT`.
- Vehicle: this is **already a supported live operation** — `jurisearch eval phase1` runs
  curated official-evidence fixtures (with `as_of`, `allowed_alternates`, official
  `verified_against`) through the real `search_payload` pipeline. The release-gating
  `known_article_statutory` fixture (`R*242-40`, as-of 1990, gold
  `legi:LEGIARTI000006590697@1989-11-04`) **passed** (see live result below).
- Design point already handled by the fixture model: `as_of` is pinned per fixture, so the gold
  version is version-matched to the retrieval date (no "wrong version counted as miss").

### 3. Temporal as-of — **RUN LIVE** through the production pipeline
- Task: same article, different `--as-of` dates → the version in force on that date.
- Gold: `CID`-grouped versions, pick the one whose `[DATE_DEBUT, DATE_FIN)` window contains the
  as-of date. Exercises the production temporal prefilter directly.
- Vehicle: the `temporal_statutory` / `conceptual_statutory` fixtures in `eval phase1`
  (`réserve-naturelle R242-41` as-of 1990; `vét.-déontologie` as-of 2003, gold
  `legi:LEGIARTI000006590698@2003-08-07`) **passed live** — the `--as-of` prefilter selects the
  in-force version against official validity windows.
- To scale beyond the curated set, pull temporal qrels from the built index
  (`documents.version_group` / `valid_from` / `valid_to`), not the archive front: the standalone
  `legi_temporal_proto.py` `.//CID` scan came back empty on the non-codified front (those JORF
  texts are single-version; multi-version families are codified articles deeper in the tar).

## Live production-pipeline result (`eval phase1`, hybrid)

The known-item + temporal categories were run **live through the real `search_payload`
pipeline** over the production index `index/phase1-freemium-20250713` (1.85M chunks), via
`jurisearch eval phase1 --mode hybrid --top-k 20 --include-dev`
(`work/03-implementation/02-evidence/2026-06-22-france-legi-eval-phase1-live-hybrid.json`):

| Fixture | Category | as-of | Result |
|---|---|---|---|
| `legi-release-candidate-code-rural-r242-40-1989` | known_article | 1990-01-01 | **PASS** |
| `legi-release-candidate-reserve-naturelle-r242-41-1990` | temporal | 1990-01-01 | **PASS** |
| `legi-release-candidate-veterinaire-deontologie-2003` | conceptual | 2003-09-01 | **PASS** |
| `legi-release-candidate-loi-1990-long-sejour` | citation_state | 2024-01-01 | **PASS** |
| `legi-hierarchy-same-section-1996` (dev) | hierarchy | 1996-01-01 | **PASS** |
| `legi-hierarchy-temporal-sibling-2000` (dev) | hierarchy | 2000-01-01 | FAIL |

**4/4 release-gating France-LEGI official-evidence fixtures pass; 5/6 overall** (the single
failure is a dev-tier temporal-sibling discrimination case). This is the production
BM25+dense+RRF path with the `--as-of` prefilter against official validity windows — not a proxy
harness. (Earlier smoke also confirmed the path directly: `"responsabilité du fait des produits
défectueux"` → `legi:LEGIARTI000006284600@1998-05-21` with `{dense_rank:6,lexical_rank:2,rrf:…}`.)

### Why curated fixtures, not a 100-query sweep (verified in source)

`search_payload` (the function both `session` and `eval phase1` call) opens its own embedded
Postgres via `open_index` → `ManagedPostgres::start_durable` and **drops it on return**
(`crates/jurisearch-cli/src/main.rs:770`, `:2763`). So PG is **cold-started per query** — start
postmaster + load the ANN index over 1.85M chunks — everywhere. `eval phase1` is fast only
because it runs a handful of fixtures. A naive 100-query ad-hoc sweep through `session` would be
hours of repeated cold starts (this is what timed out earlier). Consequence for the gate: run
**curated official-evidence fixtures** (the codebase's existing grain), or add a persistent-PG
batch-eval path (one PG lifecycle, N queries) before scaling to large automatic qrel sets.
(The orphaned postmaster from the earlier timed-out attempt was shut down cleanly with
`pg_ctl stop -m fast`; the index was left clean for codex.)

## France-LEGI gate design

Reuse the existing Phase 1 gate machinery; do not invent a parallel one.

- **New artifact kind**: `phase1_france_legi_benchmark`, consumed from a new env var
  `JURISEARCH_PHASE1_FRANCE_LEGI_BENCHMARK` (mirror of the BSARD
  `JURISEARCH_PHASE1_EXTERNAL_BENCHMARK` path).
- **New status check**: `france_legi_official_eval`, added to `phase1_gate.checks[]` as a
  quality blocker alongside (or replacing) `external_expert_annotated_eval`.
- **Runner**: executes the **production** `jurisearch search` (session JSONL or a `eval`
  subcommand), not a standalone Python retriever — this is the key upgrade over the BSARD gate.
- **Status re-derives pass from metrics**, exactly as today: the Rust gate recomputes pass from
  the artifact's per-category metrics against policy floors and ignores any self-reported
  `state`; reject artifacts that sampled/truncated the qrels (the BSARD `limit_*` rejection
  rule carries over).
- **Per-category metrics** recorded in the artifact: cross-reference recall@10/MRR@10,
  known-item recall@10/MRR@10 (with as-of policy stated), temporal version-exactness@k.
- **Provenance recorded**: archive filename + date (e.g. `Freemium_legi_global_20250713`) or
  Légifrance API revision, qrel counts per category, embedding fingerprint, retriever = the
  production pipeline (commit).

### Provisional floors (calibrate before enforcing)

The only end-to-end number we have (xref recall@10 `0.428`) is from a degraded dense-only
prototype, so floors must be set from a **first full production-pipeline calibration run**, not
guessed. Starting proposal, to be confirmed/raised after calibration:

| Category | Metric | Provisional floor | Rationale |
|---|---|---|---|
| Known-item | recall@10 (version-matched) | ≥ 0.85 | exact code+article ≈ lexical lookup; BM25 should dominate |
| Temporal | version-exactness@10 under `--as-of` | ≥ 0.90 | as-of prefilter should make this near-deterministic |
| Cross-reference | recall@10 | ≥ 0.60 | hybrid+full-index ≫ the 0.43 dense-only prototype |

Do **not** lower a floor to make an artifact pass (same discipline as the BSARD note).

### BSARD demotion

Keep BSARD as an **optional cross-lingual robustness signal**, not a release gate: it stays
useful for "does the retriever degrade gracefully on French-language Belgian statute," but the
France-LEGI gate is the jurisdiction-correct, production-path release blocker. LLeQA stays a
secondary external candidate under the same artifact discipline.

## Relationship to the LLM-draft→verify→sign-off gold (plan 0.2)

These three categories are **structurally derivable** — gold comes from DILA fields, so they
need none of the LLM-draft / official-source-verify / named-human-sign-off pipeline. That
pipeline is still required for the *natural-language / conceptual* query gold (where there is no
official field to read the answer off). The two are complementary: official-evidence categories
give a cheap, large, objective release gate now; the human-signed conceptual set is layered on
for the semantic-search claim.

## Sources / artifacts

- Cross-reference prototype: `external-benchmarks/legi_xref_proto.py` (ran; numbers above)
- Live known-item/temporal harness: `external-benchmarks/legi_live_proto.py`
- Live-index explorer / production-search smoke: `external-benchmarks/_explore_live.py`
- Temporal CID note (front-archive limitation): `external-benchmarks/legi_temporal_proto.py`
- DILA LEGI archive: `index/phase1-eval-archives/Freemium_legi_global_20250713-140000.tar.gz`
  (Licence Ouverte / Etalab); equivalent fields via the Légifrance PISTE API.
- Existing gate: `2026-06-22-external-expert-benchmark-gate.md`,
  `2026-06-22-bsard-full-benchmark-result.md`.

## Next steps

1. Settle the known-item as-of policy (pin `--as-of DATE_DEBUT` vs target current version).
2. Pull temporal qrels from the built index (`documents.version_group` / `valid_from` /
   `valid_to`), not the archive front.
3. Run all three categories through the **production** pipeline once for calibration; record a
   `phase1_france_legi_benchmark` artifact and set floors from observed numbers.
4. Wire `JURISEARCH_PHASE1_FRANCE_LEGI_BENCHMARK` + `france_legi_official_eval` into the status
   gate; demote BSARD to optional robustness signal.
5. Run the full known-item/temporal live sweep on a quiescent index (when codex is not cycling
   it) with a longer window and line-buffered progress.
