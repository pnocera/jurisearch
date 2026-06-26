# Recommendation: merge the existing LEGI index into the Phase 2 index copy

Use **B, but do it into a cloned Phase 2 index directory**, not directly into the only good Phase 2 build.

Reason: the live code and both databases make B feasible. Both indexes open under current HEAD, both are now at schema migration **v10**, both have the same projection columns, and both use the same locked embedding fingerprint:

- Phase 2: `schema_migrations = 1..10`, `index_manifest.schema.schema_version = 10`, 1,144,796 decision docs, 2,848,609 chunks/embeddings, sources `cass/capp/inca/jade`, `query_ready=true`.
- Phase 1: `schema_migrations = 1..10`, `index_manifest.schema.schema_version = 10`, 1,736,165 LEGI docs, 1,852,745 chunks/embeddings, source `legi`, `query_ready=true`.
- `documents`, `chunks`, `chunk_embeddings`, and `graph_edges` columns are identical in both indexes.
- Phase 2 has `0` `source='legi'` docs; Phase 1 has `0` non-LEGI docs.
- Embedding distribution is identical: `bge-m3:1024:normalize:true | bge-m3 | 1024`.

So re-embedding LEGI would mostly waste time and money. Option A is the conservative fallback if the merge attempt fails, but it should not be the first choice. Option C is not acceptable for the Phase 2 claim because production search/serve binds one `--index-dir`; without federation, split indexes cannot honestly satisfy “statutes + jurisprudence through the production pipeline.”

## Option B feasibility

### Tables to copy

Copy these from Phase 1 into the Phase 2-derived target:

1. `documents` where `source = 'legi'`
2. `chunks` for those LEGI documents
3. `chunk_embeddings` for those LEGI chunks
4. `graph_edges` where `from_document_id` is a LEGI document
5. `legi_metadata_roots`
6. `ingest_run` rows for `source = 'legi'`
7. `ingest_member` rows for `source = 'legi'`, preferably without copying `member_id` so the target allocates fresh IDs

Do **not** copy `schema_migrations` or `index_manifest` wholesale. Keep the target schema manifest. Rebuild/update only the target `embedding` manifest after the combined dense index is rebuilt. Delete stale `query_readiness` and `replay_snapshot` manifest rows before/after merge.

`ingest_error` is empty in the checked Phase 1 index, so there is nothing to copy. If it were non-empty, it would need a member-id remap.

### ID and FK safety

Documents/chunks are safe:

- Phase 1 document IDs are `legi:...`.
- Phase 2 decision IDs are `cass:...`, `capp:...`, `inca:...`, `jade:...`.
- Phase 1 chunk IDs are `chunk:legi:...`.
- Phase 2 has no `chunk:legi:%`.

Graph edge IDs are hash-based (`publisher-edge:<sha256>` / `inferred-edge:<sha256>`), not source-prefixed. The hash input includes `from_document_id`, so LEGI-vs-decision collisions are practically impossible, but not structurally impossible. Run a preflight collision check, or insert into a disposable cloned target so a collision just aborts the merge attempt.

FK order is straightforward:

1. `documents`
2. `legi_metadata_roots`
3. `chunks`
4. `chunk_embeddings`
5. `graph_edges`
6. `ingest_run`
7. `ingest_member`

The checked source indexes had no orphaned chunks, embeddings, or graph-edge `from_document_id` references. LEGI graph edges currently have `to_document_id = NULL`, so they do not need target documents beyond their source document.

### Status and gates

`query_ready` is not derived from ingest history. It is derived from:

- projection coverage: every `documents` row has at least one chunk;
- embedding coverage: every `chunks` row has a matching `chunk_embeddings` row with the chunk’s current fingerprint.

Because the copied LEGI chunks already have matching embeddings, the combined index can pass query readiness after:

- deleting stale `query_readiness`;
- ensuring every chunk has `embedding_fingerprint='bge-m3:1024:normalize:true'`;
- rebuilding the IVFFlat index;
- updating `index_manifest['embedding']` to the combined counts.

`corpus_sources` is read from latest completed `ingest_run` manifests by source. Copying the LEGI `ingest_run` row is enough for `corpus_sources.legi` to appear. Copying `ingest_member` is not required for query readiness or Phase 2 gate, but it preserves resume/accounting semantics for later LEGI replays.

Current `phase2_gate` checks only `cass/capp/inca/jade`, `query_ready`, honest zone provenance, and the Phase 2 benchmark artifact. It does **not** currently have a dedicated “LEGI present” check even though the claim text says statutes + jurisprudence. So after B, the actual combined index will be correct; the benchmark should still include statute+jurisprudence workflows to prevent the gate from becoming a paper pass.

