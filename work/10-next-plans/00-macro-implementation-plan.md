# Macro implementation plan for deploy + update automation

Date: 2026-06-29

Scope: implementation roadmap for:

- `01-makeitsimpletodeploy.md` - site/client deployment, local query embeddings, release bundles.
- `02-auto-update-server-crons.md` - producer/update-server automation, DILA fetch, ingest, publish,
  and timers.

This document is intentionally higher level than the two detailed plans. It defines the order of work,
the cross-plan dependencies, the acceptance gates, and the resolved decisions that should stay fixed
when implementation starts.

---

## Target end state

The implementation is complete when all of the following are true:

1. Running `./dist.sh` from `/home/pierre/Work/jurisearch` creates a fresh repository-local
   `./dist/` directory with distinct `update-server`, `site-server`, and `cli` assets. It never writes
   to filesystem `/dist`.
2. The update-server host can fetch official DILA archives to Storebox, drive ingest/enrich/embed
   against the external producer PostgreSQL server, and publish signed `core` packages and manifests
   without manual steps.
3. The site-server host can be installed and operated through `jurisearchctl`: one config, generated
   units/env files, doctor checks, DB provisioning, trust bootstrap, catch-up, readiness, local bge-m3
   query embeddings, and smoke tests.
4. The thin client can be installed as a small separate artifact and configured once with a persistent
   site URL.
5. The producer uses fast external OpenAI-compatible/OpenRouter embeddings only for public legal-source
   document embeddings. Site/customer query embeddings are always local-only for confidentiality.
6. The release artifacts exclude large/runtime assets: databases, vector indexes, downloaded legal
   archives, corpus packages, manifests produced at runtime, model weights, tokenizer files, and
   credentials.
7. Future subscription add-ons such as INPI are delivered as separate corpora, so customers download
   only the add-on corpora configured for their site and covered by their installed license tokens.

---

## Implementation strategy

Use three coordinated workstreams:

1. **Shared deploy substrate.** Add the config, rendering, external PostgreSQL, and test foundations
   that both site deployment and producer automation need.
2. **Producer/update-server vertical slice.** Build the self-feeding package origin first enough to
   prove DILA bytes become signed `core` packages through the real external PostgreSQL path.
3. **Site/client deploy product.** Wrap the existing site, syncd, bge-m3, and client behavior in
   operator commands, then package all roles through `./dist.sh`.

The recommended first implementation target is the shared substrate plus producer vertical slice. The
reason is dependency-driven: the final `dist/update-server/` bundle cannot be complete until
`jurisearch-producer` and the producer service/timer templates exist, and the producer path carries the
hardest architectural risk: external PostgreSQL execution instead of the current `ManagedPostgres` /
`--index-dir` path.

---

## Non-negotiable invariants

- **Repository-local dist.** All generated release outputs go under `./dist/` in the repository root.
- **Confidentiality split.** Producer document embeddings may use OpenRouter because source text is
  public. Site query embeddings must use the local bge-m3 service and reject non-loopback providers.
  The loopback-only embedder validation is site-config-scoped; producer embedding config explicitly
  permits external providers for public-text document embedding.
- **Single `core` corpus for v1.** LEGI, CASS, CAPP, INCA, and JADE are fetch groups/sources, not
  package corpora. The package corpus remains `core`.
- **Subscription add-ons are separate corpora.** A future restricted source such as INPI must map to
  its own corpus, package stream, manifest, baseline/rebaseline chain, and entitlement policy. It
  should not be folded into `core`.
- **Subscription-aware download.** Sites should check configured corpora and local entitlement before
  downloading subscription-tier package artifacts. Apply-time entitlement checks still remain the hard
  security gate; pre-download checks are for bandwidth, storage, and operator clarity, not trust.
- **External producer PostgreSQL.** The update-server orchestrates; it does not host database/index
  state on its root disk or Storebox.
