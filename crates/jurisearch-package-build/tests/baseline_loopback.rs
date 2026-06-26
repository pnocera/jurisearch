//! P3 acceptance (milestone M3): build a baseline on producer DB A, apply it on a SEPARATE client DB
//! B, and prove the served generation reproduces the producer state — postcondition digests match, a
//! real read resolves to the generation, the cursor is at the baseline sequence, and a re-apply is an
//! idempotent no-op.

use std::collections::BTreeMap;

use jurisearch_package::artifact;
use jurisearch_package::compat::Version;
use jurisearch_package::crypto::{AcceptAllVerifier, StubSigner};
use jurisearch_package::manifest::EmbeddedManifest;
use jurisearch_package::signed::Signed;
use jurisearch_package_build::{BaselineParams, build_baseline};
use jurisearch_storage::dense::recommended_probes;
use jurisearch_storage::outbox::{DigestSource, corpus_table_digests};
use jurisearch_storage::retrieval::{FetchDocumentsQuery, fetch_documents_json};
use jurisearch_storage::runtime::{ManagedPostgres, PgConfig, StorageError};
use jurisearch_syncd::{BaselineApplyOutcome, apply_baseline, corpus_status};

fn params() -> BaselineParams {
    let mut builder_versions = BTreeMap::new();
    builder_versions.insert("chunker".to_owned(), "c1".to_owned());
    BaselineParams {
        baseline_id: "core-2026-06-27-g0001".to_owned(),
        builder_run_id: "build-run-1".to_owned(),
        created_at: "2026-06-27T00:00:00Z".to_owned(),
        embedding_fingerprint: "fp".to_owned(),
        embedding_model: "m".to_owned(),
        embedding_dimension: 1024,
        embedding_normalize: true,
        builder_versions,
        minimum_client_version: Version::new(0, 1, 0),
    }
}

/// Seed a small searchable `core` corpus into the producer's `public` (a decision + a BM25-indexed
/// chunk + its dense embedding).
fn seed_producer(producer: &ManagedPostgres) -> Result<(), StorageError> {
    producer.execute_sql(
        "INSERT INTO documents (document_id, source, kind, source_uid, citation, title, body, \
           valid_from, source_payload_hash, canonical_json) \
         VALUES ('cass:BL1','cass','decision','cass:BL1','Cass','Arret', \
           'la responsabilite du transporteur maritime','2024-01-01','sha256:bl1','{}'); \
         INSERT INTO chunks (chunk_id, document_id, chunk_index, body, contextualized_body, \
           source_payload_hash, chunk_builder_version, embedding_fingerprint) \
         VALUES ('cass:BL1#0','cass:BL1',0,'la responsabilite du transporteur maritime', \
           'ctx responsabilite transporteur','sha256:c','c1','fp');",
    )?;
    let vector = format!(
        "[{}]",
        (0..1024).map(|_| "0.01").collect::<Vec<_>>().join(",")
    );
    producer.execute_sql(&format!(
        "INSERT INTO chunk_embeddings (chunk_id, embedding_fingerprint, embedding, model, dimension) \
         VALUES ('cass:BL1#0','fp','{vector}'::vector,'m',1024);"
    ))?;
    Ok(())
}

