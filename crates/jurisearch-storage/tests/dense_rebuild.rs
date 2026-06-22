mod common;

use common::{discover_pg_config, vector_literal};
use jurisearch_storage::{
    dense::{
        DENSE_VECTOR_INDEX_NAME, DenseRebuildSpec, finalize_dense_rebuild,
        load_chunk_embedding_inputs,
    },
    runtime::{ManagedPostgres, StorageError},
};

const EMBEDDING_FINGERPRINT: &str = "bge-m3:1024:normalize:true";

#[test]
fn dense_rebuild_requires_full_coverage_then_writes_index_and_manifest() -> Result<(), StorageError>
{
    let Some(pg_config) = discover_pg_config("dense rebuild")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-dense-rebuild.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let legal_vector = vector_literal(0);
    let other_vector = vector_literal(1);
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;

    postgres.execute_sql(&format!(
        "INSERT INTO documents \
           (document_id, source, kind, source_uid, citation, title, body, \
            valid_from, source_payload_hash, canonical_json) \
         VALUES \
           ('legi:LEGIARTI000006419320@1804-02-21', 'legi', 'article', \
            'LEGIARTI000006419320', 'Code civil article 1240', \
            'Article 1240', 'Responsabilite civile faute dommage.', \
            '1804-02-21', 'sha256:article-1240', '{{\"official\":true}}'), \
           ('legi:LEGIARTI000000000001@2024-01-01', 'legi', 'article', \
            'LEGIARTI000000000001', 'Code civil article 1', \
            'Article 1', 'Disposition generale.', \
            '2024-01-01', 'sha256:article-1', '{{\"official\":true}}'); \
         INSERT INTO chunks \
           (chunk_id, document_id, chunk_index, body, contextualized_body, source_payload_hash, \
            chunk_builder_version, embedding_fingerprint) \
         VALUES \
           ('chunk:1240:0', 'legi:LEGIARTI000006419320@1804-02-21', 0, \
            'responsabilite civile faute reparation dommage article 1240', \
            'Code civil > Article 1240\nresponsabilite civile faute reparation dommage article 1240', \
            'sha256:article-1240', 'chunker:v0', NULL), \
           ('chunk:article-1:0', 'legi:LEGIARTI000000000001@2024-01-01', 0, \
            'disposition generale article 1', \
            'Code civil > Article 1\ndisposition generale article 1', \
            'sha256:article-1', 'chunker:v0', NULL); \
         INSERT INTO chunk_embeddings \
           (chunk_id, embedding_fingerprint, embedding, model, dimension) \
         VALUES \
           ('chunk:1240:0', '{embedding_fingerprint}', '{legal_vector}', 'bge-m3', 1024);",
        embedding_fingerprint = EMBEDDING_FINGERPRINT,
        legal_vector = legal_vector,
    ))?;

    let spec = DenseRebuildSpec {
        embedding_fingerprint: EMBEDDING_FINGERPRINT,
        model: "bge-m3",
        dimension: 1024,
        normalize: true,
        provisional: true,
        reembeddable: true,
        index_lists: 1,
    };
    let incomplete = finalize_dense_rebuild(&postgres, &spec).unwrap_err();
    assert!(matches!(incomplete, StorageError::DenseRebuild { .. }));

    postgres.execute_sql(&format!(
        "INSERT INTO chunk_embeddings \
           (chunk_id, embedding_fingerprint, embedding, model, dimension) \
         VALUES \
           ('chunk:article-1:0', '{embedding_fingerprint}', '{other_vector}', 'bge-m3', 1024);",
        embedding_fingerprint = EMBEDDING_FINGERPRINT,
        other_vector = other_vector,
    ))?;

    let report = finalize_dense_rebuild(&postgres, &spec)?;
    assert_eq!(report.chunks, 2);
    assert_eq!(report.embeddings, 2);
    assert_eq!(report.index_name, DENSE_VECTOR_INDEX_NAME);
    assert_eq!(report.index_lists, 1);

    let index_name = postgres.execute_sql(&format!(
        "SELECT indexname \
         FROM pg_indexes \
         WHERE schemaname = 'public' \
           AND indexname = '{}';",
        DENSE_VECTOR_INDEX_NAME
    ))?;
    assert_eq!(index_name, DENSE_VECTOR_INDEX_NAME);

    let manifest = postgres.execute_sql(
        "SELECT value \
         FROM index_manifest \
         WHERE key = 'embedding';",
    )?;
    let manifest: serde_json::Value =
        serde_json::from_str(&manifest).expect("embedding manifest is stable JSON");
    assert_eq!(
        manifest["embedding_fingerprint"],
        "bge-m3:1024:normalize:true"
    );
    assert_eq!(manifest["normalize"], true);
    assert_eq!(manifest["provisional"], true);
    assert_eq!(manifest["reembeddable"], true);
    assert_eq!(manifest["coverage"]["chunks"], 2);
    assert_eq!(manifest["coverage"]["embeddings"], 2);
    assert_eq!(manifest["vector_index"]["name"], DENSE_VECTOR_INDEX_NAME);
    assert_eq!(manifest["vector_index"]["lists"], 1);

    Ok(())
}

