mod common;

use common::{discover_pg_config, vector_literal};
use jurisearch_storage::{
    ingest_accounting::{
        IngestCompatibility, IngestErrorInput, IngestMemberInput, IngestMemberStatus,
        IngestResumeAction, IngestRunInput, IngestRunStatus, ReplaySnapshotMode, finish_ingest_run,
        ingest_resume_decision, invalidate_cached_query_readiness, load_cached_query_readiness,
        load_ingest_embedding_coverage, load_ingest_health,
        load_ingest_health_with_replay_snapshot_mode, load_ingest_readiness,
        load_or_compute_query_readiness, record_ingest_error, record_ingest_member,
        start_ingest_run, store_query_readiness, update_ingest_member_status,
    },
    runtime::{ManagedPostgres, StorageError},
};

#[test]
fn ingest_accounting_records_members_errors_and_resume_decisions() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("ingest accounting")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-ingest-accounting.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
    let compatibility = IngestCompatibility {
        parser_version: "legi-parser:v1",
        schema_version: "canonical:v1",
        code_version: "test-code-sha",
        source_payload_hash: "sha256:article-1240",
    };

    start_ingest_run(
        &postgres,
        &IngestRunInput {
            run_id: "run-1",
            source: "legi",
            parser_version: compatibility.parser_version,
            schema_version: compatibility.schema_version,
            code_version: compatibility.code_version,
            safe_mode: true,
            archive_plan_json: Some(
                r#"{"archives":["Freemium_legi_global_20240101-000000.tar.gz"]}"#,
            ),
            manifest_json: Some(r#"{"fixture":true}"#),
        },
    )?;

    let inserted = record_ingest_member(
        &postgres,
        &IngestMemberInput {
            run_id: "run-1",
            archive_name: "Freemium_legi_global_20240101-000000.tar.gz",
            member_path: "legi/articles/LEGIARTI000006419320.xml",
            source: "legi",
            source_entity: Some("LEGIARTI000006419320"),
            date_anchor: Some("1804-02-21"),
            status: IngestMemberStatus::Inserted,
            compatibility,
        },
    )?;
    assert_eq!(inserted.status, IngestMemberStatus::Inserted);
    assert_eq!(inserted.attempt_count, 1);

    let repeated_inserted = record_ingest_member(
        &postgres,
        &IngestMemberInput {
            run_id: "run-1",
            archive_name: "Freemium_legi_global_20240101-000000.tar.gz",
            member_path: "legi/articles/LEGIARTI000006419320.xml",
            source: "legi",
            source_entity: Some("LEGIARTI000006419320"),
            date_anchor: Some("1804-02-21"),
            status: IngestMemberStatus::Inserted,
            compatibility,
        },
    )?;
    assert_eq!(repeated_inserted.member_id, inserted.member_id);
    assert_eq!(repeated_inserted.attempt_count, 2);

    let compatible_inserted = ingest_resume_decision(
        &postgres,
        "Freemium_legi_global_20240101-000000.tar.gz",
        "legi/articles/LEGIARTI000006419320.xml",
        compatibility,
    )?;
    assert_eq!(compatible_inserted.action, IngestResumeAction::Skip);
    assert_eq!(
        compatible_inserted.previous_status,
        Some(IngestMemberStatus::Inserted)
    );

    let incompatible = ingest_resume_decision(
        &postgres,
        "Freemium_legi_global_20240101-000000.tar.gz",
        "legi/articles/LEGIARTI000006419320.xml",
        IngestCompatibility {
            parser_version: "legi-parser:v2",
            ..compatibility
        },
    )?;
    assert_eq!(incompatible.action, IngestResumeAction::BlockedIncompatible);
    assert_eq!(incompatible.mismatched_fields, vec!["parser_version"]);

    let payload_incompatible = ingest_resume_decision(
        &postgres,
        "Freemium_legi_global_20240101-000000.tar.gz",
        "legi/articles/LEGIARTI000006419320.xml",
        IngestCompatibility {
            source_payload_hash: "sha256:changed",
            ..compatibility
        },
    )?;
    assert_eq!(
        payload_incompatible.action,
        IngestResumeAction::BlockedIncompatible
    );
    assert_eq!(
        payload_incompatible.mismatched_fields,
        vec!["source_payload_hash"]
    );

    let failed = record_ingest_member(
        &postgres,
        &IngestMemberInput {
            run_id: "run-1",
            archive_name: "Freemium_legi_global_20240101-000000.tar.gz",
            member_path: "legi/articles/BROKEN.xml",
            source: "legi",
            source_entity: None,
            date_anchor: None,
            status: IngestMemberStatus::Failed,
            compatibility: IngestCompatibility {
                source_payload_hash: "sha256:broken",
                ..compatibility
            },
        },
    )?;
    let error_id = record_ingest_error(
        &postgres,
        &IngestErrorInput {
            run_id: "run-1",
            member_id: Some(failed.member_id),
            error_class: "validation_error",
            error_code: "validation_missing_required_field",
            message: "missing NUM",
            retry_policy: "none",
            context_json: Some(r#"{"field":"NUM"}"#),
        },
    )?;
    assert!(error_id > 0);

    let failed_retry = ingest_resume_decision(
        &postgres,
        "Freemium_legi_global_20240101-000000.tar.gz",
        "legi/articles/BROKEN.xml",
        IngestCompatibility {
            source_payload_hash: "sha256:broken",
            ..compatibility
        },
    )?;
    assert_eq!(failed_retry.action, IngestResumeAction::Retry);
    assert_eq!(failed_retry.reason, "previous_failed");

    let recovered = record_ingest_member(
        &postgres,
        &IngestMemberInput {
            run_id: "run-1",
            archive_name: "Freemium_legi_global_20240101-000000.tar.gz",
            member_path: "legi/articles/BROKEN.xml",
            source: "legi",
            source_entity: None,
            date_anchor: None,
            status: IngestMemberStatus::Skipped,
            compatibility: IngestCompatibility {
                source_payload_hash: "sha256:broken",
                ..compatibility
            },
        },
    )?;
    assert_eq!(recovered.member_id, failed.member_id);
    assert_eq!(recovered.status, IngestMemberStatus::Skipped);

    let parsed = record_ingest_member(
        &postgres,
        &IngestMemberInput {
            run_id: "run-1",
            archive_name: "Freemium_legi_global_20240101-000000.tar.gz",
            member_path: "legi/articles/UNFINISHED.xml",
            source: "legi",
            source_entity: Some("LEGIARTIunfinished"),
            date_anchor: None,
            status: IngestMemberStatus::Parsed,
            compatibility: IngestCompatibility {
                source_payload_hash: "sha256:unfinished",
                ..compatibility
            },
        },
    )?;
    update_ingest_member_status(
        &postgres,
        parsed.member_id,
        IngestMemberStatus::Parsed,
        Some("interrupted after parse"),
    )?;
    let unfinished_retry = ingest_resume_decision(
        &postgres,
        "Freemium_legi_global_20240101-000000.tar.gz",
        "legi/articles/UNFINISHED.xml",
        IngestCompatibility {
            source_payload_hash: "sha256:unfinished",
            ..compatibility
        },
    )?;
    assert_eq!(unfinished_retry.action, IngestResumeAction::Retry);
    assert_eq!(unfinished_retry.reason, "previous_unfinished");

    let missing_member_update =
        update_ingest_member_status(&postgres, -1, IngestMemberStatus::Failed, None);
    assert!(matches!(
        missing_member_update,
        Err(StorageError::IngestAccounting { .. })
    ));
    let non_terminal_finish = finish_ingest_run(&postgres, "run-1", IngestRunStatus::Running, None);
    assert!(matches!(
        non_terminal_finish,
        Err(StorageError::IngestAccounting { .. })
    ));

    insert_projection_fixture(&postgres)?;
    finish_ingest_run(&postgres, "run-1", IngestRunStatus::Completed, None)?;

    let cold_cached_health = load_ingest_health(&postgres)?;
    assert_eq!(cold_cached_health.replay_snapshot_status, "missing");
    assert_eq!(cold_cached_health.replay_snapshot_source, "missing");

    let health =
        load_ingest_health_with_replay_snapshot_mode(&postgres, ReplaySnapshotMode::Refresh)?;
    assert_eq!(health.latest_run_id.as_deref(), Some("run-1"));
    assert_eq!(health.latest_run_status.as_deref(), Some("completed"));
    assert_eq!(health.latest_completed_run_id.as_deref(), Some("run-1"));
    assert_eq!(health.latest_manifest["fixture"], true);
    assert_eq!(health.total_members, 3);
    assert_eq!(health.inserted_members, 1);
    assert_eq!(health.skipped_members, 1);
    assert_eq!(health.failed_members, 0);
    assert!(health.error_classes.is_empty());
    assert_eq!(health.projection_coverage.covered, 1);
    assert_eq!(health.projection_coverage.total, 2);
    assert_eq!(health.embedding_coverage.covered, 2);
    assert_eq!(health.embedding_coverage.total, 2);
    let readiness = load_ingest_readiness(&postgres)?;
    assert_eq!(
        readiness.projection_coverage.covered,
        health.projection_coverage.covered
    );
    assert_eq!(
        readiness.embedding_coverage.covered,
        health.embedding_coverage.covered
    );
    assert_eq!(
        load_ingest_embedding_coverage(&postgres)?.covered,
        health.embedding_coverage.covered
    );
    assert_eq!(health.replay_snapshot_status, "available");
    assert_eq!(health.replay_snapshot_source, "refreshed");
    assert_eq!(health.replay_snapshot.documents.count, 2);
    assert_eq!(health.replay_snapshot.chunks.count, 2);
    assert_eq!(health.replay_snapshot.publisher_edges.count, 0);
    assert_eq!(health.replay_snapshot.embeddings.count, 2);
    assert_eq!(health.replay_snapshot.manifests.count, 1);
    assert_eq!(health.replay_snapshot.signature.len(), 32);
    let replay_signature = health.replay_snapshot.signature.clone();
    let cached_health = load_ingest_health(&postgres)?;
    assert_eq!(cached_health.replay_snapshot_source, "cached");
    assert_eq!(cached_health.replay_snapshot.signature, replay_signature);
    let second_refresh =
        load_ingest_health_with_replay_snapshot_mode(&postgres, ReplaySnapshotMode::Refresh)?;
    assert_eq!(second_refresh.replay_snapshot.signature, replay_signature);
    assert_eq!(second_refresh.replay_snapshot.manifests.count, 1);
    postgres.execute_sql(
        "UPDATE index_manifest \
         SET value = '{\"snapshot\":{\"documents\":null}}'::jsonb \
         WHERE key = 'replay_snapshot';",
    )?;
    let corrupt_cached_health = load_ingest_health(&postgres)?;
    assert_eq!(corrupt_cached_health.replay_snapshot_status, "missing");
    assert_eq!(corrupt_cached_health.replay_snapshot_source, "missing");
    assert_eq!(
        load_ingest_health_with_replay_snapshot_mode(&postgres, ReplaySnapshotMode::Refresh)?
            .replay_snapshot
            .signature,
        replay_signature
    );
    postgres.execute_sql(
        "UPDATE documents \
         SET title = 'Article 1240 changed' \
         WHERE document_id = 'legi:LEGIARTI000006419320@1804-02-21';",
    )?;
    assert_eq!(
        load_ingest_health(&postgres)?.replay_snapshot.signature,
        replay_signature
    );
    assert_ne!(
        load_ingest_health_with_replay_snapshot_mode(&postgres, ReplaySnapshotMode::Refresh)?
            .replay_snapshot
            .signature,
        replay_signature
    );
    postgres.execute_sql(
        "UPDATE chunk_embeddings \
         SET embedding_fingerprint = 'stale-fingerprint' \
         WHERE chunk_id = 'chunk:1240:1';",
    )?;
    let stale_embedding_readiness = load_ingest_readiness(&postgres)?;
    assert_eq!(stale_embedding_readiness.embedding_coverage.covered, 1);
    assert_eq!(stale_embedding_readiness.embedding_coverage.total, 2);
    postgres.execute_sql(
        "UPDATE chunk_embeddings \
         SET embedding_fingerprint = 'bge-m3:1024:normalize:true' \
         WHERE chunk_id = 'chunk:1240:1';",
    )?;
    let repaired_embedding_readiness = load_ingest_readiness(&postgres)?;
    assert_eq!(repaired_embedding_readiness.embedding_coverage.covered, 2);
    assert_eq!(repaired_embedding_readiness.embedding_coverage.total, 2);
    assert!(health.recovery_warnings.is_empty());

    Ok(())
}

#[test]
fn query_readiness_cache_round_trips_and_invalidates() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("query readiness cache")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-readiness-cache.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
    insert_ready_fixture(&postgres)?;

    // Cold: no cached entry.
    assert!(load_cached_query_readiness(&postgres)?.is_none());

    // Store the live readiness report and read it back unchanged.
    let readiness = load_ingest_readiness(&postgres)?;
    assert_eq!(readiness.projection_coverage.covered, 1);
    assert_eq!(readiness.projection_coverage.total, 1);
    assert_eq!(readiness.embedding_coverage.covered, 1);
    assert_eq!(readiness.embedding_coverage.total, 1);
    store_query_readiness(&postgres, &readiness)?;
    let cached = load_cached_query_readiness(&postgres)?.expect("cache present after store");
    assert_eq!(cached, readiness);

    // Explicit invalidation drops the entry.
    invalidate_cached_query_readiness(&postgres)?;
    assert!(load_cached_query_readiness(&postgres)?.is_none());

    // Starting an ingest run also invalidates the cache.
    store_query_readiness(&postgres, &readiness)?;
    assert!(load_cached_query_readiness(&postgres)?.is_some());
    start_ingest_run(
        &postgres,
        &IngestRunInput {
            run_id: "readiness-run",
            source: "legi",
            parser_version: "legi-parser:v1",
            schema_version: "canonical:v1",
            code_version: "test-code-sha",
            safe_mode: false,
            archive_plan_json: None,
            manifest_json: None,
        },
    )?;
    assert!(load_cached_query_readiness(&postgres)?.is_none());

    Ok(())
}

