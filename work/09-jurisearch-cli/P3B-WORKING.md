# P3B working notes — codex design GO-with-adjustments (qa/20260627-132858)

Implement 3B as a **narrow snapshot executor + JSON-preserving signature refactor**. Sound IFF every DB
read reachable from an extracted builder goes through the SAME `ReadSnapshot`; unsound if top-level fns
move but helper reads keep opening fresh sessions.

## Binding adjustments (apply before/while coding)

1. **Snapshot shape.** `BEGIN ISOLATION LEVEL REPEATABLE READ READ ONLY` on ONE owned `postgres::Client`,
   then `SET LOCAL search_path`. Prefer a `&mut self` read primitive (`read_text(&mut self, sql) ->
   Result<String, StorageError>`) over `RefCell` → builders take `&mut dyn ReadSnapshot`. `ROLLBACK` on
   Drop (read-only). REPEATABLE READ confirmed. Make snapshot non-Send/non-Sync.
2. **Multi-corpus.** Resolver returns N corpora, but `begin_snapshot` FAILS for len>1 until 3C (don't
   silently keep the old `jurisearch_server` union for query snapshots). len 0 → `public` (local only);
   len 1 → `jurisearch_server_<gen>`, `public`.
3. **Scope = JSON passthrough.** Keep SQL strings + JSON shapes UNCHANGED; replace only the
   connection/search-path mechanism. Defer typed Hits/Doc + fan-out to 3C/P4.
   Snapshot-bound set (the exposed Operation surface = search/fetch/cite/related/context/compare[/status]):
   - search: `hybrid_candidates_json`, `resolve_legi_citation_json`, `manifest_default_probes`, AND the
     P3A fingerprint preflight (read `snapshot.active_corpora()[0].embedding_fingerprint`, NOT a fresh
     `ManagedPostgres::client()`).
   - compare: 3 `hybrid_candidates_json` calls reuse ONE snapshot.
   - fetch/context/related/cite: `fetch_documents_json`, `context_documents_json`,
     `related_neighbours_json`, `citation_lookup_json`.
   - `fetch --part`: online Judilibre enrichment stays in CLI adapter; the cached `decision_zones_json`
     read → **EXCLUDE `fetch --part` from the 3B snapshot invariant** (adapter-side post-process), stated
     explicitly. (Decision: don't move decision_zones onto the snapshot in 3B.)
   - `search --zone`: move `zone_candidates_json`, `zone_retrieval_coverage_json`, zone
     `manifest_default_probes` onto the snapshot (zone is part of the exposed `search` op — don't leave a
     hidden fresh-session read).
   - **Excluded from 3B query-builder surface (local-only diagnostics, leave legacy):** inspect, versions,
     diff, stats, status --deep, zone_resolver_reachable, eval/gold helpers, ingestion scans. Do NOT cite
     them as snapshot coverage.
   - Preserve `psql -qAt` semantics under libpq: one row, first column, NULL→empty string, trimmed
     (matters for `manifest_default_probes`: missing JSON field → empty string → default fallback).
4. **Builder crate.** Create a thin `jurisearch-query` crate NOW (service must NOT depend on
   jurisearch-cli). It owns: side-effect-free builders, the readiness-gate helper, the `QueryEmbedder`
   trait, error mapping → `ErrorObject`. NOTE: `ErrorObject` already lives in `jurisearch-core` (not CLI);
   `Value` is serde_json. Move/mirror CLI constructors `index_not_query_ready`/`dependency_unavailable`/
   `no_results` so P4 reuses byte-identical responses. Builder inputs = small builder-input structs (CLI
   converts its Clap structs), full RequestDto can wait.
5. **Readiness gate.** `begin_snapshot` resolves active corpus + loads the readiness stamp ONCE; do NOT
   apply the full command gate at open (fetch/lexical = projection only; dense/hybrid = +embedding).
   `ensure_snapshot_ready(&snapshot, gate)` lives in the query/builder layer (not CLI), reuses
   `index_not_query_ready` UNCHANGED. Shared read-role QueryStore NEVER computes/writes readiness; a
   self-managed LOCAL QueryStore MAY run the legacy `resolve_query_readiness`/public fallback (explicit
   local-store behavior).
6. **Vocabulary.** Add `ActiveCorpus { corpus, generation, schema, sequence, fingerprint }` NOW in
   storage with a generic-client resolver (replaces the duplicated execute_read_sql resolver). Reuse
   `IngestReadinessReport` (no new ReadinessStamp). Reuse `StorageError` at storage/snapshot; map to
   `ErrorObject` in the builder layer.
7. **Other risks.** ONE shared active-corpus resolver (today 3 copies: execute_read_sql resolver, P3A
   `active_read_signature`, hybrid preflight corpus_state query — don't add a 4th). Refactor
   `open_query_index` into "open local store" + "begin snapshot" (no readiness as a pre-query side
   effect). `SearchExecution` stores `&ManagedPostgres` → store the snapshot ref so structured-citation
   resolution + hybrid fallback can't split across a swap. Embedding runtime construction stays
   adapter-side; builders take `QueryEmbedder`, call only when dense; no env reads / no `from_env()` in
   builders.

## STATUS: implementation complete, in codex review (2026-06-27-P3B-snapshot-querystore-codex-review.md)

All green: storage 108 tests (query_snapshot_p3b concurrent-swap + multi-corpus refusal; query_readiness_p3a;
retrieval_smoke; + all 21 binaries), CLI cli_byte_parity(4)/cli_session_contract(4)/cli_status_contract(15)/
cli_retrieval_contract(24) + 70 bin goldens, package-build loopbacks (baseline/incremental/rebaseline/
shared_writer/concurrency_soak), jurisearch-query builds. fmt + workspace clippy clean. jurisearch-query
cone does NOT include jurisearch-cli (ingest is inherited via storage). Deviations disclosed in the review
(search/cite builder crate-move deferred to P4; gate stays adapter-side; multi-corpus snapshot refused).

## Implementation status (live)

- ✅ Storage `crates/jurisearch-storage/src/query.rs`: `ActiveCorpus` + `resolve_active_corpora` (single
  authority), `ReadSnapshot { read_text, active_corpora }`, `QueryStore { begin_snapshot }`,
  `LocalSnapshot` (BEGIN REPEATABLE READ READ ONLY + resolve + SET LOCAL search_path + ROLLBACK on
  drop; refuses >1 corpus). `read_text` = `simple_query` with psql -qAt semantics. `impl QueryStore for
  ManagedPostgres`. Storage lib+tests GREEN.
- ✅ In-surface storage read cores renamed `*_in_snapshot(&mut dyn ReadSnapshot, …)`:
  fetch/context/related/resolve_legi_citation/hybrid (retrieval/*), citation_lookup (citation.rs),
  zone_candidates (zone_retrieval.rs). Each KEEPS a same-named `*_json(&ManagedPostgres,…)` SHIM that
  opens a one-shot snapshot + delegates → deferred callers (eval, storage tests, local diagnostics)
  compile UNCHANGED. hybrid preflight now reads `snapshot.active_corpora()` (no fresh client).
- ⏳ NEXT: `jurisearch-query` crate — `QueryEmbedder` trait + `QueryEmbedding`; error helpers moved from
  CLI (`no_results`/`dependency_unavailable`/`index_unavailable`/`storage_error_object`/
  `parse_storage_json`) as the single authority (CLI re-exports them); builders
  `build_{fetch,context,related,cite,compare,search}(input, &mut dyn ReadSnapshot, &dyn QueryEmbedder)
  -> Result<Value, ErrorObject>`. Move the PURE helpers builders need (citation parse/classify, query
  tokenization) into the crate. CLI `*_payload` → adapters: validate + open store + gate (legacy
  `ensure_query_readiness`, pre-snapshot) + `begin_snapshot` + builder + online enrichment (cite
  --online / fetch --part stay adapter-side). `SearchExecution` holds the snapshot (not &ManagedPostgres).
- ⏳ Tests: concurrent-swap (no sleep), byte-parity goldens, single-corpus parity.

## Concurrent-swap test (no sleeps)
activate gen A (sentinel doc) → open snapshot (begin runs ≥1 query in REPEATABLE READ to fix MVCC) →
on another conn activate gen B (changed sentinel) → read via open snapshot still sees A → new snapshot
sees B. Assert on a changed document value (stronger than current_schema()).
