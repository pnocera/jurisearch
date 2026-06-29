# General Cross-Plan Re-Review (r2) — `work/10-next-plans`

Reviewer: Claude (Opus 4.8)
Date: 2026-06-29
Artifacts re-reviewed:

- `work/10-next-plans/01-makeitsimpletodeploy.md` (site-side deploy)
- `work/10-next-plans/02-auto-update-server-crons.md` (producer-side automation)

Prior review: `work/10-next-plans/reviews/2026-06-29-10-next-plans-general-claude-review.md`
(verdict `FIXES_REQUIRED`: BLOCKER 1 + WARN 1–4 + NIT 1–3).

Method: re-read both plans in full, then independently re-verified the code-level claims
that the prior findings turned on — the embedding config/fingerprint surface
(`crates/jurisearch-embed/src/config.rs`, `fingerprint.rs`, the CLI
`embedding_runtime/config.rs` + `pool.rs`), and the residual `ManagedPostgres`/`index_dir`
state — via CodeGraph and targeted greps, rather than trusting the prior summary.

Bottom line: **all eight prior findings (BLOCKER 1, WARN 1–4, NIT 1–3) are resolved in the
current text.** The producer DB topology contradiction is gone — both documents now commit
to CT 111 orchestrating against an external PostgreSQL on CT 110, with open decisions #3 and
#6 marked resolved. The two new items below are precision gaps surfaced *by* the now-coherent
external-PG decision; neither is a contradiction and neither makes the documented happy path
ship a broken-but-passing deployment.

---

## 1. Findings (ordered by severity)

### WARN 1 — Nothing in `02` owns bootstrapping the producer database on CT 110 (schema, extensions, roles), even though `producer.toml` carries admin-bootstrap fields that imply it

The producer now targets the external PostgreSQL on CT 110 and the config models a bootstrap
identity for it:

- `[database] admin_user = "postgres"`, `admin_database = "postgres"`,
  `admin_password_file = "/etc/jurisearch/secrets/postgres-admin-password"`
  (`02-…crons.md:269-271`).

But no phase or command in `02` consumes those admin fields to *create* the producer schema.
The phased plan goes straight from fetch (Phase 1) to "run ingest/enrich/embed/`producer_cycle`
against the external DB" (Phase 2). The producer DB on CT 110 needs `pgvector`/`pg_search`, the
storage migrations, and writer/owner roles to exist *before* the first ingest writes a row, yet
`02` assigns that to nothing. The only "provision" reference in `02` is the boundary section
pointing at `01`'s **site** `provision-db` (`02-…crons.md:625`) — which runs on a customer site
host against that site's own database, not on bear against CT 110. The producer DB and the site
DBs are distinct databases that merely share the name `jurisearch`, so `01` Phase 3 does not
cover CT 110's producer schema.

Verified against source: the migration runner is a method on `ManagedPostgres`
(`crates/jurisearch-storage/src/migrations.rs`, confirmed in the prior review and unchanged),
so running storage migrations against an external producer PG is genuinely new capability — the
same "new capability" `01` Phase 3 calls out for the site (`01-…deploy.md:449-454`). `02`
Phase 2 acknowledges the *execution* seam is new (`02-…crons.md:362-367`) but does not name the
*provisioning* step that must precede it. This is fail-closed (the first ingest against a blank
CT 110 errors immediately), so it is not a silent-wrong-thing risk — but it is a missing step an
implementer starting Phase 2 will hit.

Recommended fix: add an explicit producer-DB provisioning step to `02` (e.g. a
`jurisearch-producer provision-db --config <path>` in Phase 2, or a sentence stating Phase 2
reuses `01` Phase 3's external-PG migration applier extended to install
extensions/roles/migrations on CT 110) and a `done-when` that a blank CT 110 `jurisearch`
database can be turned into a ready producer DB before the first `update` run. Add a Phase 2
test that ingest against an unprovisioned external PG fails with a clear "provision the producer
DB first" diagnostic rather than a raw SQL error.

### NIT 1 — `02` separates `request_model` from `model_name` correctly, but does not name the config/env seam that carries `request_model` under the recommended shell-out path, where the obvious seam cannot

The fingerprint-vs-request split is now correct in the text: site `model_name = "bge-m3"`
(`01-…deploy.md:296`) and producer `model_name = "bge-m3"` + `request_model = "baai/bge-m3"`
(`02-…crons.md:311-314`), with the parity invariant and a cross-config parity test in the matrix
(`02-…crons.md:408-410, 562`). That fully resolves the prior WARN 1 fingerprint-mismatch (the two
example fingerprints are now both `bge-m3:1024:normalize:true`).

