# work/08 — Central-ingest package distribution

Status: **COMPLETE.** All 11 phases (P0–P10) of the
[implementation plan](2026-06-26-central-ingest-package-distribution-implementation-plan.md)
are implemented, Codex-reviewed to a clean `GO`, committed, and pushed to `main`.

## What this delivers

A **producer → consumer signed package-distribution** system that lets a central ingest host
publish the jurisearch corpus to read-only client installations without those clients ever running
ingestion. Three delivery modes:

- **Baseline** — a full, self-sufficient corpus snapshot shipped as per-table **COPY-binary**
  payload files; the client loads rows then builds its own PK / BM25 / IVFFlat indexes.
- **Incremental** — a **JSONL diff** (upsert / delete / document-scoped `replace_set`) applied to
  the active generation in one cursor-gated transaction.
- **Re-baseline** — a full reissue that repoints only the affected corpus to a fresh generation
  behind stable views, leaving other corpora and the writable app schema untouched.

Every package is **Ed25519-signed and self-describing**; integrity, schema-version, embedding
fingerprint, builder version, and license entitlement are all **apply preconditions** — a failure
warns-and-rejects with a closed-vocabulary reject code and never moves the cursor.

## Architecture at a glance

- **Per-corpus generations behind stable views.** Hot indexed reads resolve the active physical
  generation schema via `corpus_state.active_generation`; `jurisearch_server.*` union views give
  transparent reads. `activate_generation` repoints views + advances the cursor in one short switch
  transaction (§7.4 / §15.1).
- **Change capture via an outbox** with a high-water **fence**, so incrementals are ordered and
  gap-free.
- **Writable app schema (`jurisearch_app` / `jurisearch_control`) outlives every generation** —
  byte-identical across incrementals and re-baselines.
- **Soft-validated references** — pin (exact `document_id`) vs as-of (`version_group` +
  `as_of_date`); the validator flags changed / vanished targets without hard FK coupling.
- **Trust & entitlement** — Ed25519 deterministic-seed signing, a trust anchor installed
  client-side, and a license token gating subscription corpora.
- **Size-driven planner** (§9.4 decision matrix) chooses baseline vs incremental catch-up from
  manifest-configured thresholds; an offline client converges to head in order, falling back to
  baseline past retention.

### New crates

| Crate | Role |
|---|---|
| `jurisearch-package` | wire contract: crypto (Ed25519 sign/verify), license tokens, reject codes |
| `jurisearch-package-build` | producer: baseline / incremental / rebaseline build, publish, remote manifest, `jurisearch-package` CLI |
| `jurisearch-syncd` | consumer: trust install, planner, apply, `status --json`, `jurisearch-syncd` CLI |

Storage primitives (generations, cursor guard, advisory locks, migrations v18–v24, trust + reference
models) live in `jurisearch-storage`.

## Phase ledger (all on `main`, Codex `GO` each)

| Phase | Commit | GO | Scope |
|---|---|---|---|
| P0 contract spine + corpus attribution | `643edfe` | r3 | corpus identity, schema-version stamp |
| P1 change-capture outbox | `6dec95c` | r4 | outbox + fence |
| P2 client storage topology | `f782f32` | r5 | generations behind views, `corpus_state` resolver |
| P3 baseline vertical slice | `fa3d128` | r5 | first end-to-end COPY-binary baseline |
| P4 incremental vertical slice | `269490f` | r3 | JSONL diff apply, outbox fence, compat stamps |
| P5 re-baseline + generation swap | `95c82c0` | r2 | full reissue, forward-supersession swap |
| P6 trust & gating | `84c80d1` | r2 | Ed25519 sign/verify + entitlement precondition |
| P7 planner + catch-up | `49231af` | r2 | §9.4 size-driven plan, ordered convergence |
| P8 reference model + validator | `c4c5403` | r2 | writable-app refs, pin/as-of resolution |
| P9 operated producer | `eec2292` | r2 | signed filesystem-published distribution loop + CLIs |
| P10 hardening / conformance / soak | `57f56e7` | r2 | reject-code conformance, concurrency soak, acceptance gate |

Plus the upstream design chain on `main`: analysis `3e04f75`, design `373a244`, conception
`5bdaa45`, plan `65c97b9`.

## Verification evidence

The repeatable acceptance evidence lives in
[`2026-06-27-acceptance-record.md`](2026-06-27-acceptance-record.md): each design invariant
(INV-1…INV-9, §13) is mapped to the deterministic test that proves it, the §15
implementation-measurement decisions are recorded as resolved facts, and the operated (long-running /
two-machine) acceptance forms are documented as ops evidence.

Key deterministic proofs (all skip cleanly without managed PG):

- `jurisearch-package-build/tests/{baseline,incremental,rebaseline}_loopback.rs` — full
  build→apply→read loops per mode.
- `jurisearch-storage/tests/generations.rs` — index inventory exists **before** activation; readiness
  resolves to the active generation; a rejected switch leaves `corpus_state` unchanged.
- `tests/trust_gating.rs` — tamper → `signature_invalid`, subscription → `missing_entitlement`.
- `tests/conformance_reject_codes.rs` — all 11 §6.3 reject codes produced by real paths, asserted on
  structured `SyncError::Reject { code, .. }`.
- `tests/reference_validation.rs`, `catchup_loop.rs`, `publish_distribution.rs`,
  `concurrency_soak.rs`.

## Process

Each phase ran the same loop: **Codex design consultation (`ask`) before coding → implement →
self-validate (build / fmt / clippy / tests) → Codex `review` → apply all findings → re-review to a
clean `GO` → commit on `main` → push.** ~7 design consultations and ~25 reviews; every per-phase and
upstream review is archived under [`reviews/`](reviews/). Design consultations repeatedly caught
architecture bugs before any code was written (a wrong primary key, a generation-label compatibility
gap, a read-only verify path, the retention model).

## Document map

- [`2026-06-26-…-conception.md`](2026-06-26-central-ingest-package-distribution-conception.md) — problem framing
- [`2026-06-26-…-design.md`](2026-06-26-central-ingest-package-distribution-design.md) — full design (the §-references above)
- [`2026-06-26-…-implementation-plan.md`](2026-06-26-central-ingest-package-distribution-implementation-plan.md) — the P0–P10 plan
- [`2026-06-26-…-prerequisites.md`](2026-06-26-central-ingest-package-distribution-prerequisites.md) — test-bed prerequisites
- [`2026-06-27-acceptance-record.md`](2026-06-27-acceptance-record.md) — invariant → test mapping, acceptance gate
- [`reviews/`](reviews/) — every Codex review (design + P0–P10, with re-review rounds)
- [`infra/`](infra), [`client-prerequisites/`](client-prerequisites) — server/client provisioning
