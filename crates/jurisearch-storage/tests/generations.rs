mod common;

use common::{discover_pg_config, vector_literal};
use jurisearch_storage::{
    citation::{CitationLookup, CitationLookupQuery, citation_lookup_json},
    france_juris::{FranceJurisGoldLimits, france_juris_gold_json, france_juris_index_revision},
    generations::{
        ActivationStamps, REPLICATED_TABLES, activate_generation, active_generation_schema,
        build_generation_indexes, create_generation_from_public, create_generation_load_tables,
        create_generation_schema, drop_retired_generation, generation_name, generation_schema,
        populate_generation_from_public, reset_building_generation,
    },
    ingest_accounting::load_query_readiness,
    retrieval::{
        ContextDocumentsQuery, FetchDocumentsQuery, context_documents_json, fetch_documents_json,
    },
    runtime::{ManagedPostgres, StorageError},
};

/// Seed a tiny searchable `core` corpus in `public`: one decision + one BM25-indexed chunk + its
/// dense embedding, so the generation clone can be exercised for both BM25 and vector reads.
fn seed_public_core(postgres: &ManagedPostgres) -> Result<(), StorageError> {
    postgres.execute_sql(
        "INSERT INTO documents (document_id, source, kind, source_uid, citation, title, body, \
           valid_from, source_payload_hash, canonical_json) \
         VALUES ('cass:GEN1','cass','decision','cass:GEN1','Cass','Arret', \
           'la responsabilite du transporteur','2024-01-01','sha256:g1','{}'); \
         INSERT INTO chunks (chunk_id, document_id, chunk_index, body, contextualized_body, \
           source_payload_hash, chunk_builder_version, embedding_fingerprint) \
         VALUES ('cass:GEN1#0','cass:GEN1',0,'la responsabilite du transporteur', \
           'ctx responsabilite','sha256:c','c1','fp');",
    )?;
    let vector = vector_literal(3);
    postgres.execute_sql(&format!(
        "INSERT INTO chunk_embeddings (chunk_id, embedding_fingerprint, embedding, model, dimension) \
         VALUES ('cass:GEN1#0','fp','{vector}'::vector,'m',1024);"
    ))?;
    Ok(())
}

fn stamps() -> ActivationStamps<'static> {
    ActivationStamps {
        sequence: 1,
        baseline_id: "core-2026-06-26-g0001",
        schema_version: 24,
        embedding_fingerprint: "fp",
        builder_versions: &serde_json::Value::Null,
        last_package_id: None,
        last_package_digest: None,
    }
}

#[test]
fn generation_clone_serves_reads_and_views_are_transparent() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("generations topology")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-generations.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
    seed_public_core(&postgres)?;

    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;

    // Build + load a generation from the producer's public working set, then activate it.
    let generation =
        create_generation_schema(&mut client, "core", 1, Some("core-2026-06-26-g0001"))?;
    assert_eq!(generation, "core_g0001");
    populate_generation_from_public(&mut client, "core", &generation)?;
    activate_generation(&postgres, "core", &generation, &stamps(), None)?;

    // corpus_state is the activation authority; the resolver maps corpus -> active physical schema.
    assert_eq!(
        active_generation_schema(&mut client, "core")?.as_deref(),
        Some("jurisearch_server_core_g0001")
    );

    // Read-transparency: the stable view returns the same rows as the public base table.
    let via_view = postgres.execute_sql(
        "SELECT count(*)::text FROM jurisearch_server.documents WHERE document_id='cass:GEN1';",
    )?;
    let via_base = postgres.execute_sql(
        "SELECT count(*)::text FROM public.documents WHERE document_id='cass:GEN1';",
    )?;
    assert_eq!(via_view.trim(), "1");
    assert_eq!(via_view.trim(), via_base.trim());

    // Hot indexed read: a BM25 search over the GENERATION's physical chunks (search_path on the
    // generation schema) must return the seeded chunk — proving LIKE INCLUDING ALL cloned the BM25
    // index into the generation.
    let gen_schema = generation_schema("core", 1);
    let bm25_hit = postgres.execute_sql_with_search_path(
        &[&gen_schema, "public"],
        "SELECT chunk_id FROM chunks WHERE contextualized_body @@@ 'responsabilite' LIMIT 1;",
    )?;
    assert_eq!(
        bm25_hit.trim(),
        "cass:GEN1#0",
        "BM25 index works on the generation clone"
    );

    // A vector read over the generation's chunk_embeddings (proves the vector column cloned).
    let vec_query = vector_literal(3);
    let vec_hit = postgres.execute_sql_with_search_path(
        &[&gen_schema, "public"],
        &format!(
            "SELECT chunk_id FROM chunk_embeddings ORDER BY embedding <-> '{vec_query}'::vector LIMIT 1;"
        ),
    )?;
    assert_eq!(vec_hit.trim(), "cass:GEN1#0");
    Ok(())
}

