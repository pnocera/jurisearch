# Q&A — 20260627-071703

## Question

# Question: can query "readiness" move entirely to apply-time (writer-owned), with zero writes at query time?

**Read this first for full context:** the target architecture document we are iterating on lives at
`work/09-jurisearch-cli/02-target-architecture.md`. Read it — especially **§6 (the strictly
read-only query service + its "state is the writer's job" property)**, **§7 (the syncd writer /
apply-time activation)**, and **§12 (the open "Readiness ownership" item)**. Settled decisions in that
doc you should treat as fixed: the query service is strictly read-only; syncd is the *only* writer;
clients use the service by URL with no auth; embeddings are computed on-site by a local `llama.cpp`
bge-m3 service; concurrency is a bounded thread pool + blocking PG pool. Ground your answer in that
doc, and flag anything in it that this readiness question shows to be wrong or under-specified.

Then read the actual code in this repo (`/home/pierre/Work/jurisearch`) and answer from the source — do
not trust my prose or my line numbers (verify them). This is a design-validation question for a target
architecture, not a code-change request.

## Context

We are designing a target deployment (work/09) where:
- a **query service** (`jurisearch serve`) connects to a shared Postgres as a **strictly read-only**
  (SELECT-only) role and must perform **zero writes** when answering a query;
- a separate **writer** (`jurisearch-syncd`) is the *only* writer, and applies/activates corpus
  generations (the work/08 machinery: per-corpus generations behind views, `corpus_state` cursor,
  index-build-before-activation).

The problem: today the read path runs a **query-readiness gate** before retrieval that, on a cache
miss, **writes** (it persists into `index_manifest`). A strictly read-only query service cannot do
that — the first query after an activation / cache invalidation / cold cache would either fail or
force us to grant the query role write access (defeating least-privilege).

Proposed resolution: make readiness **writer-owned and produced at apply/activation time** — syncd
stamps the readiness/manifest record inside the activation swap (it just built the indexes, so it
knows the generation is ready, its fingerprint, and its index inventory) — and the query service only
**reads** that stamp at query time (comparing it to its own in-memory embedder fingerprint). I want
you to verify whether that is actually achievable against the real code.

## Where to look (verify, don't trust)

- The read-path readiness gate: `ensure_query_readiness` / `load_or_compute_query_readiness`
  (around `crates/jurisearch-cli/src/index_runtime.rs` and `crates/jurisearch-cli/src/retrieval/search.rs`),
  and `open_query_index` (the shared fetch/context/related/inspect entry).
- The actual write: the `index_manifest` `INSERT ... ON CONFLICT DO UPDATE` in
  `crates/jurisearch-storage/src/ingest_accounting/readiness.rs`.
- Activation / generation swap and what it already records:
  `crates/jurisearch-storage/src/generations.rs` (`activate_generation_with_guard`, the dense manifest
  rows, `corpus_state`), and whether `index_manifest` is global vs per-generation/per-corpus.

## Questions

1. **What exactly does the current readiness path read, compute, and persist?** Trace it precisely:
   which values get written to `index_manifest`, and under exactly which conditions (cache miss,
   invalidation, cold cache). Is the write the *only* write on the read path, or are there others
   (e.g. cached query-readiness, dense-probe metadata)?
2. **Classify each readiness input** as either (a) a property of the activated generation knowable by
   the writer at apply/activation time (indexes built, embedding fingerprint, IVFFlat lists/probes,
   rowcount), or (b) something only known at query time (the request, or the server's own embedder).
3. **Is the only query-time-varying input the server's embedder fingerprint** (in-memory, needs no
   write), or is there some other lazily-computed-and-cached value that would force a write on first
   query?
4. **Can syncd stamp the readiness record inside the activation transaction** so the query service only
   reads it? Any obstacle — e.g. `index_manifest` being global rather than per-generation/per-corpus,
   or readiness keyed on something activation doesn't know?
5. **Bottom line:** can the query-time readiness check be made strictly read-only (zero writes) with
   readiness produced at apply-time? If yes, what's the minimal shape of that change. If no, identify
   the exact input that makes it impossible.

