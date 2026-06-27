//! P4 acceptance: build a baseline → apply on a client → mutate the producer → build incrementals →
//! apply them onto the ACTIVE generation, proving: a closing `valid_to` REPLICATES (INV-1, not just
//! inserts), a `replace_set` dropping a chunk leaves NO stale BM25-visible row (§5.3), out-of-order is
//! rejected with `sequence_gap`, re-apply is a no-op (INV-3), and the client converges to the producer.

use std::collections::BTreeMap;

use jurisearch_package::artifact;
use jurisearch_package::compat::Version;
use jurisearch_package::crypto::{AcceptAllVerifier, StubSigner};
use jurisearch_package::event::EventKind;
use jurisearch_package::manifest::EmbeddedManifest;
use jurisearch_package::signed::Signed;
use jurisearch_package_build::{
    BaselineParams, IncrementalParams, build_baseline, build_incremental,
};
use jurisearch_storage::outbox::{
    DigestSource, OutboxContext, OutboxEvent, corpus_table_digests, emit_change, scope_kind,
};
use jurisearch_storage::runtime::{ManagedPostgres, PgConfig, StorageError};
use jurisearch_syncd::{IncrementalApplyOutcome, apply_baseline, apply_incremental, corpus_status};

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
        embedding_fingerprint: "fp".to_owned(),
        embedding_model: "m".to_owned(),
        embedding_dimension: 1024,
        embedding_normalize: true,
        builder_versions: bv,
        minimum_client_version: Version::new(0, 1, 0),
    }
}

fn incremental_params(run: &str) -> IncrementalParams {
    let mut bv = BTreeMap::new();
    bv.insert("chunker".to_owned(), "c1".to_owned());
    IncrementalParams {
        builder_run_id: run.to_owned(),
        created_at: "2026-06-27T01:00:00Z".to_owned(),
        embedding_fingerprint: "fp".to_owned(),
        embedding_model: "m".to_owned(),
        embedding_dimension: 1024,
        embedding_normalize: true,
        builder_versions: bv,
        minimum_client_version: Version::new(0, 1, 0),
    }
}

