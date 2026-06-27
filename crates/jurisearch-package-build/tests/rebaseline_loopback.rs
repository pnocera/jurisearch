//! P5 acceptance (milestone M5): build a RE-BASELINE on producer A and apply it on client B, proving
//! the §7.4 scoped reload of ONE corpus's server set:
//!
//! - a re-embed (changed embedding fingerprint + rewritten bodies) is shipped as a full reissue that
//!   the BUILD accepts (unlike an incremental, which would refuse the boundary);
//! - a BEHIND client (still at the baseline sequence, with an unapplied incremental in between) applies
//!   the re-baseline directly and jumps forward to its sequence (forward supersession, §9.4);
//! - after the swap the corpus serves the NEW generation whose digests match the producer's new public;
//! - `jurisearch_app` rows are byte-identical (INV-4/5 — the reload never mutates app data);
//! - a SECOND installed corpus (`inpi`) — its generation, `corpus_state`, and view membership — is
//!   untouched and still served (C1 isolation; per P5 scope this covers generation/cursor/view/app
//!   isolation — the global `index_manifest` dense-probe metadata stays shared, deferred per the review);
//! - a post-re-baseline INCREMENTAL still applies — proving the client adopted the producer's
//!   DETERMINISTIC generation label, so the incremental's `active_generation` precondition resolves.

use std::collections::BTreeMap;

use jurisearch_package::compat::Version;
use jurisearch_package::crypto::{AcceptAllVerifier, StubSigner};
use jurisearch_package::event::EventKind;
use jurisearch_package_build::{
    BaselineParams, IncrementalParams, build_baseline, build_incremental, build_rebaseline,
};
use jurisearch_storage::generations::{
    ActivationStamps, activate_generation, create_generation_load_tables,
};
use jurisearch_storage::outbox::{
    DigestSource, OutboxContext, OutboxEvent, corpus_table_digests, emit_change, scope_kind,
};
use jurisearch_storage::retrieval::{FetchDocumentsQuery, fetch_documents_json};
use jurisearch_storage::runtime::{ManagedPostgres, PgConfig, StorageError};
use jurisearch_syncd::{
    IncrementalApplyOutcome, apply_baseline, apply_incremental, apply_rebaseline, corpus_status,
};

fn vector(seed: &str) -> String {
    format!(
        "[{}]",
        (0..1024).map(|_| seed).collect::<Vec<_>>().join(",")
    )
}

fn baseline_params() -> BaselineParams {
    let mut bv = BTreeMap::new();
    bv.insert("chunker".to_owned(), "c1".to_owned());
    BaselineParams {
        baseline_id: "core-2026-06-27-g0001".to_owned(),
        builder_run_id: "build-0".to_owned(),
        created_at: "2026-06-27T00:00:00Z".to_owned(),
        embedding_fingerprint: "fp1".to_owned(),
        embedding_model: "m".to_owned(),
        embedding_dimension: 1024,
        embedding_normalize: true,
        builder_versions: bv,
        minimum_client_version: Version::new(0, 1, 0),
    }
}

fn incremental_params(run: &str, fingerprint: &str) -> IncrementalParams {
    let mut bv = BTreeMap::new();
    bv.insert("chunker".to_owned(), "c1".to_owned());
    IncrementalParams {
        builder_run_id: run.to_owned(),
        created_at: "2026-06-27T01:00:00Z".to_owned(),
        embedding_fingerprint: fingerprint.to_owned(),
        embedding_model: "m".to_owned(),
        embedding_dimension: 1024,
        embedding_normalize: true,
        builder_versions: bv,
        minimum_client_version: Version::new(0, 1, 0),
    }
}

/// A re-baseline reissue: the bodies were re-processed and re-embedded under a NEW fingerprint, so the
/// builder versions stay `c1` but the embedding fingerprint moves `fp1 -> fp2` (the boundary that needs
/// a re-baseline rather than an incremental).
fn rebaseline_params() -> BaselineParams {
    let mut bv = BTreeMap::new();
    bv.insert("chunker".to_owned(), "c1".to_owned());
    BaselineParams {
        baseline_id: "core-2026-06-27-g0002".to_owned(),
        builder_run_id: "rebuild-0".to_owned(),
        created_at: "2026-06-27T02:00:00Z".to_owned(),
        embedding_fingerprint: "fp2".to_owned(),
        embedding_model: "m".to_owned(),
        embedding_dimension: 1024,
        embedding_normalize: true,
        builder_versions: bv,
        minimum_client_version: Version::new(0, 1, 0),
    }
}

