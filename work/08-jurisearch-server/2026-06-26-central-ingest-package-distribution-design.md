# Central ingest + packaged read-only client distribution — design

Date: 2026-06-26
Status: Design (no implementation plan, no phasing, no code)
Supersedes nothing; builds on: `work/08-jurisearch-server/2026-06-25-central-ingest-delta-sync-analysis.md`
Design directions incorporated from: `qa/20260626-094352-advice-request-remaining-open-design-que.md`

> This document is **design only**. It specifies *what the system is* — components, data model,
> package and manifest formats, and the protocols that bind them — at a level a subsequent
> implementation plan can build against. It deliberately does **not** sequence the work, estimate
> effort, assign phases, or write code. Where it states a value (a schema name, a field, a digest
> step) it is fixing a design contract, not prescribing an implementation.
>
> All product decisions are already settled in the analysis. This design takes them as fixed and
> resolves the design-time questions the analysis left open, following the Codex consultation
> directions. The analysis is the *why*; this is the *what*.

---

## 1. Purpose and scope

### 1.1 Purpose

Design a system in which a **central producer** ingests French legal data once (download → parse →
chunk → embed → index → enrich) and distributes the corpus to many **read-only clients** as signed,
per-corpus, ordered **incremental packages**, with **physical-media full baselines** for the first
load and for breaking-change re-baselines. Each client runs a **local service + CLI** over an
ordinary local PostgreSQL that also hosts a **writable application-layer namespace** preserved across
re-baselines.

### 1.2 In scope

- The server-side data model change (server-managed data moves into its own namespace).
- The **change-log / outbox** that makes incremental diffs computable without a uniform `updated_at`.
- The **package format**: kinds, event kinds, payload layout, apply order, embedded manifest.
- The **per-corpus remote manifest** and the signing / integrity / entitlement model.
- The client **local service** responsibilities, the local **schema layout** (server generations +
  stable views + control namespace + writable app namespace), and the **apply protocols**
  (incremental, baseline, re-baseline).
- The **identity & reference model** for the writable layer to reference server data safely.
- **Version gating**, **catch-up policy**, **integrity verification**, **failure semantics**.

### 1.3 Out of scope (explicitly deferred — not designed here)

- **Implementation**: build order, task breakdown, milestones, code.
- **Redistribution licensing** of derived data and raw upstream API bodies (deferred to future work
  by the analysis; the subscription tier is the eventual enforcement point).
- **The writable application layer's internal design** (project management, configurable AI agents).
  This design fixes only the *boundary* that layer must respect.
- **Server hosting topology / ops runbooks** (CDN choice, backup schedules, key-rotation cadence).
  The design fixes the *contracts* (signed artifacts, TLS, authenticated entitlement); concrete
  infrastructure selection is an operations decision.

### 1.4 Relationship to the analysis decisions

This design honours, without re-litigating, every **DECIDED** item in the analysis §"Decided,
deferred, and what's left": per-corpus ordered incremental packages; incremental diffs in a gap-free
chain; tiering = one package stream per corpus (subscription-gated); enrichment computed upfront and
packaged; physical-media full baselines (first load + breaking-change re-baselines); re-baseline
preserves the client's writable tables; server-managed data in its own schema/namespace; a version
row's `document_id` is immutable (logical-article identity is `source_uid`/`version_group`);
`pg_search` on every client; warn-and-reject on unmet package conditions.

It resolves the three **genuinely open (design-time)** items into concrete contracts: the
**scoped-reload procedure** (§7), the **writable→server reference strategy and identity convention**
(§8), and **catch-up behaviour** (§9.4). It also fixes the two items the analysis marked "decided at
design time": **index-in-package vs client-build** (§9.3 — client-build is fixed as the default; ship-
prebuilt is rejected for the logical path and retained only as a constrained physical variant) and the
**package manifest field contract** (§6).

---

## 2. Constraints from the current code

The design is shaped by ground-truth facts about the existing store
(`crates/jurisearch-storage`, schema version 17) and CLI. These are constraints, not decisions.

| # | Fact | Source | Design consequence |
|---|------|--------|--------------------|
| C1 | Migrations create **unqualified** tables (public schema). | `migrations.rs:23–83` | The server-namespace split (§4) is a **target change**, not something today's runner enforces. |
| C2 | `CURRENT_SCHEMA_VERSION = 17`; `run_migrations` **rejects a DB whose `schema_migrations` max exceeds the binary** (`SchemaVersionAhead`). | `migrations.rs:3`, `:704–754` | The "you must upgrade" signal already exists; it seeds the **version gate** (§10). |
| C3 | LEGI projection is **upsert-oriented**, not append-only: document rows update `valid_to`, `source_payload_hash`, `canonical_json`, hierarchy, `updated_at`; chunks and graph edges update on conflict. | `projection/legi.rs:45–105` | Diffs must carry **in-place base-row updates**, not only inserts. |
| C4 | Deterministic row PKs: LEGI versions are `legi:<source_uid>@<valid_from>`; decisions are `<source>:<source_uid>`. | `legi/canonical.rs:52–56`, `juri/types.rs:180–184` | These are the **package primitive** for idempotent apply and the identity model (§8). |
| C5 | Derived sets are **rebuildable**: `replace_zone_units_for_document` deletes all units for one decision and reinserts (cascading embeddings); decision-zone refresh deletes no-longer-derivable zone units; hierarchy backfill deletes `chunk_embeddings` for a document. | `zone_units.rs:120–169`, `decision_zones.rs:195–207`, `hierarchy_backfill.rs:216–229` | Diffs must carry **`replace_set`** events for derived tables (§5.3), not additive upserts. |
| C6 | IVFFlat indexes are **finalize-time products**: dense finalize verifies full coverage, drops/recreates the IVFFlat index, writes `index_manifest`; zone dense finalize mirrors it. BM25 is a `pg_search` index defined in migrations. | `dense.rs:93–190`, `zone_units.rs:431–524`, `migrations.rs:103–105`, `:355–369`, `:559–573` | There is **no portable logical artifact** for a populated custom index → **client builds indexes** after apply (§9.3). |
| C7 | `updated_at` is **not uniform** across the replicated set: some base/metadata tables carry and stamp it (`documents` `migrations.rs:43`; `legi_metadata_roots` `:207`, stamped in `projection/metadata.rs:43–56`; `legislation_citation_resolutions` `:689`, stamped in `legislation_citations.rs:142–146`, `:206–209`), but `chunks`, `chunk_embeddings`, `graph_edges`, `zone_units`, `zone_unit_embeddings`, `decision_zones`, `official_api_responses` have **no** update watermark, and the LEGI chunk/graph upserts don't stamp one. | `migrations.rs:43`, `:207`, `:689`, `:495–539`; `projection/legi.rs:73–105` | No generic `updated_at` high-water cursor can drive package diffs across the full set → a **change-log/outbox** is required (§5.1). |
| C8 | FK cascade chains: `chunks → documents`, `chunk_embeddings → chunks`, `zone_unit_embeddings → zone_units`, `graph_edges → documents`, all `ON DELETE CASCADE`. | `migrations.rs:46–77`, `:532–539` | A document-scoped delete naturally cascades its derived rows → enables clean `replace_set` (§5.3). |
| C9 | `serve` daemon is **single-client, sequential, advisory-locked**, JSONL over a loopback socket, reusing the query dispatcher. | `serve.rs:1–4`, `:72–123` | A starting point for the client service's **shape**, but it serves queries, not package management, and must not depend on users voluntarily closing CLI sessions. |