- **Archive cursor != package cursor.** Fetch/ingest selection is by DILA archive timestamp/name and
  per-archive journal state, not by package `change_seq`.
- **One `core` update lock.** Producer DB-mutating work is locked from ingest through enrich, embed,
  and `producer_cycle("core")`. A publish-only lock is not enough.
- **No huge payloads in release bundles.** Bundles contain binaries, templates, examples, checksums,
  manifests for fetchable assets, and docs only.
- **Systemd paths are absolute.** Generated units do not rely on unsupported environment expansion.

---

## Milestones

### M0 - Decision lock and implementation scaffolding

**Maps to:** `02` resolved decisions, `01` product decisions.

Record the resolved implementation decisions and add tracking issues or checklist items next to the
code work so the decisions do not get rediscovered mid-implementation.

Deliverables:

- Record that v1 producer orchestration is library-first: refactor ingest/enrich/embed/package
  entrypoints into reusable crates/APIs and have `jurisearch-producer` call them in-process.
- Record that LEGI/CASS/CAPP/INCA/JADE remain one `core` corpus for v1, while future restricted
  add-ons such as INPI are separate subscription corpora.
- Record that baseline refresh is automatic for v1: a new DILA global baseline triggers the rebaseline
  path without requiring a human command.
- Record that Judilibre freshness acceleration is deferred for v1; daily DILA polling is the v1
  freshness path.
- Record that Storebox retains all accepted official archives for v1.
- Record that first release artifacts are Linux `x86_64-unknown-linux-gnu` role tarballs using
  `.tar.zst`, with Debian packages deferred.

Done when:

- The decision defaults are written down in this macro plan or promoted into the detailed plans.
- The implementation can start without reopening product-shape questions during the first vertical
  slice.

### M1 - Shared config and external PostgreSQL substrate

**Maps to:** `01` Phase 1, `01` Phase 3, `02` Phase 2 prerequisite.

Build the foundation that makes both site and producer roles configurable and able to target an
operator-owned PostgreSQL server.

Deliverables:

- `crates/jurisearch-deploy` or equivalent shared deploy/config module.
- Strict `SiteConfig` parser and deterministic rendering for env files and systemd units.
- `jurisearchctl site init`, `site config-example`, `site validate`, and `site render`.
- Producer config parser for the minimum `[producer]`, `[database]`, `[fetch]`, `[package]`,
  `[enrichment]`, `[embedding]`, and `[baseline_refresh]` shape.
- Connection-based migration/provisioning path for external PostgreSQL, independent of
  `ManagedPostgres`.
- Reusable library entrypoints for ingest, enrichment, embedding, and package publishing, replacing
  the current binary-only orchestration surface where needed by `jurisearch-producer`.
- Redaction and file-permission helpers for password files and generated env files.

Acceptance gates:

- `jurisearchctl site config-example > site.toml` can render into a temp directory.
- A blank supported PostgreSQL instance can be provisioned by command or by reviewed SQL.
- Tests prove the external PostgreSQL path does not silently fall back to `ManagedPostgres`.
- Site configs that point query embeddings at OpenRouter or any other non-loopback URL are rejected.

### M2 - Producer fetch and update vertical slice

**Maps to:** `02` Phase 1 and Phase 2.

Implement enough of the update-server to prove the core data path: DILA listing -> archive mirror ->
ingest -> enrich/skip honestly -> document embed -> `producer_cycle("core")` -> signed manifest.

Deliverables:

- DILA remote listing parser and fetch cursor per source.
- Integrity gate for downloaded `.tar.gz` files.
- `jurisearch-producer fetch --source <src> [--dry-run]`.
- `jurisearch-producer provision-db --config <path>` for the producer database.
- `jurisearch-producer update --config <path> --group legislation|jurisprudence`.
- In-process orchestration through reusable ingest/enrich/embed/package library APIs. Shelling out to
  `jurisearch` / `jurisearch-package` is not the v1 design.
