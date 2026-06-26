# Central ingest + packaged read-only client distribution — analysis

Date: 2026-06-25 (revised 2026-06-26: distribution model decided as per-corpus packages)
Scope: analysis only — no design, no implementation plan.

> This revision treats all prior product decisions (`work/01-design/DECISIONS.md` D1–D21) and the
> earlier `work/05` hosted-Postgres note as **unlocked** — they are not constraints here. The
> architecture below is analysed on its own merits, fresh. Where the schema or current pipeline is
> cited, that is ground-truth from the code, not a "locked decision."

## The idea (restated with the review's framing)

A **central server + one PostgreSQL** ingests new legal data on a schedule — download → parse →
chunk → embed → build indexes — **once**, and distributes the corpus to clients as downloadable
**packages**. The shape, after review:

- **Distribution = periodic per-corpus incremental packages (DECIDED).** The server builds packages
  on a regular schedule; a package is a **compressed artifact bundling an incremental diff of data +
  schema + a minimum compatible client version**, scoped to **one corpus** (`core`, `inpi`, …). Each
  package carries the changes since the previous one, so packages form an **ordered chain** the client
  applies in sequence from its current position. A client knows which packages it can apply from its
  **subscriptions** (which corpora) and its **own version** (≥ the package minimum).
- **Full baselines ship on physical media (DECIDED).** The first corpus, and any later **re-baseline**
  forced by a breaking change (re-embed, builder bump, schema migration), are delivered on **USB key /
  SSD**; the network only ever carries the incremental follow-on packages.
- **A re-baseline must preserve the client's writable tables (DECIDED).** Applying a fresh full media
  baseline replaces only the **server-managed** tables; the client-owned writable tables (the
  application-layer extension point — projects, AI agents, …) are **kept intact**. So a re-baseline
  is a *scoped reload of the server set*, not a whole-database restore.
- Each client machine runs **a local service plus the CLI**. The service owns package
  selection/download/apply and the local database; the CLI queries the local database.
- **The client is read-only for server-managed data.** Clients consume the packaged corpus and never
  write to the server-managed tables. The guarantee is *scoped to that set*: the client deliberately
  **does** own a separate writable space — not for annotations but as an **extension point for a
  future application layer** (project management, configurable AI agents, …), out of scope here but
  shaping the design.
- **Enrichment is packaged, computed upfront (DECIDED).** The quota-limited enrichment tables
  `decision_zones` and `official_api_responses` are enriched centrally and shipped read-only in
  packages, not fetched lazily per client — so clients never call the upstream PISTE/Judilibre APIs.
- Packages carry **both data and schema** — the corpus content and the table/index structure that
  makes it queryable.
- A **superseded article is not a deletion.** The corpus must answer "what was *en vigueur* on date
  D," so old versions are retained with a closed validity window, not removed.
- **Distribution is tiered:** open corpora package to everyone; restricted/licensed corpora
  (e.g. INPI/RNE) are subscription-gated.
- **Updates are version-gated:** a package an out-of-date client can't satisfy (its version < the
  package minimum) is simply not applied until the client binary is upgraded.

This is a deliberate split of concerns: a **central producer** of packaged corpora, and many
**read-only consumers** that stay fast and offline-capable.

## Why this is the right shape for the problem

The motivation is strong and mostly about removing duplicated, expensive work:

- **Embedding is the dominant cost and should be paid once.** Throughput figures here are
  **memory-derived, not a code claim** (memory `embedding-via-openrouter`, directional only): ~195
  vec/s via OpenRouter `bge-m3` vs ~3.4 vec/s on the local APU (~57×). The corpus is large — migration
  comments cite ~1.1M decisions and ~12.9M graph edges, and
  `chunk_embeddings` / `zone_unit_embeddings` are `vector(1024)` per unit. Having every client
  re-embed the same corpus (in API spend, an embedding key, rate limits, and wall-clock) is the
  central waste; computing vectors once and replicating them removes it entirely.
