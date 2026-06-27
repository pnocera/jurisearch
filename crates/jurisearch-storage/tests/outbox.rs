mod common;

use common::discover_pg_config;
use jurisearch_package::ChangeSeq;
use jurisearch_package::event::EventKind;
use jurisearch_storage::{
    decision_zones::{UpsertDecisionZones, upsert_decision_zones_with_client},
    dense::{DenseRebuildSpec, finalize_dense_rebuild},
    legislation_citations::{
        InsertCitationOccurrence, finalize_citation_occurrence_counts,
        insert_citation_occurrence_with_client, upsert_citation_resolution_pending_with_client,
    },
    official_api_archive::{InsertOfficialApiResponse, insert_official_api_response_with_client},
    outbox::{
        DigestSource, OutboxContext, OutboxEvent, acquire_outbox_fence, corpus_table_digests,
        corpus_table_digests_with_client, current_change_seq, current_change_seq_with_client,
        emit_change, release_outbox_fence, scope_kind, scopes_changed_for_corpus,
    },
    projection::{ChunkEmbeddingInsert, insert_chunk_embeddings},
    runtime::{ManagedPostgres, StorageError},
    zone_units::{
        ZoneUnitEmbeddingInsert, ZoneUnitRow, finalize_zone_dense_rebuild,
        insert_zone_unit_embeddings, replace_zone_units_for_document,
    },
};

/// Seed one Cassation decision (source `cass` → corpus `core`).
fn seed_decision(postgres: &ManagedPostgres, document_id: &str) -> Result<(), StorageError> {
    postgres
        .execute_sql(&format!(
            "INSERT INTO documents (document_id, source, kind, source_uid, citation, title, body, \
           valid_from, source_payload_hash, canonical_json) \
         VALUES ('{document_id}','cass','decision','{document_id}','Cass','Arret','corps', \
           '2024-01-01','sha256:{document_id}','{{}}');"
        ))
        .map(|_| ())
}

#[test]
fn replace_set_emits_exactly_one_row_and_read_api_reconstructs() -> Result<(), StorageError> {
    // INV-2: a derived rebuild (`replace_zone_units_for_document`) emits exactly ONE document-scoped
    // `replace_set` outbox row — never per-row deletes — and the §5.1 read API reconstructs it.
    let Some(pg_config) = discover_pg_config("outbox replace_set")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-outbox.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;

    seed_decision(&postgres, "cass:DOC1")?;
    let before = current_change_seq(&postgres)?;

    let ctx = OutboxContext::new("test-run", 19);
    let rows = vec![
        ZoneUnitRow {
            document_id: "cass:DOC1",
            zone: "motivations",
            fragment_index: 0,
            body: "motif",
            search_body: "motif",
            source: "cass",
            text_hash: "h",
            builder_version: "zone-units:v1",
        },
        ZoneUnitRow {
            document_id: "cass:DOC1",
            zone: "moyens",
            fragment_index: 0,
            body: "moyen",
            search_body: "moyen",
            source: "cass",
            text_hash: "h",
            builder_version: "zone-units:v1",
        },
    ];
    replace_zone_units_for_document(&postgres, "cass:DOC1", &rows, Some(&ctx))?;

    // Exactly one replace_set row for the document — not one per zone unit.
    let replace_rows = postgres.execute_sql(
        "SELECT count(*)::text FROM package_change_log \
         WHERE table_name='zone_units' AND op='replace_set' AND scope_key='cass:DOC1';",
    )?;
    assert_eq!(
        replace_rows.trim(),
        "1",
        "exactly one replace_set per document (INV-2)"
    );
    let per_row_deletes =
        postgres.execute_sql("SELECT count(*)::text FROM package_change_log WHERE op='delete';")?;
    assert_eq!(
        per_row_deletes.trim(),
        "0",
        "never per-row deletes for a derived rebuild"
    );

    // The read API reconstructs the changed scope between the two change_seq watermarks.
    let head = current_change_seq(&postgres)?;
    let changed = scopes_changed_for_corpus(&postgres, "core", before, head)?;
    let zone_scope = changed
        .iter()
        .find(|s| s.table_name == "zone_units")
        .expect("zone_units scope present");
    assert_eq!(zone_scope.op, EventKind::ReplaceSet);
    assert_eq!(zone_scope.scope_kind, scope_kind::DOCUMENT);
    assert_eq!(zone_scope.scope_key, "cass:DOC1");

    // A different corpus sees none of these changes (the read API is corpus-scoped, §5.1).
    let other = scopes_changed_for_corpus(&postgres, "inpi", before, head)?;
    assert!(other.is_empty(), "read API is corpus-scoped");
    Ok(())
}

