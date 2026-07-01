//! Managed-Postgres integration tests for the re-ingest fingerprint-preservation fix.
//!
//! Re-projecting an existing chunk must NOT null a valid `chunks.embedding_fingerprint` when the
//! embedded text is unchanged, and MUST null (invalidate) it when `body`/`contextualized_body`
//! change so the embed selector re-selects the chunk. These tests pin both the projection CASE
//! (`projection/legi.rs`) and the embed pending-input selector (`dense.rs`).
//!
//! Skips when no local pgrx/pg_search-capable PostgreSQL is discoverable (same gate as the other
//! DB-backed storage tests).

mod common;

use common::{discover_pg_config, vector_literal};
use jurisearch_ingest::legi::{CanonicalChunk, CanonicalDocument};
use jurisearch_storage::{
    dense::load_chunk_embedding_inputs,
    projection::{ChunkEmbeddingInsert, insert_chunk_embeddings, insert_legi_documents},
    runtime::{ManagedPostgres, StorageError},
};

const EMBEDDING_FINGERPRINT: &str = "bge-m3:1024:normalize:true";
const SOURCE_UID: &str = "LEGIARTI000000000001";
const VALID_FROM: &str = "1990-01-01";

/// Build a single-chunk LEGI document whose chunk text is `(body, contextualized_body)`. The
/// `document_id`/`chunk_id` are stable across bodies so a re-project hits the `ON CONFLICT` path.
fn legi_doc(body: &str, contextualized_body: &str) -> CanonicalDocument {
    let document_id = format!("legi:{SOURCE_UID}@{VALID_FROM}");
    let chunk_id = format!("chunk:{document_id}:0");
    CanonicalDocument {
        document_id: document_id.clone(),
        source: "legi".to_owned(),
        kind: "article".to_owned(),
        source_uid: SOURCE_UID.to_owned(),
        version_group: None,
        citation: None,
        title: None,
        body: body.to_owned(),
        source_status: None,
        source_nature: None,
        source_article_type: None,
        valid_from: VALID_FROM.to_owned(),
        valid_to: None,
        valid_to_raw: None,
        source_url: None,
        source_payload_hash: "sha256:doc".to_owned(),
        source_archive: None,
        source_member_path: None,
        hierarchy_path: Vec::new(),
        publisher_edges: Vec::new(),
        chunks: vec![CanonicalChunk {
            chunk_id,
            document_id,
            chunk_index: 0,
            body: body.to_owned(),
            contextualized_body: contextualized_body.to_owned(),
            chunk_kind: "article_body".to_owned(),
            chunking: "structural".to_owned(),
            boundary: "article".to_owned(),
            source_fields: Vec::new(),
            source_payload_hash: "sha256:chunk".to_owned(),
            chunk_builder_version: "chunker:test".to_owned(),
            hierarchy_path: Vec::new(),
        }],
        canonical_version: "test".to_owned(),
    }
}

fn stamp_active(
    postgres: &ManagedPostgres,
    chunk_id: &str,
    vector: &str,
) -> Result<(), StorageError> {
    let embeddings = vec![ChunkEmbeddingInsert {
        chunk_id,
        embedding_fingerprint: EMBEDDING_FINGERPRINT,
        embedding_literal: vector,
        model: "bge-m3",
        dimension: 1024,
    }];
    assert_eq!(insert_chunk_embeddings(postgres, &embeddings, None)?, 1);
    Ok(())
}

fn parent_fingerprint(postgres: &ManagedPostgres, chunk_id: &str) -> Result<String, StorageError> {
    postgres
        .execute_sql(&format!(
            "SELECT coalesce(embedding_fingerprint, 'null') FROM chunks WHERE chunk_id = '{chunk_id}';"
        ))
        .map(|value| value.trim().to_owned())
}

/// Test 1: an unchanged re-project (same body, `None` fingerprint) PRESERVES a valid parent stamp,
/// and the embed selector then returns nothing for that chunk.
#[test]
fn unchanged_reproject_preserves_stamp() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("unchanged reproject preserves stamp")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-fp-preserve.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;

    let doc = legi_doc("body B", "context B");
    let chunk_id = doc.chunks[0].chunk_id.clone();

    // First projection: caller passes None → parent fingerprint starts NULL.
    insert_legi_documents(&postgres, std::slice::from_ref(&doc), None)?;
    assert_eq!(parent_fingerprint(&postgres, &chunk_id)?, "null");

    // Stamp the active fingerprint (writes chunk_embeddings + stamps chunks.embedding_fingerprint).
    stamp_active(&postgres, &chunk_id, &vector_literal(0))?;
    assert_eq!(
        parent_fingerprint(&postgres, &chunk_id)?,
        EMBEDDING_FINGERPRINT
    );

    // Re-project the SAME chunk with the SAME text and None: the CASE must PRESERVE the stamp.
    insert_legi_documents(&postgres, std::slice::from_ref(&doc), None)?;
    assert_eq!(
        parent_fingerprint(&postgres, &chunk_id)?,
        EMBEDDING_FINGERPRINT,
        "unchanged reproject nulled a valid stamp"
    );

    // Selector must not re-select an already-embedded, unchanged chunk.
    let pending =
        load_chunk_embedding_inputs(&postgres, EMBEDDING_FINGERPRINT, "bge-m3", 1024, None)?;
    assert!(
        pending.iter().all(|input| input.chunk_id != chunk_id),
        "unchanged embedded chunk re-selected for embedding: {pending:?}"
    );

    postgres.stop()?;
    Ok(())
}