- **Source access is increasingly gated.** The expansion roadmap (`work/07-datasets`) adds BOFiP,
  SIRENE, EUR-Lex, RNE/INPI, ACPR, TED, DG-COMP, etc. Several need credentials (INPI SFTP, ACPR
  registration) or are large manifest-driven bulk dumps. A central operator fetching once is the
  only realistic path for the gated sources — and it maps directly onto the **tiered distribution**
  model (open vs subscription).
- **Quota-limited enrichment is paid once (DECIDED: enrich upfront, replicate down).**
  `official_api_responses` (migration v16) and `decision_zones` (v12) come from PISTE/Judilibre under
  API quota. The chosen model is for the **operator to enrich the corpus upfront, centrally**, and
  **replicate the result** — rather than each client lazily fetching on a cache miss. The quota is
  spent once for the whole fleet, and clients never touch the upstream APIs at all; they receive the
  enriched corpus read-only. Two consequences to keep in view: the server now enriches *proactively*
  (today `decision_zones` is a lazy on-demand cache, so this is a server-side ingestion-scope
  change), bounded by provider coverage (Judilibre covers Cour de cassation `cass`+`inca` only — the
  rest stays `zone_accurate=false`); and replicating raw API bodies has a licensing/bandwidth cost
  (see below).
- **Determinism / freshness.** One dated pipeline run is reproducible; clients converge on an
  identical corpus instead of drifting by when each last ran ingest.

## The base corpus is append-only and temporal — but derived tables rebuild

This is the key consequence of "supersession is not deletion." The store
(`crates/jurisearch-storage/src/migrations.rs`, schema version 17) is ordinary PostgreSQL with
`pgvector` + `pg_search`/ParadeDB, and the legal model is **temporal**: `documents.valid_from` /
`valid_to` (+ `valid_to_raw`), as-of semantics, and validity sentinels normalised to `valid_to =
null`. A new version of an article is a **new row** (its `document_id` embeds the version date —
`legi:<source_uid>@<valid_from>`, `crates/jurisearch-ingest/src/legi/canonical.rs:52-56`) **plus a
validity-window update on the prior row** — an insert and an update, never a destructive delete.

For the **base legal corpus** this makes the change stream **additive plus in-place updates** (no
deletes for supersession): superseded version rows are retained with a closed validity window rather
than deleted, so there is no "deletion feed" to build for them. But "additive" is not the whole story
— the projection upserts by `document_id` and, on conflict, **updates** existing rows' `valid_to`,
`source_payload_hash`, `canonical_json`, and `updated_at` (`crates/jurisearch-storage/src/projection/legi.rs:45-68`).
So an incremental package must carry **base-row update events** (a closing validity window on a prior
version, a source correction) and not only inserts of new version rows. Genuine base-document removals
(pseudonymisation/redaction of a decision, mis-ingest correction) are rare and low-volume, and any
mechanism handles them as ordinary change events.

The qualification matters, and it is the part to get right: this holds for **base documents/articles
only**, not for the **derived retrieval/index-support tables** the role list below also replicates.
Those have routine delete-and-rebuild paths — `zone_units` are replaced by deleting every unit for a
decision and reinserting the current derivation
(`crates/jurisearch-storage/src/zone_units.rs:120-145`); a `decision_zones` refresh invalidates and
**deletes** already-materialised `zone_units` for rows that are no longer derivable
(`crates/jurisearch-storage/src/decision_zones.rs:195-204`); and hierarchy backfill can delete
`chunk_embeddings` while clearing chunk fingerprints
(`crates/jurisearch-storage/src/projection/hierarchy_backfill.rs:209-229`). These are normal
maintenance, not rare redactions. Because packages are **incremental diffs** (decided), each package
**must** carry explicit **delete/replace events** for `zone_units` / `zone_unit_embeddings` /
`chunk_embeddings` (and possibly `decision_zones`) alongside the additive changes — a hash/`updated_at`
upsert alone would leave orphaned derived rows on clients. This is a hard requirement of the package
format, not an option (see *Distribution mechanism*).

What would replicate, by role:

- **Authoritative corpus (replicate):** `documents`, `chunks`, `chunk_embeddings`, `graph_edges`,
  `legi_metadata_roots`, `zone_units`, `zone_unit_embeddings`, `decision_legislation_citations`,
  `legislation_citation_resolutions`.