#[test]
fn emit_in_a_rolled_back_transaction_leaves_no_orphan() -> Result<(), StorageError> {
    // The emit-in-same-txn invariant: a forced rollback discards the outbox row too.
    let Some(pg_config) = discover_pg_config("outbox rollback")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-outbox-rb.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;

    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    let ctx = OutboxContext::new("rollback-run", 19);
    {
        let mut tx = client.transaction().map_err(StorageError::PostgresClient)?;
        emit_change(
            &mut tx,
            &ctx,
            &OutboxEvent::scope(
                "core",
                "documents",
                EventKind::Upsert,
                scope_kind::DOCUMENT,
                "legi:X@2020-01-01",
            ),
        )?;
        // Drop without commit -> rollback.
        tx.rollback().map_err(StorageError::PostgresClient)?;
    }
    let count = postgres.execute_sql("SELECT count(*)::text FROM package_change_log;")?;
    assert_eq!(
        count.trim(),
        "0",
        "rolled-back emit leaves no orphan outbox row"
    );
    Ok(())
}

#[test]
fn read_api_window_is_half_open_and_ordered() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("outbox window")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-outbox-win.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;

    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    let ctx = OutboxContext::new("win-run", 19);
    let mut seqs = Vec::new();
    for key in ["a", "b", "c"] {
        let seq = emit_change(
            &mut client,
            &ctx,
            &OutboxEvent::scope(
                "core",
                "documents",
                EventKind::Upsert,
                scope_kind::DOCUMENT,
                key,
            ),
        )?;
        seqs.push(seq);
    }
    // Half-open (after, through]: excluding the first seq returns only b, c, in order.
    let changed = scopes_changed_for_corpus(&postgres, "core", seqs[0], seqs[2])?;
    let keys: Vec<&str> = changed.iter().map(|s| s.scope_key.as_str()).collect();
    assert_eq!(keys, vec!["b", "c"], "half-open window, change_seq order");

    // current_change_seq is the max.
    assert_eq!(current_change_seq(&postgres)?, seqs[2]);
    Ok(())
}

