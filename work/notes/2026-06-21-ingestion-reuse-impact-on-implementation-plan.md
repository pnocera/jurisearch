# Impact of `juridocs` ingestion-reuse notes on the `jurisearch` IMPLEMENTATION_PLAN

Date: 2026-06-21

Inputs:
- `work/notes/2026-06-21-juridocs-ingestion-reuse.md` (the reuse notes)
- `work/03-implementation/IMPLEMENTATION_PLAN.md` (the plan)

Method: each of the 12 reuse categories, the "do not copy" list, and the
recommended 11-step sequence in the notes was mapped onto the plan's
workstreams (W1–W8), phase tasks (0.x / 1.x / 2.x), the workstream↔phase
matrix, release gates, and the risk register. CodeGraph is not used: there is
no code yet (design/planning only), and the notes confirm CodeGraph is
uninitialized in both repos.

---

## 1. Bottom line

The reuse notes do **not** conflict with any locked decision in the plan
(§1 planning rules, `CONCEPTION §16`, `DECISIONS.md`). Their impact is:

- **Mostly additive reference material** that de-risks tasks already in the
  plan (temporal, hierarchy, DTD parsing, canonical-only ingestion).
- **A handful of genuine gaps** the plan does not currently name and should
  absorb: archive precedence/streaming, ingest run/member/error accounting +
  resume/quarantine, per-chunk payload hashing, token-budget chunk guardrails,
  derived-projection gating, and **ingest-health quality gates** (distinct from
  the retrieval-quality gates W2 already owns).
