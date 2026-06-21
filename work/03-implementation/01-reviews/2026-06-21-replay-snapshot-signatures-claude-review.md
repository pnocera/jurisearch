# Claude Review: Replay Snapshot Signatures

Verdict: FIXES_REQUIRED

## Findings

- **Severity: High (performance / query-readiness)** — `crates/jurisearch-storage/src/ingest_accounting.rs:527`
  (`load_replay_snapshot` invoked from `load_ingest_health`).
  - **Issue:** `load_ingest_health` now unconditionally computes the full replay
    snapshot — five full-table `md5(concat_ws(...))` + `string_agg(... ORDER BY ...)`
    aggregations over `documents`, `chunks`, `graph_edges`, `chunk_embeddings`,
    `index_manifest`, plus a sixth `md5` signature query. The `chunk_embeddings`
    component hashes `embedding::text` (the full `vector(1024)` serialized to text,
    ~10–20 KB/row) for every chunk, and the `documents`/`chunks` components hash the
    full `body`/`canonical_json`/`source_fields` of every row. Before this commit
    (`a9c7333:ingest_accounting.rs:543`) the function just set
    `replay_snapshot_status: "pending"` with no extra queries.
  - **Impact:** `load_ingest_health` is on the per-query hot path:
    `ensure_query_readiness` (`crates/jurisearch-cli/src/main.rs:1411`) calls it for
    **every** `search` (`main.rs:402`) and `fetch` (`main.rs:457`), yet the gate only
    consumes `projection_coverage` / `embedding_coverage`. Every query now pays a
    full-corpus content hash (sort + per-row md5 of large text/vector columns) that is
    irrelevant to the readiness decision. Cost grows linearly with corpus size; against
    the Phase 1 target (full LEGI, millions of articles/chunks) this adds large,
    unbounded, repeated latency to the core search feature. This is a silent regression
    introduced by wiring a diagnostic, point-in-time artifact into the query gate.
  - **Required fix:** Decouple replay-snapshot computation from the readiness path.
    Either compute the snapshot only where it is reported (the `status` command), e.g.
    a separate `load_replay_snapshot`/dedicated entry point, or have
    `ensure_query_readiness` use a coverage-only query that does not call
    `load_replay_snapshot`. The gate must not run full-corpus hashing per query.

## Suggestions

- **Cross-statement consistency (determinism):** the five component snapshots and the
  combined signature run as six independent `query_one` calls on one connection with no
  wrapping transaction (`ingest_accounting.rs:576-648`). Each statement is internally
  consistent (single READ COMMITTED snapshot), but across the six there is no shared
  snapshot, so a concurrent ingest could make the combined signature reflect a state
  that never existed at a single instant. For an artifact whose stated purpose is
  "deterministic replay-drift diffs," consider wrapping the component queries in a
  `REPEATABLE READ` transaction (or a single CTE/query) so all components share one
  snapshot. In practice snapshots are taken post-ingest, so this is non-blocking today.
- **NULL vs empty-string collision:** nullable columns are `coalesce(col, '')` before
  hashing (`ingest_accounting.rs:583-621`). This correctly preserves `concat_ws`
  positional integrity, but it makes a transition between `NULL` and `''` (e.g.
  `title`, `valid_to_raw`, `source_url`, `embedding_fingerprint`) invisible to the
  signature. If that distinction ever matters for drift detection, use a sentinel that
  cannot appear in data (e.g. `coalesce(col, '\x00')`) or hash a NULL-flag separately.
- **Avoid the round-trip for the top-level signature:** `signature_input` is built in
  Rust and then sent back to Postgres via `SELECT md5($1)`
  (`ingest_accounting.rs:623-639`). Hashing it in Rust (the crate already depends on a
  hashing facility for fingerprints) would remove one network round-trip; minor.

## Verification Notes

- Read the commit (`git show 0b52548`, full diff) and the live files:
  `crates/jurisearch-storage/src/ingest_accounting.rs` (struct defs ~120-176,
  `load_ingest_health` 504-574, `load_replay_snapshot`/`snapshot_component` 576-668) and
  `crates/jurisearch-cli/src/main.rs` (`ingest_health_payload` 1355-1376,
  `ensure_query_readiness` 1407-1437, search 390-440, fetch 442-459).
- Validated every column/type referenced by the snapshot SQL against
  `crates/jurisearch-storage/src/migrations.rs` (documents 27-44, chunks 46-58,
  chunk_embeddings 60-67, graph_edges 69-77, index_manifest 79-83). All columns exist;
  nullable columns are correctly `coalesce`d; NOT NULL columns (document_id, source,
  kind, source_uid, body, source_payload_hash, canonical_json, chunk_*, edge_kind,
  edge_source, payload, embedding, model, dimension, key, value) are used directly.
- Determinism of each component is sound: `string_agg(row_hash, E'\n' ORDER BY row_key)`
  orders by a PRIMARY KEY (unique → total order); empty tables `coalesce(..., '')` to a
  stable `md5('')`; all JSON/vector columns are `jsonb`/`vector` cast to their canonical
  `::text` form, so per-row hashes are stable for identical content. The storage test
  (`tests/ingest_accounting.rs:227-247`) meaningfully checks stability across two calls
  and a signature change after a `documents.title` update.
- SQL-injection safety: `snapshot_component` interpolates only hardcoded literals
  (`component_name`, `rows_sql`) from `load_replay_snapshot`; no user input reaches
  `format!`. The signature query is parameterized. No injection surface.
- `replay_snapshot_status` "empty"/"available" logic (528-536) correctly excludes
  `manifests` from the emptiness check (the schema seed row keeps `index_manifest`
  non-empty), so a fresh index with no ingested content still reports "empty".
- Status JSON serialization: `ingest_health_payload` serializes the whole
  `IngestHealthReport` via serde, so `replay_snapshot_status` + `replay_snapshot` are
  emitted; CLI contract assertions (`tests/cli_contract.rs:229-247`) match. The
  serialize-failure fallback omits the new keys, but that branch is unreachable for
  these plain `Serialize` types.
- Plan accuracy: `IMPLEMENTATION_PLAN.md` Phase 1.0 status now lists the replay snapshot
  component set (documents, chunks, publisher edges, chunk embeddings, manifests) as
  Done and drops the "pending placeholder" item from Remaining — this matches the code.
  The plan does not note the per-query cost the snapshot adds to the readiness gate.
- I did not re-run the listed verifications (`cargo test`/`clippy` require managed
  Postgres for the integration tests); I relied on the stated pre-review runs and
  inspected the code/tests directly. The blocking finding is a design/perf issue not
  surfaced by those green checks because the test corpus is a tiny slice.
