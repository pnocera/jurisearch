# JuriSearch site-server + thin-client — design

Date: 2026-06-27
Scope: **design only — no implementation plan, no sequencing.** This document specifies the *software
design* of the customer-site server and the thin client: the components and their responsibilities,
the abstractions (interfaces) that decouple them, how they collaborate, and how this maps onto the
crate graph. It realises the decisions in
[`02-target-architecture.md`](02-target-architecture.md) and the reasoning in
[`01-target-deployment-analysis.md`](01-target-deployment-analysis.md). The producer
(work/08) is a given and is not redesigned here.

Interface sketches below are **design artifacts** (the shape of the seams), in Rust-flavoured
pseudocode — not implementations. Method bodies, SQL, and build ordering are out of scope.

---

## 1. Governing principles

The whole design is driven by two principles; every component and seam below is justified by them.

**DRY — one authority per concept.** Each concept (the wire contract, the active-generation
resolution, the readiness model, the embedding fingerprint, response rendering, the JSONL framing,
generation activation) has **exactly one definition**, depended upon by every consumer. Server and
thin client, reader and writer, never carry parallel copies. Where the current code already holds the
single authority (e.g. the session DTOs, `activate_generation_with_guard`), the design **wraps and
reuses** it rather than re-deriving it.

**SOLID — depend on roles, not mechanisms.** Components depend on **narrow, role-specific interfaces**
(a read-only store, a writer, an embedder, a package source), never on concretions (PostgreSQL, llama,
the filesystem). Concretions implement the interfaces; binaries compose them at the edge. This is what
makes the read-only/writer split, the on-site embedder swap, and the thin/heavy build separation fall
out of the type system rather than out of discipline.

These two pull in the same direction: a single authority for each concept, exposed through a role
interface that only the components needing that role can see.

---

## 2. Responsibility decomposition (SRP)

Each unit below has **one reason to change**. Nothing mixes transport with routing with retrieval with
storage with embedding.

### Site server host

| Component | Single responsibility | Explicitly NOT responsible for |
|---|---|---|
| **Storage backend** | own the connection to the shared PG and hand out role-scoped handles (read pool / writer conn) | retrieval logic, embedding, routing |
| **Active-corpus resolver** | resolve `corpus_state` → the active physical generation schemas (one authority) | running queries, applying packages |
| **Query store (read)** | run read work inside one snapshot: resolve corpora, read readiness, fan retrieval over physical generations | any write; embedding; transport |
| **Embedder** | turn query text into a vector + fingerprint via the on-site bge-m3 | retrieval; storage; routing |
| **Fingerprint preflight** | decide query↔generation embedding compatibility (fail-closed) | embedding; retrieval SQL |
| **Site dispatcher** | map an allowlisted operation to a handler with server-owned context | the operations' logic; transport framing |
| **Transport (server)** | accept connections, frame JSONL lines, apply size/idle limits | routing; query logic |
| **Corpus writer** | apply a verified package: build → validate → activate → **stamp readiness**, atomically | serving reads; trust verification; fetching |
| **Package source** | locate and fetch manifests/artifacts from the producer's publish endpoint | verification; apply |
| **Trust verifier** | verify signature + digest + compat + entitlement of a package | apply; fetch |
| **Sync daemon** | schedule + drive the poll→plan→verify→apply loop | how any one step works (delegates to the above) |

### Thin client

| Component | Single responsibility |
|---|---|
| **Transport client** | send a `SessionRequest` to a service URL and return the `SessionResponse` |
| **Renderer** | turn a `SessionResponse` into human / `--json` output (the *same* renderer the one-shot CLI uses) |
| **CLI front-end** | parse args into a `SessionRequest`, call the transport client, hand the response to the renderer |

The local/server/admin binary keeps the heavy responsibilities (ingest, the producer-side and
operational commands); the thin client carries none of them.

---

## 3. The seams (interfaces) — DIP + ISP

The design's substance is a small set of **role interfaces**. High-level policy (a query handler, the
sync loop) is written against these; concretions are injected at the composition root. Each interface
is deliberately **narrow** so a consumer cannot reach capabilities outside its role.

### 3.1 Shared wire contract (one authority — DRY)