`finalize_dense_rebuild` itself would pass on the combined corpus if called. The current CLI command `ingest embed-chunks` is not a finalize-only command: if there are no pending chunks to embed, it returns “no chunks are available to embed” before calling `finalize_dense_rebuild`. For B, either add a finalize-only command later or run the equivalent SQL manually for the merge.

## Option A pitfalls

A is safe but wasteful. Re-ingesting LEGI into the Phase 2 index should not collide with decision IDs because sources are namespaced. It will invalidate readiness until the new LEGI chunks are embedded. `embed-chunks` will embed only chunks missing the selected fingerprint, so it should not re-embed the existing jurisprudence chunks.

The main costs/risks are operational:

- ~1.85M avoidable embedding calls;
- `query_ready=false` until embedding/finalization completes;
- possible long ingest/index maintenance unless BM25/vector indexes are dropped/rebuilt around the bulk load;
- latest ingest health will temporarily point at the LEGI run, so failed LEGI members would surface as latest-run failures.

## Concrete sequence for B

Work on a clone:

```bash
set -euo pipefail

SRC_LEGI=/home/pierre/Work/jurisearch/index/phase1-freemium-20250713
SRC_JURI=/mnt/models/jurisearch-index/phase2-jurisprudence
DST=/mnt/models/jurisearch-index/phase2-full-juridic

rsync -aH --info=progress2 "$SRC_JURI/" "$DST/"
```

Open both databases with the same pgrx Postgres used by `jurisearch`:

```bash
PGBIN=$(/home/pierre/.pgrx/18.4/pgrx-install/bin/pg_config --bindir)

# Refresh runtime configs/migrations first.
cargo run -q -p jurisearch-cli -- status --index-dir "$SRC_LEGI" >/tmp/legi-status.json
cargo run -q -p jurisearch-cli -- status --index-dir "$DST" >/tmp/full-status-before.json

LEGI_PORT=$(awk '/^port = / {print $3}' "$SRC_LEGI/pg/data/jurisearch.conf")
DST_PORT=$(awk '/^port = / {print $3}' "$DST/pg/data/jurisearch.conf")

"$PGBIN/pg_ctl" -D "$SRC_LEGI/pg/data" -l "$SRC_LEGI/pg/postgres.log" start -w
"$PGBIN/pg_ctl" -D "$DST/pg/data" -l "$DST/pg/postgres.log" start -w
```

Preflight:

```bash
PSQL_LEGI=("$PGBIN/psql" -h 127.0.0.1 -p "$LEGI_PORT" -U postgres -d jurisearch -qAt -v ON_ERROR_STOP=1)
PSQL_DST=("$PGBIN/psql" -h 127.0.0.1 -p "$DST_PORT" -U postgres -d jurisearch -qAt -v ON_ERROR_STOP=1)

"${PSQL_LEGI[@]}" -c "SELECT string_agg(version::text, ',' ORDER BY version) FROM schema_migrations;"
"${PSQL_DST[@]}"  -c "SELECT string_agg(version::text, ',' ORDER BY version) FROM schema_migrations;"

"${PSQL_DST[@]}" -c "SELECT count(*) FROM documents WHERE source='legi';"
"${PSQL_LEGI[@]}" -c "SELECT embedding_fingerprint, model, dimension, count(*) FROM chunk_embeddings GROUP BY 1,2,3;"
```

Prepare target for bulk load:

```bash
"${PSQL_DST[@]}" <<'SQL'
DELETE FROM index_manifest WHERE key IN ('query_readiness', 'replay_snapshot');
DROP INDEX IF EXISTS chunk_embeddings_embedding_ivfflat_idx;
DROP INDEX IF EXISTS chunks_bm25_idx;
DROP INDEX IF EXISTS graph_edges_publisher_citation_to_source_uid_idx;
SQL
```

Copy data, preserving table order:

```bash
# 1. Documents
"${PSQL_LEGI[@]}" -c "COPY (
  SELECT document_id, source, kind, source_uid, version_group, citation, title, body,
         valid_from, valid_to, valid_to_raw, source_url, source_payload_hash,
         canonical_json, created_at, updated_at, hierarchy_path
  FROM documents
  WHERE source='legi'
  ORDER BY document_id
) TO STDOUT WITH (FORMAT binary)" |
"${PSQL_DST[@]}" -c "COPY documents (
  document_id, source, kind, source_uid, version_group, citation, title, body,
  valid_from, valid_to, valid_to_raw, source_url, source_payload_hash,
  canonical_json, created_at, updated_at, hierarchy_path
) FROM STDIN WITH (FORMAT binary)"

# 2. LEGI metadata roots
"${PSQL_LEGI[@]}" -c "COPY (
  SELECT metadata_key, root_kind, source_uid, parent_source_uid, title, valid_from,
         valid_to, valid_to_raw, source_payload_hash, source_archive,
         source_member_path, canonical_version, canonical_json, created_at, updated_at
  FROM legi_metadata_roots
  ORDER BY metadata_key
) TO STDOUT WITH (FORMAT binary)" |
"${PSQL_DST[@]}" -c "COPY legi_metadata_roots (
  metadata_key, root_kind, source_uid, parent_source_uid, title, valid_from,
  valid_to, valid_to_raw, source_payload_hash, source_archive,
  source_member_path, canonical_version, canonical_json, created_at, updated_at
) FROM STDIN WITH (FORMAT binary)"

# 3. Chunks
"${PSQL_LEGI[@]}" -c "COPY (
  SELECT chunk_id, document_id, chunk_index, body, chunk_kind, source_fields,
         source_payload_hash, chunk_builder_version, embedding_fingerprint,
         created_at, contextualized_body, chunking, boundary, hierarchy_path
  FROM chunks
  WHERE document_id LIKE 'legi:%'
  ORDER BY document_id, chunk_index, chunk_id
) TO STDOUT WITH (FORMAT binary)" |
"${PSQL_DST[@]}" -c "COPY chunks (
  chunk_id, document_id, chunk_index, body, chunk_kind, source_fields,
  source_payload_hash, chunk_builder_version, embedding_fingerprint,
  created_at, contextualized_body, chunking, boundary, hierarchy_path
) FROM STDIN WITH (FORMAT binary)"

# 4. Embeddings
"${PSQL_LEGI[@]}" -c "COPY (
  SELECT ce.chunk_id, ce.embedding_fingerprint, ce.embedding, ce.model, ce.dimension, ce.created_at
  FROM chunk_embeddings ce
  JOIN chunks c ON c.chunk_id = ce.chunk_id
  WHERE c.document_id LIKE 'legi:%'
  ORDER BY ce.chunk_id
) TO STDOUT WITH (FORMAT binary)" |
"${PSQL_DST[@]}" -c "COPY chunk_embeddings (
  chunk_id, embedding_fingerprint, embedding, model, dimension, created_at
) FROM STDIN WITH (FORMAT binary)"

# 5. Graph edges
"${PSQL_LEGI[@]}" -c "COPY (
  SELECT edge_id, from_document_id, to_document_id, edge_kind, edge_source, payload, created_at
  FROM graph_edges
  WHERE from_document_id LIKE 'legi:%'
  ORDER BY edge_id
) TO STDOUT WITH (FORMAT binary)" |
"${PSQL_DST[@]}" -c "COPY graph_edges (
  edge_id, from_document_id, to_document_id, edge_kind, edge_source, payload, created_at
) FROM STDIN WITH (FORMAT binary)"

# 6. Ingest run
"${PSQL_LEGI[@]}" -c "COPY (
  SELECT run_id, source, status, parser_version, schema_version, code_version,
         safe_mode, archive_plan, manifest, error_message, started_at,
         completed_at, updated_at
  FROM ingest_run
  WHERE source='legi'
  ORDER BY started_at
) TO STDOUT WITH (FORMAT binary)" |
"${PSQL_DST[@]}" -c "COPY ingest_run (
  run_id, source, status, parser_version, schema_version, code_version,
  safe_mode, archive_plan, manifest, error_message, started_at,
  completed_at, updated_at
) FROM STDIN WITH (FORMAT binary)"

# 7. Ingest members, let target allocate fresh member_id values.
"${PSQL_LEGI[@]}" -c "COPY (
  SELECT run_id, archive_name, member_path, source, source_entity, date_anchor,
         status, parser_version, schema_version, code_version, source_payload_hash,
         attempt_count, error_count, last_error_class, last_error_code,
         last_error_message, created_at, updated_at
  FROM ingest_member
  WHERE source='legi'
  ORDER BY member_id
) TO STDOUT WITH (FORMAT binary)" |
"${PSQL_DST[@]}" -c "COPY ingest_member (
  run_id, archive_name, member_path, source, source_entity, date_anchor,
  status, parser_version, schema_version, code_version, source_payload_hash,
  attempt_count, error_count, last_error_class, last_error_code,
  last_error_message, created_at, updated_at
) FROM STDIN WITH (FORMAT binary)"
```

Rebuild indexes and manifest:

```bash
"${PSQL_DST[@]}" <<'SQL'
-- Rebuild lexical index from migration v9.
CREATE INDEX chunks_bm25_idx
ON chunks USING bm25 (chunk_id, contextualized_body)
WITH (
    key_field = 'chunk_id',
    text_fields = '{
        "contextualized_body": {
            "tokenizer": {
                "type": "default",
                "ascii_folding": true,
                "stemmer": "French",
                "stopwords_language": "French"
            }
        }
    }'
);

-- Dense finalization equivalent for already-present embeddings.
UPDATE chunks SET embedding_fingerprint = 'bge-m3:1024:normalize:true';

DO $$
DECLARE missing bigint;
BEGIN
  SELECT count(*)
  INTO missing
  FROM chunks c
  LEFT JOIN chunk_embeddings ce ON ce.chunk_id = c.chunk_id
  WHERE ce.chunk_id IS NULL
     OR ce.embedding_fingerprint <> 'bge-m3:1024:normalize:true'
     OR ce.model <> 'bge-m3'
     OR ce.dimension <> 1024;

  IF missing <> 0 THEN
    RAISE EXCEPTION '% chunks missing bge-m3 embeddings', missing;
  END IF;
END $$;

CREATE INDEX chunk_embeddings_embedding_ivfflat_idx
ON chunk_embeddings USING ivfflat (embedding vector_l2_ops)
WITH (lists = 32);

CREATE INDEX IF NOT EXISTS graph_edges_publisher_citation_to_source_uid_idx
ON graph_edges ((payload->>'to_source_uid'))
WHERE edge_source = 'publisher'
  AND payload->'attributes' @> '[{"key":"typelien","value":"CITATION"},{"key":"sens","value":"cible"}]'::jsonb;

ANALYZE documents;
ANALYZE chunks;
ANALYZE chunk_embeddings;
ANALYZE graph_edges;

WITH counts AS (
  SELECT
    (SELECT count(*) FROM chunks) AS chunks,
    (SELECT count(*) FROM chunk_embeddings
      WHERE embedding_fingerprint='bge-m3:1024:normalize:true'
        AND model='bge-m3'
        AND dimension=1024) AS embeddings
)
INSERT INTO index_manifest(key, value, updated_at)
SELECT 'embedding',
       jsonb_build_object(
         'embedding_fingerprint', 'bge-m3:1024:normalize:true',
         'model', 'bge-m3',
         'dimension', 1024,
         'normalize', true,
         'provisional', true,
         'reembeddable', true,
         'vector_index', jsonb_build_object(
           'name', 'chunk_embeddings_embedding_ivfflat_idx',
           'method', 'ivfflat',
           'operator_class', 'vector_l2_ops',
           'lists', 32
         ),
         'coverage', jsonb_build_object('chunks', chunks, 'embeddings', embeddings)
       ),
       now()
FROM counts
ON CONFLICT (key) DO UPDATE
SET value = EXCLUDED.value,
    updated_at = EXCLUDED.updated_at;

DELETE FROM index_manifest WHERE key IN ('query_readiness', 'replay_snapshot');
SQL
```

Stop the manual servers:

```bash
"$PGBIN/pg_ctl" -D "$SRC_LEGI/pg/data" -m fast stop
"$PGBIN/pg_ctl" -D "$DST/pg/data" -m fast stop
```

Validate through the application:

```bash
cargo run -q -p jurisearch-cli -- stats --index-dir "$DST" | jq .stats.documents_by_source
cargo run -q -p jurisearch-cli -- status --index-dir "$DST" \
  | jq '{index, corpus_sources, ingest_health: {projection_coverage: .ingest_health.projection_coverage, embedding_coverage: .ingest_health.embedding_coverage, embedding_manifest: .ingest_health.embedding_manifest}, phase2_gate: .phase2_gate}'

# Smoke both sides through the single production index.
cargo run -q -p jurisearch-cli -- search --index-dir "$DST" --kind code --top-k 3 "responsabilité civile article 1240" | jq '.candidates | length'
cargo run -q -p jurisearch-cli -- search --index-dir "$DST" --kind decision --top-k 3 "responsabilité médicale faute indemnisation" | jq '.candidates | length'
```

Optional but useful later:

```bash
# This can be slow on the full combined corpus; run when you want a fresh replay signature cache.
cargo run -q -p jurisearch-cli -- status --index-dir "$DST" --deep >/tmp/phase2-full-status-deep.json
```

## Fallback

If the merge fails in the cloned target, delete the clone and use A. A is known-good and code-supported end to end:

```bash
cargo run -q -p jurisearch-cli -- ingest legi-archives \
  --index-dir /mnt/models/jurisearch-index/phase2-jurisprudence \
  --archives-dir /home/pierre/Apps/juridocs/opendata/LEGI \
  --run-id phase2-legi-full

cargo run -q -p jurisearch-cli -- ingest embed-chunks \
  --index-dir /mnt/models/jurisearch-index/phase2-jurisprudence \
  --index-lists 32
```

But given the verified schema/fingerprint parity, B is the better first move.