#[test]
fn corpus_digests_are_independent_of_the_outbox() -> Result<(), StorageError> {
    // §5.4 backstop: corpus_table_digests reads authoritative tables (not the ledger) and returns
    // per-table counts + a stable digest. Built in P1, wired by P3.
    let Some(pg_config) = discover_pg_config("outbox digests")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-outbox-dg.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;

    seed_decision(&postgres, "cass:DG1")?;
    seed_decision(&postgres, "cass:DG2")?;
    // A chunk + embedding so the digest exercises `to_jsonb(<vector>)` (a real column type).
    postgres.execute_sql(
        "INSERT INTO chunks (chunk_id, document_id, chunk_index, body, contextualized_body, \
           source_payload_hash, chunk_builder_version) \
         VALUES ('cass:DG1#0','cass:DG1',0,'body','ctx body','sha256:c','c1');",
    )?;
    let vector = common::vector_literal(0);
    postgres.execute_sql(&format!(
        "INSERT INTO chunk_embeddings (chunk_id, embedding_fingerprint, embedding, model, dimension) \
         VALUES ('cass:DG1#0','fp','{vector}'::vector,'m',1024);"
    ))?;
    // An archived response so we can prove a previously-omitted column changes the digest.
    postgres.execute_sql(
        "INSERT INTO official_api_responses (provider, api_environment, endpoint, http_method, \
           subject_document_id, request_fingerprint, outcome, response_body_sha256, corpus) \
         VALUES ('judilibre','production','/decision','GET','cass:DG1','fp','ok','sha256:x','core');",
    )?;

    let digests = corpus_table_digests(&postgres, "core", DigestSource::ProducerPublic)?;
    let documents = digests
        .iter()
        .find(|d| d.table_name == "documents")
        .expect("documents digest present");
    assert_eq!(documents.row_count, 2);
    assert!(documents.digest.starts_with("md5:"));
    // `to_jsonb(<vector>)` worked and produced a non-empty content digest.
    let embeddings = digests
        .iter()
        .find(|d| d.table_name == "chunk_embeddings")
        .expect("chunk_embeddings digest present");
    assert_eq!(embeddings.row_count, 1);
    assert_ne!(embeddings.digest, "md5:");

    // A change to a column the OLD hand-written signature omitted (official_api_responses.error)
    // now changes the digest — the §5.4 backstop covers all replicated content (WARN-1 fix).
    let before = corpus_table_digests(&postgres, "core", DigestSource::ProducerPublic)?
        .into_iter()
        .find(|d| d.table_name == "official_api_responses")
        .expect("archive digest")
        .digest;
    postgres.execute_sql(
        "UPDATE official_api_responses SET error = 'mutated' WHERE subject_document_id = 'cass:DG1';",
    )?;
    let after = corpus_table_digests(&postgres, "core", DigestSource::ProducerPublic)?
        .into_iter()
        .find(|d| d.table_name == "official_api_responses")
        .expect("archive digest")
        .digest;
    assert_ne!(before, after, "a replicated-column change flips the digest");

    // A non-existent corpus has zero rows everywhere.
    let empty = corpus_table_digests(&postgres, "inpi", DigestSource::ProducerPublic)?;
    assert!(empty.iter().all(|d| d.row_count == 0));
    let _ = ChangeSeq::ZERO; // (imported for the read API window seeds elsewhere)
    Ok(())
}

#[test]
fn citation_writer_mutation_and_emit_roll_back_together() -> Result<(), StorageError> {
    // BLOCKER fix proof: when a citation occurrence is written + emitted inside one transaction, a
    // rollback (the failure path) discards BOTH the data row and the outbox row — no orphan either way.
    let Some(pg_config) = discover_pg_config("outbox citation rollback")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-outbox-cite-rb.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;

    seed_decision(&postgres, "cass:CITE1")?;
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    // An archived response the occurrence FKs.
    let response_id: i64 = client
        .query_one(
            "INSERT INTO official_api_responses (provider, api_environment, endpoint, http_method, \
               subject_document_id, request_fingerprint, outcome, response_body_sha256, corpus) \
             VALUES ('judilibre','production','/decision','GET','cass:CITE1','fp','ok','sha256:x','core') \
             RETURNING response_id;",
            &[],
        )
        .map_err(StorageError::PostgresClient)?
        .get("response_id");

    let ctx = OutboxContext::new("cite-rb-run", 19);
    {
        let mut tx = client.transaction().map_err(StorageError::PostgresClient)?;
        insert_citation_occurrence_with_client(
            &mut tx,
            &InsertCitationOccurrence {
                decision_document_id: "cass:CITE1",
                decision_source_uid: "cass:CITE1",
                source_response_id: response_id,
                visa_index: 0,
                citation_key: "legi-cite:x",
                article_number_raw: Some("1"),
                article_number_norm: "1",
                code_name_raw: Some("code"),
                code_name_norm: "code",
                canonical_query: "1 code",
                legifrance_url: None,
                raw_title: "Article 1 du code",
                extraction_method: "visa_title_regex",
            },
            Some(&ctx),
        )?;
        // The mutation + its emit are staged; the failure path rolls the whole unit back.
        tx.rollback().map_err(StorageError::PostgresClient)?;
    }

    assert_eq!(
        postgres.execute_sql("SELECT count(*)::text FROM decision_legislation_citations;")?,
        "0",
        "rolled-back occurrence leaves no data row"
    );
    assert_eq!(
        postgres.execute_sql("SELECT count(*)::text FROM package_change_log;")?,
        "0",
        "rolled-back occurrence leaves no ledger row"
    );
    Ok(())
}