- Producer embedding env generation that separates storage fingerprint fields from provider
  request-model fields.
- Run checkpoints for fetch cursor, ingest journal, and package high-water mark.

Acceptance gates:

- Fetching the same source twice is a no-op after the first complete download.
- Corrupt/truncated downloads are quarantined and do not advance the cursor.
- Running producer update twice publishes once; adding one fixture delta publishes exactly one new
  signed incremental.
- Empty outbox still refreshes the signed manifest and exits zero.
- OpenRouter `request_model` never changes the stored `model_name`/dimension/normalize fingerprint.
- Producer/site example configs compute matching storage fingerprints.
- A failure before publish leaves a resumable checkpoint and no partial package.

### M3 - Producer scheduling, locking, and observability

**Maps to:** `02` Phase 3, Phase 4, and Phase 5.

Turn the manual producer update command into an unattended update-server service.

Deliverables:

- `jurisearch-producer-legislation.service` and `.timer`.
- `jurisearch-producer-jurisprudence.service` and `.timer`.
- Explicit lock implementation: optional per-source fetch locks plus one `update-core.lock` around all
  DB-mutating work.
- Structured run records under `state_dir/runs/...`.
- `jurisearch-producer status --json`.
- Classified exit codes and alert hook seam.
- Automatic `auto-on-new-baseline` behavior: when DILA publishes a newer global baseline for any
  source, the producer runs the rebaseline path and publishes a signed `core` rebaseline package.
- Cron-equivalent documentation for non-systemd hosts.

Acceptance gates:

- The legislation and jurisprudence timers cannot mutate the producer DB concurrently.
- A held jurisprudence run after ingest blocks a concurrent legislation publish from shipping
  half-processed scopes.
- Daily jurisprudence timer no-ops quiet sources and still catches JADE/CAPP/INCA promptly.
- A new DILA baseline is automatically adopted only by a recorded rebaseline run, never by treating
  deltas across the baseline boundary as ordinary incrementals.
- Existing and fresh sites converge through the published rebaseline package.
- `Persistent=true` recovers missed timer windows.
- `status --json` makes current/stale/broken state clear without reading logs.

### M4 - Site deploy product

**Maps to:** `01` Phase 2, Phase 4, Phase 5, and Phase 6.

Wrap the existing site server, sync daemon, database roles, trust bootstrap, local embedder, and
readiness checks in operator commands.

Deliverables:

- `jurisearchctl site doctor --config <path> [--json]`.
- `jurisearchctl site install --config <path> [--no-start|--dry-run]`.
- `jurisearchctl site uninstall|restart|stop|logs|status`.
- `jurisearchctl site bootstrap-trust --config <path>`.
- `jurisearchctl site catch-up --config <path> [--wait]`.
- `jurisearchctl site readiness --config <path>`.
- `jurisearchctl embed doctor --config <path>`.
- `jurisearchctl embed render-service --config <path>`.
- Optional `jurisearchctl embed fetch-assets` using a signed/checksummed asset manifest.

Acceptance gates:

- Doctor distinguishes missing DB, missing extension, stale readiness, occupied bind, missing package
  manifest, trust/license issues, and bad embedder state.
- Install refuses to start `jurisearch-site` until readiness and embed doctor are green, unless forced.
- Trust anchors are never silently replaced.
- Catch-up cannot be green with no active corpus or with a cursor behind the verified producer head.
- Query embedder config is loopback-only and fingerprint-compatible with active corpus metadata.

### M5 - Thin client and smoke acceptance

**Maps to:** `01` Phase 7 and Phase 8, `02` Phase 7.

Make the installed product easy to prove from a client machine and easy to monitor from a site.

Deliverables:

- `jurisearch-client configure --server <url>` and persistent XDG config.
- `jurisearch-client doctor`.
- `jurisearchctl demo up|url|smoke|down` using real binaries and a signed fixture corpus.
- `jurisearchctl site smoke --config <path> --fetch-id <id>`.
- Tiny signed fixture package for CI/demo smoke, with a documented stable fixture document id.
- Operated single-host acceptance script.
- Updated two-host acceptance runbook.
- Read-only site watchdog timer/status command.

Acceptance gates:

- Thin client remains free of storage, embedder, ingest, PostgreSQL, tokenizer, and producer
  dependencies.
- Smoke tests include status, fetch known id, BM25 search, hybrid search when configured, and negative
  checks.
- Demo mode exercises real status/fetch/search legs and skips hybrid only with an explicit recorded
  reason when model/tokenizer assets are absent.
- A stalled site sync cursor is detected and distinguished from "producer has no new packages".
- No smoke leg is silently skipped.

### M6 - Release packaging and role bundles

**Maps to:** `01` Phase 9, plus `02` Phase 3 release-boundary requirements.

Ship the implemented roles as repeatable release artifacts.

Deliverables:

- Repository-root `./dist.sh`.
- `./dist/manifest.toml` with version, git commit, targets, bundle files, checksums, binary versions,
  and external prerequisites.
- `./dist/update-server/` bundle with `jurisearch-producer`, producer config example, service/timer
  templates, checksums, and a Linux `x86_64-unknown-linux-gnu` `.tar.zst` role tarball. Because v1 is
  library-first, update-server correctness must not depend on bundling heavy CLI binaries only so the
  producer can shell out to them.
- `./dist/site-server/` bundle with `jurisearch`, `jurisearch-syncd`, `jurisearchctl`, templates,
  example `site.toml`, checksums, and a Linux `x86_64-unknown-linux-gnu` `.tar.zst` role tarball.
- `./dist/cli/` bundle with `jurisearch-client`, completions/manpage if generated, checksums, and
  a Linux `x86_64-unknown-linux-gnu` `.tar.zst` role tarball.
- `./dist/README.md` explaining bundle roles, excluded assets, prerequisites, and install commands.
- Debian packages are deferred until the tarball install/upgrade flow is stable.
- Upgrade/rollback commands or, if deferred, explicit stubs that fail with a clear "not implemented in
  this release" diagnostic.

Acceptance gates:

- `./dist.sh` recreates only repository-local `./dist/`.
- Each bundle installs independently without the source tree.
- Bundle audits fail if database data, legal archives, corpus packages, model weights, tokenizer files,
  or credentials are included.
- Update-server bundle is not declared complete until M2 and M3 artifacts, including automatic
  rebaseline behavior, exist.

### M7 - Rebaseline repair, freshness, and hardening follow-ups

**Maps to:** `02` Phase 5 and Phase 6, later hardening plans.

Automatic rebaseline is part of M3's unattended producer contract for v1. This milestone covers
operator repair affordances, post-v1 freshness acceleration, optional retention tooling, and hardening.

Deliverables:

- Manual `jurisearch-producer rebaseline --source <src>` remains available as an operator repair/debug
  command, but it is not the default operating mode.
- Post-v1 Judilibre freshness accelerator for same-day Cassation Bulletin decisions, explicitly
  deferred from v1.
- Optional retention tooling for temporary, partial, and quarantined files. Accepted official archives
  are retained indefinitely for v1.
- Credential hardening, least-privilege service users, and network hardening in a future security
  plan.

Acceptance gates:

- Manual rebaseline repair uses the same integrity/order/convergence checks as automatic rebaseline.
- Judilibre API unavailability degrades to DILA-only freshness and never blocks core updates.

---

## Parallelization plan

Work can run in parallel after M1 begins:

- One branch/person can build `jurisearchctl` config rendering and site doctor.
- One branch/person can build DILA fetch/cursor logic against fixtures.
- One branch/person can extract reusable ingest/enrich/embed/package entrypoints and external
  PostgreSQL migration/provisioning.

Do not build final `./dist/update-server/` contents until `jurisearch-producer update` and the
producer timers exist.

