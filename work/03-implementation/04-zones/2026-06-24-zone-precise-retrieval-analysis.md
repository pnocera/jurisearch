# Zone-Precise Jurisprudence Retrieval — Analysis

Date: 2026-06-24
Status: ANALYSIS ONLY (no implementation plan). Scope: how jurisearch could support precise
zone-scoped decision retrieval — e.g. *"find decisions whose **motivations** discuss X"* — as the
best long-term capability, not the lazy retrieve-then-rerank shortcut.
Index analysed: `/mnt/models/jurisearch-index/phase2-full-juridic` (schema v12).

---

## 1. The capability gap

### 1.1 What is wanted
Retrieval **scoped to a functional part of a decision**: the user asks about the court's *reasoning*
(`motivations`), or the *holding* (`dispositif`), or the *grounds raised* (`moyens`) — and gets
decisions ranked by relevance **within that zone**, not anywhere in the text.

This is a genuinely different query than topical decision search. Its value is legal precision:
- "the court **reasoned** about X" (motivations) ≠ "a party **argued** X" (moyens, which the court may
  have **rejected**) ≠ "the **facts** mention X" (exposé). Conflating them produces wrong law.
- It makes answers citable to the operative part of a ruling.

### 1.2 Why the current index cannot do it
Three independent reasons, each sufficient:

1. **Chunks are zone-blind.** Decisions are chunked heuristically into one `decision_summary` chunk
   (from `SOMMAIRE`) plus `decision_body` chunks split on paragraph boundaries
   (`crates/jurisearch-ingest/src/juri/mod.rs`). Verified on `cass:JURITEXT000051743650`: one
   `decision_summary` (877 chars) + two `decision_body` chunks (5,490 and 4,392 chars). Each body chunk
   spans **multiple zones** (header + facts + moyens + motivations + dispositif mixed). No chunk carries
   a zone label; `chunk_kind` only distinguishes `decision_summary` vs `decision_body`.
2. **No zone dimension in retrieval.** `HybridCandidateQuery` filters by `kind_filter`
   (article/decision) and `DecisionFilters` (court/formation/publication/decided_from/decided_to)
   only (`crates/jurisearch-storage/src/retrieval.rs`). There is no zone filter and nothing to filter
   on.
3. **Official zones are absent from the searchable corpus.** Official zones come only from Judilibre
   (`decision_zones`, v12), are **lazy** (2 rows today — only what `fetch --part --online` touched),
   and are not part of any embedding/BM25 index.

Result: jurisearch answers "decisions that *mention* X somewhere" (whole-body topical retrieval), not
"decisions whose *motivations* are about X". The former is noisier and legally weaker for this use.

---

## 2. The data foundation (hard constraints on any solution)

### 2.1 Official zones exist only for Cour de cassation
Judilibre provides official zones (`introduction, expose, moyens, motivations, dispositif, annexes`)
for Cour de cassation decisions, resolved by the **first parser-valid pourvoi + decision date**
(ECLI-as-query fails; verified live). The current resolver (`decision_resolution_metadata_json`) only
yields a pourvoi when a `case_numbers` entry matches `^[0-9]{2}-[0-9]{4,6}$`; without it, enrichment
caches `unsupported` and returns no zone. So **reachability is gated on having a parser-valid pourvoi**,
not just on being a Cassation decision. Coverage in this index (1,144,796 decisions total), with the
parser-valid-pourvoi counts measured on the index:

| source | family | decisions | parser-valid pourvoi | official zones reachable (current resolver) |
|---|---|---|---|---|
| `cass` | Cour de cassation (publié) | 141,616 | **117,674** | 117,674 (23,942 cass have NO parser-valid pourvoi) |
| `inca` | Cour de cassation (inédit) | 384,312 | 377,027 | 377,027 |
| `capp` | Cour d'appel | 72,929 | 120 | **~0** — RG numbers don't resolve on Judilibre (codex-probed); the 120 are not usable |
| `jade` | administrative (Conseil d'État etc.) | 545,939 | 0 | **0** — Judilibre doesn't cover administrative |

So official-zone retrieval with the **current** resolver is reachable for **117,674 + 377,027 =
494,701 decisions (~43%)** of the jurisprudence corpus — NOT all of `cass`. The other ~57% can never
have official Judilibre zones with today's mechanism: capp (RG numbers, no resolver), jade
(administrative, no coverage), and **~23,942 `cass` + ~7k `inca` Cassation decisions that lack a
parser-valid pourvoi** (these would need an ADDITIONAL Cassation resolver — e.g. a different number
form or a reliable ECLI path — which does not exist today and is an open design item, not a given).
**This partiality is fundamental, not temporary**, and must be surfaced honestly: "motivations-scoped
search" would only ever search the resolver-reachable Cassation subset.

### 2.2 The two-texts problem (the decisive technical finding)
Judilibre zone offsets are **character indices into Judilibre's own `text`**, which is a **different
serialization** from our DILA `body`. Verified on `cass:JURITEXT000051743650`:
- local DILA `body` = **9,883 chars** (10,203 bytes)
- Judilibre `text` = **10,001 chars** (10,319 bytes)

Different length, different layout (the DILA body carries a different header rendering). Therefore the
official offsets **cannot be mapped onto our local body or our existing chunks** — doing so would
mis-slice. **Any precise zone unit must be derived from the Judilibre zone text itself**, not
retrofitted onto local chunks. We already store the full Judilibre response per enriched decision in
`decision_zones.raw_json`, but note (see §5.1) that the **normalized** `zones_json` currently holds only
`motivations`/`moyens`/`dispositif` fragment text — not `introduction`/`expose`/`annexes`. This single
fact eliminates the cheapest "label existing chunks by offset" approach.

### 2.3 External-API dependency and freshness
Zones are not self-contained in the bulk corpus; they are fetched from an external government API
(PISTE/Judilibre; live limits ~20 req/s burst, ~1M per ~14.6h window). A corpus-scale zone capability
is only as complete/fresh as a **backfill + refresh** against Judilibre (keyed on `update_date`). The
index cannot be "rebuilt offline" for zones the way everything else can.

---

## 3. What "best" actually requires (two coupled pieces)

1. **Zone DATA at corpus scale** — official zones for all resolvable Cour de cassation decisions stored
   eagerly (a bulk + incremental Judilibre backfill into `decision_zones`), not lazy. Order of
   magnitude: ~494,701 resolvable decisions × ~2 API calls ≈ ~990k calls ≈ ~14h at the quota; storage is modest (zone
   text already deduplicated per decision).
2. **Zone-AWARE retrieval** — official zone text represented as **first-class retrieval units** so a
   query can be scoped/boosted to a zone. This is the harder, more consequential piece, and the rest of
   this analysis is mostly about it.

Both are needed: data without zone-aware retrieval only powers `fetch` (display), not search; zone-aware
retrieval without the data has nothing to index.

---

## 4. Design space for zone-aware retrieval

### Option A — Zone units in the MAIN `chunks` table (+ a `zone` filter on `HybridCandidateQuery`)
Put official zone units into the same `chunks`/`chunk_embeddings`/BM25 tables the decision corpus
already uses, with a `zone` label, and add a `zone` filter parallel to `kind_filter`. This splits into
two materially different sub-variants that must not be conflated (the current contract joins retrieved
`chunks` to `documents`, serves snippets from `chunks.body`, and `fetch` serves `documents.body` from
the local DILA serialization):
- **A1 — replace** Cassation `decision_body` chunks with Judilibre-text zone chunks. Best zone precision,
  but now the **default topical** snippets and embeddings for those decisions come from Judilibre's
  serialization while `fetch` still returns the local DILA `body` — a snippet/representation mismatch,
  and it discards the validated heuristic body chunks.
- **A2 — add** official zone chunks ALONGSIDE the existing heuristic body chunks in the same table.
  Keeps topical retrieval text intact, but introduces a **retrieval-unit type/provenance model**
  (a decision now has both heuristic body chunks and official zone chunks), duplicate decision
  representations, and a requirement that default (non-zone) queries **exclude** zone-only chunks (or
  they double-count).
- **Pros (both):** one index; the zone filter is a natural extension of the existing filter model;
  supports `search --kind decision --zone motivations "X"`.
- **Cons (both):** embeds new official zone units and **entangles the honesty model** — the current
  invariant "bulk decision chunking must be `heuristic` / `zone_accurate=false`" (enforced in
  `juri/mod.rs` + the Phase 2 gate) must evolve to admit an OFFICIAL chunk class beside the heuristic
  one. (A1 additionally re-embeds/replaces the existing topical decision chunks and changes
  snippet/`fetch` consistency; A2 keeps the existing topical chunks intact and only adds zone units.)
  High blast radius on the proven retrieval path.

### Option B — Parallel zone index (separate subsystem)
Keep the existing whole-decision chunks/embeddings untouched for topical retrieval. **Add** a separate
zone retrieval structure (zone "chunks"/embeddings keyed by `(document_id, zone)`), built from the
stored Judilibre zone text. Zone-scoped queries hit the zone index; everything else is unchanged.
- **Pros:** does not disturb the validated whole-decision retrieval or its honesty invariant; the
  official-zone subsystem is **honest by construction** (it contains only official Cassation zones);
  incremental (a decision appears once enriched); clean provenance boundary.
- **Cons:** a **second dense + BM25 index** to build/maintain; **query routing** (decide when a query is
  zone-scoped); the zone index is partial (Cassation only) so the product must present it as a distinct,
  narrower capability.

### Option C — Offset overlay on existing chunks (no re-embed)
Tag each existing body chunk with the zone(s) its span overlaps, then filter.
- **Verdict: infeasible for precision.** Defeated by §2.2 (offsets index Judilibre text, not local
  body) and §1.2.1 (heuristic chunks straddle zones). It would produce fuzzy, wrong labels — exactly
  the heuristic imprecision the feature exists to escape. Discard.

### Option D — Retrieve-then-rerank (lazy)
Explicitly excluded by the user; noted only for completeness. It avoids the data + index work but cannot
give corpus-complete zone-scoped recall (only re-ranks an already-retrieved top-K, and needs K live
fetches per cold query). Not "the best".

---

## 5. Cross-cutting considerations for the real options (A/B)

### 5.1 Build zone units from Judilibre text, dedupe by decision
Because of §2.2, the zone retrieval units must be embedded from the **Judilibre zone fragment text**.
Note the current cache shape: the normalizer stores only `motivations`/`moyens`/`dispositif` fragment
text in `decision_zones.zones_json`, while the **full** Judilibre response (all six zones) is kept in
`decision_zones.raw_json`. So a zone-unit builder limited to those three zones can read `zones_json`
directly, but indexing `introduction`/`expose`/`annexes` (see the §8 open question) would require either
extending the normalizer (+ a re-normalize/backfill of existing cache rows) or deriving zone units from
`raw_json`. This keeps the zone index a clean function of the enrichment backfill: enrich → derive zone
units → embed. Multi-fragment zones (observed: motivations can have 2 fragments) must be handled
(concatenate per zone, or index per fragment with a zone key).

### 5.2 Embedding cost and model
~494,701 reachable decisions × ~3 retrievable zones (motivations/moyens/dispositif) ≈ ~1.5M zone units (vs the 4.7M
embeddings already present). Same locked `bge-m3:1024:normalize:true` fingerprint (no new model). At
the proven OpenRouter throughput (~195–292/s) this is single-digit hours, on top of the ~14h Judilibre
backfill. Bounded, but not free.

### 5.3 Retrieval & ranking model
A `zone` filter mirrors the existing `kind`/`DecisionFilters` mechanics; hybrid (BM25 over zone text +
dense over zone embeddings) reuses RRF + probes. Results group by decision and surface the matched zone.
Open question: whether zone-scoped queries should also blend a small signal from the whole-decision
context (a decision is more than its motivations) or stay strictly zone-local.

### 5.4 Honesty / provenance / gate
This is the subtle part. Today the corpus is uniformly heuristic and the Phase 2 gate asserts
`zone_accurate=false` at the source level for bulk jurisprudence. Zone-aware **retrieval** introduces
chunks/units that are `zone_accurate=true` (official Judilibre) for *some* decisions. The provenance
model already distinguishes "corpus-level heuristic" from "per-decision official overlay" for
`fetch --part --online`; zone-aware **search** extends that same distinction into the retrieval layer.
Every zone-scoped result must carry `zone_accurate` + provenance so an agent never treats a Cassation
official-zone hit and a heuristic body hit as equivalent. The gate's honesty checks would need an
explicit story for "official zone retrieval is a per-decision overlay over a Cassation subset, not a
corpus-wide property" — Option B keeps this story simplest (separate subsystem); Option A forces it into
the main index's invariants.

### 5.5 Coverage honesty in answers
A `--zone motivations` search must tell the caller it searched only the **Cour de cassation** subset
(cass+inca, only those enriched). Silently returning Cassation-only results to a corpus-wide-sounding
question would be a correctness/honesty bug. Whether to offer a clearly-marked **heuristic** zone
scoping for capp/jade (lower confidence, `zone_accurate=false`) is a real product decision — it widens
coverage but reintroduces heuristic imprecision and a second provenance tier.

### 5.6 Evaluation
The feature needs its own honest metric, or it cannot be claimed. A natural known-item construction:
query = an excerpt of a decision's official `motivations` (identifier-stripped) → measure recall@10
that the decision is retrieved **under a `motivations` scope**. This slots into the existing
`eval france-juris` pattern as a new category (e.g. `motivations_retrieval`), and guards against a
zone index that silently degrades.

---

## 6. Risks / unknowns

- **External dependency & freshness** — the zone capability's completeness tracks a Judilibre backfill;
  needs a refresh model (`update_date`/text hash) and a coverage report (how many decisions enriched).
- **Two-texts divergence** — central; resolved by building from Judilibre text, but means the zone units
  and the whole-decision body are different serializations of the same ruling (acceptable, but the
  product must not imply offset-alignment with the local body).
- **Structural partiality** — resolver-reachable Cassation only; ~43% of decisions (494,701), ~0% of
  capp/jade, and ~31k Cassation decisions without a parser-valid pourvoi excluded. Permanent without a
  second resolver.
- **Honesty-model expansion** — official-zone retrieval changes the "bulk is heuristic" invariant from
  absolute to "with a per-decision official overlay"; the gate/provenance must say so precisely.
- **Maintenance surface** — Option A grows the core index's complexity; Option B adds a second index +
  routing. Both add an enrichment pipeline to operate.
- **Multi-fragment / non-contiguous zones** — Judilibre warns zones may be non-sequential; the unit
  builder must preserve all fragments.

---

## 7. Recommended direction (analysis-level, not a plan)

The best long-term capability is a **first-class zone-scoped retrieval subsystem built from official
Judilibre zone text for Cour de cassation (cass + inca)**, with: an eager (bulk + incremental) zone
backfill feeding `decision_zones`; zone retrieval units derived from the stored Judilibre zone text
(never retrofitted onto the local body, per §2.2); a `zone` filter in hybrid retrieval; results carrying
official `zone_accurate`/provenance; explicit Cassation-only scope in responses and the gate; and a
zone-scoped eval category.

On the key architecture fork, the analysis leans toward **Option B (parallel zone index)** over **Option
A (re-chunk the main index)**: B preserves the validated whole-decision retrieval and its honesty
invariant, keeps the official-zone subsystem clean and partial-by-construction, and isolates the
external-dependency/freshness machinery — at the cost of a second index and query routing. A is more
unified but entangles the honesty model and forces a re-chunk/re-embed of the main decision corpus. This
fork (and the §5.5 product question of whether to offer marked-heuristic zone scoping for capp/jade) are
the decisions to resolve before any implementation.

Explicitly rejected: Option C (offset overlay — imprecise due to text divergence) and Option D (lazy
retrieve-then-rerank — cannot give corpus-complete zone recall).

---

## 8. Open questions to resolve before designing an implementation
1. Option A vs B (unified zone-labeled chunks vs a parallel zone index) — the central architecture fork.
2. Offer marked-heuristic zone scoping for the non-Judilibre corpus (capp/jade), or Cassation-only?
3. Strictly zone-local ranking, or blend a whole-decision context signal?
4. Which zones to index for retrieval (motivations/moyens/dispositif — and is exposé/introduction worth
   it)?
5. Gate/provenance: how official-zone retrieval coverage is reported and bounded so the "full-juridic"
   claim is not overstated.
6. Backfill operating model: one-shot vs continuous; freshness/refresh policy; coverage reporting.
