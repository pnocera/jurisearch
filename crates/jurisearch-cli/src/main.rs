use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{self, BufRead, Write},
    path::{Path, PathBuf},
    process::ExitCode,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Context;
use clap::{Args, Parser, Subcommand, ValueEnum};
use jurisearch_core::{
    SCHEMA_VERSION,
    contract::{LegalKind, agent_help},
    error::{ErrorCode, ErrorObject, ProcessExit},
    schema::compiled_schema,
    session::{SessionRequest, SessionResponse},
};
use jurisearch_embed::{EmbeddingConfig, OpenAiCompatibleClient};
use jurisearch_ingest::{
    archive::{
        ArchiveMember, ArchivePlan, ArchiveSource, ArchiveVisit, DEFAULT_MEMBER_BYTE_LIMIT,
        PlannedArchive, for_each_xml_member_until, plan_from_dir,
    },
    legi::{LegiParseError, ParsedLegiXml, parse_legi_member, source_payload_hash},
};
use jurisearch_storage::{
    dense::{
        DENSE_VECTOR_DIMENSION, DenseRebuildSpec, finalize_dense_rebuild,
        load_chunk_embedding_inputs,
    },
    ingest_accounting::{
        CoverageMetric, IngestCompatibility, IngestErrorInput, IngestHealthReport,
        IngestMemberInput, IngestMemberStatus, IngestResumeAction, IngestRunInput, IngestRunStatus,
        finish_ingest_run, ingest_resume_decision, load_ingest_embedding_coverage,
        load_ingest_health, load_ingest_projection_coverage, record_ingest_error,
        record_ingest_member, start_ingest_run, update_ingest_member_status,
        update_ingest_run_manifest,
    },
    projection::{
        ChunkEmbeddingInsert, LegiHierarchyBackfillScope, LegiMetadataRoot,
        backfill_legi_article_hierarchy_from_metadata,
        backfill_legi_article_hierarchy_from_metadata_scoped, insert_chunk_embeddings,
        insert_legi_documents, insert_legi_metadata_roots,
    },
    retrieval::{
        FetchDocumentsQuery, HybridCandidateQuery, fetch_documents_json, hybrid_candidates_json,
    },
    runtime::{ManagedPostgres, PgConfig, StorageError},
};
use serde::Deserialize;
use serde_json::{Value, json};

const LEGI_PARSER_VERSION: &str = "legi_article_metadata_parser:v2";
const CANONICAL_SCHEMA_VERSION: &str = "canonical_record:v2";
const CLI_CODE_VERSION: &str = concat!("jurisearch-cli:", env!("CARGO_PKG_VERSION"));

#[derive(Debug, Parser)]
#[command(name = "jurisearch")]
#[command(version, about = "Local-first French legal search CLI for AI agents.")]
#[command(disable_help_subcommand = true)]
struct Cli {
    #[arg(long, env = "JURISEARCH_INDEX_DIR", global = true)]
    index_dir: Option<PathBuf>,
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

#[derive(Debug, Deserialize)]
struct SessionSearchArgs {
    query: String,
    #[serde(default = "default_cli_kind")]
    kind: CliKind,
    #[serde(default = "default_top_k")]
    top_k: u32,
    #[serde(default)]
    as_of: Option<String>,
    #[serde(default)]
    index_dir: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
struct SessionFetchArgs {
    ids: Vec<String>,
    #[serde(default)]
    as_of: Option<String>,
    #[serde(default)]
    part: Option<String>,
    #[serde(default)]
    index_dir: Option<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
struct SessionStatusArgs {
    #[serde(default)]
    index_dir: Option<PathBuf>,
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
    /// Stream official LEGI archives into canonical storage with ingest accounting.
    LegiArchives {
        #[arg(long)]
        archives_dir: PathBuf,
        #[arg(long)]
        run_id: Option<String>,
        #[arg(long)]
        limit_members: Option<u32>,
        #[arg(long, default_value_t = DEFAULT_MEMBER_BYTE_LIMIT)]
        max_member_bytes: u64,
        #[arg(long)]
        quarantine_dir: Option<PathBuf>,
        #[arg(long)]
        safe_mode: bool,
    },
    /// Embed stored canonical chunks and finalize the dense ANN index.
    EmbedChunks {
        /// Maximum chunk count allowed for this run; refuses larger indexes instead of finalizing partial coverage.
        #[arg(long)]
        limit: Option<u32>,
        /// Number of ivfflat lists to use when rebuilding the dense vector index.
        #[arg(long, default_value_t = 32)]
        index_lists: u32,
    },
    /// Rebuild LEGI article hierarchy from persisted metadata across the full index.
    BackfillLegiHierarchy,
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

#[derive(Debug, Clone, Copy, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
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
    let index_dir = cli.index_dir;
    let command = cli.command.unwrap_or(Command::Help(HelpCommand {
        command: Some(HelpSubcommand::Agent),
    }));

