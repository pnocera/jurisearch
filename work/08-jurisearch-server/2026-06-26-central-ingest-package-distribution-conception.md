# Central ingest + packaged distribution — conception

Date: 2026-06-26
Status: Conception (principles and abstractions; not an implementation plan, no phasing, no code)
Companion to: `2026-06-26-central-ingest-package-distribution-design.md` (the contracts) and
`2026-06-25-central-ingest-delta-sync-analysis.md` (the decisions).

> The design document fixes *what the system is* — schemas, package formats, manifest fields,
> apply protocols. This conception sits one level above it and answers a different question:
> *why does the system take that shape, and what keeps it coherent as it grows?* It names the
> abstractions, assigns responsibilities, draws boundaries, and shows how the whole thing is held
> together by **DRY** (one authoritative definition per concept) and **SOLID** (single
> responsibility, open/closed, substitutable parts, segregated interfaces, inverted dependencies).
> Read this to understand the structure; read the design to build against it.

---

## 1. The conception in one paragraph

A **central producer** ingests French legal data once and turns every change into a **semantic
event** on a **per-corpus, ordered, gap-free log**. Those events are materialised into **signed,
immutable packages** — large **baselines** that travel on physical media, small **incrementals**
that travel over the network. Many **read-only clients** replay each corpus's package chain, in
order, onto a **local materialised copy** that lives behind a **stable read surface**, while a
**writable application layer** sits beside that copy and outlives every refresh of it. Nothing in
the system invents a second way to say the same thing: identity, change, compatibility, and trust
each have exactly one authoritative definition that travels from the producer to the client
unchanged.

That paragraph already contains the whole conception. The rest of this document is its justification
in terms of principles, because the principles are what will keep the system from rotting as corpora,
clients, and schema versions multiply.

---

## 2. The mental model: a replicated, signed event log

The system is best understood as **per-corpus log replication with materialised read state**, not as
"a database that syncs."

- The producer holds the **authoritative state** and an **append-only ledger of semantic changes** to
  it (the outbox).
- A **package** is a signed, immutable *segment* of that ledger (an incremental) or a *snapshot* of the
  state with an empty prior log (a baseline / re-baseline).
- A **client** holds a **materialised projection** of the log up to some position, plus a **cursor**
  recording exactly that position.
- Catching up means **replaying the missing segments in order** *while the retained chain is available
  and compatible*; when it is not — the chain has fallen out of the producer's retention window, or
  replaying it would cost more than a fresh load — catch-up instead restarts from a signed **baseline**
  root (§5, §6). Either way replication is strictly **one-directional**: the local service
  *materialises* signed producer packages into the local replica, and the client application never
  *originates* a semantic change to server-managed data. There is no merge, no conflict resolution, no
  bidirectional reconciliation.

Everything else in the conception falls out of taking this model seriously: ordering must be
authoritative (so the log needs its own per-corpus sequence), events must be *semantic* (so the log
must record *intent* — insert vs delete vs set-replacement — not just row images), and the read state
must be swappable without disturbing the cursor or the writable layer (so storage and position must be
decoupled).

### 2.1 The vocabulary (the nouns of the system)

These are the abstractions every later section refers to. Each is defined **once, here**.

| Abstraction | What it is | Why it exists |
|---|---|---|
| **Corpus** | The unit of distribution, ordering, entitlement, and generation (e.g. `core`, `inpi`). | One axis of independence: corpora advance, re-baseline, and are licensed separately. |
| **Event** | A semantic mutation: `upsert`, `delete`, or `replace_set`. | The only vocabulary of change. Records *intent*, which row images alone cannot. |
| **Outbox / change-log** | The producer's append-only ledger of events, written in the same transaction as the mutation. | The single source of "what changed since position N." |
| **Package** | A signed, immutable artifact: `baseline`, `re-baseline`, or `incremental`. | The transport unit. Self-describing and self-verifying. |
| **Manifest** | The contract carried with packages — a *remote* listing (planning) and an *embedded* one (apply). | The shared agreement between producer and client. |
| **Generation** | A physical, per-corpus schema holding one materialised copy (`…_<corpus>_gNNNN`). | The swap unit for a baseline: built aside, switched atomically. |
| **Stable view** | The fixed, client-facing read surface over a corpus's *active* generation. | Decouples readers from physical storage; makes a re-baseline a view repoint. |
| **Control cursor** | The per-corpus record of "where this client is," living outside every generation. | The authority on position; survives every generation swap. |
| **Writable app layer** | The client-owned namespace beside the replicated copy. | The extension point; references replicated data but is never overwritten by it. |
| **Soft reference** | A validated, FK-free pointer from the writable layer into replicated data. | Lets the app cite replicated rows without coupling its schema to the swap unit. |
| **Entitlement** | A locally verified license to a corpus. | Makes tiering intrinsic: one package stream per corpus, gated at apply time. |

