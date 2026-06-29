# Global Performance Review - jurisearch

## Biggest Win First

The single biggest win is to stop extracting France-LEGI evaluation gold from `graph_edges.payload` JSONB at query time. Both official-evidence SQL paths scan the 12.9M-row `graph_edges` table and expand `payload->'attributes'` per matching row, while the schema only indexes `graph_edges.from_document_id` and `graph_edges.to_document_id`. Materializing the publisher edge facts needed by temporal and cross-reference qrels into typed columns, or into a refreshed materialized view, should turn the current multi-minute JSONB scan into indexed lookups over the small subsets `source_tag = 'LIEN_ART'` and `typelien = 'CITATION' AND sens = 'cible'`.

## `crates/jurisearch-storage`

### P0 - `france_legi` gold extraction forces full `graph_edges` JSONB scans

**Location:** `crates/jurisearch-storage/src/france_legi.rs:82`, `crates/jurisearch-storage/src/france_legi.rs:89`, `crates/jurisearch-storage/src/france_legi.rs:93`, `crates/jurisearch-storage/src/france_legi.rs:135`, `crates/jurisearch-storage/src/france_legi.rs:142`, `crates/jurisearch-storage/src/france_legi.rs:143`, `crates/jurisearch-storage/src/migrations.rs:85`

`cross_reference_sql` filters `graph_edges` by `edge_source`, `payload->>'to_source_uid' LIKE 'LEGIARTI%'`, and two `EXISTS (SELECT 1 FROM jsonb_array_elements(...))` predicates over `attributes`. `temporal_sql` scans `graph_edges`, builds `jsonb_object_agg` from `jsonb_array_elements` for each candidate edge, then checks `source_tag`, `to_source_uid`, and required version attributes. The only graph-edge indexes created by the schema are `graph_edges_from_idx` and `graph_edges_to_idx`; there is no index on `edge_source`, `payload->>'source_tag'`, `payload->>'to_source_uid'`, or the attribute keys/values. At the stated scale, the planner has no selective access path for either SQL shape, so the likely cost is O(12.9M edges) plus JSONB array expansion for a large fraction of them. The reported 73s timeout is consistent with this source structure.

Concrete fix:

```sql
CREATE MATERIALIZED VIEW legi_publisher_edge_facts AS
SELECT
    e.edge_id,
    e.from_document_id,
    e.payload->>'source_tag' AS source_tag,
    e.payload->>'to_source_uid' AS to_source_uid,
    max(a->>'value') FILTER (WHERE a->>'key' = 'typelien') AS typelien,
    max(a->>'value') FILTER (WHERE a->>'key' = 'sens') AS sens,
    max(a->>'value') FILTER (WHERE a->>'key' = 'debut') AS debut,
    max(a->>'value') FILTER (WHERE a->>'key' = 'fin') AS fin,
    max(a->>'value') FILTER (WHERE a->>'key' = 'num') AS num,
    max(a->>'value') FILTER (WHERE a->>'key' = 'etat') AS etat
FROM graph_edges e
LEFT JOIN LATERAL jsonb_array_elements(coalesce(e.payload->'attributes', '[]'::jsonb)) a ON true
WHERE e.edge_source = 'publisher'
GROUP BY e.edge_id, e.from_document_id, e.payload->>'source_tag', e.payload->>'to_source_uid';

CREATE UNIQUE INDEX legi_publisher_edge_facts_edge_id_idx
ON legi_publisher_edge_facts(edge_id);

CREATE INDEX legi_publisher_edge_facts_temporal_idx
ON legi_publisher_edge_facts(from_document_id, to_source_uid)
WHERE source_tag = 'LIEN_ART'
  AND to_source_uid LIKE 'LEGIARTI%'
  AND debut IS NOT NULL
  AND fin IS NOT NULL
  AND num IS NOT NULL
  AND etat IS NOT NULL;

CREATE INDEX legi_publisher_edge_facts_xref_idx
ON legi_publisher_edge_facts(from_document_id, to_source_uid)
WHERE typelien = 'CITATION'
  AND sens = 'cible'
  AND to_source_uid LIKE 'LEGIARTI%';
```

