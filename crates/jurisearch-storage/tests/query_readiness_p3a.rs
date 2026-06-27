//! work/09 P3A acceptance: writer-owned readiness STAMP (apply-time coverage gate + read-only lookup,
//! a missing/stale stamp errors) and the fail-closed embedding-fingerprint preflight. Single-corpus;
//! skips cleanly when the managed PG harness is absent.

mod common;

use common::{discover_pg_config, vector_literal};
use jurisearch_storage::{
    generations::{
        ActivationStamps, activate_generation, create_generation_from_public, generation_schema,
    },
    ingest_accounting::{load_query_readiness, stamp_query_readiness},
    retrieval::{
        DecisionFilters, GroupBy, HybridCandidateQuery, RetrievalMode, RetrievalOptions,
        hybrid_candidates_json,
    },
    runtime::{ManagedPostgres, StorageError},
};

const FP: &str = "bge-m3:1024:cls:normalize=true";

fn stamps() -> ActivationStamps<'static> {
    ActivationStamps {
        sequence: 1,
        baseline_id: "core-p3a-g0001",
        schema_version: 24,
        embedding_fingerprint: FP,
        builder_versions: &serde_json::Value::Null,
        last_package_id: None,
        last_package_digest: None,
    }
}

/// Seed a tiny, fully query-ready `core` corpus in `public` (one decision + a BM25/dense-indexed chunk
/// with a matching embedding under fingerprint [`FP`]).
fn seed_ready_core(postgres: &ManagedPostgres) -> Result<(), StorageError> {
    postgres.execute_sql(
        "INSERT INTO documents (document_id, source, kind, source_uid, citation, title, body, \
           valid_from, source_payload_hash, canonical_json) \
         VALUES ('cass:P3A','cass','decision','cass:P3A','Cass','Arret', \
           'la responsabilite du transporteur','2024-01-01','sha256:p3a','{}'); \
         INSERT INTO chunks (chunk_id, document_id, chunk_index, body, contextualized_body, \
           source_payload_hash, chunk_builder_version, embedding_fingerprint) \
         VALUES ('cass:P3A#0','cass:P3A',0,'la responsabilite du transporteur', \
           'ctx responsabilite','sha256:c','c1','bge-m3:1024:cls:normalize=true');",
    )?;
    let vector = vector_literal(3);
    postgres.execute_sql(&format!(
        "INSERT INTO chunk_embeddings (chunk_id, embedding_fingerprint, embedding, model, dimension) \
         VALUES ('cass:P3A#0','{FP}','{vector}'::vector,'m',1024);"
    ))?;
    Ok(())
}

#[test]
fn readiness_is_writer_stamped_and_the_read_path_is_a_lookup() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("p3a readiness stamp")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-p3a-stamp.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
    seed_ready_core(&postgres)?;

    let generation = create_generation_from_public(&postgres, "core", 1, Some("core-p3a-g0001"))?;
    activate_generation(&postgres, "core", &generation, &stamps(), None)?;

    // The writer stamped readiness at activation; the read path is a pure lookup.
    let report = load_query_readiness(&postgres)?;
    assert_eq!(report.projection_coverage.covered, 1);
    assert_eq!(report.embedding_coverage.covered, 1);

    // A STALE stamp (the active topology's sequence advanced without restamping) errors — never a
    // recompute. (Bump the cursor out from under the stamp, then restore it.)
    postgres.execute_sql(
        "UPDATE jurisearch_control.corpus_state SET sequence = sequence + 1 WHERE corpus = 'core';",
    )?;
    let stale = load_query_readiness(&postgres).expect_err("a stale stamp must error");
    assert!(
        stale.to_string().to_lowercase().contains("stale"),
        "stale stamp errors clearly: {stale}"
    );
    postgres.execute_sql(
        "UPDATE jurisearch_control.corpus_state SET sequence = sequence - 1 WHERE corpus = 'core';",
    )?;
    assert!(
        load_query_readiness(&postgres).is_ok(),
        "restored signature resolves again"
    );

    // A MISSING stamp is a writer/apply fault — the read errors (never recomputes/writes).
    postgres.execute_sql("DELETE FROM public.index_manifest WHERE key = 'query_readiness';")?;
    let missing = load_query_readiness(&postgres).expect_err("a missing stamp must error");
    assert!(
        missing.to_string().to_lowercase().contains("never stamped")
            || missing
                .to_string()
                .to_lowercase()
                .contains("writer/apply fault"),
        "missing stamp errors clearly: {missing}"
    );

    Ok(())
}

