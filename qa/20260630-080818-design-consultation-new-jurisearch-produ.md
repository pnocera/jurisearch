# Q&A — 20260630-080818

## Question

# Design consultation — new `jurisearch-producer publish-baseline` subcommand

Repo: `/home/pierre/Work/jurisearch`. READ THE CODE; do not trust this prose — verify every claim against
the real source and push back where I'm wrong.

## Problem
An operator hand-loaded a full, query-ready `core` corpus into the EXTERNAL producer PostgreSQL (CT110):
`public.documents`=2.88M, `public.chunks`=4.70M (100% embedded, fingerprint `bge-m3:1024:normalize:true`),
`public.zone_units`=283k, `schema_version=24`. It arrived via raw ingest runs and **bypassed the
publisher**: `public.package_catalog`=0, `public.package_change_log`=0, no served artifacts, and the
producer `state_dir` (`/var/lib/jurisearch-producer`) is empty. So the producer treats the box as
un-baselined; a normal timer `update` would FETCH the DILA global baseline → `RebaselinePending` →
re-ingest + RE-EMBED 4.7M chunks from scratch — the outcome we must avoid. Timers are currently disabled.

Goal: publish the EXISTING in-DB corpus as the producer's first signed `core` baseline **without fetching
DILA and without re-embedding**, then make future runs INCREMENTAL.

## Verified facts (confirm independently)
- `build_baseline`→`build_media_package` (`crates/jurisearch-package-build/src/baseline.rs:144,264`)
  snapshots `public` via `COPY (...) TO STDOUT (FORMAT binary)` over `REPLICATED_TABLES`
  (`generations.rs:74`) — no fetch, no embedder. It signs the embedded manifest and inserts a
  `package_catalog` row (`baseline.rs:475`); `publish_package` (`publish.rs:49`) serves artifacts;
  `build_remote_manifest`/`publish_remote_manifest` (`remote_manifest.rs:63`) write the signed
  `core/manifest.json` that `jurisearch-syncd` fetches+verifies (`planner.rs:413`).
- These functions take `&impl DbClientSource`, which the producer's external `WriterHandle` implements
  (used at `update.rs:255`). The producer CLI dispatch is `jurisearch_producer.rs:150-409` (subcommands:
  ConfigExample/Validate/ProvisionDb/Install/Fetch/Update/Status/Rebaseline/Retention/Freshness — there is
  NO baseline-publish command; first baseline is called "one-time operator setup", `cycle.rs:241`).
- `jurisearch-producer rebaseline` can't bootstrap: `build_rebaseline_locked` errors "no baseline
  cataloged …" (`baseline.rs:223`).
- Fingerprint/schema/signing all align: config `storage_embedding_fingerprint()` == `bge-m3:1024:normalize:true`
  (`embed/src/fingerprint.rs:17`); `CURRENT_SCHEMA_VERSION=24` (`migrations.rs:24`); manifests are signed
  with the installed seed `producer-k1`/epoch1, trust anchor delivered out-of-band (`crypto.rs:255`,
  `remote_manifest.rs:199`). The `jurisearch-package` CLI default `--embedding-fingerprint` is a DIFFERENT
  `:cls:`-format string (`jurisearch_package.rs:94`) and must NOT be used.
- Routing is decided purely from `state_dir` files `fetch-cursor-<src>.json` + `adopted-baseline-<src>.json`
  (`baseline.rs:116,130`): equal fetched==adopted ⇒ Incremental; fetched newer ⇒ Rebaseline.

## Proposed design (critique it)
Add `jurisearch-producer publish-baseline --config <toml>` that, against the external CT110 PG via the
producer's `WriterHandle`:
1. Guards: refuse if a `core` baseline is already cataloged (idempotency); verify the data's embedding
   fingerprint and `schema_version` match config/`CURRENT_SCHEMA_VERSION` before publishing.
2. Calls `build_baseline`/`build_media_package` with corpus=`core`, embedding_fingerprint from config
   (`bge-m3:1024:normalize:true`), generation `core_g0001`, sequence 1, signer from the installed seed,
   served root = config `corpora_dir` (`/srv/jurisearch/storebox/packages`); marks the catalog row
   `status='published'`; then `publish_package` + `build_remote_manifest` + `publish_remote_manifest`.
