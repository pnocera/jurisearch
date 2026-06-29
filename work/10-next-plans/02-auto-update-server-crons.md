# Automate JuriSearch server updates from official sources — analysis + plan

Date: 2026-06-29
Scope: research + plan. This document does two things the stub asked for:

1. **Identify the core and jurisprudence official document sources** (the "possibly ftp or sftp"
   question), with concrete hosts, protocols, file naming, cadence, and licensing.
2. **Automate JuriSearch server updates** — design the scheduled producer pipeline that pulls new
   official documents and turns them into published, signed packages that sites apply automatically.

It does not redesign the package format, the query protocol, the trust model, or the work/09 site
sync semantics. It builds the *one missing seam*: getting new official bytes from the State's
distribution servers into the producer's `producer_cycle` on a schedule, without a human in the loop.

It is the **producer-side** counterpart to `01-makeitsimpletodeploy.md` (which is site-side deploy).

---

## TL;DR

- The official sources already match the code exactly. The bulk corpus comes from the **DILA
  "serveur d'échanges"** at `https://echanges.dila.gouv.fr/OPENDATA/`, which today is a plain
  **HTTPS directory listing** (Apache index), not FTP. FTP/SFTP is the *legacy* channel and is now
  granted only on request via `donnees-dila@dila.gouv.fr`. So the stub's "possibly ftp or sftp" is
  resolved: **fetch over HTTPS**, treat FTP as an optional fallback, do not build the pipeline around it.
- **Legislation** = `LEGI/` (consolidated codes, laws, regulations). **Jurisprudence** =
  `CASS/` + `CAPP/` + `INCA/` + `JADE/` (judicial + administrative case law). These are exactly the
  five `ArchiveSource` variants the ingest crate already parses. **Important:** these are *fetch/update
  groups*, **not** separate package corpora — in the current code `KNOWN_SOURCES` /
  `corpus_for_source()` map **all five sources to the single `core` package corpus**
  (`crates/jurisearch-package/src/corpus.rs`). So v1 fetches LEGI and jurisprudence on their own
  cadences but **packages everything as `core`** via one `producer_cycle("core")`. A true separate
  `jurisprudence` package corpus is a larger prerequisite (see Open decisions), not v1.
- Both use the same **DILA "Freemium"** archive scheme the code already understands: one big baseline
  `Freemium_<src>_global_<YYYYMMDD>-<HHMMSS>.tar.gz` (re-issued occasionally — the current LEGI/CASS
  baselines observed are dated 2025-07-13) plus dated deltas `<SRC>_<YYYYMMDD>-<HHMMSS>.tar.gz`.
  **Cadence is per-source, not uniform:** LEGI ~daily (~20:00–22:00 Europe/Paris) and JADE ~daily
  (consecutive days, weekend gaps) per the DILA listing; CASS ~weekly (5–8 day gaps); CAPP/INCA
  irregular (often several times a week). Treat "weekly jurisprudence" as **wrong** — schedule
  jurisprudence daily and no-op when a source has nothing new (verified 2026-06-29 against the DILA
  directory listings).
- The **PISTE** platform (`api.piste.gouv.fr`) is already wired in `jurisearch-official-api` for
  **Judilibre** (case-law enrichment / zones / fresh decisions) and the **Légifrance API**
  (legislation enrichment). It is an *enrichment + freshness accelerator*, not the bulk source.