#[test]
fn query_readiness_cache_is_trusted_until_invalidated() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("query readiness staleness")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-readiness-stale.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
    insert_ready_fixture(&postgres)?;

    // First call computes coverage live and caches it (index is fully ready).
    let (computed, from_cache) = load_or_compute_query_readiness(&postgres)?;
    assert!(!from_cache);
    assert_eq!(computed.embedding_coverage.covered, 1);
    assert_eq!(computed.embedding_coverage.total, 1);
    // Second call serves the cache without recomputing.
    let (_, from_cache) = load_or_compute_query_readiness(&postgres)?;
    assert!(from_cache);

    // A coverage-breaking mutation WITHOUT invalidation leaves the cache trusted (stale). This is
    // the invariant every mutation path must uphold: invalidate after changing coverage, or the
    // hot path will keep reporting the index ready (the bug that allowed a standalone hierarchy
    // backfill to leave a stale "ready" entry).
    postgres.execute_sql("DELETE FROM chunk_embeddings;")?;
    let (stale, from_cache) = load_or_compute_query_readiness(&postgres)?;
    assert!(from_cache);
    assert_eq!(
        stale.embedding_coverage.covered, 1,
        "stale cache still reports complete"
    );

    // After invalidation the next check recomputes and sees the now-incomplete embedding coverage.
    invalidate_cached_query_readiness(&postgres)?;
    let (recomputed, from_cache) = load_or_compute_query_readiness(&postgres)?;
    assert!(!from_cache);
    assert_eq!(recomputed.embedding_coverage.covered, 0);
    assert_eq!(recomputed.embedding_coverage.total, 1);

    Ok(())
}