Please answer with specifics and `file:line` references, and end with a clear verdict:
**FEASIBLE (readiness can be fully writer-owned/apply-time, query-time is read-only)** or
**NOT FEASIBLE (something forces a query-time write)**, with the reason.

## Answer

# Can query readiness move entirely to apply time?

## Verdict

**FEASIBLE (readiness can be fully writer-owned/apply-time, query-time is read-only).**

The current query-time write is an implementation artifact: `load_or_compute_query_readiness` lazily caches generation coverage in the global `index_manifest` row `key = 'query_readiness'`. The inputs it computes are properties of the active read topology and the activated generation, so syncd can produce the same stamp during activation. The only target query-time compatibility input is the query service's own embedder/storage fingerprint, and that can be compared in memory/read-only against writer-stamped metadata.

There are two design caveats to record in the target doc:

1. The main chunk search path currently does **not** implement the §6 fingerprint preflight. It computes a query embedding and passes its fingerprint into dense SQL, which filters by `ce.embedding_fingerprint`, but a mismatch degrades to no dense matches rather than producing the clear compatibility error required by `work/09-jurisearch-cli/02-target-architecture.md:147-156`.
2. `index_manifest` is global, not per-corpus/per-generation. That is workable for the current single aggregate active-read signature, but it is under-specified for the doc's open multi-corpus question at `work/09-jurisearch-cli/02-target-architecture.md:324-325`.

## What the current readiness path does

The target architecture requires a strictly read-only service whose query-readiness/index-state bookkeeping is writer-owned, not a query side effect (`work/09-jurisearch-cli/02-target-architecture.md:136-140`). Today, the read path violates that on a ready cache miss.

Entry points:

- Non-search read commands use `open_query_index`, which opens the index and immediately calls `ensure_query_readiness` (`crates/jurisearch-cli/src/index_runtime.rs:48-55`). Fetch, context, related, cite, compare, inspect, versions, and diff all route through this or equivalent gates (`crates/jurisearch-cli/src/retrieval/fetch.rs:30`, `crates/jurisearch-cli/src/retrieval/context.rs:20`, `crates/jurisearch-cli/src/retrieval/related.rs:26`, `crates/jurisearch-cli/src/retrieval/cite.rs:22`, `crates/jurisearch-cli/src/retrieval/compare.rs:28`, `crates/jurisearch-cli/src/status.rs:313`, `crates/jurisearch-cli/src/status.rs:327`, `crates/jurisearch-cli/src/status.rs:349`).
- Search has its own open sequence. `search_payload` opens the index (`crates/jurisearch-cli/src/retrieval/search.rs:87-91`), then `search_with_postgres` calls `ensure_query_readiness` when `verify_readiness` is true (`crates/jurisearch-cli/src/retrieval/search.rs:175-197`). Dense/hybrid search uses `QueryReadinessGate::Search`; BM25 uses `SearchLexical`.

`ensure_query_readiness` loads readiness, then enforces gates (`crates/jurisearch-cli/src/index_runtime.rs:97-137`):

- Projection coverage must be complete for every gate.
- Fetch and lexical search return after projection is complete (`crates/jurisearch-cli/src/index_runtime.rs:118-123`).
- Dense/hybrid search additionally requires embedding coverage complete (`crates/jurisearch-cli/src/index_runtime.rs:125-133`).

The storage implementation does the real work:

- It computes an active-read signature from `jurisearch_control.corpus_state` as `corpus:active_generation:sequence`, ordered by corpus, or `public` if no corpus is installed (`crates/jurisearch-storage/src/ingest_accounting/readiness.rs:28-44`).
- It sets the connection `search_path` to the active generation, or to `jurisearch_server, public` for multi-corpus reads (`crates/jurisearch-storage/src/ingest_accounting/readiness.rs:46-74`).
- It reads `index_manifest WHERE key = 'query_readiness'` (`crates/jurisearch-storage/src/ingest_accounting/readiness.rs:112-115`, `crates/jurisearch-storage/src/ingest_accounting/readiness.rs:207-217`).
- A cache hit is accepted only if the JSON parses as `CachedReadiness` and the cached signature equals the current active signature (`crates/jurisearch-storage/src/ingest_accounting/readiness.rs:207-217`).
- On a miss, malformed row, or signature mismatch, it computes projection and embedding coverage (`crates/jurisearch-storage/src/ingest_accounting/readiness.rs:219-224`).
- It writes only if both coverage metrics are complete (`crates/jurisearch-storage/src/ingest_accounting/readiness.rs:223-240`).

