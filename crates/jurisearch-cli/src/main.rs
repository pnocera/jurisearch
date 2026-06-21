use std::{
    io::{self, BufRead, Write},
    path::PathBuf,
    process::ExitCode,
};

use anyhow::Context;
use clap::{Args, Parser, Subcommand, ValueEnum};
use jurisearch_core::{
    SCHEMA_VERSION,
    contract::{LegalKind, agent_help},
    error::{ErrorObject, ProcessExit},
    schema::compiled_schema,
    session::{SessionRequest, SessionResponse},
};
use jurisearch_ingest::archive::{ArchiveSource, plan_from_dir};
use serde_json::{Value, json};

#[derive(Debug, Parser)]
#[command(name = "jurisearch")]
#[command(version, about = "Local-first French legal search CLI for AI agents.")]
#[command(disable_help_subcommand = true)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Return compact ranked candidates.
    Search(SearchArgs),
    /// Return full source text for selected stable IDs.
    Fetch(FetchArgs),
    /// Verify citations and identifiers.
    Cite(CiteArgs),
    /// Return graph neighbours.
    Related(RelatedArgs),
    /// Return structural neighbourhood.
    Context(ContextArgs),
    /// Return legal-vocabulary expansions.
    Expand(QueryArgs),
    /// Report corpus coverage, model fingerprints, and index health.
    Status,
    /// Explicit model-cache operations.
    Model(ModelCommand),
    /// Check or prepare local setup.
    Setup,
    /// Warm JSONL subprocess protocol.
    Session(JsonlArgs),
    /// Finite JSONL protocol for eval/bulk runs.
    Batch(JsonlArgs),
    /// Official-source ingestion helpers.
    Ingest(IngestCommand),
    /// Synchronize official sources.
    Sync(SyncArgs),
    /// Compiled agent help and schemas.
    Help(HelpCommand),
}

#[derive(Debug, Args)]
struct SearchArgs {
    query: String,
    #[arg(long, default_value = "all")]
    kind: CliKind,
    #[arg(long, default_value_t = 10)]
    top_k: u32,
    #[arg(long)]
    as_of: Option<String>,
}

#[derive(Debug, Args)]
struct FetchArgs {
    ids: Vec<String>,
    #[arg(long)]
    as_of: Option<String>,
    #[arg(long)]
    part: Option<String>,
}

#[derive(Debug, Args)]
struct CiteArgs {
    cite: String,
    #[arg(long)]
    strict: bool,
    #[arg(long)]
    online: bool,
}

#[derive(Debug, Args)]
struct RelatedArgs {
    id: String,
    #[arg(long)]
    rel: Option<String>,
}

#[derive(Debug, Args)]
struct ContextArgs {
    id: String,
    #[arg(long)]
    siblings: bool,
    #[arg(long)]
    as_of: Option<String>,
}

#[derive(Debug, Args)]
struct QueryArgs {
    query: String,
}

#[derive(Debug, Args)]
struct JsonlArgs {
    #[arg(long)]
    jsonl: bool,
    #[arg(long)]
    fatal: bool,
}

#[derive(Debug, Args)]
struct SyncArgs {
    #[arg(long)]
    source: Option<String>,
    #[arg(long)]
    since: Option<String>,
}

#[derive(Debug, Args)]
struct ModelCommand {
    #[command(subcommand)]
    command: Option<ModelSubcommand>,
}

#[derive(Debug, Subcommand)]
enum ModelSubcommand {
    Fetch {
        model: Option<String>,
        #[arg(long)]
        allow_download: bool,
    },
}

#[derive(Debug, Args)]
struct IngestCommand {
    #[command(subcommand)]
    command: Option<IngestSubcommand>,
}

#[derive(Debug, Subcommand)]
enum IngestSubcommand {
    /// Dry-run official archive precedence and delta ordering.
    PlanArchives {
        #[arg(long, default_value = "legi")]
        source: CliArchiveSource,
        #[arg(long)]
        archives_dir: PathBuf,
    },
}

#[derive(Debug, Args)]
struct HelpCommand {
    #[command(subcommand)]
    command: Option<HelpSubcommand>,
}