/// Minimal fully-ready index: one document, one chunk, one matching embedding.
fn insert_ready_fixture(postgres: &ManagedPostgres) -> Result<(), StorageError> {
    postgres.execute_sql(&format!(
        "INSERT INTO documents \
           (document_id, source, kind, source_uid, citation, title, body, \
            valid_from, source_payload_hash, canonical_json) \
         VALUES \
           ('legi:LEGIARTI000006419320@1804-02-21', 'legi', 'article', \
            'LEGIARTI000006419320', 'Code civil article 1240', \
            'Article 1240', 'Tout fait quelconque de l''homme...', '1804-02-21', \
            'sha256:article-1240', '{{\"official\":true}}'); \
         INSERT INTO chunks \
           (chunk_id, document_id, chunk_index, body, contextualized_body, source_payload_hash, \
            chunk_builder_version, embedding_fingerprint) \
         VALUES \
           ('chunk:1240:0', 'legi:LEGIARTI000006419320@1804-02-21', 0, \
            'responsabilite civile article 1240', \
            'Code civil > Article 1240\nresponsabilite civile article 1240', \
            'sha256:article-1240', 'chunker:v0', 'bge-m3:1024:normalize:true'); \
         INSERT INTO chunk_embeddings \
           (chunk_id, embedding_fingerprint, embedding, model, dimension) \
         VALUES \
           ('chunk:1240:0', 'bge-m3:1024:normalize:true', '{}', 'bge-m3', 1024);",
        vector_literal(0)
    ))?;
    Ok(())
}

