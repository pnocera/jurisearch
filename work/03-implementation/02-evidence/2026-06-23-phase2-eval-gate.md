# Phase 2 evaluation gate (fail-closed full-juridic claim)

Date: 2026-06-23

`jurisearch status` now exposes a fail-closed **`phase2_gate`** (scope
`phase2_full_french_juridic_search`) that gates the "best-in-class French juridic search across
statutes and jurisprudence" claim. It mirrors the `phase1_gate` machinery: `claim_allowed` is the AND
of the gating checks; advisory checks (`gating:false`) are reported but never block.

## Gating checks

- `jurisprudence_corpus_present` — BOTH judicial (cass/capp/inca) AND administrative (jade) DILA bulk
  jurisprudence have a freshness-advancing completed run (read from `status.corpus_sources`).
- `index_query_ready` — the index passes projection + embedding coverage gates.
- `honest_zone_provenance` — every present bulk source reports `zone_accurate=false`; bulk never
  claims official Judilibre zones without enrichment. A source claiming `zone_accurate=true` fails it.
- `jurisprudence_eval_benchmark` — a passing jurisprudence eval benchmark supplied via
  `JURISEARCH_PHASE2_BENCHMARK`, **fully re-derived** against policy floors (status sets
  `state=passed` itself from the validation and never trusts the artifact's self-reported `state`,
  kept only as `artifact_reported_state`). The contract requires: `jurisdiction=france`, locked
  `bge-m3:1024:normalize:true` fingerprint, non-empty evidence; **production provenance**
  (`pipeline="production"`, non-empty `code_version` + `index_revision`, `sampled=false`, boolean
  `human_in_gold`/`llm_in_gold`); and BOTH jurisprudence families plus citation coverage:
  - `judicial_retrieval` `metric=recall_at_10` ≥ 0.50 over ≥ 15 queries (Cassation/appeal);
  - `administrative_retrieval` `metric=recall_at_10` ≥ 0.50 over ≥ 15 queries;
  - `decision_citation` `metric=decision_citation_accuracy` with a MEASURED per-identifier breakdown
    `by_identifier.{ecli,pourvoi,cetatext}`, each ≥ 0.95 accuracy over ≥ 10 queries (so coverage of
    every identifier kind is proven, not just declared).

Advisory: `pseudonymisation_preserved` — preserved verbatim by the juri parser (unit + real-archive
tests); advisory until the release benchmark asserts no re-identification.

## State

Fail-closed: with no jurisprudence corpus and no benchmark artifact, `phase2_gate.state=not_ready`,
`claim_allowed=false`. The full-juridic claim opens only when a jurisdiction-correct passing
benchmark is supplied AND the corpus/coverage/provenance checks pass. No benchmark has been run yet,
so the claim is correctly closed.

## What remains for an actual GO (not just the gate)

- Build + run the Phase 2 jurisprudence eval benchmark (Cassation + administrative retrieval tasks +
  decision-citation verification) through the production pipeline and emit the gate artifact.
- Authority-weight tuning (court tier / formation / publication / recency) is eval-driven and depends
  on that benchmark; it is intentionally not hard-coded.
- Zone-aware `fetch --part` (motivations/dispositif/moyens) remains heuristic for bulk; the official
  -zone fetch gate is met only by Judilibre zone enrichment.

Implementation: `crates/jurisearch-cli/src/main.rs` (`phase2_gate_payload*`, `phase2_benchmark_*`,
`PHASE2_*` floors) + `crates/jurisearch-core/src/schema.rs` (`Phase2GateResponse`). Unit tests cover
fail-closed-without-benchmark, both-families requirement, dishonest-zone rejection, benchmark
re-derivation (valid pass + rejects low-metric/wrong-jurisdiction/sampled), and gate-opens-on-pass;
a CLI contract assertion covers the no-index fail-closed status surface.