#[test]
fn two_generations_coexist_and_the_view_switch_is_atomic() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("generations switch")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-gen-switch.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
    seed_public_core(&postgres)?;
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;

    // Generation 1: the seeded title.
    let g1 = create_generation_schema(&mut client, "core", 1, None)?;
    populate_generation_from_public(&mut client, "core", &g1)?;
    activate_generation(&postgres, "core", &g1, &stamps(), None)?;

    // Generation 2: same corpus, but a corrected title written directly into g2 (a re-baseline).
    let g2 = create_generation_schema(&mut client, "core", 2, None)?;
    populate_generation_from_public(&mut client, "core", &g2)?;
    postgres.execute_sql(&format!(
        "UPDATE {}.documents SET title = 'Arret corrige' WHERE document_id='cass:GEN1';",
        generation_schema("core", 2)
    ))?;

    // Before the switch the view still reads g1.
    assert_eq!(
        postgres
            .execute_sql(
                "SELECT title FROM jurisearch_server.documents WHERE document_id='cass:GEN1';"
            )?
            .trim(),
        "Arret"
    );
    // Both generations coexist physically.
    assert_eq!(
        postgres
            .execute_sql(&format!(
                "SELECT title FROM {}.documents WHERE document_id='cass:GEN1';",
                generation_schema("core", 2)
            ))?
            .trim(),
        "Arret corrige"
    );

    // The switch repoints the view atomically; the cursor advances; g1 is retired (not dropped).
    let mut stamps2 = stamps();
    stamps2.sequence = 2;
    activate_generation(&postgres, "core", &g2, &stamps2, Some(1))?;
    assert_eq!(
        postgres
            .execute_sql(
                "SELECT title FROM jurisearch_server.documents WHERE document_id='cass:GEN1';"
            )?
            .trim(),
        "Arret corrige",
        "view repoint is visible to readers"
    );
    assert_eq!(
        postgres
            .execute_sql(
                "SELECT generation || ':' || state FROM jurisearch_control.generation_registry \
                 ORDER BY generation;"
            )?
            .replace('\n', ","),
        "core_g0001:retired,core_g0002:active"
    );
    assert_eq!(
        postgres
            .execute_sql(
                "SELECT sequence::text FROM jurisearch_control.corpus_state WHERE corpus='core';"
            )?
            .trim(),
        "2"
    );
    // The one-active-per-corpus partial unique index holds.
    assert_eq!(
        postgres
            .execute_sql(
                "SELECT count(*)::text FROM jurisearch_control.generation_registry \
                 WHERE corpus='core' AND state='active';"
            )?
            .trim(),
        "1"
    );
    Ok(())
}

