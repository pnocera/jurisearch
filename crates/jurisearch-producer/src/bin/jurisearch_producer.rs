//! `jurisearch-producer` — update-server runtime/admin CLI (work/10 M2-B + M3).
//!
//! Owns the update-server runtime administration surface (resolved decision #9); `jurisearchctl` stays
//! focused on customer site deployment. Each subcommand delegates to the typed library APIs and prints a
//! JSON report so a timer/wrapper can parse the outcome. The process EXIT CODE is the classified exit
//! class (see [`jurisearch_producer::exit`]), so a plain cron/systemd wrapper can branch without parsing
//! JSON; the configured alert hook fires on the failure classes.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use jurisearch_fetch::ArchiveSource;
use jurisearch_producer::alert::{self, AlertEvent};
use jurisearch_producer::config::ProducerConfig;
use jurisearch_producer::error::ProducerError;
use jurisearch_producer::exit::exit_code_for;
use jurisearch_producer::freshness::JudilibreAccelerator;
use jurisearch_producer::update::{UpdateOptions, plan_forced_rebaseline};
use jurisearch_producer::{
    PRODUCER_CONFIG_EXAMPLE, build_status, cron_equivalent, fetch, install, provision_db,
    run_retention, run_update,
};
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
    /// Render + install the systemd service/timer units (and print the cron equivalent).
    Install {
        #[arg(long)]
        config: PathBuf,
        /// Render the units and print them WITHOUT writing anything.
        #[arg(long)]
        dry_run: bool,
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
    /// Run the full update for a fetch group: (fetch) → ingest → enrich → embed → publish core. Adopts a
    /// newer DILA baseline automatically via a recorded rebaseline run.
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
    /// Report current/stale/broken state as JSON (no logs, no DB connection).
    Status {
        #[arg(long)]
        config: PathBuf,
    },
    /// Operator REPAIR: force a recorded rebaseline (discard-and-rebuild) run for a source's group,
    /// re-anchoring `core` to the currently-fetched DILA baselines. Drives the SAME `rebaseline_cycle`
    /// path, lock, integrity/order/convergence checks, per-source adoption, and run record as the
    /// automatic path. NOT the default operating mode — automatic rebaseline shipped in M3.
    Rebaseline {
        #[arg(long)]
        config: PathBuf,
        /// One of: legi, cass, capp, inca, jade. The repair runs over the source's fetch group.
        #[arg(long)]
        source: String,
        /// Report what it WOULD rebaseline/re-adopt without fetching, locking, or mutating anything.
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        skip_fetch: bool,
        #[arg(long)]
        skip_enrich: bool,
    },
    /// Reclaim ONLY temporary/partial/quarantined download files. Accepted official archives and
    /// published packages are retained indefinitely and never touched. `--dry-run` (default) reports only.
    Retention {
        #[arg(long)]
        config: PathBuf,
        /// Report reclaimable files without deleting (the default; mutually exclusive with `--delete`).
        #[arg(long)]
        dry_run: bool,
        /// Opt in to actually deleting the reclaimable temp/partial/quarantine files.
        #[arg(long)]
        delete: bool,
    },
    /// Report the v1 freshness policy and the DEFERRED Judilibre accelerator status (decision #6).
    Freshness,
}

/// A command's JSON document plus its classified exit class (drives the process exit code).
struct CommandOutput {
    json: serde_json::Value,
    exit_class: &'static str,
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(out) => {
            println!("{}", serde_json::to_string_pretty(&out.json).expect("json"));
            ExitCode::from(exit_code_for(out.exit_class))
        }
        Err(err) => {
            let payload = json!({
                "status": "error",
                "exit_class": err.class(),
                "message": err.to_string(),
            });
            eprintln!("{}", serde_json::to_string_pretty(&payload).expect("json"));
            ExitCode::from(exit_code_for(err.class()))
        }
    }
}