#[test]
fn every_directly_drivable_hooked_writer_emits_its_outbox_row() -> Result<(), StorageError> {
    // NIT fix: enumerated hook-coverage — drive one owned writer per §4.2 table/group and assert the
    // expected (table, op, scope_kind) outbox row. (documents / legi_metadata_roots / chunks are
    // covered by the CLI end-to-end ingest test; here we drive the remaining writers directly.)
    let Some(pg_config) = discover_pg_config("outbox hook coverage")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-outbox-cov.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
    seed_decision(&postgres, "cass:COV1")?;
    // chunk + zone_unit prerequisites.
    postgres.execute_sql(
        "INSERT INTO chunks (chunk_id, document_id, chunk_index, body, contextualized_body, \
           source_payload_hash, chunk_builder_version) \
         VALUES ('cass:COV1#0','cass:COV1',0,'b','ctx','sha256:c','c1');",
    )?;
    let ctx = OutboxContext::new("cov-run", 19);

    // official_api_responses -> upsert / official_api_response
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    insert_official_api_response_with_client(
        &mut client,
        &InsertOfficialApiResponse {
            provider: "judilibre",
            api_environment: "production",
            endpoint: "/decision",
            http_method: "GET",
            subject_document_id: Some("cass:COV1"),
            subject_source_uid: Some("cass:COV1"),
            provider_object_id: None,
            citation_key: None,
            corpus: None,
            request_fingerprint: "fp",
            request_url: None,
            request_json: &serde_json::json!({}),
            request_body: None,
            outcome: "ok",
            http_status: Some(200),
            response_body: "",
            response_json: None,
            response_body_sha256: "sha256:y",
            error: None,
            run_id: None,
            code_version: Some("t"),
        },
        Some(&ctx),
    )?;

    // decision_zones -> replace_set / document
    upsert_decision_zones_with_client(
        &mut client,
        &UpsertDecisionZones {
            document_id: "cass:COV1",
            provider: "judilibre",
            provider_decision_id: None,
            source_uid: "cass:COV1",
            ecli: None,
            status: "ok",
            upstream_update_date: None,
            upstream_decision_date: None,
            text_hash: Some("h"),
            offset_unit: Some("char"),
            zones_json: &serde_json::json!({}),
            raw_json: &serde_json::json!({}),
            error: None,
            ttl_seconds: Some(86_400),
        },
        Some(&ctx),
    )?;

    // chunk_embeddings -> upsert / document
    let vector = common::vector_literal(0);
    insert_chunk_embeddings(
        &postgres,
        &[ChunkEmbeddingInsert {
            chunk_id: "cass:COV1#0",
            embedding_fingerprint: "fp",
            embedding_literal: &vector,
            model: "m",
            dimension: 1024,
        }],
        Some(&ctx),
    )?;

    // zone_units (replace_set) + zone_unit_embeddings (upsert)
    replace_zone_units_for_document(
        &postgres,
        "cass:COV1",
        &[ZoneUnitRow {
            document_id: "cass:COV1",
            zone: "motivations",
            fragment_index: 0,
            body: "m",
            search_body: "m",
            source: "cass",
            text_hash: "h",
            builder_version: "zone-units:v1",
        }],
        Some(&ctx),
    )?;
    insert_zone_unit_embeddings(
        &postgres,
        &[ZoneUnitEmbeddingInsert {
            zone_unit_id: "cass:COV1#motivations#0",
            embedding_fingerprint: "fp",
            embedding_literal: &vector,
            model: "m",
            dimension: 1024,
        }],
        Some(&ctx),
    )?;

    // citation occurrence (upsert/document) + resolution (upsert/citation_resolution)
    let response_id: i64 = client
        .query_one(
            "SELECT response_id FROM official_api_responses LIMIT 1;",
            &[],
        )
        .map_err(StorageError::PostgresClient)?
        .get("response_id");
    insert_citation_occurrence_with_client(
        &mut client,
        &InsertCitationOccurrence {
            decision_document_id: "cass:COV1",
            decision_source_uid: "cass:COV1",
            source_response_id: response_id,
            visa_index: 0,
            citation_key: "legi-cite:cov",
            article_number_raw: Some("1"),
            article_number_norm: "1",
            code_name_raw: Some("c"),
            code_name_norm: "c",
            canonical_query: "1 c",
            legifrance_url: None,
            raw_title: "Article 1 du c",
            extraction_method: "visa_title_regex",
        },
        Some(&ctx),
    )?;
    upsert_citation_resolution_pending_with_client(
        &mut client,
        "legi-cite:cov",
        "1",
        "c",
        "1 c",
        "cass:COV1",
        Some(&ctx),
    )?;

    // Assert each §4.2 hooked writer produced its expected (op, scope_kind, table) row.
    let captured = postgres.execute_sql(
        "SELECT string_agg(DISTINCT op || ':' || scope_kind || ':' || table_name, ',' \
             ORDER BY op || ':' || scope_kind || ':' || table_name) FROM package_change_log;",
    )?;
    let captured: std::collections::BTreeSet<&str> = captured.trim().split(',').collect();
    for expected in [
        "replace_set:document:decision_zones",
        "replace_set:document:zone_units",
        "upsert:citation_resolution:legislation_citation_resolutions",
        // The embedding writers stamp the parent table's fingerprint, so they emit the parent
        // (chunks / zone_units) scope paired with the child (WARN-2 fix).
        "upsert:document:chunks",
        "upsert:document:chunk_embeddings",
        "upsert:document:decision_legislation_citations",
        "upsert:document:zone_units",
        "upsert:document:zone_unit_embeddings",
        "upsert:official_api_response:official_api_responses",
    ] {
        assert!(
            captured.contains(expected),
            "missing outbox capture `{expected}`; captured = {captured:?}"
        );
    }
    Ok(())
}