The request/response envelope and operation vocabulary live **once**, in the lowest crate, and are the
*only* thing the thin client and the server share for the wire.

```rust
// jurisearch-contract (today: jurisearch-core::session)
pub struct SessionRequest { pub id: Option<Value>, pub command: String, pub args: Value }
pub enum   SessionResponse { Ok{..}, Err{..} }          // unchanged shape
pub struct ProtocolVersion(pub u32);
/// The framed unit carries the version EXPLICITLY, so the codec has something to reject a skewed peer on.
pub struct ProtocolEnvelope { pub proto: ProtocolVersion, pub request: SessionRequest }
pub enum   Operation { Search, Fetch, Cite, Related, Context, Compare, Status }  // the query surface
impl Operation {                                        // the ONE command<->Operation mapping (contract-owned)
    pub fn parse_command(s: &str) -> Result<Operation, ErrorObject>;  // unknown/legacy/admin/… → SESSION error
    pub fn as_command(self) -> &'static str;
    pub fn parse_args(self, args: &Value) -> Result<RequestDto, ErrorObject>;  // typed + defaulted + validated
}

// Typed per-operation request DTOs are ALSO contract-owned (one schema/defaults/validation authority).
// Server-owned fields (index_dir, data-source) are NOT wire fields here:
pub enum   RequestDto { Search(SearchReq), Fetch(FetchReq), Cite(CiteReq), /* …per Operation */ }
pub struct SearchReq { pub query: String, pub mode: RetrievalMode, pub k: u32 /* , filters, … */ }
```

**The `command`↔`Operation` mapping AND each operation's typed request DTO are contract-owned** — one
schema/defaults/validation authority. `parse_command` maps the wire string; `parse_args` deserializes,
defaults, and validates an operation's `args` into a typed `RequestDto`. **Both** the thin client
(clap → DTO → emit) and `SiteDispatcher`/handlers (parse → validate → build) use the same DTOs, so the
arg shape and defaults are defined **once** — not duplicated across the thin and server sides, and
neither side pulls heavy CLI request types across the dependency cone. Server-owned fields (`index_dir`)
are **not** wire DTO fields. The server validates **every** frame itself (a JSONL peer can send
anything); it never trusts the client to have pre-validated. Parse/allowlist failures (unknown / legacy
/ admin / model / ingest / eval `command`, or invalid args) return the **session error shape**
(`ErrorObject` → `SessionResponse::Err`) — **not** a package `Reject` code; the work/08 `Reject`
vocabulary stays scoped to package verify / apply / fingerprint-preflight.

**Version placement is explicit, not a bare type.** The codec (§3.8) frames a `ProtocolEnvelope`, and
on the first frame checks `proto` against the server's supported range: a mismatch is rejected with a
typed transport error, and an unversioned/legacy frame is **rejected** (not silently accepted). This is
what makes the architecture's "fail loudly on skew" rule real rather than an unattached `ProtocolVersion`
type. (`SessionRequest`'s `{id, command, args}` shape is unchanged — the version rides on the envelope
around it, so the existing payload contract is untouched.)

**Rendering is a sibling dependency-light authority, not a vague "renderer in contract".** Turning a
response into human / `--json` output lives in a **`jurisearch-render`** crate (or, equivalently, typed
per-operation response DTOs plus dependency-free renderers) that depends only on the contract — **no**
storage/embed/handler logic behind the boundary. Both the thin client and the one-shot CLI render
through it identically (§5/§6). This avoids the trap of either a JSON-only writer (which fails the
human/`--json` parity) or operation-specific formatting leaking into the lowest wire crate. Note
`SessionResponse.result` is an untyped `serde_json::Value` today, so the typed-DTO-or-shared-renderer
boundary is what makes parity real rather than assumed.

### 3.2 Storage: one backend, two segregated roles (ISP + DIP)

A single connection abstraction hides "self-managed vs shared server"; on top of it sit **two
disjoint role interfaces** so the query service literally cannot write and syncd literally cannot be
asked to serve a query.

