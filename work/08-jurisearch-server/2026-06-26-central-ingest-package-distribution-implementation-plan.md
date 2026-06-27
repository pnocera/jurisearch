# Central ingest + packaged distribution — implementation plan

Date: 2026-06-26
Status: Implementation plan (sequencing, deliverables, acceptance — no code)
Builds against:
- `2026-06-25-central-ingest-delta-sync-analysis.md` — the **decisions** (the *why*)
- `2026-06-26-central-ingest-package-distribution-design.md` — the **contracts** (the *what*)
- `2026-06-26-central-ingest-package-distribution-conception.md` — the **principles** (DRY/SOLID, the *why it holds*)
Companion: `2026-06-26-central-ingest-package-distribution-prerequisites.md` — what must exist in
the real world for the built system to be deployable and testable.

> This plan **sequences the build**. It does not re-open any decision (all are settled in the
> analysis) and does not re-specify any contract (all are fixed in the design). Where it names a
> crate, module, table, or command, it is locating *where the design's contracts land in the existing
> workspace*, not inventing new architecture. Every phase cites the design section it realises and the
> existing code it extends. Section references like "§7.3" point at the **design** document; "C1…C9"
> are the design's *Constraints from the current code* table; "INV-1…INV-9" are the design's
> *Design invariants* (§13).

---

## 0. Method and how to read this plan

### 0.1 The shape of the work

The design is a **producer ↔ consumer system bound by a contract** (conception §8 DIP: both sides
depend on the package/manifest contract, not on each other). That dictates the build strategy:

1. **Fix the contract in code first** (a shared crate). Both sides compile against it; neither can
   drift (conception §3 DRY).
2. **Make change-capture additive and low-risk** (the outbox is a new table + hooks at projection
   boundaries — it does not disturb any existing query path).
3. **Prove one end-to-end vertical slice as early as possible** — a baseline built on the producer
   and applied on a real second machine — *before* layering network transport, physical media
   logistics, signing, entitlement, hosting, and scheduling on top.
4. **Then harden outward**: incrementals, re-baseline, the two-tier signed manifest, catch-up, the
   writable-layer reference model, the operated hosting/scheduler surface, and the acceptance gate.

The cardinal sequencing risk is the **server-namespace + generationed-schema change** (C1 → target):
it touches the storage layer everything else stands on. It is split deliberately — the *server-side*
namespacing and the *client-side* generation topology are different phases — so the invasive part is
contained and proven before the package machinery rides on it.

### 0.2 What "done" means per phase

Every phase below has **Acceptance** criteria that are *observable and testable* (a command, a digest
comparison, a rejection code, a survived re-baseline), never "code written." Each phase is committed
to `main` only after a Codex review returns GO and `cargo fmt` + `cargo check` are clean — consistent
with this project's review-per-phase discipline.

### 0.3 The two roles and their binaries

- **Producer** (one operator, one PostgreSQL): the existing ingest pipeline + new outbox + package
  builder + manifest/signing + hosting + scheduler. Producer-only commands extend `jurisearch-cli`
  (`package …`) and a new long-running producer process where a daemon is warranted.
- **Consumer** (many machines, read-only for server data): a **new local service** (working name
  `jurisearch-syncd`) that selects/verifies/applies packages and owns the local database lifecycle,
  plus the existing `jurisearch` CLI which only **reads** the stable views. The existing `serve`
  daemon (C9) is the *shape* to extend for the service, not the service itself.

---

## 1. Component → workspace map

Where each design component lands. New crates are marked **NEW**; the rest extend existing code.

| Design component (design §) | Lands in | Notes |
|---|---|---|
| **Package/manifest contract** — event kinds, package kinds, embedded + remote manifest types, reject codes (§5.2, §6, §6.3), identity helpers (§8.1) | **NEW `jurisearch-package`** | Pure types + serde + canonicalisation + a `Signer`/`Verifier` trait. No I/O, no DB. The DRY single source of truth (conception §3). Depended on by storage, cli, and the service. |
| **Crypto** — sign/verify, key_id/epoch (§11.2) | **NEW `jurisearch-crypto`** (or a `jurisearch-package::crypto` feature) | Concrete signing scheme behind the `Signer`/`Verifier` trait. Isolated so the algorithm is swappable (conception §6 LSP). |
| **Server namespacing** — `jurisearch_server` / `jurisearch_control` / `jurisearch_app`, generationed physical schemas, stable views (§4) | `jurisearch-storage` (`migrations.rs`, new `namespacing` + `generations` + `views` modules) | The C1→target move. Server-side migration + client-side generation/view DDL helpers. |
| **Change-log / outbox** (`package_change_log`, §5.1) | `jurisearch-storage` (new `outbox` module; hooks inside `projection/*.rs`, `zone_units.rs`, `decision_zones.rs`, `official_api_archive.rs`) | New table + emit calls *in the same transaction* as each projection mutation. |
| **Diff materialisation, `replace_set`, baseline/rebaseline build** (§5.3, §6.1) | **NEW `jurisearch-package-build`** (producer) + `jurisearch-storage` read helpers | Reads outbox + authoritative tables, writes signed artifacts. |
| **Producer package catalog** — per-corpus mapping from `package_sequence` → frozen `change_seq` window, chain link, build/publish status (§5.1 two-sequence-layers) | `jurisearch-package-build` (catalog table + writer in `jurisearch-storage`) | The bridge between global `change_seq` (outbox) and per-corpus package-sequence (manifests/cursor); prevents cross-corpus false `sequence_gap`. |
| **Remote + embedded manifest, signing, hosting/entitlement** (§6, §11) | `jurisearch-package-build` (manifest emit) + producer hosting surface | Hosting surface is net-new (C9 is loopback-only). |
| **Client service** — planner / verifier / applier / generation-manager / index-materialiser / reference-validator / cursor-authority (§7, conception §4.2) | **NEW `jurisearch-syncd`** (binary) depending on `jurisearch-package`, `jurisearch-storage`, `jurisearch-crypto` | The consumer brain. Each sub-responsibility is a module (SRP). |
| **Apply protocols** — incremental / baseline / rebaseline (§7.3, §7.4) | `jurisearch-syncd::applier` + `jurisearch-storage` apply primitives | Storage exposes transactional apply primitives; the service orchestrates. |
| **Index materialisation on client** (§9.3) | `jurisearch-storage` (extends `dense.rs`, `zone_units.rs` finalize) invoked by `jurisearch-syncd` | Reuses the existing IVFFlat-finalize/BM25 discipline; runs *inside* a generation before the view switch. |
| **Catch-up policy** (§9.4) | `jurisearch-syncd::planner` | Size/cost-driven; thresholds read from the manifest (no client upgrade to tune). |
| **Writable→server reference model + validator** (§8) | `jurisearch-storage` (`jurisearch_app` schema scaffolding + resolver) + `jurisearch-syncd::reference_validator` | Soft references + a resolver; no hard cross-schema FKs. |
| **Version gate** (§10) | `jurisearch-package` (compat compare) + `jurisearch-syncd::verifier`; reuses `SchemaVersionAhead` (C2) | The gate is a precondition check, not new transport. |
| **CLI surface** — producer `package …`; client `subscribe` / `update` / `corpus status`; reads via stable views | `jurisearch-cli` (`args.rs`, `dispatch.rs`, new modules) | All **new** commands. The existing `jurisearch sync` is a *different* feature — **local official-source archive-delta ingest** (`ingest.rs::sync_payload`, `ArchiveSyncFilter`) — and is **kept**, not retired; the new server→client package updater is `update`, a separate surface (the analysis already draws this distinction). |
| **Eval / conformance / soak harness** | `jurisearch-cli::eval` extension + a new integration harness | Loopback build→apply, golden digests, soak, gate. |