- **Enrichment/provenance (replicate — DECIDED):** `official_api_responses` (append-only evidence,
  large) and `decision_zones` (overlay/cache) are **replicated, computed upfront server-side** — not
  left as a client-local lazy cache. This is a recorded decision (see the *Why* section above): the
  operator enriches the corpus centrally and ships the result read-only, so clients never call the
  quota-limited PISTE/Judilibre APIs. The cost is bandwidth — the raw `official_api_responses` archive
  is large (see *Remaining risks*) — plus a redistribution-licensing question deferred to future work.
- **Operational, do NOT replicate:** `ingest_run`, `ingest_member`, `ingest_error` — server
  accounting only.
- **Control:** `index_manifest`, `schema_migrations`.

Change-tracking primitives the schema already provides (useful to *any* mechanism): **deterministic
row PKs** — decisions use `<source>:<source_uid>` (`crates/jurisearch-ingest/src/juri/types.rs:180-184`),
LEGI article *versions* use `legi:<source_uid>@<valid_from>` (`legi/canonical.rs:52-56`), and a logical
article *family* across versions is `source_uid` / `version_group` — plus `source_payload_hash` on
documents and chunks, `embedding_fingerprint` / `chunk_builder_version` / `zone_unit_builder_version` /
`model`, and the `CURRENT_SCHEMA_VERSION` + `SchemaVersionAhead` guard in `run_migrations`.

One important gap: there is **no uniform `updated_at` watermark** across the replicated tables. Only
`documents` carries `updated_at`; `chunks`, `chunk_embeddings`, `zone_units`, and
`zone_unit_embeddings` have `created_at` only, and the LEGI chunk upsert updates body/provenance/
fingerprint fields with no `updated_at` to stamp (`crates/jurisearch-storage/src/migrations.rs:46-66`,
`:495-539`; `projection/legi.rs:73-92`). So the package producer **cannot** lean on a generic
`updated_at` high-water cursor for all data — it needs an explicit **per-table diff ledger or
snapshot comparison** (especially for chunk and derived-table updates). The PKs, payload hashes, and
fingerprints are still enough to compute that diff; the watermark just isn't free.

One naming clarification to avoid a misread: a `jurisearch sync` command already exists
(`crates/jurisearch-cli/src/ingest.rs:24-89`; `ArchiveSyncFilter`, `ingest.rs:330-358`), but it is
**local official-source archive delta ingestion** — pulling official-source delta archives (LEGI and
jurisprudence) into the local store, advertised as a STUB (`crates/jurisearch-cli/src/args.rs:135-138`). It is *not* the
server→client package distribution discussed here. The package build pipeline, entitlement, schema
distribution, package endpoints, and version gating are all **unimplemented today**.

## Distribution mechanism — DECIDED: periodic per-corpus packages

The mechanism is now chosen, and it is **application-level packages**, not PostgreSQL's own
replication. The server builds **packages on a regular schedule**; each package is a **compressed
artifact bundling data + schema + a minimum compatible client version**, scoped to **one corpus**
(`core`, `inpi`, …). A client knows which packages it may apply from two facts it already has: the
**corpuses it is subscribed to** and its **own version** (it can apply a package only if its version
≥ the package's minimum). The **first/bootstrap** load is the full corpus shipped on **physical media
(USB key / SSD)**; the network channel then carries only the regular follow-on packages.

This is candidate **C** in the trade-space, chosen over the Postgres-native options. Why it fits:

- **Tiering is intrinsic.** One package per corpus + subscription-based selection *is* the
  open-vs-licensed split — no separate clusters, no per-subscriber replication slots.
- **The version gate lives in the artifact.** The package's embedded minimum client version is
  exactly the "block updates until the client upgrades" rule (review point 7): an out-of-date client
  cannot even select a package that needs a newer binary.
- **Schema travels with the data.** "Replicate data + schema" is met by the package carrying its own
  schema, so it does not depend on DDL replication (the gap that makes logical replication awkward).
- **Offline / occasionally-connected by design.** Discrete downloadable artifacts suit a daily (or
  slower) cadence and a fleet that is not continuously connected — unlike a live standby or slot.
- **Coexists with a writable client layer.** A package loads into an ordinary local Postgres whose
  server-managed tables are kept read-only **by policy**, while the client's own writable tables (the
  planned application layer — see below) live in the *same* database. A whole-cluster physical standby
  could not host both.