---

## 3. DRY — one authoritative definition per concept

DRY here is not "don't copy-paste code." It is the stronger discipline that **every concept the
producer and client must agree on has exactly one authoritative definition, declared in one place and
carried — never re-derived — to every place that consumes it.** Duplication in a distributed system is
not verbose; it is a *correctness bug waiting for the two copies to disagree.*

| Concept | Single source of truth | Carried (not re-derived) to |
|---|---|---|
| **Identity** of a row | Deterministic PK at the projection boundary (`legi:<uid>@<valid_from>`, `<source>:<uid>`); `response_id` for archived API bodies. | Outbox scope key → package payload → client insert → soft references. The client never mints its own id for replicated rows. |
| **What changed** | The outbox, written transactionally with the mutation. | The package builder reads only the outbox to know the changed scopes; snapshot/hash exists *only* as an out-of-band QA backstop, never as a second diff path. |
| **Ordering** | The per-corpus package sequence. | `from`/`to` in the embedded manifest, `head`/`min_available` in the remote manifest, and `corpus_state.sequence` on the client — all coordinates in the *same* per-corpus package-sequence space. (The global audit `change_seq` is deliberately a *different* coordinate system and is never used for chain ordering, precisely so the two are not conflated.) |
| **Content compatibility** | The stamp set `schema_version`, `embedding_fingerprint`, `builder_versions`. | Stamped once at build and propagated identically through outbox → package → both manifests → control cursor → precondition check. The client compares these stamps against its cursor; it never recomputes compatibility. (The schema-migration / extension **bundle digest** is a separate, manifest-carried integrity/apply-precondition artifact — *not* an outbox or cursor stamp.) |
| **Client-binary compatibility** | `minimum_client_version`, on each package and in the remote manifest. | A *selection and apply precondition only*: the service compares it to its own running version. It is **not** carried in the outbox and **not** persisted in the cursor — it gates the binary, not the data. |
| **The mutation vocabulary** | The three event kinds. | The producer emits in this vocabulary; the client applies in this vocabulary; the manifest summarises in it. There is no fourth, table-specific path. |
| **A scoped derived-set replacement** | One invariant — *delete a scope's derived rows, then reinsert the authoritative set* — already enforced by the live writer (e.g. `replace_zone_units_for_document`). | The `replace_set` event *is* that operation shipped over the wire, applied per declared scope (`zone_units`/`zone_unit_embeddings` per document; `chunks_with_embeddings` per document when chunk membership/partitioning/body changes). The wire format reuses the database's own set-replacement semantics rather than inventing a parallel one; the specific scopes are *examples* of the one invariant, not separate mechanisms. |
| **The apply contract** | The embedded manifest's pre/postconditions, apply order, and index-build clause. | The client executes exactly what the manifest declares; the producer guarantees exactly what it declared. One contract, two readers. |
| **Failure meaning** | The machine-readable reject-code set. | Producer (why a package may be refused), client service (why it refused), and operator tooling all speak the same closed vocabulary instead of ad-hoc strings. |
| **Trust** | The signature + `key_id`/epoch on every artifact. | Network packages and physical media verify through the *same* mechanism — media is not a second, weaker trust path. |

The payoff of this column-2 discipline: **adding a corpus, a table, or a schema version touches one
definition, and the change propagates by transport, not by parallel edits in producer and client.** The
moment a concept has two definitions — one on each side — the system acquires a way to be subtly,
silently wrong. The conception's first job is to deny it that.

A note on the *deliberate* non-duplications, because they are the load-bearing ones:

- The diff is computed **once**, from the outbox. Logical decoding, a uniform `updated_at` watermark,
  and snapshot diffing are each rejected *as primary mechanisms* — not because they fail individually,
  but because adopting any of them creates a second source of "what changed" that can disagree with the
  outbox.
