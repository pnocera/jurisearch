//! `jurisearch-syncd` binary (plan P3 C2, hardened in P6, operated in P9): the consumer client. It owns
//! the DB lifecycle (a durable managed Postgres at the index dir, migrations) and offers the operated
//! client surface — bootstrap trust anchors, `subscribe` a license token, `update` (the P7 plan + apply
//! loop over a filesystem-published artifact root), and `status`. Trust is REAL: the verifier is built
//! from the client's installed `trust_anchor` rows, NEVER `AcceptAllVerifier`.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use jurisearch_package::crypto::{KeyEpoch, KeyId, TrustAnchor};
use jurisearch_package::manifest::RemoteManifest;
use jurisearch_package::signed::Signed;
use jurisearch_storage::runtime::{ManagedPostgres, PgConfig};
use jurisearch_storage::trust::{LICENSE_PURPOSE, PACKAGE_PURPOSE};
use jurisearch_syncd::{
    BaselineApplyOutcome, CatchupPlan, CatchupReport, DirectoryCatchupSource, apply_baseline,
    check_manifest_corpus, corpus_status, install_trust_anchor, install_verified_license_token,
    load_package_verifier, plan_catchup, read_client_cursor, run_catchup,
};

#[derive(Parser)]
#[command(
    name = "jurisearch-syncd",
    about = "JuriSearch consumer client (trust + subscribe + update + status)"
)]
struct Cli {
    /// The client index directory (a durable managed Postgres is started here).
    #[arg(long, global = true)]
    index_dir: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Apply a single baseline artifact directory (low-level; prefer `update`).
    Apply {
        #[arg(long)]
        artifact: PathBuf,
    },
    /// Install a producer trust anchor (bootstrap). Purpose is `package` or `license`.
    Trust {
        #[command(subcommand)]
        action: TrustAction,
    },
    /// Install a signed license token (from `package`-tier producers) to entitle a subscription corpus.
    Subscribe {
        /// Path to a `Signed<LicenseToken>` JSON file.
        #[arg(long)]
        token_json: PathBuf,
    },
    /// Plan + apply catch-up for a corpus from a filesystem-published artifact root (the P7 loop).
    Update {
        #[arg(long)]
        corpus: String,
        /// The published root (`<root>/<corpus>/manifest.json` + `<root>/<corpus>/packages/...`).
        #[arg(long)]
        source_root: PathBuf,
        /// The `artifact_uri` base the producer published with (matches the manifest's URIs).
        #[arg(long, default_value = "media://")]
        uri_base: String,
    },
    /// Report the cursor authority's view of every installed corpus.
    Status,
}

#[derive(Subcommand)]
enum TrustAction {
    /// Install a producer verifying key.
    InstallAnchor {
        #[arg(long, default_value = "package")]
        purpose: String,
        #[arg(long)]
        key_id: String,
        #[arg(long)]
        key_epoch: u32,
        #[arg(long)]
        public_key_hex: String,
        #[arg(long, default_value = "ed25519")]
        algorithm: String,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let pg_config = PgConfig::discover()?;
    let postgres = ManagedPostgres::start_durable(pg_config, &cli.index_dir)?;
    postgres.run_migrations()?;

    match cli.command {
        Command::Apply { artifact } => {
            let verifier = load_package_verifier(&postgres)?;
            let outcome = apply_baseline(&postgres, &artifact, &verifier)?;
            print_baseline_outcome(&outcome);
        }
        Command::Trust { action } => {
            let TrustAction::InstallAnchor {
                purpose,
                key_id,
                key_epoch,
                public_key_hex,
                algorithm,
            } = action;
            let purpose = match purpose.as_str() {
                "package" => PACKAGE_PURPOSE,
                "license" => LICENSE_PURPOSE,
                other => {
                    anyhow::bail!("unknown trust purpose `{other}` (use `package` or `license`)")
                }
            };
            let anchor = TrustAnchor {
                key_id: KeyId(key_id),
                key_epoch: KeyEpoch(key_epoch),
                algorithm,
                public_key_hex,
            };
            install_trust_anchor(&postgres, &anchor, purpose)?;
            println!(
                "installed {purpose} trust anchor key_id={} epoch={key_epoch}",
                anchor.key_id.0
            );
        }
        Command::Subscribe { token_json } => {
            let bytes = std::fs::read_to_string(&token_json)?;
            install_verified_license_token(&postgres, &bytes)?;
            println!("installed license token from {}", token_json.display());
        }
        Command::Update {
            corpus,
            source_root,
            uri_base,
        } => {
            let verifier = load_package_verifier(&postgres)?;
            let manifest_path = source_root.join(&corpus).join("manifest.json");
            let signed: Signed<RemoteManifest> =
                serde_json::from_slice(&std::fs::read(&manifest_path)?)?;
            signed
                .verify(&verifier)
                .map_err(|error| anyhow::anyhow!("remote manifest signature invalid: {error}"))?;
            // The signed manifest must actually be FOR the requested corpus — a stale/misplaced
            // manifest signed for another corpus must not advance the wrong corpus (P9 r1 WARN).
            check_manifest_corpus(&signed.payload, &corpus)?;
            let cursor = read_client_cursor(&postgres, &corpus)?;
            let plan = plan_catchup(&signed.payload, cursor.as_ref());
            match &plan {
                CatchupPlan::UpToDate => println!("{corpus}: up to date"),
                CatchupPlan::FreshBaseline(b) => {
                    println!(
                        "{corpus}: loading baseline {} (sequence {})",
                        b.baseline_id,
                        b.sequence.get()
                    )
                }
                CatchupPlan::Incremental(c) => {
                    println!("{corpus}: applying {} incremental(s)", c.len())
                }
                CatchupPlan::Blocked { code, reason } => {
                    anyhow::bail!("{corpus}: blocked ({code:?}): {reason}")
                }
            }
            let source = DirectoryCatchupSource::new(&source_root, uri_base);
            let report = run_catchup(&postgres, &source, &verifier, plan)?;
            match report {
                CatchupReport::UpToDate => {}
                CatchupReport::BaselineApplied(outcome) => print_baseline_outcome(&outcome),
                CatchupReport::IncrementalApplied { applied } => {
                    println!("{corpus}: applied {applied} incremental(s)")
                }
            }
        }
        Command::Status => {
            let statuses = corpus_status(&postgres)?;
            if statuses.is_empty() {
                println!("no corpus installed");
            }
            for status in statuses {
                println!(
                    "corpus={} generation={} sequence={} baseline={} schema={} last_package={}",
                    status.corpus,
                    status.active_generation,
                    status.sequence,
                    status.baseline_id,
                    status.schema_version,
                    status.last_package_id.as_deref().unwrap_or("-"),
                );
            }
        }
    }
    Ok(())
}

fn print_baseline_outcome(outcome: &BaselineApplyOutcome) {
    match outcome {
        BaselineApplyOutcome::Applied {
            corpus,
            generation,
            sequence,
            index_report,
        } => println!(
            "applied: corpus={corpus} generation={generation} sequence={sequence} [{index_report}]"
        ),
        BaselineApplyOutcome::AlreadyApplied { corpus, sequence } => {
            println!("already applied: corpus={corpus} sequence={sequence} (no-op)")
        }
    }
}