#[derive(Debug, Subcommand)]
enum HelpSubcommand {
    Agent,
    Schema {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliKind {
    Code,
    Decision,
    All,
}

impl From<CliKind> for LegalKind {
    fn from(kind: CliKind) -> Self {
        match kind {
            CliKind::Code => Self::Code,
            CliKind::Decision => Self::Decision,
            CliKind::All => Self::All,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliArchiveSource {
    Legi,
}

impl From<CliArchiveSource> for ArchiveSource {
    fn from(source: CliArchiveSource) -> Self {
        match source {
            CliArchiveSource::Legi => Self::Legi,
        }
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            let object = ErrorObject {
                code: jurisearch_core::error::ErrorCode::Internal,
                message: error.to_string(),
                suggestions: Vec::new(),
            };
            let _ = write_json(&json!({ "ok": false, "error": object }));
            ExitCode::from(ProcessExit::Dependency.code() as u8)
        }
    }
}

fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let command = cli.command.unwrap_or(Command::Help(HelpCommand {
        command: Some(HelpSubcommand::Agent),
    }));

    match command {
        Command::Help(help) => emit_help(help),
        Command::Status => write_json(&status_payload()),
        Command::Session(args) => run_jsonl(args, true),
        Command::Batch(args) => run_jsonl(args, false),
        Command::Ingest(ingest) => emit_ingest(ingest),
        Command::Search(args) => {
            if args.query.trim().is_empty() {
                emit_error(ErrorObject::bad_input("search query must not be empty"))
            } else {
                let kind: LegalKind = args.kind.into();
                emit_error(ErrorObject::not_implemented(&format!(
                    "search kind={} top_k={} as_of={}",
                    kind.canonical_result_kind(),
                    args.top_k,
                    args.as_of.as_deref().unwrap_or("none")
                )))
            }
        }
        Command::Fetch(args) => {
            if args.ids.is_empty() {
                emit_error(ErrorObject::bad_input(
                    "fetch requires at least one stable ID",
                ))
            } else {
                emit_error(ErrorObject::not_implemented("fetch"))
            }
        }
        Command::Cite(args) => {
            if args.cite.trim().is_empty() {
                emit_error(ErrorObject::bad_input("cite requires a non-empty citation"))
            } else {
                emit_error(ErrorObject::not_implemented(
                    if args.strict || args.online {
                        "cite --strict/--online"
                    } else {
                        "cite"
                    },
                ))
            }
        }
        Command::Related(args) => emit_error(ErrorObject::not_implemented(&format!(
            "related id={} rel={}",
            args.id,
            args.rel.as_deref().unwrap_or("any")
        ))),
        Command::Context(args) => emit_error(ErrorObject::not_implemented(&format!(
            "context id={} siblings={} as_of={}",
            args.id,
            args.siblings,
            args.as_of.as_deref().unwrap_or("none")
        ))),
        Command::Expand(args) => {
            if args.query.trim().is_empty() {
                emit_error(ErrorObject::bad_input("expand query must not be empty"))
            } else {
                emit_error(ErrorObject::not_implemented("expand"))
            }
        }
        Command::Model(args) => emit_error(ErrorObject::not_implemented(match args.command {
            Some(ModelSubcommand::Fetch { .. }) => "model fetch",
            None => "model",
        })),
        Command::Setup => emit_error(ErrorObject::not_implemented("setup")),
        Command::Sync(args) => emit_error(ErrorObject::not_implemented(&format!(
            "sync source={} since={}",
            args.source.as_deref().unwrap_or("unspecified"),
            args.since.as_deref().unwrap_or("none")
        ))),
    }
}

fn emit_help(help: HelpCommand) -> anyhow::Result<()> {
    match help.command.unwrap_or(HelpSubcommand::Agent) {
        HelpSubcommand::Agent => {
            println!("{}", agent_help());
            Ok(())
        }
        HelpSubcommand::Schema { json: true } => write_json(&compiled_schema()),
        HelpSubcommand::Schema { json: false } => {
            println!("Run `jurisearch help schema --json` for the machine-readable schema.");
            Ok(())
        }
    }
}

fn emit_ingest(ingest: IngestCommand) -> anyhow::Result<()> {
    match ingest.command {
        Some(IngestSubcommand::PlanArchives {
            source,
            archives_dir,
        }) => {
            let source = ArchiveSource::from(source);
            let plan = plan_from_dir(source, &archives_dir).map_err(|error| {
                anyhow::anyhow!(
                    "failed to plan archives in `{}`: {error}",
                    archives_dir.display()
                )
            })?;
            write_json(&json!({
                "schema_version": SCHEMA_VERSION,
                "command": "ingest plan-archives",
                "plan": plan,
            }))
        }
        None => emit_error(ErrorObject::not_implemented("ingest")),
    }
}

fn run_jsonl(args: JsonlArgs, _warm: bool) -> anyhow::Result<()> {
    if !args.jsonl {
        return emit_error(ErrorObject::bad_input(
            "session and batch require the explicit `--jsonl` flag",
        ));
    }

    let stdin = io::stdin();
    let mut stdout = io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line.context("failed to read JSONL stdin")?;
        let (response, should_exit) = match serde_json::from_str::<SessionRequest>(&line) {
            Ok(request) => dispatch_session_request(request),
            Err(error) => {
                let response = SessionResponse::err(
                    None,
                    ErrorObject::bad_input(format!("malformed JSONL request: {error}")),
                );
                if args.fatal {
                    write_session_response(&mut stdout, &response)?;
                    break;
                }
                (response, false)
            }
        };
        write_session_response(&mut stdout, &response)?;
        if should_exit {
            break;
        }
    }
    Ok(())
}

fn dispatch_session_request(request: SessionRequest) -> (SessionResponse, bool) {
    let command = request.command.trim();
    if command == "exit" {
        return (
            SessionResponse::ok(request.id, json!({ "bye": true })),
            true,
        );
    }
    let result = match command {
        "help" | "help agent" => Ok(json!({ "text": agent_help() })),
        "help schema" | "schema" => Ok(compiled_schema()),
        "status" => Ok(status_payload()),
        "search" | "fetch" | "cite" | "related" | "context" | "expand" | "model fetch"
        | "setup" | "ingest" | "sync" => Err(ErrorObject::not_implemented(command)),
        _ => Err(ErrorObject::bad_input(format!(
            "unknown session command `{command}`"
        ))),
    };

    match result {
        Ok(result) => (SessionResponse::ok(request.id, result), false),
        Err(error) => (SessionResponse::err(request.id, error), false),
    }
}

fn status_payload() -> Value {
    json!({
        "schema_version": SCHEMA_VERSION,
        "index": {
            "state": "not_configured",
            "query_ready": false,
            "message": "No index has been built yet; Phase 0 scaffold is installed."
        },
        "embedding": {
            "provider": "openai_compatible",
            "base_url": std::env::var("JURISEARCH_EMBED_BASE_URL").unwrap_or_else(|_| "http://127.0.0.1:8097/v1".into()),
            "model": std::env::var("JURISEARCH_EMBED_MODEL").unwrap_or_else(|_| "bge-m3".into()),
            "dimension": 1024,
            "normalize": true,
            "pooling": "cls"
        },
        "ingest_health": {
            "state": "pending",
            "latest_completed_run": null,
            "projection_coverage": null,
            "embedding_coverage": null,
            "recovery_warnings": []
        }
    })
}

fn emit_error(error: ErrorObject) -> anyhow::Result<()> {
    let exit: ProcessExit = error.code.into();
    write_json(&json!({ "ok": false, "error": error }))?;
    std::process::exit(exit.code());
}

fn write_json(value: &Value) -> anyhow::Result<()> {
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    serde_json::to_writer_pretty(&mut handle, value)?;
    handle.write_all(b"\n")?;
    Ok(())
}

fn write_session_response(
    stdout: &mut io::StdoutLock<'_>,
    response: &SessionResponse,
) -> anyhow::Result<()> {
    serde_json::to_writer(&mut *stdout, response)?;
    stdout.write_all(b"\n")?;
    stdout.flush()?;
    Ok(())
}