#[test]
fn chunk_embedding_inputs_prefer_contextualized_body_and_honor_limit() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("chunk embedding inputs")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-chunk-embedding-inputs.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;

    postgres.execute_sql(
        "INSERT INTO documents \
           (document_id, source, kind, source_uid, citation, title, body, \
            valid_from, source_payload_hash, canonical_json) \
         VALUES \
           ('legi:LEGIARTI000000000001@2024-01-01', 'legi', 'article', \
            'LEGIARTI000000000001', 'Code civil article 1', \
            'Article 1', 'Disposition generale.', \
            '2024-01-01', 'sha256:article-1', \
            '{\"chunks\":[{\"contextualized_body\":\"ignored document-level context\"}]}'), \
           ('legi:LEGIARTI000000000002@2024-01-01', 'legi', 'article', \
            'LEGIARTI000000000002', 'Code civil article 2', \
            'Article 2', 'Fallback body.', \
            '2024-01-01', 'sha256:article-2', \
            '{\"chunks\":[{}]}'); \
         INSERT INTO chunks \
           (chunk_id, document_id, chunk_index, body, contextualized_body, \
            source_payload_hash, \
            chunk_builder_version, embedding_fingerprint) \
         VALUES \
           ('chunk:article-1:0', 'legi:LEGIARTI000000000001@2024-01-01', 0, \
            'plain body should not be embedded first', \
            'Code civil > Article 1\nDisposition generale contextualisee.', \
            'sha256:article-1', 'chunker:v0', NULL), \
           ('chunk:article-2:0', 'legi:LEGIARTI000000000002@2024-01-01', 0, \
            'fallback body should be embedded', 'fallback body should be embedded', \
            'sha256:article-2', 'chunker:v0', NULL);",
    )?;

    let limited =
        load_chunk_embedding_inputs(&postgres, EMBEDDING_FINGERPRINT, "bge-m3", 1024, Some(1))?;
    assert_eq!(limited.len(), 1);
    assert_eq!(limited[0].chunk_id, "chunk:article-1:0");
    assert_eq!(
        limited[0].embedding_text,
        "Code civil > Article 1\nDisposition generale contextualisee."
    );

    let all = load_chunk_embedding_inputs(&postgres, EMBEDDING_FINGERPRINT, "bge-m3", 1024, None)?;
    assert_eq!(all.len(), 2);
    assert_eq!(all[1].chunk_id, "chunk:article-2:0");
    assert_eq!(all[1].embedding_text, "fallback body should be embedded");

    postgres.execute_sql(&format!(
        "INSERT INTO chunk_embeddings \
           (chunk_id, embedding_fingerprint, embedding, model, dimension) \
         VALUES \
           ('chunk:article-1:0', '{embedding_fingerprint}', '{matching_vector}', 'bge-m3', 1024), \
           ('chunk:article-2:0', 'stale-fingerprint', '{stale_vector}', 'bge-m3', 1024);",
        embedding_fingerprint = EMBEDDING_FINGERPRINT,
        matching_vector = vector_literal(0),
        stale_vector = vector_literal(1),
    ))?;
    let pending =
        load_chunk_embedding_inputs(&postgres, EMBEDDING_FINGERPRINT, "bge-m3", 1024, None)?;
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].chunk_id, "chunk:article-2:0");
    assert_eq!(
        pending[0].embedding_text,
        "fallback body should be embedded"
    );

    Ok(())
}
