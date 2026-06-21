mod common;

use common::{discover_pg_config, vector_literal};
use jurisearch_storage::{
    migrations::CURRENT_SCHEMA_VERSION,
    runtime::{ManagedPostgres, StorageError},
};

#[test]
fn migrations_install_minimal_schema_and_are_idempotent() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("schema migration smoke")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-schema-pg.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let query_vector = vector_literal(0);
    let unrelated_vector = vector_literal(1);

    {
        let postgres = ManagedPostgres::start_durable(pg_config.clone(), root.path())?;
        let report = postgres.run_migrations()?;
        assert_eq!(report.current_version, CURRENT_SCHEMA_VERSION);
        assert_eq!(
            report.applied,
            (1..=CURRENT_SCHEMA_VERSION).collect::<Vec<_>>()
        );

        let migrations = postgres.execute_sql(
            "SELECT version::text || ':' || name \
             FROM schema_migrations \
             ORDER BY version;",
        )?;
        assert!(migrations.contains("1:canonical_documents_chunks_vectors"));
        assert!(migrations.contains("2:chunk_bm25_index"));
        assert!(migrations.contains("3:ingest_operational_accounting"));
        assert!(migrations.contains("4:legi_metadata_roots"));
        assert!(migrations.contains("5:documents_source_uid_index"));
        assert!(migrations.contains("6:chunk_provenance_columns"));
        assert!(migrations.contains("7:document_hierarchy_path_index"));
        assert!(migrations.contains("8:chunk_contextualized_bm25_index"));
        assert!(migrations.contains("9:chunk_french_legal_bm25_analyzer"));
        assert_eq!(
            postgres.execute_sql(
                "SELECT count(*)::text \
                 FROM pg_indexes \
                 WHERE schemaname = current_schema() \
                   AND indexname = 'documents_source_uid_idx';",
            )?,
            "1"
        );
        assert_eq!(
            postgres.execute_sql(
                "SELECT (indexdef LIKE '%md5((hierarchy_path)::text)%')::text \
                 FROM pg_indexes \
                 WHERE schemaname = current_schema() \
                   AND indexname = 'documents_context_hierarchy_idx';",
            )?,
            "true"
        );
        assert_eq!(
            postgres.execute_sql(
                "SELECT (indexdef LIKE '%contextualized_body%' \
                         AND indexdef LIKE '%ascii_folding%' \
                         AND indexdef LIKE '%French%')::text \
                 FROM pg_indexes \
                 WHERE schemaname = current_schema() \
                   AND indexname = 'chunks_bm25_idx';",
            )?,
            "true"
        );
        assert_eq!(
            postgres.execute_sql(
                "SELECT column_name \
                 FROM information_schema.columns \
                 WHERE table_schema = current_schema() \
                   AND table_name = 'documents' \
                   AND column_name = 'hierarchy_path';",
            )?,
            "hierarchy_path"
        );
        assert_eq!(
            postgres.execute_sql(
                "SELECT string_agg(column_name, ',' ORDER BY column_name) \
                 FROM information_schema.columns \
                 WHERE table_schema = current_schema() \
                   AND table_name = 'chunks' \
                   AND column_name IN ( \
                       'boundary', \
                       'chunking', \
                       'contextualized_body', \
                       'hierarchy_path' \
                   );",
            )?,
            "boundary,chunking,contextualized_body,hierarchy_path"
        );
        assert_eq!(
            postgres.execute_sql(
                "SELECT is_nullable \
                 FROM information_schema.columns \
                 WHERE table_schema = current_schema() \
                   AND table_name = 'chunks' \
                   AND column_name = 'contextualized_body';",
            )?,
            "NO"
        );
        assert_eq!(
            postgres.execute_sql(
                "SELECT count(*)::text \
                 FROM pg_constraint \
                 WHERE conrelid = 'chunks'::regclass \
                   AND conname = 'chunks_contextualized_body_not_empty';",
            )?,
            "1"
        );

        postgres.execute_sql(&format!(
            "INSERT INTO documents \
               (document_id, source, kind, source_uid, version_group, citation, title, body, \
                valid_from, source_payload_hash, canonical_json) \
             VALUES \
               ('legi:LEGIARTI000006419320@1804-02-21', 'legi', 'article', \
                'LEGIARTI000006419320', 'LEGIARTI000006419320', 'Code civil article 1240', \
                'Article 1240', 'Tout fait quelconque de l''homme...', '1804-02-21', \
                'sha256:article-1240', '{{\"official\":true}}'), \
               ('legi:LEGIARTI000000000001@2024-01-01', 'legi', 'article', \
                'LEGIARTI000000000001', 'LEGIARTI000000000001', 'Code de cuisine article 1', \
                'Article cuisine', 'Recette sans rapport juridique', '2024-01-01', \
                'sha256:recipe', '{{\"official\":true}}'); \
             INSERT INTO chunks \
               (chunk_id, document_id, chunk_index, body, contextualized_body, source_payload_hash, \
                chunk_builder_version, embedding_fingerprint) \
             VALUES \
               ('chunk:1240:0', 'legi:LEGIARTI000006419320@1804-02-21', 0, \
                'responsabilite civile article 1240', \
                'Code civil > Article 1240\nresponsabilite civile article 1240', \
                'sha256:article-1240', \
                'chunker:v0', 'bge-m3:1024:normalize:true'), \
               ('chunk:recipe:0', 'legi:LEGIARTI000000000001@2024-01-01', 0, \
                'recette de tarte aux pommes', \
                'Code de cuisine > Article 1\nrecette de tarte aux pommes', \
                'sha256:recipe', \
                'chunker:v0', 'bge-m3:1024:normalize:true'); \
             INSERT INTO chunk_embeddings \
               (chunk_id, embedding_fingerprint, embedding, model, dimension) \
             VALUES \
               ('chunk:1240:0', 'bge-m3:1024:normalize:true', '{}', 'bge-m3', 1024), \
               ('chunk:recipe:0', 'bge-m3:1024:normalize:true', '{}', 'bge-m3', 1024);",
            query_vector, unrelated_vector
        ))?;
    }

    {
        let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
        let migration_count =
            postgres.execute_sql("SELECT count(*)::text FROM schema_migrations;")?;
        assert_eq!(migration_count, CURRENT_SCHEMA_VERSION.to_string());

        let nearest = postgres.execute_sql(&format!(
            "SELECT c.body \
             FROM chunks c \
             JOIN chunk_embeddings e ON e.chunk_id = c.chunk_id \
             ORDER BY e.embedding <-> '{}' \
             LIMIT 1;",
            query_vector
        ))?;
        assert_eq!(nearest, "responsabilite civile article 1240");

        let manifest_version = postgres.execute_sql(
            "SELECT value->>'schema_version' FROM index_manifest WHERE key = 'schema';",
        )?;
        assert_eq!(manifest_version, CURRENT_SCHEMA_VERSION.to_string());
        assert_eq!(
            postgres.execute_sql(
                "SELECT d.hierarchy_path::text || '|' || c.chunking || ':' || c.boundary || ':' || c.hierarchy_path::text \
                 FROM documents d \
                 JOIN chunks c ON c.document_id = d.document_id \
                 WHERE c.chunk_id = 'chunk:1240:0';",
            )?,
            "[]|structural:unknown:[]"
        );
    }

    Ok(())
}