/// Seed a `core` corpus: one decision with TWO BM25-indexed chunks + embeddings, `valid_to` open.
fn seed_producer(producer: &ManagedPostgres) -> Result<(), StorageError> {
    producer.execute_sql(
        "INSERT INTO documents (document_id, source, kind, source_uid, citation, title, body, \
           valid_from, valid_to, source_payload_hash, canonical_json) \
         VALUES ('cass:D1','cass','decision','cass:D1','Cass','Arret','corps','2024-01-01',NULL, \
           'sha256:d1','{}'); \
         INSERT INTO chunks (chunk_id, document_id, chunk_index, body, contextualized_body, \
           source_payload_hash, chunk_builder_version, embedding_fingerprint) \
         VALUES \
           ('cass:D1#0','cass:D1',0,'premier moyen','ctx premier moyen','sha256:c0','c1','fp'), \
           ('cass:D1#1','cass:D1',1,'second moyen distinctif','ctx second moyen distinctif STALEMARK', \
            'sha256:c1','c1','fp');",
    )?;
    producer.execute_sql(&format!(
        "INSERT INTO chunk_embeddings (chunk_id, embedding_fingerprint, embedding, model, dimension) \
         VALUES ('cass:D1#0','fp','{}'::vector,'m',1024), \
                ('cass:D1#1','fp','{}'::vector,'m',1024);",
        vector("0.01"),
        vector("0.02"),
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
    let ctx = OutboxContext::new("mutation-run", 22);
    emit_change(
        &mut tx,
        &ctx,
        &OutboxEvent::scope("core", table, op, scope_kind::DOCUMENT, scope_key),
    )?;
    tx.commit().map_err(StorageError::PostgresClient)?;
    Ok(())
}

#[test]
fn incremental_replicates_valid_to_and_drops_stale_chunks_in_order() -> Result<(), StorageError> {
    let Ok(pg_config) = PgConfig::discover() else {
        return Ok(());
    };
    let proot = tempfile::Builder::new()
        .prefix("js-inc-p.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let producer = ManagedPostgres::start_durable(pg_config.clone(), proot.path())?;
    producer.run_migrations()?;
    seed_producer(&producer)?;

    // Baseline → apply on the client.
    let base_art = tempfile::Builder::new()
        .prefix("js-inc-base.")
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
    let croot = tempfile::Builder::new()
        .prefix("js-inc-c.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let client = ManagedPostgres::start_durable(pg_config, croot.path())?;
    client.run_migrations()?;
    apply_baseline(&client, base_art.path(), &AcceptAllVerifier).expect("apply baseline");
    assert_eq!(corpus_status(&client)?[0].sequence, 1);

    // --- Mutation 1: close valid_to AND drop chunk #1 (membership change) ---
    mutate(
        &producer,
        "UPDATE documents SET valid_to='2024-12-31' WHERE document_id='cass:D1';",
        "documents",
        EventKind::Upsert,
        "cass:D1",
    )?;
    mutate(
        &producer,
        "DELETE FROM chunk_embeddings WHERE chunk_id='cass:D1#1'; \
         DELETE FROM chunks WHERE chunk_id='cass:D1#1';",
        "chunks",
        EventKind::ReplaceSet,
        "cass:D1",
    )?;
    let inc1_art = tempfile::Builder::new()
        .prefix("js-inc1.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let inc1 = build_incremental(
        &producer,
        "core",
        inc1_art.path(),
        &StubSigner,
        &incremental_params("r1"),
    )
    .expect("build inc1")
    .expect("inc1 has changes");
    assert_eq!(inc1.from_sequence, 1);
    assert_eq!(inc1.to_sequence, 2);

    // --- Mutation 2: a pure body correction on the remaining chunk (still ChunksWithEmbeddings) ---
    mutate(
        &producer,
        "UPDATE chunks SET body='premier moyen corrige', contextualized_body='ctx corrige' \
         WHERE chunk_id='cass:D1#0';",
        "chunks",
        EventKind::ReplaceSet,
        "cass:D1",
    )?;
    let inc2_art = tempfile::Builder::new()
        .prefix("js-inc2.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let inc2 = build_incremental(
        &producer,
        "core",
        inc2_art.path(),
        &StubSigner,
        &incremental_params("r2"),
    )
    .expect("build inc2")
    .expect("inc2 has changes");
    assert_eq!(inc2.from_sequence, 2);
    assert_eq!(inc2.to_sequence, 3);

    // --- Ordering: applying inc2 first (client at seq 1) is a sequence_gap ---
    let gap = apply_incremental(&client, inc2_art.path(), &AcceptAllVerifier);
    assert!(
        gap.is_err()
            && gap
                .as_ref()
                .unwrap_err()
                .to_string()
                .to_lowercase()
                .contains("sequence"),
        "out-of-order apply is a sequence_gap: {gap:?}"
    );

    // --- Apply in order ---
    let out1 = apply_incremental(&client, inc1_art.path(), &AcceptAllVerifier).expect("apply inc1");
    assert!(
        matches!(out1, IncrementalApplyOutcome::Applied { sequence: 2, .. }),
        "{out1:?}"
    );

    // INV-1: the closing valid_to REPLICATED to the active generation (not just inserts).
    let valid_to = client.execute_read_sql(
        "SELECT coalesce(valid_to::text,'OPEN') FROM documents WHERE document_id='cass:D1';",
    )?;
    assert_eq!(valid_to.trim(), "2024-12-31", "valid_to replicated");

    // §5.3: the dropped chunk left NO stale BM25-visible row.
    let stale = client
        .execute_read_sql("SELECT count(*)::text FROM chunks WHERE document_id='cass:D1';")?;
    assert_eq!(
        stale.trim(),
        "1",
        "the dropped chunk is gone from the generation"
    );
    let bm25 = client.execute_read_sql(
        "SELECT coalesce(string_agg(chunk_id, ','), 'NONE') FROM chunks \
         WHERE contextualized_body @@@ 'STALEMARK';",
    )?;
    assert_eq!(
        bm25.trim(),
        "NONE",
        "no stale chunk is BM25-visible after the replace_set"
    );

    let out2 = apply_incremental(&client, inc2_art.path(), &AcceptAllVerifier).expect("apply inc2");
    assert!(
        matches!(out2, IncrementalApplyOutcome::Applied { sequence: 3, .. }),
        "{out2:?}"
    );

    // INV-3: re-applying the committed package is an idempotent no-op.
    let again = apply_incremental(&client, inc2_art.path(), &AcceptAllVerifier).expect("re-apply");
    assert!(
        matches!(
            again,
            IncrementalApplyOutcome::AlreadyApplied { sequence: 3, .. }
        ),
        "re-apply is a no-op: {again:?}"
    );

    // Convergence: after the last incremental the client generation equals the producer's public.
    let producer_digests = corpus_table_digests(&producer, "core", DigestSource::ProducerPublic)?;
    let client_digests = corpus_table_digests(
        &client,
        "core",
        DigestSource::Generation {
            schema: "jurisearch_server_core_g0001",
        },
    )?;
    assert_eq!(
        producer_digests, client_digests,
        "client converged to the producer state"
    );
    assert_eq!(corpus_status(&client)?[0].sequence, 3);
    Ok(())
}

#[test]
fn incremental_build_rejects_an_embedding_fingerprint_boundary() -> Result<(), StorageError> {
    // r-codex P4 BLOCKER: an ordinary incremental whose embedding_fingerprint differs from the corpus's
    // cataloged stamp has crossed a boundary that needs a re-baseline — the BUILD must refuse it.
    let Ok(pg_config) = PgConfig::discover() else {
        return Ok(());
    };
    let proot = tempfile::Builder::new()
        .prefix("js-fp-p.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let producer = ManagedPostgres::start_durable(pg_config, proot.path())?;
    producer.run_migrations()?;
    seed_producer(&producer)?;
    let base_art = tempfile::Builder::new()
        .prefix("js-fp-base.")
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

    let mut bad = incremental_params("rfp");
    bad.embedding_fingerprint = "DIFFERENT".to_owned();
    let inc_art = tempfile::Builder::new()
        .prefix("js-fp-inc.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let result = build_incremental(&producer, "core", inc_art.path(), &StubSigner, &bad);
    let err = result.expect_err("a fingerprint boundary must fail to build");
    assert!(
        err.to_string().contains("embedding_fingerprint")
            && err.to_string().contains("re-baseline"),
        "build rejects the fingerprint boundary: {err}"
    );
    Ok(())
}

#[test]
fn incremental_apply_rejects_a_tampered_fingerprint_precondition() -> Result<(), StorageError> {
    // r-codex P4 BLOCKER: the consumer must compare the signed content-compatibility preconditions to
    // the corpus cursor BEFORE touching any row — a precondition that crossed a fingerprint boundary is
    // rejected even if the row digests would line up.
    let Ok(pg_config) = PgConfig::discover() else {
        return Ok(());
    };
    let proot = tempfile::Builder::new()
        .prefix("js-tp-p.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let producer = ManagedPostgres::start_durable(pg_config.clone(), proot.path())?;
    producer.run_migrations()?;
    seed_producer(&producer)?;
    let base_art = tempfile::Builder::new()
        .prefix("js-tp-base.")
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
    let croot = tempfile::Builder::new()
        .prefix("js-tp-c.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let client = ManagedPostgres::start_durable(pg_config, croot.path())?;
    client.run_migrations()?;
    apply_baseline(&client, base_art.path(), &AcceptAllVerifier).expect("apply baseline");

    mutate(
        &producer,
        "UPDATE documents SET title='Arret v2' WHERE document_id='cass:D1';",
        "documents",
        EventKind::Upsert,
        "cass:D1",
    )?;
    let inc_art = tempfile::Builder::new()
        .prefix("js-tp-inc.")
        .tempdir()
        .map_err(StorageError::Io)?;
    build_incremental(
        &producer,
        "core",
        inc_art.path(),
        &StubSigner,
        &incremental_params("rt"),
    )
    .expect("build inc")
    .expect("has changes");

    // Tamper: change the signed precondition fingerprint, re-seal.
    let manifest_path = artifact::manifest_path(inc_art.path());
    let signed: Signed<EmbeddedManifest> =
        serde_json::from_slice(&std::fs::read(&manifest_path).map_err(StorageError::Io)?).unwrap();
    let mut manifest = signed.payload;
    manifest.apply.preconditions.embedding_fingerprint = "WRONG".to_owned();
    let resealed = Signed::seal(manifest, &StubSigner).unwrap();
    std::fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&resealed).unwrap(),
    )
    .map_err(StorageError::Io)?;

    let result = apply_incremental(&client, inc_art.path(), &AcceptAllVerifier);
    let err = result.expect_err("a tampered fingerprint precondition must reject");
    assert!(
        err.to_string().to_lowercase().contains("fingerprint"),
        "apply rejects the fingerprint precondition mismatch: {err}"
    );
    // Nothing applied: the cursor stays at the baseline sequence.
    assert_eq!(
        corpus_status(&client)?[0].sequence,
        1,
        "a rejected incremental never advances the cursor"
    );
    Ok(())
}