```rust
/// One authority for "where is the DB and how do I get a handle".
pub trait StorageBackend {
    fn read_pool(&self) -> &ReadPool;        // many, read-only identity
    fn writer(&self) -> WriterHandle;        // one, writer identity
}
// Impls: SharedServerBackend (site host), ManagedPostgresBackend (producer/tests) — substitutable (LSP).

/// READ role — no write method exists on this interface (ISP). **Object-safe** (no generic method),
/// so it can be held as `&dyn QueryStore` in the dispatcher context (§3.7).
pub trait QueryStore {
    /// Open ONE read snapshot; all read work runs through the returned handle, and the snapshot ends
    /// when the handle is dropped (its lifetime is bounded by `&self`).
    fn begin_snapshot(&self) -> Result<Box<dyn ReadSnapshot + '_>, QueryError>;
}
pub trait ReadSnapshot {
    fn active_corpora(&self) -> &[ActiveCorpus];         // via the shared resolver (§3.3)
    fn readiness(&self) -> &ReadinessStamp;              // writer-stamped; read-only here (§3.4)
    fn search(&self, q: &PreparedQuery) -> Result<Hits, QueryError>;   // fans over physical gens (§4)
    fn fetch(&self, r: &FetchReq) -> Result<Doc, QueryError>;
    // … one method per query Operation; none of them write.
}

/// WRITE role — disjoint from QueryStore (ISP). Applies atomically.
pub trait CorpusWriter {
    fn apply(&self, pkg: &VerifiedPackage) -> Result<ApplyOutcome, Reject>;  // build→validate→activate→stamp
}
```

`QueryStore` is constructed with the **read-only** identity and `CorpusWriter` with the **writer**
identity, so least-privilege is a *type-level* fact, not a convention.

**Read-role visibility is an activation postcondition owned by the writer.** The type-level split is not
enough on its own: generation schemas are created **dynamically**, so a generation could be activated
that the read-only identity cannot see — and the first post-activation query would fail despite the
"active generation is ready" invariant. So the **activation path (`CorpusWriter::apply`)** owns
propagating read-role visibility: within the switch transaction it must grant the query read role
read access to each newly active generation schema (and keep `corpus_state`, `index_manifest`, and the
stable views readable). Explicit postcondition — **after activation + readiness stamping commits, the
read identity can read the full active topology selected by `ActiveCorpusResolver`; if that cannot be
guaranteed, the apply fails with the cursor unchanged.** This realises the architecture's
read-role-visibility acceptance invariant (02 §10/§11).

### 3.3 Active-corpus resolution (one authority — DRY)

The mapping `corpus_state → active physical generation schemas` is needed by **both** the read snapshot
(to route retrieval) and the writer (to stamp readiness for the active topology). It is defined once
and shared — never re-implemented on either side.

```rust
pub trait ActiveCorpusResolver {
    fn resolve(&self, conn: &Conn) -> Result<Vec<ActiveCorpus>, StorageError>;
}
pub struct ActiveCorpus { pub corpus: CorpusId, pub generation: GenerationId,
                          pub schema: SchemaName, pub fingerprint: EmbeddingFingerprint }
```

(This wraps the existing `corpus_state` logic in `crates/jurisearch-storage/src/generations.rs`,
promoting it to the single resolver both roles call.)

### 3.4 Readiness: one model, written by the writer, read by the reader (DRY)

Readiness has **one type** and **one coverage computation**. The writer produces the stamp inside
activation; the reader only looks it up. There is no second, query-time computation path.

```rust
pub struct ReadinessStamp { pub signature: TopologySignature, pub report: CoverageReport }
pub struct CoverageReport { pub projection: Coverage, pub dense: Coverage }

/// Computed ONCE, by the writer, against the new active topology, inside the activation txn.
pub fn compute_coverage(snapshot: &WriterTxn, corpora: &[ActiveCorpus]) -> CoverageReport;

/// Read role: lookup only. A missing/stale stamp is an apply/writer failure, never a recompute.
pub trait ReadinessSource { fn current(&self, snap: &ReadSnapshot) -> Result<&ReadinessStamp, QueryError>; }
```

(Wraps `crates/jurisearch-storage/src/ingest_accounting/readiness.rs`, but moves the *write* into
`CorpusWriter::apply` and leaves the read path a pure lookup.)

### 3.5 Embedding + fingerprint (one embedder seam, one compatibility check)

