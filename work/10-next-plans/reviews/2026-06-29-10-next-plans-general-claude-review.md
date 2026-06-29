# General Cross-Plan Review — `work/10-next-plans`

Reviewer: Claude (Opus 4.8)
Date: 2026-06-29
Artifacts reviewed:

- `work/10-next-plans/01-makeitsimpletodeploy.md` (site-side deploy)
- `work/10-next-plans/02-auto-update-server-crons.md` (producer-side automation)

Method: read both plans in full, then verified the concrete code claims against the
repository via CodeGraph and targeted greps (crate manifests, `serve-site`/syncd CLI
surfaces, the embedder fingerprint, the migration runner, `producer_cycle`, the DILA
archive parser/planner, the existing systemd units). Findings below cite plan
file:line and, where relevant, the verifying source location.

Overall: the two plans are unusually well-grounded — almost every capability either
plan "builds on" was confirmed to exist exactly as described (see Verification notes).
The cross-plan boundary (single artifact = the signed remote manifest + packages; `01`
owns `./dist.sh`; `02` defines the update-server contents) is coherent. However, there
is **one hard contradiction between the two plans on the producer database topology**
that blocks implementation of `02`'s recommended v1, plus several precision gaps that
would let an implementer build a non-working (but fail-closed) deployment.

---

## 1. Findings (ordered by severity)

### BLOCKER 1 — The two plans contradict each other on the producer DB topology; `02`'s recommended v1 is not implementable on the host `01` designates

- `01` fixes the producer host and its DB model in the infra snapshot:
  - CT 111 is "Lightweight CT: 2 cores, 4 GB RAM, 1 GB swap, 32 GB rootfs"
    (`01-…deploy.md:113`).
  - "It should download official legal-source archives to Storebox and **drive
    ingest/publish work against the JuriSearch PostgreSQL database on CT 110**. It should
    not store … **database data, vector indexes** … on its 32 GB root disk."
    (`01-…deploy.md:128-131`).
  - The captured bootstrap env on CT 111 already points the producer at the **external**
    DB: `JURISEARCH_POSTGRES_HOST=192.168.0.110` / `…PORT=5432`
    (`01-…deploy.md:170-171`).
- `02` fixes the opposite producer DB model:
  - "ingest, enrichment, embedding, and `producer_cycle()` all run against a
    `ManagedPostgres` rooted at an index dir … v1 reuses that model; **it does NOT
    connect to an external `postgres://` URL** (that capability only exists on the SITE
    side, per 01-…deploy Phase 3)." (`02-…crons.md:251-254`).
  - `index_dir = "/srv/jurisearch/producer-index"   # ManagedPostgres data dir`
    (`02-…crons.md:256`).
  - Open decision #3 recommends "v1 = `ManagedPostgres`/`index_dir`"
    (`02-…crons.md:568-572`); open decision #6 still lists "Where the producer runs … a
    dedicated producer host?" as undecided (`02-…crons.md:581-582`) even though `01`'s
    infra has already answered it (CT 111).

Why it matters (verified against source): `ManagedPostgres` is a **`pg_ctl`-owned,
local-data-dir** instance — `crates/jurisearch-storage/src/runtime.rs` describes
`ManagedPostgres` as "The self-managed (`pg_ctl`-owned) PG" and `run_migrations` /
ingest / `producer_cycle(producer: &ManagedPostgres, …)`
(`crates/jurisearch-package-build/src/cycle.rs:59`) all take that local handle. It
**cannot** target the remote CT 110 server; its data dir lives on whatever host runs the
producer. So `02`'s v1 model would place the entire ingested LEGI + jurisprudence
corpus, its `pgvector`/`pg_search` indexes, and the heavy ingest/embed compute **on CT
111** — directly violating `01`'s "CT 111 … should not store database data, vector
indexes" and its 32 GB rootfs (LEGI's baseline alone is ~1.1 GB compressed;
`02-…crons.md:93`). `index_dir = /srv/jurisearch/producer-index` is **not** under the
Storebox mount (`/srv/jurisearch/storebox`, `01-…deploy.md:138`), so it lands on the
32 GB rootfs and overflows; relocating a PG data dir onto the CIFS Storebox mount is not
a supported option either. Conversely, `01`'s stated model ("drive ingest against CT
110") is precisely the external-`postgres://` producer that `02` says is **out of scope
for v1** and that **does not exist in code** (`run_migrations` and the ingest/package
chain are all bound to `ManagedPostgres`; only the *site* side gets an external-PG path,
in `01` Phase 3). An implementer cannot start `02` Phase 2 without first resolving which
of the two mutually exclusive topologies is real.