- **Two sequencing changes**: capture publisher links as canonical graph edges
  during Phase 1 LEGI ingestion (not only in Phase 2's graph layer), and move
  run-accounting/quarantine/resume ahead of full-corpus ingestion.
- **One scope decision**: whether to add DILA **bulk** jurisprudence XML
  (`cass/inca/capp/jade`) as a Phase 2 source alongside the Judilibre API, and
  if so, strictly as a fallback that does not satisfy official-zone chunking.

Net effect: the plan's **shape, phases, and locked decisions stand**. What
changes is the **granularity of W3/W4/W2/W7 deliverables**, a few **new Phase 0
tasks**, and **six new risk-register lines**.

---

## 2. Impact map (note category → plan touchpoints → impact type)

Impact types: **Reinforce** (de-risks existing task, no plan change needed),
**Gap** (plan is missing a deliverable/task to add), **Resequence** (work the
plan defers should start earlier), **Decision** (a scope choice to resolve).

| # | Reuse category | Plan touchpoints | Impact |
|---|---|---|---|
| 1 | Archive naming / precedence / replay order | W4; 0.5; 2.5; manifest | **Gap** — no archive module or baseline/delta precedence anywhere in the plan |
| 2 | Streaming archive member processing + size caps | W4; 0.5→1.1; manifest | **Gap** — full-baseline throughput/streaming and member caps not addressed |
| 3 | DTD-backed parser contracts, typed IDs, error taxonomy | W1 (IDs); 0.5; 1.1; §7 tests | **Reinforce** + add a structured parse-error taxonomy and DTD coverage checklist |
| 4 | Temporal versioning rules | W4; 0.2; 0.5; 1.1 | **Reinforce** — strongest alignment; enrich fixtures + adopt the ID scheme |
| 5 | Provenance, member accounting, recovery | W3 schema; W4; W6 `status` | **Gap** — `ingest_run/member/error` tables, resume, quarantine, version fields |
| 6 | Canonical payload construction + payload hashing | W4; W3 schema; W5 | **Gap** — per-chunk `source_payload_hash` / builder version; versioned text-assembly contract |
| 7 | FR sentence splitting + embedding-size guardrails | W4; 1.2; W5 | **Gap** — tokenizer preflight, abbreviation repair, chunk-origin provenance |
| 8 | Link and reference extraction | W4; 2.3 | **Resequence** — extract publisher links to canonical edges in Phase 1, not only Phase 2 |
| 9 | LEGI hierarchy / `CONTEXTE` extraction | 1.1; 1.2 | **Reinforce** — `hierarchy_path`, embedding prefix, hierarchy-survival test |
| 10 | Derived projection / backfill discipline | W4; W3; gates | **Gap** — gate query access until projections pass; safe-mode + rollback |
| 11 | Jurisprudence bulk XML parser/loader lessons | 2.1; 2.2 | **Decision** — DILA bulk JURI/JADE vs Judilibre API; reuse only as fallback |
| 12 | Quality gates, replay invariants, cutover | W2/W3/W4/W7; release gates; `status` | **Gap** — ingest-health + replay-snapshot gates (W2 today is retrieval-only) |
| — | "Do not copy" list | risk register; W3/W4/W5 | **Reinforce** locked decisions + add error-model split + unsupported-root counters |

---

## 3. Genuine gaps the plan should absorb

These are the items that are **new** relative to the plan as written.

### G1 — Archive precedence + streaming ingest module (notes §1, §2)
The plan's 0.5 says "parse a representative LEGI XML subset" and jumps to 1.1
"full LEGI corpus", but never names: baseline-vs-delta selection, replay
ordering, mixed-dataset rejection, streaming `.tar.gz` member reads, or member
byte caps. For a full LEGI baseline this is load-bearing, not optional.

Recommendation:
- Add a **new Phase 0 task (proposed 0.5a)**: stand up `jurisearch-ingest::archive`
  (parser + planner + reader), porting `juridocs` planner semantics and adapting
  `test_archive_ordering.rs`. Replace `jd_core::dataset::Dataset` with a
  `jurisearch` source enum (`legi` first; later `jorf`, `judilibre`, `ja`,
  DILA bulk families).
- Add to **W4 deliverables**: "Archive plan (selected baseline + ordered deltas)
  recorded as an explicit manifest artifact; streaming member reads with
  configurable, manifest-recorded byte caps; deterministic member ordering."
- Add to **2.5 Incremental sync**: reuse the same replay order for
  `sync --since` reproducibility.

### G2 — Ingest run/member/error accounting, resume, quarantine (notes §5, "do not copy")
W3's schema list ("documents, chunks, vectors, graph edges, manifests, eval
traces") has **no operational ingest tables**, and no task mentions
resume-after-interruption or quarantine. 1.1 records `schema_version` /
`parser_version` on the corpus, but not at run/member granularity.

Recommendation:
- Add to **W3 schema deliverables**: `ingest_run`, `ingest_member`,
  `ingest_error` tables; member identity = (run, archive name, member path,
  date anchor).
- Add to **W4 deliverables**: resume that skips `inserted`/`skipped` members but
  retries `failed`/`parsed`; optional quarantine of failed payloads; a
  structured error taxonomy (parse / validation / DB / IO / embedding / unknown).
- Carry `parser_version`, `schema_version`, `source_payload_hash`, and
  `code_version` on run/member records and use them as recovery-compatibility
  gates. Do **not** make them part of the member identity key: that would create
  duplicate member rows and make resume semantics harder. Recovery after a
  parser/schema change must trigger targeted reprocess, not blind skip.
- Add to **W6 `status`**: surface corpus freshness, coverage, and ingest health
  in `status --json` from this data.
- **Resequence**: per notes sequence step 4, this lands **before** full-corpus
  ingestion (1.1) — propose a **new task 1.0 / late-Phase-0** gating 1.1.

### G3 — Per-chunk payload hashing + versioned text-assembly contract (notes §6)
0.4 fixes an embedding-model fingerprint, but nothing fixes **what text was
embedded**. Re-embedding diffs and regression explainability need a hash of the
exact source payload per chunk.

Recommendation:
- Add to **W4/W3**: per-chunk `source_payload_hash`, `source_fields`,
  `chunk_builder_version`, `embedding_fingerprint`; define canonical text
  assembly as a **versioned contract per `Document.kind`** with a fixed field
  order used in tests.
- Reinforces the existing locked stance: do **not** inherit `juridocs`'
  mean-pooled entity vector as the retrieval abstraction (already aligned with
  W5 chunk-direct ranking).

### G4 — Token-budget chunk guardrail + chunk-origin provenance (notes §7)
1.2 splits long articles "only on legal substructure" but never mentions a
tokenizer or an embedding-endpoint preflight. The notes' hard-won lesson:
char-based chunking alone overflows real endpoints.

Recommendation:
- Add to **1.2 / W5**: a real tokenizer (or conservative endpoint-specific
  preflight) before embedding, with the `juridocs` char/guardrail estimate kept
  only as a fallback; reuse the FR sentence splitter + legal-abbreviation repair
  (`art.`, `L.`, `R.`, enumerations) for long-article sub-splitting only.
- Record per chunk whether it came from **structural**, **zone**, or
  **heuristic/hard-split** logic (a provenance flag).

### G5 — Derived-projection gating + safe-mode/rollback (notes §10)
The plan separates canonical records from indexes implicitly but never states
that **query access must be gated until derived projections pass integrity
gates**, nor provides a rollback/safe-mode ingest path.

Recommendation:
- Add to **W4**: let canonical-record + validated source writes complete before
  search/vector/graph projections; `jurisearch ingest` must fail or warn loudly
  if an index is queryable before derived layers pass gates.
- Add a **safe-mode ingest path** (and conservative rollback) from day one, even
  if the first implementation is simple.

### G6 — Ingest-health and replay-snapshot quality gates (notes §12)
W2 today is **retrieval-quality only** (golden tasks, ablations, citation/temporal
fixtures). It does not cover ingest-health gates: failed-member %, ingest-error
counts, projection/embedding coverage, or replay-by-snapshot/diff.

Recommendation:
- Extend **W2/W3/W4/W7 deliverables** to include **ingest/operational quality
  gates**: W2 owns the gate harness/reporting, W3 owns schema/projection metrics,
  W4 owns ingestion correctness/replay inputs, and W7 owns the operational
  runbooks. Gate on latest completed run per source; gate failed-member % and
  error counts; gate projection/embedding coverage; gate replay by snapshot/diff
  over canonical records, chunks, graph edges, embeddings, and manifest fields —
  **not** by row-count summaries.
- Report gates as markdown **and** machine-readable JSON; have `status` derive
  its freshness/coverage claims from the same gate data.
- Wire these into the **Phase 0 / Phase 1 release gates** (§8) so cutover stays
  blocked when mandatory ingest gates fail.

---

## 4. Sequencing changes

### S1 — Pull link extraction into Phase 1 (notes §8)
The plan places **all** graph work in Phase 2 (2.3). The notes recommend
extracting DILA `LIEN` / `LIEN_ART` / `LIEN_SECTION_TA` / `LIEN_TXT` and inline
anchors during source parsing — i.e. during **Phase 1 LEGI ingestion** — and
materializing them as canonical `GraphEdge` records (`edge_source = publisher`),
idempotent/deduplicated, rebuildable from canonical records.

Why it matters: if Phase 1 ingestion does not capture publisher links into
canonical records, the Phase 2 graph layer forces a **re-ingestion** of LEGI.
Capturing edges as canonical records in Phase 1 makes 2.3 a pure derived-record
build.

Recommendation: add to **1.1 / W4** "extract publisher links to canonical
`GraphEdge` records"; keep 2.3 focused on traversal (`related`) and the
`inferred` citation-parser path.

### S2 — Run accounting before full-corpus ingestion (notes sequence step 4)
See G2: schedule the operational-tables/resume/quarantine work **before** 1.1,
not as an afterthought.

The notes' 11-step ingestion sequence is otherwise compatible with the plan's
Phase 0→1 ordering; the only forced reorderings are S1 and S2.

---

## 5. Scope decision to resolve

### D1 — DILA bulk jurisprudence XML vs Judilibre API (notes §11, "do not copy")
The plan's 2.1 frames jurisprudence ingestion as **Judilibre API** (official
zone offsets, transactional history). The `juridocs` `jurisp` parser targets
DILA **bulk** XML (`cass/inca/capp/jade`, roots `TEXTE_JURI_JUDI` /
`TEXTE_JURI_ADMIN`, IDs `JURITEXT*` / `CETATEXT*`) — a **different source path**
that does **not** provide official zones.