---

## 2. Workstreams (one responsibility each — conception §4 SRP)

These are *ownership lanes*, not a schedule; the phases (section 4) draw tasks from them. Each lane
changes for exactly one reason.

- **K — Contract & crypto.** The `jurisearch-package` types, canonicalisation, reject-code vocabulary,
  signing/verification. Changes only when the wire format or trust scheme changes.
- **P1 — Server data model & namespacing.** Server move into `jurisearch_server`, corpus attribution,
  generation/view DDL helpers. Changes only when the physical layout changes.
- **P2 — Change capture (outbox).** Emit semantic events at projection boundaries. Changes only when a
  new mutation semantics appears.
- **P3 — Package builder.** Materialise baseline/incremental/rebaseline artifacts from the outbox +
  authoritative tables, and own the **producer package catalog** (the per-corpus `package_sequence` ↔
  frozen `change_seq`-window mapping). Changes only when the package format changes.
- **P4 — Manifest, signing, hosting, entitlement.** Publish what exists and gate who may fetch.
- **P5 — Scheduled ingestor + proactive enrichment.** Turn today's lazy enrichment into upfront
  server-side enrichment; schedule ingest→build→publish.
- **C1 — Client storage topology.** Generations, stable views, control cursor, app namespace,
  generation registry.
- **C2 — Client service skeleton + cursor authority.** Lifecycle, the single writer of `corpus_state`.
- **C3 — Apply protocols.** Incremental, baseline, rebaseline appliers.
- **C4 — Index materialiser.** Finalize/BM25 inside a generation before activation.
- **C5 — Planner & catch-up.** Incremental-vs-baseline decision; gap-free ordering.
- **C6 — Verifier.** Signatures, digests, version gate, entitlement, pre/postconditions.
- **C7 — Reference model & validator.** Soft references + resolver for `jurisearch_app`.
- **X — Integration / eval / QA.** Loopback harness, conformance digests, soak, acceptance gate.

---

## 3. Sequencing — the phase DAG

```
P0  Contract spine + corpus attribution            (K, P1-partial)
      │
      ├──────────────► P1  Change capture / outbox          (P2)            [additive, parallel-safe]
      │
      ▼
P2  Client storage topology                         (C1)
      │   (generations · stable views · control cursor · app namespace · registry)
      ▼
P3  BASELINE vertical slice  ◄── first end-to-end   (P3-baseline, C2, C3-baseline, C4)
      │   producer builds a baseline → second machine loads gen, builds indexes, switches view, sets cursor
      ▼
P4  INCREMENTAL vertical slice                       (P3-incremental, C3-incremental)
      │   outbox → diff package → ordered gap-free apply · three event kinds · replace_set
      ▼
P5  Re-baseline + generation swap                    (C3-rebaseline, C1)
      │   scoped reload preserving jurisearch_app and other corpora
      ▼
P6  Trust & gating: two-tier manifest · signing · integrity · version gate · entitlement · reject codes
      │                                              (K, P4, C6)
      ▼
P7  Planner + size-driven catch-up + offline/gap     (C5)
      │
      ▼
P8  Writable-app reference model + validator         (C7)
      │
      ▼
P9  Operated producer: hosting surface · scheduler · proactive enrichment   (P4, P5)
      │
      ▼
P10 Hardening: atomicity/concurrency · conformance · soak · observability · ACCEPTANCE GATE  (X)
```

**Parallelism.** P1 (outbox) runs alongside P2 (client topology): they touch different sides. P3
reuses a **narrow slice of P1** — the `package_change_log` table and the §5.4 digest read helper — but
**not** P1's outbox *hooks* (a baseline carries no prior log), so the P1↔P3 coupling is just those two
artifacts, not hook completeness. Within P3+, the producer-build and client-apply halves of each
vertical slice can be developed concurrently against the P0 contract, then joined at the loopback
harness. P8 (reference model) can begin once P2 fixes the namespaces, and only *integrates* after P5.

**Why this order de-risks.** The invasive storage change is P1/P2, isolated and proven before any
package rides on it. The first time the two roles meet (P3) the surface is deliberately the *simplest*
applicable package — a baseline into an empty generation — so the contract is validated before the
diff machinery (P4) and before any trust/transport complexity (P6+). Signing and entitlement are
**stubs behind the verifier trait** until P6, so the loopback thread is end-to-end correct on data
before it is hardened on trust.

---

## 4. Phases

Each phase: **Goal · Scope/Deliverables · Acceptance · Dependencies · Risks · Realises (design §,
invariants).**

---

### Phase 0 — Contract spine and corpus attribution

**Goal.** Put the agreement between producer and client into code so neither side can drift, and give
the producer the one piece of attribution it currently lacks to build *per-corpus* packages.

**Scope / Deliverables.**

- **`jurisearch-package` crate (K).** Rust types + serde for: the three **event kinds** (`upsert`,
  `delete`, `replace_set`) (§5.2); the **package kinds** (`baseline`, `rebaseline`, `incremental`)
  (§6.1); the **embedded manifest** field groups — identity & ordering, compatibility gates,
  entitlement, integrity & signing, apply contract, payload layout (§6.2.2); the **remote manifest**
  (§6.2.1); the **machine-readable reject codes** as a closed enum (§6.3); the **per-corpus package
  sequence** type, kept distinct in the type system from the global `change_seq` (§5.1 "two sequence
  layers"); the **compatibility stamp set** (`schema_version`, `embedding_fingerprint`,
  `builder_versions`) (§10). Plus **canonicalisation** (deterministic manifest serialization for
  signing) and a `Signer`/`Verifier` **trait** (no concrete crypto yet).