#[test]
fn control_and_app_survive_a_generation_drop() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("generations survival")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-gen-survive.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
    seed_public_core(&postgres)?;
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;

    // An app row in jurisearch_app (preserved across every generation drop).
    postgres.execute_sql(
        "CREATE TABLE IF NOT EXISTS jurisearch_app.note (id text PRIMARY KEY, pinned_document text); \
         INSERT INTO jurisearch_app.note (id, pinned_document) VALUES ('n1','cass:GEN1');",
    )?;

    let g1 = create_generation_schema(&mut client, "core", 1, None)?;
    populate_generation_from_public(&mut client, "core", &g1)?;
    activate_generation(&postgres, "core", &g1, &stamps(), None)?;
    let g2 = create_generation_schema(&mut client, "core", 2, None)?;
    populate_generation_from_public(&mut client, "core", &g2)?;
    let mut stamps2 = stamps();
    stamps2.sequence = 2;
    activate_generation(&postgres, "core", &g2, &stamps2, Some(1))?;

    // Drop the retired g1: jurisearch_control.corpus_state and jurisearch_app.note are untouched.
    drop_retired_generation(&postgres, "core", &generation_name("core", 1))?;
    assert_eq!(
        postgres
            .execute_sql("SELECT to_regclass('jurisearch_server_core_g0001.documents')::text;")?
            .trim(),
        "",
        "the retired generation schema is gone"
    );
    assert_eq!(
        postgres
            .execute_sql(
                "SELECT active_generation FROM jurisearch_control.corpus_state WHERE corpus='core';"
            )?
            .trim(),
        "core_g0002",
        "corpus_state survives the drop"
    );
    assert_eq!(
        postgres
            .execute_sql("SELECT pinned_document FROM jurisearch_app.note WHERE id='n1';")?
            .trim(),
        "cass:GEN1",
        "jurisearch_app survives the drop"
    );
    Ok(())
}

#[test]
fn operated_cleanup_refuses_a_live_generation() -> Result<(), StorageError> {
    // Plan P5 (codex review): the operated cleanup `DROP SCHEMA ... CASCADE` is allowed ONLY for a
    // registry-confirmed RETIRED private generation — never an active/building/missing one — and the
    // retriable `reset_building_generation` is similarly confined to a half-built `building` row.
    let Some(pg_config) = discover_pg_config("generations cleanup safety")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-gen-cleanup.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
    seed_public_core(&postgres)?;
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;

    let g1 = create_generation_schema(&mut client, "core", 1, None)?;
    populate_generation_from_public(&mut client, "core", &g1)?;
    activate_generation(&postgres, "core", &g1, &stamps(), None)?;

    // drop_retired_generation refuses an ACTIVE generation, leaving its schema intact.
    assert!(
        drop_retired_generation(&postgres, "core", &g1).is_err(),
        "dropping an ACTIVE generation must be refused"
    );
    assert_eq!(
        postgres
            .execute_sql("SELECT to_regclass('jurisearch_server_core_g0001.documents')::text;")?
            .trim(),
        "jurisearch_server_core_g0001.documents",
        "the active generation schema is untouched after a refused drop"
    );
    // ...and refuses a generation with no registry row at all.
    assert!(
        drop_retired_generation(&postgres, "core", "core_g0099").is_err(),
        "dropping a non-existent generation must be refused"
    );

    // reset_building_generation refuses an ACTIVE generation too.
    assert!(
        reset_building_generation(&mut client, "core", &g1).is_err(),
        "resetting an ACTIVE generation must be refused"
    );

    // But it DOES reset a leftover `building` generation (schema + registry row gone), so a retried
    // media apply can re-create the same deterministic label.
    let g2 = create_generation_load_tables(&mut client, "core", 2, None)?;
    reset_building_generation(&mut client, "core", &g2)?;
    assert_eq!(
        postgres
            .execute_sql("SELECT to_regclass('jurisearch_server_core_g0002.documents')::text;")?
            .trim(),
        "",
        "the building generation schema is gone after reset"
    );
    assert_eq!(
        postgres
            .execute_sql(
                "SELECT count(*)::text FROM jurisearch_control.generation_registry \
                 WHERE corpus='core' AND generation='core_g0002';"
            )?
            .trim(),
        "0",
        "the building registry row is gone after reset"
    );
    // A re-create at the same label now succeeds — the apply is retriable.
    create_generation_load_tables(&mut client, "core", 2, None)?;
    Ok(())
}