Decision needed: do we add DILA bulk JURI/JADE as an additional Phase 2 source?

- If **yes**: keep it **distinct** from Judilibre; Judilibre API stays primary
  for zone-accurate chunking; bulk decisions without publisher zones are chunked
  as heuristic/fallback and marked accordingly (consistent with the locked
  "official zones primary" rule in 2.1). Reuse `juridocs` parser/loader,
  correction-safe upsert, golden-fixture layout, and idempotency tests.
- If **no** (Judilibre-only for v2): the `juridocs` `jurisp` parser remains a
  reference, not a dependency.

Recommendation: record this explicitly in Phase 2 scope, but do **not** default
it to accepted yet. Treat DILA bulk jurisprudence as an optional adapter after
Judilibre and justice-administrative ingestion are stable. If accepted, call it
a clearly flagged coverage fallback, not an equal source path, because it does
not satisfy the official-zone chunking requirement.

---

## 6. Reinforcements (no plan change, use as build references)

These categories de-risk existing tasks; treat the `juridocs` sources as
concrete references during implementation:

- **§3 DTD parsing / typed IDs**: feeds W1 "source-namespaced ID types" (typed
  `LEGITEXT`/`LEGIARTI`/`LEGISCTA` wrappers), 0.5, 1.1, and §7 unit tests. Add a
  structured parse-error split (XML / required-field / invalid-date / invalid-ID)
  and treat the DTD coverage matrix as a checklist — but **re-verify against the
  current DTD** before making it authoritative.
