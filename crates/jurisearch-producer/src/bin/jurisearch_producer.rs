//! `jurisearch-producer` — update-server runtime/admin CLI (work/10 M2-B).
//!
//! Owns the update-server runtime administration surface (resolved decision #9); `jurisearchctl` stays
//! focused on customer site deployment. Each subcommand delegates to the typed library APIs and prints a
//! JSON report so a timer/wrapper can parse the outcome.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use jurisearch_fetch::ArchiveSource;
use jurisearch_producer::config::ProducerConfig;
use jurisearch_producer::error::ProducerError;
use jurisearch_producer::update::UpdateOptions;
use jurisearch_producer::{PRODUCER_CONFIG_EXAMPLE, fetch, provision_db, run_update};
use serde_json::json;

#[derive(Debug, Parser)]
#[command(
    name = "jurisearch-producer",
    about = "JuriSearch update-server (producer) orchestrator: DILA fetch → ingest → enrich → embed → \
             producer_cycle(core) → signed manifest, over an external PostgreSQL."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print a complete, commented example producer.toml.
    ConfigExample,
    /// Strict-parse and validate a producer.toml.
    Validate {
        #[arg(long)]
        config: PathBuf,
    },
    /// Provision (or converge) the external producer PostgreSQL from [database].
    ProvisionDb {
        #[arg(long)]
        config: PathBuf,
    },
    /// Fetch a DILA source into the Storebox mirror (or report a dry-run plan).
    Fetch {
        #[arg(long)]
        config: PathBuf,
        /// One of: legi, cass, capp, inca, jade.
        #[arg(long)]
        source: String,
        /// List what WOULD be downloaded without downloading.
        #[arg(long)]
        dry_run: bool,
    },
    /// Run the full update for a fetch group: (fetch) → ingest → enrich → embed → publish core.
    Update {
        #[arg(long)]
        config: PathBuf,
        /// The fetch group name (e.g. legislation, jurisprudence).
        #[arg(long)]
        group: String,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        skip_fetch: bool,
        #[arg(long)]
        skip_enrich: bool,
    },
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(value) => {
            println!("{}", serde_json::to_string_pretty(&value).expect("json"));
            ExitCode::SUCCESS
        }
        Err(err) => {
            let payload = json!({
                "status": "error",
                "exit_class": err.class(),
                "message": err.to_string(),
            });
            eprintln!("{}", serde_json::to_string_pretty(&payload).expect("json"));
            ExitCode::FAILURE
        }
    }
}

fn run(cli: Cli) -> Result<serde_json::Value, ProducerError> {
    match cli.command {
        Command::ConfigExample => Ok(json!({ "producer_toml": PRODUCER_CONFIG_EXAMPLE })),
        Command::Validate { config } => {
            let cfg = ProducerConfig::load(&config)?;
            Ok(json!({
                "status": "ok",
                "command": "validate",
                "corpus": cfg.package.corpus,
                "fetch_groups": cfg.fetch_groups.iter().map(|g| &g.name).collect::<Vec<_>>(),
                "storage_embedding_fingerprint": cfg.storage_embedding_fingerprint(),
            }))
        }
        Command::ProvisionDb { config } => {
            let cfg = ProducerConfig::load(&config)?;
            let report = provision_db(&cfg)?;
            Ok(json!({
                "status": "ok",
                "command": "provision-db",
                "database": cfg.database.name,
                "database_created": report.database_created,
                "schema_version": report.schema_version,
                "extensions_present": report.extensions_present,
                "roles_provisioned": report.roles_provisioned,
            }))
        }
        Command::Fetch {
            config,
            source,
            dry_run,
        } => {
            let cfg = ProducerConfig::load(&config)?;
            let source = ArchiveSource::from_token(&source)
                .ok_or_else(|| ProducerError::UnknownSource(source.clone()))?;
            let report = fetch::fetch_source(&cfg, source, dry_run)?;
            Ok(json!({
                "status": "ok",
                "command": "fetch",
                "source": report.source.as_str(),
                "dry_run": report.dry_run,
                "planned_or_downloaded": report.planned_or_downloaded,
                "quarantined": report.quarantined,
                "already_present": report.already_present,
                "listing_total": report.listing_total,
                "cursor": {
                    "latest_file_name": report.cursor.latest_file_name,
                    "latest_compact_timestamp": report.cursor.latest_compact_timestamp,
                },
            }))
        }
        Command::Update {
            config,
            group,
            dry_run,
            skip_fetch,
            skip_enrich,
        } => {
            let cfg = ProducerConfig::load(&config)?;
            let mut options = UpdateOptions::new(group);
            options.dry_run = dry_run;
            options.skip_fetch = skip_fetch;
            options.skip_enrich = skip_enrich;
            let report = run_update(&cfg, &options)?;
            Ok(json!({
                "status": "ok",
                "command": "update",
                "group": report.group,
                "run_id": report.run_id,
                "sources": report.sources,
                "dry_run": report.dry_run,
                "exit_class": report.exit_class,
                "built_incremental": report.built_incremental,
                "enrichment": format!("{:?}", report.enrichment),
                "fetch_cursors": report.fetch_cursors.len(),
                "ingest_journals": report.ingest_journals.len(),
            }))
        }
    }
}