The exact persisted value is:

```json
{
  "signature": "<active-read-signature>",
  "report": {
    "projection_coverage": { "covered": <documents-with-chunks>, "total": <documents>, "percentage": <...> },
    "embedding_coverage": { "covered": <chunks-with-matching-embedding-fingerprint>, "total": <chunks>, "percentage": <...> }
  }
}
```

It is stored in `index_manifest(key, value, updated_at)` under the single key `query_readiness` by `INSERT ... ON CONFLICT DO UPDATE` (`crates/jurisearch-storage/src/ingest_accounting/readiness.rs:226-239`). The table itself is global: `key text PRIMARY KEY`, `value jsonb`, `updated_at` (`crates/jurisearch-storage/src/migrations.rs:100-104`), and generation DDL explicitly classifies `index_manifest` as a non-generation table (`crates/jurisearch-storage/src/generations.rs:17-20`, `crates/jurisearch-storage/src/generations.rs:53-62`).

The coverage queries are pure generation facts:

- Projection coverage: `count(DISTINCT documents.document_id)` and documents having at least one chunk (`crates/jurisearch-storage/src/ingest_accounting/readiness.rs:249-269`).
- Embedding coverage: all chunks, and chunks whose parent `chunks.embedding_fingerprint` is non-null and has a `chunk_embeddings` row with the same fingerprint (`crates/jurisearch-storage/src/ingest_accounting/readiness.rs:271-297`).

Conditions that cause the query-time write today:

- Cold cache on a fully ready index: first query computes both metrics and writes `query_readiness`.
- Activation/corpus switch: the old row's signature no longer matches, so the first query recomputes and writes the new signature if fully ready. The regression test demonstrates that a `public` signature cache cannot authorize an active generation (`crates/jurisearch-storage/tests/generations.rs:527-575`).
- Explicit invalidation: writer/ingest paths delete the cache, and the next fully ready query writes it back (`crates/jurisearch-storage/src/ingest_accounting/readiness.rs:172-190`). Invalidation is called from ingest start (`crates/jurisearch-storage/src/ingest_accounting/runs.rs:93-96`), chunk embedding (`crates/jurisearch-cli/src/ingest/pipeline.rs:491-493`), and LEGI hierarchy backfill (`crates/jurisearch-cli/src/ingest/legi.rs:790-793`).

Conditions that do **not** write:

- Matching cache hit: returns cached report (`crates/jurisearch-storage/src/ingest_accounting/readiness.rs:207-217`).
- Cache miss where either projection or embedding coverage is incomplete: computes and returns the report, but does not store it (`crates/jurisearch-storage/src/ingest_accounting/readiness.rs:219-241`). This means fetch/BM25 can currently succeed with complete projection but incomplete embeddings, but they will recompute on every query until embeddings are complete.

I did not find another persistent write in the normal retrieval path. Dense retrieval reads `index_manifest` for built-time `default_probes` (`crates/jurisearch-storage/src/retrieval/sql.rs:16-36`, `crates/jurisearch-storage/src/retrieval/hybrid.rs:19-23`) and emits `SET ivfflat.probes`, which is session state, not persistent DML. The current retrieval modules do not contain `INSERT`, `UPDATE`, or `DELETE` on the query path.

## Classification of readiness inputs

Writer/apply-time generation properties:

- Active generation and sequence: written by activation into `jurisearch_control.corpus_state` (`crates/jurisearch-storage/src/generations.rs:1080-1108`).
- Active-read signature: derived from `corpus_state`, hence writer-owned (`crates/jurisearch-storage/src/ingest_accounting/readiness.rs:28-44`).
- Projection coverage: document/chunk relationship in the activated generation (`crates/jurisearch-storage/src/ingest_accounting/readiness.rs:249-269`).
- Embedding coverage: chunk/chunk_embedding rows and matching stored fingerprint in the activated generation (`crates/jurisearch-storage/src/ingest_accounting/readiness.rs:271-297`).
- Embedding fingerprint the generation was built for: carried in the signed package compatibility block (`crates/jurisearch-package/src/manifest/embedded.rs:46-67`) and recorded in `corpus_state` on activation (`crates/jurisearch-storage/src/generations.rs:1080-1108`).
- Index inventory: `build_generation_indexes` builds constraints, BM25/non-IVFFlat indexes, IVFFlat indexes, and analyzes generation tables before activation (`crates/jurisearch-storage/src/generations.rs:217-236`, `crates/jurisearch-storage/src/generations.rs:358-395`).
- IVFFlat `lists` and default `probes`: the signed manifest declares them (`crates/jurisearch-package/src/manifest/embedded.rs:161-183`), syncd validates actual index shape and probes (`crates/jurisearch-syncd/src/apply.rs:358-448`), and activation already writes dense manifest rows inside the switch transaction (`crates/jurisearch-storage/src/generations.rs:409-477`, `crates/jurisearch-storage/src/generations.rs:1110-1129`).
- Row counts/digests: syncd validates manifest postconditions against the loaded generation before switching (`crates/jurisearch-syncd/src/apply.rs:728-758`).

Query-time inputs:

- The request and gate: fetch/BM25/dense determine whether projection-only or embedding coverage is needed (`crates/jurisearch-cli/src/index_runtime.rs:118-133`, `crates/jurisearch-cli/src/retrieval/search.rs:190-197`).
- Query text and query embedding literal: computed per request when dense/hybrid is used (`crates/jurisearch-cli/src/retrieval/search.rs:301-319`).
- The query service's configured/local embedder fingerprint: `PreparedQueryEmbedder::from_env` derives `storage_fingerprint`, and `embed` returns it with the query vector (`crates/jurisearch-cli/src/embedding_runtime/mod.rs:25-50`).
- Optional request override for `--probes`: validated at the request boundary and overrides the stored default, but it is not persisted (`crates/jurisearch-cli/src/query_support.rs:35-41`, `crates/jurisearch-storage/src/retrieval/types.rs:175-187`).

So yes: for the target readiness/compatibility preflight, the only input that is genuinely known only by the query service is its own embedder fingerprint. Other per-query values are request execution parameters and do not require a persistent cache.

## Can syncd stamp readiness in activation?

Yes. The current apply path is already structured for this.

The target doc says syncd verifies and applies in a cursor-gated transaction (`work/09-jurisearch-cli/02-target-architecture.md:182-185`) and builds indexes before activation, then repoints views and advances the cursor atomically (`work/09-jurisearch-cli/02-target-architecture.md:186-189`). The source matches that intent:

- `apply_media_package` documents the sequence as verify, load generation, build indexes, validate contract/postconditions, then atomic switch; it states the only globally visible mutation is `activate_generation_with_guard` (`crates/jurisearch-syncd/src/apply.rs:108-112`).
- The locked apply body copies payload, builds generation indexes, validates index contract, assembles dense manifests, validates postconditions, builds `ActivationStamps`, and calls `activate_generation_with_guard` (`crates/jurisearch-syncd/src/apply.rs:219-264`).
- `activate_generation_with_guard` is already the short transaction that retires old active generation, activates the new one, writes `corpus_state`, writes dense `index_manifest` rows, rebuilds stable views, and commits (`crates/jurisearch-storage/src/generations.rs:947-954`, `crates/jurisearch-storage/src/generations.rs:1060-1132`).

The minimal shape is:

1. Add a storage helper that can compute/readiness-stamp against a transaction, not by opening a new connection. The current `store_query_readiness` opens its own connection and derives the signature from the current committed state (`crates/jurisearch-storage/src/ingest_accounting/readiness.rs:147-169`), so it is not suitable for an atomic activation swap.
2. Inside `activate_generation_with_guard`, compute the same `IngestReadinessReport` for the new active topology and write `index_manifest['query_readiness']` in the activation transaction. If the helper uses the stable `jurisearch_server` views for multi-corpus mode, call it after `rebuild_server_views(&mut tx)?` and before `tx.commit()` (`crates/jurisearch-storage/src/generations.rs:1131-1132`). Alternatively compute directly over the physical schemas implied by the new `corpus_state`.
3. Store the readiness stamp even though the query path will only read it. In the target syncd path, activation should reject or fail if the report is not acceptable for the target service. If preserving today's projection-only fetch/BM25 behavior matters, the stamp format should allow partial readiness instead of treating row presence as "fully ready".
4. Replace query-time `load_or_compute_query_readiness` with a read-only resolver: read `index_manifest['query_readiness']`, require signature match, and apply the existing gate logic. On missing/malformed/stale stamp, return a clear "writer readiness stamp missing/stale for active generation" error rather than recomputing.
5. Add the main-search embedding-compatibility preflight required by §6. Read the active generation fingerprint from the readiness stamp or `corpus_state`, compare it to `PreparedQueryEmbedder.storage_fingerprint`, and reject dense/hybrid requests before running dense SQL if it differs. This is read-only; the server fingerprint remains in memory/config.

## Obstacles and underspecified points

No input makes query-time writes inherently necessary.

The real obstacles are schema/contract decisions, not physics:

- **Global `index_manifest`.** The current `query_readiness` row is global and signature-scoped, not per-generation. That can authorize the aggregate active topology, but it is not the right final shape if work/09 chooses per-corpus endpoints or mixed corpus compatibility. The same concern already exists for dense `embedding`/`zone_embedding` manifest rows; the code comments explicitly say dense manifest keys are still global and per-corpus dense isolation is deferred (`crates/jurisearch-storage/src/generations.rs:891-896`).
- **Partial readiness semantics.** Current cache presence means fully ready, but current gate behavior allows fetch/BM25 with only projection coverage complete. A read-only query service needs either a writer-stamped partial report or a target invariant that active syncd generations are always dense-ready.
- **Fingerprint preflight missing in main search.** Zone search has an explicit fingerprint readiness check (`crates/jurisearch-cli/src/retrieval/zone.rs:5-48`), but main chunk search only passes the query fingerprint into dense SQL (`crates/jurisearch-storage/src/retrieval/sql.rs:94-124`, `crates/jurisearch-storage/src/retrieval/sql.rs:205-224`). The target doc is correct to require the preflight, but it is not present in this path today.
- **Activation ordering for multi-corpus readiness.** If readiness is computed with `apply_read_search_path`, doing it after `corpus_state` update but before `rebuild_server_views` would be wrong for multi-corpus because the helper uses `jurisearch_server` views when more than one corpus is active (`crates/jurisearch-storage/src/ingest_accounting/readiness.rs:59-69`). Compute after view rebuild, or compute from explicit physical schemas.

## Bottom line

The read path's current `INSERT ... ON CONFLICT DO UPDATE` is the only persistent query-time readiness write, and it can be removed. All data it writes is derivable from writer-owned generation state and activation metadata. syncd already builds and validates indexes before activation, records the active generation/fingerprint in `corpus_state`, and has an activation transaction that writes global dense manifest rows atomically with the switch. Extending that transaction to write a writer-owned `query_readiness` stamp is achievable.

The target architecture should be tightened to say:

- query service readiness is a read-only lookup of a writer-stamped readiness record keyed/scoped to the active topology;
- a missing/stale readiness stamp is a writer/apply failure, not a query-time recompute trigger;
- dense/hybrid requests compare the server embedder fingerprint to the active generation fingerprint before retrieval;
- the readiness/embedding metadata keying strategy must be resolved with the multi-corpus exposure decision.

**FEASIBLE (readiness can be fully writer-owned/apply-time, query-time is read-only).**
