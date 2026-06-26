# Codex r2 review: central-ingest package-distribution implementation plan

Target reviewed: `work/08-jurisearch-server/2026-06-26-central-ingest-package-distribution-implementation-plan.md`

## R1 finding verification

1. **RESOLVED — package-sequence vs `change_seq` conflation.** The plan now makes the producer package catalog first-class in the component map (`:74`), seeds it in P3 (`:345-349`), fully specifies it in P4 with `included_change_seq_low/high`, chain link, build/publish status, and frozen high-watermark (`:393-405`), and keeps the Phase 1 outbox read API in `change_seq` bounds only (`:247-254`, `:269-270`).
2. **RESOLVED — missing replicated writers.** Phase 1 now explicitly names `projection/metadata.rs::insert_legi_metadata_roots_with_client` and the three `legislation_citations.rs` writers (`:236-242`), adds the citation commands and LEGI hierarchy backfill to the fixture set (`:262-265`), and requires an enumerated design-§4.2 coverage assertion (`:276-280`); these match the live writers in `projection/metadata.rs:29-87` and `legislation_citations.rs:75-220`.
3. **RESOLVED — `sync` mischaracterized as a stub to retire.** The component map and Phase 9 now state that existing `jurisearch sync` is kept as local official-source archive-delta ingest via `ingest.rs::sync_payload`, while `update`/`subscribe`/`corpus status` are net-new server-client package surfaces (`:82`, `:635-640`); I found no remaining plan text that deletes or retires local sync.
4. **RESOLVED — Phase 3 dependency on P1 helper.** The DAG note and P3 dependencies now state the precise narrow dependency: `package_change_log` plus the digest/read helper, not P1 hook completeness (`:156-160`, `:371-376`), which is consistent with a baseline snapshot data path.
5. **PARTIALLY RESOLVED — Phase 6 invariant citation.** The Phase 6 `Realises` line now correctly cites `§6.2, §6.3, §10, §11; INV-9` (`:535-537`), but the Phase 6 crypto deliverable still says `one trust path — conception §3, INV-5` (`:504-506`), while this plan defines `INV-*` as the design §13 invariants and design `INV-5` is app/control survival, not trust.

## Regression checks

- The new P3 package-catalog seed uses a read of the current `change_seq` high-watermark to establish the baseline cursor (`:345-349`). That does not contradict the "no outbox emission on the baseline path" claim because P3 explicitly excludes outbox hook emission and treats the read helper/watermark as the only P1 slice (`:371-376`).
- Phase dependencies, the DAG, the invariant matrix, and milestones remain aligned for the major flow: P1/P2 can proceed in parallel; P3 depends on P0/P2 plus the narrow P1 read/table slice; P4 follows P1/P3; P6 enforces INV-9 in the matrix (`:156-160`, `:371-376`, `:444-445`, `:530`, `:708-716`, `:771-781`).
- Grounding facts used by r1 remain intact in the revised text: schema version 17 is still the live storage baseline, `sync_payload` is implemented local archive-delta ingest, `serve` is treated as loopback-only shape rather than external hosting, and the plan's writer list now covers the previously omitted metadata/citation writers.

## New issues

### NIT - Phase 6 still contains a stale `INV-5` trust citation

Plan location: Phase 6 crypto deliverable (`:504-506`).

Because the plan's introduction defines `INV-1...INV-9` as the design document's invariants, the remaining `INV-5` citation points to the wrong contract. The text itself describes design `INV-9` and conception §9 invariant 5.

Concrete fix: change `one trust path — conception §3, INV-5` to either `one trust path — conception §3 and conception §9 invariant 5` or `one trust path — conception §3; design INV-9`.

VERDICT: FIXES_REQUIRED
