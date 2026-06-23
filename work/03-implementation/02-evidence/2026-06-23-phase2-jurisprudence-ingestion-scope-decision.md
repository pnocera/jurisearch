# Phase 2 jurisprudence ingestion — scope decision (ADR)

Date: 2026-06-23
Status: accepted
Decision owner: implementation (codex design sanity-check GO, `qa/20260623-165543`)

## Context

`IMPLEMENTATION_PLAN.md §5` (Phase 2) frames **Judilibre via the official PISTE API** plus
justice-administrative ingestion as the **required** Phase 2 ingestion path, and **DILA bulk
jurisprudence XML** (2.2a; roots `TEXTE_JURI_JUDI` / `TEXTE_JURI_ADMIN`; sources
`cass/capp/inca/jade`) as an **optional, flagged** coverage adapter that is *not a zone-accurate
replacement* for Judilibre.

The locked architecture (Planning Rules §1; `DESIGN.md`) is **official-source-first, offline XML →
canonical record**, not API-first. Phase 0/1 built the entire LEGI corpus from local DILA bulk
archives. On this machine the full DILA bulk jurisprudence corpus is present with baselines + deltas
(`/home/pierre/Apps/juridocs/opendata/{CASS,CAPP,INCA,JADE}/`, identical
`Freemium_<src>_global_<ts>.tar.gz` + `<SRC>_<ts>.tar.gz` naming to LEGI) and the official DTDs are
present (`/home/pierre/Apps/juridocs/DTD/{juri,jade}/`). The shared PISTE client already supports
Judilibre `KeyId` search + `/transactionalhistory` and Légifrance OAuth.

## Decision

For Phase 2, **DILA bulk jurisprudence XML is the primary full-corpus offline ingestion path**
(judicial: CASS/CAPP/INCA `TEXTE_JURI_JUDI`; administrative: JADE `TEXTE_JURI_ADMIN`), mirroring the
proven LEGI pipeline. **Judilibre PISTE remains required and first-class** for what bulk cannot give:
`cite --online` decision verification, `sync --since` deltas via `/transactionalhistory`, and
zone-accurate enrichment of individual decisions. Full-corpus ingestion does **not** egress through
the Judilibre API.

This corrects the implementation route while preserving the spirit of §5: the authoritative build
uses official sources, APIs are used for deltas/verification/enrichment, and the zone-accurate
property is preserved as a real, additive quality upgrade rather than a blocker.

## Correctness line: source is not quality

A bulk-ingested CASS/CAPP/INCA/JADE decision is official, searchable, citeable, and traceable to DILA
XML — but it is **not** a Judilibre zone-offset record unless enriched from Judilibre. Status and
manifests therefore report two independent properties:

- **corpus coverage** by source family (`dila_juri_judi_cass/capp/inca`, `dila_juri_admin_jade`,
  `judilibre_api_enriched`);
- **zone quality** per chunk: `zone` (official offsets present, Judilibre only), `heuristic`
  (regex/heading fallback over bulk), `structural` (trusted XML boundary), `hard_split` (size
  emergency). Bulk-only ingestion must **never** satisfy the "official-zone chunking" acceptance by
  assertion; the zone-aware `fetch --part` gate passes only over records with official zones, with a
  separate fallback-quality metric for heuristic chunks.

We never claim "Judilibre ingestion complete" when the data came only from DILA bulk; we claim
"official DILA jurisprudence bulk ingested" + "Judilibre online/enrichment available".

## Canonical decision schema constraints (baked in up front)

- **Shared decision core**: `source`, `source_uid`, `kind='decision'`, `title`/`citation`, full
  `text`, `decision_date`, court/`jurisdiction`, formation/chamber when available, `ecli`,
  `source_url`, source archive/member/payload-hash, pseudonymisation/source provenance.
- **Judicial-specific** (preserved in `canonical_json`): pourvoi / `NUMERO_AFFAIRE`, chamber,
  publication (`PUBLI_BULL`), solution, attacked decision (`FORM_DEC_ATT`/`DATE_DEC_ATT`), Judilibre
  taxonomy raw keys when enriched.
- **Administrative-specific** (preserved in `canonical_json`): `CETATEXT`, administrative court level,
  `NUMERO`, `TYPE_REC`, `PUBLI_RECUEIL`, titrage/abstracts, admin links.
- **Temporal**: decisions are *dated, not versioned*. `valid_from` is set from `decision_date` purely
  as an indexing/filter convenience and is **not** exposed as legal validity; `decision_date` stays
  explicit in canonical JSON / output. `valid_to` stays null.
- **Identifier/alias policy**: keep source-native IDs authoritative (`JURITEXT`, `CETATEXT`, `ECLI`,
  pourvoi). Internal `document_id` never pretends a DILA-bulk record came from Judilibre. ECLI is the
  preferred cross-source dedup/enrichment key; court/date/number is a cautious fallback only. A later
  Judilibre match enriches/links the same canonical decision, never creates a duplicate search hit.
- **Graph edges**: `LIENS` applied-texts, `CITATION_JP`, rapprochements, appeal/attacked-decision
  metadata are **publisher** edges (evidence-bearing, `edge_source='publisher'`); regex/parser-
  inferred citations are **separate, lower-trust** edges (`edge_source='inferred'`). Unresolved raw
  targets are stored, then resolved later without dropping evidence.
- **Pseudonymisation**: both judicial and administrative bulk XML are already pseudonymised
  (`[Adresse 1]`, `M. B...`). Preserve as-is; no cross-source linking whose purpose/effect is to undo
  pseudonymisation.
- **Embeddings privacy**: jurisprudence embeddings prefer the local endpoint; remote endpoints for
  decision text require explicit approval (CGU/privacy posture).

## Build order (first increment)

1. Extend archive source planning for `cass`, `capp`, `inca`, `jade` (existing baseline/delta convention).
2. Parse `TEXTE_JURI_JUDI` / `TEXTE_JURI_ADMIN` → `CanonicalDecision`.
3. Project into existing `documents` (`kind='decision'`), `chunks`, `graph_edges`.
4. Chunk `SOMMAIRE` separately when present; chunk full text with conservative heuristic boundaries,
   flagged `chunking='heuristic'` / `chunk_builder_version='juri_decision_heuristic:v1'`.
5. Materialise official XML links as publisher/source edges with raw evidence.
6. Add status counters + ingest manifest showing source dataset and chunking provenance.
7. Add Judilibre `cite --online` / enrichment next; upgrade zone provenance only when offsets present.

## References

- `qa/20260623-165543` — codex design sanity-check (recommendation: accept pivot, record as ADR).
- `IMPLEMENTATION_PLAN.md §5`, `§W4`, `§W8`; `DESIGN.md`; `DECISIONS.md`.
- DILA datasets: data.gouv.fr `cass` / `capp` / `jade`; Judilibre API + `/transactionalhistory`.
