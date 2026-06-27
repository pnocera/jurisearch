//! P9 acceptance: the signed filesystem-published distribution loop.
//!
//! - the producer builds + PUBLISHES a baseline + incrementals + a signed remote manifest under a
//!   deterministic root; a subscribed client polls the manifest, plans, and `run_catchup`s through a
//!   `DirectoryCatchupSource`, converging to the producer head (digest match);
//! - `producer_cycle` builds + publishes the next incremental and refreshes the manifest, and the
//!   client catches up again;
//! - the remote-manifest builder is the publish-integrity gate: a missing published artifact fails the
//!   build before any client sees the manifest;
//! - the retention window's `min_available_sequence` is computed from the retained incremental chain.

use std::collections::BTreeMap;

use jurisearch_package::compat::Version;
use jurisearch_package::crypto::{Ed25519Signer, Ed25519Verifier, KeyEpoch, KeyId};
use jurisearch_package::event::EventKind;
use jurisearch_package::manifest::remote::{CatchupPolicy, EntitlementTier};
use jurisearch_package::manifest::{EmbeddedManifest, RemoteManifest};
use jurisearch_package::signed::Signed;
use jurisearch_package_build::remote_manifest::build_remote_manifest;
use jurisearch_package_build::{
    BaselineParams, EnrichmentMode, IncrementalParams, ProducerCycleConfig, RemoteManifestParams,
    build_baseline, build_incremental, producer_cycle, publish_package, publish_remote_manifest,
    published_manifest_path, verify_published_root,
};
use jurisearch_storage::outbox::{
    DigestSource, OutboxContext, OutboxEvent, corpus_table_digests, emit_change, scope_kind,
};
use jurisearch_storage::runtime::{ManagedPostgres, PgConfig, StorageError};
use jurisearch_storage::trust::PACKAGE_PURPOSE;
use jurisearch_syncd::{
    CatchupPlan, CatchupReport, DirectoryCatchupSource, apply_baseline, corpus_status,
    install_trust_anchor, load_package_verifier, plan_catchup, read_client_cursor, run_catchup,
};

const URI_BASE: &str = "media://";

fn signer() -> Ed25519Signer {
    Ed25519Signer::from_seed(&[3u8; 32], KeyId("producer-k".to_owned()), KeyEpoch(1))
}

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
        builder_run_id: "b0".to_owned(),
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

fn remote_manifest_params(signer: &Ed25519Signer, max_retained: usize) -> RemoteManifestParams {
    RemoteManifestParams {
        publisher: "jurisearch".to_owned(),
        environment: "test".to_owned(),
        generated_at: "2026-06-27T03:00:00Z".to_owned(),
        catchup_policy: CatchupPolicy {
            max_incremental_packages: 100,
            // Generous ratios so the TINY test corpus (a JSONL diff that dwarfs the compact baseline)
            // routes to the incremental chain — the ratio routing itself is covered by the P7 planner
            // unit tests; here we exercise the published distribution loop.
            max_cumulative_diff_to_baseline_permille: 100_000,
            max_cumulative_uncompressed_to_baseline_permille: 100_000,
            max_apply_seconds_budget: 2700,
        },
        entitlement_tier: EntitlementTier::Open,
        license_epoch: 0,
        audience: None,
        signing_key_id: signer.key_id().clone(),
        uri_base: URI_BASE.to_owned(),
        max_retained_incrementals: max_retained,
        default_apply_seconds: 5,
        default_load_seconds: 600,
    }
}

fn seed_producer(producer: &ManagedPostgres) -> Result<(), StorageError> {
    producer.execute_sql(
        "INSERT INTO documents (document_id, source, kind, source_uid, citation, title, body, \
           valid_from, source_payload_hash, canonical_json) \
         VALUES ('cass:P1','cass','decision','cass:P1','Cass','Arret','corps','2024-01-01', \
           'sha256:p1','{}'); \
         INSERT INTO chunks (chunk_id, document_id, chunk_index, body, contextualized_body, \
           source_payload_hash, chunk_builder_version, embedding_fingerprint) \
         VALUES ('cass:P1#0','cass:P1',0,'corps','ctx corps','sha256:c','c1','fp');",
    )?;
    producer.execute_sql(&format!(
        "INSERT INTO chunk_embeddings (chunk_id, embedding_fingerprint, embedding, model, dimension) \
         VALUES ('cass:P1#0','fp','{}'::vector,'m',1024);",
        vector("0.01"),
    ))?;
    Ok(())
}