Then rewrite `temporal_sql` and `cross_reference_sql` to read this view instead of expanding JSONB. If the same fields are needed outside eval, prefer a real table populated during ingestion with the same indexes.

This is a schema/index change and requires a migration or an explicit refresh step after ingest. Semantics are unchanged if the materialized columns are populated from the same JSONB payload. The main correctness risk is refresh staleness: the eval command must either refresh the materialized view after ingestion or verify a manifest timestamp/version before use.

### P1 - Dense ANN index is severely under-partitioned and search probes are hard-coded

**Location:** `crates/jurisearch-cli/src/main.rs:407`, `crates/jurisearch-storage/src/dense.rs:151`, `crates/jurisearch-storage/src/retrieval.rs:78`, `crates/jurisearch-storage/src/retrieval.rs:194`

The CLI default dense rebuild uses `--index-lists 32`, and retrieval always runs `SET ivfflat.probes = 4`. With about 1.85M chunk embeddings, 32 lists means roughly 58k vectors per list; 4 probes asks pgvector to inspect roughly 232k vectors before the later validity/kind filter. This is a poor tradeoff: it is much heavier than a properly partitioned IVFFlat index while still risking recall loss. The query then overfetches only `dense_limit * 4`, with `dense_limit = top_k * 4`, so the default top-10 search considers only 160 ANN candidates before temporal filtering.

Concrete fix:

```sql
DROP INDEX IF EXISTS chunk_embeddings_embedding_ivfflat_idx;
CREATE INDEX chunk_embeddings_embedding_ivfflat_idx
ON chunk_embeddings USING ivfflat (embedding vector_l2_ops)
WITH (lists = 1360);
ANALYZE chunk_embeddings;
```

Use a runtime setting based on the manifest, for example `ivfflat.probes = ceil(sqrt(lists))` for recall-sensitive evaluation, so the production index above starts around 37 probes instead of hard-coded 4. If build time and memory are acceptable, HNSW should also be tested:

```sql
CREATE INDEX chunk_embeddings_embedding_hnsw_idx
ON chunk_embeddings USING hnsw (embedding vector_l2_ops)
WITH (m = 16, ef_construction = 64);
```

Then set `hnsw.ef_search` per query profile. This is an index rebuild, not a data migration, unless the manifest format is changed to record the search parameters. Raising probes or switching index type changes retrieval quality and latency but should not change correctness semantics beyond approximate nearest-neighbor recall.

### P1 - Embedding coverage/readiness checks scan the whole corpus on interactive commands

**Location:** `crates/jurisearch-storage/src/ingest_accounting.rs:787`, `crates/jurisearch-storage/src/ingest_accounting.rs:792`, `crates/jurisearch-storage/src/ingest_accounting.rs:809`, `crates/jurisearch-storage/src/ingest_accounting.rs:816`, `crates/jurisearch-cli/src/main.rs:1133`, `crates/jurisearch-cli/src/main.rs:1138`, `crates/jurisearch-cli/src/main.rs:1144`, `crates/jurisearch-cli/src/main.rs:650`

`ensure_query_readiness` calls `load_ingest_projection_coverage`, and dense search also calls `load_ingest_embedding_coverage`. Projection coverage does `count(DISTINCT d.document_id)` over `documents LEFT JOIN chunks`; embedding coverage does `count(*)` over all `chunks LEFT JOIN chunk_embeddings`. On the production index this is a full 1.74M-document / 1.85M-chunk aggregation before the actual search. The code correctly verifies readiness only once for the France-LEGI eval sweep, but one-shot `search`, `fetch`, `cite`, and `context` still pay this cost each invocation.

Concrete fix: persist coverage into `index_manifest` at ingest/finalize time and make the query gate read one manifest row.

```sql
INSERT INTO index_manifest(key, value, updated_at)
VALUES (
  'readiness',
  jsonb_build_object(
    'projection_covered', $1,
    'projection_total', $2,
    'embedding_covered', $3,
    'embedding_total', $4,
    'embedding_fingerprint', $5
  ),
  now()
)
ON CONFLICT (key) DO UPDATE
SET value = EXCLUDED.value,
    updated_at = EXCLUDED.updated_at;
```

