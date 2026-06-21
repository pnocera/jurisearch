mod common;

use common::{discover_pg_config, vector_literal};
use jurisearch_storage::{
    ingest_accounting::{
        IngestCompatibility, IngestErrorInput, IngestMemberInput, IngestMemberStatus,
        IngestResumeAction, IngestRunInput, IngestRunStatus, finish_ingest_run,
        ingest_resume_decision, load_ingest_health, record_ingest_error, record_ingest_member,
        start_ingest_run, update_ingest_member_status,
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

    let health = load_ingest_health(&postgres)?;
    assert_eq!(health.latest_run_id.as_deref(), Some("run-1"));
    assert_eq!(health.latest_run_status.as_deref(), Some("completed"));
    assert_eq!(health.latest_completed_run_id.as_deref(), Some("run-1"));
    assert_eq!(health.total_members, 3);
    assert_eq!(health.inserted_members, 1);
    assert_eq!(health.failed_members, 1);
    assert_eq!(
        health.error_classes[0].error_code,
        "validation_missing_required_field"
    );
    assert_eq!(health.projection_coverage.covered, 1);
    assert_eq!(health.projection_coverage.total, 2);
    assert_eq!(health.embedding_coverage.covered, 2);
    assert_eq!(health.embedding_coverage.total, 2);
    assert!(
        health
            .recovery_warnings
            .iter()
            .any(|warning| { warning == "1 member(s) failed in latest ingest run" })
    );

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
           (chunk_id, document_id, chunk_index, body, source_payload_hash, \
            chunk_builder_version, embedding_fingerprint) \
         VALUES \
           ('chunk:1240:0', 'legi:LEGIARTI000006419320@1804-02-21', 0, \
            'responsabilite civile article 1240', 'sha256:article-1240', \
            'chunker:v0', 'bge-m3:1024:normalize:true'), \
           ('chunk:1240:1', 'legi:LEGIARTI000006419320@1804-02-21', 1, \
            'dommage faute reparation', 'sha256:article-1240', \
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
