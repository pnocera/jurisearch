# Task 0 — Contracts / API survey (execution map)

Date: 2026-06-29
Status: survey only — no code changed. Inputs read: `00-macro-implementation-plan.md`,
`01-makeitsimpletodeploy.md`, `02-auto-update-server-crons.md`, `04-claude-orchestrator-instructions.md`.

This file is the execution map the orchestrator uses before starting M1 implementation. All `crate/file:line`
and symbol references below were located via codegraph against the current checkout.

**Revision r2 — Codex fixes applied.** Four refinements integrated from the Task 0 Codex review:
(1) root `Cargo.toml` `[workspace] members` is a shared-file hazard — added a mandatory **C0 scaffolding
step** (single orchestrator-owned commit that creates all new workspace-member entries + skeleton manifests
before any parallel agent fans out); (2) S7's public seam is a **client-factory trait only** — dropped the
`&mut postgres::Client` "equivalent" option (the build path opens multiple independent clients);
(3) **producer config parser ownership** resolved — owned by M2-B (Task 2) as a documented deferral from the
macro M1 list, with M1-A owning the **shared config primitives** (TOML load scaffolding + redaction +
file-permission helpers); (4) **`ErrorObject` owner** corrected — it is owned by `jurisearch-core`, not
`jurisearch-cli`, so the cycle guard is "pipeline must not depend on `jurisearch-cli`", while
`jurisearch-core::error::ErrorObject` remains a shared core protocol type.

**Revision r3 — Codex r2 fix applied.** The r2 re-review confirmed the four r2 fixes hold and found one
new WARN: the S4–S6 reusable APIs take `db: &impl DbClientSource`, but the implementation path M1-C must
extract calls **additional storage helpers that are still `ManagedPostgres`-typed** (they open their own
connection or use `execute_sql`), not just `backend.rs`/`runtime.rs`/`migrations.rs`. If left unassigned,
M1-C would have to edit storage (or duplicate storage logic) to ship the `DbClientSource` surface against
an external producer DB — reintroducing the hidden shared-file collision the gate removes. Resolution
integrated below: **M1-B now owns generalizing these helpers** (adding client-source / `*_with_client`
variants alongside the existing `ManagedPostgres` wrappers, kept as thin shims) as a reviewed deliverable
that must merge *before* M1-C begins its S4–S6 extraction; M1-C consumes only the generalized APIs and
still never edits storage. The specific helpers/files are enumerated in §1 (storage-helper generalization
sub-table) and assigned to M1-B in §3, §4, and §6.

