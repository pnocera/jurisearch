//! `jurisearch-package` — the producer package CLI (plan P9): build / publish / list / verify signed
//! artifacts + the per-corpus signed remote manifest. A focused operator surface over the
//! `jurisearch-package-build` library (kept a dedicated binary rather than threading the large
//! `jurisearch` clap tree, to isolate the producer surface). Real TLS/HTTP/CDN hosting + cron are ops.

use std::collections::BTreeMap;
use std::path::PathBuf;

use clap::{Parser, Subcommand};
use jurisearch_package::compat::Version;
use jurisearch_package::crypto::{Ed25519Signer, Ed25519Verifier, KeyEpoch, KeyId, TrustAnchor};
use jurisearch_package::manifest::remote::{CatchupPolicy, EntitlementTier};
use jurisearch_package_build::remote_manifest::build_remote_manifest;
use jurisearch_package_build::{
    BaselineParams, IncrementalParams, RemoteManifestParams, build_baseline, build_incremental,
    build_rebaseline, publish_package, publish_remote_manifest, verify_published_root,
};
use jurisearch_storage::package_catalog::catalog_rows_for_corpus;
use jurisearch_storage::runtime::{ManagedPostgres, PgConfig};

#[derive(Parser)]
#[command(
    name = "jurisearch-package",
    about = "Producer package builder/publisher (plan P9)"
)]
struct Cli {
    /// The producer index directory (a durable managed Postgres is started here). Required for
    /// commands that read the producer DB (build / publish-manifest / list / verify).
    #[arg(long, global = true)]
    index_dir: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Build + sign one artifact (baseline | incremental | rebaseline) into `--artifact-dir`.
    Build(BuildArgs),
    /// Publish a built artifact directory under the served root (`<root>/<corpus>/packages/<id>`).
    Publish {
        #[arg(long)]
        root: PathBuf,
        #[arg(long)]
        corpus: String,
        #[arg(long)]
        package_id: String,
        #[arg(long)]
        artifact_dir: PathBuf,
    },
    /// Build + sign + publish the per-corpus remote manifest from the published artifacts.
    PublishManifest(ManifestArgs),
    /// List the producer catalog chain for a corpus.
    List {
        #[arg(long)]
        corpus: String,
    },
    /// READ-ONLY verify of the PUBLISHED root (the manifest clients poll) with a PUBLIC key — checks the
    /// signed manifest's signature/corpus and every referenced artifact's existence/sha256/signature.
    Verify(VerifyArgs),
}

#[derive(clap::Args)]
struct VerifyArgs {
    #[arg(long)]
    root: PathBuf,
    #[arg(long)]
    corpus: String,
    /// The PUBLIC 32-byte verifying key (64 lowercase-hex) — never the private signing seed.
    #[arg(long)]
    public_key_hex: String,
    #[arg(long, default_value = "producer-k1")]
    key_id: String,
    #[arg(long, default_value_t = 1)]
    key_epoch: u32,
    #[arg(long, default_value = "media://")]
    uri_base: String,
}

#[derive(clap::Args)]
struct BuildArgs {
    #[arg(long)]
    corpus: String,
    /// `baseline` | `incremental` | `rebaseline`.
    #[arg(long)]
    kind: String,
    #[arg(long)]
    artifact_dir: PathBuf,
    #[arg(long, default_value = "core-baseline")]
    baseline_id: String,
    #[arg(long, default_value = "operator")]
    builder_run_id: String,
    #[arg(long, default_value = "1970-01-01T00:00:00Z")]
    created_at: String,
    #[arg(long, default_value = "bge-m3:1024:cls:normalize=true")]
    embedding_fingerprint: String,
    /// 64 lowercase-hex chars (a 32-byte Ed25519 seed).
    #[arg(long)]
    signing_seed_hex: String,
    #[arg(long, default_value = "producer-k1")]
    key_id: String,
    #[arg(long, default_value_t = 1)]
    key_epoch: u32,
}

#[derive(clap::Args)]
struct ManifestArgs {
    #[arg(long)]
    root: PathBuf,
    #[arg(long)]
    corpus: String,
    #[arg(long)]
    signing_seed_hex: String,
    #[arg(long, default_value = "producer-k1")]
    key_id: String,
    #[arg(long, default_value_t = 1)]
    key_epoch: u32,
    #[arg(long, default_value = "media://")]
    uri_base: String,
    #[arg(long, default_value_t = 120)]
    max_retained_incrementals: usize,
    #[arg(long, default_value = "jurisearch")]
    publisher: String,
    #[arg(long, default_value = "production")]
    environment: String,
    #[arg(long, default_value = "1970-01-01T00:00:00Z")]
    generated_at: String,
}

fn open_index(index_dir: &Option<PathBuf>) -> anyhow::Result<ManagedPostgres> {
    let dir = index_dir
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("--index-dir is required for this command"))?;
    let pg_config = PgConfig::discover()?;
    Ok(ManagedPostgres::start_durable(pg_config, dir)?)
}

fn signer(seed_hex: &str, key_id: &str, key_epoch: u32) -> anyhow::Result<Ed25519Signer> {
    let bytes = hex::decode(seed_hex)?;
    let seed: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| anyhow::anyhow!("--signing-seed-hex must be 64 hex chars (32 bytes)"))?;
    Ok(Ed25519Signer::from_seed(
        &seed,
        KeyId(key_id.to_owned()),
        KeyEpoch(key_epoch),
    ))
}