Keep the current count queries behind `jurisearch status --refresh` or a debug repair command. This is a schema/manifest behavior change but not a table migration. Correctness risk: a cached readiness record can become stale after manual database edits, so the manifest should include schema version, embedding fingerprint, and a refresh time or replay snapshot signature.

### P1 - Runtime profile is tuned for bulk WAL but not for the analytical workload

**Location:** `crates/jurisearch-storage/src/runtime.rs:454`, `crates/jurisearch-storage/src/runtime.rs:458`, `crates/jurisearch-storage/src/runtime.rs:460`, `crates/jurisearch-storage/src/runtime.rs:465`, `crates/jurisearch-storage/src/runtime.rs:473`

The bulk profile sets `synchronous_commit = off`, `wal_compression`, `max_wal_size`, `checkpoint_timeout`, `shared_buffers = 1GB`, and `maintenance_work_mem = 1GB`. The durable/search profile writes only preload/listen/port/socket settings and resets `synchronous_commit`. There is no durable `shared_buffers`, `effective_cache_size`, `work_mem`, `temp_buffers`, or parallel query tuning. The France-LEGI CTEs and BM25/vector candidate fusion can sort/aggregate thousands to millions of rows; default Postgres `work_mem` will spill quickly under the analytical qrel queries.

Concrete fix: add explicit profile knobs, preferably configurable by environment with conservative defaults:

```conf
shared_buffers = '2GB'
effective_cache_size = '8GB'
work_mem = '128MB'
maintenance_work_mem = '2GB'
max_parallel_workers_per_gather = 4
max_parallel_workers = 8
temp_buffers = '128MB'
```

For the gold extraction specifically, use a session-local setting around that command if global memory is a concern:

```sql
SET LOCAL work_mem = '256MB';
SET LOCAL max_parallel_workers_per_gather = 4;
```

This is pure runtime configuration. It can change resource consumption and plan choices, not result semantics.

## `crates/jurisearch-cli`

### P0 - LEGI ingestion performs per-member preparation and per-row writes instead of batch/COPY writes

**Location:** `crates/jurisearch-cli/src/main.rs:2027`, `crates/jurisearch-cli/src/main.rs:2032`, `crates/jurisearch-cli/src/main.rs:2141`, `crates/jurisearch-storage/src/projection.rs:75`, `crates/jurisearch-storage/src/projection.rs:80`, `crates/jurisearch-storage/src/projection.rs:108`, `crates/jurisearch-storage/src/projection.rs:132`, `crates/jurisearch-storage/src/projection.rs:159`, `crates/jurisearch-storage/src/projection.rs:192`, `crates/jurisearch-storage/src/projection.rs:215`

The archive ingest loop batches 128 members into one transaction, but each article member calls `insert_legi_documents_with_client(client, &[document], None)`. That function prepares the document/chunk/edge statements for each member call, then executes one statement per document, chunk, and publisher edge. At production scale this is millions of statement executions and repeated prepare calls; for 1.85M chunks plus 12.9M graph edges, the graph-edge loop alone implies about 12.9M individual inserts/upserts.

Concrete fix: change the batch layer to collect parsed `CanonicalDocument`s and metadata roots, then call storage projection once per transaction batch. Inside projection, use `COPY` into temporary staging tables followed by set-based upserts:

```sql
CREATE TEMP TABLE stage_graph_edges (
  edge_id text,
  from_document_id text,
  to_document_id text,
  edge_kind text,
  edge_source text,
  payload jsonb
) ON COMMIT DROP;

-- COPY stage_graph_edges FROM STDIN (FORMAT binary or CSV)

INSERT INTO graph_edges(edge_id, from_document_id, to_document_id, edge_kind, edge_source, payload)
SELECT edge_id, from_document_id, to_document_id, edge_kind, edge_source, payload
FROM stage_graph_edges
ON CONFLICT (edge_id) DO UPDATE
SET from_document_id = EXCLUDED.from_document_id,
    to_document_id = EXCLUDED.to_document_id,
    edge_kind = EXCLUDED.edge_kind,
    edge_source = EXCLUDED.edge_source,
    payload = EXCLUDED.payload;
```