- **Identity helpers (K).** Encode the two identities (§8.1): a specific version by `document_id`
  (`legi:<source_uid>@<valid_from>`, C4) and a logical article by `source_uid`/`version_group` +
  `as_of_date`. Reuse the existing canonical-id construction in `jurisearch-ingest`.
- **`response_id` exception (K).** Type the surrogate-key rule for `official_api_responses`: the
  producer id is the replicated key, carried verbatim, client inserts explicitly (§5.2).
- **Corpus attribution (P1).** Decide and record how each replicated row maps to a **corpus**
  (`core`, `inpi`, …). Add a corpus dimension to the replicated set — derivable from `source` where
  unambiguous, explicit column where not. This is a prerequisite for any per-corpus packaging and is
  the **only schema change in this phase** (additive column + backfill; no namespacing yet).

**Acceptance.**

- The crate round-trips every manifest and event-kind example from design §5–§6 through serde and a
  canonicalisation that is byte-stable across runs.
- A unit test asserts `change_seq` and `package_sequence` are **non-interchangeable types** (a
  compile-time guard against the §5.1 cross-corpus `sequence_gap` hazard).
- The reject-code enum is exhaustive against §6.3; a doc test maps each to its trigger.
- Every replicated row in a sample corpus resolves to exactly one corpus via the attribution rule;
  ambiguous rows fail loudly.

**Dependencies.** None. Starts first.

**Risks.** Corpus attribution for multi-source tables (e.g. citations spanning corpora) — mitigate by
scoping attribution to the *owning document's* corpus and recording the rule explicitly.

**Realises.** §5.1, §5.2, §6, §6.3, §8.1, §10; INV-1, INV-7, the DRY column-2 discipline (conception §3).

---

### Phase 1 — Change capture (the outbox)

**Goal.** Record the *semantics* of every server mutation transactionally, so incremental diffs are
computable without a uniform `updated_at` (C7) and without snapshot diffing or logical decoding as the
primary path (§5.1).

**Scope / Deliverables (P2).**

- **`package_change_log` table** with the §5.1 contract fields (`change_seq`, `corpus`,
  `ingest_run_id`, `table_name`, `op`, `scope_kind`, `scope_key`, `row_pk`, `row_hash`,
  `before_hash`/`after_hash`, optional `payload`, `builder_versions`, `embedding_fingerprint`,
  `schema_version`, `created_at`). Added as a storage migration (next `CURRENT_SCHEMA_VERSION`).
- **Emit hooks at every replicated-table writer (design §4.2 set), in the mutation's own
  transaction.** This list is fixed by the design's replicated set, not discovered later by grep:
  - LEGI documents/chunks/graph edges → in `projection/legi.rs` (the upsert paths at
    `insert_legi_documents_with_statements`): emit `upsert` with scope `document`.
  - Decisions → `projection/decisions.rs`.
  - LEGI metadata roots → `projection/metadata.rs` (`insert_legi_metadata_roots_with_client`,
    which upserts `legi_metadata_roots`): emit `upsert`.
  - Embeddings → `projection/embeddings.rs`, `zone_units.rs` insert paths.
  - Legislation citations → `legislation_citations.rs` (`insert_citation_occurrence_with_client`,
    `upsert_citation_resolution_pending_with_client`, `update_citation_resolution_with_client`,
    which write `decision_legislation_citations` and `legislation_citation_resolutions`): emit
    `upsert`. These FK into `official_api_responses.response_id`, so their apply order trails it (§5.2).
  - Derived rebuilds → `zone_units.rs::replace_zone_units_for_document` (the live writer at
    `zone_units.rs:123`) emits `replace_set` scoped to `document_id`; `decision_zones.rs` refresh and
    `hierarchy_backfill.rs` chunk-embedding clears emit the matching `replace_set`/`delete`.
  - Archived API bodies → `official_api_archive.rs` emit `upsert` carrying the producer `response_id`.
- **Outbox read API — keyed by `change_seq`, not package sequence.** The outbox lives in
  **global `change_seq`** space (§5.1: `change_seq` is a global build/audit order; the per-corpus
  **package sequence** is a *different* coordinate assigned only at build time). The read API is
  therefore `scopes changed for corpus C between change_seq (lo, hi]` — the authoritative "what
  changed"; payload materialisation is deferred to build time (§5.1). The mapping from a package
  sequence to its frozen `change_seq` window is owned by the **producer package catalog** introduced
  in P4 (§"Package catalog"), *not* by the outbox itself — the outbox never reasons in package-sequence
  space, which is exactly what prevents a cross-corpus false `sequence_gap` (§5.1).
- **QA backstop scaffolding** (§5.4): per-table row counts + ordered hash digests for a
  corpus/generation — a pure storage **read helper**, independent of the outbox hooks (it reads
  authoritative tables, not the ledger), reused by the P3 baseline loopback proof and by package
  postconditions. Built here, not yet wired.

**Acceptance.**

- Running the existing ingest commands (`ingest legi-archives`, `ingest juri-archives`,
  `ingest embed-chunks`, `ingest enrich-zones`, `ingest build-zone-units`, `ingest embed-zone-units`,
  `ingest collect-legislation-citations`, `ingest enrich-legislation-citations`, plus the LEGI
  hierarchy backfill) produces outbox rows whose `op`/`scope` correctly classify every mutation,
  verified against a known fixture ingest.
- A derived rebuild (`replace_zone_units_for_document`) produces exactly one `replace_set` row per
  document, never per-row deletes (INV-2).
- The outbox read API, replayed against a fixture, reconstructs the exact changed-scope set for a
  corpus between two arbitrary **`change_seq`** watermarks; a parallel snapshot-hash comparison agrees
  (backstop).
- Emit happens in the same transaction: a forced mid-batch rollback leaves **no** orphan outbox rows.

**Dependencies.** P0 (event-kind types, corpus attribution).

**Risks.** Missing a replicated-table writer → silent diff gaps. Mitigate with a coverage test that
asserts **every table in design §4.2's replicated set has exactly one owned writer emitting an outbox
row** — or is explicitly classified client-built (indexes) or control-only (`index_manifest`,
`schema_migrations`) and therefore intentionally hookless. The test is an enumerated assertion against
the §4.2 set, not a grep-discovered inventory, so a new replicated table cannot ship without a hook.

**Realises.** §5.1, §5.2, §5.3, §5.4; INV-1, INV-2.

---

