//! INFRA-GATED acceptance (skips cleanly without `JURISEARCH_PG_CONFIG`): the `publish-baseline` command
//! publishes the EXISTING in-DB `core` corpus as the producer's FIRST signed baseline over the producer's
//! OWN external `WriterHandle` (a `ConnectionConfig`-backed `DbClientSource`) — NEVER a `ManagedPostgres`.
//!
//! The harness `ManagedPostgres` is ONLY the operator's external server (seeding); the command's reads +
//! writes go exclusively through `config.writer_handle()`. Proves: a seeded + embedded corpus yields a
//! `published` `core-1-1` row, a served artifact + signed manifest the producer signer verifies, exit
//! class `published`; and a second run is refused (the first baseline already exists).

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use jurisearch_package::crypto::Ed25519Verifier;
use jurisearch_package_build::verify_published_root;
use jurisearch_producer::config::ProducerConfig;
use jurisearch_producer::error::ProducerError;
use jurisearch_producer::run_publish_baseline;
use jurisearch_storage::runtime::{ManagedPostgres, PgConfig, StorageError};

const FP: &str = "bge-m3:1024:normalize:true";

fn vector() -> String {
    format!("[{}]", vec!["0.01"; 1024].join(","))
}

fn producer_config(port: u16, work: &Path) -> ProducerConfig {
    let secrets = work.join("secrets");
    std::fs::create_dir_all(&secrets).unwrap();
    let seed = secrets.join("producer-signing.seed");
    std::fs::write(&seed, "09".repeat(32)).unwrap();
    std::fs::set_permissions(&seed, std::fs::Permissions::from_mode(0o600)).unwrap();

    let toml = format!(
        r#"
[producer]
corpora_dir = "{root}/packages"
archives_dir = "{root}/archives"
state_dir = "{root}/state"
publisher = "jurisearch"
environment = "test"

[database]
host = "127.0.0.1"
port = {port}
name = "jurisearch"
admin_user = "postgres"
admin_database = "jurisearch"
writer_user = "postgres"
read_user = "postgres"
owner_role = "postgres"

[fetch]
base_url = "https://echanges.dila.gouv.fr/OPENDATA"
user_agent = "jurisearch-producer-test/0.1"
retain_deltas = "all"

[[fetch_group]]
name = "jurisprudence"
sources = ["cass"]
cadence = "daily"

[package]
corpus = "core"
signing_key_id = "producer-k1"
signing_key_seed_file = "{seed}"
uri_base = "media://"

[enrichment]
mode = "disabled"

[embedding]
provider = "openai_compatible"
base_url = "https://openrouter.ai/api/v1"
model_name = "bge-m3"
request_model = "baai/bge-m3"
dimension = 1024
normalize = true
pooling = "cls"

[baseline_refresh]
mode = "auto-on-new-baseline"
"#,
        root = work.display(),
        port = port,
        seed = seed.display(),
    );
    let config = ProducerConfig::parse_str(&toml, Path::new("producer.toml")).unwrap();
    config.validate().unwrap();
    config
}

fn seed(pg: &ManagedPostgres) -> Result<(), StorageError> {
    pg.execute_sql(
        "INSERT INTO documents (document_id, source, kind, source_uid, citation, title, body, \
           valid_from, source_payload_hash, canonical_json) \
         VALUES ('cass:P1','cass','decision','cass:P1','Cass','Arret','corps','2024-01-01', \
           'sha256:p1','{}'); \
         INSERT INTO chunks (chunk_id, document_id, chunk_index, body, contextualized_body, \
           source_payload_hash, chunk_builder_version, embedding_fingerprint) \
         VALUES ('cass:P1#0','cass:P1',0,'corps','ctx corps','sha256:c','c1','bge-m3:1024:normalize:true');",
    )?;
    pg.execute_sql(&format!(
        "INSERT INTO chunk_embeddings (chunk_id, embedding_fingerprint, embedding, model, dimension) \
         VALUES ('cass:P1#0','{FP}','{}'::vector,'bge-m3',1024);",
        vector(),
    ))?;
    Ok(())
}

#[test]
fn publish_baseline_over_the_writer_handle_publishes_and_self_verifies() -> Result<(), StorageError>
{
    let Ok(pg_config) = PgConfig::discover() else {
        eprintln!("SKIP: no pgvector/pg_search assets via JURISEARCH_PG_CONFIG");
        return Ok(());
    };
    let proot = tempfile::tempdir().map_err(StorageError::Io)?;
    let pg = ManagedPostgres::start_durable(pg_config, proot.path())?;
    pg.run_migrations()?;
    seed(&pg)?;

    let work = tempfile::tempdir().map_err(StorageError::Io)?;
    let config = producer_config(pg.port, work.path());

    // Default stable baseline id (`core-bootstrap-v1`), through the external WriterHandle.
    let report = run_publish_baseline(&config, None).expect("publish-baseline succeeds");
    assert_eq!(report.exit_class, "published");
    assert_eq!(report.package_id, "core-1-1");
    assert_eq!(report.generation, "core_g0001");
    assert_eq!(report.baseline_id, "core-bootstrap-v1");
    assert_eq!(report.sequence, 1);
    assert_eq!(report.head_sequence, 1);
    assert!(report.artifacts_verified >= 1);

    let served = config.producer.corpora_dir.clone();
    let status =
        pg.execute_sql("SELECT status FROM package_catalog WHERE package_id='core-1-1';")?;
    assert_eq!(status.trim(), "published");
    assert!(report.manifest_path.exists(), "signed manifest published");
    assert!(
        served.join("core/packages/core-1-1/manifest.json").exists(),
        "served artifact exists"
    );

    // Independently re-verify with the producer signer's PUBLIC trust anchor.
    let signer = config.signer().expect("signer");
    let verifier = Ed25519Verifier::from_anchors(&[signer.trust_anchor()]).expect("verifier");
    let verified = verify_published_root(&served, "core", &config.package.uri_base, &verifier)
        .expect("published root verifies");
    assert_eq!(verified.head_sequence, 1);

    // A second run is refused — the first baseline is already published and its manifest verifies, so the
    // resume-aware bootstrap hits the specific "already baselined" refusal (publish-failed, not a dup).
    let again = run_publish_baseline(&config, None);
    let err = again.expect_err("a second publish-baseline is refused");
    assert!(matches!(err, ProducerError::Build(_)), "got {err:?}");
    assert_eq!(err.class(), "publish-failed");
    let msg = err.to_string();
    assert!(
        msg.contains("already published") && msg.contains("verifies"),
        "refusal names the already-published, verifying baseline: {err}"
    );
    Ok(())
}
