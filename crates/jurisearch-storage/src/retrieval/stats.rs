//! Corpus stats / source coverage / single-document inspection JSON.

use super::*;

/// Corpus/graph/embedding counts for `stats` — replaces ad-hoc psql for introspection. The counts
/// are exact (sequential scans over large tables), so this is an introspection command, not a hot path.
pub fn corpus_stats_json(postgres: &ManagedPostgres) -> Result<String, StorageError> {
    postgres.execute_sql(
        r#"
SELECT jsonb_build_object(
    'documents', (SELECT count(*) FROM documents),
    'documents_by_kind', COALESCE((SELECT jsonb_object_agg(kind, c) FROM (SELECT kind, count(*) c FROM documents GROUP BY kind) t), '{}'::jsonb),
    'documents_by_source', COALESCE((SELECT jsonb_object_agg(source, c) FROM (SELECT source, count(*) c FROM documents GROUP BY source) t), '{}'::jsonb),
    'chunks', (SELECT count(*) FROM chunks),
    'chunk_embeddings', (SELECT count(*) FROM chunk_embeddings),
    'graph_edges', (SELECT count(*) FROM graph_edges),
    'graph_edges_by_kind', COALESCE((SELECT jsonb_object_agg(edge_kind, c) FROM (SELECT edge_kind, count(*) c FROM graph_edges GROUP BY edge_kind) t), '{}'::jsonb),
    'graph_edges_by_source', COALESCE((SELECT jsonb_object_agg(edge_source, c) FROM (SELECT edge_source, count(*) c FROM graph_edges GROUP BY edge_source) t), '{}'::jsonb)
)::text;
"#,
    )
}

/// Per-source corpus coverage + freshness for `status`, read cheaply from each source's latest
/// completed ingest run manifest (no live full-corpus counts). Surfaces honest zone/chunking
/// provenance so judicial (cass/capp/inca) vs administrative (jade) jurisprudence coverage and
/// freshness are visible alongside legi. Sources without a completed run simply do not appear.
pub fn corpus_source_coverage_json(postgres: &ManagedPostgres) -> Result<String, StorageError> {
    postgres.execute_sql(
        r#"
SELECT COALESCE((
    SELECT jsonb_object_agg(source, summary)
    FROM (
        SELECT DISTINCT ON (source)
            source,
            jsonb_build_object(
                'latest_completed_run', run_id,
                'completed_at', completed_at,
                'dataset', manifest->>'dataset',
                'source_version', manifest->>'source_version',
                'zone_accurate', COALESCE(manifest->'zone_accurate', 'null'::jsonb),
                'chunking_provenance', manifest->>'chunking_provenance',
                'freshness', COALESCE(manifest->'freshness', 'null'::jsonb),
                -- Per-run insert counts from the latest run (a replay/sync may legitimately be 0);
                -- cumulative live corpus counts are exposed by the `stats` command.
                'last_run_coverage', COALESCE(manifest->'coverage', '{}'::jsonb)
            ) AS summary
        FROM ingest_run
        WHERE status = 'completed'
          -- Only runs that actually advanced freshness (processed archives) define per-source
          -- freshness; a no-op/incremental sync that read nothing carries a null source_version and
          -- must not regress the reported freshness to the previous full build.
          AND manifest->>'source_version' IS NOT NULL
        ORDER BY source, manifest->>'source_version' DESC, completed_at DESC NULLS LAST, run_id DESC
    ) latest
), '{}'::jsonb)::text;
"#,
    )
}

/// Raw canonical record for one document (`inspect`): the full `documents` row (incl. canonical_json),
/// its chunk count, and outgoing edge count. Returns `{"document": null, ...}` when the id is unknown.
pub fn inspect_document_json(
    postgres: &ManagedPostgres,
    document_id: &str,
) -> Result<String, StorageError> {
    let id = sql_string_literal(document_id);
    postgres.execute_sql(&format!(
        r#"
SELECT jsonb_build_object(
    'document', (SELECT to_jsonb(d) FROM documents d WHERE d.document_id = {id}),
    'chunk_count', (SELECT count(*) FROM chunks WHERE document_id = {id}),
    'outgoing_edges', (SELECT count(*) FROM graph_edges WHERE from_document_id = {id})
)::text;
"#
    ))
}