**Revision r4 — Codex r3 fixes applied.** The r3 re-review confirmed the prior fixes hold and returned two
WARNs, both integrated below: (1) the enumerated helper list was still incomplete against the real
ingest/enrich/embed call paths — added the r3 helper tail (`zone_units.rs`
`load_derivable_decision_zones_json`/`load_zone_unit_embedding_inputs`/`insert_zone_unit_embeddings`/
`finalize_zone_dense_rebuild`, `projection/embeddings.rs` `insert_chunk_embeddings`,
`ingest_accounting/readiness.rs` `invalidate_cached_query_readiness`, `legislation_citations.rs`
`load_archived_decisions_with_visa_json`) — **and reframed the M1-B storage-helper deliverable from a fixed
enumerated list to a functional scope rule + a mandatory call-graph audit** (the lists here are illustrative
and explicitly non-exhaustive; M1-B's first implementation sub-step is a codegraph call-graph audit from the
payload entrypoints that derives the COMPLETE helper set, and that audited set — not this doc's list — is
M1-C's dependency). (2) Clarified the **cli/pipeline-side `pool.rs` wrappers (M1-C extracts) vs the
storage-side insert APIs (M1-B generalizes)** boundary so the two agents do not collide on `pool.rs` vs
storage. (3) Corrected the §3 M1-C dependency cell so the table now agrees with the §4 handoff: M1-C cannot
start until BOTH the S1 trait AND the complete (audited) storage-helper generalization are reviewed and
merged. No code was changed.

---

## 0. Headline findings (read first)

1. **There are two storage abstractions already in the tree, and the producer path uses the wrong one
   for v1.**
   - `ManagedPostgres` (`crates/jurisearch-storage/src/runtime.rs:86`) — self-managed embedded Postgres
     (`initdb`/`pg_ctl`, local data dir, loopback port). This is what ingest / embed / `producer_cycle` use
     today via `index_dir → start_durable`.
   - `ConnectionConfig` / `ReadHandle` / `WriterHandle` / `SharedServerBackend`
     (`crates/jurisearch-storage/src/backend.rs:30/76/107/172`) — **attaches to an existing external
     PostgreSQL as a client** (host/port/user/password, `NoTls`). Already used by `jurisearch-syncd` and
     `serve-site`. **The external-PG connection primitive the producer needs already exists here.**
2. **The producer DB-mutating functions only depend on `ManagedPostgres` for one thing: `.client()`.**
   `build_incremental(producer: &ManagedPostgres, …)` (`incremental.rs:97`) immediately calls
   `producer.client()?` and then does **all** real work over `&mut postgres::Client`
   (`build_incremental_inner`, `incremental.rs:130`). `producer_cycle` (`cycle.rs:59`) is the same shape.
   So externalizing the producer is mostly a **signature generalization** ("take something that yields a
   `postgres::Client`"), not a rewrite. `ManagedPostgres::client()` is `runtime.rs:246`;
   `ConnectionConfig::connect()` is `backend.rs:56`; `WriterHandle::client()` is `backend.rs:120`.
3. **Migrations are bound to `ManagedPostgres` and run by shelling to `psql`.**
   `run_migrations` is a **method on `ManagedPostgres`** (`migrations.rs:1131`) that calls
   `self.execute_sql` (`runtime.rs:237`) → `psql(...)` (`runtime.rs:420`). The migration *data*
   (`MIGRATIONS` static array, `validate_migration_list()`) is free-standing — so a connection-based
   applier over a `postgres::Client` is feasible without moving the SQL. Several storage helpers are
   already client-generic (`install_trust_anchor<C: GenericClient>`, `storage/src/trust.rs:23`;
   `*_with_client` functions throughout), proving the generic-client pattern is established.
4. **`jurisearch-producer` and `jurisearch-deploy` do not exist yet** (confirmed: `ls crates/` has
   neither). Both are "to be created". `jurisearchctl` is to live as a `[[bin]]` in `jurisearch-deploy`
   per `01` "Add jurisearchctl".
5. **`deploy/` today is three static, hand-copied systemd units** (`deploy/systemd/jurisearch-site.service`,
   `jurisearch-syncd.service`, `jurisearch-bge-m3.service`). No templating, no env-file rendering, no
   producer/timer units exist. All to be created.

---

## 1. Ownership map

| Capability | Owning crate / file(s) | Key symbols | Notes |
|---|---|---|---|
| **Site/producer config parsing** | none yet | — | **To be created.** No TOML config parser for `site.toml`/`producer.toml` exists. **Ownership split (resolved, see §3/§4):** the **shared config primitives** (TOML load scaffolding, secret redaction, password/env-file permission helpers — `00-macro-implementation-plan.md:135`) are owned by **M1-A** (`jurisearch-deploy`); the **`SiteConfig` parser** is M1-A; the **producer config parser** (`[producer]`/`[database]`/`[fetch]`/`[package]`/`[enrichment]`/`[embedding]`/`[baseline_refresh]`) is deferred to **M2-B / Task 2** per `04-claude-orchestrator-instructions.md:281`. Embedder config has a TOML/env loader at `crates/jurisearch-cli/src/embedding_runtime/config.rs` (`embedding_config_from_env`) and `crates/jurisearch-embed/src/config.rs` (`EmbeddingConfig`, `EmbeddingProvider`, `fingerprint`, `storage_embedding_fingerprint`) — reuse for the `[embedder]`/`[embedding]` blocks. |
| **systemd unit / env-file templating** | `deploy/systemd/*.service` (static) | — | Three checked-in units only; "copy this unit / create the env file" style. No renderer. **To be created** in `jurisearch-deploy` (`site render`) and `jurisearch-producer` (`install`). |
| **Storage migrations & DB provisioning** | `crates/jurisearch-storage/src/migrations.rs`, `runtime.rs`, `backend.rs` | `ManagedPostgres::run_migrations` (`migrations.rs:1131`), `MIGRATIONS` + `validate_migration_list` (`migrations.rs:1183`), `execute_sql`→`psql` (`runtime.rs:237,420`), `CURRENT_SCHEMA_VERSION` | Migration runner is **`ManagedPostgres`-only and psql-shelled** today. External/connection path = run `MIGRATIONS` over a `postgres::Client` from `ConnectionConfig`. Role/grant/extension provisioning **does not exist as a command** — `start_durable_with_profile`→`ensure_database` (`runtime.rs:479`) only bootstraps the *managed* server. |
| **Ingest archive commands (DILA `.tar.gz`)** | `crates/jurisearch-cli/src/ingest.rs`, `ingest/legi.rs`, `ingest/juri.rs`; planner in `crates/jurisearch-ingest` | `emit_ingest` (`ingest.rs:91`), `sync_payload` (`ingest.rs:29`), `ingest_legi_archives_payload` (`ingest/legi.rs`), `ingest_juri_archives_payload` (`ingest/juri.rs:96`), `ArchiveSyncFilter` (`ingest.rs:327`), `plan_from_dir` / `ArchiveSource` / `PlannedArchive` (jurisearch-ingest `archive`) | Payload fns take `index_dir: Option<&Path>` → `open_index_for_bulk_ingest` (`index_runtime.rs:64`) → `ManagedPostgres`. CLI-only today; **library extraction target**. Archive *selection* keys on `ArchiveTimestamp::compact()` via `ArchiveSyncFilter { incremental, since_compact }` — the "archive cursor != package cursor" invariant is already structurally present. |
| **Enrichment runtime** | `crates/jurisearch-cli/src/enrichment/`, `ingest/pipeline.rs`; client in `crates/jurisearch-official-api` | `enrich_zones_payload` (`ingest/pipeline.rs:21`), `judilibre_zones.rs`, `PisteClient` / `OfficialApiConfig` (`jurisearch-official-api`), `EnrichmentMode` (`cycle.rs:25`: `Disabled`/`Ran`/`SkippedNoCredentials`) | CLI-only orchestration; PISTE client is library-ready. `EnrichmentMode` is the honesty contract recorded (not run) by `producer_cycle`. **Library extraction target.** |
| **Embedding runtime** | `crates/jurisearch-cli/src/embedding_runtime/`, `crates/jurisearch-embed`, `crates/jurisearch-query/src/embedder.rs` | `PreparedQueryEmbedder::from_env` (`embedding_runtime/mod.rs:26`), `OpenAiCompatibleClient`, `EmbeddingConfig::openai_compatible` (`embed/config.rs:61`), `fingerprint`/`storage_embedding_fingerprint` (`config.rs:131,148`), `request_model` field (`config.rs:76`) | **bge-m3 local query embedder** = `PreparedQueryEmbedder` (loopback, fingerprint-checked). **External document-embed path** = same `OpenAiCompatibleClient` with a different `base_url`. **`request_model` already exists as a separate field** and is NOT part of `fingerprint()` (which keys on provider/base_url_class/model/dimension/normalize/pooling) — the OpenRouter `request_model` vs storage `model_name` separation the plan requires is already structurally supported. Document-embed CLI verbs (`embed-chunks`, `embed-zone-units`) live in `ingest/pipeline.rs`. |
| **Package build / publish / signed manifest** | `crates/jurisearch-package`, `crates/jurisearch-package-build` | `producer_cycle` (`cycle.rs:59`), `ProducerCycleConfig`/`ProducerCycleReport` (`cycle.rs:36,44`), `build_incremental` (`incremental.rs:97`), `publish_package`/`publish_remote_manifest` (`publish.rs:39,86`), `build_remote_manifest`/`RemoteManifestParams` (`remote_manifest.rs`), `verify_published_root` (`verify.rs:38`), `Signed`/`verify`/`seal` (`signed.rs`), `RemoteManifest`/`RemotePackageEntry` (`package/src/manifest/remote.rs:18,79`). CLI bin: `crates/jurisearch-package-build/src/bin/jurisearch_package.rs` (`build`/`publish`/`publish-manifest`/`verify`, opens `ManagedPostgres` via `open_index` at :129, calls `run_migrations` at :176/251/272). | `producer_cycle` is library-ready but **`ManagedPostgres`-typed** and **not wired to any CLI verb / scheduler** (its own docs: "the CADENCE is the ops boundary"). The `jurisearch_package` binary is the dev/debug surface; v1 producer must call the library in-process, not shell out. |
| **syncd trust / update / status / catch-up / readiness** | `crates/jurisearch-syncd` | `main.rs` (commands `Apply`/`Trust`/`Subscribe`/`Update`/`Run`/`Status`, :208), `install_trust_anchor`/`load_package_verifier`/`install_verified_license_token`/`check_entitlement` (`syncd/src/trust.rs:24,40,112,66`), `fetch_verify_manifest`/`plan_catchup`/`run_catchup`/`DirectoryCatchupSource` (`planner.rs:413,…`), `corpus_status`/`CorpusStatus` (`status.rs:31,13`), `run_daemon` (`daemon.rs`), readiness via `resolve_query_readiness` (`jurisearch-storage/ingest_accounting`). Storage-level trust: `storage/src/trust.rs:23` (`install_trust_anchor<C>`, `install_license_token`, `load_trust_anchors`). | Fully built and external-PG-capable (uses `WriterHandle`/`ReadHandle`). `jurisearchctl site bootstrap-trust/catch-up/readiness` will **wrap** these, not reimplement. |
| **Thin client** | `crates/jurisearch-client` | `main.rs` (`run`, :47), `lib.rs` (endpoint parse + `connect`). | Thin cone preserved (no storage/embed/ingest/pg). `configure`/`doctor` verbs + XDG `client.toml` **to be added** (M5-A). |
| **CLI entrypoints & wiring** | `crates/jurisearch-cli/src/main.rs` (giant import surface, :1+), `args.rs`, dispatch via `emit_*`; `jurisearch-syncd/src/main.rs`; `jurisearch-client/src/main.rs`; `jurisearch-package-build/src/bin/jurisearch_package.rs` | clap-derive parsers per binary; `jurisearch` CLI dispatches ingest/enrich/embed/serve-site/sync. | `jurisearchctl` (in `jurisearch-deploy`) and `jurisearch-producer` are **new binaries** to be created. |
| **Storage helpers the ingest/enrich/embed path calls (still `ManagedPostgres`-typed)** | `crates/jurisearch-storage/src/projection/hierarchy_backfill.rs`, `ingest_accounting/replay_snapshot.rs`, `ingest_accounting/readiness.rs`, `zone_units.rs`, `legislation_citations.rs`, `dense.rs`, `projection/embeddings.rs` | `backfill_legi_article_hierarchy_from_metadata_scoped` (`hierarchy_backfill.rs:41`, opens its own `postgres::Client` at :46), `refresh_replay_snapshot` (`replay_snapshot.rs:64`), `invalidate_cached_query_readiness` (`readiness.rs:193`), `enrich_zone_candidates_json`/`replace_zone_units_for_document`/`zone_retrieval_coverage_json`/`load_derivable_decision_zones_json`/`load_zone_unit_embedding_inputs`/`insert_zone_unit_embeddings`/`finalize_zone_dense_rebuild` (`zone_units.rs:47,128,689,228,304,349,501`), `finalize_citation_occurrence_counts`/`load_pending_citation_resolutions_json`/`legislation_citations_coverage_json`/`load_archived_decisions_with_visa_json` (`legislation_citations.rs:205,257,355,15`), `load_chunk_embedding_inputs`/`finalize_dense_rebuild` (`dense.rs:57,116`), `insert_chunk_embeddings` (`projection/embeddings.rs:14`) | **These are the hidden tail of S4–S6 — an illustrative, explicitly non-exhaustive sample.** The pipeline-extraction path reaches them (e.g. LEGI ingest calls the hierarchy backfill at `cli/src/ingest/legi.rs:245`; `build_zone_units_payload` calls `load_derivable_decision_zones_json` at `cli/src/ingest/pipeline.rs:272`; `embed_zone_units_payload` reaches `load_zone_unit_embedding_inputs`/`insert_zone_unit_embeddings`/`finalize_zone_dense_rebuild`; `embed_chunks_payload` calls `invalidate_cached_query_readiness` at `pipeline.rs:493` and `insert_chunk_embeddings`; the legislation-citation collection path calls `load_archived_decisions_with_visa_json` at `cli/src/enrichment/legislation.rs:208`; ingest/embed completion calls `maybe_refresh_replay_snapshot` at `cli/src/ingest.rs:451`). All take `&ManagedPostgres` (several via `execute_sql`) today, so they are **owned by M1-B** and must be generalized (client-source / `*_with_client` variants, `ManagedPostgres` wrappers kept as thin shims) **before** M1-C extracts S4–S6. The COMPLETE set is M1-B's to derive via a call-graph audit (see the sub-table immediately below and §3/§4/§6). |

**Storage-helper generalization owned by M1-B — a functional scope rule, not a fixed list.** M1-B owns
generalizing **every** `ManagedPostgres`-typed (and `execute_sql`-based) storage/pool helper reachable from
the ingest → enrich → embed extraction path that M1-C will move into `jurisearch-pipeline`. **The helper
lists in this survey are illustrative and explicitly non-exhaustive.** For each such helper, M1-B adds a
client-source / `*_with_client` variant **alongside** the existing `ManagedPostgres` wrapper (the wrapper
stays as a thin shim so nothing else in the tree breaks), following the established pattern
(`install_trust_anchor<C: GenericClient>` at `storage/src/trust.rs:23`; the `*_with_client` helpers
throughout storage). M1-C then consumes ONLY these generalized APIs from `jurisearch-pipeline` and never
edits storage.

**M1-B's first implementation sub-step is a call-graph audit (NOT writing generalizations).** Before writing
any generalization, M1-B runs a codegraph call-graph audit starting from the ingest/enrich/embed payload
entrypoints — `build_zone_units_payload` (`cli/src/ingest/pipeline.rs:245`), `embed_zone_units_payload`
(`pipeline.rs:329`), `embed_chunks_payload` (`pipeline.rs:477`), the LEGI/JURI ingest payloads
(`ingest_legi_archives_payload` `cli/src/ingest/legi.rs:109`, `ingest_juri_archives_payload`
`cli/src/ingest/juri.rs:96`), `enrich_zones_payload` (`pipeline.rs:21`), and the legislation-citation
collection path (`cli/src/enrichment/legislation.rs:208`) — to derive the COMPLETE set of storage helpers
needing a client-source variant. **The audit result (the full helper list) is part of M1-B's deliverable and
is what M1-C depends on — not the illustrative list in this doc.**

cli/pipeline-side vs storage-side boundary (so M1-B and M1-C do not collide on `pool.rs` vs storage): the
embedding **pool wrappers** in `crates/jurisearch-cli/src/embedding_runtime/pool.rs` (e.g.
`embed_and_insert_chunks_with_pool` at :324, `embed_and_insert_zone_units_with_pool` at :361, and the
`insert_chunk_embeddings`/`insert_zone_unit_embeddings` call sites at pool.rs:352/387) are
**cli/pipeline-side code that M1-C extracts into `jurisearch-pipeline`** — M1-C owns them. But the **storage
insert APIs they call** (`insert_chunk_embeddings` in `projection/embeddings.rs:14`,
`insert_zone_unit_embeddings` in `zone_units.rs:349`) are **M1-B's to generalize**. M1-C rewires the
extracted pool wrappers onto M1-B's generalized insert APIs; it does not edit storage.

Illustrative (non-exhaustive) helper sample — the audited set supersedes this list:

| Storage file (M1-B owns) | Helper(s) to generalize | Today's signature / why it is `ManagedPostgres`-bound | Reached by |
|---|---|---|---|
| `projection/hierarchy_backfill.rs` | `backfill_legi_article_hierarchy_from_metadata_scoped` (:41) | takes `&ManagedPostgres`; opens its own `postgres::Client` (:46) | LEGI ingest `cli/src/ingest/legi.rs:245` (S4) |
| `ingest_accounting/replay_snapshot.rs` | `refresh_replay_snapshot(postgres: &ManagedPostgres)` (:64) | takes `&ManagedPostgres` | ingest/embed completion via `maybe_refresh_replay_snapshot` `cli/src/ingest.rs:451` (S4/S6) |
| `ingest_accounting/readiness.rs` | `invalidate_cached_query_readiness(postgres: &ManagedPostgres)` (:193) | takes `&ManagedPostgres` | `embed_chunks_payload` `cli/src/ingest/pipeline.rs:493` (S6) |
| `zone_units.rs` | `enrich_zone_candidates_json` (:47), `replace_zone_units_for_document` (:128), `zone_retrieval_coverage_json` (:689), `load_derivable_decision_zones_json` (:228, uses `execute_sql`), `load_zone_unit_embedding_inputs` (:304), `insert_zone_unit_embeddings` (:349), `finalize_zone_dense_rebuild` (:501) | take `&ManagedPostgres` (use `begin_snapshot`/own client / `execute_sql`) | enrichment + zone-unit build/embed (S5/S6): `build_zone_units_payload` `pipeline.rs:272`, `embed_zone_units_payload` `pipeline.rs:378/394/444` |
| `legislation_citations.rs` | `finalize_citation_occurrence_counts` (:205), `load_pending_citation_resolutions_json` (:257), `legislation_citations_coverage_json` (:355), `load_archived_decisions_with_visa_json` (:15, uses `execute_sql`) | take `&ManagedPostgres` | legislation citation enrichment + collection (S5): `cli/src/enrichment/legislation.rs:208` |
| `dense.rs` | `load_chunk_embedding_inputs` (:57), `finalize_dense_rebuild` (:116) | take `&ManagedPostgres` | document/chunk embedding (S6) |
| `projection/embeddings.rs` | `insert_chunk_embeddings` (:14) | takes `&ManagedPostgres` | chunk-embed insert via pool wrapper `cli/src/embedding_runtime/pool.rs:352` (S6) |

---

## 2. Library-first API seams to extract (resolved decision #1: in-process, no shell-out)

The producer must call ingest → enrich → embed → package-publish **in-process**. Today those live behind
the `jurisearch` CLI binary surface (`jurisearch-cli`) and are typed as `ManagedPostgres`. The seams:

| # | Seam (current location) | Current signature shape | Problem | Proposed reusable signature shape |
|---|---|---|---|---|
| S1 | **DB execution target** — `ManagedPostgres` (`runtime.rs:86`) vs `ConnectionConfig`/`WriterHandle` (`backend.rs`) | producer fns take `&ManagedPostgres`; only call `.client() -> postgres::Client` | Producer is hard-bound to embedded PG | Introduce a minimal **client-factory** trait in `jurisearch-storage`, `pub trait DbClientSource { fn client(&self) -> Result<postgres::Client, StorageError>; }`, implemented by both `ManagedPostgres` and `WriterHandle`/`ConnectionConfig`. It is deliberately a *connection source* (hands out multiple independent clients), **not** a single borrowed `&mut postgres::Client`, because the build path needs several concurrent clients (see S7). **This is the shared seam between M1-B and M1-C (see §4).** |
| S2 | **Connection-based migrations** — `ManagedPostgres::run_migrations` (`migrations.rs:1131`), `MIGRATIONS`/`validate_migration_list` (free) | `&self` method, applies SQL via `psql` subprocess | Can't migrate an external DB without a managed server | `pub fn run_migrations_on(client: &mut postgres::Client) -> Result<MigrationReport, StorageError>` running the existing `MIGRATIONS` array in a tx; keep the `ManagedPostgres` method as a thin wrapper. Extension creation (`pgvector`,`pg_search`) may need superuser → emit exact DBA SQL on failure. |
| S3 | **External DB provisioning** — does not exist | — | No role/grant/extension/db-create command | `pub fn provision_external_db(cfg: &ProvisionConfig) -> Result<ProvisionReport, ProvisionError>` (typed config in: admin DSN, target db, owner/writer/read roles; typed report out). Lives in `jurisearch-storage` (or `jurisearch-deploy` calling storage). Backed by S2 + role/grant SQL + activation-visibility postcondition check. |
| S4 | **Ingest archive entrypoint** — `ingest_legi_archives_payload`/`ingest_juri_archives_payload` (`cli/src/ingest/legi.rs`,`juri.rs`) | `(index_dir: Option<&Path>, source, archives_dir, run_id, limits, quarantine, safe_mode, ArchiveSyncFilter) -> Result<serde_json::Value, ErrorObject>` | CLI-typed (`ErrorObject`, JSON `Value`), `index_dir`-bound; **calls `ManagedPostgres`-typed storage helpers** (`backfill_legi_article_hierarchy_from_metadata_scoped` at `legi.rs:245`, `maybe_refresh_replay_snapshot` at `ingest.rs:451`) | `pub fn ingest_archives(db: &impl DbClientSource, req: IngestArchivesRequest) -> Result<IngestReport, IngestError>` — typed struct in, typed report out (archives ingested, journal cursor, quarantined). Extract from `jurisearch-cli` into a reusable lib (new `jurisearch-pipeline` crate, or a `lib` target promoted from `jurisearch-cli` modules). **Depends not only on the S1 trait but on M1-B having generalized the storage helpers in the §1 sub-table** (`hierarchy_backfill.rs`, `replay_snapshot.rs`) to client-source variants; M1-C consumes those, never edits storage. |
| S5 | **Enrichment entrypoint** — `enrich_zones_payload` (`cli/src/ingest/pipeline.rs:21`) | `index_dir` + PISTE creds → JSON `Value` | CLI-typed; credential policy implicit; **calls `ManagedPostgres`-typed storage helpers** (non-exhaustive: `zone_units.rs` `enrich_zone_candidates_json`/`replace_zone_units_for_document`/`zone_retrieval_coverage_json`/`load_derivable_decision_zones_json`; `legislation_citations.rs` `finalize_citation_occurrence_counts`/`load_pending_citation_resolutions_json`/`legislation_citations_coverage_json`/`load_archived_decisions_with_visa_json`) | `pub fn enrich_zones(db: &impl DbClientSource, piste: Option<&PisteClient>, req: EnrichRequest) -> Result<EnrichOutcome, EnrichError>` returning `EnrichmentMode` (`Ran{zones_enriched}`/`SkippedNoCredentials`) so the cycle records honestly. **Depends on M1-B's generalized `zone_units.rs`/`legislation_citations.rs` helpers — the audited set, not just the §1 sample — not just on the S1 trait.** |
| S6 | **Document embedding entrypoint** — `embed-chunks`/`embed-zone-units` in `cli/src/ingest/pipeline.rs`; client `OpenAiCompatibleClient` (`jurisearch-embed`) | CLI verbs; `EmbeddingConfig` already typed; **calls `ManagedPostgres`-typed storage helpers** (non-exhaustive: `dense.rs` `load_chunk_embedding_inputs`/`finalize_dense_rebuild`; `projection/embeddings.rs` `insert_chunk_embeddings`; `ingest_accounting/readiness.rs` `invalidate_cached_query_readiness`; `zone_units.rs` `replace_zone_units_for_document`/`load_zone_unit_embedding_inputs`/`insert_zone_unit_embeddings`/`finalize_zone_dense_rebuild`; `maybe_refresh_replay_snapshot`). Note: the cli-side `pool.rs` embed wrappers are extracted by M1-C, but the storage insert APIs they call are M1-B's to generalize. | CLI-bound; provider/fingerprint split must be preserved | `pub fn embed_documents(db: &impl DbClientSource, cfg: &EmbeddingConfig, req: EmbedRequest) -> Result<EmbedReport, EmbedError>`. Must keep storage fingerprint fields (`model_name`,`dimension`,`normalize`) separate from `request_model`/`base_url` — already true in `EmbeddingConfig` (`request_model` excluded from `fingerprint()`); add a regression test asserting `request_model` never leaks into `storage_embedding_fingerprint()`. **Depends on M1-B's generalized `dense.rs`/`zone_units.rs`/`projection/embeddings.rs`/`readiness.rs`/`replay_snapshot.rs` helpers — the audited set, not just the §1 sample — not just on the S1 trait.** |
| S7 | **Package cycle entrypoint** — `producer_cycle` (`cycle.rs:59`), `build_incremental` (`incremental.rs:97`) | `(&ManagedPostgres, corpus, published_root, build_dir, &dyn Signer, &ProducerCycleConfig) -> ProducerCycleReport` | `ManagedPostgres`-typed | Generalize the `producer`/`db` parameter to the **client-factory trait `&impl DbClientSource`** — **not** a single `&mut postgres::Client`. The public build path opens *multiple independent* clients from the producer: `build_incremental` opens one client for the corpus build lock (`incremental.rs:105`) **and a separate `fence_conn` for the outbox fence** (`incremental.rs:110`), and `build_remote_manifest` opens its own fresh client before taking the corpus lock (`remote_manifest.rs:62`). A single borrowed client would either lose the dedicated fence connection or leak internals the seam must hide. An optional **private** helper may take explicit `&mut db` + `&mut fence_conn` (matching today's `build_incremental_inner` at `incremental.rs:130`), but the public producer/build API takes the factory trait. Behavior unchanged; only the parameter type widens. |

Note: S4–S6 require promoting `jurisearch-cli` internal modules (`ingest`, `enrichment`, `embedding_runtime`)
into a reusable library. Recommended: a new `crates/jurisearch-pipeline` lib crate that both `jurisearch-cli`
(as a thin binary) and `jurisearch-producer` depend on. This keeps the thin-client cone intact and gives the
producer typed configs/results/errors instead of JSON `Value`/`ErrorObject`.

---

## 3. Proposed worktree / branch split (M1 parallel work)

Branch convention `agent/<task-slug>`, worktree `../jurisearch-worktrees/<task-slug>` (per orchestrator
protocol). Dependency edges from the orchestrator graph: `C0 → {A,B,C,D}`, `B→{E,G}`, `C→E`, `D→E`.

**C0 — workspace scaffolding (the new first implementation action; runs before any parallel M1 agent).**
The root `Cargo.toml` lists `[workspace] members` explicitly (`Cargo.toml:1`–`16`), so every "new crate"
path (`jurisearch-deploy`, `jurisearch-pipeline`, and the DILA fetch crate if it is a separate crate) would
collide on that one file if agents added themselves independently. To avoid a first-wave merge collision,
a **single small orchestrator-owned scaffolding commit** lands first and:
- adds the new `[workspace] members` entries to the root `Cargo.toml` (the manifest is touched exactly once,
  before fan-out);
- creates the skeleton crate manifests (`crates/jurisearch-deploy/Cargo.toml`,
  `crates/jurisearch-pipeline/Cargo.toml`, and the fetch crate's `Cargo.toml` if it is a separate crate)
  each with a trivial `src/lib.rs`.

After C0 merges to integration, each parallel agent fills in **only its own crate's files** and **never
edits the root manifest**. If the DILA fetch lands as a *module* inside an existing crate rather than a new
crate, it needs no member entry and only the two new crates are scaffolded. This makes the root manifest a
C0-owned, single-writer file rather than a shared-mutable hazard during fan-out.

| Task | Branch / worktree slug | Primary files owned | Depends on |
|---|---|---|---|
| **M1-A** site config parser + render + shared config primitives | `m1a-site-config-render` | new `crates/jurisearch-deploy/**` (`SiteConfig` parser, env/unit renderer, `[[bin]] jurisearchctl`, **shared config primitives: TOML load scaffolding + secret redaction helpers + password/env-file permission helpers** — `00-macro-implementation-plan.md:135`), `deploy/systemd/*` templates (reference) | C0 |
| **M1-B** external-PG migration/provisioning **+ storage-helper generalization** | `m1b-external-pg` | `crates/jurisearch-storage/src/migrations.rs` (S2 `run_migrations_on`), new `provision.rs` (S3), `backend.rs`/`runtime.rs` (S1 `DbClientSource` trait — **lands first, handoff to M1-C**); **storage-helper generalization (functional scope rule, must merge before M1-C extracts S4–S6):** generalize **every** `ManagedPostgres`/`execute_sql`-typed storage helper on the ingest→enrich→embed path, derived by M1-B's **call-graph audit first sub-step** (§1). Illustrative (non-exhaustive) files: `crates/jurisearch-storage/src/projection/hierarchy_backfill.rs`, `ingest_accounting/replay_snapshot.rs`, `ingest_accounting/readiness.rs` (`invalidate_cached_query_readiness`), `zone_units.rs` (incl. `load_derivable_decision_zones_json`/`load_zone_unit_embedding_inputs`/`insert_zone_unit_embeddings`/`finalize_zone_dense_rebuild`), `legislation_citations.rs` (incl. `load_archived_decisions_with_visa_json`), `dense.rs`, `projection/embeddings.rs` (`insert_chunk_embeddings`) — add client-source / `*_with_client` variants; keep `ManagedPostgres` wrappers as thin shims | C0 |
| **M1-C** library extraction (ingest/enrich/embed/package) | `m1c-pipeline-lib` | new `crates/jurisearch-pipeline/**` (S4–S6), `crates/jurisearch-package-build/src/cycle.rs` + `incremental.rs` (S7 signature widening), `crates/jurisearch-cli` (modules → thin re-exports, incl. the `embedding_runtime/pool.rs` wrappers rewired onto M1-B's generalized insert APIs) | C0 + **S1 from M1-B + M1-B complete storage-helper generalization (audited set, merged)** |
| **M2-A** DILA fetch / cursor / integrity | `m2a-dila-fetch` | new fetch crate/module (Apache index parser, fetch cursor, quarantine), reuses `jurisearch-ingest::archive::ParsedArchive`/`ArchiveSource` (read-only) | C0 |

**Must NOT run in parallel / shared-file hazards:**
- **Root `Cargo.toml` `[workspace] members` (`Cargo.toml:1`–`16`).** M1-A (`jurisearch-deploy`), M1-C
  (`jurisearch-pipeline`), and M2-A (DILA fetch crate, if separate) all need a member entry there. **Resolved
  by the C0 scaffolding commit above**: the root manifest is written exactly once, before fan-out, and no
  parallel agent edits it afterward.
- **M1-B and M1-C both need the `ManagedPostgres → postgres::Client` abstraction.** They collide on
  `jurisearch-storage` (`backend.rs`/`runtime.rs`) and on the producer signatures. Resolution in §4.
- **M1-C's S4–S6 extraction reaches further into `jurisearch-storage` than `backend.rs`/`runtime.rs`/`migrations.rs`.**
  The ingest/enrich/embed path calls additional helpers still typed `&ManagedPostgres`/`execute_sql`-based
  (illustrative, non-exhaustive: `projection/hierarchy_backfill.rs`, `ingest_accounting/replay_snapshot.rs`,
  `ingest_accounting/readiness.rs`, `zone_units.rs`, `legislation_citations.rs`, `dense.rs`,
  `projection/embeddings.rs`). **Resolved by §4 as a functional scope rule, not a fixed list: M1-B's first
  sub-step is a call-graph audit that derives the COMPLETE helper set, then M1-B owns and generalizes every
  one (client-source / `*_with_client` variants, `ManagedPostgres` wrappers kept as thin shims) and that work
  must merge before M1-C begins; M1-C consumes only the generalized APIs and never edits storage.** Note the
  boundary: the cli-side `embedding_runtime/pool.rs` embed wrappers are M1-C's to extract, but the storage
  insert APIs they call (`insert_chunk_embeddings`, `insert_zone_unit_embeddings`) are M1-B's to generalize.
- **M1-C and the `jurisearch_package` bin / `producer_cycle`**: M1-C widens `producer_cycle`/`build_incremental`
  signatures; no other M1 task may edit `cycle.rs`/`incremental.rs` concurrently. M2-B (later) consumes them.
- **M2-A and M1-C both touch `jurisearch-ingest`**: M2-A only *reads* `archive::ParsedArchive`/`ArchiveSource`
  (name parsing) and writes a new fetch module; M1-C extracts the *ingest execution* path. Safe if M2-A adds a
  new module and does not modify `archive/parser.rs`. Flag for review if M2-A needs parser changes.
- **M1-A is collision-free** with B/C/D **once C0 has landed** (new `jurisearch-deploy` crate + reference
  templates only; the root-manifest entry is created by C0, not M1-A); fully parallel thereafter.

**Producer config parser ownership (source-doc reconciliation).** The macro M1 deliverables list a producer
config parser (`00-macro-implementation-plan.md:129`), but the orchestrator instructions assign the producer
config parser to the **Task 2 producer update agent** (`04-claude-orchestrator-instructions.md:281`). The
orchestrator instructions are the operational authority for this run, so the resolved decision is:
- **The producer config parser (`[producer]`/`[database]`/`[fetch]`/`[package]`/`[enrichment]`/`[embedding]`/
  `[baseline_refresh]`) is owned by M2-B (Task 2)**, recorded here as an **intentional, documented deferral**
  from the macro M1 list — not an oversight.
- **M1-A's `jurisearch-deploy` crate owns the SHARED config primitives now** (`00-macro-implementation-plan.md:135`):
  TOML load scaffolding, secret-redaction helpers, and file-permission helpers for password/env files. Both
  the site config (M1-A) and the later producer config (M2-B) **reuse** these, so neither side invents
  parallel/duplicated secret-handling code. M1-A does **not** build the producer config schema itself; M2-B
  builds the producer schema on top of M1-A's primitives.

---

## 4. M1-B vs M1-C parallelization decision

**Verdict: NOT fully parallel. Sequence a thin shared storage-execution seam (S1) first — owned by M1-B —
then M1-B-provisioning and M1-C run in parallel on disjoint files.**

Reason: both tasks exist *because* the producer DB path is `ManagedPostgres`-bound. Their overlap is the
storage seam that yields a `postgres::Client` for an external DB — the S1 trait **plus** the **complete
(audited) set** of ingest/enrich/embed storage helpers that are still `ManagedPostgres`-typed (the §1 lists
are illustrative and non-exhaustive). `build_incremental`/`producer_cycle` only use `ManagedPostgres` for
`.client()` and everything downstream there is already `&mut postgres::Client`-generic; but the S4–S6 path
additionally calls storage helpers (e.g. `hierarchy_backfill.rs`, `replay_snapshot.rs`, `readiness.rs`,
`zone_units.rs`, `legislation_citations.rs`, `dense.rs`, `projection/embeddings.rs`) that open their own
connection or use `execute_sql`. So M1-B must generalize **both** the trait and the audited helper set before
M1-C extracts S4–S6.
With that done, M1-B and M1-C touch disjoint files and the overlap is resolvable by a defined handoff rather
than by full serialization. **M1-C never edits storage because M1-B has pre-generalized every storage helper
the pipeline path needs.**

Concrete ownership assignment (M1-B vs M1-C; M1-A's shared-config-primitive ownership is noted separately
below):

| Owns | M1-B (`m1b-external-pg`) | M1-C (`m1c-pipeline-lib`) |
|---|---|---|
| `jurisearch-storage/src/backend.rs` (S1 trait `DbClientSource` + impls) | **YES — lands first** | no (consumes the trait) |
| `jurisearch-storage/src/runtime.rs` (`ManagedPostgres` impls `DbClientSource`) | **YES** | no |
| `jurisearch-storage/src/migrations.rs` (S2 `run_migrations_on`) | **YES** | no |
| new `jurisearch-storage/src/provision.rs` (S3 roles/grants/extensions) | **YES** | no |
| **all `jurisearch-storage` helpers on the ingest→enrich→embed path** (audited set — see functional scope rule; illustrative rows below are non-exhaustive) | **YES — audit, generalize, merge before M1-C extracts** | no (consumes generalized API) |
| `jurisearch-storage/src/projection/hierarchy_backfill.rs` (generalize `backfill_legi_article_hierarchy_from_metadata_scoped`) | **YES — merge before M1-C extracts** | no (consumes generalized API) |
| `jurisearch-storage/src/ingest_accounting/replay_snapshot.rs` (generalize `refresh_replay_snapshot`) | **YES — merge before M1-C extracts** | no (consumes generalized API) |
| `jurisearch-storage/src/ingest_accounting/readiness.rs` (generalize `invalidate_cached_query_readiness`) | **YES — merge before M1-C extracts** | no (consumes generalized API) |
| `jurisearch-storage/src/zone_units.rs` (generalize `enrich_zone_candidates_json`/`replace_zone_units_for_document`/`zone_retrieval_coverage_json`/`load_derivable_decision_zones_json`/`load_zone_unit_embedding_inputs`/`insert_zone_unit_embeddings`/`finalize_zone_dense_rebuild`) | **YES — merge before M1-C extracts** | no (consumes generalized API) |
| `jurisearch-storage/src/legislation_citations.rs` (generalize `finalize_citation_occurrence_counts`/`load_pending_citation_resolutions_json`/`legislation_citations_coverage_json`/`load_archived_decisions_with_visa_json`) | **YES — merge before M1-C extracts** | no (consumes generalized API) |
| `jurisearch-storage/src/dense.rs` (generalize `load_chunk_embedding_inputs`/`finalize_dense_rebuild`) | **YES — merge before M1-C extracts** | no (consumes generalized API) |
| `jurisearch-storage/src/projection/embeddings.rs` (generalize `insert_chunk_embeddings`) | **YES — merge before M1-C extracts** | no (consumes generalized API) |
| new `crates/jurisearch-pipeline` (S4–S6 ingest/enrich/embed lib) | no | **YES** |
| `jurisearch-cli/src/embedding_runtime/pool.rs` embed wrappers (cli/pipeline-side; rewired onto M1-B's generalized insert APIs) | no (owns only the storage insert APIs the wrappers call) | **YES — extracts into pipeline** |
| `jurisearch-package-build/src/cycle.rs`, `incremental.rs` (S7 signature widen) | no | **YES** |
| `jurisearch-cli` ingest/enrich/embed modules → lib re-exports | no | **YES** |

**M1-A owns (separate from the B/C split):** the shared config primitives in `jurisearch-deploy` — TOML load
scaffolding, secret-redaction helpers, and password/env-file permission helpers (`00-macro-implementation-plan.md:135`).
These are reused by both M1-A's `SiteConfig` and M2-B's later producer config parser. The producer config
**schema/parser** itself is **M2-B's** (Task 2) per `04-claude-orchestrator-instructions.md:281`, a documented
deferral from the macro M1 list (`00-macro-implementation-plan.md:129`).

Handoff protocol for the orchestrator:
1. M1-B opens first and lands **only S1** (the `DbClientSource` trait + the two impls) as its first reviewed
   commit. Merge to integration.
2. **M1-B's next sub-step is a call-graph audit, not generalization.** Using codegraph, M1-B traces from the
   ingest/enrich/embed payload entrypoints (`build_zone_units_payload`, `embed_zone_units_payload`,
   `embed_chunks_payload`, `ingest_legi_archives_payload`/`ingest_juri_archives_payload`,
   `enrich_zones_payload`, and the legislation-citation collection path) to derive the **complete** set of
   `ManagedPostgres`/`execute_sql`-typed storage helpers needing a client-source variant. The audited list is
   itself part of the deliverable and is what M1-C depends on — **not** the illustrative §1 sample.
3. M1-B then lands its **storage-helper generalization deliverable** over the audited set: client-source /
   `*_with_client` variants of every helper found — illustratively (non-exhaustively)
   `backfill_legi_article_hierarchy_from_metadata_scoped` (`hierarchy_backfill.rs`), `refresh_replay_snapshot`
   (`replay_snapshot.rs`), `invalidate_cached_query_readiness` (`readiness.rs`), the `zone_units.rs` helpers
   (`enrich_zone_candidates_json`/`replace_zone_units_for_document`/`zone_retrieval_coverage_json`/
   `load_derivable_decision_zones_json`/`load_zone_unit_embedding_inputs`/`insert_zone_unit_embeddings`/
   `finalize_zone_dense_rebuild`), the `legislation_citations.rs` helpers (`finalize_citation_occurrence_counts`/
   `load_pending_citation_resolutions_json`/`legislation_citations_coverage_json`/
   `load_archived_decisions_with_visa_json`), the `dense.rs` helpers
   (`load_chunk_embedding_inputs`/`finalize_dense_rebuild`), and `insert_chunk_embeddings`
   (`projection/embeddings.rs`) — each **alongside** the existing `ManagedPostgres` wrapper, which is kept as
   a thin shim so nothing else in the tree breaks. (Boundary: the cli-side `embedding_runtime/pool.rs` embed
   wrappers are M1-C's to extract; the storage insert APIs they call are M1-B's to generalize here.) **This
   deliverable — the complete audited set — must be reviewed and merged to integration before M1-C begins its
   S4–S6 extraction**, because M1-C consumes these generalized APIs and must never edit storage.
4. After both the S1 trait **and** the complete (audited) storage-helper generalization have merged, M1-B continues with S2/S3
   (migrations + provisioning) **and** M1-C starts (rebased onto integration so it sees the trait + the
   generalized helpers). They no longer share files: M1-B owns all of `jurisearch-storage`; M1-C owns
   `jurisearch-pipeline`, the S7 signature widening, and the `jurisearch-cli` thin re-exports.
5. If the orchestrator prefers maximum safety over throughput, run M1-B fully, then M1-C — the cost is only
   M1-C's start latency, since M1-C is the high-risk task the macro plan already flags.

Either way: **do not let two agents edit `backend.rs`/`runtime.rs`/`migrations.rs`, the generalized storage
helpers (`hierarchy_backfill.rs`/`replay_snapshot.rs`/`zone_units.rs`/`legislation_citations.rs`/`dense.rs`),
or `cycle.rs`/`incremental.rs` at the same time. All `jurisearch-storage` edits are M1-B's; M1-C consumes
storage only through the generalized APIs.**

---

## 5. Per-task initial test commands (validation routes)

Workspace baseline (every task): `cargo build --workspace` and `cargo clippy --workspace --all-targets`.
Format gate: `cargo fmt --all --check`.

| Task | Default (CI/fixture) route | Live/credentialed route (defer unless authorized) |
|---|---|---|
| **M1-A** site config/render | `cargo test -p jurisearch-deploy` (config parser unit tests + **golden** env/unit rendering, bind-translation, loopback-only embedder rejection, redaction). Pure-logic; no DB/network. | none |
| **M1-B** external-PG | `cargo test -p jurisearch-storage` (migration-list validity, `run_migrations_on` against a client, provision idempotence, read-role-cannot-write, activation-visibility postcondition). **Live PG required** for the actual apply tests — `ManagedPostgres::start_temp` needs `pgvector`+`pg_search` assets (`require_extension_assets`, gated by `JURISEARCH_PG_CONFIG`). | Against external bear CT 110 (`192.168.0.110:5432`, `postgres/postgres`) — **authorize before running**; destructive role/grant ops. |
| **M1-C** pipeline lib | `cargo test -p jurisearch-pipeline -p jurisearch-package-build` (S7 signature compiles both managed+external; **`request_model` never enters `storage_embedding_fingerprint()`** regression test; fingerprint parity from example TOMLs). Many existing package-build tests (`incremental_loopback.rs`, `publish_distribution.rs`, `catchup_loop.rs`) use `ManagedPostgres::start_temp` → need PG assets. | Producer-side OpenRouter embedding leg needs `OPENROUTER_API_KEY` — defer; PISTE enrichment needs creds — defer (`SkippedNoCredentials` path is the CI default). |
| **M2-A** DILA fetch | `cargo test -p <fetch-crate>` against **fixture** Apache-index HTML + fixture `.tar.gz` (listing parse, cross-source rejection, no-op re-run, cursor advances only after integrity, truncated→quarantine). No network. | Live DILA `https://echanges.dila.gouv.fr/OPENDATA/` reachability — defer to authorized run; politeness/User-Agent. |

Note on the PG-asset gate: tests that build a real index (`start_temp`/`start_durable`) silently require the
`pgvector`/`pg_search` extension shared objects to be discoverable via `PgConfig`/`JURISEARCH_PG_CONFIG`.
On a host without them, those tests fail at `require_extension_assets`, not in logic. Treat "PG-asset
present" as the dividing line between the pure-logic CI route and the live route, and record it as residual
risk when the assets are absent.

---

## 6. Hidden-dependency-cycle check

| Risk | Where | Assessment / resolution |
|---|---|---|
| **`jurisearch-storage` ⇄ producer cycle** | S1 trait in storage; `cycle.rs` consumes it | No cycle: `jurisearch-package-build` already depends on `jurisearch-storage` (imports `ManagedPostgres`). Widening to a storage-defined trait keeps the edge one-directional. |
| **Pipeline lib ⇄ cli** | S4–S6 extraction | Cycle risk if the new `jurisearch-pipeline` re-imports `jurisearch-cli`. Resolution: pipeline depends only on `jurisearch-storage`/`jurisearch-ingest`/`jurisearch-embed`/`jurisearch-official-api`; `jurisearch-cli` depends on `jurisearch-pipeline` (one direction). **`jurisearch-pipeline` must not depend on `jurisearch-cli`**; prefer **pipeline-local typed errors** for producer APIs (S4–S6). Note: `ErrorObject` is **not** a cli type — it is owned by `jurisearch-core` (`crates/jurisearch-core/src/error.rs:17`) and `jurisearch-cli` merely depends on core (`crates/jurisearch-cli/Cargo.toml:15`). So `jurisearch-core::error::ErrorObject` remains a **shared core protocol type that pipeline may use from core directly** — referencing it creates no cli cycle. |
| **Root `Cargo.toml` `[workspace] members`** | `Cargo.toml:1`–`16`; M1-A/M1-C/M2-A each add a new crate | Not an import cycle but a first-wave merge hazard: explicit member list means independent additions collide. **Resolved by the C0 scaffolding commit** (§3): the root manifest + skeleton crate manifests are created once before fan-out; no parallel agent edits the root manifest afterward. |
| **Shared-mutable-file hazard: `jurisearch-storage` core files** | M1-B and M1-C both want `backend.rs`/`runtime.rs`/`migrations.rs` | Real hazard. Resolved by §4: M1-B owns all storage files and lands S1 first; M1-C never edits storage. |
| **Shared-mutable-file hazard: `jurisearch-storage` ingest/enrich/embed helpers (S4–S6 tail)** | M1-C's S4–S6 extraction calls `&ManagedPostgres`/`execute_sql`-typed helpers in `projection/hierarchy_backfill.rs` (:41), `ingest_accounting/replay_snapshot.rs` (:64), `ingest_accounting/readiness.rs` (:193), `zone_units.rs` (:47,128,689,228,304,349,501), `legislation_citations.rs` (:205,257,355,15), `dense.rs` (:57,116), `projection/embeddings.rs` (:14) — **illustrative and non-exhaustive** | Real hazard — without assignment, M1-C would have to edit these storage files (or duplicate storage logic) to ship the `DbClientSource` surface, reintroducing the hidden shared-file collision. **Resolved by §1 + §4 as a functional scope rule, not a fixed list: M1-B's first sub-step is a call-graph audit (from the payload entrypoints) deriving the COMPLETE helper set; M1-B then owns and generalizes every one (client-source / `*_with_client` variants, `ManagedPostgres` wrappers kept as thin shims) and that audited deliverable merges before M1-C begins; M1-C consumes only the generalized APIs and never edits storage. Boundary: the cli-side `embedding_runtime/pool.rs` embed wrappers are M1-C's to extract; the storage insert APIs they call (`insert_chunk_embeddings`, `insert_zone_unit_embeddings`) are M1-B's to generalize.** |
| **Shared-mutable-file hazard: producer cycle** | `cycle.rs`/`incremental.rs` | Only M1-C (S7) edits these in M1; M2-B edits later. Serialize via branch ownership. |
| **`jurisearch-ingest::archive::parser.rs`** | M2-A fetch vs M1-C ingest | No cycle; collision only if M2-A modifies the parser. M2-A should add a new fetch module and treat `parser.rs`/`ArchiveSource` as read-only. Flag to review if a parser change is unavoidable. |
| **Confidentiality boundary (not a cycle, but a guard)** | `EmbeddingConfig` shared by site (loopback) + producer (external) | Same type, two validation policies. Keep the loopback-only check **site-config-scoped** (M1-A / M4), and let the producer config (M2-B) permit external `base_url`. Do not push the loopback check into `jurisearch-embed` where it would break the producer path. |

No true import cycles found in the proposed split. The genuine collisions are shared-mutable-file hazards in
`jurisearch-storage` (both the core `backend.rs`/`runtime.rs`/`migrations.rs` files **and** the S4–S6 helper
tail — illustratively `hierarchy_backfill.rs`/`replay_snapshot.rs`/`readiness.rs`/`zone_units.rs`/
`legislation_citations.rs`/`dense.rs`/`projection/embeddings.rs`, the COMPLETE set derived by M1-B's
call-graph audit), the producer cycle, and the **root `Cargo.toml` `[workspace] members`** — resolved
respectively by the §4 ownership assignment (M1-B owns *all* `jurisearch-storage` edits, including the
audited storage-helper generalization that must merge before M1-C extracts S4–S6), the S1-first handoff, and
the C0 scaffolding commit (§3) that makes the root manifest a single-writer, pre-fan-out file.