- **Bootstrap bandwidth is removed.** Shipping the multi-GB initial set on physical media sidesteps
  the single biggest transfer; only incremental packages cross the network.

For the record, why **not** the native-replication candidates:

- **A — physical/block-level standby** gives engine-enforced read-only and ships prebuilt indexes,
  but it is **whole-cluster** (cannot also hold the writable client app layer, and cannot tier per
  corpus without separate clusters), demands the **same major version + CPU architecture** across the
  fleet, wants a live connection, and hinges on the unverified question of whether `pgvector` IVFFlat
  and especially the `pg_search`/ParadeDB BM25 custom index are block-replication safe
  (`CREATE EXTENSION` at `crates/jurisearch-storage/src/migrations.rs:24-25`, `USING bm25` at
  `migrations.rs:355-369`). The package model avoids every one of these.
- **B — logical, per-publication replication** tiers cleanly and is version-flexible, but it does
  **not** ship DDL (schema would go out-of-band), keeps a **retained slot per subscriber** (a
  fleet-scale burden), and assumes a live connection ill-suited to offline/periodic distribution.

What the package model still has to solve (the cost of choosing C):

- **It is net-new infrastructure** — package build/versioning, a **manifest** the client reads to
  decide which packages it is entitled to and version-compatible with, integrity/signing, hosting,
  and the client-side apply path. The schema's existing primitives help: deterministic row PKs
  (`<source>:<source_uid>` for decisions, `legi:<source_uid>@<valid_from>` for article versions) and
  `source_payload_hash` for idempotent apply, and `CURRENT_SCHEMA_VERSION` / `SchemaVersionAhead` as
  the seed of the version gate.
- **Index materialisation — client-build is the default; ship-prebuilt is a high-risk variant.** The
  **default analysed path** is that a package carries **rows + schema (DDL, incl. index definitions)**
  and the client builds the indexes after apply (IVFFlat finalize at corpus-sized `lists` + the
  `pg_search` BM25 build); `pg_search` is on every client (DECIDED), so the BM25 path is always
  available. The option to **ship prebuilt index data** is retained per decision, but it is **not a
  clean peer** of client-build: PostgreSQL has no portable *logical* artifact for a populated custom
  index (IVFFlat, ParadeDB BM25) independent of the table data — shipping one means copying relation
  files, which reintroduces exactly the **same engine/major-version/extension-binary/CPU-architecture
  compatibility and trust constraints** that ruled out the physical-standby option (A). So if prebuilt
  indexes are pursued, it is as a constrained physical-format variant, not something the generic
  package format gives for free. Which path is **decided at design time**.
- **Incremental packages form an ordered chain (DECIDED).** Each regular package is a **diff since
  the previous one**, so two things follow. (1) Packages are **applied in sequence** from the client's
  current position — the client tracks a per-corpus cursor/generation, gaps can't be skipped, and a
  client that has been offline catches up by applying the missed packages in order. (2) A diff carries
  three event kinds, not just inserts: **inserts** of new rows (new article versions, new decisions),
  **in-place updates** of existing base rows (a closing `valid_to`, a source correction — the
  projection upserts and updates on conflict, `projection/legi.rs:45-68`), and **delete/replace
  events** for the rebuilding derived tables (`zone_units` / `zone_unit_embeddings` /
  `chunk_embeddings`, per the caveat above). A purely additive upsert would both miss validity-window
  closes and orphan derived rows — so all three are required fields of the format, not a choice.
- **Re-baselining on a breaking change (DECIDED).** A change that invalidates the whole corpus — a
  re-embed (new fingerprint), a builder-version bump, or a **breaking/corpus-rewriting schema
  migration** — cannot be expressed as a diff, so it is handled by **redistributing a new full
  baseline on physical media** plus a raised minimum client version (the gate). (An *additive* schema
  migration is not breaking: packages carry their own schema, so additive DDL rides in a normal
  package, gated by the client version, with no re-baseline.) The constraint that shapes the apply path: the re-baseline
  **must preserve the client's writable tables**, so the media is loaded as a **scoped replacement of
  the server-managed tables only** — not a whole-cluster/whole-database restore. This is supported by
  the decision that **server-managed data lives in its own schema/namespace** (DECIDED), distinct from
  the client's writable tables, so the server set can be dropped and reloaded without touching it.