#[test]
fn real_retrieval_reads_resolve_to_the_active_generation_not_stale_public()
-> Result<(), StorageError> {
    // r1 BLOCKER fix: the production CLI read path (fetch/context/…) must read the ACTIVE generation,
    // not `public`. Prove it by making `public` differ from the generation after activation and
    // asserting the real retrieval functions return the generation's data.
    let Some(pg_config) = discover_pg_config("generations read role")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-gen-read.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
    seed_public_core(&postgres)?;
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;

    let g1 = create_generation_schema(&mut client, "core", 1, None)?;
    populate_generation_from_public(&mut client, "core", &g1)?;
    activate_generation(&postgres, "core", &g1, &stamps(), None)?;

    // Now make public DIFFER from the activated generation. A read that hit public would see this.
    postgres.execute_sql(
        "UPDATE public.documents SET title = 'STALE PUBLIC' WHERE document_id='cass:GEN1';",
    )?;

    // The real fetch path reads the generation's title ('Arret'), not the stale public title.
    let fetched = fetch_documents_json(
        &postgres,
        &FetchDocumentsQuery {
            document_ids: &["cass:GEN1"],
        },
    )?;
    assert!(
        fetched.contains("Arret") && !fetched.contains("STALE PUBLIC"),
        "fetch read the active generation, not stale public: {fetched}"
    );

    // The real context path likewise reads the generation.
    let context = context_documents_json(
        &postgres,
        &ContextDocumentsQuery {
            document_id: "cass:GEN1",
            as_of: None,
            include_siblings: false,
        },
    )?;
    assert!(
        !context.contains("STALE PUBLIC"),
        "context read the active generation, not stale public: {context}"
    );
    Ok(())
}

#[test]
fn fresh_database_has_every_empty_stable_view() -> Result<(), StorageError> {
    // r1 WARN fix: a freshly-migrated client (no active generation) has a complete jurisearch_server
    // namespace — every replicated relation is a view returning zero rows (never "does not exist").
    let Some(pg_config) = discover_pg_config("generations fresh views")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-gen-fresh.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
    for table in REPLICATED_TABLES {
        let count = postgres.execute_sql(&format!(
            "SELECT count(*)::text FROM jurisearch_server.{table};"
        ))?;
        assert_eq!(
            count.trim(),
            "0",
            "jurisearch_server.{table} exists and is empty"
        );
    }
    Ok(())
}

#[test]
fn activation_validates_building_state_and_cursor() -> Result<(), StorageError> {
    // r1 BLOCKER fix: the switch validates the target is `building` and the cursor matches, and a
    // rejected activation never advances corpus_state.
    let Some(pg_config) = discover_pg_config("generations validation")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-gen-validate.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
    seed_public_core(&postgres)?;
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;

    let g1 = create_generation_schema(&mut client, "core", 1, None)?;
    populate_generation_from_public(&mut client, "core", &g1)?;
    activate_generation(&postgres, "core", &g1, &stamps(), None)?;

    // Activating an already-active generation (not `building`) is rejected.
    let err = activate_generation(&postgres, "core", &g1, &stamps(), None).unwrap_err();
    assert!(
        matches!(err, StorageError::Generations { .. }),
        "got {err:?}"
    );

    // A switch with the wrong expected previous cursor is rejected, and the cursor is unchanged.
    let g2 = create_generation_schema(&mut client, "core", 2, None)?;
    populate_generation_from_public(&mut client, "core", &g2)?;
    let mut stamps2 = stamps();
    stamps2.sequence = 2;
    let err = activate_generation(&postgres, "core", &g2, &stamps2, Some(999)).unwrap_err();
    assert!(
        matches!(err, StorageError::Generations { .. }),
        "got {err:?}"
    );
    assert_eq!(
        postgres
            .execute_sql(
                "SELECT sequence::text FROM jurisearch_control.corpus_state WHERE corpus='core';"
            )?
            .trim(),
        "1",
        "a rejected switch never advances the cursor"
    );
    // g2 remains building (a failed switch did not activate it).
    assert_eq!(
        postgres
            .execute_sql("SELECT state FROM jurisearch_control.generation_registry WHERE generation='core_g0002';")?
            .trim(),
        "building"
    );
    Ok(())
}

