# Q&A — 20260623-165543

## Question

# Phase 2 jurisprudence ingestion — architecture decision

Repo: `/home/pierre/Work/jurisearch` (Rust workspace). I am about to implement Phase 2 of
`work/03-implementation/IMPLEMENTATION_PLAN.md` ("Jurisprudence and Full French Juridic Search":
2.1 Judilibre ingestion, 2.2 justice administrative, 2.3 graph, 2.4 decision search/fetch/context/cite,
2.5 incremental sync, 2.6 eval gate).

## The decision

The plan's §5 text says the **required** Phase 2 ingestion path is **Judilibre via the official PISTE API**,
and frames **DILA bulk jurisprudence XML** (2.2a, roots `TEXTE_JURI_JUDI` / `TEXTE_JURI_ADMIN`,
sources `cass/capp/inca/jade`) as an **optional, flagged** coverage adapter.

However, the actual situation on this machine:

- The whole project is built on an **offline official-XML → canonical record** architecture
  (see `crates/jurisearch-ingest/src/legi/mod.rs`, `crates/jurisearch-ingest/src/archive/*`,
  `crates/jurisearch-storage/src/projection.rs`). LEGI Phase 1 was done entirely from local DILA
  bulk archives.
- Local DILA bulk jurisprudence archives are present and complete with baselines + deltas:
  `/home/pierre/Apps/juridocs/opendata/{CASS,CAPP,INCA,JADE}/` each with
  `Freemium_<src>_global_<ts>.tar.gz` baseline + `<SRC>_<ts>.tar.gz` deltas — identical naming
  convention to the LEGI archives the planner already handles.
- Official DTDs are present at `/home/pierre/Apps/juridocs/DTD/juri/` (`TEXTE_JURI_JUDI`) and
  `/home/pierre/Apps/juridocs/DTD/jade/` (`TEXTE_JURI_ADMIN`). The XML carries META_COMMUN (ID =
  JURITEXT/CETATEXT, ECLI, NATURE), META_JURI (TITRE, DATE_DEC, JURIDICTION, NUMERO, SOLUTION),
  META_JURI_JUDI/ADMIN (formation, pourvoi NUMERO_AFFAIRE, publication, etc.), TEXTE (BLOC_TEXTUEL/
  CONTENU full text, SOMMAIRE titrage, CITATION_JP), and LIENS (LIEN* applied-text links).
- The shared PISTE client already exists (`crates/jurisearch-official-api`) with Judilibre `KeyId`
  search + `/transactionalhistory`, Légifrance OAuth, retry/backoff. PISTE credentials are in env.
- The DILA bulk XML does **not** carry Judilibre's official zone offsets (motivations/dispositif/
  moyens/visa with start/end), so zone chunking from bulk XML must be heuristic/fallback-flagged.

## My proposed approach (please critique)

1. Make **DILA bulk offline XML the primary full-corpus jurisprudence ingestion path** for Phase 2
   (judicial: CASS/CAPP/INCA `TEXTE_JURI_JUDI`; administrative: JADE `TEXTE_JURI_ADMIN`), mirroring
   the proven LEGI pipeline: extend the archive source enum + planner, add a `juri` parser emitting
   canonical **decision** records (the `documents` table already allows `kind='decision'`), project
   into documents/chunks/graph_edges with ingest accounting/resume/quarantine.
2. Because bulk lacks official zones, mark chunking provenance **heuristic/fallback** and keep the
   "official zones are primary boundaries" gate **honestly unmet by bulk alone** — surfaced in
   status/manifest/provenance exactly as 2.2a requires (bulk records clearly distinguishable, never
   mistakable for zone-accurate Judilibre records).
3. Wire the **Judilibre PISTE API** for what bulk cannot give: `cite --online` decision verification
   (2.4) and `sync --since` deltas via `/transactionalhistory` (2.5), and optionally zone-accurate
   enrichment of individual decisions. Not full-corpus API egress.
4. Preserve source pseudonymisation (both judicial and administrative bulk XML are already
   pseudonymised — `[Adresse 1]`, `M. B...`); no cross-source re-identification.

The net effect: Phase 2 ships real, deterministic, testable jurisprudence search offline now, with the
API used for verification/deltas, and the "official zone-accurate" property tracked honestly as a
provenance/quality distinction rather than blocked entirely.

## Questions

1. Is making DILA bulk the primary offline jurisprudence ingestion path (with Judilibre API for
   cite/sync/enrichment) a sound, plan-compatible reading of Phase 2 given the locked
   "offline official XML" architecture and what's actually available locally — or is there a
   correctness/compliance reason I must instead drive full-corpus ingestion through the Judilibre API?
2. Any trap in mapping both `TEXTE_JURI_JUDI` and `TEXTE_JURI_ADMIN` into one canonical decision
   schema (shared `documents`/`chunks`/`graph_edges`) that I should design for up front?
3. Is treating the "official zones" acceptance as a provenance-flagged quality property (honestly
   unmet by bulk, met only when Judilibre zones are present) the right way to avoid a dishonest
   gate, rather than blocking 2.1 entirely?

