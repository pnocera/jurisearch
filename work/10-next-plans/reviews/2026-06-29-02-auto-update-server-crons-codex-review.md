# Review - 02-auto-update-server-crons.md

## Findings

### BLOCKER - Producer DB/config shape does not match the existing producer APIs

The plan's target config introduces `work_db_url = "postgres:///jurisearch_producer"` and describes `jurisearch-producer update --config ...` as reusing the current ingest, enrich, embed, and package steps (`work/10-next-plans/02-auto-update-server-crons.md:199-203`, `263-275`). The current code does not have a producer path that can run those steps from a Postgres URL. `jurisearch-package` takes `--index-dir` and starts a durable `ManagedPostgres`; `producer_cycle()` takes `&ManagedPostgres` directly (`crates/jurisearch-package-build/src/bin/jurisearch_package.rs:26-30`, `crates/jurisearch-package-build/src/cycle.rs:59-66`). The ingest archive paths also require an index dir and start managed Postgres (`crates/jurisearch-cli/src/ingest/legi.rs:119-127`), and the top-level ingest/sync CLI is likewise index-dir based.

Recommended fix: either make the v1 producer config use the existing `index_dir`/`ManagedPostgres` model, or add an explicit prerequisite phase that introduces a producer storage connection abstraction usable by ingest, enrichment, embedding, and package-build from an external Postgres connection. Update the config example and Phase 2 tests to prove the chosen DB mode actually drives the existing commands.

### BLOCKER - Archive ingest resume is specified in the wrong coordinate system

The plan says the run journal tracks a published `change_seq` window and that ingest should process "archives newer than the corpus's last published change_seq window" (`work/10-next-plans/02-auto-update-server-crons.md:274-282`). That mixes two unrelated clocks. Archive selection in the existing ingest surface is by DILA archive timestamp/name: `ArchiveSyncFilter` only has `incremental` and `since_compact`, and `select_archives_to_process()` compares delta `ArchiveTimestamp::compact()` values (`crates/jurisearch-cli/src/ingest.rs:322-350`). The package cycle then consumes the database outbox after ingest (`crates/jurisearch-package-build/src/cycle.rs:67-93`). A package `change_seq` cannot safely decide which DILA archive filenames to ingest.

Recommended fix: split the state model into three explicit cursors: a fetch cursor per DILA source (`ArchiveTimestamp`, filename, size/mtime, baseline id), an ingest journal per accepted archive filename/run id/status, and a package high-water mark in outbox/change-seq space. Use the archive cursor/journal to decide which archive files are ingested, then let `producer_cycle()` decide which outbox window is packaged. Add a regression test showing a new DILA delta after a published package is selected by archive timestamp, not by change sequence.

### WARN - The jurisprudence cadence is not the State cadence

The plan repeatedly models all jurisprudence datasets as weekly (`work/10-next-plans/02-auto-update-server-crons.md:31-32`, `61-64`, `219-221`, `291-308`, `405-407`). Current public metadata and listings do not support that. On 2026-06-29, `data.gouv.fr` reports `frequency: daily` for CAPP, INCA, and JADE, while CASS is `punctual`; the current DILA Apache listings show JADE deltas on many consecutive June 2026 days and INCA/CAPP with non-weekly extra drops. CASS is roughly weekly in the listing, but the other jurisprudence sources are not.

Recommended fix: replace the single jurisprudence weekly cadence with per-source cadences or a daily jurisprudence timer that no-ops when a source has no new archive. If the product intentionally accepts weekly jurisprudence latency, label that as a product SLA tradeoff, not "the State's own cadence", and make `status --json` compare each source cursor against its actual newest upstream file.

### WARN - The empty-outbox `producer_cycle()` behavior is misstated

The plan says an empty outbox means `producer_cycle` is a no-op and publishes nothing (`work/10-next-plans/02-auto-update-server-crons.md:191-192`, `276-284`). The current implementation does not behave that way. It may build no incremental (`built_incremental = None`), but it always rebuilds and publishes the signed remote manifest afterwards (`crates/jurisearch-package-build/src/cycle.rs:67-93`).