#[test]
fn activating_with_none_against_an_installed_corpus_is_rejected() -> Result<(), StorageError> {
    // r2 BLOCKER fix: `expected_previous_sequence = None` means "first baseline" — it must be REJECTED
    // when the corpus already has a cursor, so a stale/miswired caller cannot bypass the §7.3 guard by
    // passing `None` and clobber a live cursor.
    let Some(pg_config) = discover_pg_config("generations none-guard")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-gen-none.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
    seed_public_core(&postgres)?;
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;

    let g1 = create_generation_schema(&mut client, "core", 1, None)?;
    populate_generation_from_public(&mut client, "core", &g1)?;
    activate_generation(&postgres, "core", &g1, &stamps(), None)?; // first baseline: None is OK

    let g2 = create_generation_schema(&mut client, "core", 2, None)?;
    populate_generation_from_public(&mut client, "core", &g2)?;
    let mut stamps2 = stamps();
    stamps2.sequence = 2;
    // `None` against an already-installed corpus must be rejected (a cursor already exists).
    let err = activate_generation(&postgres, "core", &g2, &stamps2, None).unwrap_err();
    assert!(
        matches!(err, StorageError::Generations { .. }),
        "got {err:?}"
    );

    // corpus_state and the registry remain on g1; g2 stays building.
    assert_eq!(
        postgres
            .execute_sql(
                "SELECT active_generation || ':' || sequence::text \
                 FROM jurisearch_control.corpus_state WHERE corpus='core';"
            )?
            .trim(),
        "core_g0001:1",
        "a rejected None-switch never moves the cursor off g1"
    );
    assert_eq!(
        postgres
            .execute_sql(
                "SELECT generation || ':' || state FROM jurisearch_control.generation_registry \
                 ORDER BY generation;"
            )?
            .replace('\n', ","),
        "core_g0001:active,core_g0002:building"
    );
    Ok(())
}

#[test]
fn query_readiness_is_writer_stamped_and_a_not_ready_generation_cannot_activate()
-> Result<(), StorageError> {
    // work/09 P3A: readiness is WRITER-owned. A ready generation's activation stamps readiness (the
    // read path is then a pure lookup), and a generation whose coverage is incomplete CANNOT activate
    // — the writer-owned gate aborts the switch with the cursor unchanged, so the read path never sees
    // a not-ready active generation (and never recomputes on read).
    let Some(pg_config) = discover_pg_config("generations readiness")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-gen-ready.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
    seed_public_core(&postgres)?;

    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;

    // A READY generation activates: the writer stamps readiness, and the read path looks it up.
    let g1 = create_generation_schema(&mut client, "core", 1, None)?;
    populate_generation_from_public(&mut client, "core", &g1)?;
    activate_generation(&postgres, "core", &g1, &stamps(), None)?;
    let report = load_query_readiness(&postgres)?;
    assert_eq!(report.projection_coverage.covered, 1);
    assert_eq!(report.embedding_coverage.covered, 1);
    assert_eq!(report.embedding_coverage.total, 1);

    // A NOT-ready generation (its dense embeddings dropped) cannot activate: the gate aborts the
    // switch with the cursor unchanged.
    let g2 = create_generation_schema(&mut client, "core", 2, None)?;
    populate_generation_from_public(&mut client, "core", &g2)?;
    postgres.execute_sql(&format!(
        "DELETE FROM {}.chunk_embeddings;",
        generation_schema("core", 2)
    ))?;
    let stamps2 = ActivationStamps {
        sequence: 2,
        ..stamps()
    };
    let error = activate_generation(&postgres, "core", &g2, &stamps2, Some(1)).unwrap_err();
    assert!(
        error.to_string().to_lowercase().contains("coverage"),
        "incomplete coverage aborts activation: {error}"
    );
    // The cursor never moved off the ready generation, and its stamp still resolves.
    assert_eq!(
        active_generation_schema(&mut client, "core")?.as_deref(),
        Some(generation_schema("core", 1).as_str())
    );
    assert_eq!(
        load_query_readiness(&postgres)?.embedding_coverage.covered,
        1
    );
    Ok(())
}