---

## 3. System overview

```
┌────────────────────────── CENTRAL PRODUCER (one operator, one PostgreSQL) ──────────────────────────┐
│                                                                                                      │
│   Scheduled ingestor ──► server tables ──► [package_change_log outbox] ──► Package Builder           │
│   (download/parse/                              (§5.1)                       │  diff materialize      │
│    chunk/embed/                                                              │  replace_set (§5.3)    │
│    enrich upfront)                                                           │  baseline / rebaseline │
│                                                                             ▼                         │
│                                                       Signed artifacts + per-corpus remote manifest   │
│                                                       (§6)  ──► artifact hosting (TLS, authenticated)  │
└──────────────────────────────────────────────────────────┬───────────────────────────────────────────┘
                                                            │  network: incremental packages only
                                  physical media (USB/SSD)  │  (baselines + re-baselines)
                                  ───────────────────────►  ▼
┌────────────────────────────────────── CLIENT MACHINE (many, read-only) ───────────────────────────────┐
│                                                                                                        │
│   Local Service (§7)                          Local PostgreSQL                                          │
│   ├─ manifest poll + entitlement      ┌──────────────────────────────────────────────────────────┐    │
│   ├─ download + verify (§11)          │ jurisearch_server  (per-corpus views) ──► jurisearch_server │    │
│   ├─ apply (incremental / baseline)   │                                  _<corpus>_gNNNN (physical) │    │
│   ├─ generation manager (§7.2)        │ jurisearch_control    (corpus_state cursor — never swapped)│    │
│   └─ index builder (§9.3)             │ jurisearch_app        (writable extension point — soft refs)│   │
│                                       └──────────────────────────────────────────────────────────┘    │
│   CLI (read path) ──────────────────────────► queries jurisearch_server views                          │
└────────────────────────────────────────────────────────────────────────────────────────────────────┘
```

Two roles, one data plane:

- **Producer** — a scheduled ingestor mutating server tables, an **outbox** capturing the semantic
  change set, a **package builder** materialising signed artifacts, and a **manifest + hosting**
  surface.
- **Consumer** — a **local service** that polls, verifies, and applies packages into a generationed
  server namespace fronted by stable views, alongside a **control** namespace (the apply cursor) and a
  **writable app** namespace. The **CLI** only reads the server views.

---

## 4. Server-managed data model and namespacing

### 4.1 Namespaces (target layout)

Today all tables are unqualified (C1). The design introduces three logical namespaces on the **client**
database, and a corresponding move on the **server**:

| Namespace | Owner | Writable? | Swapped on re-baseline? | Contents |
|-----------|-------|-----------|-------------------------|----------|
| `jurisearch_server` | server (replicated) | read-only (by policy) | the **affected corpus's views** are repointed; that corpus's physical schema is swapped | stable client-facing **views/functions**, each selecting **its corpus's active generation** |
| `jurisearch_server_<corpus>_gNNNN` | server (replicated) | read-only | **per corpus** — this is the swap unit (one generation chain *per corpus*) | physical **per-corpus** generation: that corpus's replicated tables + indexes |
| `jurisearch_control` | client service | service-writable | **no** — lives outside every generation | `corpus_state` cursor, package log, generation registry |
| `jurisearch_app` | client app layer | app-writable | **no** — preserved across re-baseline | the future application layer's own tables |

**Generation granularity is per corpus (design decision — resolves the per-corpus/global ambiguity).**
Because a client DB holds whatever **mix** of corpora it is entitled to (§11.3) and each corpus has its
own package chain and its own `corpus_state` cursor (§7.2), each corpus owns an **independent physical
generation chain** `jurisearch_server_<corpus>_gNNNN`. A re-baseline of `core` loads
`jurisearch_server_core_g<new>` and repoints **only the `core` views**, leaving `inpi` (and every other
installed corpus) and their generations **untouched and visible**. There is **no** single whole-server
generation that must contain a merged copy of every corpus — that would force every per-corpus baseline
to re-merge all other corpora and risk dropping them on a switch. The stable `jurisearch_server`
namespace presents one client-facing view per logical relation; for a relation that spans corpora the
view is a `UNION ALL` over each corpus's active generation (or a per-corpus view set), so adding/
removing/rebaselining one corpus only edits that corpus's contribution.

Rationale (from the analysis and consultation §1): a stable client-facing namespace decouples query
SQL from the physical generation, so a re-baseline is a **view repoint** of the affected corpus, not a
destructive `DROP SCHEMA`. The control cursor must live **outside** every swappable generation so it
survives a generation switch and authoritatively records, **per corpus**, "where this client is."

### 4.2 Replicated table set (by role)

Fixed from the analysis. Membership in a package is determined by role, not by table-by-table opt-in.

- **Authoritative corpus (replicate):** `documents`, `chunks`, `chunk_embeddings`, `graph_edges`,
  `legi_metadata_roots`, `zone_units`, `zone_unit_embeddings`, `decision_legislation_citations`,
  `legislation_citation_resolutions`.
- **Enrichment/provenance (replicate — computed upfront):** `official_api_responses` (append-only
  evidence, large), `decision_zones` (overlay). Shipped read-only; clients never call PISTE/Judilibre.
- **Operational (do NOT replicate):** `ingest_run`, `ingest_member`, `ingest_error` — server
  accounting only; never enters a package.
- **Control (special):** `index_manifest`, `schema_migrations` — travel with the package's schema and
  are rebuilt/stamped on the client, not blindly copied (see §9.3, §10).

