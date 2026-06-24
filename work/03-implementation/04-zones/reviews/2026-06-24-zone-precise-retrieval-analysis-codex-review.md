# Code Review: Zone-Precise Retrieval Analysis

## Findings

### 1. Material: the resolvable Cassation coverage is overstated by treating all `cass` rows as pourvoi-resolvable

The analysis says official-zone retrieval is reachable for `cass` 141,616 plus `inca` 377,027, about 518.6k decisions / 45% of the jurisprudence corpus (`work/03-implementation/04-zones/2026-06-24-zone-precise-retrieval-analysis.md:52-61`, `:86-89`, `:146-149`, `:193`). That is not what the current resolver can do.

The lazy enrichment path resolves Judilibre by the first parser-valid pourvoi only: `decision_resolution_metadata_json` returns a `pourvoi` only when a `case_numbers` entry matches `^[0-9]{2}-[0-9]{4,6}$` (`crates/jurisearch-storage/src/decision_zones.rs:48-69`), and `enrich_decision_from_judilibre` caches `unsupported` and returns no official zone when that pourvoi is absent (`crates/jurisearch-cli/src/main.rs:3652-3671`). In the inspected index, the source counts match the analysis (`cass=141616`, `inca=384312`, `capp=72929`, `jade=545939`), but parser-valid pourvoi counts are `cass=117674`, `inca=377027`, `capp=120`, `jade=0`.

So the current-code reachable official-zone set is about `117,674 + 377,027 = 494,701` decisions, not 518.6k, unless the design explicitly adds another Cassation resolver for the roughly 23,942 `cass` rows without parser-valid pourvoi. The analysis should update the table, the percentage, the backfill/API-call estimate, and the coverage-honesty language to say "cass+inca with resolver-valid pourvoi" rather than "all cass plus parser-valid inca".

Impact: this is not just a numeric nit. The recommended product contract is coverage-sensitive, and a subsequent design based on this analysis would over-promise the official-zone corpus and under-specify what happens to non-pourvoi `cass` decisions.

### 2. Material gap: `zones_json` currently stores only three normalized zones, despite the analysis discussing introduction/expose/annexes as future index candidates

The analysis correctly says Judilibre exposes more zones than the current fetch surface (`introduction`, `expose`, `moyens`, `motivations`, `dispositif`, `annexes`) and later asks which zones to index, including whether `exposé/introduction` are worth it (`work/03-implementation/04-zones/2026-06-24-zone-precise-retrieval-analysis.md:47-49`, `:225-230`). But it also repeatedly frames `decision_zones.zones_json` as the already-captured fragment-text basis for zone units (`:70-73`, `:140-144`, `:205-208`).

Current code normalizes only `motivations`, `moyens`, and `dispositif` into `zones_json` (`crates/jurisearch-cli/src/main.rs:3771-3779`). The full Judilibre response is stored in `raw_json` by the v12 cache schema/upsert path (`crates/jurisearch-storage/src/migrations.rs:450-466`, `crates/jurisearch-cli/src/main.rs:3702-3719`), so the missing zones may still be recoverable from cached raw rows. But they are not already present as normalized fragment text in `zones_json`.

The analysis should make that distinction explicit. If the future design wants introduction/expose/annexes, it needs either a normalizer/cache migration, a zone-unit builder that derives from `raw_json`, or a backfill/re-enrichment policy for old cache rows. Otherwise a designer may assume the current normalized cache is already sufficient for all official zones.

### 3. Medium: Option A is under-specified because it blurs "replace current decision chunks" with "add official zone chunks into the same table"

The analysis says Option A would "re-chunk the main decision index by zone" from Judilibre text and add a `zone` filter to `HybridCandidateQuery` (`work/03-implementation/04-zones/2026-06-24-zone-precise-retrieval-analysis.md:101-112`). That is directionally right, but it needs a sharper fork because the current search/fetch contract joins retrieved `chunks` to `documents` and serves snippets from `chunks.body` while `fetch` serves `documents.body` from the local DILA serialization (`crates/jurisearch-storage/src/retrieval.rs:269-385`).

Replacing Cassation `decision_body` chunks with Judilibre-text zone chunks would improve zone precision but make default topical search snippets and embeddings come from a different serialization than `fetch` returns. Keeping the current heuristic chunks and adding official zone chunks into the same table is a different design: it avoids replacing topical retrieval text, but requires a retrieval-unit type/provenance model, duplicate decision representations, and careful default-query exclusion of zone-only chunks.

The analysis gestures at a mixed index, but a design based on it needs this distinction called out explicitly. Without that, Option A's blast radius is hard to evaluate against the current invariant that bulk decision chunks are heuristic (`crates/jurisearch-ingest/src/juri/mod.rs:190-195`, `:250-284`, `:754-823`) and the Phase 2 gate's source-level `zone_accurate=false` check (`crates/jurisearch-cli/src/main.rs:8790-8824`).

## What Checks Out

- The core capability gap is real. Bulk jurisprudence parsing captures DILA `BLOC_TEXTUEL/CONTENU`, flags decision chunking as `heuristic`, creates optional `decision_summary` plus paragraph-packed `decision_body` chunks, and does not attach any official zone label (`crates/jurisearch-ingest/src/juri/mod.rs:1-8`, `:754-823`).
- Retrieval has no zone dimension today. `HybridCandidateQuery` has `kind_filter` and `DecisionFilters` only, and the SQL candidate paths filter through document kind/metadata, not zone metadata (`crates/jurisearch-storage/src/retrieval.rs:70-86`, `:88-135`, `:269-385`).
- The v12 `decision_zones` table is a separate lazy overlay for `fetch --part --online`, not part of BM25 or dense search (`crates/jurisearch-storage/src/migrations.rs:442-466`; `crates/jurisearch-storage/src/decision_zones.rs:78-95`).
- The sample two-texts claim is confirmed on `/mnt/models/jurisearch-index/phase2-full-juridic`: `cass:JURITEXT000051743650` has local DILA `body_chars=9883`, cached Judilibre `raw_text_chars=10001`, and three current chunks: summary 877 chars, body 5,490 chars, body 4,392 chars. The cache has two `decision_zones` rows today, both `ok`.
- The conclusion that offset overlay on existing chunks is not precise is sound. Judilibre offsets are character slices into Judilibre `text`, and current normalization slices that text into fragment text before caching (`crates/jurisearch-cli/src/main.rs:3771-3795`); applying those offsets to DILA `body` would be the wrong coordinate space.
- The preference for a parallel zone subsystem is defensible. It matches the existing v12 cache boundary and keeps the current whole-decision retrieval path and Phase 2 honesty invariant easier to reason about.

## Recommendation

Do not use the analysis as a design basis until the coverage numbers and normalized-zone-cache assumptions are corrected. After those fixes, the main architecture recommendation still looks sound: a first-class, official Judilibre-text zone retrieval subsystem, likely parallel to the existing decision index, with explicit Cassation/resolver coverage and provenance in every result.

VERDICT: FIXES_REQUIRED