Please answer concisely with a recommendation and any must-fix design constraints. This is a
design sanity-check before I write code; no repo changes needed.

## Answer

# Phase 2 Jurisprudence Ingestion: Recommendation

Recommendation: accept the proposed pivot, but record it explicitly as a Phase 2 scope decision/ADR. Use DILA bulk jurisprudence XML as the primary **full-corpus offline ingestion** path, and use Judilibre PISTE for online citation verification, transactional sync, and zone-accurate enrichment. That is the best fit for the existing architecture and the local corpus you actually have.

This is sound because the locked architecture is not "API-first"; it is **official-source-first**. The design already says the authoritative build ingests official DILA bulk XML plus Judilibre, with APIs used for incremental sync and live citation confirmation. DILA CASS/CAPP/INCA/JADE are official DILA/data.gouv.fr datasets under Licence Ouverte, with DTDs and bulk/delta delivery. I do not see a correctness or compliance reason that full-corpus ingestion must egress through Judilibre API if the same or broader official corpus is available locally as official XML.

The caveat is plan compatibility: `IMPLEMENTATION_PLAN.md §5` currently says Judilibre/justice-administrative ingestion is the required Phase 2 path and DILA bulk jurisprudence is optional fallback, not a zone-accurate replacement. So do not silently reinterpret the plan. Add a small decision note saying: "for full-corpus deterministic ingestion, Phase 2 accepts official DILA jurisprudence bulk as the primary corpus path; Judilibre remains required for zone-accurate judicial records, cite --online, transactionalhistory, and enrichment." That preserves the spirit of the plan while correcting the implementation route.

## Answers

### 1. Bulk primary vs Judilibre full-corpus API

Yes: make DILA bulk primary for deterministic full-corpus ingestion. It mirrors the proven LEGI path, avoids quota/rate-limit fragility, is resumable and replayable, and fits embedded single-node indexing. Judilibre should remain first-class, but not as the mandatory full-corpus transport.

The correctness line is provenance, not transport. A bulk-ingested CASS decision is official, searchable, citeable, and traceable to DILA XML, but it is not a Judilibre zone-offset record unless enriched from Judilibre. Status and manifests must therefore report two distinct qualities:

- corpus coverage: official DILA bulk present;
- zone quality: `zone`, `heuristic`, or `unavailable`, with Judilibre offsets as the only source of Judilibre zone accuracy.

Do not claim "Judilibre ingestion complete" if the data came only from CASS/CAPP/INCA XML. Claim "official DILA jurisprudence bulk ingested" and "Judilibre online/enrichment available".

### 2. Traps in one canonical decision schema

The main trap is accidental identity collapse or duplication. DILA judicial XML, Judilibre JSON, and administrative XML may describe overlapping decisions with different source IDs. Design an identifier policy before parsing:

- Keep source-native IDs: `JURITEXT`, `CETATEXT`, ECLI, pourvoi, `NUMERO_AFFAIRE`.
- Use a stable internal `document_id` that never pretends a record came from Judilibre when it came from DILA bulk.
- Add or plan a document-alias/identifier table if the existing `documents` row cannot represent multiple source IDs cleanly.
- Deduplicate/enrich by strong keys in this order: ECLI, exact source-native ID, court/date/number fallback only with caution.
- If a later Judilibre enrichment matches a bulk decision, update/enrich the same canonical decision or create a linked alias, not a duplicate search hit.

Second trap: forcing judicial and administrative metadata into one over-normalized shape. Use a small shared decision core and preserve raw source metadata in `canonical_json`:

- shared: source, source_uid, kind=`decision`, title/citation, text, decision_date, court/jurisdiction, formation/chamber when available, ECLI, source_url, source archive/member/hash, pseudonymisation/source provenance;
- judicial-specific: pourvoi, chamber, publication, solution, attacked decision, Judilibre taxonomy/raw keys;
- administrative-specific: `CETATEXT`, administrative court level, `NUMERO_AFFAIRE`, abstracts/titrage fields, administrative-specific links.

Third trap: temporal semantics. Decisions are dated, not versioned like LEGI articles. If the current storage requires `valid_from`, set it from `decision_date` as an indexing/filter convenience, but do not expose it as legal validity. Keep `decision_date` explicit in canonical JSON and API output.

Fourth trap: graph semantics. Treat `LIENS`, applied texts, `CITATION_JP`, rapprochements, appeal/attacked-decision metadata, and inferred citation-parser links as separate edge kinds. Publisher/bulk edges should use `edge_source=publisher` or a more precise official-source tag; parser-inferred edges should be separate and lower-trust. Store unresolved raw targets when needed, then resolve later without dropping evidence.

### 3. Official zones as a provenance-flagged quality property

Yes. That is the right way to avoid a dishonest gate.

The hard invariant should be: when official publisher offsets are present, use them and reassemble by zone identity; when they are absent, chunk heuristically and say so everywhere. Bulk-only ingestion should not block jurisprudence search, but it must not satisfy the "official-zone chunking" acceptance by assertion.