### 4.3 The stable-view indirection

`jurisearch_server` contains **views (or stable SQL functions) over each corpus's active generation**,
not base tables. The client query path (the CLI, and later the app layer's read queries) references
`jurisearch_server.<name>` and is **unaware of the per-corpus generation suffix**. This is the mechanism
that makes a re-baseline atomic from the reader's perspective (§7.2) and lets one corpus be repointed
without disturbing the others.

Design note (consultation §1): the switch is a **view replacement** (`CREATE OR REPLACE VIEW`) for the
rebaselined corpus only, **not**
`ALTER DATABASE SET search_path` (connection state — a running session keeps the old setting) and
**not** `ALTER SCHEMA … RENAME` as the normal path (stronger catalog locks, broader plan/cache
invalidation). If layered views prove too costly for hot retrieval paths, the fallback is **stable SQL
functions** or a minimal set of compatibility views for the public query entry points only — keeping
the same generation-indirection concept. This view-vs-function trade-off for hot paths is the one
**performance question left to implementation measurement**; the indirection contract itself is fixed.

---

## 5. Change capture and the package payload

### 5.1 The change-log / outbox (`package_change_log`)

**Design decision (consultation §3):** the producer derives incremental diffs from a **semantic
change ledger written transactionally at the projection boundaries**, not from a uniform `updated_at`
(C7), not from snapshot/hash as the primary mechanism, and **not** from logical decoding.

The projection paths already know the *semantics* of each mutation — LEGI writes documents/chunks/
graph edges through prepared upserts (`projection/legi.rs:45–105`); embedding inserts go through batch
staging + upsert (`projection/embeddings.rs:53–130`, `zone_units.rs:342–418`); zone units are already
per-document replace operations (`zone_units.rs:120–169`). Each such path emits a ledger row **in the
same transaction** as the mutation.

Ledger shape (design contract — exact column types may be refined at implementation, but these fields
are required):

```sql
CREATE TABLE package_change_log (
  change_seq          bigserial PRIMARY KEY,
  corpus              text NOT NULL,
  ingest_run_id       text NOT NULL,
  table_name          text NOT NULL,
  op                  text NOT NULL CHECK (op IN ('upsert','delete','replace_set')),
  scope_kind          text NOT NULL,   -- e.g. 'document', 'logical_article'
  scope_key           text NOT NULL,   -- e.g. a document_id
  row_pk              jsonb NOT NULL DEFAULT '{}'::jsonb,
  row_hash            text,
  before_hash         text,
  after_hash          text,
  payload             jsonb,           -- optional; builder may rematerialize at build time
  builder_versions    jsonb NOT NULL DEFAULT '{}'::jsonb,
  embedding_fingerprint text,
  schema_version      integer NOT NULL,
  created_at          timestamptz NOT NULL DEFAULT now()
);
```

Design principles for the ledger:

- **It records scopes touched, not necessarily full row bodies.** For large/vector-heavy rows the
  ledger may carry only the changed PK/scope + hashes; the **package builder materialises row payloads
  from the authoritative server tables at build time**. What must be authoritative in the ledger is the
  *list of scopes changed since package N*.
- **It is the primary diff source.** Snapshot/hash comparison is retained only as a **QA backstop**
  (§5.4), never the primary mechanism.
- **It captures `op` semantics** — `upsert` vs `delete` vs `replace_set` — which snapshot diffing and
  logical decoding cannot reconstruct (they cannot say "replace the complete zone-unit set for document
  D under builder vN").

**Two distinct sequence layers (design contract — avoids a cross-corpus `sequence_gap`).** The ledger's
`change_seq bigserial` is a **global build/audit ordering** across all corpora — it is *not* the package
chain sequence. Package ordering is a **separate, per-corpus, gap-free monotonic counter** — call it the
**package sequence** — assigned at package-build time, one chain per corpus. **All** of
`from_sequence`/`to_sequence` (embedded manifest), the remote manifest's `head_sequence` /
`min_available_sequence` / `catchup_ranges`, and `corpus_state.sequence` (§7.2) use **this per-corpus
package sequence**, never `change_seq`. This is required precisely because `change_seq` interleaves
corpora: if `core` package boundaries were read off the global `change_seq`, an `inpi` change landing
between two `core` packages would make the next `core` package's `from_sequence` non-contiguous and trip
a false `sequence_gap` (§7.3). The builder therefore filters the ledger by `corpus`, materialises that
corpus's changed scopes since its last package sequence, and stamps the new package with the next
per-corpus sequence; `change_seq` is retained only to order and audit the build.

### 5.2 Event kinds (required, all three)

A package is a **diff**, so the format carries three event kinds. All three are **required fields of
the format**, not options (analysis §"Distribution mechanism", risk #2):

1. **`upsert`** — inserts of new rows (new article versions `legi:<uid>@<valid_from>`, new decisions
   `<source>:<uid>`) **and** in-place updates of existing base rows (a closing `valid_to`, a source
   correction — C3). Keyed by deterministic PK (C4) and idempotent via `source_payload_hash`.
2. **`delete`** — rare base-document removals (pseudonymisation/redaction, mis-ingest correction).
   Low-volume; **not** the representation for routine derived rebuilds.
3. **`replace_set`** — the scoped rebuild of a derived set (§5.3).

A purely additive upsert stream would both **miss validity-window closes** and **orphan derived rows** —
hence all three are mandatory.

**Identity for non-`document_id`-keyed tables — `official_api_responses` (design contract).** The
deterministic-PK + `source_payload_hash` idempotence above (C4) covers `documents`, `chunks`, decisions,
and version rows. It does **not** cover `official_api_responses`, which uses a **server-assigned
`response_id bigserial` surrogate key** (`migrations.rs:593`; the writer appends and returns the assigned
id — `official_api_archive.rs:37,60,67,92`), and whose id is referenced as a **FK by the v17 citation
tables** (`migrations.rs:650,676`) with citation extraction treating the **highest `response_id` per
decision as the latest archived response** (`legislation_citations.rs:11,27,38`). The contract: the
producer's **`response_id` is the immutable replicated key** — packages carry it verbatim and the client
**inserts it explicitly** (overriding the local `bigserial`), never minting a local id (the client is
read-only for server data, so its sequence is never used for these rows). Apply order makes
`official_api_responses` land **before** the dependent citation tables so the FKs and the
"highest id = latest" ordering are preserved idempotently across re-apply. This exception is reflected in
the manifest payload-layout/apply-order contract (§6.2.2). (A deterministic content key —
provider/endpoint/request-fingerprint/body-digest — is a possible future alternative but is **not**
required by this design; preserving the producer id is the lower-risk rule.)

### 5.3 Derived rebuilds as scoped `replace_set` (not per-row deletes, not truncation)

**Design decision (consultation §4):** derived rebuilds (`zone_units`, `zone_unit_embeddings`, and —
when chunk membership changes — `chunks` together with its cascaded `chunk_embeddings`) are expressed as
**document-scoped `replace_set` operations**, mirroring the live writer `replace_zone_units_for_document`
(C5, C8). This is naturally idempotent under ordered apply: replaying the same operation yields the same
set.

`replace_set` payload contract (per scope):

```json
{
  "op": "replace_set",
  "table_group": "zone_units",
  "scope": { "document_id": "cass:..." },
  "builder_version": "zone-unit-builder-vN",
  "source_text_hash": "...",
  "embedding_fingerprint": "bge-m3:1024:cls:normalize=true",
  "set_digest": "<deterministic hash over ordered (pk,row_hash)>",
  "rows": {
    "zone_units": [ ... ],
    "zone_unit_embeddings": [ ... ]
  }
}
```

Client apply of one `replace_set` (inside the package transaction): delete the scope's `zone_units`
(cascade removes old `zone_unit_embeddings` via C8) → insert the provided `zone_units` → insert
matching `zone_unit_embeddings` → **verify the set** (every embedding has a zone unit; every unit
carries the expected builder version; if dense readiness is required, every unit has an embedding with
the expected fingerprint) → compare `set_digest`.

Scope rules:

- **`zone_units` / `zone_unit_embeddings`** → scope = one **`document_id`**.
- **`chunks` (with cascaded `chunk_embeddings`)** → a **document-scoped `chunks_with_embeddings`
  replacement** whenever the document's **chunk membership, partitioning, or contextualised body
  changes** (a source correction or non-rebaseline chunking change that shrinks/repartitions a document).
  This is **required because `chunks` are themselves replicated, BM25-indexed, fetchable rows**
  (`migrations.rs:46`, `:355`), and the live LEGI projection **upserts current chunks but does not delete
  chunks that dropped out of the current set** (`projection/legi.rs:73`): an embeddings-only replacement
  would leave **stale chunk text visible to BM25/fetch**. Apply: delete all `chunks` for the
  `document_id` (cascading `chunk_embeddings` via C8) → insert the provided current chunk rows → insert/
  verify `chunk_embeddings` if dense readiness is required → compare `set_digest`.
- **`chunk_embeddings` only** → the **narrower** replacement, allowed **only** when the chunk **row set
  is unchanged** and just an embedding payload/fingerprint is corrected. A single-embedding correction
  may even use a plain `upsert` by `chunk_id`. **Any** change to chunk membership/partitioning/body
  **must** use the `chunks_with_embeddings` scope above, not this one, so stale chunk rows cannot survive.
  (A chunking-*builder*-version bump that invalidates the whole corpus remains a rebaseline, §6.1 — not
  an incremental.)
- **Multi-corpus DBs:** if one server namespace hosts multiple corpora, the scope key becomes
  `(corpus, document_id)`.

Prohibited representations: **per-row delete streams** as the primary form (verbose, error-prone, don't
encode the set-replacement invariant — acceptable only for rare base redactions/repairs); and
**generation-wide truncation** in ordinary diffs (destroys locality, forces a large unavailable
window — appropriate **only** for a baseline/re-baseline or a builder/fingerprint full reissue).

Required stamps on every `replace_set`: scope key; stable row PKs (`zone_unit_id`, `chunk_id`); builder
stamps (`chunk_builder_version`, `zone_unit_builder_version`, `zone_schema_version`); embedding stamps
(`embedding_fingerprint`, `model`, `dimension`, `normalize`); provenance (`source_payload_hash`,
`text_hash`); package `from_sequence`/`to_sequence`; and the optional `set_digest`.

### 5.4 Snapshot/hash as QA backstop only

After building package N, the producer computes per-table **row counts** and **ordered hash digests**
for the affected corpus/generation and includes them in the manifest (§6 `postconditions`).
Periodically it compares a **package-applied staging database** against a direct server snapshot. This
is verification, never the primary diff path.

---

## 6. Package and manifest format

### 6.1 Package kinds

| Kind | Channel | Trigger | Apply |
|------|---------|---------|-------|
| `baseline` | physical media | first load of a corpus | load into a fresh per-corpus generation, build indexes, switch that corpus's views (§7.4) |
| `rebaseline` | physical media | breaking change (re-embed / builder bump / corpus-rewriting migration) | same as baseline, **scope-replacing only that corpus's server set** (preserve `jurisearch_app` and other corpora) (§7.4) |
| `incremental` | network | scheduled diff since previous package | ordered, gap-free apply into the corpus's **active** generation (§7.3) |

What forces a **`rebaseline`** (not expressible as a diff): an **embedding model change** (fingerprint
bump → full re-issue of every vector), a **builder-version bump** (invalidates all derived rows of that
kind), or a **breaking/corpus-rewriting schema migration**. An **additive** schema migration is *not*
breaking — packages carry their own schema, so additive DDL rides in a normal `incremental` package,
gated only by the client version.

### 6.2 Two-tier manifest (split, both signed)

**Design decision (consultation §6):** the **per-corpus remote manifest** (the listing the client polls
to plan downloads) is separate from each package's **embedded manifest** (self-sufficient — the client
must never have to trust only the remote listing once it holds an artifact).

#### 6.2.1 Per-corpus remote manifest (signed)

Lists the corpus's chain head, retention window, active baseline, and per-package compatibility/size
metadata so the client can **plan** without downloading. Required structure:

```json
{
  "manifest_version": 1,
  "generated_at": "2026-06-26T00:00:00Z",
  "publisher": "jurisearch",
  "corpus": "core",
  "environment": "production",
  "head_sequence": 1088,
  "min_available_sequence": 970,
  "active_baseline": {
    "baseline_id": "core-2026-06-25-g000124",
    "generation": "core_g000124",
    "sequence": 1040,
    "schema_version": 17,
    "artifact_uri": "...", "compressed_size_bytes": 0, "sha256": "...", "signature": "..."
  },
  "packages": [
    {
      "package_id": "core-1041-1042",
      "from_sequence": 1041, "to_sequence": 1042,
      "artifact_uri": "...",
      "compressed_size_bytes": 0, "uncompressed_size_bytes": 0,
      "estimated_apply_seconds": 0,
      "row_counts": { "documents": 10 },
      "requires_baseline": false,
      "minimum_client_version": "x.y.z",
      "schema_version": 17,
      "embedding_fingerprint": "bge-m3:1024:cls:normalize=true",
      "builder_versions": { "chunk_builder_version": "...", "zone_unit_builder_version": "..." },
      "sha256": "...", "signature": "..."
    }
  ],
  "catchup_ranges": [
    { "from_sequence": 1000, "to_sequence": 1088, "mode": "incremental_ok" },
    { "from_sequence": 800, "mode": "requires_baseline", "baseline_id": "core-2026-06-25-g000124" }
  ],
  "catchup_policy": { "max_incremental_packages": 120, "max_cumulative_diff_to_baseline_ratio": 0.33 },
  "entitlement": { "corpus": "core", "tier": "open|subscription", "license_epoch": 3, "audience": "..." },
  "signing": { "key_id": "...", "algorithm": "..." }
}
```

#### 6.2.2 Per-package embedded manifest (signed, self-sufficient)

Travels inside the artifact. Field groups (all required unless noted):

- **Identity & ordering:** `package_format_version`, `package_id`, `corpus`, `package_kind`
  (`incremental`|`baseline`|`rebaseline`), `from_sequence`, `to_sequence`, `previous_package_id`,
  `previous_package_sha256` (chain link), `baseline_id`, `generation`, `created_at`, builder run ID.
- **Compatibility gates:** `minimum_client_version` (and `maximum_client_version` only if a known-bad
  newer range ever exists), `schema_version` + schema-migration-bundle digest, `requires_extensions`
  (`vector`, `pg_search` + versions if known), `embedding_fingerprint` (+ model/dimension/normalize),
  `builder_versions` (chunk builder, zone-unit builder, zone schema, citation extractor/resolver if
  those outputs are packaged). `postgres_major_min`/`max` are **absent/advisory** for the default
  logical path and present **only** for a physical-format variant (§9.3).
- **Entitlement:** `entitlement_corpus`, `tier`/SKU, `license_epoch`, optional `audience`/tenant scope,
  and an **entitlement-policy digest** so the client can explain "not subscribed to corpus X" rather
  than a generic integrity failure.
- **Integrity & signing:** artifact `sha256`, uncompressed-payload digest, **per-file digests** (one
  per table/change file), manifest canonicalisation algorithm, signature algorithm + signature + key ID
  + key/cert epoch. Optional transparency-log index reserved for future supply-chain audit.
- **Apply contract:** `expected_client_from_sequence`, `result_sequence`, `requires_empty_generation`
  (baseline/rebaseline), `schema_ops_digest`, `operations` summary (counts by table × op kind),
  `replace_scopes` counts + optional scope digests, `preconditions` (current schema version, embedding
  fingerprint, builder versions, active baseline/generation), `postconditions` (expected row counts +
  deterministic table/set digests), `index_build` contract (BM25 indexes to build, IVFFlat to finalize,
  `lists`/`probes` defaults, queryable-before-finalize flag — **default: not advertised active until
  indexes built and manifests written**), `idempotency_key` (= `package_id` + digest), `rollback_policy`
  (transaction rollback for incrementals; keep-previous-generation-until-validated for baselines).
- **Payload layout:** per-file list (table name, op kind, format — `copy-binary`|`jsonl`|`parquet` —
  compression, row count, digest) and the **dependency apply order**: base tables before dependents;
  derived `replace_set` after base; embeddings after chunks/zone units; **`official_api_responses`
  before the citation tables that FK to its `response_id`** (the surrogate-key exception, §5.2); index
  finalize last.

### 6.3 Machine-readable reject codes

Every warn-and-reject outcome carries one explicit code so the service can explain itself:
`client_too_old`, `schema_ahead`, `missing_entitlement`, `sequence_gap`, `wrong_generation`,
`embedding_fingerprint_mismatch`, `builder_version_mismatch`, `signature_invalid`, `digest_mismatch`,
`extension_missing`, `baseline_required`.

---

## 7. Client service and apply protocols

### 7.1 The local service

The client runs a **local service + CLI** (analysis §"service + CLI split"). The service owns package
selection/download/apply, the generation lifecycle, and the control cursor; the CLI only queries the
`jurisearch_server` views. The existing `serve` daemon (C9) shows the codebase can host a long-running
loopback process and is the **shape** to extend — but it is single-client/sequential and serves
queries; the package service must **not** rely on users voluntarily closing CLI sessions, so apply
coordination uses an advisory lock + short critical sections (§7.2), not "ask everyone to disconnect."

Service responsibilities: poll the per-corpus remote manifest → filter by **entitlement** (local
license token) and **version** → plan incremental vs baseline (§9.4) → download → **verify** (§11) →
**apply, including any index materialisation the package requires (§9.3)** → advance the control cursor
→ background **reference validation** (§8). Index materialisation is **part of apply/activation**, not a
step after the cursor advances (§7.3, §9.3): a corpus is never advertised query-ready, and its cursor
never advances, until the indexes the package requires are built. Only **reference validation** (§8) is
genuinely deferred to the background after the cursor advances.

### 7.2 The control cursor and generation registry

A control table outside the swappable generation is the authority on client position
(consultation §6):

```sql
-- jurisearch_control.corpus_state  (one row per installed corpus)
corpus               text PRIMARY KEY,
active_generation    text NOT NULL,     -- e.g. 'core_g000124'  (per-corpus generation)
sequence             bigint NOT NULL,   -- last applied to_sequence
baseline_id          text NOT NULL,
schema_version       integer NOT NULL,
embedding_fingerprint text NOT NULL,
builder_versions     jsonb NOT NULL,
last_package_id      text,
last_package_digest  text,
applied_at           timestamptz NOT NULL
```

The cursor is **keyed by corpus**: each installed corpus independently records its own active
generation, sequence, baseline, and compatibility stamps, so corpora advance and re-baseline
independently. Plus a **generation registry** tracking each `jurisearch_server_<corpus>_gNNNN` and its
state (`building`, `active`, `retired`) for rollback and async cleanup.

### 7.3 Incremental apply protocol (ordered, gap-free, idempotent)

For each `incremental` package, in **one transaction** against the **active** generation:

1. Take the **package-apply advisory lock** (DB-level) with a low `lock_timeout`; fail cleanly rather
   than block behind a long user query.
2. Check **`corpus_state.sequence == package.expected_client_from_sequence`** (equivalently
   `from_sequence − 1`). A mismatch → `sequence_gap` (warn-and-reject; never partial-apply). A retry of
   an already-committed package sees the advanced cursor and is **skipped as already applied**.
3. Verify all preconditions (schema version, fingerprint, builder versions, generation) → mismatch →
   the corresponding reject code.
4. Apply events in **dependency order** (§6.2.2): base `upsert`/`delete`, then derived `replace_set`,
   then embeddings.
5. Verify **postconditions** (row counts + table/set digests).
6. Advance `corpus_state.sequence → to_sequence`, stamp `last_package_id`/digest, **commit**.

**Index work for incrementals.** Ordinary incrementals apply into the **existing** generation, so new/
changed rows are indexed by PostgreSQL's **row-level index maintenance** (pg_search BM25 and pgvector
IVFFlat both maintain on insert/update) **inside the same apply transaction** — no separate finalize is
needed, and the cursor advance at step 6 already reflects an indexed state. A finalize (IVFFlat `lists`
rebuild, BM25 rebuild) is **reserved** for baselines/re-baselines (§7.4) and for the rare incremental
that carries a **new index definition** via additive index DDL; in that case the index build runs as
part of apply **before** step 6, so the cursor never advances on an unbuilt index. The package's
`index_build` contract (§6.2.2) declares which case applies; the default for an ordinary incremental is
"no finalize required."

Gap-free guarantee: a missed package is **caught up by applying the missed packages in order**, never
skipped. A long-offline client applies the retained chain in sequence (or re-baselines — §9.4).

### 7.4 Baseline / re-baseline apply protocol (generation load + view switch)

**Design decision (consultation §1):** a media baseline/re-baseline uses a **staged generation + short
stable-view switch**, **not** in-place `DROP SCHEMA … CASCADE` (which is **disaster-recovery only**).

Phases:

1. **Long phase (off the live read path):** load the media baseline into a **new** physical schema
   `jurisearch_server_<corpus>_g<new>` while queries keep reading this corpus's `jurisearch_server`
   views pointed at its current generation (and all **other** corpora keep serving from their own
   generations, untouched). Build everything inside the new generation: table load → constraints → BM25
   indexes → IVFFlat finalize → `ANALYZE` → `index_manifest` rows → **validation**. This mirrors the
   existing finalize discipline (indexes built only after coverage is complete — `dense.rs:122–160`,
   `zone_units.rs:461–499`). This phase blocks normal reads only for shared extension/catalog work.
2. **Short switch (one transaction):** take the package-apply advisory lock; verify this corpus's cursor
   still equals the expected old baseline/generation; `CREATE OR REPLACE VIEW` **the rebaselined
   corpus's** `jurisearch_server` views to point at its new generation (other corpora's views are not
   touched); update that corpus's `corpus_state` row (active_generation, baseline_id, sequence,
   fingerprint, builder versions); commit. Low `lock_timeout`, fail cleanly.