- **Apply atomicity under load.** Applying a package while the CLI queries the same DB, plus any local
  index rebuild, needs a window/transaction discipline that does not stall reads.

## The "service + CLI on the client" split, and the writable extension point

Putting a **service** on the client (alongside the CLI) is the right call: a one-shot binary cannot
poll the package manifest, decide which packages it is entitled to and version-compatible with,
download and verify them, and apply them transactionally. The service owns package
selection/download/apply and the local database; the CLI just queries it. The existing
`crates/jurisearch-cli/src/serve.rs` (a single-client, advisory-locked, loopback socket daemon)
shows the codebase can already host a long-running local process — a starting point for the service's
shape, though it currently serves queries, not package management.

**Server-managed tables are read-only on the client; the client owns a separate writable space.**
The decision to enrich upfront and replicate removes the last query-time writes to *server-managed*
tables (`decision_zones` / `official_api_responses` are now received as read-only replicated data, so
a client `fetch --part` is served from the local store with **no online call**). But "read-only"
is *scoped to server data* deliberately, because the writable space is a **planned extension point**,
not a leftover: it is intended to carry a larger application layer — **project management,
configurable AI agents, and similar** — that is **out of scope for this analysis** but shapes the
architecture. Two consequences:

- **The local DB holds both read-only server tables and writable client tables.** The client
  application layer writes its *own* tables (never the server-managed ones). Read-only on the server
  set is enforced by **policy/permissions**, not by making the whole database read-only.
- **The two sets live in distinct schemas/namespaces (DECIDED).** Server-managed data sits in its own
  schema/namespace (a change from today's unqualified-table migrations), separate from the client's
  writable tables, so a breaking-change re-baseline can drop and reload the server set from media while
  **keeping the client's writable tables intact**.
- **Cross-references need an identity policy — `document_id` is version-specific, not a logical-article
  id.** A given row's `document_id` is **immutable once created** (deterministic:
  `legi:<source_uid>@<valid_from>` for article versions), which is what "`document_id` never changes"
  (DECIDED) properly means — and because supersession retains the old version row, a writable reference
  to a *specific version* `document_id` keeps resolving across updates and a re-baseline. But
  `document_id` is **not** a stable identifier for "the article" across versions: a new version is a
  new row with a new `document_id`; the logical-article identity is `source_uid` / `version_group`
  (`crates/jurisearch-ingest/src/legi/canonical.rs:52-56`, `legi/tests.rs:18-27`). So the app layer
  must reference a **specific historical version** by `document_id`, but "this article, as-of date D"
  by `source_uid` / `version_group` + a version-selection policy.
- **A writable→server foreign key across the re-baseline is not safe "by construction" — it needs a
  named strategy.** In PostgreSQL a writable table FK into the server schema makes dropping/reloading
  that schema either blocked or a constraint drop/recreate dance (the server set itself already uses
  `ON DELETE CASCADE` internally — `migrations.rs:46-76`, `:450-451`, `:495-511`). The scoped reload
  must therefore pick a cross-schema reference strategy: **soft references validated on apply** (no
  hard FK), **load the new server schema under a new generation and atomically switch** a
  view/`search_path`, or **drop and recreate the client-owned constraints after revalidating** the
  references. This is a design requirement, not a given.
- **This is consistent with the package model and confirms ruling out a whole-cluster standby (A).**
  A physical standby makes the *entire* database read-only and so could neither host the writable
  application layer nor support a server-only scoped re-baseline; the package model (C) loads server
  tables into an ordinary local Postgres that the application layer also uses. So the writable
  extension point is not a future "if" — it is a reason the chosen mechanism is the right one.

## Tiered distribution (open vs subscription)