#[test]
fn occurrence_count_finalize_emits_a_resolution_change() -> Result<(), StorageError> {
    // r2 BLOCKER fix: a second occurrence of an already-known citation_key bumps the resolution's
    // `occurrence_count` (a replicated, digested column) via the finalizer — which must emit a
    // `legislation_citation_resolutions` upsert in the same transaction (the pending-upsert DO NOTHING
    // emitted nothing for the second occurrence).
    let Some(pg_config) = discover_pg_config("outbox finalize counts")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-outbox-fin.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
    seed_decision(&postgres, "cass:FIN1")?;
    seed_decision(&postgres, "cass:FIN2")?;

    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    let response_id: i64 = client
        .query_one(
            "INSERT INTO official_api_responses (provider, api_environment, endpoint, http_method, \
               subject_document_id, request_fingerprint, outcome, response_body_sha256, corpus) \
             VALUES ('judilibre','production','/decision','GET','cass:FIN1','fp','ok','sha256:x','core') \
             RETURNING response_id;",
            &[],
        )
        .map_err(StorageError::PostgresClient)?
        .get("response_id");

    let ctx = OutboxContext::new("fin-run", 19);
    // Two decisions cite the same article: 2 occurrences, 1 resolution (DO NOTHING on the second).
    for doc in ["cass:FIN1", "cass:FIN2"] {
        insert_citation_occurrence_with_client(
            &mut client,
            &InsertCitationOccurrence {
                decision_document_id: doc,
                decision_source_uid: doc,
                source_response_id: response_id,
                visa_index: 0,
                citation_key: "legi-cite:fin",
                article_number_raw: Some("1"),
                article_number_norm: "1",
                code_name_raw: Some("c"),
                code_name_norm: "c",
                canonical_query: "1 c",
                legifrance_url: None,
                raw_title: "Article 1 du c",
                extraction_method: "visa_title_regex",
            },
            Some(&ctx),
        )?;
        upsert_citation_resolution_pending_with_client(
            &mut client,
            "legi-cite:fin",
            "1",
            "c",
            "1 c",
            doc,
            Some(&ctx),
        )?;
    }

    let before = current_change_seq(&postgres)?;
    finalize_citation_occurrence_counts(&postgres, Some(&ctx))?;
    let after = current_change_seq(&postgres)?;

    // The count was recomputed to 2, and the finalize emitted exactly one resolution upsert.
    assert_eq!(
        postgres.execute_sql(
            "SELECT occurrence_count::text FROM legislation_citation_resolutions \
             WHERE corpus='core' AND citation_key='legi-cite:fin';",
        )?,
        "2"
    );
    let changed = scopes_changed_for_corpus(&postgres, "core", before, after)?;
    let resolution_changes: Vec<_> = changed
        .iter()
        .filter(|s| s.table_name == "legislation_citation_resolutions")
        .collect();
    assert_eq!(
        resolution_changes.len(),
        1,
        "finalize emits one resolution upsert for the count change"
    );
    assert_eq!(resolution_changes[0].op, EventKind::Upsert);
    assert_eq!(resolution_changes[0].scope_key, "legi-cite:fin");

    // Idempotent: a second finalize changes nothing, so emits nothing.
    let stable = current_change_seq(&postgres)?;
    finalize_citation_occurrence_counts(&postgres, Some(&ctx))?;
    assert_eq!(
        current_change_seq(&postgres)?,
        stable,
        "no-op finalize emits nothing"
    );
    Ok(())
}