3. **Cleanup:** retain the previous generation for rollback/diagnostics until a post-switch smoke check
   passes and no old transactions reference it; then drop it asynchronously with a bounded lock timeout
   (if it cannot drop, mark `retired` and retry).

The **writable `jurisearch_app` namespace is never touched** by this procedure — it lives outside the
swap unit. That is precisely what makes the re-baseline a **scoped reload of the server set**, not a
whole-database restore, and what confirms ruling out a whole-cluster physical standby (which would make
the entire DB read-only).

---

## 8. Identity and writable→server reference model

### 8.1 The two identities (fixed)

- **A specific historical version** is identified by **`document_id`**, which is **immutable once
  created** (deterministic `legi:<source_uid>@<valid_from>`, C4). Because supersession **retains** old
  version rows, a reference to a specific-version `document_id` keeps resolving across updates *and*
  across a re-baseline.
- **"The article, as-of date D"** is identified by **`source_uid` / `version_group` + an as-of date** —
  `document_id` is **not** a stable identifier for the logical article across versions (a new version
  is a new row with a new `document_id`).

### 8.2 Writable→server references are soft and validated (no hard cross-schema FKs)

**Design decision (consultation §2):** the `jurisearch_app` layer references server data via
**validated soft references** — ordinary columns plus validation state — **not** hard cross-schema FKs.
A hard FK would force constraint drop/recreate (or `NOT VALID` gymnastics) on **every** media baseline
and turn a server reload into a writable-schema migration — too much coupling for an extension point
whose tables are out of scope.