- Indexes are described **once** (as DDL in the package's schema) and **materialised on the client**
  (finalised for a baseline, engine-maintained inside the apply transaction for ordinary incrementals
  — §4.2), never shipped as data. Shipping prebuilt index data would mean the index exists in two
  forms — source DDL and copied relation files — bound to engine internals; rejected by default for
  exactly that reason.

---

## 4. SRP — one reason to change per part

Single Responsibility is the partition of the system into parts that each **change for one reason**.
The test applied throughout: *if two concerns would force edits to the same part for unrelated reasons,
split them.*

### 4.1 The coarse split — producer / consumer / reader

| Part | Sole responsibility | Changes only when… |
|---|---|---|
| **Ingestor** | Mutate authoritative server state from upstream sources. | …a source format or ingestion rule changes. |
| **Outbox** | Record the *semantics* of each mutation, transactionally. | …a new mutation semantics appears at a projection boundary. |
| **Package builder** | Materialise signed artifacts from outbox segments. | …the package/transport format changes. |
| **Manifest + hosting** | Publish what exists and gate who may fetch it. | …the catalogue, entitlement, or signing surface changes. |
| **Client service** | Select, verify, apply, and track position. | …the apply protocol or lifecycle changes. |
| **CLI / app reads** | Query the stable view surface. | …a user-facing query changes. |

The ingestor does not know packages exist. The CLI does not know generations exist. The builder does
not call upstream APIs. Each boundary is a place the system can change on one side without dragging the
other along.

### 4.2 The fine split inside the client service

The client service is itself decomposed so that lifecycle concerns do not entangle:

- **Planner** — decides incremental-vs-baseline from manifest metadata (size, gaps, compatibility).
- **Verifier** — checks signatures, digests, entitlement, preconditions. *Only* says yes/no-with-reason.
- **Applier** — executes the manifest's apply contract for one package.
- **Generation manager** — builds, activates (view switch), retires, and cleans up generations.
- **Index materialiser** — ensures the manifest-declared index state before activation and cursor
  advance: a full IVFFlat/BM25 finalise for a baseline/re-baseline, engine-maintained indexes inside
  the apply transaction for an ordinary incremental, and an explicit build only for the rare
  incremental that adds new index DDL.
- **Reference validator** — re-resolves the writable layer's soft references after a switch.
- **Cursor authority** — the *only* writer of `corpus_state`.

The two SRP separations that matter most, because they are what make the rest safe:

1. **Position is not storage.** The control cursor (where the client is) is a different concern from
   the generation (what the client holds), changes for different reasons, and therefore lives in a
   different namespace that is *never* part of a swap. This is what lets a re-baseline replace the data
   without ever putting the client's recorded position at risk.
2. **Server data is not application data.** The replicated copy and the writable app layer are owned by
   different parties, change for different reasons, and so occupy different namespaces. The app layer's
   survival across a re-baseline is not a feature bolted on — it is the direct consequence of having
   refused to make the two one responsibility.

---

## 5. OCP — open to new corpora and formats, closed to engine edits

The system must absorb new corpora, new tables, new encodings, and new schema versions **without
reopening the apply engine**. Open/Closed is achieved by turning each expected axis of growth into a
*parameter or a data declaration* rather than a branch in code.

| Axis of growth | Closed (untouched) | Open via |
|---|---|---|
| A new corpus | The apply engine; every other corpus's data and chain. | `corpus` is a parameter; each corpus has its own chain, cursor, and generation line. Adding one is configuration + a new chain, not an engine edit. |
| A new replicated table that fits the existing identities, dependency order, and event kinds | The outer apply lifecycle and the event vocabulary. | It joins the replicated set by **role membership** (authoritative / enrichment / control), not by a per-table branch. A table needing a *new* identity exception, FK ordering, or set-replacement scope (as `official_api_responses` needed its producer-assigned `response_id` and pre-citation apply order) requires a **deliberate extension of the manifest/apply contract** — the outer lifecycle stays closed, but the contract is not infinitely generic. |
| A new payload encoding | The apply protocol. | The manifest's payload-layout **declares** the encoding per file (`copy-binary` / `jsonl` / `parquet`); the applier dispatches on the declaration. |
| New catch-up thresholds | The client binary. | Thresholds are **manifest-configured per corpus** — policy is data, tunable without a client upgrade. |
| A new entitlement tier | The apply engine. | Tier is a stamp checked against a local token; "open" is just "no subscription required." |
| An additive schema migration | The transport and the baseline machinery. | Packages carry their own schema; additive DDL rides a normal incremental, gated only by client version. |

The decisive OCP property is **per-corpus generations**. Because each corpus owns an independent
physical generation line behind its own slice of the stable view, **re-baselining `core` is closed
over `inpi`** — `inpi` and every other installed corpus are not read, not merged, not touched. There is
no whole-server generation that every corpus must be re-merged into; that design would make every
single-corpus change an all-corpus modification — the precise opposite of open/closed.

The complementary closure is on the **read path**: queries name the stable view, never a generation
suffix, so swapping the physical generation underneath is invisible to readers. The read surface is
closed against storage churn.

The boundary line: OCP applies to the *expected* axes named above. A genuinely new *kind* of change —
a fourth event semantics, a second trust root — is meant to require deliberate design, not a silent
config toggle. The conception is open where growth was anticipated and honestly closed where it was not.

---

## 6. LSP — parts are substitutable behind their contracts

Liskov Substitution: wherever the system has several concrete forms of one abstraction, **any form must
honour the abstraction's contract**, so the consumer can treat them uniformly.

- **Package kinds behind the apply contract.** `baseline`, `re-baseline`, and `incremental` are three
  realisations of "an applicable package." Each honours the same contract: *verify preconditions →
  apply → verify postconditions → advance the cursor exactly once, atomically, with no partial
  movement on failure.* Their mechanics differ (an incremental is one transaction into the active
  generation; a baseline stages a new generation and switches a view) but the **observable contract is
  identical**, so the service's outer loop does not special-case them beyond selecting the strategy.
- **Generations behind the stable view.** Every generation is a valid substitute for the previous one
  *as seen through the view*: same relation names, same columns, same query semantics. A reader cannot
  observe which generation backs the view — that indistinguishability is exactly what makes the switch
  safe.
- **The escape hatch.** "Reproduce from official source" must yield state **substitutable** for the
  packaged state — same logical content, same identities — or it is not an escape hatch but a fork. The
  contract (deterministic identity, declared fingerprints/builders) is what makes independent
  reproduction a true substitute.
- **The physical-format variant.** A prebuilt-index package is admissible *only if* it produces state
  observably identical to client-build. It is a constrained substitute (gated on engine/arch in its
  manifest), never a free peer — because it can only honour the contract under conditions the logical
  path does not require. The conception keeps it substitutable but fenced.

The LSP discipline is what lets §4's outer loop stay simple: it programs to "a package" and "a
generation," and trusts every concrete form to behave.

---

## 7. ISP — narrow interfaces, each fit to one consumer

Interface Segregation: **no consumer should depend on a surface larger than it uses.** Each boundary in
the system is sized to its client, so changes to one surface do not ripple into consumers that never
needed it.

- **The manifest is split by use.** The **remote manifest** answers "what should I download?" (chain
  head, retention window, sizes, compatibility) — the *planning* interface. The **embedded manifest**
  answers "how do I apply this artifact I already hold?" (pre/postconditions, apply order, digests) —
  the *apply* interface. A planner does not carry the full apply contract; an applier does not depend on
  the whole chain listing; and crucially the client **never has to trust the remote listing once it
  holds an artifact** — the artifact is self-sufficient. Two interfaces because there are two distinct
  consumers.
- **Read vs apply vs control are three surfaces.** The CLI sees **only** the stable read views — not the
  apply machinery, not the cursor, not the generations. The service sees the apply and control surfaces.
  No consumer is forced to link against capability it does not exercise; a read client cannot even
  express a write to replicated state.
- **The writable layer sees a soft-reference surface, not the tables.** The app layer depends on a thin
  contract — reference columns plus a resolver — not on the physical replicated schema. It is segregated
  from everything about the storage it points into.
- **Failure is a narrow, closed vocabulary.** Reject codes (`client_too_old`, `missing_entitlement`,
  `sequence_gap`, `signature_invalid`, …) are a small, explicit interface for "why not," instead of a
  wide, unstable surface of free-text errors that every consumer would have to parse defensively.

---

## 8. DIP — high-level policy depends on abstractions, not storage

Dependency Inversion: **high-level concerns depend on stable abstractions; volatile, low-level details
depend on those same abstractions — never the reverse.** This is the principle that makes the whole
re-baseline story possible.

- **Reads depend on the view, not the generation.** The query path (high-level: "give me this relation")
  depends on the stable `jurisearch_server` abstraction. The physical generation (low-level, volatile —
  it is replaced on every baseline) is what depends on conforming to that view's shape. The dependency
  points *toward* the stable thing, so the volatile thing can change freely underneath.
- **Position is inverted out of storage.** The control cursor (high-level: "where am I") does not live
  inside the data it tracks; the data generation is the volatile detail, and the cursor — the stable
  authority — sits outside it. Inverting this dependency is what guarantees the cursor survives a swap
  it would otherwise be destroyed by.
- **The writable layer depends on identity, not on physical rows.** It points at replicated data through
  *soft references resolved by a resolver*, not hard cross-schema foreign keys. A hard FK would invert
  the dependency the wrong way — the durable, app-owned layer would depend on the volatile, swappable
  physical tables, and every re-baseline would become a writable-schema migration. Soft references keep
  the app depending on the stable abstractions (`document_id` for an immutable version,
  `source_uid`/`version_group` + as-of for a logical article) instead.
- **Both sides depend on the contract, not on each other.** The producer and the client do not depend on
  each other's internals; both depend on the **package/manifest contract**. The contract is the
  abstraction in the middle. Either side can be reimplemented as long as the contract holds — which is
  also what makes the independent-reproduction escape hatch (§6) coherent.

DIP and DRY meet here: the *abstraction* every party depends on (§8) is the *single authoritative
definition* every party shares (§3). They are the same artifacts — the contract, the identity, the
view — seen from two angles.

---

## 9. The invariants the principles protect

The principles above are not ornamental; they exist to hold a small set of non-negotiable invariants
true. If an implementation choice ever threatens one of these, the choice is wrong — not the invariant.

1. **One-directional replication.** The local service materialises signed producer packages into the
   local replica; the client application never originates a semantic mutation to server-managed data,
   and there is no merge path back to the producer. (Cuts out an entire class of conflict and
   reconciliation logic the system refuses to own.)
2. **Ordered, gap-free, idempotent apply.** Position is governed by the cursor; within the retained
   incremental chain a missed package is caught up by applying the missing packages *in order*, never
   skipped, and a re-applied package is a no-op. When the chain is unavailable or replaying it would
   cost more than a fresh load, catch-up restarts from a signed baseline root — it never skips packages
   to close a gap. (SRP: cursor as sole position authority; LSP: every package honours it.)
3. **Atomicity with no partial movement.** A package either fully applies and advances the cursor, or
   changes nothing observable. Activation (including any required index materialisation) precedes cursor
   advance; readers see old-or-new, never half. (LSP contract; DIP view switch.)
4. **The writable layer and the cursor outlive every generation.** A re-baseline scope-replaces *one
   corpus's server data* and nothing else. (SRP: position≠storage, app≠server; DIP: app→identity.)
5. **Every artifact is self-sufficient and signed.** Trust, version, and entitlement are *apply
   preconditions*, identical for network and media. (DRY trust root; ISP self-sufficient artifact.)
6. **Unmet conditions warn and reject with a machine-readable code.** Never a guess, never a partial
   apply. (ISP failure vocabulary; DRY shared codes.)
7. **Identity is assigned once, by the producer, and replicated verbatim.** The client mints no ids for
   replicated rows. (DRY identity.)

---

## 10. What this conception deliberately does not decide

A conception earns trust partly by being honest about its edges. The following are **out of scope here**
and belong to the design (the contracts) or to implementation/operations:

- **Concrete schemas, package layouts, manifest field lists, apply algorithms** — these are the design
  document's job; this conception only names the abstractions and their responsibilities.
- **Build order, phasing, milestones, code** — no implementation plan is implied by any ordering in this
  text.
- **The writable layer's *internal* design** — this conception fixes only the *boundary* (soft
  references, separate namespace) the app must respect, not its features.