#[test]
fn dense_finalizers_emit_a_parent_scope_for_stamped_fingerprints() -> Result<(), StorageError> {
    // r3 BLOCKER fix: the dense finalizers stamp the parent table's `embedding_fingerprint` (a
    // replicated column). When a finalize actually changes it (stale/null parent, correct child), it
    // must emit a document-scoped parent upsert; a no-op re-finalize emits nothing.
    let Some(pg_config) = discover_pg_config("outbox dense finalize")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-outbox-fz.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
    let ctx = OutboxContext::new("fz-run", 19);
    let vector = common::vector_literal(0);

    // --- chunk dense finalize: stale (NULL) chunks.embedding_fingerprint, correct child embedding ---
    seed_decision(&postgres, "cass:FZC")?;
    postgres.execute_sql(
        "INSERT INTO chunks (chunk_id, document_id, chunk_index, body, contextualized_body, \
           source_payload_hash, chunk_builder_version) \
         VALUES ('cass:FZC#0','cass:FZC',0,'b','ctx','sha256:c','c1');",
    )?;
    postgres.execute_sql(&format!(
        "INSERT INTO chunk_embeddings (chunk_id, embedding_fingerprint, embedding, model, dimension) \
         VALUES ('cass:FZC#0','m:1024:normalize:true','{vector}'::vector,'m',1024);"
    ))?;
    let chunk_spec = DenseRebuildSpec {
        embedding_fingerprint: "m:1024:normalize:true",
        model: "m",
        dimension: 1024,
        normalize: true,
        provisional: true,
        reembeddable: true,
        index_lists: 1,
    };
    let before = current_change_seq(&postgres)?;
    finalize_dense_rebuild(&postgres, &chunk_spec, Some(&ctx))?;
    assert_eq!(
        postgres
            .execute_sql("SELECT embedding_fingerprint FROM chunks WHERE chunk_id='cass:FZC#0';")?,
        "m:1024:normalize:true",
        "finalize stamped the parent chunk fingerprint"
    );
    let changed =
        scopes_changed_for_corpus(&postgres, "core", before, current_change_seq(&postgres)?)?;
    assert!(
        changed.iter().any(|s| s.table_name == "chunks"
            && s.op == EventKind::Upsert
            && s.scope_key == "cass:FZC"),
        "finalize emitted a chunks document upsert; changed = {changed:?}"
    );
    // Idempotent: re-finalize changes nothing -> emits nothing.
    let stable = current_change_seq(&postgres)?;
    finalize_dense_rebuild(&postgres, &chunk_spec, Some(&ctx))?;
    assert_eq!(
        current_change_seq(&postgres)?,
        stable,
        "no-op chunk finalize emits nothing"
    );

    // --- zone-unit dense finalize: stale zone_units.embedding_fingerprint, correct child embedding ---
    postgres.execute_sql(
        "INSERT INTO zone_units (zone_unit_id, document_id, zone, fragment_index, body, search_body, \
           source, text_hash, zone_unit_builder_version) \
         VALUES ('cass:FZC#motivations#0','cass:FZC','motivations',0,'m','m','cass','h','zone-units:v1');",
    )?;
    postgres.execute_sql(&format!(
        "INSERT INTO zone_unit_embeddings (zone_unit_id, embedding_fingerprint, embedding, model, dimension) \
         VALUES ('cass:FZC#motivations#0','m:1024:normalize:true','{vector}'::vector,'m',1024);"
    ))?;
    let zone_spec = DenseRebuildSpec {
        embedding_fingerprint: "m:1024:normalize:true",
        model: "m",
        dimension: 1024,
        normalize: true,
        provisional: true,
        reembeddable: true,
        index_lists: 1,
    };
    let before = current_change_seq(&postgres)?;
    finalize_zone_dense_rebuild(&postgres, &zone_spec, Some(&ctx))?;
    let changed =
        scopes_changed_for_corpus(&postgres, "core", before, current_change_seq(&postgres)?)?;
    assert!(
        changed.iter().any(|s| s.table_name == "zone_units"
            && s.op == EventKind::Upsert
            && s.scope_key == "cass:FZC"),
        "zone finalize emitted a zone_units document upsert; changed = {changed:?}"
    );
    let stable = current_change_seq(&postgres)?;
    finalize_zone_dense_rebuild(&postgres, &zone_spec, Some(&ctx))?;
    assert_eq!(
        current_change_seq(&postgres)?,
        stable,
        "no-op zone finalize emits nothing"
    );
    Ok(())
}