Reference column contract (design shape for any app table referencing server data):

```
target_kind          -- 'document_version' | 'logical_article' | 'decision' | 'chunk' | 'zone_unit'
corpus
document_id          -- when pinning a specific immutable version row
source, source_uid, version_group, as_of_date   -- when tracking a logical object over time
resolved_document_id, resolved_generation, resolved_schema_version, validated_at, validation_status
```

Validation is run by the **local service after each package apply** and on demand before workflows that
need a live target. Re-baseline becomes: switch generation → run reference validation in the background
→ mark missing/changed targets explicitly → let app UX decide pin / retarget / warn. If a future app
table needs stronger guarantees, add a **client-owned** validation table
(`reference_id, active_generation, resolved_document_id, valid`) — **not** a direct FK into server
physical tables.

### 8.3 Reference convention by meaning

- **"This exact version/text I saw"** (citations in a generated memo, audit evidence, quoted passages,
  a saved search result, any reproducibility-sensitive artifact) → pin by **`document_id`**.
- **"The article applicable at date D" / "track this article over time"** → **`source_uid`/
  `version_group` + `as_of_date`**; the resolver selects the server row whose `valid_from`/`valid_to`
  window contains the as-of date. Store the last `resolved_document_id` as a **cache/evidence** field —
  not the semantic identity.