3. (Part 2) Seeds `state_dir` markers for all 5 sources (legi/cass/inca/capp/jade) with equal
   fetched==adopted `baseline_file_name` derived from each `ingest_run.archive_plan`, so the next timer run
   is Incremental. Emits a JSON report (sequence/generation/package_id/digests) and a producer exit class.

## Questions (answer numbered; verify against source)
1. Is wiring `build_baseline`/`build_remote_manifest` through the producer's external `WriterHandle`
   (`DbClientSource`) sound? Any external-PG-specific hazards vs the self-managed path the
   `jurisearch-package` bin uses (the REPEATABLE-READ snapshot, binary COPY transport, served-root layout,
   the `corpora_dir`/manifest paths from `ProducerConfig`)?
2. `status='built'` vs `'published'`: the manual `jurisearch-package publish` doesn't call
   `mark_package_published`, but `rebaseline_cycle` does (`cycle.rs:313`). Does `build_remote_manifest`
   require `'published'`, and should `publish-baseline` set it? Any retention implications?
3. What exact `generation` / `baseline_id` / `sequence` conventions must the first baseline use so (a) a
   site-server/`jurisearch-syncd` client cleanly applies it and (b) later incrementals chain correctly?
   Is `core_g0001` / seq 1 right, or is there a canonical helper?
4. Part-2 state seeding: is deriving each source's `baseline_file_name` from `ingest_run.archive_plan`
   correct, and is writing BOTH `fetch-cursor-<src>.json` and `adopted-baseline-<src>.json` (equal) the
   right way to force `Current`→Incremental? Should this live IN `publish-baseline` or be a separate step?
   What if the hand-load didn't correspond to a clean per-source DILA global baseline (e.g. multiple
   tarballs / no single baseline file)?
5. What would make the published baseline BROKEN/unverifiable (e.g. `verify_catalog_identity`
   `remote_manifest.rs:232`, digest mismatch, missing served files, fingerprint/precondition mismatch)?
   How should the command DRY-RUN / self-verify (ideally re-running the verify path a client would) BEFORE
   it commits the `package_catalog` row and writes the manifest — i.e. make it transactional/atomic so a
   half-publish can't leave a broken manifest?
6. SCOPE the minimal testable slice vs what to defer: what is the smallest correct first version (e.g.
   build+catalog+serve+manifest for `core`, single baseline, no Part-2 seeding) that can be validated
   end-to-end (ideally a `jurisearch-syncd` client syncing it in a test), and what should be deferred?
7. Anything I'm missing or getting wrong about the architecture, the producer-vs-client table split, or a
   simpler supported path I've overlooked.

End with a clear verdict (e.g. "GO with adjustments" + numbered adjustments) and the recommended minimal
scope.

## Answer

# Design Consultation: `jurisearch-producer publish-baseline`

I verified the claims against the source. The high-level idea is sound: the package builder already knows how to snapshot the existing producer `public` corpus into a signed first `core` baseline without fetching DILA or embedding. The proposed command should exist, but I would adjust ordering, idempotency, status handling, and state seeding before calling it safe.

## 1. External `WriterHandle` Wiring

Yes, using the producer external `WriterHandle` is architecturally sound. `ProducerConfig::writer_handle()` returns a `WriterHandle` and explicitly documents it as the external producer `DbClientSource`; the update path already drives `producer_cycle` over that handle. `build_baseline`, `build_rebaseline`, `build_incremental`, and `build_remote_manifest` all take `&impl DbClientSource`, not `ManagedPostgres`.

The builder itself is also the right primitive. `build_media_package` snapshots all `REPLICATED_TABLES` from `public` inside one `REPEATABLE READ`, read-only transaction and uses binary `COPY (...) TO STDOUT (FORMAT binary)`. It does not fetch archives and does not call the embedder. It freezes `change_seq_high`, writes per-table copybin files, signs the embedded manifest, and inserts `package_catalog` with `status = 'built'`.

External-PG-specific hazards to handle:

1. Hold the producer `update-core` lock while bootstrapping. `run_update` uses that lock across ingest/enrich/embed/publish, but a new command would otherwise race a timer/manual update.
2. Also serialize the package chain. `build_baseline` does not acquire `acquire_corpus_build_lock`; incrementals and rebaselines do. The command should either acquire the package advisory lock around the bootstrap or add a first-baseline cycle helper that does.
3. Build into a package staging directory, not directly into `corpora_dir`. `corpora_dir` is the published root consumed by `publish_package(root, corpus, package_id, artifact_dir)`. The artifact dir should be something like `<corpora_dir>/core/.staging/bootstrap-baseline`, then publish to `<corpora_dir>/core/packages/core-1-1`.
4. Binary `COPY` baselines are PostgreSQL-major pinned. `build_media_package` records the external producer server major in `postgres_major_min/max`; `syncd` rejects a client whose PostgreSQL major is outside that exact window. This is the main external-PG operational hazard versus a loopback/self-managed setup.
5. Use producer config for the fingerprint and signer. Do not use `jurisearch-package` CLI defaults: that binary defaults to `bge-m3:1024:cls:normalize=true`, while producer config derives `bge-m3:1024:normalize:true`.

## 2. `built` vs `published`

`build_remote_manifest` does not require `status = 'published'`. It reads all catalog rows for the corpus, ordered by sequence, with no status filter, finds the latest baseline/rebaseline, and then reads the artifact from the published root. So a `built` row can be included if the artifact exists; if the artifact is missing, manifest building fails.

That means status is currently operational metadata, not a manifest filter. Still, `publish-baseline` should mark the row `published`, and it should do so only after `publish_package` succeeds. The normal cycle does exactly that for incrementals and rebaselines:

1. build artifact and insert `status = 'built'`
2. publish artifact to served root
3. update status to `published`
4. build and publish remote manifest

The standalone `jurisearch-package publish` command does not mark the row, which is an inconsistency in the manual CLI path. For the new producer command, follow the cycle behavior, not the old manual CLI.

Retention impact appears low today: producer retention is scoped to temp/partial/quarantine downloads, and `delete_unpublished_package_row` is used by rebaseline crash discard logic for `status = 'built'`. Still, leaving the first baseline as `built` is misleading and can interact badly with future recovery/diagnostics.

Implementation note: `mark_package_published` is private inside `cycle.rs`. Either expose a small public helper from `jurisearch-package-build`/`jurisearch-storage`, or run the same guarded SQL from the producer command and require exactly one row updated.

## 3. First Baseline Identity Conventions

Use the existing `build_baseline` conventions, not hand-rolled values:

1. `package_sequence = 1`
2. `package_id = core-1-1`
3. `from_sequence = 1`
4. `to_sequence = 1`
5. `generation = generation_name("core", 1) = core_g0001`
6. `package_kind = baseline`
7. `included_change_seq_low = 0`

That is already hard-coded in `build_baseline`, with a comment that this is the P3 first baseline convention. Later incrementals take the latest catalog row, set `from_sequence` to the previous package sequence, and use the same `baseline_id` and `generation`; later rebaselines use `generation_counter_of(prev.generation) + 1`.

`baseline_id` is not generated by `build_baseline`; it comes from `BaselineParams`. It should be stable across retries. Do not default it to a fresh timestamp if a retry might see an existing `core-1-1` row, because the embedded manifest digest includes created-at/builder-run data and catalog idempotency checks manifest/package identity. Prefer one of:

1. require `--baseline-id`, or
2. derive it deterministically from a proven per-source DILA baseline set, or
3. use an explicit operator label such as `core-ct110-bootstrap-20260630`.

For CT110, `core_g0001` / sequence 1 is right. The variable part is the stable `baseline_id`.

## 4. Part-2 State Seeding

The routing claim is correct but incomplete. `baseline_decision` is purely:

1. load `fetch-cursor-<src>.json`
2. load `adopted-baseline-<src>.json`
3. compare their `baseline_file_name`

Equal fetched/adopted means `Current`; fetched newer or adopted missing means `RebaselinePending`; any pending source makes the group route to rebaseline.

So writing both files with equal `baseline_file_name` is the right routing mechanism. But doing it automatically inside `publish-baseline` is risky unless the command can prove the DB snapshot actually corresponds to those upstream baselines.

Deriving the source baseline from `ingest_run.archive_plan` is reasonable only under strict conditions:

1. exactly one completed current ingest lineage per source is selected;
2. the `archive_plan.baseline.file_name` parses for that source;
3. all deltas applied into the DB are after that baseline;
4. the corpus is not an uncontrolled blend of multiple baseline epochs;
5. the archive files used to seed the fetch cursor are either present locally or the operator accepts that fetch may redownload them.