#[test]
fn chunk_provenance_backfill_sql_materializes_existing_canonical_json() -> Result<(), StorageError>
{
    let Some(pg_config) = discover_pg_config("chunk provenance migration backfill")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-chunk-provenance-migration.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;

    postgres.execute_sql(
        "INSERT INTO documents \
           (document_id, source, kind, source_uid, version_group, citation, title, body, \
            valid_from, source_payload_hash, canonical_json) \
         VALUES \
           ('legi:LEGIARTI000000000001@2024-01-01', 'legi', 'article', \
            'LEGIARTI000000000001', 'LEGIARTI000000000001', 'Code civil article 1', \
            'Article 1', 'Disposition generale.', '2024-01-01', \
            'sha256:article-1', \
            '{\"hierarchy_path\":[\"Code civil\",\"Titre preliminaire\"],\"chunks\":[{\"contextualized_body\":\"Code civil > Article 1\\nDisposition generale.\",\"chunking\":\"structural\",\"boundary\":\"article\",\"hierarchy_path\":[\"Code civil\",\"Titre preliminaire\"]}]}'); \
         INSERT INTO chunks \
           (chunk_id, document_id, chunk_index, body, source_payload_hash, \
            chunk_builder_version, contextualized_body, chunking, boundary, hierarchy_path) \
         VALUES \
           ('chunk:article-1:0', 'legi:LEGIARTI000000000001@2024-01-01', 0, \
            'Disposition generale.', 'sha256:article-1', 'chunker:v0', \
            'Disposition generale.', 'structural', 'unknown', '[]'::jsonb);",
    )?;

    postgres.execute_sql(
        "UPDATE chunks c \
         SET contextualized_body = COALESCE( \
                 NULLIF(d.canonical_json->'chunks'->c.chunk_index->>'contextualized_body', ''), \
                 c.contextualized_body \
             ), \
             chunking = COALESCE( \
                 NULLIF(d.canonical_json->'chunks'->c.chunk_index->>'chunking', ''), \
                 c.chunking \
             ), \
             boundary = COALESCE( \
                 NULLIF(d.canonical_json->'chunks'->c.chunk_index->>'boundary', ''), \
                 c.boundary \
             ), \
             hierarchy_path = COALESCE( \
                 d.canonical_json->'chunks'->c.chunk_index->'hierarchy_path', \
                 c.hierarchy_path \
             ) \
         FROM documents d \
         WHERE d.document_id = c.document_id \
           AND jsonb_typeof(d.canonical_json->'chunks') = 'array';",
    )?;
    postgres.execute_sql(
        "UPDATE documents d \
         SET hierarchy_path = COALESCE( \
                 CASE \
                     WHEN jsonb_typeof(d.canonical_json->'hierarchy_path') = 'array' \
                         THEN d.canonical_json->'hierarchy_path' \
                     ELSE NULL \
                 END, \
                 ( \
                     SELECT c.hierarchy_path \
                     FROM chunks c \
                     WHERE c.document_id = d.document_id \
                       AND jsonb_typeof(c.hierarchy_path) = 'array' \
                       AND jsonb_array_length(c.hierarchy_path) > 0 \
                     ORDER BY c.chunk_index \
                     LIMIT 1 \
                 ), \
                 d.hierarchy_path \
             );",
    )?;

    assert_eq!(
        postgres.execute_sql(
            "SELECT c.contextualized_body, c.chunking, c.boundary, \
                    d.hierarchy_path->>1, c.hierarchy_path->>1 \
             FROM chunks c \
             JOIN documents d ON d.document_id = c.document_id \
             WHERE c.chunk_id = 'chunk:article-1:0';",
        )?,
        "Code civil > Article 1\nDisposition generale.|structural|article|Titre preliminaire|Titre preliminaire"
    );

    Ok(())
}