Apply the same pattern to `documents` and `chunks`, or at minimum prepare statements once per transaction batch and pass all parsed documents to one projection call. This is implementation-only if staging tables are temporary. Semantics remain the same if conflict handling is identical.

### P1 - Embedding rebuild loads all pending chunks into memory before any work starts

**Location:** `crates/jurisearch-storage/src/dense.rs:34`, `crates/jurisearch-storage/src/dense.rs:43`, `crates/jurisearch-storage/src/dense.rs:61`, `crates/jurisearch-storage/src/dense.rs:75`, `crates/jurisearch-cli/src/main.rs:2523`, `crates/jurisearch-cli/src/main.rs:2781`, `crates/jurisearch-cli/src/main.rs:2782`, `crates/jurisearch-cli/src/main.rs:2785`

`load_chunk_embedding_inputs` returns a `Vec<ChunkEmbeddingInput>` containing every stale/missing chunk text, and `embed_and_insert_chunks_with_pool` then chunks that vector into another `VecDeque`, cloning each batch. With 1.85M chunks and contextualized text capped at 6,000 characters, the worst-case resident set can reach many GB before the first embedding request is issued. The `--limit` path deliberately refuses partial finalization, so the production path must load the full pending set.

Concrete fix: stream IDs/text in bounded pages and commit embeddings page-by-page. A simple version is a stable keyset loop:

```sql
SELECT c.chunk_id, c.body, c.contextualized_body
FROM chunks c
LEFT JOIN chunk_embeddings ce ON ce.chunk_id = c.chunk_id
WHERE (ce.chunk_id IS NULL OR ce.embedding_fingerprint <> $1 OR ce.model <> $2 OR ce.dimension <> $3)
  AND c.chunk_id > $4
ORDER BY c.chunk_id
LIMIT $5;
```

For deterministic document order, keyset on `(c.document_id, c.chunk_index, c.chunk_id)` instead. Feed each page directly into the endpoint pool. This is implementation-only and should preserve semantics. The main correctness requirement is that finalization still proves zero missing chunks after the streaming phase.

### P1 - Embedding inserts still do two database round-trips per vector

**Location:** `crates/jurisearch-cli/src/main.rs:2838`, `crates/jurisearch-cli/src/main.rs:2849`, `crates/jurisearch-storage/src/projection.rs:636`, `crates/jurisearch-storage/src/projection.rs:655`, `crates/jurisearch-storage/src/projection.rs:672`, `crates/jurisearch-storage/src/projection.rs:686`

Each successful embedding batch is inserted by `insert_chunk_embeddings`, which starts a transaction, then for every embedding runs one `UPDATE chunks` and one `INSERT INTO chunk_embeddings ... ON CONFLICT`. At 1.85M chunks this is about 3.7M statement executions, plus pgvector text parsing for every vector literal. The endpoint pool can make embedding generation concurrent, but the main thread serializes storage writes batch-by-batch.

Concrete fix: replace the per-row loop with a staging table and set-based update/upsert:

```sql
CREATE TEMP TABLE stage_chunk_embeddings (
  chunk_id text PRIMARY KEY,
  embedding_fingerprint text NOT NULL,
  embedding vector(1024) NOT NULL,
  model text NOT NULL,
  dimension integer NOT NULL
) ON COMMIT DROP;

-- COPY stage_chunk_embeddings FROM STDIN

UPDATE chunks c
SET embedding_fingerprint = s.embedding_fingerprint
FROM stage_chunk_embeddings s
WHERE c.chunk_id = s.chunk_id
  AND (c.embedding_fingerprint IS NULL OR c.embedding_fingerprint = s.embedding_fingerprint);

INSERT INTO chunk_embeddings(chunk_id, embedding_fingerprint, embedding, model, dimension)
SELECT chunk_id, embedding_fingerprint, embedding, model, dimension
FROM stage_chunk_embeddings
ON CONFLICT (chunk_id) DO UPDATE
SET embedding_fingerprint = EXCLUDED.embedding_fingerprint,
    embedding = EXCLUDED.embedding,
    model = EXCLUDED.model,
    dimension = EXCLUDED.dimension;
```