```rust
pub trait Embedder { fn embed(&self, text: &str) -> Result<QueryEmbedding, EmbedError>; }
pub struct QueryEmbedding { pub vector: Vec<f32>, pub fingerprint: EmbeddingFingerprint }
// Impl: LocalBgeM3Embedder (on-site llama.cpp endpoint). Substitutable for a test stub (LSP).

/// One fail-closed compatibility check, used before any dense retrieval, per corpus touched.
pub fn ensure_compatible(server: &EmbeddingFingerprint, gen: &EmbeddingFingerprint)
    -> Result<(), Reject>;     // Reject::EmbeddingFingerprintMismatch (reuses the §6.3 vocabulary)
```

The thin client never references `Embedder`; only the server composition root does.

### 3.6 Producer-facing seams for syncd (DIP)

The sync loop is pure policy over abstractions — it knows nothing about HTTP, the filesystem, or the
clock.

```rust
pub trait PackageSource {                       // fs / object-store / CDN behind one interface
    fn latest_manifest(&self, corpus: CorpusId) -> Result<SignedManifest, SourceError>;
    fn fetch(&self, artifact: &ArtifactRef) -> Result<Artifact, SourceError>;
}
pub trait TrustVerifier {                        // wraps jurisearch-package crypto + entitlement
    fn verify(&self, m: &SignedManifest, a: &Artifact) -> Result<VerifiedPackage, Reject>;
}
pub trait Clock { fn now(&self) -> Instant; }    // injected → the loop is testable without real time
```

### 3.7 Operation dispatch (OCP) and server-owned context

The **dispatch loop** is closed for modification: a handler is registered per `Operation`, never added
by editing a match. (Adding a *brand-new* operation is still an explicit change to the closed
`Operation` wire vocabulary plus a protocol-version bump — only the loop, not the contract, is closed.)
Crucially the dispatcher injects a **server-owned context** and never trusts the request for it.

```rust
pub struct ServerContext<'a> {                   // server-owned, LONG-LIVED deps only; NOT from the client
    pub store: &'a dyn QueryStore,               // active corpora come from a snapshot, not from here
    pub embedder: &'a dyn Embedder,
}
pub trait OperationHandler { fn handle(&self, ctx: &ServerContext, args: &Value) -> SessionResponse; }

pub struct SiteDispatcher { handlers: HashMap<Operation, Box<dyn OperationHandler>> }
// - allowlist: only the query Operation set is registered; admin/model/ingest are absent by construction.
// - server-owned binding: any client-supplied `index_dir`/data-source field is stripped/ignored.
```

**Active-generation resolution stays a single in-snapshot authority.** A handler opens
`ctx.store.begin_snapshot()` and obtains active corpora, readiness, and retrieval **only** through that
`ReadSnapshot` (§3.2/§3.3) — no pre-resolved corpus topology is passed through `ServerContext`, so a
request can never run against a corpus list resolved *outside* the snapshot that also reads readiness
and retrieves. A request-level corpus filter is passed *into* the snapshot-scoped query, not resolved
ahead of it.

This is the security boundary from the architecture, expressed as a design rule: the wire envelope is
reused, but the *handler set* and the *context* are the server's, not the caller's.

**Response building is an extracted, side-effect-free authority — NOT the CLI `*_payload` functions.**
Today's `search_payload` / `fetch_payload` / … resolve an `index_dir`, start a self-managed Postgres,
and run the read-path readiness *write* — exactly the side effects the service must not have. So the
design extracts a dependency-light **per-operation response builder** that takes the **typed, validated
request DTO** (§3.1) + a `ReadSnapshot` + the `Embedder` and returns the response body. The site
`OperationHandler`s call it
after `begin_snapshot()`; the existing CLI `*_payload` functions are refactored into thin **adapters**
(CLI-only arg validation + local index open) over the *same* builder. One response-building authority,
and the service carries none of the CLI side effects.

### 3.8 Transport (server + client) — one dependency-light codec authority

The JSONL **codec** — request/response encode/decode, newline framing, max-line behaviour,
protocol-version rejection, and canonical transport errors — is a single **dependency-light** authority
(a `jurisearch-transport` module/crate, or `jurisearch-contract::jsonl`) with **no** dependency on the
heavy CLI stack. The server accept loop and the `JsonlClient` both call it.