Under the package model this is clean: tiering *is* the per-corpus packaging. There is one package
stream per corpus (`core`, `inpi`, …); a client downloads a corpus's packages only if its
**subscription** covers that corpus (and its **version** clears the package minimum). Open corpora
are simply packages with no subscription requirement. A single client DB then holds whatever mix of
corpora the client is entitled to — no separate clusters, no per-subscriber slots.

Redistribution licensing (the operator now ships derived data — and, with the decision to package
`official_api_responses`, the raw byte-faithful upstream API bodies — to many clients) is
**deferred to future work** and not analysed here; the subscription tier is the natural place to
gate restricted corpora when that work happens. The only engineering note worth carrying forward is
that the raw `official_api_responses` archive (body text + parsed jsonb + sha256 per exchange) is
**large**, so it is a real bandwidth/storage line item for whichever tier carries it.

## Version-gated packages

The version gate (review point 7) is carried **in each package**: the package declares the minimum
client version that can apply it, and the client service applies a package only if its version
clears that minimum — otherwise it waits until the client binary is upgraded. The relevant question
is **what forces the server to raise a package's minimum** (i.e. what makes a client upgrade
mandatory):

- a **schema migration** the client's binary doesn't yet understand (the `schema_migrations` +
  `SchemaVersionAhead` guard is the existing seed for this — it already *rejects* a DB ahead of the
  binary, which is exactly the "you must upgrade" signal a package minimum encodes). Note an
  *additive* migration raises the minimum but still **rides in a normal package** (packages carry
  schema); only a *breaking/corpus-rewriting* migration forces a media re-baseline (above);
- an **embedding model change** — re-embedding bumps the fingerprint (`bge-m3 / 1024 / cls /
  normalize=true` today) and is a **full re-issue of every vector** (a new full package, not an
  incremental one), so it must raise the minimum;
- a **builder-version bump** (`chunk_builder_version`, `zone_unit_builder_version`) that invalidates
  all derived rows of that kind → full re-issue + raised minimum.

So each package must carry, as first-class fields, its **minimum client version, corpus, schema
version, embedding fingerprint, and builder versions** — and the client manifest is what the service
reads to pick the packages it is both *entitled to* (subscription) and *compatible with* (version).
When a package's conditions are not met, the client policy is **warn and reject** (DECIDED): the
service surfaces the reason (e.g. "client too old for `core` package N") and declines to apply,
rather than partially applying or silently skipping.

## Remaining risks and verification items

1. **Index materialisation** — the default is **client-side build** (IVFFlat finalize at corpus-sized
   `lists` + `pg_search` BM25 build after apply); `pg_search` is on every client (decided), so it is
   not an availability gate. The retained option to **ship prebuilt indexes** is not a free
   alternative: PostgreSQL has no portable logical form for a populated custom index, so it means
   shipping relation files, which reintroduces the engine/major-version/extension-binary/architecture
   constraints that ruled out a physical standby. Treat it as a constrained physical variant, not a
   peer of client-build.
2. **Incremental-chain correctness** — because packages are incremental diffs applied in order, the
   format **must** carry all three event kinds: inserts, **in-place base-row updates** (closing
   `valid_to`, source corrections — `projection/legi.rs:45-68`), and **delete/replace events** for the
   rebuilding derived tables (`zone_units` / `zone_unit_embeddings` / `chunk_embeddings`). A purely
   additive upsert both misses validity-window closes and orphans derived rows. The client must also
   enforce **ordered, gap-free application** (a per-corpus cursor; a missed package is caught up, never
   skipped). This is the key correctness item of the model.
3. **Full baselines via physical media (first load + breaking-change re-baselines)** — the multi-GB
   baseline ships on USB/SSD, which removes the bandwidth problem but adds **logistics, integrity, and
   trust** concerns: the media must be signed/verifiable on apply, and it includes the sizeable
   `official_api_responses` archive (raw bodies + parsed jsonb + sha per exchange). A re-baseline adds
   a harder constraint: it must **scope-replace only the server-managed tables and preserve the
   client's writable tables** — so it cannot be a whole-database restore. The server-data
   schema/namespace separation makes the drop+reload tractable (reload that schema only), and because a
   version row's `document_id` is immutable and supersession retains old rows, a writable reference to
   a *specific version* keeps resolving. The unresolved mechanics — a writable→server FK across the
   schema boundary, and a writable reference that means "the article" rather than a fixed version — are
   a design requirement (see *the writable extension point*), not safe by default.
