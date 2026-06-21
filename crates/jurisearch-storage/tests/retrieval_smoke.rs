mod common;

use common::{discover_pg_config, vector_literal};
use jurisearch_storage::{
    retrieval::{
        FetchDocumentsQuery, HybridCandidateQuery, fetch_documents_json, hybrid_candidates_json,
    },
    runtime::{ManagedPostgres, StorageError},
};

#[test]
fn migrated_schema_supports_bm25_and_vector_candidate_retrieval() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("retrieval smoke")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-retrieval-pg.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let legal_vector = vector_literal(0);
    let unrelated_vector = vector_literal(1);

    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
    let bm25_index = postgres.execute_sql(
        "SELECT indexname \
         FROM pg_indexes \
         WHERE schemaname = 'public' \
           AND tablename = 'chunks' \
           AND indexname = 'chunks_bm25_idx';",
    )?;
    assert_eq!(bm25_index, "chunks_bm25_idx");

    postgres.execute_sql(&format!(
        "INSERT INTO documents \
           (document_id, source, kind, source_uid, citation, title, body, \
            valid_from, source_payload_hash, canonical_json) \
         VALUES \
           ('legi:LEGIARTI000006419320@1804-02-21', 'legi', 'article', \
            'LEGIARTI000006419320', 'Code civil article 1240', \
            'Article 1240', 'Tout fait quelconque de l''homme oblige celui par la faute duquel il est arrive a le reparer.', \
            '1804-02-21', 'sha256:article-1240', '{{\"official\":true}}'), \
           ('legi:LEGIARTI000000000001@2024-01-01', 'legi', 'article', \
            'LEGIARTI000000000001', 'Code de cuisine article 1', \
            'Article cuisine', 'Recette de tarte aux pommes avec cannelle.', \
            '2024-01-01', 'sha256:recipe', '{{\"official\":true}}'); \
         INSERT INTO chunks \
           (chunk_id, document_id, chunk_index, body, source_payload_hash, \
            chunk_builder_version, embedding_fingerprint) \
         VALUES \
           ('chunk:1240:0', 'legi:LEGIARTI000006419320@1804-02-21', 0, \
            'responsabilite civile faute reparation dommage article 1240', \
            'sha256:article-1240', 'chunker:v0', 'bge-m3:1024:normalize:true'), \
           ('chunk:recipe:0', 'legi:LEGIARTI000000000001@2024-01-01', 0, \
            'recette tarte pommes cannelle dessert', \
            'sha256:recipe', 'chunker:v0', 'bge-m3:1024:normalize:true'); \
         INSERT INTO chunk_embeddings \
           (chunk_id, embedding_fingerprint, embedding, model, dimension) \
         VALUES \
           ('chunk:1240:0', 'bge-m3:1024:normalize:true', '{}', 'bge-m3', 1024), \
           ('chunk:recipe:0', 'bge-m3:1024:normalize:true', '{}', 'bge-m3', 1024);",
        legal_vector, unrelated_vector
    ))?;

    let lexical = postgres.execute_sql(
        "SELECT chunk_id \
         FROM chunks \
         WHERE body @@@ 'responsabilite faute dommage' \
         ORDER BY paradedb.score(chunk_id) DESC \
         LIMIT 1;",
    )?;
    assert_eq!(lexical, "chunk:1240:0");

    let vector = postgres.execute_sql(&format!(
        "SELECT chunk_id \
         FROM chunk_embeddings \
         ORDER BY embedding <-> '{}' \
         LIMIT 1;",
        legal_vector
    ))?;
    assert_eq!(vector, "chunk:1240:0");

    let candidates = hybrid_candidates_json(
        &postgres,
        &HybridCandidateQuery {
            query_text: "responsabilite faute dommage",
            query_embedding: &legal_vector,
            embedding_fingerprint: "bge-m3:1024:normalize:true",
            as_of: "2024-01-01",
            lexical_limit: 10,
            dense_limit: 10,
            limit: 3,
        },
    )?;
    let candidates: serde_json::Value =
        serde_json::from_str(&candidates).expect("retrieval response is stable JSON");
    assert_eq!(candidates["query"], "responsabilite faute dommage");
    assert_eq!(candidates["candidates"][0]["chunk_id"], "chunk:1240:0");
    assert_eq!(
        candidates["candidates"][0]["scores"]["lexical_rank"].as_u64(),
        Some(1)
    );
    assert_eq!(
        candidates["candidates"][0]["scores"]["dense_rank"].as_u64(),
        Some(1)
    );
    assert!(
        candidates["candidates"][0]["cursor"]
            .as_str()
            .is_some_and(|cursor| cursor.ends_with(":chunk:1240:0"))
    );

    let empty_fetch = fetch_documents_json(&postgres, &FetchDocumentsQuery { document_ids: &[] })?;
    let empty_fetch: serde_json::Value =
        serde_json::from_str(&empty_fetch).expect("empty fetch response is stable JSON");
    assert_eq!(empty_fetch["documents"].as_array().unwrap().len(), 0);

    let fetch = fetch_documents_json(
        &postgres,
        &FetchDocumentsQuery {
            document_ids: &["legi:LEGIARTI000006419320@1804-02-21"],
        },
    )?;
    let fetch: serde_json::Value =
        serde_json::from_str(&fetch).expect("fetch response is stable JSON");
    assert_eq!(
        fetch["documents"][0]["document_id"],
        "legi:LEGIARTI000006419320@1804-02-21"
    );
    assert_eq!(
        fetch["documents"][0]["chunks"][0]["embedding_fingerprint"],
        "bge-m3:1024:normalize:true"
    );

    Ok(())
}