```rust
pub trait TransportClient { fn call(&self, req: &SessionRequest) -> Result<SessionResponse, TransportError>; }
// Impl: JsonlClient over TCP or UDS, addressed by the configured service URL (no auth); uses the shared codec.
```

Listener binding, idle/read/write timeouts, server-owned context binding, and `index_dir` stripping
stay in the **server composition layer**, *not* in the codec. (Today's `crates/jurisearch-cli/src/serve.rs`
mixes the codec with CLI args/output/session dispatch and `index_dir` injection; the design *splits*
that — the framing rules move into the dependency-light codec so the thin client can reuse them without
pulling `serve.rs`'s CLI/session dependencies, while the server keeps its listener/loop.)

---

## 4. Multi-corpus query collaboration

Because indexes live on the **physical generation schemas** (not the union views), an all-corpora
search is a **fan-out + fuse** over the resolver's output, all inside one snapshot:

```
ReadSnapshot::search(q):
  corpora      = self.active_corpora()                       // resolver (§3.3), one authority
  for c in corpora touched by q:
      ensure_compatible(server_fp, c.fingerprint)?           // §3.5, fail-closed, per corpus
  arms         = corpora.map(|c| run_indexed_search(c.schema, q))   // each hits physical indexes
  return fuse(arms)                                          // RRF/merge + paginate above the arms
```

The single retrieval primitive `run_indexed_search` is reused per corpus (DRY); the union views are
reserved for non-indexed reads/status. This is the one place the "documented follow-up" in
`crates/jurisearch-storage/src/runtime.rs:286-290` is realised as a design rule.

## 5. Module / crate structure (dependency inversion in the graph)

The crate graph is arranged so **dependencies point toward abstractions**, and the thin client's
dependency cone excludes the heavy stack.

Three **dependency-light** crates form the shared base — the wire contract, the JSONL codec, and the
renderer — none depending on storage/embed/ingest, so the thin client can use all three without the
heavy stack:

```
        ┌──────────────────────────────────────────────────────────────────────┐
        │  jurisearch-contract (wire DTOs, Operation, Reject)                     │
        │  jurisearch-transport (JSONL codec: encode/decode, framing, versioning) │  ← dependency-light base
        │  jurisearch-render    (human / --json formatting)                       │     (no heavy deps)
        └──────────────────────────────────────────────────────────────────────┘
               ▲                 ▲                      ▲              ▲
   ┌───────────┘        ┌────────┘              ┌───────┘      ┌───────┘
   │ jurisearch-client  │ jurisearch-query      │ jurisearch-  │ jurisearch-syncd
   │ (thin CLI:         │ (QueryStore handlers, │  storage     │ (Clock, PackageSource,
   │  TransportClient + │  SiteDispatcher,      │ (StorageBackend, │ TrustVerifier, CorpusWriter
   │  contract+render   │  Embedder seam)       │  QueryStore/CorpusWriter, │  loop = policy)
   │  +transport only)  │                       │  ActiveCorpusResolver,    │
   │  NO storage/embed  │  depends on storage   │  Readiness)               │
   └────────────────────┘  + embed + base       └──────────────┘           └──────────────
```

- **`jurisearch-contract`** (formalising today's `jurisearch-core::session`): wire DTOs, `Operation`,
  `Reject`, protocol version. No storage/embed/ingest dependency.
- **`jurisearch-transport`**: the dependency-light JSONL codec (§3.8) — encode/decode + framing +
  version rejection — used by both the server accept loop and `JsonlClient`.
- **`jurisearch-render`**: human/`--json` formatting (§3.1), used by both the thin client and the
  one-shot CLI so they render identically without either pulling the heavy stack.
- **`jurisearch-client`**: the thin CLI — depends only on the three base crates above + a
  `TransportClient`. This is the design answer to "thin build is an extraction, not a flag": a *separate
  crate with a separate dependency cone*, so it cannot accidentally link storage/embed.
- **`jurisearch-query`**: the read-only query service — the `SiteDispatcher`, the `OperationHandler`s,
  the `Embedder` seam, the snapshot-scoped query flow. Each handler opens `ctx.store.begin_snapshot()`
  and calls the **extracted response builder** (below), never the side-effecting CLI `*_payload`
  functions.
- **`jurisearch-storage`**: gains `StorageBackend` (self-managed + shared-server impls), the segregated
  `QueryStore`/`CorpusWriter`, the single `ActiveCorpusResolver`, and the readiness model.
- **`jurisearch-syncd`**: the daemon as policy over `Clock`/`PackageSource`/`TrustVerifier`/`CorpusWriter`.
- The existing heavy `jurisearch-cli` remains the local/admin/producer-side binary; it composes the same
  abstractions for local use, so there is **no second copy** of any handler or renderer.

Composition roots (the three binaries) are the *only* places concretions meet interfaces — wiring a
`SharedServerBackend`, a `LocalBgeM3Embedder`, a filesystem `PackageSource`, etc.

---

## 6. DRY ledger — the single authorities

| Concept | One authority | Consumers |
|---|---|---|
| Wire envelope + operation vocabulary | `jurisearch-contract` (§3.1) | thin client, query service |
| Per-operation request DTOs + defaults/validation | `jurisearch-contract` (typed `RequestDto`, §3.1) | thin client (clap→DTO), `SiteDispatcher`/handlers (parse+validate), response builder |
| Session/transport errors (bad command, invalid args, not-found) | `ErrorObject` (jurisearch-core) | dispatcher, handlers, codec |
| Response rendering (human + `--json`) | `jurisearch-render` (§3.1) | thin client, one-shot CLI |
| Query response building | an extracted per-operation **response builder** taking `(validated args, ReadSnapshot, Embedder)` → response body | site `OperationHandler`s **and** the CLI `*_payload` adapters |
| JSONL codec (encode/decode + framing + version) | `jurisearch-transport` (§3.8) | server accept loop, client `JsonlClient` |
| Active-generation resolution | `ActiveCorpusResolver` (§3.3) | read snapshot, corpus writer |
| Readiness model + coverage | `ReadinessStamp` + `compute_coverage` (§3.4) | writer (produce), reader (lookup) |
| Embedding fingerprint + compat check | `ensure_compatible` (§3.5) | preflight, readiness, writer |
| Generation activation | `activate_generation_with_guard` (work/08) | corpus writer only |
| Read-role visibility at activation | the activation path in `CorpusWriter` (postcondition: read role sees the new active topology, else apply fails) | query service read identity |
| Per-corpus indexed retrieval | `run_indexed_search` (§4) | every corpus arm |
| Reject vocabulary (package verify / apply / fingerprint-preflight ONLY — not command/arg parsing) | the §6.3 `Reject` codes (work/08) | writer, verifier, preflight |

No concept appears twice; the server↔client and reader↔writer boundaries are crossed by *shared
authorities*, not by parallel definitions.

## 7. SOLID ledger — where each principle lives

- **SRP** — §2: every component has one reason to change; transport ≠ dispatch ≠ handler ≠ store ≠
  embedder ≠ writer ≠ loop.
- **OCP** — §3.7: the dispatch **loop** is closed to modification (handlers register per `Operation`);
  new corpora flow through the resolver; new package transports implement `PackageSource`. (Adding a
  *new* operation is still an explicit `Operation` wire-contract change + version bump — only the loop,
  not the contract, is closed.)
- **LSP** — §3.2/§3.5: `ManagedPostgresBackend` and `SharedServerBackend` are interchangeable behind
  `StorageBackend`; the local embedder and a test stub behind `Embedder`. Substitution never changes
  caller correctness.
- **ISP** — §3.2: `QueryStore` (read) and `CorpusWriter` (write) are **disjoint**; the query service
  never sees a write method, the writer never sees a serve method. The thin client sees only
  `TransportClient` + contract.
- **DIP** — §3/§5: policy (handlers, the sync loop) depends on traits; concretions (PG, llama, fs,
  clock) are injected at the composition roots. The crate graph points toward `jurisearch-contract`.

## 8. Out of scope (by intent)

- **No implementation plan / sequencing** — milestones live in the architecture doc's delivery
  sketch; *how to build this in what order* is a separate planning document, not here.
- **No schemas, SQL, or wire bytes** — the design fixes interfaces and responsibilities; concrete
  encodings are implementation.
- **Producer internals** — work/08 owns ingest, package building, signing; this design only *consumes*
  its `VerifiedPackage` / `Reject` / activation authorities.