#[test]
fn the_stamp_gate_rejects_an_incomplete_generation() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("p3a coverage gate")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-p3a-gate.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
    seed_ready_core(&postgres)?;
    let generation = create_generation_from_public(&postgres, "core", 1, Some("core-p3a-g0001"))?;
    activate_generation(&postgres, "core", &generation, &stamps(), None)?;

    // Drop the active generation's embeddings, then call the stamp helper directly inside a
    // transaction: the coverage gate refuses (this is the same gate activation + incremental run).
    postgres.execute_sql(&format!(
        "DELETE FROM {}.chunk_embeddings;",
        generation_schema("core", 1)
    ))?;
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    let mut tx = client.transaction().map_err(StorageError::PostgresClient)?;
    let error = stamp_query_readiness(&mut tx, &generation)
        .expect_err("the gate must reject an incomplete generation");
    assert!(
        error.to_string().to_lowercase().contains("coverage"),
        "the gate cites incomplete coverage: {error}"
    );
    Ok(())
}

#[test]
fn the_stamp_gate_rejects_a_generation_whose_fingerprint_differs_from_the_active_stamp()
-> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("p3a fingerprint gate")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-p3a-fpgate.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
    seed_ready_core(&postgres)?; // chunk + embedding are internally consistent with FP

    let generation = create_generation_from_public(&postgres, "core", 1, Some("core-p3a-g0001"))?;
    // Activate with a DIFFERENT active fingerprint than the generation's rows carry: the rows are
    // self-consistent, but dense retrieval (which filters by the ACTIVE fingerprint) would find zero
    // vectors — so the gate must compute dense coverage against the active fingerprint and refuse.
    let mismatched = ActivationStamps {
        embedding_fingerprint: "a-different-fingerprint",
        ..stamps()
    };
    let error = activate_generation(&postgres, "core", &generation, &mismatched, None)
        .expect_err("a fingerprint-mismatched generation is not query-ready");
    assert!(
        error.to_string().to_lowercase().contains("coverage"),
        "the gate refuses on dense coverage against the active fingerprint: {error}"
    );
    // Nothing activated: the cursor is unchanged.
    let cursor = postgres.execute_sql(
        "SELECT count(*)::text FROM jurisearch_control.corpus_state WHERE corpus='core';",
    )?;
    assert_eq!(
        cursor.trim(),
        "0",
        "the mismatched activation aborts cursor-unchanged"
    );
    Ok(())
}

#[test]
fn embedding_fingerprint_preflight_fails_closed_for_dense_and_hybrid_not_bm25()
-> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("p3a fingerprint preflight")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-p3a-fp.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
    seed_ready_core(&postgres)?;
    let generation = create_generation_from_public(&postgres, "core", 1, Some("core-p3a-g0001"))?;
    activate_generation(&postgres, "core", &generation, &stamps(), None)?;

    let query = |mode: RetrievalMode, fingerprint: Option<&'static str>| HybridCandidateQuery {
        query_text: "responsabilite transporteur",
        query_embedding: Some("[1,0,0]"),
        embedding_fingerprint: fingerprint,
        retrieval_mode: mode,
        group_by: GroupBy::Chunk,
        options: RetrievalOptions::default(),
        after_cursor: None,
        as_of: "2024-06-01",
        kind_filter: None,
        decision_filters: DecisionFilters::default(),
        project_authority: false,
        lexical_limit: 10,
        dense_limit: 10,
        limit: 10,
    };
    let is_fp_mismatch = |result: &Result<String, StorageError>| matches!(result, Err(error) if error.to_string().contains("embedding_fingerprint_mismatch"));

    // A WRONG query fingerprint fails CLOSED for dense and hybrid (before retrieval) — no silent
    // degrade to lexical, no false empty dense result.
    assert!(is_fp_mismatch(&hybrid_candidates_json(
        &postgres,
        &query(RetrievalMode::Hybrid, Some("WRONG-FP"))
    )));
    assert!(is_fp_mismatch(&hybrid_candidates_json(
        &postgres,
        &query(RetrievalMode::Dense, Some("WRONG-FP"))
    )));
    // BM25 does not use dense, so the preflight does not apply (a wrong fingerprint is irrelevant).
    assert!(!is_fp_mismatch(&hybrid_candidates_json(
        &postgres,
        &query(RetrievalMode::Bm25, Some("WRONG-FP"))
    )));
    // The MATCHING fingerprint passes the preflight (the result is not a fingerprint error).
    assert!(!is_fp_mismatch(&hybrid_candidates_json(
        &postgres,
        &query(RetrievalMode::Hybrid, Some(FP))
    )));

    Ok(())
}