    match command {
        Command::Help(help) => emit_help(help),
        Command::Status => write_json(&status_payload(index_dir.as_deref())),
        Command::Session(args) => run_jsonl(args, true),
        Command::Batch(args) => run_jsonl(args, false),
        Command::Ingest(ingest) => emit_ingest(ingest, index_dir.as_deref()),
        Command::Search(args) => {
            if args.query.trim().is_empty() {
                emit_error(ErrorObject::bad_input("search query must not be empty"))
            } else if args.top_k == 0 {
                emit_error(ErrorObject::bad_input("search --top-k must be at least 1"))
            } else {
                emit_search(args, index_dir.as_deref())
            }
        }
        Command::Fetch(args) => {
            if args.ids.is_empty() {
                emit_error(ErrorObject::bad_input(
                    "fetch requires at least one stable ID",
                ))
            } else {
                emit_fetch(args, index_dir.as_deref())
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

fn emit_search(args: SearchArgs, index_dir: Option<&Path>) -> anyhow::Result<()> {
    match search_payload(args, index_dir) {
        Ok(response) => write_json(&response),
        Err(error) => emit_error(error),
    }
}

fn search_payload(args: SearchArgs, index_dir: Option<&Path>) -> Result<Value, ErrorObject> {
    let query_text = parade_query_text(&args.query).ok_or_else(|| {
        ErrorObject::bad_input("search query must contain at least one searchable token")
    })?;
    let index_dir = require_existing_index_dir(index_dir)?;
    let kind: LegalKind = args.kind.into();
    if matches!(kind, LegalKind::Decision) {
        return Err(ErrorObject::bad_input(
            "Phase 0.6 search currently supports `--kind all` or `--kind code` over the LEGI subset",
        ));
    }

    let postgres = open_index(index_dir.as_path())?;
    ensure_query_readiness(&postgres, QueryReadinessGate::Search)?;
    let embedding_config = embedding_config_from_env();
    let expected_fingerprint = embedding_config.fingerprint();
    let embedding_fingerprint = embedding_config.storage_embedding_fingerprint();
    let client = OpenAiCompatibleClient::new(embedding_config).map_err(embedding_error_object)?;
    let embedding = client
        .embed_query(args.query.as_str(), &expected_fingerprint)
        .map_err(embedding_error_object)?;
    let query_embedding = pgvector_literal(&embedding.values);
    let as_of = args.as_of.unwrap_or_else(today_utc);
    let response = hybrid_candidates_json(
        &postgres,
        &HybridCandidateQuery {
            query_text: query_text.as_str(),
            query_embedding: query_embedding.as_str(),
            embedding_fingerprint: embedding_fingerprint.as_str(),
            as_of: as_of.as_str(),
            kind_filter: if matches!(kind, LegalKind::Code) {
                Some("article")
            } else {
                None
            },
            lexical_limit: args.top_k.saturating_mul(4),
            dense_limit: args.top_k.saturating_mul(4),
            limit: args.top_k,
        },
    )
    .map_err(storage_error_object)?;
    let response: Value = serde_json::from_str(&response)
        .map_err(|error| dependency_unavailable(error.to_string()))?;
    if response["candidates"]
        .as_array()
        .is_some_and(|candidates| candidates.is_empty())
    {
        Err(no_results("search returned no candidates"))
    } else {
        Ok(response)
    }
}

fn emit_fetch(args: FetchArgs, index_dir: Option<&Path>) -> anyhow::Result<()> {
    match fetch_payload(args, index_dir) {
        Ok(response) => write_json(&response),
        Err(error) => emit_error(error),
    }
}

fn fetch_payload(args: FetchArgs, index_dir: Option<&Path>) -> Result<Value, ErrorObject> {
    if args.as_of.is_some() || args.part.is_some() {
        return Err(ErrorObject::bad_input(
            "fetch --as-of and --part are reserved for a later fetch slice and are not applied yet",
        ));
    }
    let index_dir = require_existing_index_dir(index_dir)?;
    let postgres = open_index(index_dir.as_path())?;
    ensure_query_readiness(&postgres, QueryReadinessGate::Fetch)?;
    let ids = args.ids.iter().map(String::as_str).collect::<Vec<_>>();
    let response = fetch_documents_json(&postgres, &FetchDocumentsQuery { document_ids: &ids })
        .map_err(storage_error_object)?;
    let response: Value = serde_json::from_str(&response)
        .map_err(|error| dependency_unavailable(error.to_string()))?;
    if response["documents"]
        .as_array()
        .is_some_and(|documents| documents.is_empty())
    {
        Err(no_results(
            "fetch returned no documents for the requested IDs",
        ))
    } else {
        Ok(response)
    }
}

fn session_search_payload(args: Value) -> Result<Value, ErrorObject> {
    let args = serde_json::from_value::<SessionSearchArgs>(args)
        .map_err(|error| ErrorObject::bad_input(format!("invalid search args: {error}")))?;
    if args.query.trim().is_empty() {
        return Err(ErrorObject::bad_input("search query must not be empty"));
    }
    if args.top_k == 0 {
        return Err(ErrorObject::bad_input("search top_k must be at least 1"));
    }
    let index_dir = args.index_dir;
    search_payload(
        SearchArgs {
            query: args.query,
            kind: args.kind,
            top_k: args.top_k,
            as_of: args.as_of,
        },
        index_dir.as_deref(),
    )
}

fn session_fetch_payload(args: Value) -> Result<Value, ErrorObject> {
    let args = serde_json::from_value::<SessionFetchArgs>(args)
        .map_err(|error| ErrorObject::bad_input(format!("invalid fetch args: {error}")))?;
    if args.ids.is_empty() {
        return Err(ErrorObject::bad_input(
            "fetch requires at least one stable ID",
        ));
    }
    let index_dir = args.index_dir;
    fetch_payload(
        FetchArgs {
            ids: args.ids,
            as_of: args.as_of,
            part: args.part,
        },
        index_dir.as_deref(),
    )
}

fn session_status_payload(args: Value) -> Result<Value, ErrorObject> {
    let args = if args.is_null() {
        SessionStatusArgs::default()
    } else {
        serde_json::from_value::<SessionStatusArgs>(args)
            .map_err(|error| ErrorObject::bad_input(format!("invalid status args: {error}")))?
    };
    Ok(status_payload(args.index_dir.as_deref()))
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

fn emit_ingest(ingest: IngestCommand, index_dir: Option<&Path>) -> anyhow::Result<()> {
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
        Some(IngestSubcommand::LegiArchives {
            archives_dir,
            run_id,
            limit_members,
            max_member_bytes,
            quarantine_dir,
            safe_mode,
        }) => {
            if limit_members == Some(0) {
                return emit_error(ErrorObject::bad_input(
                    "ingest legi-archives --limit-members must be at least 1 when provided",
                ));
            }
            if max_member_bytes == 0 {
                return emit_error(ErrorObject::bad_input(
                    "ingest legi-archives --max-member-bytes must be at least 1",
                ));
            }
            match ingest_legi_archives_payload(
                index_dir,
                archives_dir.as_path(),
                run_id,
                limit_members,
                max_member_bytes,
                quarantine_dir.as_deref(),
                safe_mode,
            ) {
                Ok(response) => write_json(&response),
                Err(error) => emit_error(error),
            }
        }
        Some(IngestSubcommand::EmbedChunks { limit, index_lists }) => {
            if limit == Some(0) {
                return emit_error(ErrorObject::bad_input(
                    "ingest embed-chunks --limit must be at least 1 when provided",
                ));
            }
            if index_lists == 0 {
                return emit_error(ErrorObject::bad_input(
                    "ingest embed-chunks --index-lists must be at least 1",
                ));
            }
            match embed_chunks_payload(index_dir, limit, index_lists) {
                Ok(response) => write_json(&response),
                Err(error) => emit_error(error),
            }
        }
        Some(IngestSubcommand::BackfillLegiHierarchy) => {
            match backfill_legi_hierarchy_payload(index_dir) {
                Ok(response) => write_json(&response),
                Err(error) => emit_error(error),
            }
        }
        None => emit_error(ErrorObject::not_implemented("ingest")),
    }
}

#[derive(Debug, Default)]
struct LegiArchiveIngestCounters {
    visited_members: usize,
    inserted_documents: usize,
    inserted_chunks: usize,
    inserted_publisher_edges: usize,
    parsed_metadata_members: usize,
    persisted_metadata_members: usize,
    hierarchy_backfilled_documents: usize,
    hierarchy_backfill_invalidated_embeddings: usize,
    skipped_members: usize,
    skipped_compatible_members: usize,
    failed_members: usize,
    quarantined_payloads: usize,
    parsed_metadata_roots: BTreeMap<String, usize>,
    unsupported_roots: BTreeMap<String, usize>,
    processed_article_document_ids: BTreeSet<String>,
    processed_section_source_uids: BTreeSet<String>,
}

fn legi_archive_manifest(
    plan: &ArchivePlan,
    counters: &LegiArchiveIngestCounters,
    run_status: &str,
) -> Value {
    let latest_archive = plan.deltas.last().unwrap_or(&plan.baseline);
    json!({
        "source": "legi",
        "dataset": "LEGI",
        "run_status": run_status,
        "complete": run_status == IngestRunStatus::Completed.as_str(),
        "parser_version": LEGI_PARSER_VERSION,
        "canonical_schema_version": CANONICAL_SCHEMA_VERSION,
        "code_version": CLI_CODE_VERSION,
        "source_version": latest_archive.timestamp.to_string(),
        "freshness": {
            "latest_archive": latest_archive.file_name.as_str(),
            "latest_archive_kind": latest_archive.kind,
            "latest_archive_timestamp": latest_archive.timestamp.to_string(),
            "latest_archive_timestamp_compact": latest_archive.timestamp.compact()
        },
        "archive_plan": {
            "baseline": planned_archive_manifest(&plan.baseline),
            "deltas": plan.deltas.iter().map(planned_archive_manifest).collect::<Vec<_>>(),
            "skipped_count": plan.skipped.len(),
            "skipped": &plan.skipped
        },
        "coverage": {
            "visited_members": counters.visited_members,
            "inserted_documents": counters.inserted_documents,
            "inserted_chunks": counters.inserted_chunks,
            "inserted_publisher_edges": counters.inserted_publisher_edges,
            "parsed_metadata_members": counters.parsed_metadata_members,
            "persisted_metadata_members": counters.persisted_metadata_members,
            "hierarchy_backfill_scoped_documents": counters.processed_article_document_ids.len(),
            "hierarchy_backfill_scoped_sections": counters.processed_section_source_uids.len(),
            "hierarchy_backfilled_documents": counters.hierarchy_backfilled_documents,
            "hierarchy_backfill_invalidated_embeddings": counters.hierarchy_backfill_invalidated_embeddings,
            "skipped_members": counters.skipped_members,
            "skipped_compatible_members": counters.skipped_compatible_members,
            "failed_members": counters.failed_members,
            "quarantined_payloads": counters.quarantined_payloads,
            "parsed_metadata_roots": &counters.parsed_metadata_roots,
            "unsupported_roots": &counters.unsupported_roots
        }
    })
}

fn planned_archive_manifest(archive: &PlannedArchive) -> Value {
    json!({
        "source": archive.source,
        "kind": archive.kind,
        "timestamp": archive.timestamp.to_string(),
        "timestamp_compact": archive.timestamp.compact(),
        "file_name": archive.file_name.as_str()
    })
}

fn ingest_legi_archives_payload(
    index_dir: Option<&Path>,
    archives_dir: &Path,
    run_id: Option<String>,
    limit_members: Option<u32>,
    max_member_bytes: u64,
    quarantine_dir: Option<&Path>,
    safe_mode: bool,
) -> Result<Value, ErrorObject> {
    let index_dir = require_configured_index_dir(index_dir)?;
    let postgres = open_index(index_dir.as_path())?;
    let plan = plan_from_dir(ArchiveSource::Legi, archives_dir).map_err(|error| {
        ErrorObject::bad_input(format!("failed to plan LEGI archives: {error}"))
    })?;
    let run_id = run_id.unwrap_or_else(default_legi_run_id);
    let archive_plan_json =
        serde_json::to_string(&plan).map_err(|error| dependency_unavailable(error.to_string()))?;
    let initial_manifest = legi_archive_manifest(
        &plan,
        &LegiArchiveIngestCounters::default(),
        IngestRunStatus::Running.as_str(),
    );
    let initial_manifest_json = initial_manifest.to_string();

    start_ingest_run(
        &postgres,
        &IngestRunInput {
            run_id: run_id.as_str(),
            source: "legi",
            parser_version: LEGI_PARSER_VERSION,
            schema_version: CANONICAL_SCHEMA_VERSION,
            code_version: CLI_CODE_VERSION,
            safe_mode,
            archive_plan_json: Some(archive_plan_json.as_str()),
            manifest_json: Some(initial_manifest_json.as_str()),
        },
    )
    .map_err(storage_error_object)?;

    let mut counters = LegiArchiveIngestCounters::default();
    let mut fatal_error = None::<ErrorObject>;
    let limit_members = limit_members.map(|limit| limit as usize);
    let mut archives = vec![&plan.baseline];
    archives.extend(plan.deltas.iter());

    'archives: for archive in archives {
        let archive_name = archive.file_name.as_str();
        let read_result = for_each_xml_member_until(&archive.path, max_member_bytes, |member| {
            if limit_members.is_some_and(|limit| counters.visited_members >= limit) {
                return Ok(ArchiveVisit::Stop);
            }
            counters.visited_members += 1;
            if let Err(error) = process_legi_archive_member(
                &postgres,
                run_id.as_str(),
                archive_name,
                &member,
                quarantine_dir,
                &mut counters,
            ) {
                fatal_error = Some(storage_error_object(error));
                return Ok(ArchiveVisit::Stop);
            }
            Ok(
                if limit_members.is_some_and(|limit| counters.visited_members >= limit) {
                    ArchiveVisit::Stop
                } else {
                    ArchiveVisit::Continue
                },
            )
        });

        if let Err(error) = read_result {
            let error = ErrorObject::bad_input(format!(
                "failed to read LEGI archive `{}`: {error}",
                archive.path.display()
            ));
            fatal_error = Some(error);
        }
        if fatal_error.is_some()
            || limit_members.is_some_and(|limit| counters.visited_members >= limit)
        {
            break 'archives;
        }
    }

    if fatal_error.is_none() {
        let backfill_scope = LegiHierarchyBackfillScope {
            document_ids: counters
                .processed_article_document_ids
                .iter()
                .cloned()
                .collect(),
            section_source_uids: counters
                .processed_section_source_uids
                .iter()
                .cloned()
                .collect(),
        };
        if !backfill_scope.is_empty() {
            match backfill_legi_article_hierarchy_from_metadata_scoped(&postgres, &backfill_scope) {
                Ok(report) => {
                    counters.hierarchy_backfilled_documents = report.documents_updated;
                    counters.hierarchy_backfill_invalidated_embeddings =
                        report.embeddings_invalidated;
                }
                Err(error) => {
                    fatal_error = Some(storage_error_object(error));
                }
            }
        }
    }

    let manifest_run_status = if counters.failed_members == 0 && fatal_error.is_none() {
        IngestRunStatus::Completed
    } else {
        IngestRunStatus::Failed
    };
    let final_manifest = legi_archive_manifest(&plan, &counters, manifest_run_status.as_str());
    let final_manifest_json = final_manifest.to_string();
    if let Err(error) = update_ingest_run_manifest(&postgres, run_id.as_str(), &final_manifest_json)
    {
        fatal_error.get_or_insert_with(|| storage_error_object(error));
    }

    let run_status = if counters.failed_members == 0 && fatal_error.is_none() {
        IngestRunStatus::Completed
    } else {
        IngestRunStatus::Failed
    };
    let error_message = fatal_error.as_ref().map(|error| error.message.as_str());
    finish_ingest_run(&postgres, run_id.as_str(), run_status, error_message)
        .map_err(storage_error_object)?;
    if let Some(error) = fatal_error {
        return Err(error);
    }

    Ok(json!({
        "schema_version": SCHEMA_VERSION,
        "command": "ingest legi-archives",
        "run_id": run_id,
        "run_status": run_status.as_str(),
        "safe_mode": safe_mode,
        "index_dir": index_dir,
        "archives_dir": archives_dir,
        "archives": {
            "baseline": plan.baseline.file_name,
            "deltas": plan.deltas.iter().map(|archive| archive.file_name.as_str()).collect::<Vec<_>>(),
            "skipped": plan.skipped
        },
        "manifest": final_manifest,
        "limit_members": limit_members,
        "max_member_bytes": max_member_bytes,
        "visited_members": counters.visited_members,
        "inserted_documents": counters.inserted_documents,
        "inserted_chunks": counters.inserted_chunks,
        "inserted_publisher_edges": counters.inserted_publisher_edges,
        "parsed_metadata_members": counters.parsed_metadata_members,
        "persisted_metadata_members": counters.persisted_metadata_members,
        "hierarchy_backfill_scoped_documents": counters.processed_article_document_ids.len(),
        "hierarchy_backfill_scoped_sections": counters.processed_section_source_uids.len(),
        "hierarchy_backfilled_documents": counters.hierarchy_backfilled_documents,
        "hierarchy_backfill_invalidated_embeddings": counters.hierarchy_backfill_invalidated_embeddings,
        "skipped_members": counters.skipped_members,
        "skipped_compatible_members": counters.skipped_compatible_members,
        "failed_members": counters.failed_members,
        "quarantined_payloads": counters.quarantined_payloads,
        "parsed_metadata_roots": counters.parsed_metadata_roots,
        "unsupported_roots": counters.unsupported_roots,
        "quarantine_dir": quarantine_dir
    }))
}

fn process_legi_archive_member(
    postgres: &ManagedPostgres,
    run_id: &str,
    archive_name: &str,
    member: &ArchiveMember,
    quarantine_dir: Option<&Path>,
    counters: &mut LegiArchiveIngestCounters,
) -> Result<(), StorageError> {
    let source_payload_hash = source_payload_hash(&member.bytes);
    let compatibility = IngestCompatibility {
        parser_version: LEGI_PARSER_VERSION,
        schema_version: CANONICAL_SCHEMA_VERSION,
        code_version: CLI_CODE_VERSION,
        source_payload_hash: source_payload_hash.as_str(),
    };
    let resume = ingest_resume_decision(
        postgres,
        archive_name,
        member.member_path.as_str(),
        compatibility,
    )?;
    match resume.action {
        IngestResumeAction::Skip => {
            record_legi_member(
                postgres,
                run_id,
                LegiMemberRecordInput {
                    archive_name,
                    member_path: member.member_path.as_str(),
                    source_entity: None,
                    date_anchor: None,
                    status: IngestMemberStatus::Skipped,
                    compatibility,
                },
            )?;
            counters.skipped_members += 1;
            counters.skipped_compatible_members += 1;
            return Ok(());
        }
        IngestResumeAction::BlockedIncompatible => {
            let message = format!(
                "resume blocked by compatibility mismatch on fields [{}]",
                resume.mismatched_fields.join(", ")
            );
            let record = record_legi_member(
                postgres,
                run_id,
                LegiMemberRecordInput {
                    archive_name,
                    member_path: member.member_path.as_str(),
                    source_entity: None,
                    date_anchor: None,
                    status: IngestMemberStatus::Failed,
                    compatibility,
                },
            )?;
            record_legi_member_error(
                postgres,
                run_id,
                Some(record.member_id),
                "validation_error",
                "compatibility_mismatch",
                message.as_str(),
                "none",
                archive_name,
                member,
                quarantine_dir,
                counters,
            )?;
            counters.failed_members += 1;
            return Ok(());
        }
        IngestResumeAction::Process | IngestResumeAction::Retry => {}
    }

    match parse_legi_member(member) {
        Ok(ParsedLegiXml::Article(document)) => {
            let document = *document;
            let document_id = document.document_id.clone();
            let record = record_legi_member(
                postgres,
                run_id,
                LegiMemberRecordInput {
                    archive_name,
                    member_path: member.member_path.as_str(),
                    source_entity: Some(document.source_uid.as_str()),
                    date_anchor: Some(document.valid_from.as_str()),
                    status: IngestMemberStatus::Parsed,
                    compatibility,
                },
            )?;
            let report = insert_legi_documents(postgres, &[document], None)?;
            update_ingest_member_status(
                postgres,
                record.member_id,
                IngestMemberStatus::Inserted,
                None,
            )?;
            counters.inserted_documents += report.documents;
            counters.inserted_chunks += report.chunks;
            counters.inserted_publisher_edges += report.publisher_edges;
            counters.processed_article_document_ids.insert(document_id);
        }
        Ok(ParsedLegiXml::TextVersion(text)) => {
            process_legi_metadata_root(
                postgres,
                run_id,
                archive_name,
                member,
                compatibility,
                counters,
                "TEXTE_VERSION",
                Some(text.text_id.as_str()),
                Some(text.valid_from.as_str()),
                LegiMetadataRoot::TextVersion(text.as_ref()),
            )?;
        }
        Ok(ParsedLegiXml::SectionTa(section)) => {
            let section_source_uid = section.section_id.clone();
            process_legi_metadata_root(
                postgres,
                run_id,
                archive_name,
                member,
                compatibility,
                counters,
                "SECTION_TA",
                section.section_id.as_deref(),
                Some(section.valid_from.as_str()),
                LegiMetadataRoot::SectionTa(section.as_ref()),
            )?;
            if let Some(section_source_uid) = section_source_uid {
                counters
                    .processed_section_source_uids
                    .insert(section_source_uid);
            }
        }
        Ok(ParsedLegiXml::TextStruct(text_struct)) => {
            process_legi_metadata_root(
                postgres,
                run_id,
                archive_name,
                member,
                compatibility,
                counters,
                "TEXTELR",
                Some(text_struct.text_id.as_str()),
                text_struct.source_date_debut_hint.as_deref(),
                LegiMetadataRoot::TextStruct(text_struct.as_ref()),
            )?;
        }
        Ok(ParsedLegiXml::UnsupportedRoot { root }) => {
            *counters.unsupported_roots.entry(root.clone()).or_default() += 1;
            record_legi_member(
                postgres,
                run_id,
                LegiMemberRecordInput {
                    archive_name,
                    member_path: member.member_path.as_str(),
                    source_entity: Some(root.as_str()),
                    date_anchor: None,
                    status: IngestMemberStatus::Skipped,
                    compatibility,
                },
            )?;
            counters.skipped_members += 1;
        }
        Err(error) => {
            let (error_class, error_code) = legi_parse_error_class(&error);
            let message = error.to_string();
            let record = record_legi_member(
                postgres,
                run_id,
                LegiMemberRecordInput {
                    archive_name,
                    member_path: member.member_path.as_str(),
                    source_entity: None,
                    date_anchor: None,
                    status: IngestMemberStatus::Failed,
                    compatibility,
                },
            )?;
            record_legi_member_error(
                postgres,
                run_id,
                Some(record.member_id),
                error_class,
                error_code,
                message.as_str(),
                "none",
                archive_name,
                member,
                quarantine_dir,
                counters,
            )?;
            counters.failed_members += 1;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn process_legi_metadata_root(
    postgres: &ManagedPostgres,
    run_id: &str,
    archive_name: &str,
    member: &ArchiveMember,
    compatibility: IngestCompatibility<'_>,
    counters: &mut LegiArchiveIngestCounters,
    root: &str,
    source_uid: Option<&str>,
    date_anchor: Option<&str>,
    metadata_root: LegiMetadataRoot<'_>,
) -> Result<(), StorageError> {
    let report = insert_legi_metadata_roots(postgres, &[metadata_root])?;
    *counters
        .parsed_metadata_roots
        .entry(root.to_owned())
        .or_default() += 1;
    record_legi_member(
        postgres,
        run_id,
        LegiMemberRecordInput {
            archive_name,
            member_path: member.member_path.as_str(),
            source_entity: source_uid.or(Some(root)),
            date_anchor,
            status: IngestMemberStatus::Skipped,
            compatibility,
        },
    )?;
    counters.parsed_metadata_members += 1;
    counters.persisted_metadata_members += report.metadata_roots;
    counters.skipped_members += 1;
    Ok(())
}

struct LegiMemberRecordInput<'a> {
    archive_name: &'a str,
    member_path: &'a str,
    source_entity: Option<&'a str>,
    date_anchor: Option<&'a str>,
    status: IngestMemberStatus,
    compatibility: IngestCompatibility<'a>,
}

fn record_legi_member(
    postgres: &ManagedPostgres,
    run_id: &str,
    input: LegiMemberRecordInput<'_>,
) -> Result<jurisearch_storage::ingest_accounting::IngestMemberRecord, StorageError> {
    record_ingest_member(
        postgres,
        &IngestMemberInput {
            run_id,
            archive_name: input.archive_name,
            member_path: input.member_path,
            source: "legi",
            source_entity: input.source_entity,
            date_anchor: input.date_anchor,
            status: input.status,
            compatibility: input.compatibility,
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn record_legi_member_error(
    postgres: &ManagedPostgres,
    run_id: &str,
    member_id: Option<i64>,
    error_class: &str,
    error_code: &str,
    message: &str,
    retry_policy: &str,
    archive_name: &str,
    member: &ArchiveMember,
    quarantine_dir: Option<&Path>,
    counters: &mut LegiArchiveIngestCounters,
) -> Result<(), StorageError> {
    let quarantined = maybe_quarantine_payload(
        quarantine_dir,
        run_id,
        archive_name,
        member.member_path.as_str(),
        &member.bytes,
    )?;
    if quarantined {
        counters.quarantined_payloads += 1;
    }
    let context = json!({
        "archive_name": archive_name,
        "member_path": member.member_path,
        "quarantined": quarantined
    })
    .to_string();
    record_ingest_error(
        postgres,
        &IngestErrorInput {
            run_id,
            member_id,
            error_class,
            error_code,
            message,
            retry_policy,
            context_json: Some(context.as_str()),
        },
    )?;
    Ok(())
}

fn maybe_quarantine_payload(
    quarantine_dir: Option<&Path>,
    run_id: &str,
    archive_name: &str,
    member_path: &str,
    bytes: &[u8],
) -> Result<bool, StorageError> {
    let Some(quarantine_dir) = quarantine_dir else {
        return Ok(false);
    };
    let run_dir = quarantine_dir.join(sanitize_quarantine_component(run_id));
    fs::create_dir_all(&run_dir).map_err(StorageError::Io)?;
    let file_name = format!(
        "{}__{}",
        sanitize_quarantine_component(archive_name),
        sanitize_quarantine_component(member_path)
    );
    fs::write(run_dir.join(file_name), bytes).map_err(StorageError::Io)?;
    Ok(true)
}

fn sanitize_quarantine_component(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn legi_parse_error_class(error: &LegiParseError) -> (&'static str, &'static str) {
    match error {
        LegiParseError::Xml { .. } => ("parse_error", "parse_malformed_xml"),
        LegiParseError::MissingRequiredField { .. } => {
            ("validation_error", "validation_missing_required_field")
        }
        LegiParseError::InvalidDate { .. } => ("validation_error", "validation_invalid_date"),
        LegiParseError::InvalidId { .. } => ("validation_error", "validation_invalid_id"),
    }
}

fn backfill_legi_hierarchy_payload(index_dir: Option<&Path>) -> Result<Value, ErrorObject> {
    let index_dir = require_existing_index_dir(index_dir)?;
    let postgres = open_index(index_dir.as_path())?;
    let report =
        backfill_legi_article_hierarchy_from_metadata(&postgres).map_err(storage_error_object)?;

    Ok(json!({
        "schema_version": SCHEMA_VERSION,
        "command": "ingest backfill-legi-hierarchy",
        "index_dir": index_dir,
        "scope": "full",
        "hierarchy_backfilled_documents": report.documents_updated,
        "hierarchy_backfill_invalidated_embeddings": report.embeddings_invalidated,
    }))
}

fn default_legi_run_id() -> String {
    format!("legi-{}", unix_seconds())
}

fn embed_chunks_payload(
    index_dir: Option<&Path>,
    limit: Option<u32>,
    index_lists: u32,
) -> Result<Value, ErrorObject> {
    let index_dir = require_existing_index_dir(index_dir)?;
    let postgres = open_index(index_dir.as_path())?;
    let embedding_config = embedding_config_from_env();
    let expected_fingerprint = embedding_config.fingerprint();
    let embedding_fingerprint = embedding_config.storage_embedding_fingerprint();
    let dimension = i32::try_from(embedding_config.dimension).map_err(|_| {
        dependency_unavailable(format!(
            "embedding dimension {} is too large for dense rebuild metadata",
            embedding_config.dimension
        ))
    })?;
    if dimension != DENSE_VECTOR_DIMENSION {
        return Err(dependency_unavailable(format!(
            "embedding dimension {} does not match storage vector({})",
            embedding_config.dimension, DENSE_VECTOR_DIMENSION
        )));
    }

    let load_limit = limit.map(|value| value.saturating_add(1));
    let inputs =
        load_chunk_embedding_inputs(&postgres, load_limit).map_err(storage_error_object)?;
    if let Some(limit) = limit {
        let limit = usize::try_from(limit).unwrap_or(usize::MAX);
        if inputs.len() > limit {
            return Err(ErrorObject::bad_input(
                "ingest embed-chunks --limit would leave chunks unembedded; run on a smaller smoke index or omit --limit to finalize the full dense index",
            ));
        }
    }
    if inputs.is_empty() {
        return Err(no_results("no chunks are available to embed"));
    }

    let client =
        OpenAiCompatibleClient::new(embedding_config.clone()).map_err(embedding_error_object)?;
    let mut owned_embeddings = Vec::with_capacity(inputs.len());
    for input in &inputs {
        let embedding = client
            .embed_query(input.embedding_text.as_str(), &expected_fingerprint)
            .map_err(|error| embedding_error_object_with_context(error, &input.chunk_id))?;
        owned_embeddings.push((input.chunk_id.clone(), pgvector_literal(&embedding.values)));
    }
    let embeddings = owned_embeddings
        .iter()
        .map(|(chunk_id, embedding_literal)| ChunkEmbeddingInsert {
            chunk_id: chunk_id.as_str(),
            embedding_fingerprint: embedding_fingerprint.as_str(),
            embedding_literal: embedding_literal.as_str(),
            model: embedding_config.model.as_str(),
            dimension: embedding_config.dimension,
        })
        .collect::<Vec<_>>();
    // Embedding upserts and dense finalization are separate recoverable steps:
    // re-running the command converges before the manifest/index is advertised.
    let embeddings_inserted =
        insert_chunk_embeddings(&postgres, &embeddings).map_err(storage_error_object)?;
    let rebuild = finalize_dense_rebuild(
        &postgres,
        &DenseRebuildSpec {
            embedding_fingerprint: embedding_fingerprint.as_str(),
            model: embedding_config.model.as_str(),
            dimension,
            normalize: embedding_config.normalize,
            provisional: embedding_config.provisional,
            reembeddable: embedding_config.reembeddable,
            index_lists,
        },
    )
    .map_err(storage_error_object)?;

    Ok(json!({
        "schema_version": SCHEMA_VERSION,
        "command": "ingest embed-chunks",
        "index_dir": index_dir,
        "limit": limit,
        "chunks_considered": inputs.len(),
        "embeddings_inserted": embeddings_inserted,
        "embedding": {
            "model": embedding_config.model,
            "dimension": embedding_config.dimension,
            "normalize": embedding_config.normalize,
            "pooling": embedding_config.pooling,
            "max_input_chars": embedding_config.max_input_chars,
            "max_estimated_tokens": embedding_config.max_estimated_tokens,
            "estimated_chars_per_token": embedding_config.estimated_chars_per_token,
            "fingerprint": embedding_fingerprint,
            "provisional": embedding_config.provisional,
            "reembeddable": embedding_config.reembeddable
        },
        "dense_rebuild": {
            "chunks": rebuild.chunks,
            "embeddings": rebuild.embeddings,
            "embedding_fingerprint": rebuild.embedding_fingerprint,
            "index_name": rebuild.index_name,
            "index_lists": rebuild.index_lists
        }
    }))
}

fn require_existing_index_dir(index_dir: Option<&Path>) -> Result<PathBuf, ErrorObject> {
    let configured = configured_index_dir(index_dir);
    let Some(index_dir) = configured else {
        return Err(index_unavailable(
            "index directory is required; pass `--index-dir` or set JURISEARCH_INDEX_DIR",
        ));
    };
    if !index_dir.join("pg/data/PG_VERSION").is_file() {
        return Err(index_unavailable(format!(
            "`{}` is not an initialized jurisearch index",
            index_dir.display()
        )));
    }
    Ok(index_dir)
}

fn require_configured_index_dir(index_dir: Option<&Path>) -> Result<PathBuf, ErrorObject> {
    configured_index_dir(index_dir).ok_or_else(|| {
        index_unavailable(
            "index directory is required; pass `--index-dir` or set JURISEARCH_INDEX_DIR",
        )
    })
}

fn configured_index_dir(index_dir: Option<&Path>) -> Option<PathBuf> {
    index_dir
        .map(Path::to_path_buf)
        .or_else(|| std::env::var_os("JURISEARCH_INDEX_DIR").map(PathBuf::from))
}

fn open_index(index_dir: &Path) -> Result<ManagedPostgres, ErrorObject> {
    let pg_config = PgConfig::discover().map_err(storage_error_object)?;
    ManagedPostgres::start_durable(pg_config, index_dir).map_err(storage_error_object)
}

fn embedding_config_from_env() -> EmbeddingConfig {
    let embedding_base_url = std::env::var("JURISEARCH_EMBED_BASE_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8097/v1".into());
    let mut embedding_config = EmbeddingConfig::phase0_bge_m3(
        embedding_base_url,
        std::env::var("JURISEARCH_EMBED_API_KEY").ok(),
    );
    if let Ok(model) = std::env::var("JURISEARCH_EMBED_MODEL") {
        embedding_config.model = model;
    }
    if let Ok(dimension) = std::env::var("JURISEARCH_EMBED_DIMENSION") {
        embedding_config.dimension = dimension.parse().unwrap_or(embedding_config.dimension);
    }
    if let Ok(normalize) = std::env::var("JURISEARCH_EMBED_NORMALIZE") {
        embedding_config.normalize = normalize.parse().unwrap_or(embedding_config.normalize);
    }
    if let Ok(pooling) = std::env::var("JURISEARCH_EMBED_POOLING") {
        embedding_config.pooling = pooling;
    }
    if let Ok(max_chars) = std::env::var("JURISEARCH_EMBED_MAX_INPUT_CHARS") {
        embedding_config.max_input_chars =
            parse_optional_usize(&max_chars).unwrap_or(embedding_config.max_input_chars);
    }
    if let Ok(max_tokens) = std::env::var("JURISEARCH_EMBED_MAX_ESTIMATED_TOKENS") {
        embedding_config.max_estimated_tokens =
            parse_optional_usize(&max_tokens).unwrap_or(embedding_config.max_estimated_tokens);
    }
    if let Ok(chars_per_token) = std::env::var("JURISEARCH_EMBED_ESTIMATED_CHARS_PER_TOKEN")
        && let Ok(parsed) = chars_per_token.parse::<usize>()
        && parsed != 0
    {
        embedding_config.estimated_chars_per_token = parsed;
    }
    embedding_config
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
    let SessionRequest { id, command, args } = request;
    let command = command.trim();
    if command == "exit" {
        return (SessionResponse::ok(id, json!({ "bye": true })), true);
    }
    let result = match command {
        "help" | "help agent" => Ok(json!({ "text": agent_help() })),
        "help schema" | "schema" => Ok(compiled_schema()),
        "status" => session_status_payload(args),
        "search" => session_search_payload(args),
        "fetch" => session_fetch_payload(args),
        "cite" | "related" | "context" | "expand" | "model fetch" | "setup" | "ingest" | "sync" => {
            Err(ErrorObject::not_implemented(command))
        }
        _ => Err(ErrorObject::bad_input(format!(
            "unknown session command `{command}`"
        ))),
    };

    match result {
        Ok(result) => (SessionResponse::ok(id, result), false),
        Err(error) => (SessionResponse::err(id, error), false),
    }
}

fn status_payload(index_dir: Option<&Path>) -> Value {
    let embedding_config = embedding_config_from_env();
    let embedding_base_url = embedding_config.base_url.clone().unwrap_or_default();
    let embedding_manifest = embedding_config.manifest();
    let embedding_fingerprint = embedding_manifest.fingerprint;
    let (index, ingest_health) = status_index_and_ingest_health(index_dir);

    json!({
        "schema_version": SCHEMA_VERSION,
        "index": index,
        "embedding": {
            "provider": embedding_fingerprint.provider,
            "base_url": embedding_base_url,
            "base_url_class": embedding_fingerprint.base_url_class,
            "model": embedding_fingerprint.model,
            "dimension": embedding_fingerprint.dimension,
            "normalize": embedding_fingerprint.normalize,
            "pooling": embedding_fingerprint.pooling,
            "max_input_chars": embedding_config.max_input_chars,
            "max_estimated_tokens": embedding_config.max_estimated_tokens,
            "estimated_chars_per_token": embedding_config.estimated_chars_per_token,
            "provisional": embedding_manifest.provisional,
            "reembeddable": embedding_manifest.reembeddable
        },
        "ingest_health": ingest_health
    })
}

fn status_index_and_ingest_health(index_dir: Option<&Path>) -> (Value, Value) {
    let Some(index_dir) = configured_index_dir(index_dir) else {
        return (
            json!({
                "state": "not_configured",
                "query_ready": false,
                "message": "No index has been built yet; Phase 0 scaffold is installed."
            }),
            pending_ingest_health(),
        );
    };

    let index_path = index_dir.to_string_lossy().into_owned();
    if !index_dir.join("pg/data/PG_VERSION").is_file() {
        return (
            json!({
                "state": "not_initialized",
                "query_ready": false,
                "path": index_path,
                "message": "The configured index directory is not initialized."
            }),
            pending_ingest_health(),
        );
    }

    match open_index(&index_dir) {
        Ok(postgres) => match load_ingest_health(&postgres) {
            Ok(report) => {
                let query_ready = coverage_complete(
                    report.projection_coverage.covered,
                    report.projection_coverage.total,
                ) && coverage_complete(
                    report.embedding_coverage.covered,
                    report.embedding_coverage.total,
                );
                let message = if query_ready {
                    "Index is initialized and projection/embedding coverage gates pass."
                } else {
                    "Index is initialized but projection/embedding coverage gates are incomplete."
                };
                (
                    json!({
                        "state": "ready",
                        "query_ready": query_ready,
                        "path": index_path,
                        "message": message
                    }),
                    ingest_health_payload(report),
                )
            }
            Err(error) => {
                let error = storage_error_object(error);
                (
                    json!({
                        "state": "unavailable",
                        "query_ready": false,
                        "path": index_path,
                        "message": "Index exists but ingest health could not be loaded.",
                        "error": error
                    }),
                    pending_ingest_health(),
                )
            }
        },
        Err(error) => (
            json!({
                "state": "unavailable",
                "query_ready": false,
                "path": index_path,
                "message": "Index exists but could not be opened.",
                "error": error
            }),
            pending_ingest_health(),
        ),
    }
}

fn ingest_health_payload(report: IngestHealthReport) -> Value {
    let latest_completed_run = report.latest_completed_run_id.clone();
    match serde_json::to_value(report) {
        Ok(mut value) => {
            if let Value::Object(map) = &mut value {
                map.insert("state".to_owned(), json!("available"));
                map.insert(
                    "latest_completed_run".to_owned(),
                    json!(latest_completed_run),
                );
            }
            value
        }
        Err(error) => json!({
            "state": "unavailable",
            "latest_completed_run": null,
            "projection_coverage": null,
            "embedding_coverage": null,
            "recovery_warnings": [format!("failed to serialize ingest health: {error}")]
        }),
    }
}

fn pending_ingest_health() -> Value {
    json!({
        "state": "pending",
        "latest_completed_run": null,
        "projection_coverage": null,
        "embedding_coverage": null,
        "recovery_warnings": []
    })
}

fn coverage_complete(covered: i64, total: i64) -> bool {
    total > 0 && covered == total
}

#[derive(Debug, Clone, Copy)]
enum QueryReadinessGate {
    Fetch,
    Search,
}

impl QueryReadinessGate {
    fn command(self) -> &'static str {
        match self {
            Self::Fetch => "fetch",
            Self::Search => "search",
        }
    }
}

fn ensure_query_readiness(
    postgres: &ManagedPostgres,
    gate: QueryReadinessGate,
) -> Result<(), ErrorObject> {
    let projection_coverage =
        load_ingest_projection_coverage(postgres).map_err(storage_error_object)?;
    let projection_ready =
        coverage_complete(projection_coverage.covered, projection_coverage.total);
    if !projection_ready {
        return Err(index_not_query_ready(
            gate,
            "projection coverage gate is incomplete",
            &projection_coverage,
            None,
        ));
    }

    if matches!(gate, QueryReadinessGate::Fetch) {
        return Ok(());
    }

    let embedding_coverage =
        load_ingest_embedding_coverage(postgres).map_err(storage_error_object)?;
    let embedding_ready = coverage_complete(embedding_coverage.covered, embedding_coverage.total);
    if matches!(gate, QueryReadinessGate::Search) && !embedding_ready {
        return Err(index_not_query_ready(
            gate,
            "embedding coverage gate is incomplete",
            &projection_coverage,
            Some(&embedding_coverage),
        ));
    }

    Ok(())
}

fn index_not_query_ready(
    gate: QueryReadinessGate,
    reason: &str,
    projection_coverage: &CoverageMetric,
    embedding_coverage: Option<&CoverageMetric>,
) -> ErrorObject {
    let embedding_coverage = embedding_coverage
        .map(|metric| format!("{}/{}", metric.covered, metric.total))
        .unwrap_or_else(|| "not checked".to_owned());
    ErrorObject {
        code: ErrorCode::IndexUnavailable,
        message: format!(
            "index is not query-ready for `{}`: {reason}; projection coverage {}/{}, embedding coverage {embedding_coverage}",
            gate.command(),
            projection_coverage.covered,
            projection_coverage.total,
        ),
        suggestions: vec![
            "Run `jurisearch status` to inspect ingest health and coverage gates.".into(),
            "Run `jurisearch ingest legi-archives` and `jurisearch ingest embed-chunks` before retrieval commands.".into(),
        ],
    }
}

fn index_unavailable(message: impl Into<String>) -> ErrorObject {
    ErrorObject {
        code: ErrorCode::IndexUnavailable,
        message: message.into(),
        suggestions: vec![
            "Build or select an index before running retrieval commands.".into(),
            "Pass `--index-dir <path>` or set JURISEARCH_INDEX_DIR.".into(),
        ],
    }
}

fn dependency_unavailable(message: impl Into<String>) -> ErrorObject {
    ErrorObject {
        code: ErrorCode::DependencyUnavailable,
        message: message.into(),
        suggestions: vec![
            "Check PostgreSQL extension setup and embedding endpoint configuration.".into(),
        ],
    }
}

fn no_results(message: impl Into<String>) -> ErrorObject {
    ErrorObject {
        code: ErrorCode::NoResults,
        message: message.into(),
        suggestions: vec!["Try a different query, ID, or --as-of date.".into()],
    }
}

fn upstream_unavailable(message: impl Into<String>) -> ErrorObject {
    ErrorObject {
        code: ErrorCode::Upstream,
        message: message.into(),
        suggestions: vec!["Check the configured OpenAI-compatible embeddings endpoint.".into()],
    }
}

fn storage_error_object(error: StorageError) -> ErrorObject {
    let message = error.to_string();
    match &error {
        StorageError::StorageLockBusy { .. } | StorageError::AdvisoryLockBusy { .. } => {
            index_unavailable(message)
        }
        _ => dependency_unavailable(message),
    }
}

fn embedding_error_object(error: jurisearch_embed::EmbeddingError) -> ErrorObject {
    let message = error.to_string();
    match &error {
        jurisearch_embed::EmbeddingError::InputTooLong(_) => ErrorObject::bad_input(message),
        jurisearch_embed::EmbeddingError::Endpoint(_)
        | jurisearch_embed::EmbeddingError::InvalidResponse(_)
        | jurisearch_embed::EmbeddingError::EmptyResponse => upstream_unavailable(message),
        _ => dependency_unavailable(message),
    }
}

fn embedding_error_object_with_context(
    error: jurisearch_embed::EmbeddingError,
    chunk_id: &str,
) -> ErrorObject {
    let mut object = embedding_error_object(error);
    object.message = format!("embedding chunk `{chunk_id}` failed: {}", object.message);
    object
}

fn parade_query_text(query: &str) -> Option<String> {
    let terms = query
        .split(|character: char| !character.is_alphanumeric())
        .map(str::trim)
        .filter(|term| !term.is_empty())
        .collect::<Vec<_>>();
    if terms.is_empty() {
        None
    } else {
        Some(terms.join(" "))
    }
}

fn pgvector_literal(values: &[f32]) -> String {
    let values = values
        .iter()
        .map(|value| format!("{value:.8}"))
        .collect::<Vec<_>>()
        .join(",");
    format!("[{values}]")
}

fn parse_optional_usize(value: &str) -> Option<Option<usize>> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("none") || value == "0" {
        return Some(None);
    }
    value.parse::<usize>().ok().map(Some)
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn today_utc() -> String {
    let days_since_epoch = unix_seconds() / 86_400;
    let (year, month, day) = civil_from_days(days_since_epoch as i64);
    format!("{year:04}-{month:02}-{day:02}")
}

fn civil_from_days(days_since_epoch: i64) -> (i64, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    let year = year + if month <= 2 { 1 } else { 0 };
    (year, month as u32, day as u32)
}

fn default_cli_kind() -> CliKind {
    CliKind::All
}

fn default_top_k() -> u32 {
    10
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