- **Jurisprudence decisions** → `document_id` (= `<source>:<source_uid>`) is usually both specific and
  logical (decisions are not temporal versions); still store `source`/`source_uid` for display/repair.
- **Chunks / zone units** → anchor app objects at the **document/article level + offsets/quote-hashes**,
  **not** hard references to `chunk_id`/`zone_unit_id`, unless the feature is explicitly
  retrieval-debugging. Chunking/zone builders can be reissued (the schema carries
  `chunk_builder_version` / `zone_unit_builder_version` precisely as a warning that these are derived
  identities).

---

## 9. Indexing, sizing, and catch-up

### 9.1 Enrichment is computed upfront and packaged

`decision_zones` (today a **lazy on-demand cache**) and `official_api_responses` are **enriched
proactively server-side** and shipped read-only. Consequence the design accepts: this is a server-side
ingestion-scope expansion **bounded by provider coverage** (Judilibre covers Cour de cassation
`cass`+`inca` only; the rest stays `zone_accurate=false`). Clients make **no** upstream PISTE/Judilibre
calls — a client `fetch --part` is served entirely from the local store.

### 9.2 The `official_api_responses` size line item

The raw archive (body text + parsed jsonb + sha256 per exchange) is **large** and is a real
bandwidth/storage line item for whichever tier carries it. Redistribution licensing of these raw
upstream bodies is **deferred to future work**; the subscription tier is the eventual gate.