### Phase 2 — Client storage topology

**Goal.** Establish the client-side physical layout that makes a re-baseline a *view repoint* and keeps
position and the writable layer outside every swap (§4, §7.2) — the structural precondition for every
apply protocol.

**Scope / Deliverables (C1).**

- **Namespaces** (§4.1): `jurisearch_server` (stable client-facing views/functions), per-corpus
  physical generations `jurisearch_server_<corpus>_gNNNN`, `jurisearch_control` (service-writable,
  never swapped), `jurisearch_app` (app-writable, preserved). DDL helpers in a new
  `jurisearch-storage::generations` module.
- **Stable-view indirection** (§4.3): for each replicated relation, a view in `jurisearch_server`
  selecting the active per-corpus generation (UNION ALL across corpora where a relation spans them).
  The CLI read path is migrated to reference `jurisearch_server.<name>` and becomes unaware of the
  generation suffix.
- **Control schema** (§7.2): `jurisearch_control.corpus_state` (one row per installed corpus:
  `active_generation`, `sequence`, `baseline_id`, `schema_version`, `embedding_fingerprint`,
  `builder_versions`, `last_package_id`, `last_package_digest`, `applied_at`) and a **generation
  registry** (`building`/`active`/`retired`).
- **Cursor authority module** (C2 seed): the *only* writer of `corpus_state` (conception §4.2).
- **Generation build/activate/retire primitives** (used by P3+): create a new generation schema,
  load into it, `CREATE OR REPLACE VIEW` switch, async retire — **not** `DROP SCHEMA` on the operated
  path (§7.4).

**Acceptance.**

- The existing CLI `search`/`fetch`/`context`/`cite`/`related` return identical results whether reading
  base tables (today) or the `jurisearch_server` views over a single seeded generation — a regression
  proof that the indirection is read-transparent.
- Two generations of the same corpus can coexist; flipping the view between them changes query results
  atomically with no reader seeing a half-state (INV-3, INV-4).
- `corpus_state` and `jurisearch_app` survive a `DROP`+recreate of a generation schema (INV-5).
- A measured note on view-vs-function overhead on the hot retrieval path (§4.3, §15.1) — the contract
  is fixed; the form is the one measured trade-off.

**Dependencies.** P0. Runs in parallel with P1.

**Risks.** View-layer cost on hot paths; UNION-ALL plans across corpora. Mitigate with the §4.3
fallback (stable SQL functions / minimal compatibility views for hot entry points) and measurement.

**Realises.** §4, §7.2, §7.4; INV-4, INV-5; conception §4.2 (position≠storage, server≠app).

---

### Phase 3 — Baseline vertical slice (first end-to-end)

**Goal.** The first time both roles meet: the producer builds a **baseline** for one small corpus; a
**second machine** loads it into a fresh generation, builds indexes, switches the view, and records the
cursor. Validates the whole contract on the simplest applicable package before any diff, trust, or
transport complexity.

**Scope / Deliverables.**

- **Baseline builder (P3).** From the authoritative server tables for one corpus, emit a `baseline`
  artifact: rows (per-file payload, encoding declared per file — §6.2.2) + schema (DDL incl. index
  definitions) + an embedded manifest with apply pre/postconditions, payload layout, and the dependency
  apply order (base before derived; embeddings after chunks/zone-units; `official_api_responses` before
  citation tables — §5.2/§6.2.2). Signing **stubbed** behind the `Signer` trait (real in P6).
- **Package catalog seed (P3).** Introduce the producer **package catalog** (detailed in P4) and write
  its first row for the baseline: the starting per-corpus `package_sequence`, `baseline_id`, and the
  **`change_seq` high-watermark of the baseline snapshot** — so the first incremental (P4) has a
  well-defined `lo` to diff from. (When P1 runs before P3 this watermark is real; if a baseline is cut
  before any outbox rows exist it is simply the current `change_seq` max.)
- **Baseline applier (C3-baseline).** In `jurisearch-syncd`: verify (stub-OK) → load into a new
  `jurisearch_server_<corpus>_g0001` → constraints → **index materialiser (C4)**: BM25 build + IVFFlat
  finalize at corpus-sized `lists`, reusing the existing finalize discipline (`dense.rs`,
  `zone_units.rs` finalize) → `ANALYZE` → `index_manifest` rows → validation → **view switch** →
  write `corpus_state` (cursor at the baseline sequence). Index work happens **inside the generation,
  before the switch** (§9.3, INV-6).
- **Service skeleton (C2).** Minimal `jurisearch-syncd`: read a local artifact path, run the baseline
  apply, expose `corpus status`. Extends the long-running-loopback shape of `serve.rs` (C9) but owns
  the DB lifecycle and does not depend on users closing CLI sessions.
- **Loopback harness (X).** Build a baseline on machine/DB A, apply on machine/DB B, diff the resulting
  `jurisearch_server` views against A's source via the QA digests (§5.4).

**Acceptance.**

- After apply on the second machine, every postcondition digest matches the producer's; CLI `search`/
  `fetch` over the applied corpus return results byte-identical to the producer for a fixture query set.
- The corpus is **never** advertised query-ready until indexes are built (INV-6): a probe during the
  long phase sees the old/empty state, never an unindexed generation.
- The view switch is atomic (readers see old-or-new); `corpus_state` reflects the baseline exactly once.
- Re-applying the same baseline is idempotent (cursor already at target → no-op).

**Dependencies.** P0, P2, and a **narrow slice of P1**: the `package_change_log` table (for the catalog
watermark seed) and the §5.4 digest/postcondition **read helper** (for the loopback proof). The baseline
**data apply** does *not* depend on P1's outbox **hooks** — a baseline is a snapshot with an empty prior
log (conception §2), so hook completeness is irrelevant to applying it. If strict phase isolation is
wanted, the digest helper and the `change_seq`-max read can equivalently sit in P0; the substantive
point is that no *outbox emission* is on the baseline path.

**Risks.** Index build time on a real corpus; first cross-machine extension/version mismatch surfaces
here (feeds the prerequisites doc). Mitigate by starting with a deliberately small corpus and recording
measured baseline size + build time (feeds catch-up thresholds in P7).

**Realises.** §6.1, §7.4 (load+switch), §9.3, §11.1 (sequence, stubbed); INV-3, INV-4, INV-6.

---

### Phase 4 — Incremental vertical slice

**Goal.** Turn the outbox into **ordered, gap-free, idempotent** incremental packages and apply them
on top of the baseline generation — the core diff machinery.

**Scope / Deliverables.**