Recommended fix: rewrite the invariant as "no package is built when the outbox is empty; the manifest may still be refreshed" and update the test matrix accordingly. If the desired behavior is truly "publish nothing", add a preflight outbox check before calling `producer_cycle()` and test that wrapper behavior separately from the library function.

### WARN - The recommended binary placement ignores current crate boundaries

The open-decision recommendation is to put a `jurisearch-producer` bin in `jurisearch-package-build` (`work/10-next-plans/02-auto-update-server-crons.md:401-404`), but the full update chain needs ingest, Judilibre/Legifrance enrichment, and embedding logic that currently lives in the `jurisearch-cli` binary crate. `crates/jurisearch-cli/Cargo.toml` defines only `[[bin]]`, not a reusable library, while `jurisearch-package-build` currently depends only on `jurisearch-package`, `jurisearch-storage`, and basic CLI/DB crates.

Recommended fix: add a phase or open decision for the orchestration boundary. Either refactor the reusable ingest/enrichment/embed entrypoints into library crates that `jurisearch-producer` can call, or state that the first implementation shells out to the existing `jurisearch` and `jurisearch-package` binaries with strict JSON parsing/checkpointing. Without that decision, Phase 2 is underspecified.

### NIT - Existing local archive incremental sync is omitted from the current-state section

The document correctly identifies that there is no remote DILA fetcher, but it overstates the local gap by saying there is no incremental "only new files" logic (`work/10-next-plans/02-auto-update-server-crons.md:140-147`). There is already a top-level `jurisearch sync --source --archives-dir --since` path that uses `ArchiveSyncFilter { incremental: true, since_compact }` and skips the baseline (`crates/jurisearch-cli/src/ingest.rs:29-78`, `322-350`; `crates/jurisearch-cli/src/args.rs:462-478`).

Recommended fix: update Part B to say "remote fetch cursor/listing is missing; local archive ingestion already has a timestamp-bounded incremental sync path." Then decide whether the producer command reuses that sync path, exposes the same filter as a library API, or replaces it.

### NIT - Several external-source claims need citations or softer wording

Some external factual claims were not verifiable from repository source and were only partially supported by current public listings: baseline reissue cadence "twice a year" (`work/10-next-plans/02-auto-update-server-crons.md:29-32`, `229-231`, `333-336`), FTP/SFTP fallback wording (`47-51`), and Judilibre same-day/within-week publication cadence (`109-111`). The repo verifies the code-facing pieces: PISTE production/sandbox base URLs and credentials (`crates/jurisearch-official-api/src/config.rs:46-58`, `119-151`), Judilibre `search`, `decision`, and `transactionalhistory` endpoints (`crates/jurisearch-official-api/src/client.rs:34-67`), and Legifrance search/OAuth behavior (`crates/jurisearch-official-api/src/client.rs:207-329`). It does not verify those public cadence/policy claims.

Recommended fix: add dated official citations next to those claims, or soften them to observed/current-state language. For the FTP line, prefer a narrow operational statement: "the supported implementation target is HTTPS; any non-HTTPS exchange-channel access is manual/operator-provided unless officially documented and tested."

## Confirmed claims

I confirmed the core architectural framing: the reviewed plan is right that DILA OPENDATA is exposed as an HTTPS Apache-style listing today; `ArchiveSource::ALL` is exactly `Legi`, `Cass`, `Capp`, `Inca`, and `Jade`; the parser recognizes the documented baseline and delta filename shapes (`crates/jurisearch-ingest/src/archive/parser.rs:12-18`, `23-78`); `plan_from_dir()` chooses the latest baseline and subsequent deltas (`crates/jurisearch-ingest/src/archive/planner.rs:45-140`); `jurisearch-package` exposes build/publish/publish-manifest/verify but no `producer_cycle` CLI verb (`crates/jurisearch-package-build/src/bin/jurisearch_package.rs:35-60`); and `jurisearch-syncd run` is already a poll/plan/verify/apply daemon with interval/backoff (`crates/jurisearch-syncd/src/main.rs:132-189`, `crates/jurisearch-syncd/src/daemon.rs:205-276`).

VERDICT: FIXES_REQUIRED
