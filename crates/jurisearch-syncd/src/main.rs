//! `jurisearch-syncd` binary (plan P3 C2): a minimal consumer service skeleton — read a local artifact
//! directory, apply a baseline onto the client index, and report `corpus status`. It owns the DB
//! lifecycle (opens a durable managed Postgres at the index dir, runs migrations) rather than depending
//! on a user closing a CLI session (contrast `serve.rs`). Trust is stubbed in P3 (`AcceptAllVerifier`);
//! the real verifier lands in P6.

use std::path::PathBuf;

use clap::{Parser, Subcommand};
use jurisearch_package::crypto::AcceptAllVerifier;
use jurisearch_storage::runtime::{ManagedPostgres, PgConfig};
use jurisearch_syncd::{BaselineApplyOutcome, apply_baseline, corpus_status};

#[derive(Parser)]
#[command(
    name = "jurisearch-syncd",
    about = "JuriSearch consumer sync service (baseline apply + status)"
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
    /// Apply a baseline artifact directory into the client index.
    Apply {
        /// Path to the baseline artifact directory (containing `manifest.json` + `payload/`).
        #[arg(long)]
        artifact: PathBuf,
    },
    /// Report the cursor authority's view of every installed corpus.
    Status,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let pg_config = PgConfig::discover()?;
    let postgres = ManagedPostgres::start_durable(pg_config, &cli.index_dir)?;
    postgres.run_migrations()?;

    match cli.command {
        Command::Apply { artifact } => {
            let outcome = apply_baseline(&postgres, &artifact, &AcceptAllVerifier)?;
            match outcome {
                BaselineApplyOutcome::Applied {
                    corpus,
                    generation,
                    sequence,
                    index_report,
                } => println!(
                    "applied baseline: corpus={corpus} generation={generation} sequence={sequence} [{index_report}]"
                ),
                BaselineApplyOutcome::AlreadyApplied { corpus, sequence } => {
                    println!("already applied: corpus={corpus} sequence={sequence} (no-op)");
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