#[test]
fn a_build_snapshot_freezes_the_corpus_and_change_seq_against_concurrent_commits()
-> Result<(), StorageError> {
    // P3 BLOCKER regression: the baseline is cut from ONE producer snapshot so the payload and the
    // catalog `change_seq` window can never disagree under concurrent ingest. Prove the `_with_client`
    // digest + change_seq reads honour a REPEATABLE READ snapshot: a COMMITTED concurrent write is
    // invisible inside the open transaction (the old per-call connections would have seen it).
    let Some(pg_config) = discover_pg_config("outbox snapshot")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-outbox-snap.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;

    seed_decision(&postgres, "cass:S1")?;
    let mut writer = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    let ctx = OutboxContext::new("snap-run", 21);
    emit_change(
        &mut writer,
        &ctx,
        &OutboxEvent::scope(
            "core",
            "documents",
            EventKind::Upsert,
            scope_kind::DOCUMENT,
            "S1",
        ),
    )?;

    // Open the build snapshot and read the corpus + change_seq through it.
    let mut snap = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    let mut tx = snap
        .build_transaction()
        .isolation_level(postgres::IsolationLevel::RepeatableRead)
        .read_only(true)
        .start()
        .map_err(StorageError::PostgresClient)?;
    let docs_before =
        corpus_table_digests_with_client(&mut tx, "core", DigestSource::ProducerPublic)?
            .into_iter()
            .find(|d| d.table_name == "documents")
            .expect("documents digest")
            .row_count;
    let seq_before = current_change_seq_with_client(&mut tx)?;

    // A COMMITTED concurrent write: a new document + a new outbox row (bumps change_seq).
    seed_decision(&postgres, "cass:S2")?;
    emit_change(
        &mut writer,
        &ctx,
        &OutboxEvent::scope(
            "core",
            "documents",
            EventKind::Upsert,
            scope_kind::DOCUMENT,
            "S2",
        ),
    )?;

    // Inside the SAME snapshot, both reads are unchanged.
    let docs_after =
        corpus_table_digests_with_client(&mut tx, "core", DigestSource::ProducerPublic)?
            .into_iter()
            .find(|d| d.table_name == "documents")
            .expect("documents digest")
            .row_count;
    let seq_after = current_change_seq_with_client(&mut tx)?;
    assert_eq!(
        docs_before, docs_after,
        "documents count is frozen in the build snapshot"
    );
    assert_eq!(
        seq_before, seq_after,
        "change_seq high-water is frozen in the build snapshot"
    );
    tx.commit().map_err(StorageError::PostgresClient)?;

    // Outside the snapshot, a fresh read DOES see the concurrent commit — proving the freeze above was
    // the transaction snapshot, not a missing write.
    let docs_now = corpus_table_digests(&postgres, "core", DigestSource::ProducerPublic)?
        .into_iter()
        .find(|d| d.table_name == "documents")
        .expect("documents digest")
        .row_count;
    assert_eq!(
        docs_now,
        docs_before + 1,
        "a fresh read sees the concurrent commit"
    );
    assert_eq!(
        current_change_seq(&postgres)?,
        ChangeSeq::new(seq_before.get() + 1)
    );
    Ok(())
}

