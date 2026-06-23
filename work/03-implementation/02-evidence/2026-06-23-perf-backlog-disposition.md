# Performance backlog disposition (2026-06-23)

Source: `reviews/2026-06-23-global-performance-codex-review.md` (codex global perf review). Each item
below is implemented (with codex review) or deferred with rationale. The production index
`index/phase1-freemium-20250713` is already built and no re-ingestion is planned, so ingestion/embed
optimizations have no current payoff and cannot be validated at 1.85M-row scale here — flagged where
relevant.

## Implemented (codex-reviewed GO, committed)

| Item | Change | Commit | Measured / validated |
|---|---|---|---|
| P0 #1 | France-LEGI gold SQL: JSONB `@>` + index-scan seed bounding | `5480388` | 139s+ (never finished) → ~27s on prod index |
| P1 #5 | Durable PG analytical/parallel config (work_mem, parallelism, shared_buffers) | `5480388` | enables the analytical CTEs |
| P2 #11 | Reuse query embedder across France-LEGI eval queries | `6a8f36a` | one client build vs ~192 |
| P1 #4 | Query-readiness manifest cache (+ stale-cache fix) | `d5376e8`, `ee3117a` | interactive search 3.80s → 1.93s; managed-PG tests |
| P1 #7 | Chunk-embedding inserts → staged set-based upsert (UNNEST) | `2e312ed` | guard/idempotency tests; ~3.7M execs → set-based |
| P1 #6 | Stream pending chunks page-by-page during full embed | `07d0577` | unit + live empty-path smoke; bounds peak memory |
| P0 #2 (partial) | Prepare LEGI projection statements once per ingest batch | `4b48aa3` | ~5.2M Parse round-trips → ~42k; managed-PG + target_spike_corpus |
| P2 #9 (adapted) | Env opt-out for finalize replay-snapshot refresh | `e41d3d8` | unit + cli_contract; default unchanged |

Also: P1 #3 dense-fusion direction was addressed separately by the weighted-RRF work
(`reviews/2026-06-23-retrieval-fusion-tuning.md`); ANN list/probe retuning would require rebuilding
the dense index and the calibration showed dense semantics (not ANN recall) is the known-item limit.

## Deferred (scale-gated / migration-risk, with rationale)

- **P0 #2 set-based executes (COPY/UNNEST staging for documents/chunks/edges).** The bigger win for
  the ~16.5M per-row executes, but a high-risk rewrite of the core projection that built the
  production index. It only pays off on a future re-ingest and cannot be validated at 1.85M-row
  scale here. The contained prepare-once part was done; the set-based rewrite is the follow-up when a
  re-ingest is planned (and can be validated at scale).
- **P2 #10 parallel archive parse.** Codex explicitly gates this on the set-based insert ("Do not do
  this before the insert path is set-based") — otherwise concurrent parsers just feed the per-row DB
  bottleneck faster. Deferred with P0 #2's set-based part.
- **P1 #8 normalized `TEXTELR.structure_links` table.** Optimizes only the full-resume hierarchy
  backfill (triggered when compatible members are skipped during resume — a rare path). It is a
  schema migration plus an ingestion-projection change: on the EXISTING index the new table would be
  empty, so the rewritten backfill query would find no structure links unless paired with a data
  migration or a JSONB fallback — i.e. it risks breaking the resume-backfill path on the built index
  without a re-ingest. Deferred until a re-ingest (which would populate the table) is planned.

## Notes
- The whole backlog was applied at the user's explicit direction ("do the full backlog"), with
  judgment to flag scale-validation gaps and adapt/skip where an item conflicts with a deliberate
  prior decision (P2 #9 opt-out vs the finalize-refresh cheap-status design).