---

## Resolved decisions

These are no longer open for v1 unless the product direction changes:

1. **Producer orchestration is library-first.** Refactor ingest, enrichment, embedding, and package
   publishing into reusable APIs and have `jurisearch-producer update` call them in-process. This costs
   more up front than shelling out, but it gives typed configs/results/errors, cleaner resumability,
   fewer fragile environment translations, and better tests around the external PostgreSQL path.
2. **DILA legislation/jurisprudence stays in `core` for v1.** LEGI, CASS, CAPP, INCA, and JADE remain
   fetch groups/sources inside the single `core` package stream. Do not create a separate
   `jurisprudence` corpus just to split those sources in v1.
3. **Restricted add-ons are separate corpora.** When sources like INPI are added, model them as
   subscription corpora with their own source attribution, producer package stream, signed manifests,
   baseline/rebaseline chain, site cursor, and license entitlement policy.
4. **Add-on downloads are subscription-aware.** Site sync should avoid downloading subscription-tier
   package artifacts unless the site is configured for that corpus and has a local valid entitlement.
   The existing apply-time entitlement gate remains mandatory even if pre-download checks pass.
5. **Baseline refresh is automatic.** When DILA publishes a newer global baseline for a source, the
   producer should run the rebaseline path and publish a signed rebaseline package without waiting for
   a human command. This must be an explicit, recorded rebaseline run with normal integrity, ordering,
   and convergence checks.
6. **Judilibre freshness accelerator is deferred.** v1 relies on daily DILA polling for jurisprudence
   freshness. The accelerator remains documented as a later optional enhancement for same-day
   Cassation Bulletin freshness, but it should not be built into the first deploy/update release.
7. **Archive retention keeps everything accepted.** Storebox retains all accepted official baselines
   and deltas for v1. This preserves reproducibility, auditability, rebuilds, and debugging. Cleanup
   applies only to temporary/partial downloads and controlled quarantine handling.
8. **First release format is Linux role tarballs.** `./dist.sh` first emits
   `x86_64-unknown-linux-gnu` `.tar.zst` bundles for `update-server`, `site-server`, and `cli`, plus
   `./dist/manifest.toml`, checksums, and `./dist/README.md`. Debian packages are deferred.
9. **Producer install/admin commands live under `jurisearch-producer`.** For v1,
   `jurisearch-producer fetch|install|provision-db|update|status|rebaseline` and related subcommands
   own update-server runtime administration. `jurisearchctl` remains focused on customer site
   deployment and local query-service operations.
10. **Acceptance fixtures.** CI/demo acceptance uses a tiny signed fixture package with a documented
    stable fixture document id. Operated bear acceptance uses a real DILA document id after the
    producer has published real packages.

---

## Review gates

Run a review before each irreversible expansion point:

1. After M1, review external PostgreSQL provisioning and config validation.
2. After M2, review the producer update data path, especially cursor coordinate systems, embedding
   fingerprint parity, and no-partial-publish behavior.
3. After M3, review timer locking and systemd templates.
4. After M4/M5, review site confidentiality boundaries and smoke-test honesty.
5. After M6, review bundle contents and `./dist/manifest.toml`.

---

## Minimal first vertical slice

If implementation needs the smallest useful proof before polishing:

1. Implement producer config parsing and external PostgreSQL provisioning.
2. Extract reusable ingest/enrich/embed/package entrypoints needed by `jurisearch-producer`.
3. Implement DILA fetch against fixture listings and one real source.
4. Implement `jurisearch-producer update --group legislation` with in-process orchestration.
5. Publish one signed `core` package to a local served root.
6. Use existing `jurisearch-syncd`/site commands manually to apply and query it.
7. Only then wrap the same path in `jurisearchctl site install`, smoke tests, timers, and `./dist.sh`.

This slice validates the riskiest path first: official bytes become a signed package through the
external producer database, and a site can consume it.