Concretely:

- `chunking = "zone"` only for Judilibre/export records or another source that gives offsets.
- `chunking = "heuristic"` for regex/heading/section fallback over bulk XML.
- `chunking = "structural"` only where the XML itself gives a real structural boundary you trust.
- `chunking = "hard_split"` only for size-based emergency splits.
- `source_fields` should name the XML fields used, e.g. `TEXTE/BLOC_TEXTUEL/CONTENU`, `SOMMAIRE`, `CITATION_JP`.
- `chunk_builder_version` should distinguish `juri_decision_zone:v1` from `juri_decision_heuristic:v1`.
- `fetch --part motivations|dispositif|moyens` should return an exact/official provenance flag for zone chunks and an approximate/unavailable flag for heuristic chunks.

This lets Phase 2 ship useful jurisprudence search before full zone enrichment, while preserving a meaningful eval gate for zone-aware fetch.

## Must-Fix Design Constraints

1. **Record the scope decision.** Update the plan or add an ADR before code: DILA bulk is accepted as primary full-corpus official XML ingestion; Judilibre remains the zone-accurate/API/enrichment path. Without that, the implementation will look like it violates §5.

2. **Do not conflate source and quality.** A document can be official but not zone-accurate. Store both: `source_family/source_dataset` and `chunking/zone_provenance`.

3. **Create an identifier/alias strategy up front.** Avoid duplicate decisions across DILA CASS/CAPP/INCA and Judilibre. ECLI is the best cross-source key when present, but keep source-native IDs authoritative.

4. **Keep raw metadata.** Normalize only what the CLI needs for search/fetch/cite; preserve raw XML-derived metadata and raw Judilibre taxonomy keys in `canonical_json`/payload for later corrections.

5. **Separate publisher edges from inferred edges.** DILA/Judilibre-provided links are evidence-bearing official metadata. Regex or parser-discovered links are useful but must not share the same trust level.

6. **Do not re-identify.** Preserve source pseudonymisation and avoid cross-source linking whose purpose or effect is to undo pseudonymisation. This matters even with official open data.

7. **Make status brutally explicit.** Report coverage by source family: `dila_juri_judi_cass`, `dila_juri_judi_capp`, `dila_juri_judi_inca`, `dila_juri_admin_jade`, `judilibre_api_enriched`. Also report chunking distribution: `% zone`, `% heuristic`, `% hard_split`, `% missing text`, parse failures, quarantined members.

8. **Treat deltas as replay events, not append-only files.** Preserve archive/member ordering, payload hashes, delete/update semantics if present, run/member/error accounting, and deterministic replay signatures.

9. **Eval separately by quality tier.** Jurisprudence search can pass over bulk; zone-aware `fetch --part` should only pass over records with official zones, with a separate fallback-quality metric for heuristic chunks.

10. **Be careful with external services.** If decision embeddings ever use remote endpoints, revisit privacy/CGU posture. For the stated CLI-only/single-node/offline direction, prefer local embedding for jurisprudence unless explicitly approved.

## Practical First Increment

Implement `juri` bulk ingestion first:

1. Extend archive source planning for `cass`, `capp`, `inca`, `jade` using the existing baseline/delta convention.
2. Parse `TEXTE_JURI_JUDI` and `TEXTE_JURI_ADMIN` into `CanonicalDecision`.
3. Project into existing `documents`, `chunks`, and `graph_edges` with `kind='decision'`.
4. Chunk `SOMMAIRE` separately when present, then chunk full text using conservative heuristic boundaries, all flagged `chunking='heuristic'`.
5. Materialise official XML links as publisher/source edges with raw evidence.
6. Add status counters and an ingest manifest showing source dataset and chunking provenance.
7. Add Judilibre `cite --online`/enrichment next, matching by ECLI/source ID and upgrading zone provenance only when offsets are actually present.

That gives real deterministic jurisprudence search quickly, and it leaves the zone-accurate Judilibre path as an additive quality upgrade instead of a blocker.

## Sources Checked

- Local plan/design: `work/03-implementation/IMPLEMENTATION_PLAN.md`, `work/01-design/DESIGN.md`, `work/01-design/DECISIONS.md`.
- Local code shape: `crates/jurisearch-ingest/src/legi/mod.rs`, `crates/jurisearch-ingest/src/archive/planner.rs`, `crates/jurisearch-storage/src/projection.rs`, `crates/jurisearch-official-api/src/lib.rs`.
- Judilibre data.gouv.fr API page: https://www.data.gouv.fr/dataservices/api-judilibre
- Judilibre dataset page: https://www.data.gouv.fr/datasets/api-judilibre
- Judilibre OpenAPI/GitHub docs, including zones and `/transactionalhistory`: https://github.com/Cour-de-cassation/judilibre-search/
- DILA CASS dataset: https://www.data.gouv.fr/datasets/cass
- DILA CAPP dataset: https://www.data.gouv.fr/datasets/capp
- DILA JADE dataset: https://www.data.gouv.fr/datasets/jade
