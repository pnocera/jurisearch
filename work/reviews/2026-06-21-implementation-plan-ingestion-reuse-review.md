# Implementation plan review — ingestion-reuse update — `jurisearch`

Date: 2026-06-21
Scope: review of `work/03-implementation/IMPLEMENTATION_PLAN.md` *after* its 2026-06-21 update folding in the `juridocs` ingestion-reuse findings. Reviewed against the impact note (`work/notes/2026-06-21-ingestion-reuse-impact-on-implementation-plan.md`), the source reuse notes (`work/notes/2026-06-21-juridocs-ingestion-reuse.md`), the locked conception (`CONCEPTION.md §16` / `DECISIONS.md`), and the prior plan review (`work/reviews/2026-06-20-implementation-plan-review.md`). Question: did the update absorb the findings faithfully, completely, and without breaking the plan's internal consistency or any locked decision?

## Verdict

**Approve — the update is faithful, substantively complete, and internally consistent. Green-light, with one dependency-matrix correction (P2) and a short list of intentionally-or-accidentally deferred reinforcement details (P3) to confirm before execution.**

Every load-bearing item from the impact note is in the plan, and not superficially: the six gaps (G1–G6), the two resequencings (S1–S2), and the DILA-bulk decision (D1) each landed as concrete deliverables, new phase tasks with acceptance criteria, matrix rows, release-gate lines, cross-cutting tests, and risk-register entries. The update respects the plan's own discipline (testable acceptance per subsection) and reopens no locked decision. It also improves on the raw note in one place — see "What the update gets right," item 4.

What remains is small: one matrix dependency is under-specified, and a handful of *Reinforce*-class details from the note (temporal ID/field naming, fixture-set breadth, DTD-matrix caveat, a named hierarchy-survival test) were not pinned. None blocks execution; they are "confirm or consciously defer" items.

## What the update gets right

1. **Complete coverage of the substantive gaps.** G1 archive precedence/streaming → new task **0.5a** (lines 319–334) + W4 deliverable (95) + Phase 0 gate (394) + 2.5 reuse (657). G2 operational accounting → W3 schema (76–79) + new task **1.0** (407–425). G3 payload hashing → W3 (79), W4 (99), 1.1 (436–437), 1.2 (458). G4 token preflight/chunk-origin → W5 (116), 1.2 (455–457). G5 projection gating/safe-mode → W4 (102), W7 (158), 1.0 (417, 425). G6 ingest-health gates → W2 (52, 66), 0.2 (242, 249), 1.7 (545), Phase 1 gate (774–775, 783). This is thorough, not a checkbox pass.

2. **Resequencing is correctly encoded, not just asserted.** S2: **1.0 gates 1.1** in the matrix (190–191) and 1.1 is explicitly "Depends on 1.0." S1: publisher links are *emitted* in 1.1 (435, acceptance 446) and merely *materialized* in 2.3 (623) — so the Phase-2 graph layer becomes a pure derived-record build and no LEGI re-ingestion is forced. The matching risk line (812) closes the loop.

3. **D1 handled conservatively and within the lock.** DILA bulk jurisprudence is an **optional** adapter (2.2a, 602–616), gated behind "2.1 + 2.2 stable," explicitly *not* defaulted to accepted, with a Phase-2 scope note (568) and acceptance criteria that forbid it being mistaken for zone-accurate Judilibre data (614–615). This preserves the locked "official zones are primary chunk boundaries" rule (583) while still capturing the cheap coverage win.

4. **The update corrected the raw note rather than copying it.** The original reuse note (§5) put `parser_version`/`schema_version` etc. *in the member identity*. The plan instead carries them as **recovery-compatibility metadata, not identity keys** (W3 line 77; 1.0 lines 412–414), which is the right call — identity-keying those fields would multiply member rows and corrupt resume semantics. The impact note and plan are aligned on this.

5. **Phase-0 eval envelope avoids a false dependency.** 0.2 defines the ingest-health report envelope with *placeholder/pending* categories that "are not treated as passed" before W3/W4/W7 metrics exist (242, 249). This lets the gate harness exist in Phase 0 without depending on 1.0 — a clean way to honour "gates before features."