### 9.3 Index materialisation — client-build (fixed default)

**Design decision:** packages carry **rows + schema (DDL incl. index definitions)**, and the **client
builds the indexes** — IVFFlat finalize at corpus-sized `lists` + the `pg_search` BM25 build.
`pg_search` is on every client (decided), so BM25 is always available — not an availability gate.

**When the build runs (consistent with the apply state machine, §7.1/§7.3):** a full **finalize**
(IVFFlat `lists` rebuild + BM25 build, mirroring `dense.rs:122–190` / `zone_units.rs:461–524`) runs in
the **long phase of a baseline/re-baseline, before the view switch** (§7.4) — so a generation is only
exposed once fully indexed. For **ordinary incrementals**, new/changed rows are indexed by PostgreSQL's
**row-level index maintenance inside the apply transaction**; no finalize is required and the cursor
advance reflects an indexed state. The only incremental that triggers an explicit build is one carrying
a **new index definition** (additive index DDL), which builds within apply **before** the cursor
advances. In every case, **index materialisation is part of apply/activation — never a step after the
cursor advances** — so a corpus is never advertised query-ready on an unbuilt index.

Shipping **prebuilt index data** is **rejected for the default logical path**: PostgreSQL has no
portable *logical* artifact for a populated custom index (IVFFlat, ParadeDB BM25) independent of the
table data (C6). Shipping one means copying **relation files**, which reintroduces the exact
engine/major-version/extension-binary/CPU-architecture constraints that ruled out the physical standby.
It is retained **only** as a constrained **physical-format variant** (with `postgres_major_min/max` and
arch gates in the embedded manifest), never as a free peer of client-build.

### 9.4 Catch-up policy (size-driven, not count-driven)

**Design decision (consultation §5):** the client chooses incremental catch-up vs fresh baseline by
**cumulative byte size + estimated apply cost**, not chain length. Vectors make bytes the right proxy —
10 small text corrections ≠ 10 packages of millions of refreshed embeddings.

- **Server retains** a window per corpus (e.g. ~90 days **and** ~120 latest daily packages) and
  publishes `min_available_sequence` + precomputed `catchup_ranges`.
- **Prefer incremental** while *all*: no gap to head; every package compatible (version, schema,
  fingerprint, builders); cumulative **compressed** diff < ~25–35% of the compressed baseline;
  estimated apply work under budget (e.g. ~30–45 min on the reference client profile); no package marked
  `requires_baseline_after_apply`/`superseded_by_baseline`.
- **Prefer fresh baseline** when *any*: client `sequence < min_available_sequence`; cumulative
  compressed diffs > ~⅓ of baseline; cumulative uncompressed row/vector bytes > ~50% of baseline bytes;
  the range crosses a fingerprint/builder full reissue; the chain includes a breaking schema/corpus
  rewrite; expected apply/index time exceeds media baseline load time.

Thresholds are **manifest-configured per corpus** (`catchup_policy`), not hard-coded — keeping the
policy tunable without a client upgrade. The numeric ranges above are design defaults to be confirmed
against measured baseline/diff sizes during implementation.

---

## 10. Version gating

The gate lives **in each package** (`minimum_client_version`); the service applies a package only if
its version clears that minimum, else **warns and rejects** with `client_too_old`. This reuses the
existing `SchemaVersionAhead` shape (C2) — a DB ahead of the binary is already rejected, which is
exactly the "you must upgrade" signal a package minimum encodes.

What **forces the server to raise a package minimum** (mandatory client upgrade):

- a **schema migration** the client binary doesn't yet understand (additive → rides in a normal package,
  gated by version; breaking/corpus-rewriting → media re-baseline, §6.1);
- an **embedding model change** (fingerprint bump → full re-issue of every vector → new full package/
  baseline, raised minimum);
- a **builder-version bump** (`chunk_builder_version` / `zone_unit_builder_version`) invalidating all
  derived rows of that kind → full re-issue + raised minimum.

Each package therefore carries as first-class fields: **minimum client version, corpus, schema version,
embedding fingerprint, builder versions** (§6.2.2 compatibility gates), and the client manifest is what
the service reads to pick packages it is both *entitled to* and *compatible with*.

---

## 11. Integrity, trust, and entitlement

### 11.1 Verification sequence (every step warn-and-reject; no partial cursor movement)

1. Verify the **remote manifest signature** before making download decisions.
2. Verify the **artifact digest** (`sha256`) after download.
3. Verify the **embedded manifest signature/digest** before unpacking.
4. Verify **per-file digests** before applying each table/change file.
5. Verify **post-apply row/set digests** (postconditions) **before** advancing the cursor.

Any failure → warn-and-reject with the matching code (§6.3). The cursor advances **only** after all
data, indexes, and postcondition checks pass.

### 11.2 Trust root

Clients apply server-built **data, schema, and prebuilt embeddings they cannot cheaply re-derive**, so
artifacts (network packages **and** physical media) are **signed and verified on apply**, with a
`key_id`/key-epoch in every manifest. A **"reproduce from official source" escape hatch** is retained
for clients whose reproducibility posture demands it.

### 11.3 Entitlement is an apply precondition, not URL hiding

The remote manifest is filtered by subscription where possible, **and** the client independently
verifies the embedded package's `entitlement_corpus`/`tier` against a **locally installed license
token**. A mismatch → `missing_entitlement`. Tiering is intrinsic to the model: one package stream per
corpus; open corpora are simply packages with no subscription requirement; a single client DB holds
whatever mix of corpora the client is entitled to — no separate clusters, no per-subscriber slots.

### 11.4 Hosting contract

Serving signed packages to many external clients requires **authenticated, TLS-protected hosting** with
**credential-based subscription enforcement per corpus**. The current `serve` daemon is loopback-only
and unauthenticated (C9), so the producer-side hosting surface is net-new. (Concrete infrastructure is
an ops decision, §1.3.)