fn mutate(producer: &ManagedPostgres, sql: &str, scope_key: &str) -> Result<(), StorageError> {
    let mut client = producer.client()?;
    let mut tx = client.transaction().map_err(StorageError::PostgresClient)?;
    tx.batch_execute(sql)
        .map_err(StorageError::PostgresClient)?;
    let ctx = OutboxContext::new("mutation-run", 24);
    emit_change(
        &mut tx,
        &ctx,
        &OutboxEvent::scope(
            "core",
            "documents",
            EventKind::Upsert,
            scope_kind::DOCUMENT,
            scope_key,
        ),
    )?;
    tx.commit().map_err(StorageError::PostgresClient)?;
    Ok(())
}

fn read_signed_manifest(root: &std::path::Path) -> Signed<RemoteManifest> {
    let bytes = std::fs::read(published_manifest_path(root, "core")).expect("manifest bytes");
    serde_json::from_slice(&bytes).expect("manifest json")
}

#[test]
fn publish_then_update_converges_and_producer_cycle_extends_the_chain() -> Result<(), StorageError>
{
    let Ok(pg_config) = PgConfig::discover() else {
        return Ok(());
    };
    let sgnr = signer();
    let proot = tempfile::Builder::new()
        .prefix("js-pub-p.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let producer = ManagedPostgres::start_durable(pg_config.clone(), proot.path())?;
    producer.run_migrations()?;
    seed_producer(&producer)?;

    let published = tempfile::Builder::new()
        .prefix("js-pub-root.")
        .tempdir()
        .map_err(StorageError::Io)?;

    // Build + publish the baseline.
    let base_art = tempfile::Builder::new()
        .prefix("js-pub-base.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let base = build_baseline(
        &producer,
        "core",
        base_art.path(),
        &sgnr,
        &baseline_params(),
    )
    .expect("baseline");
    publish_package(published.path(), "core", &base.package_id, base_art.path())
        .expect("publish baseline");

    // Build + publish two incrementals.
    for (run, doc) in [("r1", "rev1"), ("r2", "rev2")] {
        mutate(
            &producer,
            &format!("UPDATE documents SET title='{doc}' WHERE document_id='cass:P1';"),
            "cass:P1",
        )?;
        let art = tempfile::Builder::new()
            .prefix("js-pub-inc.")
            .tempdir()
            .map_err(StorageError::Io)?;
        let inc = build_incremental(
            &producer,
            "core",
            art.path(),
            &sgnr,
            &incremental_params(run),
        )
        .expect("inc")
        .expect("inc changes");
        publish_package(published.path(), "core", &inc.package_id, art.path())
            .expect("publish inc");
        // The artifact tempdir can drop now — the published copy is independent.
        drop(art);
    }

    // Build + publish the signed remote manifest.
    let manifest = build_remote_manifest(
        &producer,
        "core",
        published.path(),
        &sgnr,
        &remote_manifest_params(&sgnr, 100),
    )
    .expect("remote manifest");
    publish_remote_manifest(published.path(), "core", &manifest).expect("publish manifest");
    assert_eq!(manifest.payload.head_sequence.get(), 3);
    assert_eq!(manifest.payload.packages.len(), 2);
    assert_eq!(manifest.payload.active_baseline.sequence.get(), 1);

    // Client: install trust anchor, apply the published baseline.
    let croot = tempfile::Builder::new()
        .prefix("js-pub-c.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let client = ManagedPostgres::start_durable(pg_config, croot.path())?;
    client.run_migrations()?;
    install_trust_anchor(&client, &sgnr.trust_anchor(), PACKAGE_PURPOSE).expect("install anchor");
    let verifier = load_package_verifier(&client).expect("verifier");
    apply_baseline(&client, base_art.path(), &verifier).expect("apply baseline");
    assert_eq!(corpus_status(&client)?[0].sequence, 1);

    // Poll the published manifest, verify it, plan, and run catch-up via the filesystem source.
    let source = DirectoryCatchupSource::new(published.path(), URI_BASE);
    let signed = read_signed_manifest(published.path());
    signed.verify(&verifier).expect("manifest signature");
    let cursor = read_client_cursor(&client, "core")
        .expect("cursor")
        .expect("installed");
    let plan = plan_catchup(&signed.payload, Some(&cursor));
    assert!(matches!(plan, CatchupPlan::Incremental(ref c) if c.len() == 2));
    let report = run_catchup(&client, &source, &verifier, plan).expect("catch-up");
    assert_eq!(report, CatchupReport::IncrementalApplied { applied: 2 });
    assert_eq!(corpus_status(&client)?[0].sequence, 3);

    // Convergence to the producer head.
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
        "client converged to head after publish/update"
    );

    // --- producer_cycle builds + publishes the next incremental and refreshes the manifest. ---
    mutate(
        &producer,
        "UPDATE documents SET title='rev3' WHERE document_id='cass:P1';",
        "cass:P1",
    )?;
    let build_dir = tempfile::Builder::new()
        .prefix("js-pub-cycle.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let config = ProducerCycleConfig {
        incremental_params: incremental_params("r3"),
        remote_manifest_params: remote_manifest_params(&sgnr, 100),
        enrichment: EnrichmentMode::Ran { zones_enriched: 0 },
    };
    let cycle = producer_cycle(
        &producer,
        "core",
        published.path(),
        build_dir.path(),
        &sgnr,
        &config,
    )
    .expect("producer cycle");
    assert!(
        cycle.built_incremental.is_some(),
        "the cycle built an incremental"
    );
    assert_eq!(cycle.enrichment, EnrichmentMode::Ran { zones_enriched: 0 });

    // The client catches up again from the refreshed manifest.
    let signed2 = read_signed_manifest(published.path());
    signed2
        .verify(&verifier)
        .expect("refreshed manifest signature");
    assert_eq!(signed2.payload.head_sequence.get(), 4);
    let cursor2 = read_client_cursor(&client, "core")
        .expect("cursor")
        .expect("installed");
    let plan2 = plan_catchup(&signed2.payload, Some(&cursor2));
    assert!(matches!(plan2, CatchupPlan::Incremental(ref c) if c.len() == 1));
    run_catchup(&client, &source, &verifier, plan2).expect("second catch-up");
    assert_eq!(corpus_status(&client)?[0].sequence, 4);
    Ok(())
}

#[test]
fn the_remote_manifest_build_fails_when_a_published_artifact_is_missing() -> Result<(), StorageError>
{
    let Ok(pg_config) = PgConfig::discover() else {
        return Ok(());
    };
    let sgnr = signer();
    let proot = tempfile::Builder::new()
        .prefix("js-pubm-p.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let producer = ManagedPostgres::start_durable(pg_config, proot.path())?;
    producer.run_migrations()?;
    seed_producer(&producer)?;
    let published = tempfile::Builder::new()
        .prefix("js-pubm-root.")
        .tempdir()
        .map_err(StorageError::Io)?;

    // Publish the baseline but BUILD (not publish) an incremental → the catalog has it, the root lacks it.
    let base_art = tempfile::Builder::new()
        .prefix("js-pubm-base.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let base = build_baseline(
        &producer,
        "core",
        base_art.path(),
        &sgnr,
        &baseline_params(),
    )
    .expect("baseline");
    publish_package(published.path(), "core", &base.package_id, base_art.path())
        .expect("publish baseline");
    mutate(
        &producer,
        "UPDATE documents SET title='rev1' WHERE document_id='cass:P1';",
        "cass:P1",
    )?;
    let inc_art = tempfile::Builder::new()
        .prefix("js-pubm-inc.")
        .tempdir()
        .map_err(StorageError::Io)?;
    build_incremental(
        &producer,
        "core",
        inc_art.path(),
        &sgnr,
        &incremental_params("r1"),
    )
    .expect("inc")
    .expect("inc changes");
    // (deliberately NOT publishing the incremental)

    let result = build_remote_manifest(
        &producer,
        "core",
        published.path(),
        &sgnr,
        &remote_manifest_params(&sgnr, 100),
    );
    let err = result.expect_err("a missing published artifact must fail the manifest build");
    assert!(
        err.to_string().contains("missing"),
        "the manifest build cites the missing artifact: {err}"
    );
    Ok(())
}

#[test]
fn verify_published_root_checks_the_actual_manifest_and_fails_on_tamper() -> Result<(), StorageError>
{
    let Ok(pg_config) = PgConfig::discover() else {
        return Ok(());
    };
    let sgnr = signer();
    let proot = tempfile::Builder::new()
        .prefix("js-vfy-p.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let producer = ManagedPostgres::start_durable(pg_config, proot.path())?;
    producer.run_migrations()?;
    seed_producer(&producer)?;
    let published = tempfile::Builder::new()
        .prefix("js-vfy-root.")
        .tempdir()
        .map_err(StorageError::Io)?;

    let base_art = tempfile::Builder::new()
        .prefix("js-vfy-base.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let base = build_baseline(
        &producer,
        "core",
        base_art.path(),
        &sgnr,
        &baseline_params(),
    )
    .expect("baseline");
    publish_package(published.path(), "core", &base.package_id, base_art.path())
        .expect("publish baseline");
    mutate(
        &producer,
        "UPDATE documents SET title='rev1' WHERE document_id='cass:P1';",
        "cass:P1",
    )?;
    let inc_art = tempfile::Builder::new()
        .prefix("js-vfy-inc.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let inc = build_incremental(
        &producer,
        "core",
        inc_art.path(),
        &sgnr,
        &incremental_params("r1"),
    )
    .expect("inc")
    .expect("changes");
    publish_package(published.path(), "core", &inc.package_id, inc_art.path())
        .expect("publish inc");
    let manifest = build_remote_manifest(
        &producer,
        "core",
        published.path(),
        &sgnr,
        &remote_manifest_params(&sgnr, 100),
    )
    .expect("manifest");
    publish_remote_manifest(published.path(), "core", &manifest).expect("publish manifest");

    // The PUBLIC verifier verifies the actual published root (no signing seed).
    let verifier = Ed25519Verifier::from_anchors(&[sgnr.trust_anchor()]).expect("verifier");
    verify_published_root(published.path(), "core", URI_BASE, &verifier)
        .expect("published root verifies");

    // Corrupt the published manifest clients poll → verify must fail (a catalog-only check would miss it).
    std::fs::write(
        published_manifest_path(published.path(), "core"),
        b"{not a manifest}",
    )
    .map_err(StorageError::Io)?;
    assert!(
        verify_published_root(published.path(), "core", URI_BASE, &verifier).is_err(),
        "a corrupted published manifest must fail verify"
    );
    Ok(())
}

#[test]
fn build_remote_manifest_rejects_a_tampered_embedded_identity() -> Result<(), StorageError> {
    let Ok(pg_config) = PgConfig::discover() else {
        return Ok(());
    };
    let sgnr = signer();
    let proot = tempfile::Builder::new()
        .prefix("js-emb-p.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let producer = ManagedPostgres::start_durable(pg_config, proot.path())?;
    producer.run_migrations()?;
    seed_producer(&producer)?;
    let published = tempfile::Builder::new()
        .prefix("js-emb-root.")
        .tempdir()
        .map_err(StorageError::Io)?;

    let base_art = tempfile::Builder::new()
        .prefix("js-emb-base.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let base = build_baseline(
        &producer,
        "core",
        base_art.path(),
        &sgnr,
        &baseline_params(),
    )
    .expect("baseline");
    publish_package(published.path(), "core", &base.package_id, base_art.path())
        .expect("publish baseline");
    mutate(
        &producer,
        "UPDATE documents SET title='rev1' WHERE document_id='cass:P1';",
        "cass:P1",
    )?;
    let inc_art = tempfile::Builder::new()
        .prefix("js-emb-inc.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let inc = build_incremental(
        &producer,
        "core",
        inc_art.path(),
        &sgnr,
        &incremental_params("r1"),
    )
    .expect("inc")
    .expect("changes");
    publish_package(published.path(), "core", &inc.package_id, inc_art.path())
        .expect("publish inc");

    // Edit ONLY the published incremental's embedded-manifest identity (payload files untouched). The
    // canonical embedded digest then differs from the cataloged `manifest_digest` → the builder rejects.
    let inc_manifest_path = published
        .path()
        .join("core")
        .join("packages")
        .join(&inc.package_id)
        .join("manifest.json");
    let mut signed: Signed<EmbeddedManifest> =
        serde_json::from_slice(&std::fs::read(&inc_manifest_path).map_err(StorageError::Io)?)
            .expect("embedded manifest");
    signed.payload.identity.builder_run_id = "tampered".to_owned();
    std::fs::write(
        &inc_manifest_path,
        serde_json::to_vec_pretty(&signed).expect("serialize"),
    )
    .map_err(StorageError::Io)?;

    let err = build_remote_manifest(
        &producer,
        "core",
        published.path(),
        &sgnr,
        &remote_manifest_params(&sgnr, 100),
    )
    .expect_err("a tampered embedded identity must fail the manifest build");
    assert!(
        err.to_string().contains("embedded-manifest digest")
            || err.to_string().contains("!= cataloged"),
        "rejection cites the embedded-manifest identity: {err}"
    );
    Ok(())
}

#[test]
fn retention_window_min_available_is_the_earliest_retained_incremental() -> Result<(), StorageError>
{
    let Ok(pg_config) = PgConfig::discover() else {
        return Ok(());
    };
    let sgnr = signer();
    let proot = tempfile::Builder::new()
        .prefix("js-ret-p.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let producer = ManagedPostgres::start_durable(pg_config, proot.path())?;
    producer.run_migrations()?;
    seed_producer(&producer)?;
    let published = tempfile::Builder::new()
        .prefix("js-ret-root.")
        .tempdir()
        .map_err(StorageError::Io)?;

    let base_art = tempfile::Builder::new()
        .prefix("js-ret-base.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let base = build_baseline(
        &producer,
        "core",
        base_art.path(),
        &sgnr,
        &baseline_params(),
    )
    .expect("baseline");
    publish_package(published.path(), "core", &base.package_id, base_art.path())
        .expect("publish baseline");
    for (run, doc) in [("r1", "rev1"), ("r2", "rev2"), ("r3", "rev3")] {
        mutate(
            &producer,
            &format!("UPDATE documents SET title='{doc}' WHERE document_id='cass:P1';"),
            "cass:P1",
        )?;
        let art = tempfile::Builder::new()
            .prefix("js-ret-inc.")
            .tempdir()
            .map_err(StorageError::Io)?;
        let inc = build_incremental(
            &producer,
            "core",
            art.path(),
            &sgnr,
            &incremental_params(run),
        )
        .expect("inc")
        .expect("changes");
        publish_package(published.path(), "core", &inc.package_id, art.path())
            .expect("publish inc");
        drop(art);
    }

    // Retain only the newest 2 incrementals (3→4 and 4→... actually 2→3, 3→4): min_available = 2.
    let manifest = build_remote_manifest(
        &producer,
        "core",
        published.path(),
        &sgnr,
        &remote_manifest_params(&sgnr, 2),
    )
    .expect("remote manifest");
    assert_eq!(
        manifest.payload.head_sequence.get(),
        4,
        "head is the newest incremental"
    );
    assert_eq!(
        manifest.payload.packages.len(),
        2,
        "only the newest 2 incrementals are retained"
    );
    assert_eq!(
        manifest.payload.min_available_sequence.get(),
        2,
        "min_available is the earliest retained incremental's from_sequence"
    );
    Ok(())
}