- The **site side is already automated**: `jurisearch-syncd run` is a poll→plan→verify→apply daemon.
  What is missing is **producer-side automation**: there is no fetcher for the DILA server, and
  `producer_cycle()` exists in the library but is wired to neither a CLI verb nor a scheduler ("the
  CADENCE is the ops boundary", per its own module docs). **That seam is this plan.**
- **v1 producer storage uses an external PostgreSQL server.** On the current bear infra, the lightweight
  update-server CT (`jurisearch-update`, CT 111) fetches/mirrors official archives to Storebox and
  orchestrates the workflow, while ingest/enrich/embed/package DB work runs against the JuriSearch
  PostgreSQL 18 guest (`jurisearch`, CT 110). The existing code's `ManagedPostgres`-only producer path
  is therefore not sufficient for v1; Phase 2 must add the producer-side external-PG execution seam.
- **Embedding placement is deliberately split.** The producer embeds public legal-source documents and
  may use a fast external OpenAI-compatible provider such as OpenRouter for throughput. Site hosts embed
  **customer queries** only, and must use the local bge-m3 service for confidentiality; `syncd` applies
  already-embedded packages and never calls an external embedding API. In this plan, "server-side
  embeddings" means producer/update-ingest document embeddings over public source text, not
  customer-facing site query embeddings.

---

## Part A — Official document sources (the "identify the sources" deliverable)

### A.1 Primary bulk source: DILA "serveur d'échanges" (OPENDATA)

- **URL / protocol:** `https://echanges.dila.gouv.fr/OPENDATA/` — an open, unauthenticated **HTTPS**
  Apache directory listing. Each dataset is a sub-directory (`Index of /OPENDATA/<DATASET>/`).
- **Non-HTTPS channels:** the same host historically exposed FTP (`ftp://echanges.dila.gouv.fr/`), and
  DILA references contacting `donnees-dila@dila.gouv.fr` for data-directory access. **Decision /
  operational statement:** the *supported implementation target is HTTPS*. Any non-HTTPS
  exchange-channel access (FTP/SFTP) is manual / operator-provided and out of scope unless officially
  documented and tested — the pipeline is not built around it.
- **License:** Licence Ouverte / Open Licence 2.0 (Etalab), under the *Arrêté du 24 juin 2014* on free
  reuse of the State's legal databases. Reuse (including commercial) is allowed with attribution.
- **Operator / publisher:** DILA (Direction de l'information légale et administrative).

#### Datasets that map to JuriSearch corpora

All five sources currently package as the single **`core`** corpus (`KNOWN_SOURCES`); the "Fetch group"
column is the *scheduling* unit, not a package corpus.

| Fetch group | DILA dataset | Dir on echanges | `ArchiveSource` | Packages as | Delta cadence (observed 2026-06-29) | What it is |
|---|---|---|---|---|---|---|
| **legislation** | LEGI | `/OPENDATA/LEGI/` | `Legi` | `core` | **daily** (~20–22h) | Consolidated codes, laws, decrees, regulations (in force + historical versions) |
| **jurisprudence** | CASS | `/OPENDATA/CASS/` | `Cass` | `core` | **~weekly** (5–8 day gaps) | Cour de cassation — *published* decisions (Bulletin) |
| **jurisprudence** | INCA | `/OPENDATA/INCA/` | `Inca` | `core` | **irregular** (several/week) | Cour de cassation — *inédites* (unpublished) decisions |
| **jurisprudence** | CAPP | `/OPENDATA/CAPP/` | `Capp` | `core` | **irregular** (several/week) | Cours d'appel (appellate) decisions |
| **jurisprudence** | JADE | `/OPENDATA/JADE/` | `Jade` | `core` | **~daily** (weekend gaps) | Administrative justice (Conseil d'État, CAA, TA) |

> Two things vary independently and must not be conflated. **(1) Cadence is per-source** — do not assume
> a single jurisprudence rhythm; the scheduler (Phase 3) runs jurisprudence **daily** and no-ops sources
> with no new archive, so CASS's weekly drop and JADE's daily drop are both picked up promptly without a
> per-source timer matrix. **(2) Package corpus is single** — all of the above land in the `core` outbox
> and are published by one `producer_cycle("core")`.

Observed snapshot (2026-06-28/29), to calibrate sizing and cadence:

- `LEGI/`: baseline `Freemium_legi_global_20250713-140000.tar.gz` ≈ **1.1 GB**; daily deltas
  `LEGI_YYYYMMDD-HHMMSS.tar.gz` ranging **17 KB – 42 MB** (typically 1–6 MB), one per day.
- `CASS/`: baseline `Freemium_cass_global_20250713-140000.tar.gz` ≈ **248 MB**; deltas
  `CASS_YYYYMMDD-HHMMSS.tar.gz` ≈ **5.7 KB – 484 KB**, appearing roughly every 5–8 days.

> This is exactly the naming `crates/jurisearch-ingest/src/archive/parser.rs` already parses
> (`BASELINE_RE` / `DELTA_RE`, `ArchiveSource::ALL`). The fetcher therefore only has to *discover and
> download* files whose names the ingest planner already recognises and orders.

#### Other DILA datasets (future corpora, out of scope now)

`JORF` / `JORFSIMPLE` (Journal officiel), `KALI` (collective agreements), `CONSTIT` (Conseil
constitutionnel), `DOLE` (legislative dossiers), `CNIL`, `BODACC`, `BOAMP`, etc. The fetcher and
scheduler should be **dataset-generic** so these light up later by adding a corpus → dataset mapping,
not new code.

### A.2 Mirror / discovery layer: data.gouv.fr

Each fonds also has a catalog page on `data.gouv.fr` (e.g. `/datasets/legi-codes-lois-et-reglements-consolides`,
`/datasets/cass`, `/datasets/capp`, `/datasets/inca`, `/datasets/jade`). These mostly **point back to
the same `echanges.dila.gouv.fr` resources**. Value: a stable, machine-readable catalog (dataset
metadata, resource URLs, checksums when present) that can be used to *discover* the canonical bytes
without scraping HTML. **Decision:** treat data.gouv.fr as an optional discovery source; the canonical
bytes come from `echanges.dila.gouv.fr`.

### A.3 API layer: PISTE (Légifrance + Judilibre) — already wired

`crates/jurisearch-official-api` already speaks PISTE:

- **Base URLs:** `https://api.piste.gouv.fr` + `https://oauth.piste.gouv.fr` (sandbox variants exist).
- **Légifrance API** (OAuth2 `client_credentials`, `scope=openid`): consolidated-text search/fetch.
  Used for legislation enrichment / citation resolution. Creds:
  `JURISEARCH_PISTE_LEGIFRANCE_CLIENT_ID` / `_SECRET` (or `PISTE_OAUTH_CLIENT_ID/SECRET`).
- **Judilibre API** (`KeyId` header): `/cassation/judilibre/v1.0/search`, `/decision`,
  `/transactionalhistory` are already implemented; `/export` and `/taxonomy` are standard Judilibre
  endpoints we can add. Used for case-law **zone enrichment** (`enrich-zones`) and — importantly — as a
  **freshness feed** between DILA drops (notably for CASS's weekly rhythm). Cred:
  `JURISEARCH_PISTE_JUDILIBRE_KEY_ID` (or
  `PISTE_API_KEY`). `JURISEARCH_PISTE_ENV=production|sandbox`.
- **Quotas:** per-application, set on the PISTE portal; production quotas exceed sandbox; DILA may
  change them at any time. The crate already has `RetryPolicy` with backoff. The scheduler must be a
  *polite* client (bounded concurrency, honour `429/Retry-After`, run in low-traffic windows).
- **Judilibre coverage / cadence** (per the Cour de cassation / justice.gouv.fr open-data pages — see
  Sources; external policy, not verified from repo): Cour de cassation since 2021-09-30; cours d'appel
  and first-instance progressively (through 2024–2025); *published* (Bulletin) decisions reported
  **same day**, others **within ~1 week**. License: Open Licence 2.0.

**Role split (recommended):** DILA Freemium archives are the **canonical, deterministic, package-able
document source** (they reproduce byte-for-byte, which suits signed packages and the existing ingest).
PISTE/Judilibre is the **enrichment source** (official `zones`/`visa`) and an **optional freshness
accelerator** for Cassation decisions in the days before the next CASS (weekly) / INCA delta. Do not make the
canonical corpus depend on a live API.

---

## Part B — Current state vs. the gap

### B.1 What already exists (do not rebuild)

- **Ingest from on-disk archives:** `jurisearch ingest plan-archives | legi-archives | juri-archives`
  consume a `--archives-dir` of already-downloaded Freemium files, with run accounting, member byte
  limits, quarantine, and `--safe-mode`. Precedence/ordering (`baseline` then ordered `delta`s) is in
  `archive::planner` (`plan_from_dir`).
- **Enrichment + embedding steps:** `ingest enrich-zones`, `build-zone-units`, `embed-zone-units`,
  `embed-chunks`, `collect-legislation-citations`, `enrich-legislation-citations`.
- **Producer packaging:** `jurisearch-package build|publish|publish-manifest|verify`, and the library
  function `jurisearch_package_build::producer_cycle()` that runs *build-incremental → publish →
  refresh-remote-manifest* for one corpus from the outbox window — explicitly designed "callable by
  tests/CLI now and by a cron/daemon later (the CADENCE is the ops boundary)".
- **Site auto-update:** `jurisearch-syncd run` is already a poll→plan→verify→apply daemon
  (`daemon::run_daemon`) with `interval_secs`. Sites need no new cron; they need a healthy timer.

### B.2 What is missing (this plan)

1. **A DILA fetcher.** Nothing in the tree downloads from `echanges.dila.gouv.fr`; `ingest`/`sync`
   assume the archives already exist in a local `--archives-dir`. The *remote* side is what is missing:
   no directory-listing parser, no remote fetch cursor, no download/integrity step. Note the **local**
   side is already incremental: `jurisearch sync --source <s> --archives-dir <d> --since <ts>` uses
   `ArchiveSyncFilter { incremental: true, since_compact }` to skip the baseline and ingest only deltas
   newer than a timestamp (`crates/jurisearch-cli/src/ingest.rs`). The producer should reuse this path
   (or expose its filter as a library API) rather than reinvent local delta selection.
2. **An orchestrated producer update job.** No command chains *fetch → plan → ingest → enrich → embed →
   producer_cycle*. `producer_cycle()` is library-only — not exposed as a CLI verb and not scheduled.
3. **The schedule itself ("the crons").** No systemd timers / cron units, no locking, no per-source
   cadence (LEGI daily vs jurisprudence per-source — see A.1), no fail-closed alerting, no baseline-refresh policy, no
   retention/observability for an unattended run.

### B.3 Producer vs. site — resolving the title's ambiguity

"Automate jurisearch **server** updates" has two readings. Make the boundary explicit:

- **Update-server host (the package origin orchestrator): NEW automation — the subject of this plan.**
  A scheduled job fetches official deltas to Storebox, then drives ingest/enrich/embed/package work
  against the producer PostgreSQL database and publishes signed incremental packages + refreshed
  manifests to the served root. It is installed from the `dist/update-server/` release bundle produced
  by `01` Phase 9's root `./dist.sh` command; the bundle contains binaries/config/templates, not
  database contents, corpus package data, vector indexes, model weights, or tokenizer files.
- **Producer database host: existing bear CT 110 in v1.** The current deployment target is an external
  PostgreSQL 18 server at `192.168.0.110:5432`. CT 111 is intentionally lightweight and must not host
  the producer index/database itself.
- **Site hosts (query servers): ALREADY automated.** `jurisearch-syncd` polls the producer's manifest
  and applies. The only "cron-like" need here is a **watchdog/health timer** (Part D, Phase 7): assert
  the daemon is live, the cursor is advancing, and readiness is green — alert if a site falls behind.

---

## Part C — Target operator experience

### C.1 Producer host (new)

One config file; the orchestration command takes a **fetch group** (`legislation` / `jurisprudence`),
and always packages the single `core` corpus afterward.

```sh
# one-time DB bootstrap against the external producer PostgreSQL:
jurisearch-producer provision-db --config /etc/jurisearch/producer.toml

# one-shot, manual (fetch+ingest+enrich+embed a group, then package core):
jurisearch-producer update --config /etc/jurisearch/producer.toml --group legislation
jurisearch-producer update --config /etc/jurisearch/producer.toml --group jurisprudence

# what the timers run (idempotent; downloads may overlap, but ALL DB-mutating work holds one `core`
# update lock so a package never captures a half-processed scope; fail-closed):
sudo systemctl enable --now jurisearch-producer-legislation.timer   # daily
sudo systemctl enable --now jurisearch-producer-jurisprudence.timer # daily (no-ops if nothing new)

# observe:
jurisearch-producer status --config /etc/jurisearch/producer.toml --json
journalctl -u jurisearch-producer-legislation.service
```

`jurisearch-producer update --group <g>` performs, for the requested fetch group, end to end:

1. **fetch** — list the group's DILA dataset dir(s), download only files newer than the per-source
   fetch cursor, verify integrity, mirror into `archives/<src>/`.
2. **plan** — `archive::planner` over the local mirror (baseline precedence + ordered deltas).
3. **ingest** — `legi-archives` / `juri-archives` for the newly-arrived archives only (resume by run id).
4. **enrich** — `enrich-zones` (Judilibre) + legislation-citation steps, fail-closed-or-skip per
   credential policy (the existing `EnrichmentMode`).
5. **embed** — `embed-chunks` + `embed-zone-units`. This is producer-side document embedding over
   public official legal text, so v1 may use fast OpenRouter/OpenAI-compatible bge-m3 for throughput.
   This is intentionally different from site query embedding, which stays local for confidentiality.
6. **publish `core`** — `producer_cycle("core")` builds the next incremental from the **`core`** outbox
   window (which now contains this group's changes, since all sources attribute to `core`), signs it,
   publishes it, and refreshes the signed remote manifest. When the outbox window is empty it builds
   **no incremental** (`built_incremental = None`) but **still rebuilds and republishes the signed
   remote manifest** — it is not a full no-op. If "publish absolutely nothing on an empty window" is
   wanted, the orchestrator must do a preflight outbox check *before* calling `producer_cycle()`.
   **Locking — must span the whole DB-mutating workflow, not just publish.** Because both groups write
   the *same* `core` outbox and the package builder selects by corpus (not by source or completed-run
   window), the `core` update lock must be acquired **before ingest** and held through enrich → embed →
   `producer_cycle("core")`. Scoping the lock to publish only is **unsafe**: the jurisprudence timer
   could ingest CASS/JADE rows into the `core` outbox, pause before enrich/embed, and a concurrent
   legislation timer could then build a `core` incremental that captures those *un-enriched,
   un-embedded* scopes. Only the pure network download (no DB writes) may run outside the lock.

Sites then pick it up on their own `jurisearch-syncd` poll. No producer→site push.

### C.2 Minimum producer config shape

```toml
[producer]
corpora_dir   = "/srv/jurisearch/storebox/packages"   # served root (manifest + packages)
archives_dir  = "/srv/jurisearch/storebox/archives"   # downloaded DILA mirror (per-source subdirs)
state_dir     = "/var/lib/jurisearch-producer"         # small local orchestration state

[database]
# Producer-side EXTERNAL PostgreSQL. This is new v1 capability: today's ingest/enrich/embed/package
# chain is tied to ManagedPostgres/--index-dir, so Phase 2 must add or thread a connection-based
# producer storage path. On bear this points from CT 111 to CT 110.
host = "192.168.0.110"
port = 5432
name = "jurisearch"
admin_user = "postgres"
admin_database = "postgres"
admin_password_file = "/etc/jurisearch/secrets/postgres-admin-password"
sslmode = "disable" # bootstrap/private-LAN setting for bear; hardening is a future plan

[fetch]
base_url      = "https://echanges.dila.gouv.fr/OPENDATA"
user_agent    = "jurisearch-producer/<version> (+contact)"
max_concurrency = 2
timeout_secs  = 120
retain_deltas = "all"        # or a window; baselines retained until superseded + applied

# Fetch groups = SCHEDULING units (what a timer fetches+ingests), NOT package corpora.
# Generic so JORF/KALI/CONSTIT light up later by adding a source here + to KNOWN_SOURCES.
[[fetch_group]]
name    = "legislation"
sources = ["legi"]
cadence = "daily"

[[fetch_group]]
name    = "jurisprudence"
sources = ["cass", "inca", "capp", "jade"]   # different rhythms; run daily, no-op when nothing new
cadence = "daily"

[package]
# v1: ALL ingested sources attribute to the single `core` corpus (KNOWN_SOURCES in
# crates/jurisearch-package/src/corpus.rs). One producer_cycle, one manifest, one `core` update lock.
corpus = "core"

[enrichment]
mode = "auto"   # run when PISTE creds present, else SkippedNoCredentials (never fabricated)
# PISTE creds come from systemd EnvironmentFile / credentials, never inline here:
#   JURISEARCH_PISTE_ENV, JURISEARCH_PISTE_JUDILIBRE_KEY_ID,
#   JURISEARCH_PISTE_LEGIFRANCE_CLIENT_ID / _SECRET

[embedding]
# Producer-side DOCUMENT embedding. These inputs are public legal-source texts, not confidential
# customer queries, so v1 may use a fast external OpenAI-compatible provider such as OpenRouter.
# Site query embedding remains local-only; see 01-makeitsimpletodeploy.md.
provider = "openai_compatible"
base_url = "https://openrouter.ai/api/v1"
# Canonical storage-fingerprint model. Must match the site-local query embedder exactly.
model_name = "bge-m3"
# Provider request model. OpenRouter needs this provider-specific id, but it must not change storage
# fingerprints.
request_model = "baai/bge-m3"
dimension = 1024
normalize = true
pooling = "cls"
api_key_env = "OPENROUTER_API_KEY"
# The storage fingerprint must match the site-local query embedder's storage fingerprint
# (model, dimension, normalize). Pooling is a deploy/package validation rule, not part of the storage
# fingerprint.

[baseline_refresh]
# DILA re-issues Freemium baselines occasionally (no fixed schedule; detect from the listing).
# Adopting a new baseline = a rebaseline, an explicit policy — see Phase 5.
mode = "manual"   # "manual" | "auto-on-new-baseline"
```

---

## Part D — Phased implementation plan

### Phase 1 — DILA remote listing + fetch cursor (read-only)

- **Goal.** Turn `https://echanges.dila.gouv.fr/OPENDATA/<SRC>/` into a verified, incremental download
  into the local mirror, without ingesting anything.
- **Builds on.** `archive::ParsedArchive::parse_file_name` (already validates names), `ArchiveSource`.
- **Deliverables.**
  - A `jurisearch-fetch` crate (or module): list a dataset directory (parse the Apache index), filter
    to names that parse for the requested `ArchiveSource`, and compute the "new since cursor" set.
  - A persistent **fetch cursor** per source under `state_dir` (highest `ArchiveTimestamp` fetched +
    seen-baseline id), so re-runs only pull genuinely new files.
  - Conditional download (`If-Modified-Since` / `HEAD` size+date precheck), bounded concurrency,
    timeout, retry/backoff (reuse `RetryPolicy`), polite `User-Agent`.
  - Integrity gates: enforce `.tar.gz` decompresses and is a readable tar before the file is accepted
    into the mirror (DILA does not publish per-file checksums alongside the archives; treat a clean
    gunzip+tar open as the integrity proof, quarantine otherwise).
  - `jurisearch-producer fetch --source legi --dry-run` lists what *would* be downloaded.
- **Invariants under test.**
  - A name that does not parse for the requested source is ignored, never downloaded (cross-source
    safety, mirroring `parser.rs::rejects_cross_source_filename`).
  - Re-running fetch with no new remote files downloads nothing and exits zero.
  - A truncated/corrupt download is quarantined and does not advance the cursor.
  - The cursor only advances for files fully downloaded *and* integrity-checked.
- **Done when.** `fetch --source legi` populates `archives_dir/legi/` with exactly the new baseline +
  deltas, and a second run is a no-op.

### Phase 2 — `producer update`: orchestrate fetch → ingest → enrich → embed → publish

- **Goal.** One idempotent command that takes a **fetch group** from "new bytes on DILA" to a
  "published signed `core` package", reusing every existing step.
- **Prerequisite.** Resolve open-decision #1 (orchestration boundary) first — it determines whether
  this command calls library functions or shells out to `jurisearch` / `jurisearch-package`.
  Additionally, implement the producer-side external PostgreSQL seam: today's ingest/enrich/embed and
  `producer_cycle()` flow is `ManagedPostgres`/`--index-dir` based, but v1 must run those DB-mutating
  steps against the configured external producer database (`[database]`, CT 110 on bear). A Phase 2
  test must prove the external-PostgreSQL mode actually drives ingest/enrich/embed/package end to end.
- **Builds on.** Phase 1 fetch; `ingest plan-archives/legi-archives/juri-archives` (+ the existing
  `jurisearch sync --since` incremental filter); `enrich-zones`, `build-zone-units`,
  `embed-zone-units`, `embed-chunks`; `producer_cycle()`.
- **Deliverables.**
  - `jurisearch-producer provision-db --config <path>` for the external producer PostgreSQL database.
    It installs/checks required extensions (`pgvector`, `pg_search`), roles/grants as needed for the
    producer workflow, and storage/package migrations before the first `update` run. This may reuse
    the connection-based migration applier introduced by `01` Phase 3, but it targets the producer DB
    on CT 110, not a customer site DB. An unprovisioned external PG must fail with a clear
    "run producer provision-db first" diagnostic, not a raw SQL error.
  - `jurisearch-producer update --config <path> --group <legislation|jurisprudence>` driving the full
    chain per Part C.1, with `--dry-run`, `--skip-fetch`, `--skip-enrich`, and `--only <step>` for ops.
    The packaged corpus is always `core` (from `[package].corpus`), not the group name.
  - Wire `producer_cycle("core")` to this command (the missing CLI seam). Run the **DB-mutating** part
    of `update` (ingest → enrich → embed → `producer_cycle("core")`) under `state_dir/update-core.lock`,
    acquired **before ingest** (see Phase 3); the package builder's existing per-corpus build lock is
    only an internal build-snapshot safeguard, not the workflow lock. Record the `ProducerCycleReport`
    (built incremental id or none, manifest path, enrichment outcome).
  - Producer embedding configuration and environment wiring for fast document embedding through
    OpenRouter/OpenAI-compatible bge-m3. The command must keep storage fingerprint fields
    (`model_name`, `dimension`, `normalize`) separate from provider request fields (`request_model`,
    `base_url`, credentials). It must verify that the produced storage fingerprint matches the
    site-local query embedder contract before publishing a package. Under the recommended shell-out
    orchestration path, `request_model` must be passed through the pool spec
    (`JURISEARCH_EMBED_POOL` / `EmbeddingPoolEndpointConfigFile.request_model`), not by setting
    `JURISEARCH_EMBED_MODEL`; the single-endpoint env surface has no request-model field and would
    corrupt the stored fingerprint. It must never reuse this external provider for site/customer query
    text.
  - **Three explicit cursors — do not conflate the clocks** (these live in different coordinate
    systems and must stay separate):
    1. **Fetch cursor** (per DILA source, from Phase 1): highest `ArchiveTimestamp` + filename +
       size/mtime + baseline id seen on the remote server.
    2. **Ingest journal** (per accepted archive *filename* / run id / status): what has been streamed
       into canonical storage. Archive *selection* is by **DILA archive timestamp/name** — the existing
       ingest surface already filters this way (`ArchiveSyncFilter { incremental, since_compact }`,
       `select_archives_to_process()` comparing `ArchiveTimestamp::compact()`), NOT by package sequence.
    3. **Package high-water mark** (in outbox / `change_seq` space): owned by `producer_cycle()`, which
       packages the next outbox window *after* ingest has written rows. The producer never uses a
       `change_seq` to decide which archive filenames to fetch/ingest.
- **Invariants under test.**
  - Empty outbox window ⇒ `producer_cycle` builds **no incremental** but **still refreshes the signed
    manifest**; the command exits zero. (For "publish nothing", a preflight outbox check must gate the
    call — test that wrapper separately.)
  - A blank but reachable external producer PostgreSQL fails before ingest with a clear
    `producer-db-unprovisioned`/equivalent diagnostic; after `provision-db`, the same `update` reaches
    the ingest step.
  - Enrichment without PISTE creds yields `SkippedNoCredentials`, never a fabricated "ran" (the
    manifest must not claim enrichment that did not happen — existing cycle contract).
  - Producer document embedding may use OpenRouter because the input documents are public; site query
    embedding remains local-only. A config that would send site/customer query text to an external
    provider is outside this plan and must be rejected by the site deployment plan.
  - Producer and site embedders must agree on the storage fingerprint (model, dimension, normalize);
    mismatch fails before publish or before serving, not at query time.
  - The producer OpenRouter request model (`request_model = "baai/bge-m3"`) does not change the stored
    fingerprint (`model_name = "bge-m3"`); tests compute the producer and site fingerprints from the
    example TOMLs and assert equality.
  - Shell-out embedding env generation maps `request_model` through `JURISEARCH_EMBED_POOL` and never
    through `JURISEARCH_EMBED_MODEL`; a regression test catches the fingerprint-changing mistake.
  - The configured producer database is the external PostgreSQL server, not a local `index_dir`; the
    test chain must fail if any step silently falls back to `ManagedPostgres`.
  - A failure in any step leaves a resumable checkpoint and does **not** publish a partial package.
  - **Archive selection is by archive timestamp, not change sequence:** a new DILA delta that lands
    *after* a package was published is selected for ingest by its `ArchiveTimestamp`, regardless of the
    current `change_seq` (regression test required — this is the BLOCKER-2 trap).
  - Ingest does not reprocess already-journaled archive filenames (no re-streaming the whole mirror
    each night).
- **Done when.** Running `update` twice in a row publishes once; an injected new delta publishes
  exactly one new incremental that `jurisearch-package verify` accepts against the public key.

### Phase 3 — Scheduling: systemd timers / cron units ("the crons")

- **Goal.** Run `update` unattended on the right cadence, safely, with no overlap.
- **Builds on.** Phases 1–2; the `deploy/systemd/*.service` conventions from work/09 + `01-…deploy`.
- **Deliverables.**
  - `jurisearch-producer-legislation.service` + `.timer` — **daily**, e.g. `OnCalendar=*-*-* 22:30
    Europe/Paris` (after LEGI's ~20–22h drop) with `RandomizedDelaySec` to avoid hammering on the hour.
  - `jurisearch-producer-jurisprudence.service` + `.timer` — **daily** (e.g. `OnCalendar=*-*-* 23:30`),
    **not weekly**: the four jurisprudence sources have different rhythms (CASS ~weekly, JADE ~daily,
    CAPP/INCA irregular), so a daily run that **no-ops sources with no new archive** picks each up
    promptly without a per-source timer matrix. A weekly timer would add up to ~6 days of latency to
    JADE/CAPP/INCA for no benefit.
  - Producer service/timer templates, example `producer.toml`, and any required shell-out binaries are
    shipped by the root release script in the distinct `dist/update-server/` bundle. Runtime outputs
    from the producer are not release-bundle payloads. Downloaded archives, packages, manifests, and
    temporary download files live on Storebox in the current bear deployment; database/vector-index
    state lives in the PostgreSQL CT, not in the update-server bundle or rootfs.
  - A cron equivalent (documented) for non-systemd hosts.
  - **Mutual exclusion — one `core` update lock around all DB-mutating work.** Both timers write the
    *same* `core` outbox, and the package builder selects rows by corpus (not by source/group/run), so
    they **cannot mutate the producer DB concurrently**. A single `core` update lock
    (`state_dir/update-core.lock`) is acquired **before ingest** and held through enrich → embed →
    `producer_cycle("core")`. **Contract = bounded wait, not silent skip:** a scheduled/manual run
    waits up to a bounded timeout for the lock, so a closely-spaced run (e.g. the 22:30 and 23:30
    timers, or a manual run during a timer) proceeds *after* the current workflow finishes and still
    publishes its fetched group — rather than dropping that work until the next daily firing. Only if
    the wait times out does the run exit with a distinct **`skipped-lock-held`** status (a recorded
    contention signal, not a silent no-op). Only the pure remote **download** (network, no DB writes)
    may run outside the lock — an optional
    per-source `state_dir/fetch-<src>.lock` just prevents two runs racing the same files into the
    mirror. **Do not** scope the lock to publish only: the build lock + outbox fence serialize the build
    *snapshot* but do **not** isolate a caller's preceding ingest/enrich/embed, so a publish-only lock
    would let one group package the other's half-processed scopes. (`systemd` `RefuseManualStart` is not
    enough — use explicit flocks so cron and manual runs are covered too.)
    - *If* truly concurrent per-group DB mutation is ever required, that is a larger design: add
      completed-run/source eligibility to the outbox and teach the builder to package only completed
      workflow windows. Out of scope for v1 — call it out, don't pretend the current builder supports it.
  - Service hardening matching the existing units: dedicated user, `ProtectSystem`,
    `ReadOnlyPaths`/`ReadWritePaths` with **absolute** paths, `EnvironmentFile` for PISTE creds
    (mode 0600 / systemd credentials), `Nice`/`IOSchedulingClass` so a 1 GB baseline pull doesn't
    starve the box.
- **Invariants under test.**
  - The two timers never mutate the producer DB concurrently (single `core` update lock held across
    ingest→enrich→embed→publish), even when both fire close together.
  - **No half-processed publish:** hold a jurisprudence run after ingest but before embed, fire the
    legislation timer — it must block on the `core` update lock and must **not** publish a `core`
    package containing the not-yet-embedded jurisprudence scopes until the jurisprudence run completes.
  - Timer misfire (host asleep at 22:30) still runs via `Persistent=true`.
  - Generated units use absolute paths and don't rely on unsupported env expansion (same rule as
    `01-…deploy` Phase 4).
- **Done when.** Enabling the two daily timers yields a producer that publishes a new `core` package
  whenever LEGI drops *or any* of CASS/CAPP/INCA/JADE drops, with zero manual steps and no wasted work
  on quiet days (empty runs no-op), and a missed window self-heals on the next boot/firing.

### Phase 4 — Failure handling, observability, and alerting

- **Goal.** Make an unattended pipeline *safe to ignore until it breaks*, and loud when it does.
- **Builds on.** Phases 1–3.
- **Deliverables.**
  - Structured run records (`state_dir/runs/<group>/<ts>.json`): per-step status, bytes fetched,
    archives ingested, decisions enriched, change_seq window, published id, duration, exit class.
  - `jurisearch-producer status --json`: last run per fetch group, last published `core` package + manifest head,
    "behind upstream?" (compare local fetch cursor to the newest remote file), enrichment health.
  - Classified exit codes: `published` / `no-op` / `skipped-lock-held` / `upstream-unreachable` /
    `integrity-failed` / `ingest-failed` / `enrich-degraded` / `publish-failed` — so a timer wrapper can alert
    appropriately (e.g. degraded enrichment is a warning; publish-failed is a page).
  - A hook seam (command/webhook) on non-zero so operators wire their own alerting; **no built-in
    external calls** (consistent with the project's no-hidden-egress stance).
- **Invariants under test.**
  - `upstream-unreachable` (DILA 5xx / timeout) is distinct from `integrity-failed` and never advances
    the cursor or publishes.
  - A degraded run (enrichment skipped) publishes a correctly-labelled package and is reported as
    `enrich-degraded`, not silent success.
- **Done when.** A reviewer can read one `status --json` and know whether the corpus is current,
  stale, or broken, and why.

### Phase 5 — Baseline-refresh (rebaseline) policy

- **Goal.** Handle DILA re-issuing the `Freemium_<src>_global_*` baseline (cadence not contractually
  documented — the LEGI/CASS baselines on the server were observed dated 2025-07-13; design for
  "occasionally, on no fixed schedule", detected from the listing, not a calendar) without drift or
  accidental full reprocessing.
- **Builds on.** `build_rebaseline` / `apply_rebaseline` already in the package/syncd crates;
  `archive::planner` baseline precedence.
- **Deliverables.**
  - Detection: fetch notices a new baseline id newer than the cursor's known baseline **for a given
    source** (baselines are per-source: `Freemium_legi_global_*`, `Freemium_cass_global_*`, …).
  - Policy gate (`baseline_refresh.mode`): `manual` (default — fetch + alert, await
    `jurisearch-producer rebaseline --source <src>`) or `auto-on-new-baseline`.
  - A `rebaseline` path that re-ingests the new per-source baseline and emits a **`core` rebaseline
    package** (the package corpus is still `core`) so sites re-anchor via the existing
    `apply_rebaseline`, rather than trying to delta across a baseline cut. Note a new baseline for *one*
    source still rebuilds the whole `core` baseline, since `core` spans all sources — call this out in
    ops docs as a heavier operation than a delta.
- **Invariants under test.**
  - A new baseline is never silently adopted under `manual`.
  - Deltas straddling a baseline boundary are ordered correctly / rejected, never mis-applied.
  - Sites converge to the new baseline through the published rebaseline package, not an ad-hoc reset.
- **Done when.** A simulated baseline re-issue produces a published rebaseline that a fresh and an
  existing site both converge on.

### Phase 6 — (Optional) Judilibre freshness accelerator

- **Goal.** Close the up-to-7-day gap created by CASS's weekly DILA delta for *published* (Bulletin)
  Cassation decisions, using the API already in `jurisearch-official-api`.
- **Builds on.** `judilibre_transactional_history`, `judilibre_search`, `judilibre_decision`.
- **Deliverables.**
  - An incremental Judilibre pull (via `/transactionalhistory` or `/export`) that feeds the *same*
    canonical decision tables/outbox the DILA ingest writes, deduped by provider id, so a later DILA
    delta is a no-op rather than a conflict.
  - Off by default; enabled per-source (CASS/INCA); strictly rate-limited and credential-gated.
- **Invariants under test.**
  - A decision pulled via API and later seen in a DILA delta does not double-insert or fork identity.
  - API unavailability degrades to "DILA-only freshness", never blocks the core pipeline.
- **Done when.** With the accelerator on, a same-day Bulletin decision is queryable before its weekly
  DILA delta, with no divergence once the delta arrives.

### Phase 7 — Site-side watchdog (health, not new sync)

- **Goal.** Since sites already auto-sync, give operators a timer that *proves* they're keeping up.
- **Builds on.** `jurisearch-syncd status`, the work/09 readiness signals, `01-…deploy` `site readiness`.
- **Deliverables.**
  - A lightweight `site watchdog` timer: assert daemon active, cursor advanced within N intervals,
    readiness green, embedder fingerprint matches active corpus; classify + (optionally) alert.
- **Invariants under test.**
  - A stalled cursor (daemon up but not advancing) is detected and distinguished from "no new packages".
  - Watchdog never mutates state.
- **Done when.** A site silently falling behind the producer is surfaced within one cadence interval.

---

## Test matrix

| Area | Required tests |
|---|---|
| Remote listing | parse Apache index; ignore non-matching names; cross-source rejection; empty dir |
| Fetch cursor | only-new selection; no-op re-run; cursor advances only after integrity pass; resume after crash |
| Integrity | corrupt gzip quarantined; truncated download not accepted; clean tar accepted |
| Producer DB provisioning | blank external PG reports "run producer provision-db first"; provision installs extensions/roles/migrations; rerun idempotent |
| Producer DB model | external PostgreSQL mode actually drives ingest/enrich/embed/package end to end against the configured producer DB; no silent `ManagedPostgres` fallback |
| Update-server release | `dist/update-server/` contains producer binary, required shell-out binaries, config and service/timer templates; excludes DB/index/archive/package/model payloads |
| Producer embedding | OpenRouter/OpenAI-compatible document embedding over public text; provider `request_model` separate from fingerprint `model_name`; shell-out maps request model through `JURISEARCH_EMBED_POOL`, not `JURISEARCH_EMBED_MODEL`; no external site-query embedding; producer/site storage fingerprint parity computed from example configs |
| Cursor coordinate systems | archive selected by `ArchiveTimestamp` after a published package (NOT by `change_seq`); ingest journal skips already-done filenames |
| Corpus attribution | jurisprudence ingest (cass/capp/inca/jade) produces **`core`** outbox rows (`corpus_for_source`); `producer_cycle("core")` packages them; `--group jurisprudence` never targets a non-existent `jurisprudence` corpus |
| Orchestration | empty outbox ⇒ no incremental built BUT manifest refreshed; "publish nothing" only via preflight gate; resumable mid-step failure; no partial publish |
| Enrichment honesty | no creds ⇒ `SkippedNoCredentials`; manifest never claims un-run enrichment |
| Scheduling | single `core` update lock spans ingest→enrich→embed→publish (download may overlap); **held jurisprudence run blocks a concurrent legislation publish from shipping half-processed scopes**; daily run no-ops when a source has nothing new; persistent misfire recovery; absolute-path units |
| Observability | exit-class taxonomy; `status --json` current/stale/broken; behind-upstream detection |
| Rebaseline | manual gate holds; auto mode adopts once; delta/baseline boundary ordering |
| Judilibre accel | dedup vs DILA delta; API-down degrades gracefully |
| Site watchdog | stalled-cursor detection; read-only |

---

## Open decisions (need a human call)

1. **Crate placement + orchestration boundary.** The full chain needs *ingest*, *Judilibre/Légifrance
   enrichment*, and *embedding* — which today live in the `jurisearch-cli` **binary** crate (only
   `[[bin]]`, no reusable library), while `jurisearch-package-build` depends only on
   `jurisearch-package` / `jurisearch-storage`. So a `jurisearch-producer` bin in
   `jurisearch-package-build` **cannot directly call** those steps as library functions. Two options:
   - **(A, recommended for v1) Orchestrate by shelling out** to the existing `jurisearch` and
     `jurisearch-package` binaries with strict `--json` parsing + per-step checkpointing. Fewest moving
     parts; no refactor; the orchestrator owns only fetch + the three cursors + scheduling. This still
     requires adding an external-PostgreSQL execution mode to the invoked commands; shelling out must
     not silently fall back to `--index-dir`/`ManagedPostgres`.
   - **(B, later) Extract reusable library crates** for ingest/enrich/embed entrypoints so a producer
     bin can call them in-process. Cleaner long-term, but a real refactor of `jurisearch-cli`.
   *Recommendation: ship (A); treat (B) as a follow-up. Phase 2 is underspecified until this is chosen,
   so it is a prerequisite decision, not a detail.* (Mirrors the `jurisearchctl` decision in `01-…deploy`.)
2. **Single `core` corpus vs split `jurisprudence` corpus.** Today `KNOWN_SOURCES` attributes all five
   sources to `core`, so v1 packages legislation + jurisprudence as one `core` chain (this plan's
   model). A genuine `jurisprudence` package corpus — so sites could subscribe to case law without
   legislation — is a **larger prerequisite project**: change `KNOWN_SOURCES`, extend the storage
   backfill `CASE` in lock-step (the drift test enforces this), create/publish a `jurisprudence`
   baseline + catalog row, teach sites to subscribe to both, and add attribution regression tests.
   *Recommendation: keep the single `core` corpus for v1; treat the split as its own scoped effort if a
   product requirement needs corpus-level separation.*
3. **Producer DB model.** Resolved for v1: use the external PostgreSQL producer database. On bear,
   CT 111 (`jurisearch-update`) is the lightweight scheduler/fetch/orchestration host and CT 110
   (`jurisearch`) is the PostgreSQL 18 database host. This requires new producer-side connection-based
   storage execution because today's ingest / enrich / embed / `producer_cycle()` path is still
   `ManagedPostgres`/`--index-dir` based. Do not implement a local `producer.index_dir` fallback for
   v1; it would put database data/vector indexes on the update-server root disk or on Storebox, both of
   which are explicitly wrong for the current deployment.
4. **Jurisprudence freshness.** Ship Phase 6 (Judilibre accelerator) now, or rely on DILA cadence for
   v1? Note DILA jurisprudence is **mostly already daily** (JADE daily; CAPP/INCA several times a
   week), so the only real same-day gap the accelerator closes is **CASS published (Bulletin)
   decisions**, which can appear in Judilibre the day of but only land in the weekly CASS delta later.
   *Recommendation: defer — a daily jurisprudence timer over DILA already keeps JADE/CAPP/INCA current;
   add the Judilibre accelerator only if same-day **Cassation Bulletin** case law is a product
   requirement.*
5. **Baseline-refresh default.** `manual` (safe, recommended) vs `auto-on-new-baseline`.
6. **Where the producer runs.** Resolved for current v1 deployment: a dedicated update-server CT
   (`jurisearch-update`, CT 111) orchestrates fetch/schedule/publish work, while DB-heavy work targets
   the dedicated PostgreSQL CT (`jurisearch`, CT 110). Future deployments may co-locate those roles,
   but the implementation must not require co-location.
7. **Retention.** How long to keep downloaded deltas/baselines in `archives_dir` after they're ingested
   and published (disk vs. reproducibility).

---

## Boundary with `01-makeitsimpletodeploy.md`

`01` makes a **site** deployable (config, doctor, provision-db, install, trust, readiness, smoke).
**This plan** makes the **producer** *self-feeding* (fetch official sources → ingest/enrich/embed →
publish on a schedule). They meet at exactly one artifact: the **signed remote manifest + packages**
under the served root. `01`'s sites consume it via `jurisearch-syncd`; this plan's producer keeps it
fresh. `01` Phase 9 owns the root `./dist.sh` release builder and must emit separate
`update-server`, `site-server`, and `cli` bundles. The `site-server` and `cli` bundles can be completed
from `01` alone; the `update-server` bundle is complete only after this plan's Phase 2-3 produce
`jurisearch-producer` and producer service/timer templates. This plan defines the update-server runtime
contents.
Neither changes the package format, trust model, or query protocol.

---

## Sources

- [DILA OPENDATA index — echanges.dila.gouv.fr/OPENDATA](https://echanges.dila.gouv.fr/OPENDATA/)
- [DILA LEGI archives directory](https://echanges.dila.gouv.fr/OPENDATA/LEGI/)
- [DILA CASS archives directory](https://echanges.dila.gouv.fr/OPENDATA/CASS/)
- [LEGI dataset — data.gouv.fr](https://www.data.gouv.fr/datasets/legi-codes-lois-et-reglements-consolides)
- [CASS dataset — data.gouv.fr](https://www.data.gouv.fr/datasets/cass) ·
  [CAPP](https://www.data.gouv.fr/datasets/capp) ·
  [INCA](https://www.data.gouv.fr/datasets/inca) ·
  [JADE](https://www.data.gouv.fr/datasets/jade)
- [Légifrance — Open data et API](https://www.legifrance.gouv.fr/contenu/pied-de-page/open-data-et-api)
- [Légifrance — FAQ API (quotas)](https://www.legifrance.gouv.fr/contenu/pied-de-page/foire-aux-questions-api)
- [API Légifrance — data.gouv.fr](https://www.data.gouv.fr/dataservices/legifrance)
- [Cour de cassation — Open data et API (Judilibre)](https://www.courdecassation.fr/acces-rapide-judilibre/open-data-et-api)
- [API Judilibre — data.gouv.fr](https://www.data.gouv.fr/dataservices/api-judilibre)
- [Ministère de la Justice — Open data des décisions de justice](https://www.justice.gouv.fr/documentation/open-data-decisions-justice)
- [PISTE — catalogue des API](https://piste.gouv.fr/api-catalog-sandbox) ·
  [AIFE — PISTE](https://aife.economie.gouv.fr/nos-applications/piste/)
- Licence: *Arrêté du 24 juin 2014* (réutilisation libre des bases de données juridiques) — Licence Ouverte / Etalab 2.0