Also, a fetch cursor is more than `baseline_file_name`: the fetch engine skips files using `cursor.fetched`, keyed by archive file name. If you seed only `baseline_file_name`, routing becomes incremental, but the next fetch may redownload old deltas/baselines because `is_fetched()` will return false for them. The ingest journal may de-duplicate some work, but it is not the fetch cursor’s contract. A robust state seeding step should populate `fetched` entries for the known accepted baseline and deltas when sha/size/timestamp are known.

I would not put this into the minimal `publish-baseline` command. Make it a separate explicit step, or an optional `--seed-state-from-ingest-runs` mode with a loud provenance report. If the hand-load does not correspond to a clean per-source DILA global baseline set, do not silently seed equality. The honest options are:

1. leave state unseeded and accept that the next fetched global baseline will require rebaseline;
2. require operator-supplied per-source baseline file names plus a waiver that the DB is known to contain those baselines and all subsequent deltas;
3. build a stronger provenance validator before allowing automatic seeding.

For the CT110 operational goal, you probably need state seeding, but it should be treated as a separate, auditable adoption operation, not as an incidental side effect of publishing the package.

## 5. Broken/Unverifiable Baseline Risks and Self-Verification

Ways to publish a broken or unusable baseline:

1. catalog row and embedded manifest disagree on package id, kind, baseline id, generation, sequence, payload digest, or embedded-manifest digest; `verify_catalog_identity` catches this while building the remote manifest;
2. artifact is cataloged but not present under `<root>/<corpus>/packages/<package_id>`;
3. artifact URI base in the remote manifest does not match what clients use;
4. remote manifest signature does not verify against the distributed trust anchor;
5. embedded artifact signature does not verify, or differs from the signature copied into the remote manifest entry;
6. per-file digest or aggregate payload digest mismatch;
7. client DB schema version or schema bundle digest differs from the signed compatibility block;
8. client PostgreSQL major differs from the producer major for binary-copy baseline payloads;
9. required extensions (`vector`, `pg_search`) are absent on the client;
10. wrong corpus in the remote manifest or embedded artifact;
11. actual DB rows are not fully embedded under the config fingerprint, so the baseline is query-incomplete even though it is package-valid.

Preflight guards should include:

1. no published `core` media root already exists; handle a pre-existing `built` row as a recoverable partial or fail with an exact repair message;
2. `max(schema_migrations.version) == CURRENT_SCHEMA_VERSION`;
3. chunk embedding coverage: every `chunks` row has matching `chunk_embeddings` under config fingerprint/model/dimension, and `chunks.embedding_fingerprint` is stamped consistently;
4. zone-unit embedding coverage with the same check for `zone_units` / `zone_unit_embeddings`;
5. config fingerprint equals the expected storage format from model/dimension/normalize;
6. signing seed loads and its public trust anchor matches the intended deployment anchor;
7. `corpora_dir` is writable and package destination is absent or byte-identical.

On atomicity: with the current `build_baseline` API, you cannot fully verify before committing the `package_catalog` row, because `build_media_package` writes the artifact and inserts the catalog row itself. You can still make the operation recoverable and prevent a broken client-visible manifest:

1. take `update-core` and package build locks;
2. build into durable staging under the served filesystem;
3. publish the package;
4. mark the catalog row `published`;
5. build the remote manifest in memory, which verifies catalog identity against the published artifact;
6. publish `manifest.json` atomically by temp-then-rename;
7. run `verify_published_root` using a verifier built from the signer’s public key/trust anchor;
8. optionally run a real `syncd` loopback: `DirectoryCatchupSource` + `fetch_verify_manifest` + `plan_catchup(None)` + `run_catchup` into a fresh client DB.

For a stronger design, split the builder into “materialize artifact + candidate catalog row” and “commit catalog row”. Then the command can validate the artifact before inserting the row. Even then, DB and filesystem cannot be a single transaction; the real safety property is: never publish `core/manifest.json` until every referenced artifact is present and verified, and make partial rows/staging recoverable.

I would also consider changing `build_remote_manifest` to ignore rows whose status is not `published`. That would make the status column enforce the intended contract. If you do that, the old manual `jurisearch-package publish` path must either mark rows or be documented as a low-level tool.

## 6. Minimal Testable Slice

Smallest correct first version:

1. add `jurisearch-producer publish-baseline --config <toml>`;
2. load and validate `ProducerConfig`;
3. take `update-core` lock;
4. open `config.writer_handle()` and ensure provisioned/current schema;
5. verify embedding coverage/fingerprint from actual DB rows;
6. load signer from config;
7. build baseline into `<corpora_dir>/<corpus>/.staging/bootstrap-baseline`;
8. publish package to `config.producer.corpora_dir`;
9. mark catalog row `published`;
10. build and atomically publish remote manifest using config-derived `RemoteManifestParams`;
11. verify the published root with the public verifier;
12. emit JSON report with corpus, generation, baseline_id, package_id, sequence/head, change_seq high, artifact digest, manifest path, and exit class.

Do not include Part-2 state seeding in the first slice. It is operationally important for CT110, but it is a different correctness domain: upstream provenance and fetch/adoption state, not package publication.

Tests for that slice:

1. unit tests for preflight rejection: existing baseline, schema mismatch, missing chunk embeddings, missing zone embeddings, wrong fingerprint;
2. integration test using a managed producer DB plus `WriterHandle` where possible, mirroring `update_publish_gated`;
3. end-to-end publish test: build command/library helper, verify `core/manifest.json`, then use `DirectoryCatchupSource` and `syncd` to apply the baseline into a fresh client DB;
4. crash/retry test: a `built` row plus staged artifact is resumed or diagnosed; no manifest is published until recovery completes;
5. idempotent no-op test for an already published identical baseline, or a deliberate refusal with a clear “already baselined” class.

Deferred:

1. automatic state seeding from `ingest_run.archive_plan`;
2. full fetch cursor population with sha/size for historical archives;
3. provenance validation for mixed raw ingest histories;
4. remote network/TLS verification beyond the existing directory source;
5. broader retention policy for published packages.

## 7. Architecture Notes and Missing Points

The producer/client table split is as expected. Baseline artifacts copy only `REPLICATED_TABLES` from producer `public`; producer operational tables such as `ingest_run`, `package_catalog`, and `package_change_log` are not replicated to the client. The client applies into generation schemas and stamps `jurisearch_control.corpus_state`. That is the right split.

The simpler `jurisearch-package` CLI path is not appropriate for CT110 as-is:

1. it opens a self-managed `ManagedPostgres` via `--index-dir`, not the producer external PostgreSQL;
2. its default embedding fingerprint is the wrong `:cls:` string;
3. its `publish` command does not mark catalog rows as `published`.

The reusable library path is the supported lower layer; the missing piece is the producer-native operator command.

One more important point: publishing the first baseline does not itself prove future increments are safe. Future incrementals chain from `package_catalog.included_change_seq_high`; that part is fine. But fetch/adoption state determines whether the producer tries to ingest a new DILA global baseline as rebaseline or proceeds incrementally. State seeding must therefore be based on real upstream provenance, not just the existence of a packaged snapshot.

## Verdict

GO with adjustments.

Required adjustments:

1. Use `build_baseline` through `WriterHandle`, but wrap it in the producer `update-core` lock and the per-corpus package build advisory lock.
2. Build into a durable staging artifact directory under the served root; then `publish_package`; then mark the row `published`; then build/publish `manifest.json`.
3. Do not pass `corpora_dir` as the artifact directory. It is the published root.
4. Use config-derived `BaselineParams`: storage fingerprint from `ProducerConfig`, signer from the installed seed, builder versions matching producer code.
5. Use canonical first-baseline sequence/generation from `build_baseline`: `core-1-1`, sequence 1, `core_g0001`.
6. Require or deterministically derive a stable `baseline_id`; avoid clock-derived retry-unstable identity.
7. Add real DB preflights for schema version and actual embedding coverage/fingerprint.
8. Self-verify with `build_remote_manifest` identity checks plus `verify_published_root`; ideally add a `syncd` loopback integration test.
9. Treat Part-2 state seeding as separate or optional audited functionality. Equal fetched/adopted markers are the right routing mechanism, but deriving them from `ingest_run.archive_plan` is safe only when the per-source provenance is clean and proven.
10. Consider making `build_remote_manifest` filter `status = 'published'`, or at least keep the new command serialized so no manifest builder sees a half-published `built` row.

Recommended minimal scope: implement `publish-baseline` for package/catalog/serve/manifest only, with locks, preflight, status marking, and published-root verification. Defer state seeding to a separate `adopt-baseline-state`/`seed-state` design unless CT110 needs a one-off audited operational command immediately.