4. **Package distribution & entitlement enforcement** — serving signed packages to many external
   clients needs authenticated, TLS-protected hosting and credential-based subscription enforcement
   per corpus; the current `serve` daemon is loopback-only and unauthenticated, so this is net-new.
5. **Package integrity / trust root** — clients apply server-built data, schema, *and* prebuilt
   embeddings they cannot cheaply re-derive; packages should be signed and verified on apply, with a
   "reproduce from official source" escape hatch retained if the reproducibility posture matters.
6. **Server-side upfront enrichment (consequence of the replicate decision)** — `decision_zones` is a
   *lazy on-demand* cache today; packaging it means the server must enrich the corpus *proactively*,
   a server-side ingestion-scope expansion bounded by provider coverage (Judilibre `cass`+`inca`
   only; the rest stays `zone_accurate=false`).
7. **Apply atomicity under load** — applying a package (plus any local index rebuild) while the CLI
   queries the same DB needs a window/transaction discipline that does not stall reads.
8. **Server operational burden** — a scheduled central ingestor, package build/versioning, signed
   artifact hosting, a manifest service, backups, monitoring, and per-client entitlement/version
   negotiation is a real operated service the project does not run today.

## Decided, deferred, and what's left

The architecture is now settled. **Decided:** periodic per-corpus packages; packages are
**incremental diffs** in an ordered, gap-free chain; tiering = one package stream per corpus
(subscription-gated); enrichment computed upfront server-side and packaged; full baselines (first
load + breaking-change re-baselines) on **physical media**; a re-baseline **preserves the client's
writable tables**; **server-managed data in its own schema/namespace**; a version row's
**`document_id` is immutable** once created (the logical-article id across versions is
`source_uid`/`version_group`); **`pg_search` on every client**; and client policy on an unmet package
condition is **warn and reject**.

**Deferred (decided to decide later, options kept open):**
- **Index in the package vs built on the client** — at design time. Default is client-build; the
  retained "distribute prebuilt indexes" option is a constrained physical variant (same
  version/extension/arch), not a free peer.
- **Package manifest field contract** — exact fields/format at design time (the known-needed set:
  corpus, sequence number, minimum client version, schema version, embedding fingerprint, builder
  versions, signature).
- **Redistribution licensing** — handled in future work, not analysed here.

**Genuinely still open (design-time):**
1. The **scoped-reload procedure** itself — the concrete drop/reload of the server schema on a media
   re-baseline, and the apply transaction that does it without stalling the CLI or the writable layer.
2. The **writable→server reference strategy** — whether the app layer uses a hard cross-schema FK
   (requiring constraint drop/recreate on re-baseline), validated soft references, or a
   load-new-generation-and-switch; and the **identity convention** for app references (specific
   version by `document_id` vs logical article by `source_uid`/`version_group` + as-of).
3. **Catch-up behaviour** for a long-offline client — applying a long run of missed incremental
   packages in order, and when it is cheaper to issue a fresh media baseline instead.

## Bottom line

The idea is sound and well-suited to the problem, and the shape is now largely decided: **a central
producer that builds signed, per-corpus incremental packages (a data diff + schema + minimum client
version) on a schedule**, applied as an **ordered chain** by a **read-only-for-server-data** client
running a **service + CLI**, with **upfront-enriched** quota tables shipped in the packages, a
**version gate carried in each package**, and **physical-media (USB/SSD) full baselines** for the
first load and for breaking-change re-baselines — the latter scope-replacing only the server tables
so the client's writable application-layer data survives. The **base** corpus being **temporal**
(supersession ≠ deletion) makes the data friendly to diff, but "additive" is not the whole picture: a
package carries inserts, **in-place base-row updates** (closing validity windows, corrections), and
**delete/replace events** for the derived retrieval/index tables (`zone_units`, `zone_unit_embeddings`,
`chunk_embeddings`) that rebuild — applied in order. The schema already provides the primitives this
needs (deterministic version-specific row PKs with `source_uid`/`version_group` as the logical-article
id, payload hashes, timestamps, fingerprints, builder/schema versions, the `SchemaVersionAhead` gate
seed), and centralising the embedding + gated/quota fetches removes the single largest source of
duplicated client cost.