#[test]
fn france_eval_gold_and_revision_follow_the_active_generation_not_stale_public()
-> Result<(), StorageError> {
    // r2 WARN fix: France eval gold qrels and the index revision are extracted from the SERVED corpus,
    // so they must read the active generation, not stale `public`.
    let Some(pg_config) = discover_pg_config("france eval read role")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-france-gen.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;

    // A 'cass' decision with a decision_summary chunk long enough (120-2000 chars, identifier-free) to
    // qualify as a retrieval qrel. GENMARKER is the generation's distinctive token.
    let gen_body = "La responsabilite du transporteur maritime engage le commettant lorsque le \
                    prepose GENMARKER commet une faute caracterisee dans l execution du contrat de \
                    transport international de marchandises diverses et variees.";
    postgres.execute_sql(&format!(
        "INSERT INTO documents (document_id, source, kind, source_uid, citation, title, body, \
           valid_from, source_payload_hash, canonical_json) \
         VALUES ('cass:FR1','cass','decision','cass:FR1','Cass','Arret','{gen_body}', \
           '2024-01-01','sha256:fr1','{{}}'); \
         INSERT INTO chunks (chunk_id, document_id, chunk_index, chunk_kind, body, \
           contextualized_body, source_payload_hash, chunk_builder_version, embedding_fingerprint) \
         VALUES ('cass:FR1#0','cass:FR1',0,'decision_summary','{gen_body}','ctx','sha256:c','c1','fp');"
    ))?;
    let vector = vector_literal(3);
    postgres.execute_sql(&format!(
        "INSERT INTO chunk_embeddings (chunk_id, embedding_fingerprint, embedding, model, dimension) \
         VALUES ('cass:FR1#0','fp','{vector}'::vector,'m',1024);"
    ))?;

    // Revision over the producer (public) BEFORE any generation: the 1-document baseline digest.
    let revision_baseline = france_juris_index_revision(&postgres)?;

    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    let g1 = create_generation_schema(&mut client, "core", 1, None)?;
    populate_generation_from_public(&mut client, "core", &g1)?;
    activate_generation(&postgres, "core", &g1, &stamps(), None)?;

    // Make public STALE (corrupt the qrel text) and LARGER (add a second document) than the generation.
    postgres.execute_sql(
        "UPDATE public.chunks SET body = replace(body,'GENMARKER','STALEPUBLIC') \
           WHERE chunk_id='cass:FR1#0'; \
         INSERT INTO documents (document_id, source, kind, source_uid, citation, title, body, \
           valid_from, source_payload_hash, canonical_json) \
         VALUES ('cass:FR2','cass','decision','cass:FR2','Cass','Arret2','x','2024-01-02', \
           'sha256:fr2','{}');",
    )?;

    // Gold qrels come from the generation (GENMARKER), never the stale public text (STALEPUBLIC).
    let gold = france_juris_gold_json(
        &postgres,
        FranceJurisGoldLimits {
            judicial_retrieval: 5,
            administrative_retrieval: 0,
            ecli: 0,
            pourvoi: 0,
            cetatext: 0,
        },
    )?;
    assert!(
        gold.contains("GENMARKER"),
        "gold read the generation: {gold}"
    );
    assert!(
        !gold.contains("STALEPUBLIC"),
        "gold must not read stale public: {gold}"
    );

    // The revision tracks the generation's 1-document count, not public's grown 2-document count.
    let revision_after = france_juris_index_revision(&postgres)?;
    assert_eq!(
        revision_after, revision_baseline,
        "revision follows the active generation (1 doc), not stale public (2 docs)"
    );
    Ok(())
}