fn manifest_params(args: &ManifestArgs, signer: &Ed25519Signer) -> RemoteManifestParams {
    RemoteManifestParams {
        publisher: args.publisher.clone(),
        environment: args.environment.clone(),
        generated_at: args.generated_at.clone(),
        catchup_policy: CatchupPolicy {
            max_incremental_packages: 120,
            max_cumulative_diff_to_baseline_permille: 330,
            max_cumulative_uncompressed_to_baseline_permille: 500,
            max_apply_seconds_budget: 2700,
        },
        entitlement_tier: EntitlementTier::Open,
        license_epoch: 0,
        audience: None,
        signing_key_id: signer.key_id().clone(),
        uri_base: args.uri_base.clone(),
        max_retained_incrementals: args.max_retained_incrementals,
        default_apply_seconds: 60,
        default_load_seconds: 600,
    }
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Build(args) => {
            let producer = open_index(&cli.index_dir)?;
            producer.run_migrations()?;
            let sgnr = signer(&args.signing_seed_hex, &args.key_id, args.key_epoch)?;
            let mut builders = BTreeMap::new();
            builders.insert("chunker".to_owned(), "c1".to_owned());
            let base = BaselineParams {
                baseline_id: args.baseline_id.clone(),
                builder_run_id: args.builder_run_id.clone(),
                created_at: args.created_at.clone(),
                embedding_fingerprint: args.embedding_fingerprint.clone(),
                embedding_model: "bge-m3".to_owned(),
                embedding_dimension: 1024,
                embedding_normalize: true,
                builder_versions: builders.clone(),
                minimum_client_version: Version::new(0, 1, 0),
            };
            match args.kind.as_str() {
                "baseline" => {
                    let report =
                        build_baseline(&producer, &args.corpus, &args.artifact_dir, &sgnr, &base)?;
                    println!(
                        "built baseline {} generation={}",
                        report.package_id, report.generation
                    );
                }
                "rebaseline" => {
                    let report = build_rebaseline(
                        &producer,
                        &args.corpus,
                        &args.artifact_dir,
                        &sgnr,
                        &base,
                    )?;
                    println!(
                        "built rebaseline {} generation={}",
                        report.package_id, report.generation
                    );
                }
                "incremental" => {
                    let inc = IncrementalParams {
                        builder_run_id: args.builder_run_id.clone(),
                        created_at: args.created_at.clone(),
                        embedding_fingerprint: args.embedding_fingerprint.clone(),
                        embedding_model: "bge-m3".to_owned(),
                        embedding_dimension: 1024,
                        embedding_normalize: true,
                        builder_versions: builders,
                        minimum_client_version: Version::new(0, 1, 0),
                    };
                    match build_incremental(
                        &producer,
                        &args.corpus,
                        &args.artifact_dir,
                        &sgnr,
                        &inc,
                    )? {
                        Some(report) => println!("built incremental {}", report.package_id),
                        None => println!("no changes in the outbox window — no incremental built"),
                    }
                }
                other => {
                    anyhow::bail!("unknown --kind `{other}` (baseline|incremental|rebaseline)")
                }
            }
        }
        Command::Publish {
            root,
            corpus,
            package_id,
            artifact_dir,
        } => {
            let dest = publish_package(&root, &corpus, &package_id, &artifact_dir)?;
            println!("published {package_id} -> {}", dest.display());
        }
        Command::PublishManifest(args) => {
            let producer = open_index(&cli.index_dir)?;
            producer.run_migrations()?;
            let sgnr = signer(&args.signing_seed_hex, &args.key_id, args.key_epoch)?;
            let manifest = build_remote_manifest(
                &producer,
                &args.corpus,
                &args.root,
                &sgnr,
                &manifest_params(&args, &sgnr),
            )?;
            let path = publish_remote_manifest(&args.root, &args.corpus, &manifest)?;
            println!(
                "published manifest head={} min_available={} packages={} -> {}",
                manifest.payload.head_sequence.get(),
                manifest.payload.min_available_sequence.get(),
                manifest.payload.packages.len(),
                path.display()
            );
        }
        Command::List { corpus } => {
            let producer = open_index(&cli.index_dir)?;
            producer.run_migrations()?;
            let mut db = producer.client()?;
            for row in catalog_rows_for_corpus(&mut db, &corpus)? {
                println!(
                    "seq={} id={} kind={} generation={} status={}",
                    row.package_sequence,
                    row.package_id,
                    row.package_kind,
                    row.generation,
                    row.status
                );
            }
        }
        Command::Verify(args) => {
            // Read-only verify with a PUBLIC key (no DB, no signing seed).
            let bytes = hex::decode(&args.public_key_hex)?;
            let key_bytes: [u8; 32] = bytes
                .as_slice()
                .try_into()
                .map_err(|_| anyhow::anyhow!("--public-key-hex must be 64 hex chars (32 bytes)"))?;
            let anchor = TrustAnchor {
                key_id: KeyId(args.key_id.clone()),
                key_epoch: KeyEpoch(args.key_epoch),
                algorithm: "ed25519".to_owned(),
                public_key_hex: hex::encode(key_bytes),
            };
            let verifier = Ed25519Verifier::from_anchors(&[anchor])
                .map_err(|error| anyhow::anyhow!("trust anchor invalid: {error}"))?;
            let report =
                verify_published_root(&args.root, &args.corpus, &args.uri_base, &verifier)?;
            println!(
                "OK: published root verifies for `{}` (head={} packages={} artifacts_checked={})",
                report.corpus, report.head_sequence, report.packages, report.artifacts_checked
            );
        }
    }
    Ok(())
}