/// Test 2: a changed-body re-project INVALIDATES the parent stamp (NULL) and the selector re-selects
/// the chunk even though `chunk_embeddings` still holds the old, otherwise-matching row. The embed
/// input text reflects the new body.
#[test]
fn changed_body_reproject_invalidates_and_reselects() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("changed body reproject invalidates")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-fp-invalidate.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;

    let original = legi_doc("body B", "context B");
    let chunk_id = original.chunks[0].chunk_id.clone();

    insert_legi_documents(&postgres, std::slice::from_ref(&original), None)?;
    stamp_active(&postgres, &chunk_id, &vector_literal(0))?;
    assert_eq!(
        parent_fingerprint(&postgres, &chunk_id)?,
        EMBEDDING_FINGERPRINT
    );

    // Re-project the SAME chunk_id with CHANGED text: the CASE must NULL the parent stamp.
    let changed = legi_doc(
        "body B prime — nouveau texte",
        "context B prime — nouveau texte",
    );
    insert_legi_documents(&postgres, std::slice::from_ref(&changed), None)?;
    assert_eq!(
        parent_fingerprint(&postgres, &chunk_id)?,
        "null",
        "changed-body reproject failed to invalidate the parent stamp"
    );

    // The old chunk_embeddings row still matches fingerprint/model/dimension, so ONLY the new
    // `c.embedding_fingerprint IS NULL` clause can re-select it.
    let child = postgres
        .execute_sql(&format!(
            "SELECT count(*) FROM chunk_embeddings \
             WHERE chunk_id = '{chunk_id}' AND embedding_fingerprint = '{EMBEDDING_FINGERPRINT}' \
               AND model = 'bge-m3' AND dimension = 1024;"
        ))?
        .trim()
        .to_owned();
    assert_eq!(
        child, "1",
        "expected the stale child embedding to still be present"
    );

    let pending =
        load_chunk_embedding_inputs(&postgres, EMBEDDING_FINGERPRINT, "bge-m3", 1024, None)?;
    let selected = pending
        .iter()
        .find(|input| input.chunk_id == chunk_id)
        .expect("invalidated chunk must be re-selected for embedding");
    assert!(
        selected.embedding_text.contains("nouveau texte"),
        "embed input did not reflect the new body: {:?}",
        selected.embedding_text
    );

    postgres.stop()?;
    Ok(())
}

/// Test 3: a fresh chunk (NULL parent, no child embedding) is selected — unchanged baseline behavior.
#[test]
fn fresh_chunk_is_selected() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("fresh chunk selected")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-fp-fresh.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;

    let doc = legi_doc("fresh body", "fresh context");
    let chunk_id = doc.chunks[0].chunk_id.clone();
    insert_legi_documents(&postgres, std::slice::from_ref(&doc), None)?;
    assert_eq!(parent_fingerprint(&postgres, &chunk_id)?, "null");

    let pending =
        load_chunk_embedding_inputs(&postgres, EMBEDDING_FINGERPRINT, "bge-m3", 1024, None)?;
    assert!(
        pending.iter().any(|input| input.chunk_id == chunk_id),
        "fresh unembedded chunk was not selected: {pending:?}"
    );

    postgres.stop()?;
    Ok(())
}

/// Test 4 (optional): the direct-stamp convenience API — an unchanged re-project with `Some(fp)`
/// over a NULL parent stamps the parent to `fp`.
#[test]
fn direct_stamp_over_null_parent_stamps_fingerprint() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("direct stamp over null parent")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-fp-directstamp.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;

    let doc = legi_doc("stable body", "stable context");
    let chunk_id = doc.chunks[0].chunk_id.clone();

    // First projection with None → NULL parent.
    insert_legi_documents(&postgres, std::slice::from_ref(&doc), None)?;
    assert_eq!(parent_fingerprint(&postgres, &chunk_id)?, "null");

    // Re-project the SAME unchanged text with Some(fp): unchanged branch + non-null EXCLUDED stamps it.
    insert_legi_documents(
        &postgres,
        std::slice::from_ref(&doc),
        Some(EMBEDDING_FINGERPRINT),
    )?;
    assert_eq!(
        parent_fingerprint(&postgres, &chunk_id)?,
        EMBEDDING_FINGERPRINT,
        "direct-stamp API did not stamp the NULL parent"
    );

    postgres.stop()?;
    Ok(())
}