#[test]
fn citation_lookup_resolves_decision_identifiers_against_the_active_generation()
-> Result<(), StorageError> {
    // r3 BLOCKER fix: identifier resolution (`cite`, and the France ecli/pourvoi/cetatext scoring that
    // calls `citation_lookup_json`) must read the ACTIVE generation, not stale `public` — otherwise a
    // client split-brains: fetch/search read the generation while `cite` matches against empty public.
    let Some(pg_config) = discover_pg_config("citation lookup generation")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-cite-gen.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;

    // A cass decision carrying an ECLI + pourvoi in its canonical record, with a chunk + embedding so
    // the generation is query-ready (the work/09 P3A apply-time coverage gate requires it).
    postgres.execute_sql(
        "INSERT INTO documents (document_id, source, kind, source_uid, citation, title, body, \
           valid_from, source_payload_hash, canonical_json) \
         VALUES ('cass:DEC1','cass','decision','JURITEXT000000000001','Cass','Arret','corps', \
           '2024-01-01','sha256:dec1', \
           '{\"ecli\":\"ECLI:FR:CCASS:2024:C100001\",\"case_numbers\":[\"22-21.812\"]}'); \
         INSERT INTO chunks (chunk_id, document_id, chunk_index, body, contextualized_body, \
           source_payload_hash, chunk_builder_version, embedding_fingerprint) \
         VALUES ('cass:DEC1#0','cass:DEC1',0,'corps','ctx corps','sha256:cd1','c1','fp');",
    )?;
    let vector = vector_literal(3);
    postgres.execute_sql(&format!(
        "INSERT INTO chunk_embeddings (chunk_id, embedding_fingerprint, embedding, model, dimension) \
         VALUES ('cass:DEC1#0','fp','{vector}'::vector,'m',1024);"
    ))?;
    let generation = create_generation_from_public(&postgres, "core", 1, None)?;
    activate_generation(&postgres, "core", &generation, &stamps(), None)?;
    // Empty public: only the generation holds the decision now.
    postgres.execute_sql("DELETE FROM public.documents;")?;

    // ECLI lookup resolves to the generation's decision (would be empty if it read stale public).
    let by_ecli = citation_lookup_json(
        &postgres,
        &CitationLookupQuery {
            lookup: CitationLookup::DecisionEcli("ECLI:FR:CCASS:2024:C100001"),
            limit: 25,
        },
    )?;
    assert!(
        by_ecli.contains("cass:DEC1"),
        "ECLI lookup read the active generation: {by_ecli}"
    );

    // Pourvoi lookup (index-backed via the GIN index cloned into the generation) likewise resolves.
    let by_pourvoi = citation_lookup_json(
        &postgres,
        &CitationLookupQuery {
            lookup: CitationLookup::DecisionPourvoi("22-21.812"),
            limit: 25,
        },
    )?;
    assert!(
        by_pourvoi.contains("cass:DEC1"),
        "pourvoi lookup read the active generation: {by_pourvoi}"
    );
    Ok(())
}