The residual gap is the wiring seam. Verified against source:
`EmbeddingConfig` has both `model` and `request_model` fields and `request_model()` falls back to
`model` when unset (`crates/jurisearch-embed/src/config.rs:47-48, 193-199`) — so the separation is
real in the struct. **But the single-endpoint config/env surface has no `request_model`:**
`EmbeddingConfigFile` exposes `model`, `dimension`, `normalize`, `base_url`, … with **no
`request_model` field** (`crates/jurisearch-cli/src/embedding_runtime/config.rs:28-48`);
`request_model` is only settable through the *pool* spec
(`EmbeddingPoolEndpointConfigFile`, `…/config.rs:52-56`, i.e. `JURISEARCH_EMBED_POOL`). Under the
recommended orchestration option A (shell out to `jurisearch ingest embed-chunks`,
`02-…crons.md:580-590`), the producer drives embedding through exactly that single-endpoint env —
which means setting the OpenRouter id via the obvious `JURISEARCH_EMBED_MODEL=baai/bge-m3` would
move the **fingerprint** model to `baai/bge-m3` and silently break parity, while there is no
`JURISEARCH_EMBED_REQUEST_MODEL` to carry it correctly.

Recommended fix: in `02` Phase 2, state the concrete seam — under shell-out (A), the producer
must pass `request_model` via the pool spec (`JURISEARCH_EMBED_POOL` /
`EmbeddingPoolEndpointConfigFile.request_model`), because the single-endpoint env
(`JURISEARCH_EMBED_MODEL`) has no `request_model` companion and would otherwise corrupt the
fingerprint. (Equivalently: if the producer keeps its own `[embedding]` block, document that it
maps `request_model` onto the pool path, not onto `JURISEARCH_EMBED_MODEL`.) This is the one
non-obvious wiring trap the parity test alone won't catch if the operator wires the subprocess by
hand.

---

## 2. Verification of the three re-review focus items

1. **Producer DB topology contradiction resolved in favor of CT 111 orchestrating against
   external PostgreSQL on CT 110 — CONFIRMED.**
   - `02` TL;DR now states "v1 producer storage uses an external PostgreSQL server … CT 111 …
     orchestrates the workflow, while ingest/enrich/embed/package DB work runs against the
     JuriSearch PostgreSQL 18 guest (`jurisearch`, CT 110)" (`02-…crons.md:50-54`).
   - Producer config points at the external DB: `[database] host = "192.168.0.110" port = 5432`
     (`02-…crons.md:266-272`).
   - Open decision #3 is now "Resolved for v1: use the external PostgreSQL producer database … CT
     111 … is the lightweight scheduler/fetch/orchestration host and CT 110 … is the PostgreSQL 18
     database host" and explicitly forbids a local `index_dir` fallback (`02-…crons.md:599-605`).
   - Open decision #6 is now "Resolved for current v1 deployment: a dedicated update-server CT
     (CT 111) orchestrates … while DB-heavy work targets the dedicated PostgreSQL CT (CT 110)"
     (`02-…crons.md:614-617`).
   - `01`'s infra snapshot agrees: CT 110 = PostgreSQL host, CT 111 = lightweight orchestrator
     that "drive[s] ingest/publish work against the JuriSearch PostgreSQL database on CT 110" and
     "should not store … database data, vector indexes" (`01-…deploy.md:115-116, 128-134`).
   - All residual `ManagedPostgres`/`index_dir` mentions describe the *current code limitation to
     be replaced* or a *no-fallback test* (`02-…crons.md:52, 264, 365, 411-412, 560, 586, 603`);
     none recommend it for v1. Matches the prompt's stated validation scan. The two plans no
     longer contradict each other — prior BLOCKER 1 is resolved.

2. **Producer `request_model` separated from storage `model_name`, and site query embedding is
   loopback-only — CONFIRMED (with the wiring-seam NIT above).**
   - Producer: `model_name = "bge-m3"` (canonical fingerprint) vs `request_model = "baai/bge-m3"`
     (provider id), with the explicit comment that `request_model` "must not change storage
     fingerprints" (`02-…crons.md:311-321`); Phase 2 invariant asserts the request model "does not
     change the stored fingerprint" and the test matrix computes parity from the example TOMLs
     (`02-…crons.md:408-410, 562`). Both example fingerprints now equal `bge-m3:1024:normalize:true`
     (verified against `fingerprint.rs:17-21`). Prior WARN 1 resolved.
   - Site loopback rule is now **unconditional**: "`embedder.base_url` is always required to
     resolve to loopback (`localhost`, `127.0.0.0/8`, or `::1`) for site deployments … A site
     config pointing query embeddings at OpenRouter or any other non-loopback provider is rejected
     before any file is written" (`01-…deploy.md:386-388`), backed by the config-parser test
     "external site embedder URL rejected" (`01-…deploy.md:681`). Prior WARN 2 resolved.

