# `jurisearch` — Design Document

> **Name:** `jurisearch` (binary + Cargo workspace/crate); project directory `legalsearch`. (Decided — `DECISIONS.md` D1.)
> **Status:** Design **lock-ready** (architecturally sound to lock per the 2026-06-20 lock-readiness review) — not implementation. The conceptual reference is `work/02-conception/CONCEPTION.md`; this `DESIGN.md` remains the richer design/research record.
> **Ambition:** production-grade — the target is the **best-in-class French juridic search engine for AI agents**, not an MVP, demo, or RAG prototype. The phases in §17 are quality gates, not a path to a toy; "works on a sample" is never a milestone. Scope of the claim: **Phase 1 = best-in-class LEGI/statutory search**; full best-in-class *French juridic* search (statutes **and** jurisprudence) is reached at **Phase 2** (§17).
> **Scope:** A from-scratch, local-first **search engine + CLI** for the French legal corpus, designed to be **called by LLM harnesses that can spawn subprocesses** (Claude Code, tool-use agents, autonomous research agents). **CLI-only** — invoked one-shot or driven over a long-lived stdin/stdout **JSONL session**. No MCP, no HTTP server.
> **Source of requirements:** `work/00-foundation/search.md` — the seven architectural pillars below map directly onto that document.
> **Revised:** 2026-06-20 per `work/00-foundation/assessment.md` (legal failure modes → hard constraints, §2.1) **and** `work/reviews/2026-06-20-design-review.md` (system shape): the binary is **`jurisearch`**; the search/runtime core is **Rust** (Python only for offline ingestion helpers); the surface is **CLI-only** (MCP/HTTP/`serve` removed, replaced by a JSONL **session**); **complete inline help** is part of the agent contract.
> **Direction-locked:** 2026-06-20 per `work/reviews/2026-06-20-direction-lock-review.md`. The last three open decisions are now **decided**: (D3) backend is **embedded Postgres + `pgvector` + `pg_search`** — the spike now *validates* that one stack, it does not choose among peers; (D5) query embeddings default to a **remote / OpenAI-compatible HTTP endpoint** (which includes a local self-hosted server such as `llama.cpp`), with in-process Rust embeddings demoted to an optional offline backend; (D7) the authoritative corpus is **official DILA/LEGI XML from day 1** — no HF bootstrap of the authoritative index. The roadmap is reframed into **production-grade phases** (§17) with explicit best-in-class acceptance gates (§15). **Qdrant remains out of scope.**
> **Lock-ready:** 2026-06-20 per `work/reviews/2026-06-20-lock-readiness-review.md` — a wording-cleanup pass (zones-vs-relationships consistency, "selected backend / `pg_search`" wording, derived-dataset framing). No architecture reopened. Validation gates (backend spike, embedding-model eval, reranker eval) are the only remaining uncertainty.

---

## 1. Purpose & scope

`jurisearch` answers one question well: *"Given a legal query (and optionally a date, a court, a code), return the smallest set of authoritative, correctly-cited French legal passages an LLM needs to reason — and let the LLM verify every citation it produces."*

It is **not** a chatbot, not a RAG answer-generator, and not a legal-advice engine. It is the **retrieval + grounding substrate** that sits underneath an LLM agent. The agent does the reasoning; `jurisearch` does the finding, the structuring, the dating, and the proving.

### In scope
- French **codified law** (Codes, lois, règlements consolidés — the LEGI base), from **official DILA/LEGI XML**.
- French **jurisprudence**: Cour de cassation (Judilibre), Conseil d'État / CAA / TA (justice administrative open data).
- **Temporal** ("as-of") retrieval across statutory versions.
- **Hybrid** lexical+semantic retrieval with legal authority ranking.
- A **stable, token-efficient agent contract**: one-shot CLI `--json` + a **JSONL session** mode + **complete inline help**.
- **Citation verification** / grounding.