The genuine work is not in the SQL. It is in the **package pipeline** (diff build, sequencing,
signing, manifest, hosting, and the client-side ordered download/apply), in the **incremental-chain
correctness** (carrying derived-table delete/replace events, gap-free ordered apply, catch-up, and
physical-media re-baselining), in the **scoped server-schema reload** that preserves the writable
application layer (including the writable→server reference/identity strategy), and in the **operated
server itself** (scheduled ingestor, package build/host, manifest, monitoring, backups). The data
plane is friendly — the repo already provides a temporal base corpus, immutable version-specific row
PKs, `pg_search` on every client, and useful change-tracking fields; the **decided target
architecture adds** a distinct server-managed schema/namespace (today's migrations create unqualified
tables). So **the effort is moderate on the data and substantial on the package/service
infrastructure**, not on the database. Redistribution licensing is real but **deferred to future
work**. The remaining items are design details *within* this model, not a choice of architecture.

## Appendix — design directions (from a Codex consultation; beyond the analysis scope)

These are **forward design recommendations**, not part of the analysis itself — produced by a Codex
review of this document against the repository, addressing the open/deferred items above. None changes
a decision; they are starting points for the design phase. Full record:
`qa/20260626-094352-advice-request-remaining-open-design-que.md`.

1. **Scoped re-baseline → generationed schema + view switch.** Keep a stable client-facing namespace
   (`jurisearch_server`) as views over a physical generation schema (`…_gNNNN`). Load and build all
   indexes in a *new* generation off the live read path, then flip the views in one short
   advisory-locked transaction (cursor check + low `lock_timeout`); retain the old generation for
   rollback and drop it asynchronously. In-place `DROP SCHEMA … CASCADE` is disaster-recovery only.
2. **Writable→server references → soft + validated, not hard FKs.** Store references as columns + a
   validation state the service re-checks after each apply. Pin **exact evidence** (citations, quotes,
   audit) by `document_id`; track **"the article as-of D"** by `source_uid`/`version_group` +
   `as_of_date`; anchor app objects at document/article level (offsets / quote-hash), not chunk/zone IDs.
3. **Diff generation → an ingest-side change-log / outbox** written transactionally at the projection
   boundaries (which already know upsert vs replace-set), recording changed scope/PK + hashes, with the
   builder materialising payloads at build time. `updated_at` is non-uniform (only `documents` has it);
   snapshot/hash is a QA backstop; logical decoding is rejected (it re-couples to WAL/slots).
4. **Derived rebuilds → per-document `replace_set`** ops (delete the document's set, insert the new
   set, verify a per-scope digest), stamped with builder versions + embedding fingerprint — mirroring
   the live `replace_zone_units_for_document` writer, idempotent under ordered apply. No per-row delete
   streams; no generation-wide truncation in ordinary packages.
5. **Catch-up → decide by cumulative byte size, not package count.** Apply incrementally while the
   chain is retained, all packages are compatible, and the cumulative diff is below ~⅓ of baseline
   (within an apply-time budget); otherwise ship a fresh media baseline. The server retains a window
   (e.g. ~90 days / ~120 packages) and publishes `min_available_sequence` + `catchup_ranges`.
6. **Manifest → split signed remote (per-corpus) and embedded (per-package) manifests.** The embedded
   manifest is self-sufficient: identity/ordering (`from`/`to_sequence`, `previous_package_id`),
   compatibility gates, entitlement, per-file digests + signature, an apply contract
   (pre/postconditions, index-build contract, idempotency key), and machine-readable reject codes
   (`client_too_old`, `sequence_gap`, `wrong_generation`, `missing_entitlement`, …). A
   `jurisearch_control.corpus_state` cursor — kept *outside* the swappable generation — enforces
   `sequence == from_sequence − 1` and advances only after all checks pass; entitlement is an apply
   precondition, not just URL hiding.