3. **`dist/update-server` Phase 2 references and cross-plan sequencing are unambiguous —
   CONFIRMED.**
   - Bundle contents reference is now qualified: "the package/ingest binaries required by
     `02-auto-update-server-crons.md` Phase 2 / open decision #1" and "shell-out dependencies if
     `02-auto-update-server-crons.md` Phase 2 / open decision #1 chooses that path"
     (`01-…deploy.md:632-635, 651-653`). Prior WARN 3 resolved.
   - The heavy-binary note is explicit: the update-server bundle "intentionally includes the heavy
     `jurisearch` binary plus `jurisearch-package`; the thin-cone invariant applies to `dist/cli/`,
     not to the update-server role" (`01-…deploy.md:635-637`). Prior NIT 3 resolved.
   - Build-ordering dependency is stated in both sequencing surfaces: `01` — "`./dist.sh` can emit
     `site-server` and `cli` bundles from this plan alone, but the complete `update-server` bundle
     is gated on `02-auto-update-server-crons.md` Phase 2-3 because that plan creates
     `jurisearch-producer` and the producer service/timer templates" (`01-…deploy.md:726-728`);
     `02` boundary section — "the `update-server` bundle is complete only after this plan's
     Phase 2-3 produce `jurisearch-producer` and producer service/timer templates"
     (`02-…crons.md:629-633`). Prior WARN 4 resolved.

Also confirmed resolved: NIT 1 (demo bge-m3 prerequisite now stated, `01-…deploy.md:76-79`),
NIT 2 (`sync.corpora` must "correspond to corpora the configured producer actually publishes …
For the current v1 producer this means `core` only", `01-…deploy.md:374-376`, with a distinct
doctor/catch-up failure at `:423`/`:685`).

---

## 3. Open questions / residual risks

1. **Producer DB provisioning ownership (WARN 1):** which command/phase installs
   extensions/roles/migrations on CT 110's producer DB, and is it `01` Phase 3's external-PG
   applier reused or a new producer step? Pin this before Phase 2 implementation.
2. **`request_model` seam under shell-out (NIT 1):** name the env/config path that carries it so
   an implementer does not regress parity through `JURISEARCH_EMBED_MODEL`. The cross-config
   parity test (`02-…crons.md:562`) guards the *example TOMLs* but not a hand-wired subprocess
   invocation.
3. **Orchestration boundary (`02` open decision #1, A vs B):** still correctly flagged as a Phase 2
   prerequisite (`02-…crons.md:577-590`); unchanged from the prior review and not a defect — just
   the decision both WARN 1 and NIT 1 wait on (it determines whether provisioning/embedding run
   in-process or via shelled-out `jurisearch`).
4. **Lock held across OpenRouter embedding round-trips** (informational, unchanged): the `core`
   update lock correctly spans enrich → embed → publish (`02-…crons.md:243-251, 444-458`), so
   external embedding latency is inside the lock; acceptable since the contract is bounded-wait,
   not skip.

## 4. Verification notes

What I inspected this pass:

- Re-read both plans end to end.
- Re-verified the embedding split at code level: `EmbeddingConfig.model` vs `.request_model` and
  `request_model()` fallback (`crates/jurisearch-embed/src/config.rs:47-48, 193-199`);
  `storage_embedding_fingerprint` = `{model}:{dimension}:normalize:{normalize}`, pooling excluded
  (`crates/jurisearch-embed/src/fingerprint.rs:15-22`); the single-endpoint `EmbeddingConfigFile`
  has **no** `request_model`, which exists only on `EmbeddingPoolEndpointConfigFile`
  (`crates/jurisearch-cli/src/embedding_runtime/config.rs:28-56`) and `EmbeddingEndpointPoolConfig`
  (`…/pool.rs:10-15`) — this is what grounds NIT 1.
- Greps over both plans for `index_dir|ManagedPostgres` (all remaining mentions are
  current-limitation / no-fallback-test, none recommend v1) and `request_model|provision`
  (producer `provision` appears only as a back-reference to `01`'s *site* provision-db, which
  grounds WARN 1).
- Trailing-whitespace scan over both files: none (matches the prompt's stated validation).
- The deeper code claims the prior review verified (KNOWN_SOURCES→core mapping, `producer_cycle`
  empty-window-still-refreshes-manifest, archive selection by `ArchiveTimestamp` not `change_seq`,
  syncd never embeds, thin-client cone, `serve-site` bind flags, external-PG migration runner is
  new capability) were confirmed accurate in r1 and the relevant plan text is unchanged; I did not
  re-derive every one, focusing this pass on the items the three focus areas and the two new
  findings turn on.

## 5. Verdict

The three contradictions this re-review was scoped to — producer DB topology, embedding
request/fingerprint separation + site-query loopback enforcement, and `dist`/Phase-2 cross-plan
sequencing — are all cleanly resolved, along with the remaining WARN/NIT items from r1. The two
remaining findings are additive precision gaps (a missing producer-DB-provisioning step; an
unnamed `request_model` wiring seam under shell-out), both fail-closed and both with concrete
fixes above. Neither is a contradiction and neither blocks starting implementation; they should
be folded into Phase 2 of `02` when option #1 is chosen.

VERDICT: GO