#[test]
fn baseline_builds_on_producer_and_applies_on_a_separate_client() -> Result<(), StorageError> {
    let Ok(pg_config) = PgConfig::discover() else {
        return Ok(()); // managed PG not available in this environment
    };

    // Producer DB A.
    let producer_root = tempfile::Builder::new()
        .prefix("jurisearch-baseline-producer.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let producer = ManagedPostgres::start_durable(pg_config.clone(), producer_root.path())?;
    producer.run_migrations()?;
    seed_producer(&producer)?;

    // Build the baseline artifact.
    let artifact_root = tempfile::Builder::new()
        .prefix("jurisearch-baseline-artifact.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let report = build_baseline(
        &producer,
        "core",
        artifact_root.path(),
        &StubSigner,
        &params(),
    )
    .expect("baseline build");
    assert_eq!(report.package_id, "core-1-1");
    assert_eq!(report.generation, "core_g0001");
    assert_eq!(
        report.total_rows, 3,
        "1 document + 1 chunk + 1 embedding = 3 replicated rows"
    );

    // Producer catalog row exists.
    let catalog = producer.execute_sql(
        "SELECT package_id || ':' || status FROM package_catalog WHERE corpus='core';",
    )?;
    assert_eq!(catalog.trim(), "core-1-1:built");

    // Client DB B (fresh, no data).
    let client_root = tempfile::Builder::new()
        .prefix("jurisearch-baseline-client.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let client = ManagedPostgres::start_durable(pg_config, client_root.path())?;
    client.run_migrations()?;

    // Before apply: no corpus installed, and a read finds nothing.
    assert!(
        corpus_status(&client)?.is_empty(),
        "fresh client has no corpus"
    );

    // Apply the baseline onto B.
    let outcome =
        apply_baseline(&client, artifact_root.path(), &AcceptAllVerifier).expect("baseline apply");
    match outcome {
        BaselineApplyOutcome::Applied {
            corpus,
            generation,
            sequence,
            ..
        } => {
            assert_eq!(corpus, "core");
            assert_eq!(generation, "core_g0001");
            assert_eq!(sequence, 1);
        }
        other => panic!("expected Applied, got {other:?}"),
    }

    // ACCEPTANCE: the served generation's digests equal the producer's authoritative digests.
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
        "every postcondition digest matches the producer (loopback proof)"
    );

    // A real read on B resolves to the active generation (execute_read_sql under the hood).
    let fetched = fetch_documents_json(
        &client,
        &FetchDocumentsQuery {
            document_ids: &["cass:BL1"],
        },
    )?;
    assert!(
        fetched.contains("cass:BL1") && fetched.contains("transporteur"),
        "client fetch returns the applied document: {fetched}"
    );

    // The cursor authority reports the corpus at the baseline sequence.
    let statuses = corpus_status(&client)?;
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].corpus, "core");
    assert_eq!(statuses[0].sequence, 1);
    assert_eq!(statuses[0].active_generation, "core_g0001");
    assert_eq!(statuses[0].last_package_id.as_deref(), Some("core-1-1"));

    // r2 WARN-1: the client cursor records the PACKAGE/artifact digest (matching the producer catalog's
    // `package_digest`), NOT the manifest digest — so the P4 chain link compares like-for-like.
    let client_cursor_digest = client.execute_sql(
        "SELECT last_package_digest FROM jurisearch_control.corpus_state WHERE corpus='core';",
    )?;
    let producer_pkg_digest = producer
        .execute_sql("SELECT package_digest FROM package_catalog WHERE package_id='core-1-1';")?;
    assert_eq!(
        client_cursor_digest.trim(),
        producer_pkg_digest.trim(),
        "client cursor digest == producer package_digest (payload), not the manifest digest"
    );

    // r3 WARN-2: the client wrote the dense `index_manifest` so its query path honours the
    // package-declared `default_probes` (corpus-sized), not a hard-coded fallback.
    let manifest_lists: u32 = client
        .execute_sql(
            "SELECT value->'vector_index'->>'lists' FROM index_manifest WHERE key='embedding';",
        )?
        .trim()
        .parse()
        .expect("embedding index_manifest lists");
    let manifest_probes: u32 = client
        .execute_sql(
            "SELECT value->'vector_index'->>'default_probes' FROM index_manifest WHERE key='embedding';",
        )?
        .trim()
        .parse()
        .expect("embedding index_manifest default_probes");
    assert_eq!(
        manifest_probes,
        recommended_probes(manifest_lists),
        "the client's stored default_probes matches the package contract for its lists"
    );

    // Idempotent re-apply: the cursor is already at the result sequence with the same digest → no-op.
    let again =
        apply_baseline(&client, artifact_root.path(), &AcceptAllVerifier).expect("re-apply");
    assert!(
        matches!(
            again,
            BaselineApplyOutcome::AlreadyApplied { sequence: 1, .. }
        ),
        "re-applying the same baseline is an idempotent no-op, got {again:?}"
    );

    Ok(())
}

#[test]
fn apply_rejects_a_baseline_whose_signed_ivfflat_lists_was_tampered() -> Result<(), StorageError> {
    // r2 WARN-2: the consumer enforces the SIGNED index-build contract against the actual generation
    // index — not just the index name. Tamper the manifest's declared IVFFlat `lists` and prove apply
    // rejects (the built index's real `lists` no longer matches the contract), so a drifted index shape
    // can never activate.
    let Ok(pg_config) = PgConfig::discover() else {
        return Ok(());
    };
    let producer_root = tempfile::Builder::new()
        .prefix("jurisearch-tamper-producer.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let producer = ManagedPostgres::start_durable(pg_config.clone(), producer_root.path())?;
    producer.run_migrations()?;
    seed_producer(&producer)?;

    let artifact_root = tempfile::Builder::new()
        .prefix("jurisearch-tamper-artifact.")
        .tempdir()
        .map_err(StorageError::Io)?;
    build_baseline(
        &producer,
        "core",
        artifact_root.path(),
        &StubSigner,
        &params(),
    )
    .expect("baseline build");

    // Tamper: bump every declared IVFFlat `lists` to an impossible value, then re-seal (stub signer).
    let manifest_path = artifact::manifest_path(artifact_root.path());
    let signed: Signed<EmbeddedManifest> =
        serde_json::from_slice(&std::fs::read(&manifest_path).map_err(StorageError::Io)?)
            .expect("manifest json");
    let mut manifest = signed.payload;
    for ivf in &mut manifest.apply.index_build.ivfflat_finalize {
        ivf.lists += 9999;
    }
    let resealed = Signed::seal(manifest, &StubSigner).expect("reseal");
    std::fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&resealed).expect("serialize"),
    )
    .map_err(StorageError::Io)?;

    let client_root = tempfile::Builder::new()
        .prefix("jurisearch-tamper-client.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let client = ManagedPostgres::start_durable(pg_config, client_root.path())?;
    client.run_migrations()?;

    let result = apply_baseline(&client, artifact_root.path(), &AcceptAllVerifier);
    let error = result.expect_err("apply must reject a tampered IVFFlat lists");
    assert!(
        error.to_string().contains("lists"),
        "rejection cites the index-contract lists mismatch: {error}"
    );
    // Nothing was activated — the corpus is still uninstalled.
    assert!(
        corpus_status(&client)?.is_empty(),
        "a rejected apply never activates a generation"
    );
    Ok(())
}