Check that the number of updated chunks equals the staging row count before inserting, preserving the current mismatch guard. This is implementation-only unless a permanent staging table is chosen.

### P1 - Full hierarchy backfill can be triggered during resume and contains nested JSONB scans

**Location:** `crates/jurisearch-cli/src/main.rs:1896`, `crates/jurisearch-cli/src/main.rs:1897`, `crates/jurisearch-storage/src/projection.rs:312`, `crates/jurisearch-storage/src/projection.rs:328`, `crates/jurisearch-storage/src/projection.rs:366`, `crates/jurisearch-storage/src/projection.rs:368`, `crates/jurisearch-storage/src/projection.rs:373`, `crates/jurisearch-storage/src/projection.rs:384`

If any compatible members are skipped during resume, ingestion switches from scoped hierarchy backfill to a full backfill. The full backfill query joins all LEGI articles to all `TEXTELR` metadata roots, expands `canonical_json->'structure_links'` for each text root, then runs two additional lateral expansions to find preceding section links and aggregate section ancestry. There are useful btree indexes on `legi_metadata_roots(root_kind, source_uid)` and `parent_source_uid`, but no index can accelerate these lateral JSONB expansions.

Concrete fix: normalize `TEXTELR.structure_links` at metadata ingestion time into a table:

```sql
CREATE TABLE legi_text_structure_links (
  metadata_key text NOT NULL REFERENCES legi_metadata_roots(metadata_key) ON DELETE CASCADE,
  ordinality integer NOT NULL,
  source_tag text NOT NULL,
  target_source_uid text,
  debut date,
  fin date,
  link jsonb NOT NULL,
  PRIMARY KEY (metadata_key, ordinality)
);

CREATE INDEX legi_text_structure_links_article_idx
ON legi_text_structure_links(target_source_uid, metadata_key, ordinality)
WHERE source_tag = 'LIEN_ART';

CREATE INDEX legi_text_structure_links_section_idx
ON legi_text_structure_links(metadata_key, ordinality DESC)
WHERE source_tag = 'LIEN_SECTION_TA';
```

Then the fallback branch becomes an indexed join from article source UID to text structure links and a bounded predecessor lookup for the section. This is a migration and ingestion projection change. Semantics should remain stable if the stored `link` JSONB is retained for exact current hierarchy logic.

### P2 - `refresh_replay_snapshot` hashes full large tables after ingest and embed finalization

**Location:** `crates/jurisearch-cli/src/main.rs:1947`, `crates/jurisearch-cli/src/main.rs:2454`, `crates/jurisearch-cli/src/main.rs:2568`, `crates/jurisearch-storage/src/ingest_accounting.rs:837`, `crates/jurisearch-storage/src/ingest_accounting.rs:844`, `crates/jurisearch-storage/src/ingest_accounting.rs:856`, `crates/jurisearch-storage/src/ingest_accounting.rs:865`, `crates/jurisearch-storage/src/ingest_accounting.rs:874`

A completed ingest and dense rebuild refresh a replay snapshot that computes MD5 signatures over `documents`, `chunks`, `graph_edges`, `chunk_embeddings`, and `index_manifest`. On the production corpus this is an intentional full-table pass across millions of rows plus large JSONB/vector text representations. It is not on the search path, but it can dominate the tail of maintenance commands and make users think ingest has hung after core work completed.

Concrete fix: make this snapshot explicitly opt-in for production, or maintain incremental per-run signatures from inserted/updated rows and reserve full-table snapshotting for `status --deep` / CI validation. This is behavior/configuration, not a schema migration. Correctness risk: replay diagnostics become weaker unless the command clearly distinguishes cached/incremental signatures from deep snapshots.

## `crates/jurisearch-ingest`

### P2 - Archive parsing is single-threaded and fully materializes each XML member

**Location:** `crates/jurisearch-ingest/src/archive/reader.rs:61`, `crates/jurisearch-ingest/src/archive/reader.rs:73`, `crates/jurisearch-ingest/src/archive/reader.rs:83`, `crates/jurisearch-ingest/src/archive/reader.rs:102`, `crates/jurisearch-ingest/src/archive/reader.rs:128`, `crates/jurisearch-ingest/src/legi/mod.rs:447`, `crates/jurisearch-ingest/src/legi/mod.rs:448`, `crates/jurisearch-ingest/src/legi/mod.rs:479`

