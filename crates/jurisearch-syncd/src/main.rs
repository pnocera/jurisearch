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
use jurisearch_storage::backend::{
    ConnectionConfig, WriterConnection, WriterHandle, WriterVisibility,
};
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
    /// SELF-MANAGED mode: the client index directory (a durable managed Postgres is started here, and
    /// migrations are run). Required unless `--server-host` selects shared-server mode.
    #[arg(long, global = true)]
    index_dir: Option<PathBuf>,
    /// SHARED-SERVER mode (work/09 P2B): attach to an existing, already-migrated + role-provisioned
    /// PostgreSQL as a CLIENT (no `pg_ctl`, no migrations) using the writer identity. Setting this host
    /// selects shared-server mode.
    #[arg(long, global = true)]
    server_host: Option<String>,
    #[arg(long, global = true, default_value_t = 5432)]
    server_port: u16,
    #[arg(long, global = true, default_value = "jurisearch")]
    server_db: String,
    /// The writer role to connect as (shared-server mode).
    #[arg(long, global = true, default_value = "jurisearch_write")]
    writer_user: String,
    #[arg(long, global = true)]
    writer_password: Option<String>,
    /// The read role + view-owner role whose visibility the writer stamps at activation.
    #[arg(long, global = true, default_value = "jurisearch_read")]
    read_role: String,
    #[arg(long, global = true, default_value = "jurisearch_owner")]
    owner_role: String,
    #[command(subcommand)]
    command: Command,
}

/// The writer connection the one-shot commands run through: the self-managed (`pg_ctl`-owned)
/// PostgreSQL, or a shared-server writer handle attached to an existing PG.
enum WriterConn {
    // Boxed: `ManagedPostgres` (temp dir + locks) is much larger than `WriterHandle`.
    SelfManaged(Box<ManagedPostgres>),
    Shared(WriterHandle),
}

impl WriterConn {
    fn conn(&self) -> &dyn WriterConnection {
        match self {
            WriterConn::SelfManaged(postgres) => postgres.as_ref(),
            WriterConn::Shared(handle) => handle,
        }
    }
}

/// Build the writer connection from the CLI: shared-server attach when `--server-host` is set
/// (no migrations, no `pg_ctl`), else the self-managed durable PG (migrated at start).
fn build_writer(cli: &Cli) -> anyhow::Result<WriterConn> {
    if let Some(host) = &cli.server_host {
        let config = ConnectionConfig {
            host: host.clone(),
            port: cli.server_port,
            dbname: cli.server_db.clone(),
            user: cli.writer_user.clone(),
            password: cli.writer_password.clone(),
            application_name: "jurisearch-syncd".to_owned(),
        };
        let visibility = WriterVisibility {
            read_role: cli.read_role.clone(),
            view_owner_role: cli.owner_role.clone(),
        };
        Ok(WriterConn::Shared(WriterHandle::new(config, visibility)))
    } else {
        let index_dir = cli.index_dir.as_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "--index-dir is required in self-managed mode (or pass --server-host to attach to a shared server)"
            )
        })?;
        let pg_config = PgConfig::discover()?;
        let postgres = ManagedPostgres::start_durable(pg_config, index_dir)?;
        postgres.run_migrations()?;
        Ok(WriterConn::SelfManaged(Box::new(postgres)))
    }
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
    Status {
        /// Emit machine-readable JSON (the management CLI's primary result) instead of human lines.
        #[arg(long)]
        json: bool,
    },
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
    let writer = build_writer(&cli)?;
    let conn = writer.conn();

    match cli.command {
        Command::Apply { artifact } => {
            let verifier = load_package_verifier(conn)?;
            let outcome = apply_baseline(conn, &artifact, &verifier)?;
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
            install_trust_anchor(conn, &anchor, purpose)?;
            println!(
                "installed {purpose} trust anchor key_id={} epoch={key_epoch}",
                anchor.key_id.0
            );
        }
        Command::Subscribe { token_json } => {
            let bytes = std::fs::read_to_string(&token_json)?;
            install_verified_license_token(conn, &bytes)?;
            println!("installed license token from {}", token_json.display());
        }
        Command::Update {
            corpus,
            source_root,
            uri_base,
        } => {
            let verifier = load_package_verifier(conn)?;
            let manifest_path = source_root.join(&corpus).join("manifest.json");
            let signed: Signed<RemoteManifest> =
                serde_json::from_slice(&std::fs::read(&manifest_path)?)?;
            signed
                .verify(&verifier)
                .map_err(|error| anyhow::anyhow!("remote manifest signature invalid: {error}"))?;
            // The signed manifest must actually be FOR the requested corpus — a stale/misplaced
            // manifest signed for another corpus must not advance the wrong corpus (P9 r1 WARN).
            check_manifest_corpus(&signed.payload, &corpus)?;
            let cursor = read_client_cursor(conn, &corpus)?;
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
            let report = run_catchup(conn, &source, &verifier, plan)?;
            match report {
                CatchupReport::UpToDate => {}
                CatchupReport::BaselineApplied(outcome) => print_baseline_outcome(&outcome),
                CatchupReport::IncrementalApplied { applied } => {
                    println!("{corpus}: applied {applied} incremental(s)")
                }
            }
        }
        Command::Status { json } => {
            let statuses = corpus_status(conn)?;
            if json {
                // The management CLI's primary result goes to stdout as stable JSON.
                println!("{}", serde_json::to_string_pretty(&statuses)?);
            } else {
                if statuses.is_empty() {
                    println!("no corpus installed");
                }
                for status in statuses {
                    println!(
                        "corpus={} generation={} sequence={} baseline={} schema={} fingerprint={} \
                         last_package={} last_digest={} applied_at={}",
                        status.corpus,
                        status.active_generation,
                        status.sequence,
                        status.baseline_id,
                        status.schema_version,
                        status.embedding_fingerprint,
                        status.last_package_id.as_deref().unwrap_or("-"),
                        status.last_package_digest.as_deref().unwrap_or("-"),
                        status.applied_at.as_deref().unwrap_or("-"),
                    );
                }
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