/// Seed a `core` corpus on the producer: one decision + a BM25-indexed chunk + its dense embedding.
fn seed_producer(producer: &ManagedPostgres) -> Result<(), StorageError> {
    producer.execute_sql(
        "INSERT INTO documents (document_id, source, kind, source_uid, citation, title, body, \
           valid_from, source_payload_hash, canonical_json) \
         VALUES ('cass:RB1','cass','decision','cass:RB1','Cass','Arret', \
           'responsabilite du transporteur maritime','2024-01-01','sha256:rb1','{}'); \
         INSERT INTO chunks (chunk_id, document_id, chunk_index, body, contextualized_body, \
           source_payload_hash, chunk_builder_version, embedding_fingerprint) \
         VALUES ('cass:RB1#0','cass:RB1',0,'responsabilite du transporteur maritime', \
           'ctx responsabilite transporteur','sha256:c','c1','fp1');",
    )?;
    producer.execute_sql(&format!(
        "INSERT INTO chunk_embeddings (chunk_id, embedding_fingerprint, embedding, model, dimension) \
         VALUES ('cass:RB1#0','fp1','{}'::vector,'m',1024);",
        vector("0.01"),
    ))?;
    Ok(())
}

/// Run a producer mutation + its outbox emit in one transaction (mirrors a real writer).
fn mutate(
    producer: &ManagedPostgres,
    sql: &str,
    table: &str,
    op: EventKind,
    scope_key: &str,
) -> Result<(), StorageError> {
    let mut client = producer.client()?;
    let mut tx = client.transaction().map_err(StorageError::PostgresClient)?;
    tx.batch_execute(sql)
        .map_err(StorageError::PostgresClient)?;
    let ctx = OutboxContext::new("mutation-run", 24);
    emit_change(
        &mut tx,
        &ctx,
        &OutboxEvent::scope("core", table, op, scope_kind::DOCUMENT, scope_key),
    )?;
    tx.commit().map_err(StorageError::PostgresClient)?;
    Ok(())
}

/// Install a SECOND corpus `inpi` directly on the client as an active (empty) generation, so the test
/// can prove a `core` re-baseline leaves another installed corpus + its generation untouched.
fn install_second_corpus(client: &ManagedPostgres) -> Result<(), StorageError> {
    let mut db = client.client()?;
    create_generation_load_tables(&mut db, "inpi", 1, Some("inpi-2026-g0001"))?;
    let bv = serde_json::json!({ "chunker": "i1" });
    let stamps = ActivationStamps {
        sequence: 1,
        baseline_id: "inpi-2026-g0001",
        schema_version: 24,
        embedding_fingerprint: "fpi",
        builder_versions: &bv,
        last_package_id: Some("inpi-1-1"),
        last_package_digest: Some("sha256:inpi"),
    };
    activate_generation(client, "inpi", "inpi_g0001", &stamps, None)?;
    Ok(())
}