6. **Shared ownership of operational gates is sensible.** Ingest-health gating is split W2 harness / W3 metrics / W4 replay inputs / W7 runbooks (52, matrix 273-equivalent), rather than dumped on W2 alone. Matches the reality that W2 owned *retrieval* quality, not ingestion operations.

7. **Housekeeping is complete.** All six new risk lines present (806, 807, 811, 812, 813, 814); unit/integration/golden test sections updated (707, 711, 716, 724–727, 741); §10 next-work reordered to put the archive module (step 4) and ingest accounting (step 8) ahead of full-corpus work; header date, inputs, and §11 references updated.

## Findings

### P2 — Task 1.0's matrix dependencies omit the storage backend (0.3)

1.0 (lines 407–425) creates `ingest_run`/`ingest_member`/`ingest_error` **schema and repository APIs** in embedded Postgres and relies on the migration mechanism owned by W3. But the matrix (line 190) lists its dependencies as only **"0.5 + 0.5a."** Those are the LEGI spike and the archive module — neither provides the database, schema, or migration path. 1.0 cannot create operational tables before **0.3** (embedded Postgres spike + minimal schema + migration mechanics) exists.

The matrix is declared "authoritative for ownership," so this should be exact. Fix: change 1.0's "Depends on" to **"0.3 + 0.5 + 0.5a."** Severity is low in practice (1.0 is Phase 1, all of Phase 0 precedes it), but the dependency DAG is the part implementors read, and the prior 2026-06-20 review specifically flagged implicit intra-phase dependencies as the gap between "readable" and "executable."

### P3 — 1.7 depends on 1.0 only transitively

1.7 now **runs the ingest-health and replay-snapshot gates** (line 545) over the tables and metrics that 1.0 creates, but 1.7's matrix dependencies (line 197) are "1.1–1.6 + 0.7" — 1.0 is included only because 1.1 depends on it. That is technically sufficient but obscures the real data dependency. Consider listing 1.0 explicitly in 1.7's "Depends on" for clarity.

### P3 — Reinforcement-class details from the note were not pinned

The impact note's §6 "Reinforcements" are lower priority than the gaps, but three concrete recommendations did not make it into the plan text. They are not blockers; the ask is to fold a one-liner each or consciously defer:

- **Temporal naming/fixtures (note §4).** The canonical ID scheme `legi:<LEGIARTI>@<valid_from>`, the `version_group` key, and preserving raw `dateFin` as **`valid_to_raw`** are not pinned. 1.1 has "Build version groups" (433) and 0.5 preserves "raw source value" (309), so the *behaviour* is present but the field/ID contract is not named. Separately, 0.2's temporal fixtures (240: `valid_to = null` / 2016 boundary / same-day) were **not** widened to the note's fuller set (modified / abrogated / sentinel). Given temporal correctness is a release gate, pinning the fixture set is cheap insurance.
- **DTD checklist caveat (note §3).** Typed parser errors (0.5 line 308) and a DTD-required-field unit test (711) were added — good — but the note's "treat the `juridocs` DTD matrix as a *checklist*, then **re-verify against the current DTD** before making it authoritative" caveat is absent. Worth one line in 1.1 so the reused matrix isn't trusted blind.
- **Named hierarchy-survival test (note §9).** The note asked for an explicit test that `Code → Livre → Titre → Chapitre → Section → Article` survives ingestion. 1.2 acceptance covers "hierarchy-sensitive cases" (468), which is close but not the named structural-survival assertion.

### P3 — W6's deliverable list doesn't mention the enriched `status`

G2/G6 require `status --json` to surface ingest health, latest-run, coverage, and recovery warnings. This is specified in 1.0 acceptance (424) and the Phase 1 gate (783), but **W6's own deliverable list** (129–143) is still the bare command surface. Functionally fine; for traceability, add "ingest-health fields" to the `status` line under W6.

### P3 — 0.5a sits slightly above Phase 0's "validation spike" altitude