### Out of scope (deferred to later phases)
- **MCP server, HTTP server, `serve` daemon** — the surface is CLI-only by product decision. Warm multi-call use is served by the JSONL session mode (§11), which is still a plain subprocess interface.
- **Qdrant or any separate vector/search service** — the backend is decided (embedded Postgres, D3); Qdrant stays out unless the chosen stack fails hard acceptance criteria (§13.3).
- EU law (EUR-Lex/CURIA), conventions collectives (KALI), BOFIP, doctrine — designed-for but ingested only in Phase 3+.
- Answer synthesis, summarization, drafting (the agent's job).
- Multi-tenant SaaS / hosting (local-first single-user tool first).

### The seven pillars → components
| Foundation pillar (`00-foundation/search.md`) | Where it lives in this design |
|---|---|
| 1. Structure-aware hierarchical chunking | §6 Chunking |
| 2. Hybrid retrieval + legal vocabulary mapping | §7 Retrieval, §9 Vocabulary |
| 3. Temporal tracking / time-travel queries | §5 Data model, §12 Temporal |
| 4. Graph-RAG (legal knowledge graph) | §8 Graph layer |
| 5. Jurisdictional / "value" filtering | §7 Retrieval (filters + weighting) |
| 6. Strict citation verification & links | §10.5 Grounding & citation verification |
| 7. Multi-step agentic query expansion | §10 Agent contract (search→fetch loop) |

---

## 2. Design principles

1. **Agent-first ergonomics.** Every output is structured, stable, citeable, and *token-frugal by default*: concise-by-default with an opt-in `detailed` mode, pagination/cursors, actionable errors, natural-language identifiers alongside technical IDs. Because there is no MCP, **the interface contract lives in the binary itself** — `jurisearch help agent` and `jurisearch help schema --json` are the discovery surface (§10.4). General agent-ergonomics principles are sound regardless of harness; the interface is a POSIX-style CLI contract, not coupled to any one agent ecosystem. (See `RESEARCH.md §4`.)
2. **Two-step retrieval (search → fetch).** Cheap `search` returns IDs + citations + short snippets; a separate `fetch` returns full text only for the items the agent actually wants. This is the single biggest lever on context budget.
3. **Grounded, never inventive.** `jurisearch` only ever returns text that exists in the index, always with a stable ID and an official source URL. It exposes a `cite`/verify path so the agent can prove its own citations.
4. **Temporal-correct by construction.** Every statutory passage carries `valid_from/valid_to/status`. Queries can be pinned to a date; the default is "in force today".
5. **Structure-preserving.** Chunks never cross the natural legal hierarchy. Every chunk knows its ancestry (Code → Livre → Titre → Chapitre → Section → Article) or its decision zone (visa / moyens / motivations / dispositif).
6. **Local-first index + Rust runtime; embeddings via a configured endpoint.** The index (Postgres + vectors + BM25), and all filtering, ranking, and assembly run locally in Rust against a local index directory. The one query-time dependency is the **dense query embedding**, computed by the configured provider (D5): by default a **remote / OpenAI-compatible HTTP endpoint** — which may be a hosted API *or* a local self-hosted server (e.g. `llama.cpp` on loopback) for full privacy/offline. An optional in-process Rust embedder (`fastembed-rs`) remains available for single-binary offline setups. So "no third-party network at query time" is achievable (local endpoint or in-process) but is a *deployment choice*, not an absolute; the active provider/model fingerprint is recorded in the manifest and surfaced by `status` (§11.2, §13.1, §14). Rust startup is tiny; the JSONL session keeps the endpoint connection and index readers warm across calls.
7. **Offline-reproducible ingestion with a hard Rust/runtime boundary.** Built from official bulk dumps + APIs into versioned **canonical records**, fully re-buildable, with recorded provenance and coverage dates. Python may help *produce* canonical records offline; **Rust owns schema validation, index construction, and every query path** (§13.4).

### 2.1 Non-negotiable implementation constraints

The foundation assessment (`00-foundation/assessment.md`) stresses that the thesis is right but the *failure modes specific to French legal data* must be named and enforced. These are hard invariants — every component below is built to satisfy them, and the eval harness (§15) gates on them:

1. **Official-source provenance on every indexed object.** No chunk enters the index without a stable official ID and an official source URL. (§5, §16)
2. **Versioned statutory IDs + validity windows are mandatory, not optional.** Temporal metadata is the *core differentiator* (§12), so `valid_from/valid_to/status/version_group` are required fields on every statutory chunk — never best-effort. LEGI deliberately ships *modified* and *abrogated* versions next to in-force ones, so this is the difference between correct and dangerously-wrong answers.
3. **Corpus freshness + update provenance are recorded and surfaced.** `jurisearch status` reports dataset versions, coverage date ranges, and build date so the agent can caveat. (§14)
4. **Official decision zones/offsets before heuristic chunking.** Decisions are split on the publisher's zone offsets when present; regex splitting is a *fallback only*, and fragments are reassembled by zone identity because they can be non-sequential in the source text. (§6)
5. **Search and fetch stay separate** to bound agent token usage. (§2.2, §10)
6. **Strict citation verification over all identifier families** — Légifrance IDs (LEGIARTI/LEGITEXT/LEGISCTA), ECLI, pourvoi, NOR, CETATEXT. Every result carries stable IDs + an official URL; generated citations are re-resolvable via `cite` *before* the agent commits them. (§10.5)
7. **Evaluation uses real legal tasks**, not generic semantic similarity — *and*, under CLI-only, also tests CLI/JSON behaviour, help completeness, session mode, and exit codes. (§15)

---

## 3. High-level architecture

```
                       INGESTION (offline / scheduled)
  ┌──────────────────────────────────────────────────────────────────────┐
  │ Légifrance PISTE API   LEGI/JORF/KALI bulk    Judilibre API   Justice │
  │ (codes, lois, JORF)    dumps (XML)            (Cass)         Admin XML │
  └──────────────────────────────┬───────────────────────────────────────┘
                                  │  optional Python helpers for difficult
                                  │  parsing / API clients (official XML only)
                                  ▼
  ┌──────────────────────────────────────────────────────────────────────┐
  │ CANONICAL RECORDS  — JSONL / Parquet / Arrow, versioned schema (§5)    │
  │   the contract handoff: everything downstream is Rust-only             │
  └──────────────────────────────┬───────────────────────────────────────┘
                                  ▼  Rust schema validator + index builder
  ┌──────────────────────────────────────────────────────────────────────┐
  │ jurisearch-ingest (Rust)                                               │
  │   • validate canonical records  • structure/zone-aware chunking (§6)   │
  │   • detect citations → graph edges (§8)  • embed chunks  • manifest    │
  └──────────────────────────────┬───────────────────────────────────────┘
                                  ▼
  ╔══════════════════════════════════════════════════════════════════════╗
  ║ LOCAL INDEX ARTIFACT (single directory, §13.5)                        ║
  ║   embedded Postgres (docs · chunks · metadata · temporal · graph ·    ║
  ║   pgvector dense · pg_search/FTS lexical)  + manifest + schemas       ║
  ╚══════════════════════════════════════════╤═══════════════════════════╝
                                             ▼
  ┌──────────────────────────────────────────────────────────────────────┐
  │ jurisearch-core (Rust retrieval core, a library)                      │
  │   pre-filter (temporal/jurisdiction) → BM25 + dense → RRF fuse        │
  │   → optional rerank → authority re-weight → assemble                   │
  └──────────────────────────────┬───────────────────────────────────────┘
                                  ▼
  ┌──────────────────────────────────────────────────────────────────────┐
  │ jurisearch-cli (Rust, clap)                                            │
  │   one-shot commands · `--json` · JSONL `session` mode · inline help    │
  └──────────────────────────────┬───────────────────────────────────────┘
                                  ▼
                LLM HARNESS AGENT (spawns it as a subprocess)
```

**Key boundaries.**
- **Canonical records are the language boundary.** Optional Python may *produce* them; Rust validates and owns everything after. Rust tests prove canonical records can be indexed and searched with **no Python in the loop** (§13.4).
- **The retrieval core is a plain Rust library.** The CLI (one-shot + JSONL session) is the only adapter; the logical command set is identical in both modes.

---

## 4. Data sources & coverage

(Details and source URLs in `RESEARCH.md §1`.)

| Source | Content | Access | Format | IDs | Temporal | License |
|---|---|---|---|---|---|---|
| **LEGI bulk** (DILA / data.gouv.fr) | Codes, lois, règlements consolidés | Bulk dump + daily deltas | XML | `LEGIARTI`, `LEGITEXT`, `LEGISCTA` | `dateDebut`/`dateFin`, status `VIGUEUR/MODIFIE/ABROGE/ABROGE_DIFF` | Licence Ouverte (Etalab) |
| **Légifrance PISTE API** | Same as LEGI + JORF + targeted lookups | REST, OAuth2 client-credentials | JSON | same | versioned | Licence Ouverte |
| **JORF** | Journal officiel (texts as published) | Bulk + API | XML/JSON | `JORFTEXT`, `JORFARTI` | by publication date | Licence Ouverte |
| **Judilibre** (Cour de cassation) | Cass decisions; CA/first-instance rolling out | REST API + `/export` + `/transactionalhistory` | JSON | `ECLI`, pourvoi no. | decision date | Licence Ouverte (pseudonymised) |
| **Justice administrative open data** | Conseil d'État, CAA, TA decisions | Bulk + "open data" portal | XML | `CETATEXT`, `ECLI` | decision date | Licence Ouverte (pseudonymised) |
| **Derived HF corpora** (`AgentPublic/legi`) — *non-authoritative* | Pre-parsed/embedded LEGI; comparison & smoke-test fixture only | HF Datasets | Parquet/JSON | LEGI ids | varies | check per-dataset |

**Strategy (official-source-first, D7):** the authoritative build ingests **official DILA bulk XML** (LEGI, JORF, justice-administrative) plus **Judilibre**; no derived/third-party dataset feeds the authoritative index. The **APIs** (PISTE / Judilibre) are used for (a) incremental sync and (b) live citation confirmation of items not yet in the local index. `Judilibre /transactionalhistory` and LEGI daily deltas give clean incremental updates.

**Official-XML-first (D7).** The first and only authoritative ingestion path is the **official LEGI/DILA XML parser** (§13.4). It must preserve raw source identifiers, dates, hierarchy, status, cross-reference links, and source provenance, and emit canonical records from official XML only. Derived datasets (`AgentPublic/legi`, louisbrulenaudet `legalkit`) are **non-authoritative**: usable as comparison fixtures, regression baselines, or smoke-test data — **never** as a source of canonical records, chunks, or embeddings for a real index, and they must not influence the embedding-model decision (§13.4, D7/D14).

**Where Python is allowed:** official DILA XML parsing, existing API clients (e.g. `pylegifrance`), derived-dataset comparison/regression tooling (non-authoritative), and converting official dumps into **canonical JSONL/Parquet/Arrow**. That is the entire Python remit — it ends at the canonical-record boundary (§13.4).

---

## 5. Canonical data model

Everything normalizes to two record types plus graph edges. This is the **canonical-record schema** Python emits and Rust validates; it is versioned (`schema_version`) and shipped in the index `schemas/` directory.

### 5.1 `Document` (a code-article version, or a decision)
```
id              stable internal id, source-namespaced:
                  "legi:LEGIARTI000006419292@2016-10-01"   (statute version)
                  "judilibre:ECLI:FR:CCASS:2022:CO00123"   (Cass decision)
                  "ja:CETATEXT000045..."                   (administrative)
                  "ecli:..."                               (cross-court key)
                (the "jurisearch:" namespace is reserved for synthetic internal
                 objects only — never for source documents)
kind            record type: "article" | "decision"
                (the CLI filter `--kind code|decision|all` maps code ⇒ article
                 records, decision ⇒ decision records; mapping is documented in
                 `help schema --json`, the arbiter — §10.2)
canonical_cite  "Code civil, art. 1240"  |  "Cass. com., 14 sept. 2022, n° 21-12.345"
source_url      official Légifrance / Judilibre / justice-administrative URL
official_ids    { legi_id?, ecli?, nor?, pourvoi?, cetatext? }
title           human-readable, used as the agent-facing identifier
language        "fr"
-- statutory only --
hierarchy_path  ["Code civil","Livre III","Titre III","Chapitre II","Section …"]
valid_from      date  (dateDebut)
valid_to        date | null  (dateFin) — open-ended validity is normalized to null;
                source sentinels (e.g. AgentPublic/legi "2999-01-01", or missing
                dateFin) collapse to null; the raw source value is preserved in
                provenance/detailed (§12)
valid_to_raw    raw source value before normalization (provenance)
status          "VIGUEUR" | "MODIFIE" | "ABROGE" | "ABROGE_DIFF"
version_group   key linking all temporal versions of the same article
-- decision only --
court           "Cour de cassation" | "Conseil d'État" | "Cour d'appel" | "TA" | …
chamber         "Chambre commerciale" | "3e chambre civile" | …
formation       e.g. "Assemblée plénière"
decision_date   date
publication      normalized authority label: "B" (bulletin) | "P" | "Inédit" | … (§7)
publication_raw  raw Judilibre taxonomy keys, kept verbatim as an array:
                 e.g. ["b","r"]  (keys: b / r / l / c)  — normalize for display,
                 preserve raw for fidelity
solution         normalized: "Cassation" | "Rejet" | …   (+ solution_raw: raw key)
zones            TEXT-OFFSET RANGES ONLY — official publisher text delimitations,
                 each with character offsets into `text`:
                 { introduction, exposé/visa, moyens, motivations, dispositif,
                   moyens_annexes, summary (sommaire) }
                 — Judilibre/justice-admin offsets when present; fragments can be
                   NON-SEQUENTIAL, so they are keyed by zone identity, not by position.
                   (`motivations` ≡ the classic *motifs*.)
                 NOTE: rapprochements and applied texts are NOT zones (see below).
related_decisions publisher "rapprochements de jurisprudence" (decision ↔ decision).
                 Relationship metadata, NOT text → graph edges (§8), edge_source=publisher.
texts_applied    publisher applied/visa texts (decision → article). Relationship
                 metadata, NOT text → graph edges (§8), edge_source=publisher.
text             full normalized text
```

### 5.2 `Chunk` (the retrieval unit — see §6)
```
chunk_id, document_id
text, token_count
section_label       e.g. "Article 1240" or "Motivations"
hierarchy_path      inherited from document (denormalized for filtering)
valid_from/valid_to/status   inherited (denormalized for temporal filtering)
court/chamber/date/publication  inherited (decisions)
chunking            "structural" | "zone" | "heuristic"  (provenance of boundaries)
embedding           dense vector (bge-m3, pending benchmark)
sparse_terms        lexical field (FR-normalized; pg_search / FTS / Tantivy)
```

### 5.3 Graph edges (`§8`)
```
(src_id) --[rel]--> (dst_id)   rel ∈ { cites, interpreted_by, appeals,
                                       supersedes, applies_article,
                                       rapprochements, refers_to }
edge_source ∈ { publisher, inferred }   -- publisher links are trusted; regex
                                            -inferred links are flagged
```

**Design choice — IDs are natural where possible.** The agent-facing identifier is the human `title`/`canonical_cite` ("Code civil, art. 1240"); the opaque, **source-namespaced** technical IDs (`legi:…`, `judilibre:…`, `ja:…`) are present but secondary. `detailed` mode exposes both; `concise` leads with the citation. Natural identifiers reduce citation hallucination.

---

## 6. Structure-aware chunking (Pillar 1)

Chunking is content-type specific; **no chunk crosses a structural boundary.** It runs in `jurisearch-ingest` (Rust) over validated canonical records.

**Codes / statutes.** The atomic unit is the **article version**. One article = one (or few) chunks. Each chunk carries its full `hierarchy_path` so the agent sees scope ("…Section 2 — De la responsabilité du fait des choses") without a second call. Very long articles split on enumerations (`1°, 2°, …`) / alinéas, never mid-sentence; every sub-chunk repeats the article header + path. These chunks are tagged `chunking: structural`. (Chunks are always generated here from **official-XML** article-version records; chunks from any derived dataset — e.g. `AgentPublic/legi`'s 1024-char `RecursiveCharacterTextSplitter` splits — are **never** used in a real index, §4, §13.4.)

**Decisions.** Split on the publisher's **official zones, not regex** (assessment constraint §2.1.4). Judilibre exposes structured **text** zones — *introduction, visa, moyens, motivations, dispositif, moyens annexes, summary (sommaire)* — as **character offsets** into the full text (rapprochements and applied texts are *not* zones — they are relationship metadata, see below); justice-administrative decisions carry analogous parts (*considérants*, *par ces motifs*). Use those offsets as the primary chunk boundaries (`chunking: zone`). Two French-legal failure modes drive this:
- **Zone fragments can be non-sequential** in the raw text. Chunks are reassembled by *zone identity* (every fragment tagged `motivations` forms the motivations chunk) rather than by text position. Splitting/concatenating by position would interleave legal reasoning with procedural arguments or annexes.
- **Regex-only splitting is brittle** and is used *only as a fallback* when offsets are absent (e.g. older administrative decisions). When the fallback fires, the chunk records `chunking: heuristic` so the agent and the eval set know the boundaries are approximate.

The chunk's `section_label` records its zone, so the agent can target "reasoning" (`motivations`, ≡ classic *motifs*) vs "outcome" (`dispositif`) vs "procedural arguments" (`moyens`, `moyens annexes`). The **summary/sommaire** chunk (headnote, when present) is indexed separately and boosted for "what did this case decide" queries; **rapprochements** and **applied texts** are relationship metadata, not text zones — they are never chunked and instead populate the graph (§8, §5.1).

**Why not fixed 512-token chunks:** legal scope is inherited from ancestry; arbitrary splits destroy it (the central complaint in `00-foundation`). Structural chunks also make citations exact (`art. 1240` vs "somewhere in this 512-token blob").

Each chunk is embedded with a **contextualized prefix** (path + title prepended before embedding, stripped from returned text) so the dense vector encodes the section scope, not just the bare sentence.

---

## 7. Indexing & retrieval engine (Pillars 2 & 5)

### 7.1 Three indices over the same chunks
- **Lexical / sparse (BM25):** French-aware analyzer — lowercasing, accent folding handled carefully (preserve where legally meaningful), **elision** splitting (`l'`, `d'`, `qu'`), light stemming/lemmatization, and a **legal stopword/booster** list. Critical for exact statutory references ("article 1240", "L. 217-4") and rare legal terms. Implemented via **`pg_search`/ParadeDB** (Tantivy/BM25 inside Postgres) as the **selected lexical engine**, with **native Postgres FTS** and **standalone Tantivy** as hard-failure fallbacks only (§13).
- **Dense / semantic:** `bge-m3` embeddings (multilingual, strong FR, 8k context, single model yields dense + learned-sparse), stored and searched with **`pgvector`**. Captures conceptual queries ("rupture de contrat injustifiée" → "rupture brutale des relations commerciales établies"). **`bge-m3` is the benchmark-default, not an assumption** — it must be validated on the legal eval set (§15) against French specialists (`sentence-camembert-large`, Solon) before being locked in (D4). **Both query and document embeddings are produced by the configured embeddings endpoint** (default remote / OpenAI-compatible, including a local `llama.cpp` server; in-process Rust `fastembed-rs` optional) — using the **same provider/model fingerprint**, recorded in the manifest, with a hard dimension check (D5, §11.2, §13.1).
- **Metadata + temporal columns:** `kind`, `court`, `chamber`, `publication`, `hierarchy_path`, `valid_from/valid_to/status`, `decision_date` — Postgres columns/indexes, filterable as pre-filters (applied *before* fusion so temporal/jurisdiction constraints are exact, not best-effort).

### 7.2 Retrieval pipeline
```
query
  └─(optional) legal vocabulary expansion (§9)
  └─ pre-filter: kind / court / chamber / as-of date / code  (exact metadata)
  └─ run BM25 top-N  +  dense top-N  (parallel)
  └─ FUSE with Reciprocal Rank Fusion (RRF)        ← custom Rust, rank-based
  └─ (benchmark-gated) RERANK fused top-K with a local cross-encoder
  └─ AUTHORITY re-weight (jurisdiction value, §7.3)  ← custom Rust layer
  └─ assemble results (citation, snippet, ids, validity)
```
**Why RRF as the default fuser:** dense (cosine) and BM25 scores live on different, query-shifting scales; RRF fuses by *rank*, avoiding fragile per-query normalization. RRF is implemented as a **custom Rust layer** so it is identical across whichever backend wins the spike. Optional normalized-weighted fusion (`alpha`) is available for tuning. (`RESEARCH.md §3`.)

**Reranking is a benchmark-gated ranking component, not a roadmap nice-to-have (direction-lock P1).** A cross-encoder reads query+passage jointly and fixes ordering errors bi-encoders make on legal nuance. It is neither assumed nor deferred by default: a release may ship **without** it **only if** evals prove hybrid+authority already meets the best-in-class quality bar (§15); and if reranking materially improves legal recall / nDCG / citation-exactness **within the latency budget**, it ships **before** the first best-in-class (Phase 1) release. Either way the eval gate decides, after a Rust inference spike (model availability, tokenizer behaviour, ONNX/Candle compatibility, latency, packaging). The reranker (`bge-reranker-v2-m3` first candidate — multilingual, BGE-aligned — not hardwired) runs only on the fused top-K (e.g. 50→keep 8), so cost stays bounded.

**Provider abstraction (like embeddings).** The reranker is pluggable — `reranker.provider = "disabled" | "local" | "http"` (local cross-encoder via `ort`/Candle, or an HTTP rerank endpoint) — so the quality bar is **never blocked by Rust inference packaging**: if local inference is the only blocker but the quality gain is large, ship the **HTTP** provider rather than dropping reranking from a best-in-class release. **Adoption gate (§15):** on fused top-K only (e.g. 50→8), require a *meaningful* improvement over hybrid+authority on recall@k, nDCG, citation-exactness, and stale-citation handling, within the latency + token/call budget; a deferral to Phase 3 must record the eval result.

### 7.3 Jurisdictional "value" weighting (Pillar 5)
A configurable authority prior nudges (does not hard-filter) ranking, implemented as a custom Rust scoring layer:
- Court tier: `Cour de cassation` (esp. *Assemblée plénière* / *chambre mixte*) and `Conseil d'État` > Cour d'appel > TJ/TA.
- Publication level: bulletin `B`/`P` > `Inédit`.
- Recency prior for jurisprudence (mild), tunable.

This is **a ranking signal, not a gate** — the agent can ask for `--court "Cour d'appel"` regional trends and the prior steps aside. Weights live in config (§14) and are set by the eval set (§15), never hard-coded magic numbers in hot paths.

---

## 8. Legal knowledge graph layer (Pillar 4 — Graph-RAG)

A lightweight graph built at ingestion from detected cross-references, stored as Postgres tables in the same index artifact.

- **Nodes:** articles, decisions (and later: directives, doctrine).
- **Edges:** `applies_article` (decision → article), `interpreted_by` (article → decision), `cites` (decision → decision / article), `appeals` (decision → lower decision), `supersedes` (article version → version), `rapprochements` (decision ↔ decision — the publisher's own "rapprochements de jurisprudence", taken directly from Judilibre metadata, not inferred).
- **Extraction:** a Rust citation parser for French citation forms (`art. 1240`, `L. 217-4`, `n° 21-12.345`, ECLI) yields *inferred* links, **plus the explicit publisher relationship fields** from the canonical record (§5.1) — `texts_applied` → `applies_article` edges and `related_decisions` (rapprochements) → `rapprochements` edges. Publisher-provided links are trusted (`edge_source: publisher`); regex-inferred links are flagged (`edge_source: inferred`) so the agent can weight them.

**Agent value (`jurisearch related <id>`):** from a relevant article, surface the *candidate* attached jurisprudence; from a decision, walk the appeal chain, follow the publisher's *rapprochements*, or pull the articles it applies. This turns flat search into one-hop legal-reasoning support without the agent issuing many blind searches.

**What the graph must NOT do (assessment constraint).** It does not, on its own, assert *jurisprudence constante*. A settled-law claim requires authority ranking (§7.3), publication level, recency, citation frequency, and a human-checkable source set — so `related` returns **ranked candidate** decisions carrying those signals, never a bare "this is settled law" verdict. The graph surfaces the links; the agent, with the authority signals, draws the conclusion.

**Storage:** edge tables live in the same embedded Postgres (no separate graph DB at this scale); traversal is bounded (1–2 hops) and returns the same `Document` summaries as search.

---

## 9. Legal vocabulary mapping / query expansion (Pillar 2)

An optional pre-retrieval step mapping lay language → formal legal terminology and synonyms:
- "virer un employé" → "licenciement"; "annuler un contrat" → "résolution"/"nullité"/"résiliation" (context-dependent → expand to all, let ranking decide).
- Maintained as a small curated lexicon (seed) + optional LLM-assisted expansion done **offline** during ingestion, applied to the **BM25 leg only** (the dense leg already handles paraphrase). No network call at query time.

Exposed two ways: (a) automatic behind `search` (`--expand`), and (b) explicit `jurisearch expand "<query>"` so the *agent* can inspect/choose terms — supporting the agentic loop in Pillar 7. Expansion is conservative and logged in results (`expanded_terms`) for transparency/grounding.

---

## 10. The agent contract — CLI command surface

This is the heart of the tool: a **stable, documented, token-frugal interface** an LLM can drive, **entirely over the CLI** (one-shot or JSONL session). With no MCP, the CLI is the API and its **inline help is part of that API** (§10.4). All commands accept `--json` (default for non-TTY) and `--format concise|detailed`.

### 10.1 Commands

| Command | Purpose | Key flags |
|---|---|---|
| `jurisearch search "<query>"` | Hybrid ranked search. Returns IDs + citation + snippet (concise). | `--kind code\|decision\|all` `--court` `--chamber` `--code "Code civil"` `--as-of YYYY-MM-DD` `--date-from/--date-to` `--top-k` `--cursor` `--expand` `--format` |
| `jurisearch fetch <id…>` | Full text of one/more documents by stable ID. Step 2 of search→fetch. | `--part motivations\|dispositif\|moyens\|visa\|summary\|…` (zone names per §6; `motifs` accepted as an alias for `motivations`) `--with-context` (parent section / siblings) `--as-of` |
| `jurisearch cite <id \| "free-text citation">` | **Verify/resolve** a citation: confirm it exists, return canonical form + official URL, flag mismatches. | `--strict` `--as-of` |
| `jurisearch related <id>` | Graph traversal (Pillar 4). Returns **ranked candidate** neighbours with authority signals — never a settled-law verdict (§8). | `--rel cites\|interpreted-by\|appeals\|applies-article\|rapprochements` `--depth 1\|2` |
| `jurisearch context <id>` | Structural neighborhood: ancestry path + sibling articles (codes) or other zones (decisions). | `--up` `--siblings` |
| `jurisearch expand "<query>"` | Return legal-terminology expansion of a query (Pillar 2/9). | `--max-terms` |
| `jurisearch status` | Coverage report: corpora indexed, date ranges, freshness, build provenance, model/tokenizer/schema versions, **and missing local models**. | `--json` |
| `jurisearch model fetch [<model>]` / `jurisearch setup` | Pre-fetch & cache local embedding/reranker models so query time stays fully offline (§11 model-cache rule). | `--allow-download` |
| `jurisearch help agent` | The complete agent-facing contract in one call (replaces MCP tool descriptions, §10.4). | |
| `jurisearch help schema [--json]` | Machine-readable request/response/error JSON schemas for every command. | `--json` |
| `jurisearch session` | **Warm multi-call mode.** One process, models/index loaded once; reads one JSON command per line on stdin, writes one JSON response per line on stdout (§11). | `--jsonl` |
| `jurisearch batch` | Same JSONL protocol over a finite input file/stream, then exits. | `--jsonl` |
| `jurisearch ingest …` / `jurisearch sync` | Admin: build / incrementally update the index from canonical records or sources. | `ingest canonical <path>` · `ingest legi <path>` · `sync --source legi\|judilibre\|ja --since` |

Admin commands exist but are kept out of the agent's way; the agent-facing core is `search / fetch / cite / related / context / status / help`.

### 10.2 Output schema (stable, versioned)

`search` (concise) — minimal context cost:
```json
{
  "schema_version": "1",
  "query": "responsabilité du fait des choses",
  "as_of": "2026-06-20",
  "expanded_terms": ["garde de la chose", "article 1242"],
  "count": 8,
  "next_cursor": "eyJvZmZzZXQiOjh9",
  "results": [
    {
      "id": "legi:LEGIARTI000006419292@2016-10-01",
      "cite": "Code civil, art. 1242",
      "kind": "article",
      "path": "Code civil › Livre III › Titre III › Ss-titre II › Ch. II",
      "valid": { "from": "2016-10-01", "to": null, "status": "VIGUEUR" },
      "snippet": "On est responsable … du fait des choses que l'on a sous sa garde…",
      "score": 0.83,
      "source_url": "https://www.legifrance.gouv.fr/codes/article_lc/LEGIARTI000006419292"
    }
  ]
}
```
`detailed` adds: official IDs (LEGIARTI/ECLI/NOR), full hierarchy array, court/chamber/formation/publication/solution, all temporal versions (`version_group`), and graph edge counts.

**Conventions:**
- **Concise default**, `detailed` opt-in. Response capped (~25k tokens) with truncation notice + `next_cursor`; truncation messages steer the agent toward narrower queries.
- **Natural-language identifier first** (`cite`), opaque source-namespaced IDs secondary.
- **`kind` vocabulary mapping (documented, eval-enforced):** the CLI filter flag is `--kind code|decision|all`; the per-result field is `kind: "article" | "decision"`. The mapping is `code ⇒ article`, `decision ⇒ decision`, `all ⇒ both`. `help schema --json` is the final arbiter and both vocabularies appear there so an agent never has to guess.
- **`valid_to: null` is the in-force sentinel** (open-ended validity). As-of interval semantics are exactly `valid_from <= as_of && (valid_to is null || as_of < valid_to)` (§12); the raw pre-normalization value is exposed only in `detailed`/provenance.
- **`--json` discipline:** when `--json` is set, **stdout carries only valid JSON**; all diagnostics go to **stderr**. (Eval-enforced, §15.)
- **Actionable errors** with examples, not bare codes:
  ```json
  { "error": "no_results",
    "message": "No in-force article matched. Try --as-of for a historical version, or widen with --kind all.",
    "suggestions": ["jurisearch search \"...\" --kind all", "jurisearch search \"...\" --as-of 2015-06-01"] }
  ```
- **Deterministic ordering**; stable exit codes (`0` ok, `2` no-results, `3` bad-input, `4` index-missing, `5` upstream/API).
- Each `search` response is **self-describing enough to drive the next call** (cursor, expanded terms, suggested refinements) — supports Pillar 7's agentic loop without hidden state.

### 10.3 The canonical agent loop (Pillar 7)
```
1. jurisearch search "<facts>" --kind code                 → framework (e.g. art. L.217-4)
2. jurisearch related legi:LEGIARTI… --rel interpreted-by   → attached jurisprudence
3. jurisearch search "<refined>" --court "Cour de cassation" --date-from 2022-01-01
4. jurisearch fetch <ids> --part motivations                → full reasoning for synthesis
5. jurisearch cite "Cass. com., 14 sept. 2022, n°…"         → verify before the agent writes it
```
In a session, the same five steps are one JSON line each on stdin (§11) — no per-call cold start.

### 10.4 Complete inline help (the discovery contract)

Because there is no MCP, the agent cannot rely on external tool descriptions. **The help surface is the contract** and must be self-contained and complete (review P0):

- `jurisearch --help` — concise overview, command list, global flags, a few examples, and a pointer to `help agent`.
- `jurisearch <command> --help` — that command's complete flags, defaults, accepted enum values, examples, JSON output shape, exit codes, and common errors.
- `jurisearch help agent` — the **entire** agent-facing contract in one call: command inventory; when to use `search` vs `fetch` vs `cite` vs `related`; all flags and accepted enum values; JSON request/response examples; pagination/cursor rules; temporal-query rules; the citation-verification workflow; exit codes; the error-object schema; and token-budget guidance.
- `jurisearch help schema --json` — machine-readable JSON schemas for every command's request, response, and error objects.

**Help (and schema) must work without an index.** Because inline help is the discovery surface, `--help`, `help agent`, and `help schema --json` are **compiled into the binary** and answerable **before any corpus is installed** (no `index-missing` error). The same schema files are also copied into the index `schemas/` dir for provenance; `help schema --json` reads the compiled-in copy by default and additionally reports the **active index schema version** when an index is present.

This help text is **part of the agent API**, not optional documentation: the eval harness asserts it covers every command, flag, enum, and schema, and that it works with no index (§15).

### 10.5 Grounding & citation verification (`cite`) — Pillar 6

Citation grounding is a first-class contract, not a side effect of search (the central anti-hallucination guarantee, invariant §2.1.6). `jurisearch cite` resolves a citation and tells the agent *exactly* how confident it can be before it commits the citation to prose.

**Accepted inputs.** Internal IDs (`legi:…`, `judilibre:…`, `ja:…`); official IDs — `LEGIARTI`/`LEGITEXT`/`LEGISCTA`, `ECLI`, pourvoi number, `NOR`, `CETATEXT`; and **free-text citations** (`"Cass. com., 14 sept. 2022, n° 21-12.345"`, `"article 1240 du Code civil"`), parsed by the same citation parser used for graph extraction (§8).

**Match result states** (`status` in the response):
| state | meaning |
|---|---|
| `exact` | resolves to one document; canonical form matches input. |
| `normalized` | resolves to one document after normalizing format/abbreviations; the corrected canonical form is returned. |
| `ambiguous` | matches >1 document (e.g. an article number without a code, or a pourvoi shared across links); `candidates[]` is returned for disambiguation. |
| `stale_version` | the cited text exists but not for the requested/implied date (e.g. a pre-reform article cited as current); the in-force/as-of version is suggested. |
| `not_found` | no document matches in the local index (and, if allowed, not upstream either). |
| `source_unavailable` | resolved an ID but the official source/version could not be confirmed (upstream offline, or local-only mode). |

**Local vs official-source checks.** `cite` resolves against the **local index by default** (offline, deterministic). With `--online` it may additionally confirm existence/text against the official API (PISTE/Judilibre) for items not in the local index or to upgrade a `source_unavailable` to confirmed — never silently; offline stays the default for confidential work.

**`--strict`** turns leniency off: only `exact`/`normalized` count as verified (exit `0`); `ambiguous`, `stale_version`, `not_found`, `source_unavailable` are treated as **failure** (exit `2`) with suggestions. This is the mode the grounding eval drives (§15).

**`--as-of` interaction.** A date (explicit `--as-of`, or implied by a dated citation like "14 sept. 2022") selects the version to verify against. Citing today's text for a 2015 dispute returns `stale_version` with the correct 2015 version, satisfying the temporal invariant (§12).

**Response schema** (concise):
```json
{ "schema_version": "1",
  "input": "article 1240 du code civil",
  "status": "normalized",
  "verified": true,
  "as_of": "2026-06-20",
  "resolved": {
    "id": "legi:LEGIARTI000006419292@2016-10-01",
    "cite": "Code civil, art. 1240",
    "valid": { "from": "2016-10-01", "to": null, "status": "VIGUEUR" },
    "source_url": "https://www.legifrance.gouv.fr/codes/article_lc/LEGIARTI000006419292",
    "checked_against": "local"
  },
  "candidates": [] }
```
A failure (`--strict`, fabricated citation) returns `"verified": false`, the `status`, an actionable `message`, and `suggestions[]` — and exit `2`.

---

## 11. Serving model & latency (CLI-only)

LLM harnesses subprocess tools repeatedly; cold-start latency would dominate. The review removes MCP/HTTP/`serve`, so latency is handled within the CLI shape:

1. **One-shot CLI** (`jurisearch search …`) — loads the index reader, calls the configured embeddings endpoint for the query vector, answers, exits. Rust keeps **binary startup tiny** and index reads memory-mapped, so the dominant per-call cost is the **embeddings round-trip** (and the reranker, if enabled) — see §11.2.
2. **JSONL session** (`jurisearch session --jsonl`) — **the recommended mode for agent loops.** One process: loads index readers **once** and keeps the **embeddings-endpoint connection** (and any optional in-process model / reranker) warm, then reads one JSON command per line on stdin and writes one JSON response per line on stdout, for the same logical commands (`search`, `fetch`, `cite`, `related`, `context`, `status`, `help`). Exits cleanly on `{ "command": "exit" }`. Still a plain subprocess/stdio interface — **not** MCP or HTTP.
3. **`batch --jsonl`** — same protocol over a finite stream, then exit; handy for eval runs and bulk verification.

**The latency the old `serve` used to hide.** Removing `serve` means a naïve one-shot CLI re-pays per call: an in-process embedder would reload the model, and even an endpoint adds connection/handshake cost each invocation. Rust shrinks binary/index startup; the **session** mode amortizes the embeddings connection (and any warm in-process model/reranker). Spike target (§13.3): **stable JSON under 500 ms warm** for common queries.

**Query embeddings (endpoint-first, D5).** The dense leg embeds only the *query* (tiny). By default this is an HTTP call to a configured **OpenAI-compatible `/v1/embeddings`** endpoint — a hosted API, or a local self-hosted server such as `llama.cpp` on loopback for privacy/offline. An in-process Rust embedder (`fastembed-rs`) is an optional fallback for single-binary offline use. **Document and query embeddings must use the same provider + model fingerprint** (recorded in the manifest); a mismatch is a hard error (§11.2). Document embeddings are precomputed at ingestion through the same provider.

### 11.1 Session wire protocol (JSONL)

The session/batch protocol is a fixed contract, not just prose. One JSON object per line on stdin; one per line on stdout.

**Request envelope:**
```json
{"id":"req-1","command":"search","args":{"query":"article 1240","kind":"code","top_k":5}}
```
**Response envelope:**
```json
{"id":"req-1","ok":true,"result":{ /* same payload as the one-shot command */ }}
{"id":"req-2","ok":false,"error":{"code":"bad_input","message":"…","suggestions":[]}}
```

Rules:
- **Order preserved:** responses come back in input order unless a future explicit parallelism mode opts out; the `id` is echoed so a client can correlate regardless.
- **stdout is JSONL only**, one object per line; all diagnostics/logs go to **stderr**.
- **Every agent command works in a session** — `search`, `fetch`, `cite`, `related`, `context`, `expand`, `status`, **and** `help` / `help schema` (so discovery works without leaving the session).
- **Malformed input is non-fatal:** an unparseable line yields a JSONL error object (`{"id":null,"ok":false,"error":{"code":"bad_input",…}}`) and the process keeps running; a `--fatal` mode can opt into exit-on-error instead.
- **`exit`:** `{"command":"exit"}` emits a final `{"ok":true,"result":{"bye":true}}` acknowledgement, then the process closes stdout and exits `0`. (Documented no-ambiguity behaviour rather than silent close.)
- Exit codes (`0/2/3/4/5`) apply to the **process**; per-request failures are carried in the `error` object with the same `code` vocabulary.

### 11.2 Embeddings provider readiness & fingerprint

Whatever the provider, the embedding used at query time must match the one baked into the index, and the tool must **fail fast** rather than return silently-wrong vectors.

**Endpoint mode (default, D5).**
- Speak OpenAI-style `POST /v1/embeddings`; require a **dedicated embedding model**, not a chat model casually used as an embedder.
- Record provider, `base_url` host class, **model**, **vector dimension**, normalization, and pooling mode in the index **manifest** (the *fingerprint*).
- **Hard dimension/fingerprint check:** if the endpoint returns a vector whose dimension (or model fingerprint) differs from the index, `search` fails with an actionable error — never degraded results. Re-embedding requires an explicit index migration declared in the manifest.
- Treat a local endpoint as a **"remote provider" from the CLI's perspective** even on `127.0.0.1`; `status` reports endpoint reachability. (`llama.cpp` profile + config in §14.)

**In-process mode (optional, offline single-binary).** `fastembed-rs` downloads a model on first use, so to keep offline honest: `jurisearch model fetch [<model>]` / `setup` pre-fetches it (the only commands allowed to download), `status` reports missing models, and `search`/`session` **fail with an actionable error** rather than download implicitly — unless `--allow-download` is passed.

---

## 12. Temporal / time-travel queries (Pillar 3)

**This is the core differentiator (assessment).** LEGI deliberately keeps *modified* and *abrogated* versions alongside texts in force, so temporal correctness — not hybrid retrieval or the graph — is what separates `jurisearch` from a thin API wrapper. It is a hard invariant (§2.1), not an enhancement. The binding requirement: **every statutory hit must answer "which version, valid when, from which official source?"** — satisfied by the `valid` block + `source_url` returned on every result (§10.2).

- Every statutory chunk carries `valid_from/valid_to/status`; all temporal versions share a `version_group`.
- `--as-of <date>` adds an exact pre-filter with explicit null-handling: `valid_from <= as_of && (valid_to is null || as_of < valid_to)` → returns the article **as it stood then**. `valid_to: null` is the in-force/open-ended sentinel. Default `--as-of today` returns only `VIGUEUR`.
- **Sentinel normalization:** open-ended validity from any source (a missing `dateFin`, or sentinels such as `AgentPublic/legi`'s `2999-01-01`) is normalized to `valid_to: null` at ingestion; the raw value is preserved in provenance (`valid_to_raw`, §5.1). This stops a `2999-01-01` sentinel from leaking into range math or display.
- **Edge cases are eval-tested (§15):** reform-date boundaries (e.g. the 2016 contract-law reform), same-day version changes, and back-to-back versions sharing a `version_group`.
- `jurisearch fetch <article> --as-of 2015-06-01` returns the pre-2016-reform text; `jurisearch context <id> --siblings --as-of …` reconstructs the surrounding section at that date.
- Decisions are dated, not versioned; temporal filters on jurisprudence use `decision_date` ranges.
- This directly serves the foundation's example: arguing a 2015 case needs 2015 law, not today's.

---

## 13. Tech stack recommendation

**Decided stack: a Rust search/runtime core over embedded Postgres + `pgvector` + `pg_search`, with endpoint-based embeddings; Python only as offline ingestion tooling.** (Supersedes the earlier "Python core + LanceDB" idea.) Rust suits a CLI-only, subprocess-spawned tool (tiny startup, easy distribution, one language owning every query path); the backend and the embeddings provider are **no longer open choices** (D3/D5) — the spike *validates* them (§13.3), it does not re-pick.

### 13.1 Runtime stack (Rust)
| Concern | Choice (v1) | Why / alternatives |
|---|---|---|
| Language (runtime/search) | **Rust** | Tiny startup for subprocess use; single static-ish binary; one language owns all query paths. |
| CLI / help | **`clap`** | Subcommands, value enums, generated + custom help, completions — backs the inline-help contract (§10.4). |
| Storage substrate | **Embedded Postgres** (`pg-embed` / `postgresql_embedded`) | One local relational store for documents, chunks, metadata, temporal filters, graph edges, manifests, eval traces — started as a local process by the Rust app. Gated by the spike (§13.3). |
| Dense vectors | **`pgvector`** on embedded Postgres | Exact/approx NN inside the same store. Alt (no-Postgres path): a local Rust vector index. |
| Lexical / BM25 | **`pg_search`/ParadeDB** (selected) | Tantivy/BM25 inside Postgres; AGPL-3.0 accepted (D-licensing). Hard-failure fallbacks: **native Postgres FTS**, **standalone Tantivy** (max FR-tokenizer control). |
| Query + doc embeddings | **OpenAI-compatible HTTP endpoint** (default, D5) | Hosted API *or* local self-hosted `llama.cpp` on loopback (§14). Same provider/model fingerprint for docs + queries; hard dimension check (§11.2). Optional offline: in-process **`fastembed-rs`** (`ort`/Candle). |
| Fusion | **custom Rust RRF** | Backend-independent; identical regardless of store. |
| Reranker | **benchmark-gated, pluggable** (§7.2) | `provider = disabled\|local\|http`: local cross-encoder via `ort`/Candle, or an HTTP rerank endpoint if quality justifies and local packaging lags. `bge-reranker-v2-m3` first candidate. |
| Authority scoring | **custom Rust layer** | Config-driven weights (§14), set by eval (§15). |
| Metadata / graph | **Postgres tables** | Same store; no separate graph DB. |
| Ingestion helpers | **optional Python** (`tools/ingest-python`) | Official DILA/LEGI XML parsing + API clients → canonical records only; never at query time (§13.4). |

**"Embedded Postgres" = a managed local Postgres *process*, not an in-process SQLite-like library.** `pg-embed`/`postgresql_embedded` download-and-cache or bundle real Postgres binaries and run them as a **child process** bound to a Unix socket / ephemeral loopback port. That carries packaging and lifecycle consequences (process supervision, locking, clean shutdown) — enumerated as spike criteria (§13.3). If they prove too heavy, the **no-Postgres fallback** (Tantivy + a local Rust vector index + SQLite/Arrow metadata) is the genuinely in-process path.

### 13.2 Crate layout
- `jurisearch-core` — Rust retrieval core (library): pre-filter, BM25 + dense candidates, RRF, authority, assembly.
- `jurisearch-cli` — Rust CLI over the core: one-shot, `--json`, JSONL `session`/`batch`, inline help.
- `jurisearch-ingest` — Rust schema validator + index builder (chunking, embedding, graph, manifest).
- `tools/ingest-python` — *optional* offline ingestion/conversion helpers; never on a query path.

### 13.3 Backend validation spike (the backend is decided — D3)
The backend **is** embedded Postgres + `pgvector` + `pg_search` (Postgres tables hold documents, chunks, metadata, temporal columns, graph edges, manifest, eval traces). The spike **validates** that one stack against hard acceptance criteria **before** building features — it does **not** re-choose among peers, and it must not reopen the product decision unless the chosen stack fails a hard criterion. Indicative target dataset: **50k LEGI article versions + 10k Judilibre decisions**; pipeline: FR-tokenized BM25 (`pg_search`) + dense (`pgvector`) candidates → **temporal pre-filter → custom-RRF → authority post-scoring → stable JSON under 500 ms warm**.

**Fallbacks — engaged only on a hard failure, in this precedence:** (1) native Postgres FTS **if `pg_search` packaging fails**; (2) standalone Tantivy + a local Rust vector index + SQLite/Arrow **if embedded Postgres itself fails**; (3) LanceDB (Rust SDK) **only if the Postgres route fails both packaging *and* quality gates**. These are documented fallbacks, **not active design options**. **Qdrant stays out of scope** — a separate service conflicts with the embedded CLI shape.

**Backend spike acceptance criteria** (packaging *and* quality):
- **Postgres binaries:** bundled vs downloaded-and-cached; download size and offline-install story.
- **Extension versions:** pinned `pgvector` and `pg_search` builds compatible with the bundled Postgres, and how they are installed/migrated into the data dir.
- **Binding:** Unix socket or ephemeral loopback port, with **no public network exposure by default.**
- **Concurrency:** single-writer locking across simultaneous `jurisearch` processes against one index.
- **Lifecycle:** startup, crash recovery, and clean shutdown of the child Postgres (no orphaned processes / stale locks).
- **Upgrades:** index / schema / extension migration story across `jurisearch` versions.
- **Platforms:** cross-platform target policy (documented even if v1 ships Linux-only).
- **Warm query latency:** stable JSON < 500 ms for common queries.
- **Temporal prefilter behaviour:** correct, fast `as-of` filtering at corpus scale.
- **BM25 quality:** French legal tokenization (elision, accents, statutory references) via `pg_search`.
- **Hybrid fusion quality:** custom RRF over `pg_search` + `pgvector` meets the eval bar (§15).

### 13.4 Ingestion ⇄ runtime boundary (hard rule)
- Python **may** download, parse, inspect, and normalize official sources, and emit **canonical JSONL/Parquet/Arrow** records.
- Rust **owns** schema validation, index construction, manifest generation, and **all** query execution.
- A Rust test asserts that canonical records can be indexed and searched **with no Python in the loop** — this keeps Python from leaking into the runtime.
- If ingestion stays partly Python, the CLI still documents the supported **import artifact** (`jurisearch ingest canonical <path>`) so agents/operators never need to know the Python tooling.
- **Official LEGI/DILA XML is the only authoritative ingestion path (D7).** The XML parser preserves raw IDs, dates, hierarchy, status, cross-reference links, and provenance, and emits canonical records from official XML only. Derived datasets (`AgentPublic/legi`, `legalkit`) are **non-authoritative** — comparison / regression / smoke-test fixtures only, **never** a source of canonical records, chunks, or embeddings for a real index (§4).

### 13.5 Index artifact layout
```
index/
  manifest.json     # source dataset versions, build date, corpus coverage,
                    # model + tokenizer + schema versions
  pg/               # embedded Postgres data directory (if the Postgres path wins)
  lexical/          # standalone Tantivy index (only if not using pg_search/FTS)
  vectors/          # external vector index (only if not using pgvector)
  docs/             # canonical stored documents/chunks (if not stored in Postgres)
  graph/            # edge tables (if not stored in Postgres)
  schemas/          # versioned JSON schemas — a PROVENANCE COPY (the authoritative
                    # schemas are compiled into the binary, so `help schema --json`
                    # works with no index; §10.4)
```
`pg/` is a real Postgres **data directory** managed by a child process (§13.1), not a single embedded file. Everything under `index/` is reproducible from canonical records + the manifest.

---

## 14. Configuration & secrets

- **Config file** `~/.config/jurisearch/config.toml`: index path, default `--format`, the **embeddings provider** (below), reranker on/off, authority weights (§7.3), corpora to ingest.
- **Embeddings provider (endpoint-first, D5).** Default is an OpenAI-compatible HTTP endpoint; the same block points at a hosted API *or* a local self-hosted server (`llama.cpp`). Model, dimension, normalization, and pooling are pinned and written to the index fingerprint (§11.2):
  ```toml
  [embedding]
  provider  = "openai_compatible"        # default; "in_process" for offline single-binary
  base_url  = "http://127.0.0.1:8080/v1" # llama.cpp on loopback, or a hosted API URL
  model     = "bge-m3"                    # a DEDICATED embedding model, not a chat model
  api_key   = "no-key"                    # or via JURISEARCH_EMBED_API_KEY
  dimension = 1024                        # validated against the index; mismatch = hard error
  normalize = true
  pooling   = "mean"                      # llama.cpp requires a pooling mode other than "none"
  ```
  A local endpoint on `127.0.0.1` is still treated as a **"remote provider"** by the CLI (§11.2).
- **Reranker provider (benchmark-gated, §7.2).** Pluggable like embeddings — local or HTTP, so the quality bar is never blocked by local inference packaging:
  ```toml
  [reranker]
  provider = "disabled"                       # "local" (ort/Candle) | "http" (rerank endpoint)
  model    = "bge-reranker-v2-m3"
  base_url = "http://127.0.0.1:8081/rerank"   # only for provider = "http"
  top_k    = 50                               # rerank the fused top-K, keep ~8
  ```
  Adopted only if it clears the §15 adoption gate; the result (adopt now / defer to Phase 3) is recorded.
- **Secrets** via env / OS keyring, never committed to disk:
  - `PISTE_CLIENT_ID` / `PISTE_CLIENT_SECRET` (Légifrance + Judilibre OAuth2 client-credentials) — used by ingestion/sync only.
  - Optional `JURISEARCH_EMBED_API_KEY` for the embeddings endpoint (omit / `no-key` for local `llama.cpp`).
- **Env prefix** `JURISEARCH_` for all environment overrides (e.g. `JURISEARCH_INDEX_PATH`, `JURISEARCH_FORMAT`, `JURISEARCH_EMBED_BASE_URL`, `JURISEARCH_MODEL_DIR`).
- **Local model cache (in-process mode only):** when `provider = "in_process"` (or a local reranker runs), models live in `JURISEARCH_MODEL_DIR` (default `~/.cache/jurisearch/models`), populated by `jurisearch model fetch`. Query-time downloads are **off by default** (`--allow-download` to opt in); `status` flags any configured-but-missing model (§11.2).
- **Provenance:** the index `manifest.json` records source dataset versions, build date, coverage, schema version, and the **embeddings fingerprint** (provider, model, dimension, normalization, pooling), surfaced by `jurisearch status` (so the agent knows corpus freshness/coverage and can caveat answers).

---

## 15. Evaluation harness (quality gate)

Tools are only as good as their eval, and "best-in-class" needs measurable gates, not a slogan. Build a **real, production-grade French-legal eval set** from day one (not a sample), test the **CLI contract** itself, and hold every release to the explicit **best-in-class acceptance gates** below:

**Retrieval quality.**
- **Tasks:** realistic multi-call agent workflows: "find the framework article for hidden defects in online sales, then the 2022+ Cass. jurisprudence applying it." Gold = expected article IDs / ECLIs.
- **Metrics:** recall@k, nDCG, citation-exactness, *and* agent-centric metrics (tool-call count, tokens consumed, end-to-end latency).
- **Ablations:** BM25-only vs dense-only vs hybrid vs hybrid+rerank; with/without authority weighting; with/without vocabulary expansion. The reranker is judged here before being adopted (§7.2).
- **Embedding-model gate:** benchmark `bge-m3`, `sentence-camembert-large`, Solon, **and at least one strong hosted multilingual model** (served through the same endpoint); choose the winner on **legal retrieval metrics after hybrid fusion**, not standalone dense recall. On a near-tie, prefer `bge-m3` (it preserves the learned-sparse / ColBERT upgrade path).
- **Grounding / citation test:** `cite` returns the correct state per case — `exact`, `normalized`, `ambiguous`, `stale_version`, `not_found`, `source_unavailable`; `--strict` rejects fabricated **and** stale citations (exit `2`); a dated citation resolves to the correct historical version (§10.5, §12).
- **Temporal edge tests:** sentinel normalization (`2999-01-01` → `null`), reform-date boundaries (2016 reform), and same-day version changes (§12).
- Held-out set to avoid tuning-to-the-test. Used to set RRF/authority weights rather than guessing.

**CLI / contract behaviour (CLI-only acceptance criteria).**
- `jurisearch help agent` contains every command, flag, enum, and schema; `help schema --json` parses — **and all of `help`/`help agent`/`help schema` work with no index installed** (compiled-in schemas, §10.4).
- One-shot CLI returns **valid JSON** for success, no-results, and bad-input cases.
- **Session protocol (§11.1):** the JSONL session handles many sequential calls without restarting; input order is preserved (with echoed `id`); `help`/`help schema` work inside a session; a **malformed JSON line yields a JSONL error without killing the process**; `exit` is acknowledged then the process exits `0`.
- **Model-cache guard (§11.2):** in in-process mode with a model missing, `search` fails with an actionable error (no silent download); `model fetch` / `--allow-download` resolves it; `status` reports the gap.
- **Embeddings fingerprint guard (§11.2):** a query against an index whose embeddings dimension/model fingerprint differs from the configured provider fails fast — never degraded results.
- **`kind` mapping** is consistent across CLI, output, and `help schema --json` (`code⇒article`).
- A full `search → fetch → cite` loop stays within token budgets.
- **Exit codes are stable** (`0/2/3/4/5`).
- With `--json`, **stdout is JSON-only**; stderr carries diagnostics only.

**Best-in-class acceptance gates (the bar a release must clear).**
- **Official-source fidelity:** every result traces to an official source ID, source URL, source version, and the build manifest.
- **Temporal accuracy:** zero current-law leakage into historical `--as-of` queries in the eval set.
- **Citation exactness:** fabricated, ambiguous, stale, and malformed citations are rejected or correctly disambiguated (§10.5).
- **Agent efficiency:** `search → fetch → cite` workflows stay within strict token and tool-call budgets.
- **Ranking quality:** BM25 / dense / hybrid / +authority / +reranker benchmarked on French legal tasks; the shipped configuration is the eval-winner.
- **Corpus coverage:** `status` reports exact coverage + freshness for LEGI, Judilibre, and justice administrative.
- **Operational reproducibility:** the index rebuilds from official inputs + manifest to an equivalent artifact.

---

## 16. Security, compliance & licensing

- **Project licensing:** **AGPL-3.0 is acceptable** for this project, which is what makes **`pg_search`/ParadeDB** the **selected** lexical engine (packaging/runtime fit is a validation gate, not a licensing question).
- **Data licensing:** all sources are Licence Ouverte (Etalab) / free-reuse (DILA décret 24 June 2014) — redistribution and local indexing permitted; record attribution in `jurisearch status`.
- **Pseudonymisation:** Judilibre and justice-administrative decisions are already pseudonymised at source (natural-person names removed). `jurisearch` **must not** attempt re-identification and preserves source pseudonymisation; do not cross-link to re-identify.
- **No legal-advice framing:** outputs are retrieval results with sources; the tool surface and docs state it is a research aid, not legal advice.
- **Secrets** never logged; API errors surfaced as actionable (`5` exit) without leaking tokens.
- **Determinism/audit:** every result is traceable to a source URL + dataset version (in the manifest) for downstream verification.

---

## 17. Phased roadmap

Production-grade phases, not an MVP path. Each phase is a **quality gate** (§15); "works on a sample" is never a milestone, and no phase imports derived-dataset chunks or skips official-XML edge cases.

**Scope of the "best-in-class" claim.** Phase 1 delivers best-in-class **LEGI / statutory** search; the overall ambition — best-in-class **French juridic** search across statutes *and* jurisprudence — is only reached at **Phase 2**. Docs and `status` must not present a LEGI-only release as the complete juridic engine.

**Phase 0 — Backend & ingestion validation.**
- Validate the decided backend stack against its acceptance criteria (§13.3): packaging, child-Postgres lifecycle, pinned `pgvector`/`pg_search`, warm latency, temporal prefilter, French BM25 quality, hybrid fusion quality.
- Build the **official LEGI/DILA XML parser** + canonical-record schema + Rust schema validation; stand up the **evaluation harness** and the embeddings-endpoint contract (§11.2).

**Phase 1 — Production-quality LEGI search (best-in-class for *statutory* law).**
- Authoritative ingestion from **official LEGI XML** only; structure-aware chunking; **temporal correctness** (as-of, sentinels, version groups).
- Postgres + `pgvector` + `pg_search` index; hybrid (BM25 + dense via the endpoint) + **custom RRF + authority**; **reranker if the eval gate says it earns its latency** (§7.2).
- Full CLI/JSONL contract: `search`, `fetch`, `cite`, `context`, `status`, `help agent`, `help schema`, `session`/`batch` (§11.1); **citation verification** (§10.5).
- Meets the **best-in-class acceptance gates** (§15) for LEGI before release.

**Phase 2 — Full jurisprudence coverage (completes best-in-class *French juridic* search).**
- Add **Judilibre** + **justice administrative** ingestion: **zone-aware decision chunking** (official offsets, non-sequential reassembly, regex fallback — §6); **graph relationships** (`related`, rapprochements, applies_article); **authority weighting** tuned by eval; vocabulary expansion.
- Incremental `sync`. Coverage + freshness reported by `status`.

**Phase 3 — Best-in-class ranking.**
- Reranker tuning; learned-sparse (SPLADE / bge-m3 sparse) or ColBERT if the eval set shows gains; agent-workflow benchmarks; cross-encoder distillation for speed.
- Expand corpora (designed-for): EU law (EUR-Lex/CURIA), KALI, BOFIP, doctrine.

---

## 18. Remaining validation gates (all product decisions are made — see `DECISIONS.md`)

Every product/architecture decision is settled: **name** (`jurisearch`), **runtime** (Rust; Python offline-only), **transport** (CLI-only + JSONL session; no MCP/HTTP), **backend** (embedded Postgres + `pgvector` + `pg_search` — D3), **embeddings** (OpenAI-compatible endpoint default, incl. local `llama.cpp`; in-process optional — D5), **corpus source** (official DILA/LEGI XML from day 1 — D7), and **Qdrant** (out of scope). What remains is **validation**, not choice:

1. **Backend spike result** — does the decided stack clear all §13.3 acceptance criteria (packaging + quality)? Fallbacks engage only on a hard failure.
2. **Embedding model** — `bge-m3` vs French specialists, decided by the eval set (§15), served over the configured endpoint.
3. **Reranker** — does it clear the latency/quality gate (§7.2) to ship in Phase 1, or hold to Phase 3?

See `RESEARCH.md` for the sourced evidence behind every choice above.