---

## 12. Apply atomicity and concurrency

- **Incremental** apply runs in **one transaction** with a **package-apply advisory lock** and a low
  `lock_timeout`, so it fails cleanly instead of stalling behind a long CLI query (§7.3).
- **Baseline/re-baseline** keeps the write-heavy/index-heavy work in the **long phase off the live read
  path**, and confines reader impact to a **short view-switch transaction** (§7.4).
- The **control cursor** outside the swap unit makes retries deterministic and apply idempotent
  (already-applied packages are skipped via the cursor).
- The **CLI read path goes through the stable `jurisearch_server` views**, which are repointed
  atomically at commit — readers see either the old or the new generation, never a half-applied state.

---

## 13. Design invariants (summary contract)

1. **Three event kinds are mandatory:** `upsert` (incl. in-place base-row updates), `delete`,
   `replace_set`. A purely additive stream is incorrect.
2. **Derived rebuilds are document-scoped `replace_set`s** with builder/fingerprint stamps and a
   `set_digest` — including **`chunks` (with cascaded `chunk_embeddings`)** whenever chunk membership/
   partitioning/body changes, since `chunks` are themselves BM25-indexed replicated rows; never per-row
   delete streams (except rare redactions) and never generation-wide truncation in ordinary diffs.
3. **Ordered, gap-free apply** governed by `corpus_state.sequence`; missed packages are caught up, never
   skipped; retries are idempotent via the cursor.
4. **Server data lives in per-corpus generationed schemas behind stable views**; a re-baseline repoints
   **only the affected corpus's views** (other installed corpora and their generations are untouched),
   never an in-place destructive drop on the operated path.
5. **The writable `jurisearch_app` namespace and the `jurisearch_control` cursor live outside every
   generation** and survive every re-baseline.
6. **Index materialisation is part of apply/activation, never after the cursor advances:** finalize on
   baseline/re-baseline before the view switch; row-level maintenance inside the txn for ordinary
   incrementals; explicit build before cursor advance only for a package adding a new index definition.
7. **Writable→server references are soft and validated**, pinned by `document_id` for exact evidence and
   by `source_uid`/`version_group` + `as_of_date` for logical articles; no hard cross-schema FKs.
8. **Client builds indexes** (IVFFlat finalize + BM25); prebuilt-index shipping is a
   rejected-by-default physical variant only.
9. **Every package is signed and self-sufficient**; integrity, version, and entitlement are **apply
   preconditions**; unmet conditions → **warn-and-reject** with a machine-readable code and **no partial
   cursor movement**.

---

## 14. Traceability — design decisions to their source

| Design element | Decided by | Resolved/specified here |
|----------------|-----------|--------------------------|
| Per-corpus ordered incremental packages | Analysis (DECIDED) | §6.1, §7.3 |
| Physical-media baselines + re-baselines | Analysis (DECIDED) | §6.1, §7.4 |
| Re-baseline preserves writable tables | Analysis (DECIDED) | §4.1, §7.4 |
| Server data in its own namespace | Analysis (DECIDED) | §4 |
| Enrichment computed upfront + packaged | Analysis (DECIDED) | §9.1 |
| `document_id` immutable; logical id = `source_uid`/`version_group` | Analysis (DECIDED) | §8.1 |
| `pg_search` on every client | Analysis (DECIDED) | §9.3 |
| Warn-and-reject on unmet conditions | Analysis (DECIDED) | §6.3, §11, §13.9 |
| Scoped reload via generationed schema + view switch | Consultation §1 (open → resolved) | §4.3, §7.4 |
| Soft validated writable→server references | Consultation §2 (open → resolved) | §8.2–§8.3 |
| Change-log/outbox as primary diff source | Consultation §3 (open → resolved) | §5.1 |
| `replace_set` for derived tables | Consultation §4 (open → resolved) | §5.3 |
| Size-driven catch-up policy | Consultation §5 (open → resolved) | §9.4 |
| Split signed manifests + apply contract | Consultation §6 (open → resolved) | §6.2 |
| Index-in-package vs client-build | Analysis (decide-at-design) | §9.3 — client-build fixed |
| Package manifest field contract | Analysis (decide-at-design) | §6.2 — fixed |

---

## 15. Questions intentionally left to implementation

These are **implementation choices**, not unresolved architecture — the design fixes the contract
around each:

1. **View vs stable-function indirection for hot retrieval paths** (§4.3) — measured at implementation;
   the generation-indirection contract is fixed regardless.
2. **On-the-wire payload encoding** per file (`copy-binary` vs `jsonl` vs `parquet`) (§6.2.2) — the
   manifest declares it per file; the choice is a performance/tooling decision.
3. **Exact catch-up thresholds** (§9.4) — design defaults given; final numbers come from measured
   baseline/diff sizes and are manifest-configured (no client upgrade needed to tune).
4. **Signing scheme specifics** (algorithm, key custody, rotation cadence) (§11.2) — the manifest
   carries `key_id`/epoch; the cryptographic and ops specifics are an implementation/ops decision.
5. **Hosting/CDN topology and authentication mechanism** (§11.4) — contract is "authenticated, TLS,
   per-corpus entitlement"; the concrete service is an ops decision.

---

## 16. Bottom line

The design realises the analysis's decided architecture as a concrete set of contracts: a **central
producer** that captures semantic change in an **ingest-side outbox**, materialises **signed per-corpus
packages** (three event kinds, document-scoped `replace_set`s for derived tables, a self-sufficient
embedded manifest with apply pre/postconditions), and publishes a **split, signed remote manifest** with
size-driven catch-up ranges. A **client service** applies that chain **in order, gap-free, idempotently**
into a **generationed server namespace fronted by stable views**, while a **control cursor** and a
**writable app namespace** sit outside the swap unit so a **media re-baseline scope-replaces only the
server set**. The writable layer references server data through **soft, validated references** — pinned
by `document_id` for evidence, by `source_uid`/`version_group` + as-of for logical articles — never hard
cross-schema FKs. **Clients build their own indexes**; **every artifact is signed**; **version and
entitlement are apply preconditions**; and any unmet condition is a **warn-and-reject with a
machine-readable code and no partial cursor movement**. The data plane is friendly because the repo
already provides a temporal base corpus, immutable version-specific PKs, and the change-tracking fields
this design leans on; the substantive work the design defines is the **package/manifest pipeline**, the
**client apply protocols**, and the **generationed schema + view-switch** that keeps the writable
application layer intact across re-baselines.