- **§4 Temporal**: strongest alignment with W4 / 0.5 / 1.1 and the 0.2 temporal
  fixtures. Adopt `Document.id = "legi:<LEGIARTI>@<valid_from>"` +
  `version_group = <LEGIARTI>`; preserve raw `dateFin` as `valid_to_raw` even
  when normalized to null; expand 0.2 fixtures to cover current / modified /
  abrogated / same-day-boundary / sentinel; no upsert-overwrite of history.
- **§9 Hierarchy**: reinforces 1.1 `hierarchy_path` and 1.2 embedding context.
  Build the path from `CONTEXTE`/`TEXTELR`, include path labels in the embedding
  prefix while keeping returned text clean, and add a test that
  `Code → Livre → Titre → Chapitre → Section → Article` survives ingestion.
- **"Do not copy" list**: reaffirms locked decisions already in §1 (no
  `search_document`/`reference_index` core contract; vector dimension from the
  fingerprint, not a fixed `vector(768)`; chunk-direct ranking; official zones
  over sentence chunking; official-source-only). Two small **additions** worth
  adopting: (a) **separate accounting** so an embedding/index-projection failure
  never makes a successfully-ingested source-of-truth entity look failed; and
  (b) explicit **unsupported-root classification + counters**
  (`ignored_unsupported`, never "inserted unknown").

---

## 7. Proposed risk-register additions (§9)

| Risk | Impact | Mitigation |
|---|---:|---|
| Full LEGI baseline volume/throughput exceeds the naive subset parser | High | Streaming member reads + configurable member caps from Phase 0 (G1); deterministic ordering tests |
| Resume/recovery after a parser/schema change preserves stale bad rows | High | Record `parser_version`/`schema_version`/`source_payload_hash`/`code_version` and enforce recovery-compatibility gates; targeted reprocess, not blind skip (G2) |
| Embedding/projection failure masks a successful canonical ingestion | Medium | Separate canonical-ingestion vs embedding/index-projection accounting and retry/backfill states (G2, §6) |
| Phase 1 ingestion that omits publisher links forces Phase 2 LEGI re-ingestion | Medium | Capture publisher links as canonical `GraphEdge` records during 1.1 (S1) |
| Char-based chunk sizing overflows the embedding endpoint | Medium | Tokenizer/endpoint preflight before embedding; `juridocs` guardrail only as fallback (G4) |
| DILA bulk jurisprudence parser mistaken as zone-accurate | Medium | Keep DILA bulk as optional flagged fallback; Judilibre and justice-administrative ingestion remain primary for official zones (D1) |

---

## 8. Suggested matrix rows (§2 traceability) if gaps are accepted

| Phase task | Owner | Depends on | Note |
|---|---|---|---|
| 0.5a Archive precedence + streaming module | W4 | 0.1 schema stubs | Ports `juridocs` archive parser/planner/reader + ordering tests (G1) |
| 1.0 Ingest run/member/error accounting + resume/quarantine | W4/W3/W2/W7 | 0.5 + 0.5a | W4 accounting, W3 schema, W2 gates, W7 runbooks; carries recovery-compatibility metadata (G2, S2) |
| (extend) 1.1 Full LEGI canonicalization | W4 | 1.0 | Also emits publisher `GraphEdge` records (S1) |
| (extend) Ingest-health gate harness | W2/W3/W4/W7 | 0.5a/1.0 | W2 reporting, W3 metrics, W4 replay inputs, W7 runbooks (G6) |

---

## 9. What does **not** change

- All §1 planning rules and locked decisions hold; nothing in the notes reopens
  product name, Rust runtime/search path, Python-offline-only boundary, CLI-only
  surface, official-source-only ingestion, embedded Postgres + `pgvector` +
  `pg_search`, OpenAI-compatible embeddings, or the fallback precedence.
- The phase structure (0 → 1 → 2 → 3+) and the W1–W8 ownership model are intact;
  the gaps slot **into** existing workstreams rather than adding new ones.
- The notes are explicitly a **playbook/reference**, not a storage/search design
  to copy — consistent with the plan treating `juridocs` table shapes as
  "adapt, not inherit".

---

## 10. Recommended next step

Fold the six gaps (G1–G6) and two resequencings (S1–S2) into
`IMPLEMENTATION_PLAN.md` as deliverable/task edits and matrix rows, add the six
risk lines, and record D1 (DILA bulk vs Judilibre) as an explicit Phase 2 scope
rule. None of this requires a design re-decision — it is plan granularity
catching up to a battle-tested ingestion playbook.
