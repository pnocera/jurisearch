# Corpus-wide authority-aware ranking for French jurisprudence search — ANALYSIS

- **Date:** 2026-06-24
- **Status:** ANALYSIS (option space + trade-offs only — no design chosen, no implementation)
- **Scope:** jurisprudence (court decisions, `kind='decision'`) across `cass` / `inca` / `capp` / `jade`, in BOTH the main chunk-based retrieval path and the parallel zone overlay
- **Author note:** every codebase claim below is cited as `file:line`. Where a fact could not be verified in code it is flagged as an assumption.

---

## 1. Problem statement & motivation

jurisearch ranks jurisprudence purely by **relevance**: a reciprocal-rank-fusion (RRF) of a BM25 (lexical) arm and a dense (vector) arm. The fused score is a function of the two ranks and two static weights only:

```
fused_score = lex_w / (RRF_K + lexical_rank) + dense_w / (RRF_K + dense_rank)
```
(`crates/jurisearch-storage/src/retrieval.rs:636-646`, `RRF_K = 60.0` at `:13`, default weights `1.0 / 0.3` at `:14`/`:19`).

Nothing in this score reflects the **precedential authority** of the decision. Yet for case-law a published Cour de cassation decision (`cass`, esp. one flagged for the Bulletin / Rapport annuel) is, for the same on-topic relevance, a far better answer than an *inédit* (`inca`) or a Cour d'appel (`capp`) decision — it binds lower courts more strongly and is the canonical statement of the rule. Authority is therefore a genuine relevance signal that the ranker discards.

The product question this analysis frames (NOT answers): **should ranking incorporate authority, and through which mechanism, without altering the proven default retrieval path that is eval-gated?**

Two conclusions are taken as given (and verified against code below):

1. **Authority is corpus-wide.** It is a property of a decision (`source`, publication markers, court level), not of the zone overlay. Any authority-aware ranking must apply consistently to BOTH the main path (`hybrid_candidates_json`) and the zone path (`zone_candidates_json`), or be deliberately scoped to neither-only-one.
2. **Filtering by publication already exists corpus-wide; ranking-boost by authority exists nowhere.** `DecisionFilters.publication` is a hard predicate reused by both paths (`retrieval.rs:137-142`, used by zone at `zone_retrieval.rs:208-212`). There is no authority term anywhere in either scoring expression.

---

## 2. Legal authority model (French jurisprudence)

This section is domain background; the markers it names are matched to what the corpus actually stores in §4.

### 2.1 Judicial order (ordre judiciaire) — Cour de cassation and below

French jurisprudence has no formal *stare decisis*, but a de-facto authority hierarchy exists and is reflected in how decisions are published:

1. **Cour de cassation, published ("publié au Bulletin")** — the apex civil/criminal court. A published arrêt states the rule the Court wants known and followed. Within "published", a **PBRI** ladder of diffusion markers signals increasing authority:
   - **P** — *Publié au Bulletin* (the chambre's Bulletin).
   - **B** — Bulletin (in the modern Judilibre vocabulary the published flag is conveyed as letters such as `"b"`).
   - **R** — selected for the **Rapport annuel** (the Court's annual report) — the most authoritative tier; the Court explicitly highlights the decision.
   - **I** — diffused on the website / *Internet*.
   - A decision can carry several at once (Judilibre returns e.g. `publication: ["b","r"]`).
2. **Cour de cassation, inédit (`inca`, unpublished)** — a real Cassation decision NOT selected for the Bulletin. Still apex-court, still binding inter partes, but the Court signalled it is not a reference statement of the rule. Authority below "published".
3. **Cours d'appel (`capp`)** — second-degree courts. Persuasive, not binding on other courts; authority below any Cassation decision. (Cour d'appel decisions are identified by **RG numbers**, not pourvoi numbers — relevant because Judilibre cannot resolve them; see §4.)
4. **First-instance / lower courts** — not a separate DILA bulk source here; out of scope.

### 2.2 Administrative order (ordre administratif) — Conseil d'État analog (`jade`)

A parallel hierarchy with its own diffusion vocabulary, **Lebon** (the *Recueil Lebon*) rather than the Bulletin:

1. **Conseil d'État, published in the Recueil Lebon ("classé A")** — apex administrative court, highest authority.
2. **Conseil d'État, "mentionné aux tables" ("classé B")** — cited in the Lebon tables; intermediate.
3. **Cours administratives d'appel (CAA) / tribunaux administratifs** — lower; the sample `jade` record in the corpus is in fact a `CAA de PARIS` decision (`crates/jurisearch-ingest/src/juri/tests.rs:190`), i.e. the `jade` source is NOT exclusively Conseil d'État.

The administrative publication marker in the DILA bulk is `PUBLI_RECUEIL`, a **class letter** (e.g. `"C"`), not a boolean and not aligned letter-for-letter with the judicial B/R/I ladder (`tests.rs:194`).

### 2.3 Caveats the design must internalize (authority ≠ relevance)

- **A highly on-point inédit can and should beat an off-topic published decision.** Authority is a *secondary* signal; it must never override a large relevance gap or it will bury the best answer.
- **Recency interacts with authority.** A published decision later overruled (revirement) is less useful than a fresh inédit applying the new rule. The corpus has decision dates (`valid_from`, §4) but no overruled/superseded edge, so "current good law" is not directly encoded.
- **Markers are not commensurable across orders.** Judicial `"oui"`/`["b","r"]` and administrative `"C"` cannot be compared on one scale without an explicit, defensible mapping.
- **Coverage is uneven (§4).** `capp` and `jade` carry no Bulletin/Rapport markers; over-trusting "published" risks systematically demoting whole sources for which the signal simply does not exist.

---

## 3. Current-state audit: how ranking works today

### 3.1 The fused score and where a re-rank would plug in

- **Score formula.** `ranked_candidate_ctes` builds the `ranked` CTE; for Hybrid the `fused_score` is the weighted RRF of `lexical_rank` and `dense_rank` (`retrieval.rs:636-646`). BM25-only and Dense-only modes use a fixed `1.0 / (60.0 + rank)` (`:675-681`, `:722-728`).
- **Final order.** The chunk grouping orders by `round(fused_score, 8) DESC, chunk_id` and pages with a keyset cursor on the rounded score (`retrieval.rs:318`, cursor `:335-336`, `cursor_predicate` `:536-550`). The document grouping picks each document's best chunk then orders by `cursor_score DESC, document_id` (`:361-371`, `document_cursor_predicate` `:555-567`).
- **Tunable surface.** Per-request `RetrievalOptions { rrf_lexical_weight, rrf_dense_weight, ivfflat_probes }` (`retrieval.rs:63-68`), resolved by `effective_rrf_weights` / `effective_probes` (`:161-173`) with env fallbacks `JURISEARCH_RRF_LEXICAL_WEIGHT` / `JURISEARCH_RRF_DENSE_WEIGHT` (`:23-35`). These are carried as immutable request state, NOT process env, so concurrent requests can differ deterministically (`:60-68`).
- **Plug-in points for a boost** (analysis, not a recommendation):
  - *In-SQL, inside `ranked`*: multiply/add an authority term into `fused_score`. Touches the proven default expression directly — highest isolation risk.
  - *In-SQL, after `ranked`, in `limited`/`scored`*: re-order `limited`'s `ORDER BY` (`:318`, `:370`) with an authority key BEFORE the `LIMIT`. Must keep the cursor's ordering key consistent or paging breaks.
  - *In Rust, post-SQL*: `search_with_postgres` parses the candidate JSON into a `Value` and already mutates it (truncate to `top_k`, attach cursor/pagination/routing — `main.rs:3769-3805`). A deterministic re-rank of the already-materialized candidate array could live here. The candidates carry `source` and `citation`/`title` but NOT `publication` today (`retrieval.rs:331-334`), so the projection would have to add the authority fields first.

### 3.2 What is filterable today (corpus-wide)

`DecisionFilters` (`retrieval.rs:90-102`) → SQL via `predicate()` (`:120-156`):
- `jurisdiction` → `d.canonical_json->>'jurisdiction' ILIKE %…%` (`:125-130`)
- `formation` → `d.canonical_json->>'formation' ILIKE %…%` (`:131-136`)
- `publication` → `lower(d.canonical_json->>'publication') = lower(…)` **exact** (`:137-142`)
- `decided_from` / `decided_to` → `d.valid_from >= / <= …::date` (`:143-154`)
- Any non-empty filter implies `d.kind = 'decision'` (`:124`).

Crucially these are **hard filters** (rows excluded), not ranking signals. The same `DecisionFilters` is reused verbatim by the zone path (`zone_retrieval.rs:208-212`, `ZoneCandidateQuery.decision_filters` at `:37-38`), so the *filter* dimension is already corpus-wide and consistent. CLI surface: `--court` / `--formation` / `--publication` / `--decided-from` / `--decided-to` (`main.rs:329-343`, mapped in `decision_filters()` `:407-415`).

### 3.3 What authority data exists, and where (the critical audit)

**`documents` has NO projected authority columns.** The table columns are `document_id, source, kind, source_uid, version_group, citation, title, body, valid_from, valid_to, valid_to_raw, source_url, source_payload_hash, canonical_json, …` (`migrations.rs:27-44`). So:

- **`source`** (`cass`/`inca`/`capp`/`jade`) — a **first-class column** on `documents` (`migrations.rs:29`), already selected into every candidate (`retrieval.rs:331`, zone `:264`). This is the cheapest, most reliable authority axis: it cleanly separates published-Cassation / inédit-Cassation / appeal / administrative. **`zone_units` also carries its own `source` column** (`migrations.rs:505`), so the zone path can read source authority without even joining `documents`.
- **`publication`, `formation`, `jurisdiction`** — NOT columns; they live ONLY inside `documents.canonical_json` (the canonical `CanonicalDecision` is serialized whole into `canonical_json`, `projection.rs:344` and the insert at `:345-366`). They are reached at query time via `canonical_json->>'…'` (`retrieval.rs:127-139`). There are GIN/expression indexes for ECLI and normalized case numbers (`migrations.rs:412-413`, `:429-430`) but **none for `publication`** — a publication-based filter or boost is currently a JSON extraction with no supporting index.
- **`decision_date`** is mapped to `documents.valid_from` at projection (`valid_from = decision_date`, `projection.rs:284`/`:357`), so recency is available as a real date column.

**Publication marker quality / coverage per source** (the honesty problem):

| Source | Order | Marker field (bulk XML) | Stored value(s) | Authority granularity |
|---|---|---|---|---|
| `cass` (published) | judicial | `PUBLI_BULL@publie` → `canonical_json.publication` | `"oui"` / `"non"` (boolean-ish flag) | binary published/not — **NO B/R/I tier from the bulk** |
| `inca` (inédit) | judicial | `PUBLI_BULL@publie` | typically `"non"`/absent | binary |
| `capp` | judicial (appeal) | `PUBLI_BULL@publie` | usually absent / `"non"` | **no Bulletin concept — marker not meaningful** |
| `jade` | administrative | `PUBLI_RECUEIL` → `canonical_json.publication` | class letter, e.g. `"C"` | Lebon class letter — **different vocabulary, not B/R/I** |

Evidence: extraction logic at `crates/jurisearch-ingest/src/juri/mod.rs:680-683` (judicial = `PUBLI_BULL_publie`, administrative = `PUBLI_RECUEIL`); `capture_publi_bull` stores the raw `publie` attribute (`mod.rs:441-443`); test asserts judicial `publication = "oui"` (`tests.rs:89`) and administrative `publication = "C"` (`tests.rs:194`). The CLI help string itself documents the gap: `--publication` "e.g. \"oui\" for published, \"C\" for recueil class" (`main.rs:335`).

**The fine-grained PBRI ladder (`["b","r"]`) is NOT in the bulk corpus.** It comes from the **Judilibre `/decision` API**, observed in code where inédit decisions resolve with `publication=[]` (`main.rs:4202-4203`). Judilibre responses are durably archived in **`official_api_responses`** (migration **v16**, `migrations.rs:582-637`) — the raw body + parsed `response_json` (`:609-610`). So the richer marker is *archived and queryable in `response_json`* but **not projected onto `documents` or `zone_units`** and **only exists for Judilibre-reachable Cassation decisions** (`cass`/`inca`; NOT `capp`/`jade`, `main.rs:4204-4205`, migration note `migrations.rs:446-448`). (Correction to the brief: `official_api_responses` is migration **v16**; `v17` is `decision_legislation_citations` — `migrations.rs:639-701`. The Judilibre `publication` field is inside v16's `response_json`, also potentially in `decision_zones.raw_json` `migrations.rs:464`.)

**Net data-availability picture:**
- Reliable, indexed, corpus-wide authority axis: **`source`** (4 levels).
- Coarse, JSON-only, judicial-binary / administrative-class: **`canonical_json.publication`** (no index, source-dependent semantics).
- Rich PBRI ladder: **only in archived Judilibre responses**, Cassation-only, not projected — would need a backfill to be usable in ranking.
- Recency: `valid_from` (real date column).

### 3.4 The zone path mirrors the main path

`zone_candidates_json` (`zone_retrieval.rs:198-277`) is the deliberate "Option B" sibling: same RRF/probes machinery (`:160-189`), same shared helpers (`effective_rrf_weights`, `effective_probes`, `document_cursor_predicate`, `format_sql_f64`, `RRF_K` imported at `:14-17`), same candidate JSON shape plus `zone`/`zone_accurate`/`provider` (`:259-272`), grouped to one best fragment per decision (`:240-243`). It is Cassation-only (`cass`+`inca`) and every unit is official (`:7-8`). Because it reuses the SAME score expression and the SAME `DecisionFilters`, **any authority term added to one path must be added to the other to preserve consistency** — and the zone path already has `source` locally (`migrations.rs:505`) plus the `documents` join (`zone_retrieval.rs:238`) for `canonical_json.publication`.

---

## 4. The eval / gate that any change must answer to

### 4.1 Phase 2 france-juris benchmark (the gate)

- Entry: `eval_france_juris_payload` (`main.rs:2526`), runs gold qrels through the production pipeline `search_with_postgres(Hybrid, kind=decision)` (`:2628-2661`) and `citation_lookup_json` (`:2717`).
- Categories & metric: **recall@10** (document-level, fixed `top_k=10`, `:2545-2547`) for `judicial_retrieval` (cass/capp/inca) and `administrative_retrieval` (jade) (`france_juris.rs:54-57`); plus `decision_citation` accuracy for ecli/pourvoi/cetatext.
- Gold construction: **known-item**, NO human/LLM — query = a cleaned excerpt of the decision's official headnote (`decision_summary` chunk) with identifiers stripped; gold = the one containing `document_id` (`france_juris.rs:1-15`, `:71-80`).
- Floors (the regression guard): `PHASE2_MIN_RETRIEVAL_RECALL_AT_10 = 0.50` (`main.rs:165`), min 15 judicial + 15 administrative queries (`:166-167`), citation accuracy `0.95` (`:168`) with ≥10 queries/identifier (`:171`). Pass logic `:2750-2756`; the gate re-derives pass/fail from the recorded fields, and metrics are floored so the recorded value can never exceed the measured one (`:2735`, floor at `:2778` etc.). Gate re-derivation: `main.rs:10712-10797`.

### 4.2 Zone benchmark (separate, measured-only)

- `eval_france_juris_zones_payload` (`main.rs:2834`) emits a SEPARATE `phase2_zone_benchmark` artifact, measures recall@10 of `search --zone …` per zone (`:2864-2872`), is **NOT a Phase 2 gate input**, and records a PROPOSED advisory floor only (`:897`, `:2830-2833`).

### 4.3 What this means for an authority change

The gate measures **known-item recall@10**: "is the single correct decision in the top 10?" It does **NOT** measure "is the *most authoritative relevant* decision ranked first." Two consequences:

1. **Primarily a regression guard.** The current gate only observes whether the single known-item gold *crosses the top-10 boundary*. Re-ordering WITHIN the top-10 does not change recall@10, so the gate cannot reward "most authoritative relevant result first." It CAN, incidentally, show a recall@10 *gain* if a gold doc currently sitting just below rank 10 is promoted into the first 10 — and, conversely, it can *regress* if a boost pushes a current hit from rank ≤10 to >10. So treat it as a regression guard (keep recall@10 ≥ 0.50 and not below the current measured value); incidental gains are possible but it is NOT a metric for authority-ordering quality.
2. **A new metric is needed to show benefit** (§6) — recall@10 is blind to ordering-by-authority quality.

---

## 5. Option space (enumerated and compared — no choice made)

For each option: how it plugs into today's code, isolation risk vs the proven default path + gate, eval-ability, complexity, and how it stays consistent across main + zone.

### (a) Filter-only / status quo
**What:** keep ranking relevance-only; expose authority solely as the existing hard filters (`--publication`, and implicitly `--court`/source). User narrows to "published Cassation" explicitly.
- *Plug-in:* already shipped (`retrieval.rs:120-156`; `main.rs:329-343`).
- *Isolation:* perfect — no change to default ranking; opt-in only.
- *Eval-ability:* nothing new to measure; gate unaffected.
- *Complexity:* zero.
- *Main+zone:* already consistent (shared `DecisionFilters`).
- *Cost:* does not satisfy the motivation — relevance order among returned decisions still ignores authority; the user must know to filter, and a hard filter can *exclude* a perfect inédit.

### (b) Post-retrieval deterministic re-rank / boost of the candidate set (Rust, after SQL)
**What:** widen the SQL output to a re-rank window, re-order that window in Rust by a deterministic function of relevance + authority (e.g. stable sort by `fused_score` adjusted by a bounded authority bonus), then truncate to `top_k`.

**Critical pool correction — there are THREE distinct pools, not one** (load-bearing for this option):
1. **Arm limits** — `lexical_limit` / `dense_limit` = `top_k * 4` (concise) or `top_k * 20` (zone) (`main.rs:3691-3696`, zone `:3422-3441`). These bound the BM25 and dense arms that FEED the RRF fusion; they are NOT what Rust receives.
2. **Final SQL output `LIMIT`** — `search_with_postgres` separately passes `query_limit = top_k + 1` into `hybrid_candidates_json` (`main.rs:3697`, `:3711-3727`), which storage applies as the final page `LIMIT` (`retrieval.rs:366-371`, chunk mode `:317-319`). So the SQL returns at most `top_k + 1` rows (the page + one pagination sentinel).
3. **Returned `top_k`** — Rust then truncates from ≤ `top_k + 1` to `top_k` (`main.rs:3777-3784`; zone `query_limit=top_k+1` at `:3422-3441`, truncate `:3466-3474`).

**Therefore a Rust-only re-rank as-is sees only the displayed page plus the sentinel — it can re-order within the top-10 but CANNOT promote an authoritative result from, say, rank 15 into the top 10.** To be useful, option (b) MUST first widen pool #2: increase the SQL final output `LIMIT` to a re-rank window (e.g. `top_k * W`), re-rank that window in Rust, truncate to `top_k`, and then re-work the cursor for the wider window. Without that SQL-limit change, (b) degenerates into a within-page re-order.

- *Plug-in:* widen `query_limit` to a re-rank window in `search_with_postgres`/`zone_search_payload`, then re-order the materialized array in the JSON-mutation region (`main.rs:3769-3805`) BEFORE `truncate(top_k)` (`:3777-3785`). Requires the candidate projection to expose `source` (already present, `retrieval.rs:331`) and `publication` (NOT present — must be added to the SELECT/JSON, and likewise in zone `:264`).
- *Isolation:* the proven SQL **score expression** is untouched; the re-rank is a separable, gateable layer that can default OFF. But note it is NOT zero-SQL-change: widening the output `LIMIT` and re-deriving the cursor are required, so "default OFF" must also restore the original `top_k + 1` limit + cursor when the knob is unset.
- *Eval-ability:* high — run the gate with the layer on/off; deterministic (stable tie-breaks already exist on `document_id`/`chunk_id`).
- *Complexity:* moderate–high — the re-rank window widening + cursor re-derivation is the real work. Pagination currently keys on the rounded `fused_score` (`retrieval.rs:535-567`); once the displayed order diverges from the SQL `fused_score` order, the keyset cursor must be re-derived from the new order (or re-ranking confined to the first page).
- *Main+zone:* a shared Rust helper applied in both `search_with_postgres` and `zone_search_payload` (`main.rs:3373-3439`) keeps them consistent; both produce the same candidate shape and both need the same `query_limit`/cursor rework.
- *Caveat:* even a widened window is bounded by the arm limits (#1, `top_k*4`/`top_k*20`) feeding the RRF — the re-rank can resurface an authoritative doc only if it is within the fused candidate set, never one that fell outside the arms entirely.

### (c) Authority term blended into the RRF fused_score (in-SQL, inside `ranked`)
**What:** add an authority component to `fused_score`, e.g. `+ authority_weight * authority_score(source, publication)` or an extra RRF arm.
- *Plug-in:* directly in the `ranked` CTE expression (`retrieval.rs:641-645`) and the zone twin (`zone_retrieval.rs:179-184`). Needs `source`/`publication` available inside the CTE (a join to `documents` already exists in `lexical`/`dense`; `publication` would be an extra `canonical_json->>` extraction — unindexed).
- *Isolation:* **highest risk** — it changes the exact default score that the gate validated and that the RRF weights were calibrated against (`retrieval.rs:15-18` documents the LEGI calibration). A non-zero default authority weight is a default-behaviour change; a zero default makes it inert (then it is effectively option (b) but in SQL).
- *Eval-ability:* measurable, but the score is now entangled with the RRF weights, so re-tuning interactions (lexical/dense/authority) is a 3-way sweep, not 2-way (`eval tune` supports `rrf-dense`/`rrf-lexical`/`probes` today, `main.rs:2085-2122` — authority would be a new sweep param).
- *Complexity:* high — couples authority to the fusion math; cursor stays consistent automatically (still ordering by `fused_score`).
- *Main+zone:* must edit both score expressions identically; the shared `format_sql_f64`/`RRF_K` helps but the term itself is duplicated.

### (d) Authority as a strict tie-breaker
**What:** keep `fused_score` as the primary key; use authority only to break ties (or near-ties within an epsilon band), e.g. `ORDER BY fused_score DESC, authority_rank DESC, id`.
- *Plug-in:* change the final `ORDER BY` in `limited`/`scored` (`retrieval.rs:318`, `:336`, `:370`, `:389`) and the zone equivalent (`zone_retrieval.rs:243`, `:249`, `:270`); the cursor predicate must include the new key or paging breaks (`:536-567`).
- *Isolation:* low–moderate — exact-score ties are rare (RRF over distinct ranks), so a pure tie-break changes almost nothing; an epsilon-band tie-break changes more and is closer to (b)/(c) in effect.
- *Eval-ability:* good; near-inert on recall@10 (only re-orders equal scores), so it mostly needs a NEW ordering-quality metric to show value.
- *Complexity:* moderate (cursor key change is the trap). Determinism preserved.
- *Main+zone:* must replicate the ORDER BY + cursor change in both; today both share `document_cursor_predicate`, so a shared change is feasible but the cursor encoding (`<score>:<id>`) would need an authority component.

### (e) Learning-to-rank / cross-encoder rerank model
**What:** a learned reranker over (query, candidate) features incl. authority, source, recency, BM25/dense scores.
- *Plug-in:* a new post-retrieval stage (like (b) but model-driven), consuming the candidate pool.
- *Isolation:* layerable OFF by default, but introduces a model dependency, latency, and non-determinism risk (must be pinned/seeded to keep the gate reproducible — the benchmark relies on determinism, `france_juris.rs:14-15`).
- *Eval-ability:* needs labeled training/eval data the project does NOT have — current gold is known-item with one relevant doc per query (`france_juris.rs:7-12`); no graded relevance, no "which of several relevant is most authoritative" labels. Building that is a project in itself and risks human/LLM-in-gold, which the benchmark explicitly avoids (`main.rs:2810-2812`).
- *Complexity:* highest; likely premature given no graded eval set.
- *Main+zone:* one model can serve both, but feature parity (zone vs chunk) must be ensured.

### Comparison summary

| Option | Isolation risk | Default-behaviour change | Eval-ability w/ current gate | New metric needed | Complexity | Main+zone consistency |
|---|---|---|---|---|---|---|
| (a) filter-only | none | none | n/a | no | none | already shared |
| (b) post-SQL re-rank | low (score expr untouched) | none if default-off (must restore `top_k+1` limit+cursor) | high (on/off) | yes (to show gain) | moderate–high (widen output LIMIT + re-derive cursor) | shared Rust helper |
| (c) RRF-blended | **high** | yes unless weight=0 | medium (3-way tune) | yes | high | duplicate in 2 SQL |
| (d) tie-breaker | low–moderate | minimal (pure ties) | high (near-inert) | yes | moderate (cursor) | duplicate ORDER+cursor |
| (e) LTR/cross-encoder | medium (model dep) | none if off | **blocked** (no graded data) | yes + training data | highest | one model, feature parity |

---

## 6. Risks & invariants

- **Isolation invariant (hard).** The default path is proven and gate-locked; the codebase is explicit about it (`retrieval.rs:1349-1350` "no change to default retrieval"; zone isolation comments `zone_retrieval.rs:1-12`; migration isolation notes `migrations.rs:443-448`, `:486-493`). Any authority mechanism should default to **OFF / inert** so that, absent an explicit knob, behaviour is byte-identical (`main.rs:3286` already promises "byte-identical … below" when `--zone` is absent — a new ranking knob should make the same promise when unset).
- **Recall/precision trade-off.** Over-boosting authority can push the single most-relevant decision out of the top-k and *regress recall@10* (the only thing the gate measures). The boost must be bounded and relevance-dominant (cf. the inédit-beats-off-topic-published caveat, §2.3).
- **Honesty / no overclaiming.** `capp` and `jade` lack Bulletin/Rapport markers; the binary `publie="oui"` and the Lebon class letter are NOT the PBRI ladder. The system must not present a `capp` decision as "unpublished/low-authority" merely because the marker is absent, nor invent a B/R tier the bulk does not carry. The PBRI ladder is only honestly available for Judilibre-resolved Cassation decisions (in archived `response_json`, §3.3). This mirrors the existing `zone_accurate` honesty discipline (`zone_retrieval.rs:266`, `migrations.rs:445`).
- **Determinism.** The benchmark is deterministic by construction (`france_juris.rs:14-15`); any authority term must be a pure function of stored fields with stable tie-breaks (the existing `, id` tie-breaks must be preserved).
- **Cursor / pagination correctness.** The keyset cursor encodes the rounded `fused_score` (`retrieval.rs:335-336`, `:555-567`). If the *displayed* order stops matching the *SQL* order (options b/d), paging will duplicate or skip rows. Any re-rank must either re-derive the cursor from the new order or be confined to non-paged contexts.
- **Config / backward-compat.** A new knob should follow the existing `RetrievalOptions` precedent — per-request, immutable request state, env fallback, validated range (`retrieval.rs:60-68`, `main.rs:399-405`, `validate_retrieval_options` `:419-436`) — so warm sessions and a future server stay concurrency-safe.
- **No index for `publication`.** A publication-driven boost/filter is currently an unindexed `canonical_json->>` extraction (`retrieval.rs:137-139`; no publication index in `migrations.rs`). A design that ranks on it at scale likely needs a projected column or expression index (a migration), unlike `source` which is already a column.

---

## 7. Eval strategy (measure, don't assume)

The driving principle: **authority-aware ranking must be measured to help, and guarded against recall regression.** Concrete, gateable ideas (for the design to choose among):

1. **Regression guard (mandatory, exists today).** Re-run the Phase 2 france-juris gate (`eval france-juris`) and the zone benchmark (`eval france-juris-zones`) with the authority layer ON and OFF. Pass condition: judicial & administrative recall@10 must NOT drop below the current measured value (and never below `0.50`, `main.rs:165`). This catches "authority buried the gold doc."
2. **A new ordering-quality metric (to show benefit).** recall@10 cannot reward better ordering-by-authority. Candidate approaches:
   - **Authority-graded gold:** for a query with several relevant decisions, label the *most authoritative correct* one; measure how often it ranks first / nDCG with authority-weighted gains. This needs graded labels the corpus does not yet have — and must avoid human/LLM-in-gold to stay consistent with the benchmark's honesty stance (`main.rs:2810-2812`), or be a clearly separate, advisory, measured-only artifact like the zone benchmark.
   - **Synthetic authority qrels from official fields (no human/LLM):** e.g. when two decisions share the same headnote/visa/citation cluster but differ in `source`/`publication`, the published-Cassation one is the structurally-authoritative answer. This stays "publisher-authored" labels (like the existing known-item gold, `france_juris.rs:1-15`).
   - **Pairwise preference checks:** sample (published-Cassation, inédit/appeal) pairs that BOTH match a query; measure the fraction where the authoritative one ranks higher — a directly interpretable "authority lift" number.
3. **A new measured-only benchmark category/artifact**, modeled on `phase2_zone_benchmark` (separate `kind`, separate `--out`, advisory floor, NOT a gate input — `main.rs:2828-2833`, `:897`), so an authority metric can be tracked without inflating the gated full-corpus claim until it is trusted.
4. **Parameter sweep for the authority weight**, extending `eval tune` (which already sweeps `rrf-dense`/`rrf-lexical`/`probes`, `main.rs:2085-2122`) with an `authority` param, to find a setting that maximizes the new metric subject to no recall@10 regression.
5. **Per-source breakdown.** Report the metric split by `source` so a gain concentrated in Cassation cannot mask a regression on `capp`/`jade` (where the marker is absent).

---

## 8. Open questions to resolve in the DESIGN phase

1. **Which mechanism** (a–e)? The isolation invariant strongly favors a default-OFF *layer* (b/d) over editing the proven `fused_score` (c); the design must justify the choice against the new ordering metric, not assume it.
2. **Authority scale & mapping.** How to combine the reliable `source` axis (4 levels) with the coarse `publication` marker — and whether to project/backfill the richer Judilibre PBRI ladder from `official_api_responses.response_json` (Cassation-only) onto a queryable column. Define an explicit, defensible cross-order mapping (judicial `oui`/`b`/`r` vs administrative Lebon class), or keep the orders on separate scales.
3. **Where authority data should live.** Stay JSON-only (`canonical_json->>'publication'`, unindexed) vs add a projected column / expression index (a migration) — required if (c)/(d) rank on publication at scale.
4. **Boost magnitude / bound.** How much authority may move a result, and the relevance dominance rule that prevents burying a more-relevant inédit (the §2.3 caveat). Likely an explicit cap, or band-limited tie-break.
5. **Pagination/cursor design** if the displayed order diverges from the SQL order (options b/d): re-derive the cursor from the new order, or confine re-ranking to the first page.
6. **Recency / "good law".** Whether to fold decision date (`valid_from`) into the authority/freshness score, and whether an overruled-edge is worth building (none exists today).
7. **Config surface.** The exact knob name(s), defaults (must be inert), validation range, env fallback, and whether it lives in `RetrievalOptions` (shared by both paths) — and how it is exposed on `search`, `session`, and a future `serve`.
8. **Gold for the new metric.** Can an honest, publisher-authored, no-LLM authority-graded gold be built from official fields, or must it be a separate measured-only advisory artifact?
9. **Scope decision.** Apply to BOTH paths (the stated requirement) — confirm the zone path's Cassation-only population still benefits from a `source`-based axis where the only distinction is `cass` vs `inca` (it does: published vs inédit is exactly the within-Cassation authority split).

---

## Appendix — key citations

- Score / RRF / weights / options: `crates/jurisearch-storage/src/retrieval.rs:13-19`, `:23-35`, `:60-68`, `:161-173`, `:636-646`, ordering `:318`/`:370`, cursor `:535-567`.
- DecisionFilters (incl. `publication`): `crates/jurisearch-storage/src/retrieval.rs:90-156`.
- Zone path: `crates/jurisearch-storage/src/zone_retrieval.rs:1-17`, `:160-189`, `:198-277`.
- Decision projection (canonical_json holds publication/formation/jurisdiction; valid_from = decision_date): `crates/jurisearch-storage/src/projection.rs:281-366`.
- `documents` schema (no publication column; `source` is a column): `crates/jurisearch-storage/src/migrations.rs:27-44`; ECLI/case-number indexes (no publication index) `:412-430`.
- Publication extraction & values: `crates/jurisearch-ingest/src/juri/mod.rs:150-151`, `:441-443`, `:680-683`; tests `crates/jurisearch-ingest/src/juri/tests.rs:89`, `:194`.
- Judilibre archive (v16) + PBRI only-here / Cassation-only: `crates/jurisearch-storage/src/migrations.rs:582-637`, `:446-448`; `crates/jurisearch-cli/src/main.rs:4202-4208`.
- Zone tables: `crates/jurisearch-storage/src/migrations.rs:450-573` (`zone_units.source` at `:505`).
- CLI search args / filters / routing / re-rank plug-in: `crates/jurisearch-cli/src/main.rs:296-415`, `:3282-3324`, `:3373-3439`, `:3659-3834`.
- Phase 2 gate + benchmark: `crates/jurisearch-cli/src/main.rs:160-172`, `:2522-2826`, `:10712-10797`; gold `crates/jurisearch-storage/src/france_juris.rs:1-80`.
- Zone benchmark (measured-only): `crates/jurisearch-cli/src/main.rs:2828-2833`, `:2834-…`.