fn insert_projection_fixture(postgres: &ManagedPostgres) -> Result<(), StorageError> {
    postgres.execute_sql(&format!(
        "INSERT INTO documents \
           (document_id, source, kind, source_uid, citation, title, body, \
            valid_from, source_payload_hash, canonical_json) \
         VALUES \
           ('legi:LEGIARTI000006419320@1804-02-21', 'legi', 'article', \
            'LEGIARTI000006419320', 'Code civil article 1240', \
            'Article 1240', 'Tout fait quelconque de l''homme...', '1804-02-21', \
            'sha256:article-1240', '{{\"official\":true}}'), \
           ('legi:LEGIARTI000000000001@2024-01-01', 'legi', 'article', \
            'LEGIARTI000000000001', 'Unprojected fixture', \
            'Article fixture', 'Document deliberately left without chunks.', '2024-01-01', \
            'sha256:article-without-chunks', '{{\"official\":true}}'); \
         INSERT INTO chunks \
           (chunk_id, document_id, chunk_index, body, contextualized_body, source_payload_hash, \
            chunk_builder_version, embedding_fingerprint) \
         VALUES \
           ('chunk:1240:0', 'legi:LEGIARTI000006419320@1804-02-21', 0, \
            'responsabilite civile article 1240', \
            'Code civil > Article 1240\nresponsabilite civile article 1240', \
            'sha256:article-1240', \
            'chunker:v0', 'bge-m3:1024:normalize:true'), \
           ('chunk:1240:1', 'legi:LEGIARTI000006419320@1804-02-21', 1, \
            'dommage faute reparation', \
            'Code civil > Article 1240\ndommage faute reparation', \
            'sha256:article-1240', \
            'chunker:v0', 'bge-m3:1024:normalize:true'); \
         INSERT INTO chunk_embeddings \
           (chunk_id, embedding_fingerprint, embedding, model, dimension) \
         VALUES \
           ('chunk:1240:0', 'bge-m3:1024:normalize:true', '{}', 'bge-m3', 1024), \
           ('chunk:1240:1', 'bge-m3:1024:normalize:true', '{}', 'bge-m3', 1024);",
        vector_literal(0),
        vector_literal(1)
    ))?;
    Ok(())
}