Phase 0's charter is "prove the stack." 0.5a (319–334) — full archive filename parser, baseline/delta planner, streaming reader, manifest artifact, configurable byte caps, ordering tests — reads more like production plumbing than a spike. It is defensible (it is load-bearing for full-corpus ingestion and the semantics are small), but to keep Phase 0 honest, scope 0.5a to the **planner/reader semantics + deterministic-ordering tests** needed to de-risk 1.1, not a complete production ingest path. A one-line scope qualifier would prevent gold-plating.

### Doc-hygiene nit (not a plan defect)

The impact note's bottom line says the granularity changes touch "W3/W4/W2/W7," but the plan also (correctly) modified **W5** (token-budget preflight, line 116). The plan is right; the note's summary undercounts. Worth a one-word fix in the note if it's kept as the canonical record.

## Consistency check — impact note → plan

| Impact-note item | Plan landing | Status |
|---|---|---|
| G1 archive precedence + streaming | 0.5a, W4 (95), gate (394), 2.5 (657), tests (707, 724), risk (806) | folded |
| G2 run/member/error accounting + resume/quarantine | W3 (76–79), 1.0 (407–425), risk (807), test (725) | folded (identity-key refinement applied) |
| G3 payload hashing + versioned text-assembly | W3 (79), W4 (99), 1.1 (436–437), 1.2 (458) | folded |
| G4 token preflight + chunk-origin provenance | W5 (116), 1.2 (455–457, 465–466), test (716), risk (813) | folded |
| G5 projection gating + safe-mode/rollback | W4 (102), W7 (158), 1.0 (417, 425), test (727) | folded |
| G6 ingest-health + replay gates | W2 (52, 66), 0.2 (242, 249), 1.7 (545), gate (774–775, 783), golden (741) | folded |
| S1 publisher links in Phase 1 | 1.1 (435, 446), 2.3 (623), risk (812), test (726) | folded |
| S2 accounting before full corpus | 1.0 gates 1.1 (190–191) | folded |
| D1 DILA bulk decision | scope note (568), 2.2a (602–616), matrix (200), risk (814) | folded (optional, flagged, not defaulted) |
| §4 temporal ID/field naming + fixture breadth | behaviour present; contract/fixtures not pinned | **partial (P3)** |
| §3 DTD re-verify caveat | error taxonomy + test present; caveat absent | **partial (P3)** |
| §9 named hierarchy-survival test | "hierarchy-sensitive cases" only | **partial (P3)** |
| 1.0 depends on storage backend (0.3) | matrix lists 0.5 + 0.5a only | **gap (P2)** |

## Locked-decision check

No violations. The update reopens nothing in §1: Rust-only runtime, Python-offline-only boundary, CLI-only surface, official-source-only ingestion, embedded Postgres + `pgvector` + `pg_search`, OpenAI-compatible embeddings, and the fallback precedence are all intact. DILA bulk (2.2a) is an official DILA source, kept optional and zone-honest, so it does not breach "official zones primary." Phase-scoped best-in-class claims (1.7 / 2.6 / §8) are preserved and even tightened (Phase 1 now also gates on ingest-health, line 556).

## Bottom line

Green-light. The ingestion-reuse update is a model of folding a findings note into an execution plan: every substantive gap became a task with acceptance criteria, the resequencings are encoded in the dependency DAG rather than merely asserted, and the one scope decision is parked as an explicit, conservatively-scoped option. Before the first build batch, correct 1.0's dependency to include **0.3** (P2) and decide whether to pin the four deferred *Reinforce* details (P3) or record them as conscious deferrals. With those, the plan remains a low-ambiguity, executable document.

## Reference trail

- Reviewed: `work/03-implementation/IMPLEMENTATION_PLAN.md` (2026-06-21 update)
- Against: `work/notes/2026-06-21-ingestion-reuse-impact-on-implementation-plan.md`, `work/notes/2026-06-21-juridocs-ingestion-reuse.md`
- Lock: `work/02-conception/CONCEPTION.md §16`, `work/01-design/DECISIONS.md`, `work/01-design/DESIGN.md`
- Prior: `work/reviews/2026-06-20-implementation-plan-review.md`