- **Operational choices** — signing algorithm and key custody, CDN/hosting topology, retention windows,
  the exact catch-up thresholds. The conception fixes that these are *manifest-declared* and *config, not
  code*; the values are an ops/measurement decision.
- **The one measured trade-off** — view vs stable-function indirection on hot read paths. The
  *indirection* is fixed by DIP; the *form* is left to measurement.

---

## 11. Bottom line

The system is **per-corpus log replication with materialised read state**, and its long-term health
rests entirely on two disciplines. **DRY** guarantees that identity, change, ordering, compatibility,
trust, and failure each have *one* definition that travels from producer to client unchanged — so the
two sides cannot drift into silent disagreement. **SOLID** guarantees that the parts are separable
(SRP), that growth is absorbed by parameters and declarations rather than engine edits (OCP), that the
several forms of a package or a generation are uniformly substitutable behind their contracts (LSP),
that every consumer sees only the surface it needs (ISP), and that the durable concerns — reads,
position, the writable layer, the cross-party agreement — depend on stable abstractions rather than on
the volatile storage beneath them (DIP). The most important structural facts of the system — that a
re-baseline replaces one corpus's data while leaving the cursor, the application layer, and every other
corpus untouched; that an offline client catches up by replaying the retained chain in order — or from a
fresh baseline when that chain is gone or too costly to replay; that a corpus can be added without
reopening the engine — are not features added on top. They are what these principles
*force* once the system is conceived as a signed, ordered, single-source-of-truth event log.