- **Producer package catalog (P3 — the bridge between the two sequence spaces).** A producer-side,
  **per-corpus** catalog table that records, for each built package: `corpus`, `package_sequence`,
  `package_kind`, `previous_package_id`/digest, the **frozen outbox window**
  `included_change_seq_low`/`included_change_seq_high` (the `change_seq` range this package covers),
  `baseline_id`, build status, and publish status. This is the missing link the outbox alone cannot
  provide: the outbox lives in **global `change_seq`** space, while `from_sequence`/`to_sequence`,
  the remote manifest's `head`/`min_available`, and `corpus_state.sequence` live in **per-corpus
  package-sequence** space (§5.1). The catalog is the single place that maps one to the other. The
  builder **freezes** a `change_seq` high-watermark at build start (so concurrent ingest after that
  point lands in the *next* package, never duplicating or dropping scopes), reads the outbox by
  `change_seq` bounds derived from the catalog (P1's read API), and only then assigns the next
  per-corpus `package_sequence`. Package sequence appears **only** in manifests, the remote listing,
  and the client cursor check — never in the outbox query.
- **Incremental builder (P3-incremental).** For corpus C, take `lo = catalog.last.included_change_seq_high`
  and `hi = ` the frozen high-watermark; materialise the scopes changed in `(lo, hi]` into a diff
  carrying **all three event kinds**: `upsert` (inserts + in-place base-row updates incl. closing
  `valid_to` — C3/§5.2), `delete` (rare redactions), and document-scoped `replace_set` for derived
  tables (§5.3). Assign the next **per-corpus** package sequence; stamp
  `from_sequence`/`to_sequence`, `previous_package_id`/`previous_package_sha256`, compatibility stamps,
  postcondition digests; write the catalog row.
- **`replace_set` semantics (P3/C3).** Encode the §5.3 scope rules exactly: `zone_units`(+embeddings)
  per `document_id`; **`chunks_with_embeddings` per document whenever chunk membership/partitioning/body
  changes** (because `chunks` are BM25-indexed replicated rows and the live LEGI projection does not
  delete dropped chunks — `projection/legi.rs`); narrow `chunk_embeddings`-only replacement *only* when
  the chunk row set is unchanged. Carry the required builder/fingerprint stamps and `set_digest`.
- **Incremental applier (C3-incremental).** In one transaction against the **active** generation
  (§7.3): advisory lock + low `lock_timeout` → cursor check `sequence == expected_client_from_sequence`
  (mismatch → `sequence_gap`) → preconditions → apply in dependency order → postconditions → advance
  cursor → commit. New/changed rows are indexed by **row-level maintenance inside the apply txn**
  (no finalize for ordinary incrementals — §7.3/§9.3); an incremental carrying *new* index DDL builds
  before the cursor advances.
- **Idempotency & ordering.** A re-applied committed package sees the advanced cursor and is skipped; a
  gap is rejected, never skipped (INV-2/INV-3).

**Acceptance.**

- Producer mutates corpus → builds incremental → client applies → views match producer digests; the
  closing of a `valid_to` on a prior article version replicates (not just inserts) — the §5.2/INV-1
  "additive is not enough" proof.
- A `replace_set` that drops a chunk from a document leaves **no** stale chunk visible to BM25/fetch on
  the client (the §5.3 stale-chunk hazard test).
- Applying packages out of order is rejected with `sequence_gap`; applying them in order, including
  after a simulated offline gap, converges to the producer state (INV-2).
- Re-applying any committed package is a no-op (INV-3).
- **Concurrent-ingest catalog correctness (the §5.1 cross-corpus proof):** with `core` and `inpi`
  outbox rows interleaving in `change_seq`, and with new ingest landing *during* a `core` package
  build, the built `core` chain covers every `core` scope exactly once with **no duplicate and no
  dropped scope**, the next package's `from_sequence` is contiguous (no false `sequence_gap`), and the
  late-arriving rows appear in the *following* package — proving the frozen-watermark catalog isolates
  the two sequence spaces.

**Dependencies.** P1 (outbox + `change_seq` read API), P3 (a generation + cursor to apply onto, and the
package catalog).

**Risks.** The `chunks_with_embeddings` vs `chunk_embeddings`-only distinction is the subtlest
correctness point — give it dedicated fixtures (membership change, partition change, body correction,
pure embedding correction).

**Realises.** §5.2, §5.3, §7.3, §9.3 (incremental indexing); INV-1, INV-2, INV-3, INV-6.

---

### Phase 5 — Re-baseline and generation swap

**Goal.** Apply a **breaking-change** full reissue (re-embed / builder bump / corpus-rewriting
migration) as a **scoped reload of one corpus's server set**, provably preserving `jurisearch_app`,
`jurisearch_control`, and every other installed corpus (§6.1, §7.4) — the hardest atomicity story.

**Scope / Deliverables (C3-rebaseline, C1).**

- **Re-baseline builder (P3).** Same artifact shape as a baseline, marked `rebaseline`, triggered by a
  fingerprint/builder/breaking-schema change; raises `minimum_client_version` (§10).
- **Re-baseline applier.** The §7.4 three-phase protocol: (1) long phase — load the media baseline into
  a **new** `jurisearch_server_<corpus>_g<new>` off the live read path, build everything inside it
  (load → constraints → BM25 → IVFFlat finalize → ANALYZE → index_manifest → validation); (2) short
  switch — advisory lock + cursor check + `CREATE OR REPLACE VIEW` repointing **only** the rebaselined
  corpus's views + update its `corpus_state` row + commit; (3) async retire of the previous generation
  after a smoke check, with `retired` fallback if it cannot drop.
- **Per-corpus isolation proof (C1).** Re-baselining `core` does not read, merge, or touch `inpi` or
  any other generation (§4.1 per-corpus generations; conception §5 OCP "rebaselining core is closed
  over inpi"). *(P5 scope, per the review: physical generation tables, `corpus_state`, the unioned
  views, and `jurisearch_app` are per-corpus isolated and proven so. The global `index_manifest`
  dense-probe metadata is **still shared** across corpora — a re-baseline with different dense settings
  would change another corpus's probe defaults. Per-corpus dense-metadata isolation (scoping
  `index_manifest` by generation and resolving it via `corpus_state.active_generation`) is **deferred**;
  the operated profile uses one dense configuration across corpora, so this is not yet a live hazard.)*

**Acceptance.**

- A re-baseline of `core` with rows written in `jurisearch_app` referencing `core` data: after the
  swap, `jurisearch_app` is byte-identical and its soft references still resolve (or are flagged by the
  P8 validator) — never dropped (INV-4, INV-5).