#[test]
fn rebaseline_scope_replaces_core_preserving_app_and_other_corpora() -> Result<(), StorageError> {
    let Ok(pg_config) = PgConfig::discover() else {
        return Ok(()); // managed PG not available in this environment
    };

    // Producer DB A: seed + first baseline.
    let proot = tempfile::Builder::new()
        .prefix("js-rb-p.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let producer = ManagedPostgres::start_durable(pg_config.clone(), proot.path())?;
    producer.run_migrations()?;
    seed_producer(&producer)?;

    let base_art = tempfile::Builder::new()
        .prefix("js-rb-base.")
        .tempdir()
        .map_err(StorageError::Io)?;
    build_baseline(
        &producer,
        "core",
        base_art.path(),
        &StubSigner,
        &baseline_params(),
    )
    .expect("baseline");

    // Client DB B: apply the baseline, then install a SECOND corpus + write app data.
    let croot = tempfile::Builder::new()
        .prefix("js-rb-c.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let client = ManagedPostgres::start_durable(pg_config.clone(), croot.path())?;
    client.run_migrations()?;
    apply_baseline(&client, base_art.path(), &AcceptAllVerifier).expect("apply baseline");
    assert_eq!(corpus_status(&client)?[0].sequence, 1);

    install_second_corpus(&client)?;

    // App data referencing the core document — must survive the reload byte-for-byte (INV-4/5).
    client.execute_sql(
        "CREATE TABLE jurisearch_app.app_notes (id int PRIMARY KEY, ref_document_id text, note text); \
         INSERT INTO jurisearch_app.app_notes VALUES (1,'cass:RB1','my pinned evidence');",
    )?;
    let app_before = client
        .execute_sql("SELECT id||'|'||ref_document_id||'|'||note FROM jurisearch_app.app_notes;")?;

    // Snapshot inpi's cursor row before the core re-baseline (must be untouched after).
    let inpi_before = client.execute_sql(
        "SELECT active_generation||'@'||sequence FROM jurisearch_control.corpus_state WHERE corpus='inpi';",
    )?;
    assert_eq!(inpi_before.trim(), "inpi_g0001@1");

    // --- Producer: an incremental the BEHIND client never applies (so the re-baseline lands ahead). ---
    mutate(
        &producer,
        "UPDATE documents SET title='Arret (rev)' WHERE document_id='cass:RB1';",
        "documents",
        EventKind::Upsert,
        "cass:RB1",
    )?;
    let inc_skip = tempfile::Builder::new()
        .prefix("js-rb-incskip.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let inc2 = build_incremental(
        &producer,
        "core",
        inc_skip.path(),
        &StubSigner,
        &incremental_params("r-skip", "fp1"),
    )
    .expect("build skipped incremental")
    .expect("has changes");
    assert_eq!((inc2.from_sequence, inc2.to_sequence), (1, 2));

    // --- Producer: the re-embed (fp1 -> fp2 + rewritten content) shipped as a RE-BASELINE. ---
    mutate(
        &producer,
        "UPDATE documents SET body='nouvelle analyse de la responsabilite maritime' \
           WHERE document_id='cass:RB1'; \
         UPDATE chunks SET body='nouvelle analyse responsabilite maritime', \
           contextualized_body='ctx nouvelle analyse responsabilite', embedding_fingerprint='fp2' \
           WHERE chunk_id='cass:RB1#0'; \
         UPDATE chunk_embeddings SET embedding_fingerprint='fp2', embedding='REEMBED'::vector \
           WHERE chunk_id='cass:RB1#0';"
            .replace("REEMBED", &vector("0.09"))
            .as_str(),
        "chunks",
        EventKind::ReplaceSet,
        "cass:RB1",
    )?;

    let rb_art = tempfile::Builder::new()
        .prefix("js-rb-art.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let rb = build_rebaseline(
        &producer,
        "core",
        rb_art.path(),
        &StubSigner,
        &rebaseline_params(),
    )
    .expect("build re-baseline");
    assert_eq!(
        rb.generation, "core_g0002",
        "re-baseline is on a NEW generation"
    );
    assert_eq!(
        (rb.from_sequence, rb.to_sequence),
        (2, 3),
        "the re-baseline advances the per-corpus sequence past the skipped incremental"
    );

    // --- The BEHIND client (still at sequence 1) applies the re-baseline and jumps forward to 3. ---
    let outcome =
        apply_rebaseline(&client, rb_art.path(), &AcceptAllVerifier).expect("apply re-baseline");
    match outcome {
        jurisearch_syncd::BaselineApplyOutcome::Applied {
            corpus,
            generation,
            sequence,
            ..
        } => {
            assert_eq!(corpus, "core");
            assert_eq!(generation, "core_g0002");
            assert_eq!(
                sequence, 3,
                "forward supersession: 1 -> 3, skipping the incremental"
            );
        }
        other => panic!("expected Applied, got {other:?}"),
    }

    // ACCEPTANCE 1: the new generation reproduces the producer's NEW public exactly.
    let producer_digests = corpus_table_digests(&producer, "core", DigestSource::ProducerPublic)?;
    let client_digests = corpus_table_digests(
        &client,
        "core",
        DigestSource::Generation {
            schema: "jurisearch_server_core_g0002",
        },
    )?;
    assert_eq!(
        producer_digests, client_digests,
        "the re-baselined generation matches the producer's re-embedded public"
    );

    // ACCEPTANCE 2: a read on the client returns the NEW content from the active generation.
    let fetched = fetch_documents_json(
        &client,
        &FetchDocumentsQuery {
            document_ids: &["cass:RB1"],
        },
    )?;
    assert!(
        fetched.contains("nouvelle analyse"),
        "the client serves the re-embedded body: {fetched}"
    );
    let status = corpus_status(&client)?;
    let core = status
        .iter()
        .find(|s| s.corpus == "core")
        .expect("core present");
    assert_eq!(core.sequence, 3);
    assert_eq!(core.active_generation, "core_g0002");
    let core_fp = client.execute_sql(
        "SELECT embedding_fingerprint FROM jurisearch_control.corpus_state WHERE corpus='core';",
    )?;
    assert_eq!(
        core_fp.trim(),
        "fp2",
        "the cursor records the re-baselined fingerprint"
    );

    // ACCEPTANCE 3: the old generation is retired; the new one is active.
    let states = client.execute_sql(
        "SELECT string_agg(generation||':'||state, ',' ORDER BY generation) \
         FROM jurisearch_control.generation_registry WHERE corpus='core';",
    )?;
    assert_eq!(states.trim(), "core_g0001:retired,core_g0002:active");

    // ACCEPTANCE 4: app data is byte-identical (the reload never touched jurisearch_app).
    let app_after = client
        .execute_sql("SELECT id||'|'||ref_document_id||'|'||note FROM jurisearch_app.app_notes;")?;
    assert_eq!(
        app_before, app_after,
        "jurisearch_app survives the re-baseline unchanged"
    );

    // ACCEPTANCE 5: the second corpus + its generation are untouched and still served.
    let inpi_after = client.execute_sql(
        "SELECT active_generation||'@'||sequence FROM jurisearch_control.corpus_state WHERE corpus='inpi';",
    )?;
    assert_eq!(
        inpi_after.trim(),
        "inpi_g0001@1",
        "inpi's cursor is untouched"
    );
    let inpi_state = client.execute_sql(
        "SELECT state FROM jurisearch_control.generation_registry \
         WHERE corpus='inpi' AND generation='inpi_g0001';",
    )?;
    assert_eq!(
        inpi_state.trim(),
        "active",
        "inpi's generation stays active through core's swap"
    );

    // ACCEPTANCE 6: a post-re-baseline INCREMENTAL still applies — the client adopted the producer's
    // deterministic generation label, so the incremental's `active_generation` precondition resolves.
    mutate(
        &producer,
        "UPDATE documents SET title='Arret (post-rebaseline)' WHERE document_id='cass:RB1';",
        "documents",
        EventKind::Upsert,
        "cass:RB1",
    )?;
    let inc_post = tempfile::Builder::new()
        .prefix("js-rb-incpost.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let inc4 = build_incremental(
        &producer,
        "core",
        inc_post.path(),
        &StubSigner,
        &incremental_params("r-post", "fp2"),
    )
    .expect("build post-rebaseline incremental")
    .expect("has changes");
    assert_eq!((inc4.from_sequence, inc4.to_sequence), (3, 4));

    let post = apply_incremental(&client, inc_post.path(), &AcceptAllVerifier)
        .expect("apply post-rebaseline incremental");
    assert!(
        matches!(post, IncrementalApplyOutcome::Applied { sequence: 4, .. }),
        "the incremental applies onto the re-baselined generation: {post:?}"
    );
    let converged = corpus_table_digests(
        &client,
        "core",
        DigestSource::Generation {
            schema: "jurisearch_server_core_g0002",
        },
    )?;
    let producer_final = corpus_table_digests(&producer, "core", DigestSource::ProducerPublic)?;
    assert_eq!(
        producer_final, converged,
        "client converged after the post-rebaseline incremental"
    );

    // ACCEPTANCE 7 (P5 r1 WARN): a FRESH client with NO `core` generation applies the SAME re-baseline
    // directly and must adopt the PRODUCER's deterministic label `core_g0002` — NOT a client-local
    // `core_g0001` that `next_generation_counter` would allocate. This is the exact fresh-client bug the
    // design note warned about: if the applier regressed to a local counter, this client would end on
    // `core_g0001` and the post-re-baseline incremental (which preconditions on `active_generation =
    // core_g0002`) would then be REJECTED.
    let fresh_root = tempfile::Builder::new()
        .prefix("js-rb-fresh.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let fresh = ManagedPostgres::start_durable(pg_config, fresh_root.path())?;
    fresh.run_migrations()?;
    assert!(
        corpus_status(&fresh)?.is_empty(),
        "the fresh client has no corpus"
    );

    let fresh_outcome = apply_rebaseline(&fresh, rb_art.path(), &AcceptAllVerifier)
        .expect("fresh re-baseline apply");
    match fresh_outcome {
        jurisearch_syncd::BaselineApplyOutcome::Applied {
            generation,
            sequence,
            ..
        } => {
            assert_eq!(
                generation, "core_g0002",
                "a fresh client adopts the producer's deterministic generation label, not a local g0001"
            );
            assert_eq!(
                sequence, 3,
                "the fresh client lands directly on the re-baseline sequence"
            );
        }
        other => panic!("expected Applied, got {other:?}"),
    }
    let fresh_active = fresh.execute_sql(
        "SELECT active_generation FROM jurisearch_control.corpus_state WHERE corpus='core';",
    )?;
    assert_eq!(fresh_active.trim(), "core_g0002");

    // The post-re-baseline incremental applies on the fresh client too — its `active_generation`
    // precondition (`core_g0002`) resolves only because the fresh client adopted the producer label.
    let fresh_post = apply_incremental(&fresh, inc_post.path(), &AcceptAllVerifier)
        .expect("fresh client applies the post-rebaseline incremental");
    assert!(
        matches!(
            fresh_post,
            IncrementalApplyOutcome::Applied { sequence: 4, .. }
        ),
        "the incremental's active_generation precondition resolves on the fresh client: {fresh_post:?}"
    );

    Ok(())
}