#[test]
fn an_emitter_blocked_by_the_fence_commits_above_the_frozen_high_water() -> Result<(), StorageError>
{
    // P4 WARN-1 regression: the high-water FENCE makes a builder's frozen `hi` a true commit-order mark.
    // While a builder holds the EXCLUSIVE fence and reads `hi`, a concurrent emitter (SHARED lock) is
    // blocked, so it cannot commit a change_seq <= hi; once the fence releases, it commits ABOVE hi and
    // is picked up by the FOLLOWING package. (This is what makes the early fence release safe.)
    let Some(pg_config) = discover_pg_config("outbox fence")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-outbox-fence.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
    seed_decision(&postgres, "cass:F0")?;

    // The builder takes the exclusive fence, then freezes `hi`.
    let mut fence = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    acquire_outbox_fence(&mut fence)?;
    let hi = current_change_seq(&postgres)?;

    // A concurrent emitter (in its own txn) blocks on the shared fence until release.
    let conn = postgres.connection_string();
    let handle = std::thread::spawn(move || {
        let mut emitter = postgres::Client::connect(&conn, postgres::NoTls).unwrap();
        let mut tx = emitter.transaction().unwrap();
        let ctx = OutboxContext::new("blocked-emitter", 24);
        let seq = emit_change(
            &mut tx,
            &ctx,
            &OutboxEvent::scope(
                "core",
                "documents",
                EventKind::Upsert,
                scope_kind::DOCUMENT,
                "blocked",
            ),
        )
        .unwrap();
        tx.commit().unwrap();
        seq
    });

    // Release the fence; the blocked emitter now proceeds and commits ABOVE the frozen high-water mark.
    release_outbox_fence(&mut fence)?;
    let blocked_seq = handle.join().expect("emitter thread");
    assert!(
        blocked_seq.get() > hi.get(),
        "a fence-blocked emitter ({}) commits above the frozen hi ({})",
        blocked_seq.get(),
        hi.get()
    );
    Ok(())
}