- A second installed corpus (`inpi`) and its generation are untouched and continuously queryable
  throughout the `core` re-baseline.
- The switch confines reader impact to the short transaction; the long build phase does not block normal
  reads beyond shared extension/catalog work (§12).
- `DROP SCHEMA … CASCADE` is **never** used for the apply/switch path (that path repoints views with
  `CREATE OR REPLACE VIEW`); it is allowed only for the **locked cleanup of a registry-confirmed,
  retired, off-read-path private generation schema** (or a retriable half-built `building` schema),
  with a `retired` fallback if the cleanup cannot drop (§7.4). *(P5 review: wording reconciled with the
  committed operated-cleanup code, which already confines `CASCADE` to that case.)*

**Dependencies.** P2 (generations/views), P3 (baseline load + index materialiser).

**Risks.** The swap must hold under a concurrently-querying CLI. Mitigate with the low-`lock_timeout`
fail-clean discipline and a concurrency soak in P10.

**Realises.** §6.1, §7.4, §4.1; INV-4, INV-5, INV-6; conception §5 (OCP), §9.

---

### Phase 6 — Trust and gating

**Goal.** Make every artifact self-sufficient and verified: signing, the two-tier manifest, the full
integrity sequence, the version gate, and entitlement as an **apply precondition** — all warn-and-reject
with machine-readable codes and **no partial cursor movement** (§6.2, §6.3, §10, §11).

**Scope / Deliverables.**

- **Crypto (K).** Concrete `Signer`/`Verifier` behind the trait; `key_id`/epoch in every manifest
  (§11.2). Sign baselines, re-baselines, incrementals, and **both** manifests; verification on apply for
  network **and** media (one trust path — conception §3 DRY trust root / conception §9 invariant 5;
  design INV-9).
- **Two-tier manifest (P4).** The per-corpus **remote manifest** (§6.2.1: head/min-available sequence,
  active baseline, per-package compat/size, catchup ranges + policy, entitlement, signing) and the
  per-package **embedded manifest** (§6.2.2, already typed in P0) — both signed, the embedded one
  self-sufficient (ISP §7: a client never trusts the remote listing once it holds an artifact).
- **Verifier (C6).** The §11.1 ordered sequence: remote-manifest signature → artifact digest → embedded
  manifest signature/digest → per-file digests → post-apply row/set digests — every step warn-and-reject
  with the §6.3 code, cursor advances only after all pass.
- **Version gate (C6).** `minimum_client_version` compare → `client_too_old`; reuse the
  `SchemaVersionAhead` shape (C2) for `schema_ahead`. What forces a raised minimum is recorded (§10:
  schema migration / embedding-model change / builder bump).
- **Entitlement (C6).** Remote manifest filtered by subscription **and** independent embedded-package
  `entitlement_corpus`/`tier` check against a **locally installed license token** → `missing_entitlement`
  (§11.3). Entitlement is an apply precondition, not URL hiding.

**Acceptance.**

- A tampered artifact (flipped byte) → `signature_invalid`/`digest_mismatch`, no cursor movement.
- An out-of-date client → `client_too_old`; a DB ahead of the binary → `schema_ahead`.
- A client without the `inpi` token cannot apply an `inpi` package even given the bytes →
  `missing_entitlement`.
- Media and network artifacts verify through the **same** mechanism (no weaker media path).
- Every reject path returns one closed-vocabulary code (§6.3) and leaves the cursor untouched (INV-9).

**Dependencies.** P3, P4 (artifacts exist to sign/verify).

**Risks.** Key custody/rotation is an ops prerequisite (see prerequisites doc); the *code* fixes
`key_id`/epoch and a rotation-tolerant verifier, the *deployment* of keys is out of code scope (§15.4).

**Realises.** §6.2, §6.3, §10, §11; INV-9 (signed/self-sufficient artifacts; integrity, version, and
entitlement as apply preconditions; warn-and-reject with no partial cursor movement). Reinforces the
conception's trust and failure-vocabulary principles (conception §3, §7).

---

### Phase 7 — Planner and size-driven catch-up

**Goal.** Let the client choose **incremental catch-up vs fresh baseline** by cumulative byte size +
estimated apply cost (not chain length), and handle long-offline catch-up and gaps (§9.4).

**Scope / Deliverables (C5).**

- **Planner.** Poll the remote manifest → filter by entitlement + version → decide incremental vs
  baseline by the §9.4 rules (prefer incremental while no gap, all compatible, cumulative **compressed**
  diff below the manifest-configured ratio, estimated apply under budget; prefer baseline when
  `sequence < min_available_sequence`, ratios exceeded, the range crosses a fingerprint/builder reissue
  or breaking schema, or apply time exceeds media load).
- **Manifest-configured thresholds (§9.4/§15.3).** Read `catchup_policy` from the remote manifest so the
  policy is tunable without a client upgrade (conception §5 OCP: policy is data).
- **Gap-free catch-up.** Apply the retained chain in order; a missed package is caught up, never
  skipped; a client past the retention window restarts from a baseline root (§9.4, conception §9 INV-2).