Recommended fix (pick one and write it into both plans):

1. **Co-locate the producer's `ManagedPostgres` on CT 110** (the 48-core/192 GB/1 TB
   host). The `jurisearch-producer` orchestrator, ingest, embed, and `producer_cycle`
   then run on CT 110; CT 111 is reduced to the network-fetch/mirror role its size fits
   ("download archives to Storebox"). This keeps `02`'s `ManagedPostgres` model but
   *moves the producer host*, and requires `01` to stop saying CT 111 "drives
   ingest/publish work against CT 110." Update open decision #6 to record this.
2. **Promote `02` open decision #3 option B (external-PG producer) into v1**, mirroring
   `01` Phase 3's external-PG migration applier but extended to ingest/enrich/embed/
   `producer_cycle`. This matches `01`'s CT-111-against-CT-110 wording and the captured
   bootstrap env, but it is a real new capability (the whole producer chain currently
   requires `ManagedPostgres`) and must be called out as a prerequisite, not deferred.

Either way, delete the contradiction: `01`'s CT 111 description and `02`'s
"`ManagedPostgres`, not external `postgres://`" statement cannot both stand.

---

### WARN 1 — Producer and site embedder config examples use different `model` strings, which by the plans' own parity rule must be identical → the documented happy path produces a fingerprint mismatch

- Site (`01-…deploy.md:296`): `[embedder] model_name = "bge-m3"`.
- Producer (`02-…crons.md:294-301`): `[embedding] base_url = "https://openrouter.ai/api/v1"`,
  `model_name = "baai/bge-m3"`, with the comment "The storage fingerprint must match the
  site-local query embedder's storage fingerprint (model, dimension, normalize)."
- `02` Phase 2 makes this a hard gate: "Producer and site embedders must agree on the
  storage fingerprint (model, dimension, normalize); mismatch fails before publish or
  before serving" (`02-…crons.md:380-382`).

Why it matters (verified): the storage fingerprint is the **verbatim** model string —
`crates/jurisearch-embed/src/fingerprint.rs:17` computes
`format!("{}:{}:normalize:{}", self.model, self.dimension, self.normalize)` (pooling is
deliberately excluded — the plans get that right). So the producer example yields
`baai/bge-m3:1024:normalize:true` and the site yields `bge-m3:1024:normalize:true`;
these are **not equal**, and the parity gate (correctly fail-closed) would reject every
package / refuse hybrid serving. The capability to fix this exists in code but the plan
points the operator at the wrong field: the embeddings request sends
`self.config.request_model()` (`crates/jurisearch-embed/src/client.rs:164`) while the
fingerprint uses `self.config.model` — and a test
(`crates/jurisearch-embed/src/tests.rs:71` `request_model_alias_does_not_change_stored_fingerprint`)
confirms you can set `request_model = "baai/bge-m3"` while keeping `model = "bge-m3"`.
But the single-endpoint env surface the producer config implies
(`JURISEARCH_EMBED_MODEL`, …) has **no `JURISEARCH_EMBED_REQUEST_MODEL`** — `request_model`
is only settable through the multi-endpoint `JURISEARCH_EMBED_POOL` spec
(`crates/jurisearch-cli/src/embedding_runtime/config.rs:380`). So `model_name =
"baai/bge-m3"` in `[embedding]` will, on the obvious wiring, set the **fingerprint**
model to `baai/bge-m3` and break parity.