#[test]
fn a_loaded_generation_has_the_full_index_inventory_before_activation() -> Result<(), StorageError>
{
    // r-codex P3 D3 footgun guard: `create_generation_load_tables` clones EXCLUDING INDEXES (which
    // also drops PK/UNIQUE, since they are index-backed); `build_generation_indexes` must recreate the
    // FULL inventory — PK + every replayed index + the two IVFFlat ANN indexes at corpus-sized lists —
    // so the loaded generation is structurally equal to `public` and its indexes are FUNCTIONAL before
    // it is ever activated.
    let Some(pg_config) = discover_pg_config("generation index inventory")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-gen-inventory.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
    seed_public_core(&postgres)?;
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;

    // Load path: clone column shape only, bulk-load rows, then build the index/constraint inventory.
    let g1 = create_generation_load_tables(&mut client, "core", 1, None)?;
    populate_generation_from_public(&mut client, "core", &g1)?;
    let report = build_generation_indexes(&mut client, &g1, "core")?;
    assert!(report.constraints_built >= 1, "PK/UNIQUE recreated");
    assert_eq!(
        report.ivfflat_built.len(),
        2,
        "both IVFFlat ANN indexes built"
    );

    let schema = generation_schema("core", 1);

    // The PK on documents is back (it would be absent if EXCLUDING INDEXES had not been compensated).
    let pk = postgres.execute_sql(&format!(
        "SELECT count(*)::text FROM pg_constraint \
         WHERE conrelid = '{schema}.documents'::regclass AND contype = 'p';"
    ))?;
    assert_eq!(
        pk.trim(),
        "1",
        "documents primary key recreated in the generation"
    );

    // Exactly two IVFFlat indexes and at least one BM25 index live in the generation schema.
    let by_am = |am: &str| -> Result<String, StorageError> {
        postgres.execute_sql(&format!(
            "SELECT count(*)::text FROM pg_index x \
             JOIN pg_class i ON i.oid = x.indexrelid \
             JOIN pg_am a ON a.oid = i.relam \
             JOIN pg_namespace n ON n.oid = i.relnamespace \
             WHERE n.nspname = '{schema}' AND a.amname = '{am}';"
        ))
    };
    assert_eq!(
        by_am("ivfflat")?.trim(),
        "2",
        "two IVFFlat ANN indexes in the generation"
    );
    assert!(
        by_am("bm25")?.trim().parse::<i32>().unwrap_or(0) >= 1,
        "BM25 index in the generation"
    );

    // r-codex P3 WARN-2: the FOREIGN KEY inventory is recreated too (the load-mode footgun). The
    // generation's FK count over replicated tables equals `public`'s, and points INTO the generation.
    let replicated_array = REPLICATED_TABLES
        .iter()
        .map(|t| format!("'{t}'"))
        .collect::<Vec<_>>()
        .join(",");
    let gen_fks = postgres.execute_sql(&format!(
        "SELECT count(*)::text FROM pg_constraint c \
         JOIN pg_namespace n ON n.oid = c.connamespace \
         WHERE n.nspname = '{schema}' AND c.contype = 'f';"
    ))?;
    let public_fks = postgres.execute_sql(&format!(
        "SELECT count(*)::text FROM pg_constraint c \
         JOIN pg_class t ON t.oid = c.conrelid \
         JOIN pg_namespace n ON n.oid = t.relnamespace \
         WHERE n.nspname = 'public' AND c.contype = 'f' AND t.relname IN ({replicated_array});"
    ))?;
    assert_eq!(
        gen_fks.trim(),
        public_fks.trim(),
        "the generation's FK inventory matches public over replicated tables"
    );
    assert_eq!(
        report.foreign_keys_built.to_string(),
        gen_fks.trim(),
        "every recreated FK is accounted for in the report"
    );
    assert!(
        report.foreign_keys_built >= 2,
        "chunks->documents and chunk_embeddings->chunks at least"
    );
    // A FK in the generation references INTO the generation, not back to public.
    let fk_target_schema = postgres.execute_sql(&format!(
        "SELECT DISTINCT nf.nspname FROM pg_constraint c \
         JOIN pg_namespace n ON n.oid = c.connamespace \
         JOIN pg_class cf ON cf.oid = c.confrelid \
         JOIN pg_namespace nf ON nf.oid = cf.relnamespace \
         WHERE n.nspname = '{schema}' AND c.contype = 'f' AND cf.relname = 'documents';"
    ))?;
    assert_eq!(
        fk_target_schema.trim(),
        schema,
        "a replicated FK target resolves to the generation, not public"
    );

    // The indexes are FUNCTIONAL: a BM25 search and a vector search over the generation return the seed.
    let gen_schema = generation_schema("core", 1);
    let bm25_hit = postgres.execute_sql_with_search_path(
        &[&gen_schema, "public"],
        "SELECT chunk_id FROM chunks WHERE contextualized_body @@@ 'responsabilite' LIMIT 1;",
    )?;
    assert_eq!(
        bm25_hit.trim(),
        "cass:GEN1#0",
        "BM25 index functional in the generation"
    );
    let vec_query = vector_literal(3);
    let vec_hit = postgres.execute_sql_with_search_path(
        &[&gen_schema, "public"],
        &format!(
            "SELECT chunk_id FROM chunk_embeddings ORDER BY embedding <-> '{vec_query}'::vector LIMIT 1;"
        ),
    )?;
    assert_eq!(
        vec_hit.trim(),
        "cass:GEN1#0",
        "IVFFlat index functional in the generation"
    );
    Ok(())
}
