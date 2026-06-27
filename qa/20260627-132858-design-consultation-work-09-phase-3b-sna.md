# Phase 3B Design Consultation

**Verdict: GO with adjustments.**

The proposed direction matches the actual source and the committed P3A state: readiness is now writer-stamped for installed corpora, but the read path still uses per-call `execute_read_sql` shell sessions, plus fresh libpq clients for the hybrid fingerprint preflight. 3B should therefore focus on one held read transaction and moving every query-facing storage read and helper read behind that handle. Do not introduce typed `Hits`/`Doc` fan-out yet.

## 1. Snapshot Shape

`BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY` on one owned `postgres::Client`, followed by `SET LOCAL search_path`, is the right practical shape for 3B. It avoids the self-referential lifetime problem you would hit if the snapshot struct tried to own both `Client` and `postgres::Transaction<'_>`, while still giving one transaction and one MVCC snapshot.

Adjustment: prefer a `&mut self` read primitive if you can tolerate builder signatures like `&mut dyn ReadSnapshot`. That avoids `RefCell` and runtime borrow failures:

```rust
pub trait ReadSnapshot {
    fn read_text(&mut self, sql: &str) -> Result<String, StorageError>;
    fn active_corpora(&self) -> &[ActiveCorpus];
    fn readiness(&self) -> &IngestReadinessReport;
}
```

If you want to stay closer to the design's `&self` methods, `RefCell<postgres::Client>` is sound for one request, one thread, one connection. Make the snapshot explicitly non-`Send`/non-`Sync` by construction and keep borrows short.

Use `ROLLBACK` on `Drop` rather than `COMMIT`. In a read-only transaction they are equivalent for state, but rollback is the safer best-effort cleanup when errors cannot be surfaced from `Drop`.

`REPEATABLE READ` is the right isolation level. `READ COMMITTED` would let `corpus_state`/manifest/helper reads and retrieval observe different commits. `SERIALIZABLE` is not needed. Because activation does not drop active/retired generation schemas, a snapshot pinned to `jurisearch_server_<old_gen>, public` remains valid after a swap.

One important correction: for 3B query snapshots, do not silently preserve the old `>1 active corpora -> jurisearch_server, public` behavior. P3A's strict readiness lookup already rejects multi-corpus, and 3B's deferral says fan-out is 3C. Let the resolver return multiple corpora, but have query snapshot open fail for `len > 1` until 3C.

## 2. Scope And Required Read Functions

JSON passthrough is the right 3B slice. Keep the SQL strings and JSON shapes unchanged, replace only the connection/search-path mechanism, and defer typed `Hits`/`Doc` methods plus multi-corpus fan-out to 3C/P4.

The minimum snapshot-bound set is not just the top-level `*_json` functions. Anything a builder calls that reads database state must use the same snapshot:

1. Main search: `hybrid_candidates_json`, `resolve_legi_citation_json`, `manifest_default_probes`, and the P3A embedding-fingerprint preflight. The fingerprint preflight should read `snapshot.active_corpora()[0].embedding_fingerprint`, not open a fresh `ManagedPostgres::client()`.
2. Compare: the three `hybrid_candidates_json` calls must reuse one snapshot, not one snapshot per mode.
3. Fetch/context/related/cite: `fetch_documents_json`, `context_documents_json`, `related_neighbours_json`, and `citation_lookup_json`.
4. Inspect/versions/diff/stats if those remain session/query-service operations in 3B: `inspect_document_json`, `document_versions_json`, `document_diff_json`, `corpus_stats_json`.
5. `fetch --part`: online Judilibre enrichment belongs in the CLI adapter, but the cached-zone read `decision_zones_json` is still a database read. Either move the offline cached read onto the snapshot, or explicitly exclude `fetch --part` from the 3B snapshot invariant.
6. `search --zone`: either move `zone_candidates_json`, `zone_retrieval_coverage_json`, and the zone `manifest_default_probes` path onto the snapshot, or explicitly leave zone search as a legacy adapter path for 3B. Do not half-move search and leave zone hidden behind fresh `execute_read_sql`.

Safe deferrals: eval/gold helpers in `france_legi.rs` and `france_juris.rs`, ingestion/enrichment candidate scans, `zone_resolver_reachable_json`, and other operator/status diagnostics unless you explicitly put `status --deep` into the 3B builder surface.

Also preserve current `psql -qAt` output semantics when replacing it with libpq: one row, first column, `NULL` as empty string where current callers rely on parse failure fallback, and trimmed text. This matters for helpers such as `manifest_default_probes`, where a missing JSON field currently becomes an empty string and falls back to defaults.

## 3. Builder Crate Placement

Do not make the future query service depend on `jurisearch-cli`. That would drag CLI arg/env/startup concerns in the wrong direction and undo the builder extraction.