The archive reader streams the gzip tar sequentially but reads each `.xml` member into a `Vec<u8>`, converts it to `&str`, then parses it with `quick_xml::Reader::from_str`. This is reasonable for correctness and simplicity, and the 64MB transaction byte cap bounds batch memory. It is still a throughput ceiling because parse, canonicalization, resume decisions, and database writes all happen on the ingest thread.

Concrete fix after the database write path is batched: split ingestion into a producer/parser pool and a single DB writer. The archive reader can keep tar order while workers parse members into canonical records; the writer then commits deterministic batches. Do not do this before the insert path is set-based, because concurrent parse workers will otherwise just feed the existing per-row DB bottleneck faster. This is implementation-only, but deterministic ingest accounting and quarantine ordering need tests.

## `crates/jurisearch-embed`

### P2 - Query embedding client is rebuilt for every dense one-shot search

**Location:** `crates/jurisearch-cli/src/main.rs:1146`, `crates/jurisearch-cli/src/main.rs:1151`, `crates/jurisearch-embed/src/lib.rs:357`, `crates/jurisearch-embed/src/lib.rs:372`, `crates/jurisearch-embed/src/lib.rs:376`

Dense search creates a new `OpenAiCompatibleClient` for each CLI invocation, including a new `ureq::Agent` and tokenizer load. In a CLI-only process this is expected, and the cost is probably small relative to starting managed Postgres and running retrieval. For batch/eval, the current code still calls `search_with_postgres` per qrel, so it also rebuilds the embedding client per query even though Postgres readiness is checked once.

Concrete fix: for eval and any future batch search, create the embedding client once and pass a prepared query-embedding function into the loop. For one-shot CLI, a daemon mode or session command would be required to avoid cold setup. This is implementation-only and does not affect result semantics.

## Prioritized Summary

| Priority | Area | Bottleneck | Estimated cost at production scale | Fix type |
|---|---|---|---|---|
| P0 | `jurisearch-storage/france_legi.rs` | JSONB `graph_edges` scans and per-row `jsonb_array_elements` for temporal/cross-reference gold | O(12.9M edges) per gold extraction; already observed >73s non-completion | Migration/materialized facts plus query rewrite |
| P0 | `jurisearch-cli` + `projection.rs` | LEGI ingestion prepares per member and inserts/upserts per row | Millions of round-trips; about 12.9M graph-edge writes alone | Batch/COPY implementation rewrite |
| P1 | Dense retrieval | IVFFlat `lists=32`, `probes=4`, small ANN overfetch | Hundreds of thousands of vector comparisons per query with recall risk | Index rebuild plus runtime tuning |
| P1 | Query readiness | Full projection/embedding coverage counts on one-shot commands | Full 1.74M-document / 1.85M-chunk aggregation before search/fetch | Manifest cache plus refresh command |
| P1 | Runtime config | Durable profile lacks analytical memory/parallel settings | Sort/hash spills and weak parallelism on eval CTEs | Config-only |
| P1 | Embedding rebuild | Loads all pending chunk texts and clones them into a work queue | Potential multi-GB RSS before first request | Streaming/keyset implementation |
| P1 | Embedding inserts | Two SQL executions per vector | About 3.7M executions for 1.85M embeddings | Staging/COPY implementation |
| P1 | Hierarchy backfill | Full-resume backfill expands `TEXTELR.structure_links` JSONB repeatedly | Potential full metadata-root/article scan after resume | Migration to normalized structure links |
| P2 | Replay snapshot | Full-table MD5 signatures after maintenance commands | Full scan of documents, chunks, graph edges, embeddings | Config/diagnostic behavior |
| P2 | Archive parsing | Single-threaded tar/XML parse with per-member materialization | Secondary once DB writes are fixed | Parser/writer pipeline |
| P2 | Embedding runtime | Rebuilds embedding client/tokenizer per eval query | Small to moderate eval overhead | Reuse client in batch paths |

VERDICT: GO
