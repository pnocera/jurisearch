# Review — 10-next-plans macro implementation plan

Date: 2026-06-29
Reviewer: Claude (Opus 4.8)
Artifacts:

- `work/10-next-plans/00-macro-implementation-plan.md`
- `work/10-next-plans/01-makeitsimpletodeploy.md`
- `work/10-next-plans/02-auto-update-server-crons.md`

Scope: planning quality and implementation readiness — contradictions across the macro/detailed
plans, unresolved blockers, dependency/milestone gaps, ambiguity around producer/site boundaries,
subscriptions, release assets, rebaseline, and embedding confidentiality, and partial propagation of
the now-resolved user decisions.

Overall: the three documents are coherent, internally consistent on the hard architectural points
(single `core` corpus, library-first orchestration, external producer PostgreSQL, confidentiality
split, single `core` update lock spanning ingest→publish, automatic rebaseline, repository-local
`./dist/`, role-distinct bundles excluding heavy assets). The resolved decisions are propagated well
into the macro plan and into `02`. The findings below are reconciliation gaps and under-specified
seams, not architectural defects. None block implementation of the v1 critical path.

---

## Findings (ordered by severity)

### WARN-1 — `01`'s shipped-artifacts inventory is stale: omits `jurisearch-producer`, still lists `jurisearch-package`

- **Where:** `01-makeitsimpletodeploy.md:232-237` (the "Add `jurisearchctl`" shipped-artifacts list)
  vs `00-macro-implementation-plan.md:264-269` (M6 update-server bundle) and
  `00-...:357-360` (resolved decision 9) and `02-...:653-664` (`01` Phase 9 update-server bundle).
- **Why it matters:** `01`'s canonical artifact inventory lists five binaries —
  `jurisearch`, `jurisearch-syncd`, `jurisearch-client`, `jurisearch-package`, `jurisearchctl` — and
  does **not** include `jurisearch-producer`, which is the central new binary the macro plan (decision
  9) and `02` make the owner of update-server administration. Conversely it still lists
  `jurisearch-package` as a shipped "producer/package tooling" artifact, yet no release bundle in `01`
  Phase 9 places `jurisearch-package` anywhere (the update-server bundle ships `jurisearch-producer`,
  not `jurisearch-package`; site-server ships `jurisearch`/`jurisearch-syncd`/`jurisearchctl`; cli
  ships `jurisearch-client`). With v1 being library-first ("do not shell out to `jurisearch-package`"),
  the role of a standalone `jurisearch-package` binary is now ambiguous: is it deprecated, retained as
  an ops/debug tool, or bundled somewhere? An implementer reading `01` alone would build the wrong
  binary set.
- **Fix:** Reconcile the `01` inventory: add `jurisearch-producer` (update-server admin/runtime), and
  either (a) drop `jurisearch-package` from the shipped list and note it is subsumed by the
  library-first producer path, or (b) explicitly state it ships as an operator/debug tool inside the
  `update-server` bundle. Make the inventory match `01` Phase 9's three bundles exactly.

### WARN-2 — Demo mode (`jurisearchctl demo …`) is a prominent `01` feature mapped to no macro milestone

- **Where:** `01-makeitsimpletodeploy.md:60-81` (and the demo commands `demo up|url|smoke|down`) vs
  `00-macro-implementation-plan.md:86-310` (milestones M0–M7 contain no demo deliverable or gate).
- **Why it matters:** `01` treats the single-host demo as required product proof ("a status-only demo
  is not sufficient product proof", must apply a fixture corpus, must start a real loopback bge-m3 for
  hybrid). It carries non-trivial requirements (socket ownership, `--local` socket-path rule, fixture
  with a documented known id, hybrid asset gating). The macro plan — which claims to define "the order
  of work, the cross-plan dependencies, the acceptance gates" — never schedules it. Its closest
  milestone, M5 ("Thin client and smoke acceptance"), lists `jurisearchctl site smoke` and the
  single-host acceptance script but not `demo up/url/smoke/down`. The demo work risks being dropped or
  rediscovered late.