Create a very thin `jurisearch-query` crate now, or at least a non-CLI crate/module boundary that can become it without moving public APIs again. The crate should own:

1. Side-effect-free builders.
2. The command-specific readiness gate helper.
3. The small query embedder trait.
4. Error mapping to `ErrorObject`.

This is less churn than it looks because `ErrorObject` is already in `jurisearch-core`; it is not CLI-owned. `serde_json::Value` is just `serde_json`. The CLI-owned pieces are convenience constructors such as `index_not_query_ready`, `dependency_unavailable`, and `no_results`; move or mirror those so P4 can reuse byte-identical responses without depending on CLI.

The builder inputs should not be Clap request structs long-term. For 3B, it is acceptable for CLI adapters to convert the current request structs into small builder input structs, while full contract `RequestDto` coverage can still wait if Phase 1 deferred it.

## 4. Readiness Gate

Open the snapshot by resolving the active corpus and loading the writer-owned readiness stamp once. Do not apply the full command gate unconditionally at snapshot open, because `fetch` and lexical search only need projection coverage while dense/hybrid search needs embedding coverage.

Recommended shape:

```rust
let mut snapshot = store.begin_snapshot()?;
ensure_snapshot_ready(&snapshot, QueryReadinessGate::Search)?;
build_search(req, &mut *snapshot, embedder)
```

Put `ensure_snapshot_ready` outside CLI, in the query/builder layer, so the service does not reimplement it. Move the existing `index_not_query_ready` message construction unchanged so byte parity is preserved.

For the local producer/public topology, keep the P3A legacy compute-on-read seam, but make it an explicit local-store behavior. A shared read-role `QueryStore` should never compute or write readiness. A self-managed local `QueryStore` may run the legacy `resolve_query_readiness`/public fallback before or during snapshot setup to preserve existing CLI behavior.

## 5. Error And Vocabulary Scope

Add `ActiveCorpus` now. The snapshot needs a first-class resolved value anyway: corpus, generation, physical schema, sequence, and embedding fingerprint. This should live in storage with a generic-client resolver and replace the duplicated `execute_read_sql` resolver logic.

Reuse `IngestReadinessReport` for 3B rather than inventing a separate `ReadinessStamp` type immediately. If the signature needs to be exposed, expose a small wrapper around the existing cached-readiness lookup rather than adding a second readiness model.

Reuse `StorageError` at the storage/snapshot layer for 3B. Map to `ErrorObject` in the query builder layer. A richer `QueryError` can wait for P4 unless the new `jurisearch-query` crate needs a narrow public error type to avoid leaking storage internals.

## 6. Deterministic Concurrent-Swap Test

No sleeps are needed.

1. Apply/activate generation A with a sentinel document value.
2. Open a read snapshot. Ensure `begin_snapshot()` performs at least one resolver/readiness query inside `REPEATABLE READ`; that establishes the MVCC snapshot deterministically.
3. On another writer connection, apply/activate generation B with a changed sentinel value.
4. Read through the already-open snapshot and assert it still sees generation A's schema/data.
5. Open a new snapshot and assert it sees generation B.

This proves both parts of the invariant: the physical search path is pinned to the old active generation, and a new request sees the new active topology. You can add a lighter assertion on `current_schema()` or the stored `ActiveCorpus.schema`, but also reading a changed document value is the stronger proof.

## 7. Other 3B Risks To Handle Before Coding

The active-corpus resolver must become a shared storage authority. Right now `execute_read_sql` has one resolver, P3A readiness has a private `active_read_signature`, and hybrid fingerprint preflight has its own `corpus_state` query. 3B should not add a fourth copy.

`open_query_index` currently returns `ManagedPostgres` after running readiness. Refactor it into "open local store" plus "begin snapshot" rather than keeping readiness as a pre-query side effect. Otherwise the builder extraction will still have hidden startup/readiness work in the adapter path.

Search routing must use one snapshot for structured-citation resolution and the hybrid fallback. The current `SearchExecution` stores `&ManagedPostgres`; after 3B it should store the snapshot reference, so the structured miss and fallback cannot split across a swap.

Embedding runtime construction should stay adapter-side. Builders should take a trait like `QueryEmbedder` and call it only when dense retrieval is used. Do not let builders read env vars or construct `PreparedQueryEmbedder::from_env()`.

Status/deep diagnostics are not automatically part of the query snapshot invariant. Decide explicitly whether `status`, `stats`, and zone status blocks are in the 3B query-builder surface. If they are not, leave them as CLI/operator legacy paths and do not use them as evidence that 3B has achieved request-snapshot coverage.

Net: implement 3B as a narrow snapshot executor plus JSON-preserving signature refactor, but be strict about helper reads. The design is sound if every database read reachable from an extracted builder goes through the same `ReadSnapshot`; it is not sound if top-level functions move while helper reads continue opening fresh sessions.