Recommended fix: in `02`, separate the two concepts explicitly — the fingerprint
`model` must be the canonical `bge-m3` (identical to `01`'s site value), and the
OpenRouter-specific id `baai/bge-m3` must go in a distinct `request_model` (and the plan
must say which config/env seam carries it, since the single-endpoint env path can't).
Add a cross-config parity assertion to the test matrix: "`producer.storage_fingerprint
== site.storage_fingerprint`" computed from the two example TOMLs, not just an
intra-producer check.

---

### WARN 2 — The non-negotiable site-query confidentiality boundary is asserted in prose but not enforced by an unconditional validation rule

The constraint is explicit and non-negotiable: customer query text "must not go to
OpenRouter or any external embedding provider" (`01-…deploy.md:331-333`, repeated in
non-goals `:187-192`). But the Phase 1 validation rule that would enforce it is
**conditionally scoped**: "If `base_url` and `port` are both set, they must name the
same loopback port" (`01-…deploy.md:379`). A `site.toml` with `[embedder] base_url =
"https://some-host/api/v1"` and `port` omitted (serve-site only needs `base_url`, not
`port`) would not trip that rule. The Phase 6 "Endpoint binds loopback only" check
(`01-…deploy.md:543`) inspects where the **bge-m3 unit** binds, not where serve-site's
configured `base_url` points — and `serve-site` reads `JURISEARCH_EMBED_BASE_URL`
independently (`crates/jurisearch-cli/src/embedding_runtime/config.rs:246`). So nothing
in the stated rules hard-prevents a site from being configured to embed customer queries
against an external host.

Why it matters: this is the project's stated confidentiality boundary; relying on prose
plus a conditional check is exactly the "implementer could build the wrong thing" risk
the review brief calls out.

Recommended fix: make it an **unconditional** Phase 1 validation rule — the site
`[embedder].base_url` host MUST resolve to loopback (`127.0.0.0/8` / `::1`), and any
non-loopback embedder host is rejected before any file is written (mirroring the
existing non-loopback-bind refusal). Add a negative test ("external embedder base_url
rejected") to the config-parser test matrix.

---

### WARN 3 — `01` Phase 9 references "the chosen Phase 2 orchestration path", which is `02`'s Phase 2, not `01`'s — a reader following `01` linearly will misresolve it

`01` Phase 9 deliverables and invariants say the `dist/update-server/` bundle includes
"the package/ingest binaries required by **the chosen Phase 2 orchestration path**"
(`01-…deploy.md:627-628`) and "including shell-out dependencies if **Phase 2** chooses
that path" (`01-…deploy.md:652`). Within `01`, "Phase 2" is the **host doctor** phase
(`01-…deploy.md:398`), which has nothing to do with orchestration strategy. The intended
referent is `02` Phase 2 / open decision #1 (shell-out (A) vs library extraction (B),
`02-…crons.md:548-559`).

Why it matters: the contents of a release bundle are gated on a decision documented in a
*different file*; an implementer building `dist.sh` from `01` alone cannot tell which
binaries belong in `update-server`.

Recommended fix: qualify every such reference as
"`02-auto-update-server-crons.md` Phase 2 / open decision #1" and state the concrete
consequence: under the recommended shell-out path (A), `dist/update-server/` must bundle
the full `jurisearch` binary **and** `jurisearch-package` **and** `jurisearch-producer`.

---

### WARN 4 — The build-ordering dependency between the two plans is absent from both sequencing summaries

`01` Phase 9's `dist/update-server/` bundle must contain `jurisearch-producer`
(`01-…deploy.md:626`) and the producer service/timer templates — all of which are
**created in `02`** (`02` Phase 2/3). `02` in turn says the producer "is installed from
the `dist/update-server/` release bundle produced by `01` Phase 9's root `./dist.sh`"
(`02-…crons.md:187-189`). So Phase 9's update-server bundle cannot be built, tested, or
its "no huge assets / correct role boundary" invariants exercised until `02` lands. Yet
`01`'s sequencing summary (`01-…deploy.md:699-714`) presents phases 1–9 as a single
linear track reachable from `01` alone, and `02`'s plan does not flag that its output is
a hard input to `01` Phase 9.

Why it matters: a team executing `01` end-to-end will hit Phase 9 expecting to finish
the release story and discover the producer half of the bundle does not exist yet —
a half-built deployment path exactly of the kind the brief warns about.

Recommended fix: in both sequencing sections, state the dependency explicitly —
`./dist.sh` can emit the `site-server` and `cli` bundles after `01` Phase 8, but the
`update-server` bundle is gated on `02` (Phases 2–3) and should be implemented/tested as
part of `02`, with `01` Phase 9 owning only the script skeleton and the two site/cli
bundles until then.

---

### NIT 1 — Demo mode's embedder/model prerequisite is unstated, and the model is deliberately not bundled

The single-host demo (`01-…deploy.md:59-77`) "starts the same binaries and exercises the
same site protocol," and Phase 8 smoke runs a hybrid search "if embedder is configured"
(`01-…deploy.md:599`). A hybrid leg needs a live local bge-m3 (`llama-server` + the
`*.gguf` model + tokenizer), but those assets are explicitly excluded from every release
bundle (`01-…deploy.md:632-634`). The demo section never says `demo up` must start a
bge-m3 endpoint or that the operator must fetch model/tokenizer assets first, so "prove
the product on a workstation" understates its real prerequisites.

Recommended fix: state that `demo up` either provisions/starts a loopback bge-m3 (and
requires the model/tokenizer to be fetched via `embed fetch-assets`) or that the demo's
hybrid leg is skipped with a documented reason when no embedder is present.

### NIT 2 — `[sync] corpora` accepts values the v1 producer never publishes

`01` validates only that `sync.corpora` is non-empty (`01-…deploy.md:368`), but `02` v1
publishes a single `core` corpus and a real `jurisprudence` package corpus is explicitly
out of scope (`02-…crons.md:560-567`, verified: `KNOWN_SOURCES` maps all five sources to
`core` in `crates/jurisearch-package/src/corpus.rs:75-83`). An operator who lists
`corpora = ["core", "jurisprudence"]` will pass validation and only fail later at
catch-up.

Recommended fix: note in `01` that `corpora` entries must correspond to corpora the
subscribed producer actually publishes (v1: `core` only), or have `doctor`/`catch-up`
surface "no producer manifest for corpus `<x>`" as a distinct, early failure.

### NIT 3 — Make explicit that `dist/update-server/` intentionally contains the heavy `jurisearch` binary

Under the recommended shell-out orchestration (A), the update-server bundle necessarily
includes the full `jurisearch` CLI (which links storage/embed/ingest/official-api —
verified: `crates/jurisearch-cli/Cargo.toml` depends on all four, and the crate is
binary-only with no `lib.rs`). That is correct and does not conflict with the *cli*
bundle's thin-cone invariant (`01-…deploy.md:654`, which governs `dist/cli/`, not
`update-server`), but the two "role boundary" framings read as if all three bundles are
slim. A one-line note that `update-server` is intentionally the heavy role would prevent
a reviewer from mistaking the full `jurisearch` binary for a packaging error.

---

## 2. Open questions / residual risks

1. **Producer host + DB model (the BLOCKER):** which topology is canonical — producer
   `ManagedPostgres` co-located on CT 110, or an external-PG producer connecting from CT
   111 to CT 110? This is `02` open decision #6 (and interacts with #3); `01` has
   implicitly pre-decided it via the infra snapshot, creating the conflict. A human must
   pick one and reconcile both documents.
2. **Orchestration boundary (`02` open decision #1):** shell-out (A) vs library
   extraction (B) is correctly flagged as a Phase 2 prerequisite. Confirmed accurate:
   `jurisearch-cli` is binary-only (no `lib.rs`), and `jurisearch-package-build` depends
   only on `jurisearch-package`/`jurisearch-storage`, so a `jurisearch-producer` bin
   there genuinely cannot call ingest/enrich/embed as a library. The choice gates both
   `02` Phase 2 and the `dist/update-server/` contents (WARN 3/4).
3. **Embedding-parity enforcement** (WARN 1/2): once the model-string and loopback
   issues are fixed, a single cross-config parity test comparing the two example TOMLs'
   storage fingerprints would lock the contract that both plans depend on.
4. **Producer-side embedding throughput vs. the lock** (informational, not a defect):
   `02` correctly holds the `core` update lock across enrich → embed → publish, which
   means the OpenRouter embedding round-trips happen inside the lock. That is the right
   safety call (per the well-argued half-processed-publish analysis,
   `02-…crons.md:418-429`); just note the lock can be held for a while on large delta
   nights, which is acceptable since the contract is bounded-wait, not skip.

## 3. Verification notes

Claims checked against the repository and confirmed accurate:

- `KNOWN_SOURCES`/`corpus_for_source` map all five `ArchiveSource` variants to `core`
  (`crates/jurisearch-package/src/corpus.rs:75-97`), and migration 18 enforces the
  drift lock (`crates/jurisearch-storage/src/migrations.rs:1232-1257`). `02`'s "all
  five package as `core`" model is correct.
- `producer_cycle()` exists as a library-only seam (`…package-build/src/cycle.rs:59`);
  its only caller is a test (`publish_distribution.rs`) — confirming `02`'s "not exposed
  as a CLI verb and not scheduled." It builds no incremental on an empty window but still
  republishes the signed manifest (`cycle.rs:77-100`), matching `02`'s invariant.
- Archive selection is by `ArchiveTimestamp`, not `change_seq`:
  `ArchiveSyncFilter { incremental, since_compact }` and `select_archives_to_process`
  compare `timestamp.compact()` (`crates/jurisearch-cli/src/ingest.rs:326-351`); the
  baseline-precedence planner is in `archive/planner.rs:59-140`. `02`'s "three cursors"
  and BLOCKER-2 trap framing are well-grounded. (Note: that filter is `pub(crate)`, so
  exposing it as a library API — as `02` B.2 suggests — is real work, correctly flagged.)
- DILA name parsing matches the documented Freemium scheme (`BASELINE_RE`/`DELTA_RE`,
  `crates/jurisearch-ingest/src/archive/parser.rs:12-19`); the five-source enum and
  cross-source rejection exist.
- `storage_embedding_fingerprint` = `model:dimension:normalize` with pooling excluded
  (`crates/jurisearch-embed/src/fingerprint.rs:15-22`); both plans correctly treat
  pooling as a deploy-time validation rule, not a fingerprint component. `serve-site`
  does consume `JURISEARCH_EMBED_NORMALIZE` and `JURISEARCH_EMBED_POOLING` from env
  (`…/embedding_runtime/config.rs:281-285`), so `01`'s rendering contract is implementable.
- `run_migrations` is a method on `ManagedPostgres` using `execute_sql`
  (`crates/jurisearch-storage/src/migrations.rs:1131-1180`); `01` Phase 3 correctly calls
  the external-PG migration applier "new capability." A client-style libpq path already
  exists for site role connections (`SharedServerBackend`/`ConnectionConfig::connect`,
  `crates/jurisearch-storage/src/backend.rs:53-71`), which the new applier can build on.
- `serve-site` flags `--socket`/`--tcp host:port` + `--allow-lan`/`--allow-wildcard-lan`
  exist exactly as `01`'s bind translation assumes (`crates/jurisearch-cli/src/args.rs:408-450`;
  non-loopback/wildcard refusal tested in `site/serve.rs`).
- syncd verbs `trust`/`subscribe`/`update`/`run`/`status` and `run_daemon`
  (poll→plan→verify→apply) exist (`crates/jurisearch-syncd/src/main.rs`,
  `daemon.rs:205`); the `update`/catch-up path applies baselines/incrementals and never
  embeds — consistent with "syncd never calls an external embedding API."
- Thin-client cone is intact: `jurisearch-client` depends only on
  core/transport/render (`crates/jurisearch-client/Cargo.toml`), and today's CLI is the
  flat positional `command`/`args` with `--server`/`--local`/`$JURISEARCH_SITE_URL`
  that `01` Phase 7 proposes to restructure; `--local` resolves
  `unix://$XDG_RUNTIME_DIR/jurisearch-site.sock` exactly as `01`'s demo note relies on.
- Judilibre/Légifrance endpoints (`judilibre_search`, `judilibre_transactional_history`,
  `judilibre_decision`, `legifrance_search`) and `RetryPolicy` exist in
  `jurisearch-official-api`; `build_rebaseline`/`apply_rebaseline` exist
  (`…package-build/src/baseline.rs:187`, `…syncd/src/apply.rs:76`) as `02` Phase 5 assumes.
- Existing systemd units match the rendering targets, including the explicit note that
  systemd does not expand env vars in `ReadOnlyPaths` (`deploy/systemd/jurisearch-syncd.service`)
  and the `--pooling cls` flag on the bge-m3 unit — both plans' "absolute paths in path
  directives" rule is grounded in this real constraint.
- `jurisearchctl`, `crates/jurisearch-deploy`, `jurisearch-producer`, and `./dist.sh` do
  **not** yet exist — both plans describe net-new work, not stale state.

The bootstrap-credential handling is acceptable per the brief: both plans label
`root / 20Sense20` and `postgres / postgres` as bootstrap-only and push secrets to
`0600`/credential files in the product config (`01-…deploy.md:82-93, 304-307`), and do
not normalize them as production design.

## 4. Verdict

The two plans are well-researched and faithful to the code, but they currently disagree
on the single most consequential producer decision (the DB topology, BLOCKER 1), and the
documented embedding configs would ship a fingerprint mismatch (WARN 1) that defeats the
hybrid path the whole product depends on. Both must be resolved in the documents before
implementation starts; WARN 2 (confidentiality enforcement) is a non-negotiable-boundary
gap that should be closed in the same pass.

VERDICT: FIXES_REQUIRED