fn run(cli: Cli) -> Result<CommandOutput, ProducerError> {
    match cli.command {
        Command::ConfigExample => Ok(ok(
            "ok",
            json!({ "producer_toml": PRODUCER_CONFIG_EXAMPLE }),
        )),
        Command::Validate { config } => {
            let cfg = ProducerConfig::load(&config)?;
            Ok(ok(
                "ok",
                json!({
                    "status": "ok",
                    "command": "validate",
                    "corpus": cfg.package.corpus,
                    "fetch_groups": cfg.fetch_groups.iter().map(|g| &g.name).collect::<Vec<_>>(),
                    "storage_embedding_fingerprint": cfg.storage_embedding_fingerprint(),
                }),
            ))
        }
        Command::ProvisionDb { config } => {
            let cfg = ProducerConfig::load(&config)?;
            let report = provision_db(&cfg)?;
            Ok(ok(
                "ok",
                json!({
                    "status": "ok",
                    "command": "provision-db",
                    "database": cfg.database.name,
                    "database_created": report.database_created,
                    "schema_version": report.schema_version,
                    "extensions_present": report.extensions_present,
                    "roles_provisioned": report.roles_provisioned,
                }),
            ))
        }
        Command::Install { config, dry_run } => {
            let cfg = ProducerConfig::load(&config)?;
            let report = install(&cfg, dry_run)?;
            Ok(ok(
                "ok",
                json!({
                    "status": "ok",
                    "command": "install",
                    "dry_run": report.dry_run,
                    "unit_dir": report.unit_dir,
                    "written": report.written,
                    "enable_timers": report.timers,
                    "cron_equivalent": cron_equivalent(&cfg),
                }),
            ))
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
            Ok(ok(
                "ok",
                json!({
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
                }),
            ))
        }
        Command::Update {
            config,
            group,
            dry_run,
            skip_fetch,
            skip_enrich,
        } => {
            let cfg = ProducerConfig::load(&config)?;
            let mut options = UpdateOptions::new(group.clone());
            options.dry_run = dry_run;
            options.skip_fetch = skip_fetch;
            options.skip_enrich = skip_enrich;
            match run_update(&cfg, &options) {
                Ok(report) => {
                    // Fire the alert hook if this (success) class is a trigger — by default it is NOT, so
                    // a healthy or degraded run is silent; an operator may widen `[alert].on_classes`.
                    let _ = alert::fire_if_triggered(
                        &cfg,
                        &AlertEvent {
                            exit_class: report.exit_class,
                            group: &report.group,
                            run_id: &report.run_id,
                            message: "update completed",
                        },
                    );
                    Ok(CommandOutput {
                        exit_class: report.exit_class,
                        json: json!({
                            "status": "ok",
                            "command": "update",
                            "group": report.group,
                            "run_id": report.run_id,
                            "sources": report.sources,
                            "dry_run": report.dry_run,
                            "exit_class": report.exit_class,
                            "rebaselined": report.rebaselined,
                            "adopted_baselines": report.adopted_baselines,
                            "built_incremental": report.built_incremental,
                            "enrichment": format!("{:?}", report.enrichment),
                            "fetch_cursors": report.fetch_cursors.len(),
                            "ingest_journals": report.ingest_journals.len(),
                        }),
                    })
                }
                Err(err) => {
                    // Fail-closed: fire the alert hook on the failure class before propagating.
                    let _ = alert::fire_if_triggered(
                        &cfg,
                        &AlertEvent {
                            exit_class: err.class(),
                            group: &group,
                            run_id: "",
                            message: &err.to_string(),
                        },
                    );
                    Err(err)
                }
            }
        }
        Command::Status { config } => {
            let cfg = ProducerConfig::load(&config)?;
            let status = build_status(&cfg)?;
            Ok(ok(
                "ok",
                serde_json::to_value(&status).expect("status json"),
            ))
        }
        Command::Rebaseline {
            config,
            source,
            dry_run,
            skip_fetch,
            skip_enrich,
        } => {
            let cfg = ProducerConfig::load(&config)?;
            // A rebaseline re-anchors the whole `core` corpus; the run is locked + ingested per GROUP, so
            // resolve the requested SOURCE to its fetch group and run the repair over that group.
            let group = cfg.group_for_source(&source)?;
            if dry_run {
                // Pure preview: no fetch, no lock, no mutation — just what a real run WOULD re-anchor.
                let plan = plan_forced_rebaseline(&cfg, &group)?;
                return Ok(CommandOutput {
                    exit_class: "dry-run",
                    json: json!({
                        "status": "ok",
                        "command": "rebaseline",
                        "dry_run": true,
                        "source": source,
                        "group": plan.group,
                        "sources": plan.sources,
                        "would_rebaseline": plan.has_work(),
                        "planned_baselines": plan
                            .baselines
                            .iter()
                            .map(|(token, name)| json!({ "source": token, "baseline": name }))
                            .collect::<Vec<_>>(),
                        "exit_class": "dry-run",
                    }),
                });
            }
            let mut options = UpdateOptions::rebaseline(group.clone());
            options.skip_fetch = skip_fetch;
            options.skip_enrich = skip_enrich;
            match run_update(&cfg, &options) {
                Ok(report) => {
                    let _ = alert::fire_if_triggered(
                        &cfg,
                        &AlertEvent {
                            exit_class: report.exit_class,
                            group: &report.group,
                            run_id: &report.run_id,
                            message: "rebaseline completed",
                        },
                    );
                    Ok(CommandOutput {
                        exit_class: report.exit_class,
                        json: json!({
                            "status": "ok",
                            "command": "rebaseline",
                            "dry_run": false,
                            "source": source,
                            "group": report.group,
                            "run_id": report.run_id,
                            "sources": report.sources,
                            "exit_class": report.exit_class,
                            "rebaselined": report.rebaselined,
                            "adopted_baselines": report.adopted_baselines,
                            "published_package": report.built_incremental,
                            "enrichment": format!("{:?}", report.enrichment),
                        }),
                    })
                }
                Err(err) => {
                    let _ = alert::fire_if_triggered(
                        &cfg,
                        &AlertEvent {
                            exit_class: err.class(),
                            group: &group,
                            run_id: "",
                            message: &err.to_string(),
                        },
                    );
                    Err(err)
                }
            }
        }
        Command::Retention {
            config,
            dry_run,
            delete,
        } => {
            let cfg = ProducerConfig::load(&config)?;
            // Default-safe: delete ONLY when `--delete` is explicit. `--dry-run` is the default and
            // never combines with delete.
            if dry_run && delete {
                return Err(ProducerError::ConfigInvalid(
                    "pass either --dry-run (default) or --delete, not both".to_owned(),
                ));
            }
            let report = run_retention(&cfg, delete)?;
            Ok(ok(
                "ok",
                json!({
                    "status": "ok",
                    "command": "retention",
                    "dry_run": report.dry_run,
                    "reclaimable_files": report.reclaimable_files,
                    "reclaimable_bytes": report.reclaimable_bytes,
                    "deleted_files": report.deleted_files,
                    "deleted_bytes": report.deleted_bytes,
                    "items": serde_json::to_value(&report.items).expect("items json"),
                }),
            ))
        }
        Command::Freshness => Ok(ok(
            "ok",
            json!({
                "status": "ok",
                "command": "freshness",
                "judilibre_accelerator": serde_json::to_value(JudilibreAccelerator::status())
                    .expect("freshness json"),
            }),
        )),
    }
}

/// A success [`CommandOutput`] with a fixed exit class (`ok` ⇒ process exit 0; not in the failure set).
fn ok(exit_class: &'static str, json: serde_json::Value) -> CommandOutput {
    CommandOutput { json, exit_class }
}