- **Fix:** Add demo-mode deliverables/gates to a macro milestone (naturally M5, possibly M4), or state
  explicitly that demo mode is deferred/out of v1 scope. If kept, mirror `01`'s constraints (real
  binaries, fixture corpus, hybrid asset gating, explicit skip reason).

### WARN-3 — Conflicting "first target" guidance between the macro plan and `01`'s sequencing summary

- **Where:** `00-macro-implementation-plan.md:53-58` ("recommended first implementation target is the
  shared substrate plus producer vertical slice" — producer-first, risk-driven) vs
  `01-makeitsimpletodeploy.md:744-749` ("The first useful milestone is phases 1-4" — site-first).
- **Why it matters:** The two documents recommend opposite starting points. The macro plan deliberately
  reorders to attack the external-PostgreSQL/producer risk first and explains why; `01`'s own
  Sequencing summary still tells a reader to build the site deploy path (phases 1–4) first. A team
  following `01` in isolation would invert the macro's intended risk ordering. This is a
  partial-propagation gap: the macro's reordering was not reflected back into `01`.
- **Fix:** Add a one-line note at `01`'s Sequencing summary that the macro plan governs cross-plan
  ordering and that the v1 first target is the shared substrate + producer vertical slice (macro M1–M2),
  with `01` phases 1–4 sequenced per the macro milestones rather than ahead of the producer slice.

### WARN-4 — Pre-download subscription check depends on a manifest "entitlement listing" that no plan specifies adding, while `02` says the package format is unchanged

- **Where:** `01-makeitsimpletodeploy.md:534-538` (Phase 5: "fetch and verify its remote manifest and
  check whether the manifest's entitlement listing is open or covered…") and `01-...:320-323`; vs
  `02-...:663` ("Neither changes the package format, trust model, or query protocol").
- **Why it matters:** The subscription-aware pre-download gate requires the remote manifest to expose,
  before artifact download, whether a corpus is open or subscription-tier. If that entitlement metadata
  is not already present in the signed manifest, `01` Phase 5 needs a manifest field that `02`'s
  "format unchanged" statement forbids adding — a latent contradiction. For v1 it is non-blocking
  because `core` is open (the check trivially passes), but the seam is under-specified for the first
  real subscription add-on (INPI) and the plans disagree on whether the manifest can carry it.
- **Fix:** State explicitly whether the current signed manifest already exposes per-corpus entitlement
  (open vs subscription) for pre-download inspection. If it does, cite it so the gate has a concrete
  source. If it does not, reconcile `02`'s "format unchanged" claim (e.g. "manifest gains an
  advisory, non-authoritative entitlement listing; apply-time signed entitlement is unchanged").

### NIT-1 — Naming collision: planned `02-deploy-runbook.md` shares the `02-` prefix with `02-auto-update-server-crons.md`

- **Where:** `01-makeitsimpletodeploy.md:718` ("Add `work/10-next-plans/02-deploy-runbook.md`").
- **Why it matters:** `02-auto-update-server-crons.md` already owns the `02-` slot in this directory.
  A second `02-deploy-runbook.md` makes the numbering ambiguous and breaks the implied ordering.
- **Fix:** Renumber the runbook (e.g. `03-deploy-runbook.md`) or drop the numeric prefix.

### NIT-2 — Shared `jurisearch-deploy` crate hosts both the loopback-only site embedder validation and the producer's intentionally non-loopback OpenRouter config

- **Where:** `00-macro-implementation-plan.md:124` (single shared `crates/jurisearch-deploy`) and
  `01-...:396-399` (site rule: `embedder.base_url` must resolve to loopback) vs `02-...:314-316`
  (producer `base_url = "https://openrouter.ai/api/v1"`, non-loopback).
- **Why it matters:** The macro plan lists distinct "SiteConfig parser" and "Producer config parser"
  deliverables (M1), so this is not a contradiction — but it is an easy implementation trap: a shared
  validation helper that enforces loopback must be wired to the site parser only and must never run
  against the producer embedding config, or the producer's legitimate OpenRouter URL would be rejected.
- **Fix:** Add a one-line invariant that the loopback-only embedder rule is site-config-scoped and that
  the producer config explicitly permits external embedding providers for public-text document
  embedding.

### NIT-3 — Macro milestones omit several `01` commands (`site init`, `validate`, `render`, `embed render-service`)

- **Where:** `00-macro-implementation-plan.md:206-254` (M4/M5 deliverables) vs `01-...:372-376`
  (`site init`, `config-example`, `validate`, `render`) and `01-...:561-566` (`embed render-service`).
- **Why it matters:** Several `01` deliverables map cleanly into M1's "config rendering" and M4's
  embedder work but are not enumerated, so the macro's milestone deliverable lists are not a complete
  cross-walk of `01`'s command surface. Low risk because they are implied by M1/M4, but a checklist
  reader could miss them.
- **Fix:** Either note that M1 subsumes `01` Phase 1 config commands wholesale, or add the missing
  verbs to the relevant milestone deliverable lists.

### NIT-4 — Decision-9 command enumeration is non-exhaustive (omits `fetch`, `install`)

- **Where:** `00-macro-implementation-plan.md:357-360` lists
  `jurisearch-producer install|provision-db|update|status|rebaseline` but the resolved-decisions prose
  for v1 also relies on `jurisearch-producer fetch` (`02-...:354`) and `install` (present) /
  per-step flags.
- **Why it matters:** Minor; `fetch` is a first-class Phase 1 verb. The enumeration reads as canonical
  but is illustrative.
- **Fix:** Add `fetch` (and mark the list non-exhaustive) so the producer CLI surface is consistent
  across the macro plan and `02`.

---

## Open questions / risks

1. **Migration applier ownership.** The macro folds `01` Phase 3 (`site provision-db`) and the
   connection-based external-PostgreSQL migration applier into M1 (shared substrate), while `02`
   Phase 2 (`producer provision-db`) reuses that applier in M2. The ordering is sound, but confirm the
   applier is genuinely corpus/role-agnostic so both the customer-site DB and the producer DB on CT 110
   can share it without `ManagedPostgres` leakage (macro M2 gate at `00-...:170` already asserts this —
   keep it).
2. **Manifest entitlement source of truth** (see WARN-4): confirm whether entitlement metadata already
   lives in the signed manifest before the pre-download gate is implemented.
3. **`jurisearch-package` lifecycle** (see WARN-1): decide retained-as-debug vs deprecated, and pin its
   bundle, before M6.

## Verification notes

- `git diff --check` on the three files: the prompt reports it passed; not re-run here (no file edits
  were made by this review).
- Cross-checked the non-negotiable constraints from the prompt against the artifacts:
  - Repository-local `./dist/`, never `/dist`: asserted consistently (`00-...:21,63,281`;
    `01-...:643-648,681-682`). OK.
  - Distinct update-server/site-server/cli artifacts excluding heavy assets: consistent
    (`00-...:264-284`; `01-...:653-688`; `02-...:595`). OK.
  - Site query embeddings local-only; producer doc embeddings may use OpenRouter: consistent and
    repeatedly fenced (`01-...:340-360`; `02-...:310-333,420-432`). OK.
  - Update-server lightweight, DB-heavy work on external PostgreSQL CT 110; Storebox for runtime
    outputs: consistent (`01-...:115-177`; `02-...:50-60,624-630`). OK.
  - Automatic rebaseline must be explicit/recorded, no silent cross-baseline delta application:
    consistent (`00-...:344-347`; `02-...:526-553`). OK.
  - Subscription add-ons not hidden-URL-only; apply-time entitlement is the hard gate: consistent
    (`00-...:71-73`; `01-...:320-323`; `02-...` decision 2) — with the manifest-listing caveat in
    WARN-4.
  - Single `core` corpus for v1; single `core` update lock spanning ingest→publish: consistent and
    well-argued (`02-...:250-258,470-502`). OK.

---

VERDICT: GO