- **Reference-client apply-cost model.** Calibrated against the measured baseline/diff sizes from P3/P4
  on the reference client profile (feeds the prerequisites doc's "reference client").

**Acceptance.**

- A client offline for N packages catches up by applying exactly the missing packages in order and lands
  on the producer head (digest match).
- A client past `min_available_sequence` is correctly routed to a baseline, not a doomed incremental
  chain.
- Flipping the manifest `catchup_policy` thresholds changes the planner's decision with **no** client
  rebuild.
- The estimated-apply-cost decision matches measured reality within a recorded tolerance on the
  reference profile.

**Dependencies.** P4 (incrementals), P6 (manifest, entitlement, version gate), P5 (baseline fallback).

**Realises.** §9.4; INV-2.

---

### Phase 8 — Writable-app reference model and validator

**Goal.** Let the future application layer reference server data **safely across re-baselines** via
soft, validated references — never hard cross-schema FKs (§8) — and re-resolve them after each apply.

**Scope / Deliverables (C7).**

- **Reference column contract** in `jurisearch_app` (§8.2): `target_kind`, `corpus`, `document_id` (pin
  a specific immutable version), `source`/`source_uid`/`version_group`/`as_of_date` (track a logical
  object), `resolved_document_id`/`resolved_generation`/`resolved_schema_version`/`validated_at`/
  `validation_status`. No hard FK into server physical tables.
- **Resolver** (§8.3): pin-by-`document_id` for exact evidence; `source_uid`/`version_group` + `as_of`
  selecting the row whose validity window contains the date for logical articles; document/article-level
  + offset/quote-hash anchoring for chunk/zone references (not `chunk_id`/`zone_unit_id`).
- **Reference validator (C7).** Run by the service **after each apply** and on demand (§8.2): re-resolve
  references, mark missing/changed targets explicitly, leave pin/retarget/warn to app UX. This is the
  *only* genuinely-deferred-to-background step after the cursor advances (§7.1).

**Acceptance.**

- An app row pinning a specific `document_id` keeps resolving across an incremental update **and** a
  re-baseline of that corpus (INV-4); supersession retaining old rows is what makes this hold (§8.1).
- An app row tracking a logical article by `source_uid`/`version_group` + `as_of_date` resolves to the
  correct version before and after a new version lands.
- After a re-baseline, the validator flags exactly the references whose targets changed/vanished and
  nothing else; `jurisearch_app` is never mutated by the server reload itself.

**Dependencies.** P2 (namespaces), P5 (re-baseline to validate against).

**Risks.** This is the boundary for an out-of-scope app layer; keep the contract thin (conception §7
ISP) so the app's internal design stays free.

**Realises.** §8; INV-4, INV-7; conception §7, §8 (DIP: app depends on identity, not physical rows).

---

### Phase 9 — Operated producer: hosting, scheduler, proactive enrichment

**Goal.** Stand up the producer as an operated service: authenticated TLS hosting of signed artifacts +
remote manifests, a scheduled ingest→build→publish loop, and the **proactive** enrichment the
replicate-upfront decision requires (§5/§9.1, §11.4).

**Scope / Deliverables.**

- **Hosting surface (P4).** Authenticated, TLS-protected serving of signed packages + per-corpus remote
  manifests, with **credential-based per-corpus entitlement** at the edge *and* (already, from P6) as an
  apply precondition (§11.4). Net-new — `serve` is loopback-only (C9). (Concrete CDN/object-store is an
  ops decision, §1.3/§15.5 — the code fixes the contract: signed artifacts, TLS, per-corpus auth.)
- **Scheduler (P5).** Drive the existing ingest commands → outbox → `package build` → sign → publish on
  a cadence; assign per-corpus package sequences; maintain the retention window + `min_available_sequence`
  + precomputed `catchup_ranges` (§9.4).
- **Proactive enrichment (P5).** Convert `decision_zones` from a lazy on-demand cache (today) to
  **upfront server-side** enrichment, bounded by provider coverage (Judilibre `cass`+`inca` only; the
  rest stays `zone_accurate=false`) (§9.1). Enriched `decision_zones` + `official_api_responses` flow
  through the outbox and into packages; **clients make no PISTE/Judilibre calls** (a client
  `fetch --part` is served from the local store).
- **Producer `package` CLI (P3/P4).** `package build {baseline|incremental|rebaseline} --corpus …`,
  `package sign`, `package publish`, `package list`, `package verify` (a staging-apply QA loop, §5.4).
- **Client update CLI (C2/C5).** `subscribe <corpus>` (install a license token), `update [--corpus]`
  (the planner/apply loop), `corpus status` (cursor/generation/compat). These are **net-new** and do
  **not** replace `jurisearch sync` — which remains the local official-source archive-delta ingest path
  (`ingest.rs::sync_payload`); the two are distinct features the analysis explicitly separates. (If the
  name collision is judged confusing, a *separate* rename/deprecation plan can address it without
  removing the working local archive-delta functionality.)

**Acceptance.**

- A scheduled run ingests a delta, builds + signs + publishes an incremental, updates the remote
  manifest, and a subscribed client `update` applies it end-to-end over TLS with entitlement enforced.
- A client without a corpus subscription is refused at the edge **and** would be refused at apply
  (defense in depth, §11.3).
- `decision_zones` for `cass`/`inca` are enriched upfront and shipped; a client serves `fetch --part`
  with zero upstream calls; out-of-coverage decisions correctly carry `zone_accurate=false`.
- The producer publishes a coherent retention window; a client at the window edge is routed correctly
  by the P7 planner.

**Dependencies.** P5, P6, P7 (the full apply/verify/plan stack must exist before operating it).

**Risks.** Proactive enrichment expands ingestion scope and spends PISTE/Judilibre quota upfront (a
prerequisite); the `official_api_responses` archive is large (bandwidth/storage line item, §9.2).
Redistribution licensing for restricted corpora is **deferred** (§1.3) but gates shipping `inpi`/licensed
tiers — flagged in the prerequisites doc, not built here.

**Realises.** §5 (proactive), §9.1, §9.2, §9.4 (retention), §11.3, §11.4.

---

### Phase 10 — Hardening, conformance, soak, acceptance gate

**Goal.** Prove the invariants hold under concurrency and at scale, give the system observability, and
gate the claim.

**Scope / Deliverables (X).**

- **Atomicity/concurrency soak (§12).** Apply incrementals and a re-baseline while the CLI continuously
  queries the same DB; assert readers see old-or-new only, the advisory lock + low `lock_timeout` fail
  clean (never stall behind a long query), and the cursor is the deterministic retry authority.
- **Conformance suite.** Golden producer→client digests for: baseline, incremental (all three event
  kinds), `replace_set` (the four chunk-scope cases), re-baseline (with `jurisearch_app` survival),
  catch-up, every reject code (§6.3), version gate, entitlement, tamper.
- **Observability.** Client `corpus status` (cursor/generation/sequence/compat stamps/last digest);
  apply logs with reject codes; producer build/publish logs + retention/catch-up state. Structured to
  stderr/JSON, never mixed into query stdout (matching the project's diagnostics discipline).
- **Acceptance gate.** A documented, repeatable end-to-end run on the **minimum viable test bed** (see
  prerequisites doc) exercising every INV-1…INV-9, with measured baseline/diff sizes feeding the final
  catch-up thresholds (§9.4/§15.3).
- **Implementation-measurement items resolved (§15).** Record the view-vs-function choice (§15.1), the
  per-file payload encoding (§15.2), the final catch-up numbers (§15.3) — each was left to measurement by
  the design.

**Acceptance.**

- All nine design invariants (§13) have a passing conformance test (the INV→phase matrix, section 5).
- A 24h+ soak with interleaved updates/queries shows no half-applied read, no stalled reader, no cursor
  divergence, no orphaned derived rows.
- The acceptance run reproduces producer state on a fresh second machine from media baseline + network
  incrementals, survives a re-baseline with app data intact, and refuses every malformed/unauthorised/
  out-of-order input with the right code.

**Dependencies.** All prior phases.

**Realises.** §12, §13 (all invariants), §15; conception §9 (the invariants the principles protect).

---

## 5. Invariant → phase enforcement matrix

Each design invariant (§13) is *enforced* in the phase that builds it and *proven* in P10 conformance.

| Invariant (§13) | Enforced in | Proven by |
|---|---|---|
| INV-1 Three event kinds mandatory (incl. in-place base-row updates) | P1, P4 | `valid_to`-close replication test (P4); conformance |
| INV-2 Derived rebuilds = document-scoped `replace_set`; ordered gap-free | P1, P4 | stale-chunk test, sequence_gap test (P4); conformance |
| INV-3 Atomic, no partial movement; idempotent via cursor | P3, P4 | re-apply no-op; concurrency soak (P10) |
| INV-4 Per-corpus generations behind stable views; re-baseline repoints only the affected corpus | P2, P5 | inpi-untouched-during-core-rebaseline (P5) |
| INV-5 `jurisearch_app` + `jurisearch_control` outlive every generation | P2, P5 | app-survives-rebaseline (P5/P8) |
| INV-6 Index materialisation part of apply/activation, never after cursor advance | P3, P4, P5 | not-query-ready-until-indexed probe (P3) |
| INV-7 Soft validated references; pin vs as-of | P8 | resolve-across-rebaseline (P8) |
| INV-8 Client builds indexes; prebuilt is a fenced physical variant only | P3, P4 | client-build finalize (P3) |
| INV-9 Signed, self-sufficient; integrity/version/entitlement are apply preconditions; warn-and-reject | P6 | tamper/old/unentitled reject tests (P6) |

---

## 6. Cross-cutting concerns

- **Package-format versioning.** The embedded manifest carries `package_format_version` (§6.2.2); the
  contract crate owns its evolution. A format bump is itself a `minimum_client_version` event — the gate
  protects the wire format, not just the data.
- **Schema migration of the contract.** The outbox table, control schema, and namespaces are storage
  migrations advancing `CURRENT_SCHEMA_VERSION` past 17; each must keep the existing `SchemaVersionAhead`
  guard meaningful (C2).
- **Testing strategy by layer.** Contract crate: serde/canonicalisation/property tests. Storage:
  managed-Postgres integration tests (the project's existing pattern). Producer↔consumer: the loopback
  harness (build on A, apply on B, digest-compare). Whole system: conformance + soak + the acceptance run.
- **Diagnostics discipline.** Service and producer logs go to stderr/structured JSON; query stdout stays
  pure (matching the established CLI contract).
- **Reuse, not reinvention.** The index materialiser reuses `dense.rs` / `zone_units.rs` finalize; the
  `replace_set` applier mirrors `replace_zone_units_for_document`; the service extends the `serve.rs`
  long-running shape; identity reuses the ingest canonical-id construction (conception §3 DRY: reuse the
  DB's own set-replacement semantics rather than inventing a parallel one).

---

## 7. Explicitly deferred (not built by this plan)

Carried verbatim from the design's out-of-scope (§1.3) and "decided to decide later":

- **Redistribution licensing** of derived data and raw upstream API bodies — gates shipping
  restricted/subscription corpora; the subscription tier is the eventual enforcement point. Flagged as a
  real go-live gate in the prerequisites doc.
- **The writable application layer's internal design** — P8 fixes only the boundary it must respect.
- **Concrete hosting topology / key custody / rotation cadence** — §15.4/§15.5 ops decisions; the code
  fixes the contracts (signed, TLS, per-corpus entitlement, `key_id`/epoch).
- **The prebuilt-index physical variant** (§9.3) — rejected as the default; retained only as a fenced,
  engine/arch-gated variant, not built unless a measured need appears.

---

## 8. Risk register (top items)

| Risk | Phase | Mitigation |
|---|---|---|
| Missed projection boundary → silent diff gap | P1 | Coverage test asserting every replicated-table write emits an outbox row; snapshot-hash backstop (§5.4) |
| `chunks_with_embeddings` vs `chunk_embeddings`-only confusion → stale BM25 rows | P4 | Dedicated four-case fixtures; the §5.3 stale-chunk acceptance test |
| View-indirection cost on hot retrieval paths | P2 | §4.3 fallback to stable functions / minimal compat views; measure (§15.1) |
| Cross-machine extension/major-version/arch mismatch | P3 | Client-build default (§9.3) sidesteps relation-file transport; pin extension availability in prerequisites |
| Re-baseline swap under live CLI load | P5, P10 | Low-`lock_timeout` fail-clean; concurrency soak |
| Proactive-enrichment quota + `official_api_responses` size | P9 | Bounded to provider coverage; size measured; redistribution licensing deferred but flagged |
| Key custody / rotation is out-of-code | P6, P9 | Code fixes `key_id`/epoch + rotation-tolerant verify; ops owns custody (prerequisites) |

---

## 9. Milestone checkpoints (vertical-slice gates)

1. **M0 — Contract compiles, corpus attribution resolves** (end P0).
2. **M1 — Outbox captures every mutation semantics** (end P1).
3. **M2 — Reads are generation-transparent** (end P2).
4. **M3 — Baseline reproduces producer state on a second machine** (end P3) — *first end-to-end*.
5. **M4 — Incrementals replicate inserts, in-place updates, and replace_sets, gap-free** (end P4).
6. **M5 — Re-baseline preserves app + other corpora** (end P5).
7. **M6 — Every artifact signed; integrity/version/entitlement enforced** (end P6).
8. **M7 — Size-driven catch-up + offline recovery** (end P7).
9. **M8 — Soft references survive re-baseline** (end P8).
10. **M9 — Operated producer publishes; subscribed client auto-updates over TLS** (end P9).
11. **M10 — Acceptance gate green on the minimum viable test bed** (end P10).

---

## 10. Bottom line

The architecture is settled and the contracts are fixed; this plan is purely about **build order**. It
puts the producer↔client agreement into a shared crate first (so the two sides cannot drift), makes
change-capture an additive low-risk outbox, and isolates the one invasive storage change (server
namespacing + client generations) before any package rides on it. It then drives a **baseline vertical
slice to a real second machine as the first end-to-end proof**, and only afterward layers on
incrementals, the scoped re-baseline that preserves the writable layer, signing/entitlement/version
gating, size-driven catch-up, the soft-reference model, and the operated hosting/scheduler/enrichment
surface — closing on a conformance + soak + acceptance gate that proves all nine design invariants. The
data plane is friendly (temporal corpus, deterministic version-specific PKs, the existing finalize
discipline, `pg_search` everywhere); the substantive build is the **package pipeline**, the **client
apply protocols**, and the **generationed schema + view switch** — exactly where the design said the
real work lives.