/// Build a baseline, apply `tamper` to the signed manifest, re-seal, and apply it on a fresh client.
/// Returns the apply result, or `None` when managed PG is unavailable (the test then skips).
fn build_tamper_apply(
    tamper: impl FnOnce(&mut EmbeddedManifest),
) -> Option<Result<BaselineApplyOutcome, jurisearch_syncd::SyncError>> {
    let pg_config = PgConfig::discover().ok()?;
    let producer_root = tempfile::Builder::new()
        .prefix("jurisearch-tamper-p.")
        .tempdir()
        .unwrap();
    let producer = ManagedPostgres::start_durable(pg_config.clone(), producer_root.path()).unwrap();
    producer.run_migrations().unwrap();
    seed_producer(&producer).unwrap();

    let artifact_root = tempfile::Builder::new()
        .prefix("jurisearch-tamper-a.")
        .tempdir()
        .unwrap();
    build_baseline(
        &producer,
        "core",
        artifact_root.path(),
        &StubSigner,
        &params(),
    )
    .unwrap();

    let manifest_path = artifact::manifest_path(artifact_root.path());
    let signed: Signed<EmbeddedManifest> =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    let mut manifest = signed.payload;
    tamper(&mut manifest);
    let resealed = Signed::seal(manifest, &StubSigner).unwrap();
    std::fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&resealed).unwrap(),
    )
    .unwrap();

    let client_root = tempfile::Builder::new()
        .prefix("jurisearch-tamper-c.")
        .tempdir()
        .unwrap();
    let client = ManagedPostgres::start_durable(pg_config, client_root.path()).unwrap();
    client.run_migrations().unwrap();
    let result = apply_baseline(&client, artifact_root.path(), &AcceptAllVerifier);
    // A rejection happens before activation; prove nothing was installed on a rejected apply.
    if result.is_err() {
        assert!(corpus_status(&client).unwrap().is_empty());
    }
    Some(result)
}

#[test]
fn apply_rejects_a_tampered_aggregate_artifact_digest() {
    // r3 WARN-1: even with every per-file digest intact, a manifest whose aggregate
    // `integrity.artifact_sha256` was changed (it is the cursor identity) is rejected — the consumer
    // recomputes the aggregate over the verified payload and requires equality.
    let Some(result) = build_tamper_apply(|m| {
        m.integrity.artifact_sha256 = "sha256:tampered-aggregate".to_owned();
    }) else {
        return;
    };
    let error = result.expect_err("tampered artifact_sha256 must reject");
    assert!(
        error.to_string().contains("artifact_sha256"),
        "rejection cites the aggregate digest mismatch: {error}"
    );
}

#[test]
fn apply_rejects_tampered_ivfflat_probes() {
    // r3 WARN-2: the signed IVFFlat `probes` must be internally consistent with `lists`; a tampered
    // probe value is rejected before activation.
    let Some(result) = build_tamper_apply(|m| {
        for ivf in &mut m.apply.index_build.ivfflat_finalize {
            ivf.probes += 7;
        }
    }) else {
        return;
    };
    let error = result.expect_err("tampered probes must reject");
    assert!(
        error.to_string().contains("probes"),
        "rejection cites the probes mismatch: {error}"
    );
}

#[test]
fn apply_rejects_a_payload_file_set_that_disagrees_with_integrity_digests() {
    // r4 WARN-1: the aggregate must be computed over the VERIFIED files read off disk, not the signed
    // map. Drop a payload-file entry (so it is never read) while integrity still claims its digest — the
    // verified set no longer equals `integrity.per_file_digests`, so apply rejects before activation.
    let Some(result) = build_tamper_apply(|m| {
        m.payload.files.pop();
    }) else {
        return;
    };
    let error = result.expect_err("a payload/integrity mismatch must reject");
    assert!(
        error.to_string().contains("verified payload files")
            || error.to_string().contains("aggregate payload digest"),
        "rejection cites the verified-set mismatch: {error}"
    );
}
