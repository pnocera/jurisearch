use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{self, BufRead, Write},
    net::{TcpStream, ToSocketAddrs},
    path::{Path, PathBuf},
    process::ExitCode,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::Context;
use clap::{Args, Parser, Subcommand, ValueEnum};
use jurisearch_core::{
    SCHEMA_VERSION,
    contract::{CitationState, LegalKind, OutputFormat, agent_help},
    error::{ErrorCode, ErrorObject, ProcessExit},
    eval::phase1_eval_fixture_summary,
    expand::expand_query,
    schema::compiled_schema,
    session::{SessionRequest, SessionResponse},
};
use jurisearch_embed::{EmbeddingConfig, EmbeddingProvider, OpenAiCompatibleClient};
use jurisearch_ingest::{
    archive::{
        ArchiveMember, ArchivePlan, ArchiveSource, ArchiveVisit, DEFAULT_MEMBER_BYTE_LIMIT,
        PlannedArchive, for_each_xml_member_until, plan_from_dir,
    },
    legi::{LegiParseError, ParsedLegiXml, parse_legi_member, source_payload_hash},
};
use jurisearch_official_api::{OfficialApiConfig, PisteClient};
use jurisearch_storage::{
    citation::{CitationLookup, CitationLookupQuery, citation_lookup_json},
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
        ContextDocumentsQuery, FetchDocumentsQuery, HybridCandidateQuery, RetrievalCursor,
        RetrievalMode, context_documents_json, fetch_documents_json, hybrid_candidates_json,
    },
    runtime::{ManagedPostgres, PgConfig, StorageError},
};
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};
use url::Url;

const LEGI_PARSER_VERSION: &str = "legi_article_metadata_parser:v4";
const CANONICAL_SCHEMA_VERSION: &str = "canonical_record:v3";
const CLI_CODE_VERSION: &str = concat!("jurisearch-cli:", env!("CARGO_PKG_VERSION"));
const MODEL_CACHE_REQUIRED_FILES: &[&str] = &["model.onnx", "tokenizer.json"];
const LOOPBACK_ENDPOINT_CONNECT_TIMEOUT: Duration = Duration::from_millis(250);

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
    #[arg(long, default_value = "hybrid")]
    mode: CliSearchMode,
    #[arg(long, default_value = "concise")]
    format: CliOutputFormat,
    #[arg(long, default_value_t = 10)]
    top_k: u32,
    #[arg(long)]
    cursor: Option<String>,
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
    #[serde(default = "default_search_mode")]
    mode: CliSearchMode,
    #[serde(default = "default_output_format")]
    format: CliOutputFormat,
    #[serde(default = "default_top_k")]
    top_k: u32,
    #[serde(default)]
    cursor: Option<String>,
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

#[derive(Debug, Deserialize)]
struct SessionCiteArgs {
    cite: String,
    #[serde(default)]
    strict: bool,
    #[serde(default)]
    online: bool,
    #[serde(default)]
    as_of: Option<String>,
    #[serde(default)]
    index_dir: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
struct SessionContextArgs {
    id: String,
    #[serde(default)]
    siblings: bool,
    #[serde(default)]
    as_of: Option<String>,
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
    #[arg(long)]
    as_of: Option<String>,
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

#[derive(Debug, Args, Deserialize)]
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

#[derive(Debug, Clone, Copy, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
enum CliSearchMode {
    Hybrid,
    Bm25,
    Dense,
}

impl From<CliSearchMode> for RetrievalMode {
    fn from(mode: CliSearchMode) -> Self {
        match mode {
            CliSearchMode::Hybrid => Self::Hybrid,
            CliSearchMode::Bm25 => Self::Bm25,
            CliSearchMode::Dense => Self::Dense,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
enum CliOutputFormat {
    Concise,
    Detailed,
}

impl From<CliOutputFormat> for OutputFormat {
    fn from(format: CliOutputFormat) -> Self {
        match format {
            CliOutputFormat::Concise => Self::Concise,
            CliOutputFormat::Detailed => Self::Detailed,
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
        Command::Session(args) | Command::Batch(args) => run_jsonl(args),
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
                emit_cite(args, index_dir.as_deref())
            }
        }
        Command::Related(args) => emit_error(ErrorObject::not_implemented(&format!(
            "related id={} rel={}",
            args.id,
            args.rel.as_deref().unwrap_or("any")
        ))),
        Command::Context(args) => {
            if args.id.trim().is_empty() {
                emit_error(ErrorObject::bad_input(
                    "context requires a non-empty stable ID",
                ))
            } else {
                emit_context(args, index_dir.as_deref())
            }
        }
        Command::Expand(args) => {
            if args.query.trim().is_empty() {
                emit_error(ErrorObject::bad_input("expand query must not be empty"))
            } else {
                emit_expand(args)
            }
        }
        Command::Model(args) => emit_model(args),
        Command::Setup => write_json(&setup_payload()),
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
    let retrieval_mode: RetrievalMode = args.mode.into();
    let output_format: OutputFormat = args.format.into();
    let after_cursor = args
        .cursor
        .as_deref()
        .map(parse_search_cursor)
        .transpose()?;
    let normalized_query_text = parade_query_text(&args.query);
    let query_text = if retrieval_mode.uses_lexical() {
        normalized_query_text.ok_or_else(|| {
            ErrorObject::bad_input("search query must contain at least one searchable token")
        })?
    } else if normalized_query_text.is_none() {
        return Err(ErrorObject::bad_input(
            "search query must contain at least one searchable token",
        ));
    } else {
        args.query.trim().to_owned()
    };
    let index_dir = require_existing_index_dir(index_dir)?;
    let kind: LegalKind = args.kind.into();
    if matches!(kind, LegalKind::Decision) {
        return Err(ErrorObject::bad_input(
            "Phase 0.6 search currently supports `--kind all` or `--kind code` over the LEGI subset",
        ));
    }

    let postgres = open_index(index_dir.as_path())?;
    let readiness_gate = if retrieval_mode.uses_dense() {
        QueryReadinessGate::Search
    } else {
        QueryReadinessGate::SearchLexical
    };
    ensure_query_readiness(&postgres, readiness_gate)?;
    let (query_embedding, embedding_fingerprint) = if retrieval_mode.uses_dense() {
        let embedding_config = embedding_config_from_env();
        ensure_embedding_runtime_ready(&embedding_config, false)?;
        let expected_fingerprint = embedding_config.fingerprint();
        let embedding_fingerprint = embedding_config.storage_embedding_fingerprint();
        let client =
            OpenAiCompatibleClient::new(embedding_config).map_err(embedding_error_object)?;
        let embedding = client
            .embed_query(args.query.as_str(), &expected_fingerprint)
            .map_err(embedding_error_object)?;
        (
            Some(pgvector_literal(&embedding.values)),
            Some(embedding_fingerprint),
        )
    } else {
        (None, None)
    };
    let as_of = args.as_of.unwrap_or_else(today_utc);
    let kind_filter = if matches!(kind, LegalKind::Code) {
        Some("article")
    } else {
        None
    };
    let lexical_limit = args.top_k.saturating_mul(4);
    let dense_limit = args.top_k.saturating_mul(4);
    let query_limit = args.top_k.saturating_add(1);
    let response = hybrid_candidates_json(
        &postgres,
        &HybridCandidateQuery {
            query_text: query_text.as_str(),
            query_embedding: query_embedding.as_deref(),
            embedding_fingerprint: embedding_fingerprint.as_deref(),
            retrieval_mode,
            after_cursor: after_cursor.as_ref().map(|cursor| RetrievalCursor {
                score: cursor.score.as_str(),
                chunk_id: cursor.chunk_id.as_str(),
            }),
            as_of: as_of.as_str(),
            kind_filter,
            lexical_limit,
            dense_limit,
            limit: query_limit,
        },
    )
    .map_err(storage_error_object)?;
    let mut response: Value = serde_json::from_str(&response)
        .map_err(|error| dependency_unavailable(error.to_string()))?;
    let expansion = expand_query(&args.query);
    response["format"] = json!(output_format.as_str());
    response["limit"] = json!(args.top_k);
    response["expansion_seed_version"] = json!(expansion.seed_version);
    response["expanded_terms"] = json!(expansion.expanded_terms);
    let mut next_cursor = None;
    let top_k = args.top_k as usize;
    if let Some(candidates) = response["candidates"].as_array_mut()
        && candidates.len() > top_k
    {
        candidates.truncate(top_k);
        // Storage always projects a cursor; keep next_cursor tied to the last displayed row.
        next_cursor = candidates
            .last()
            .and_then(|candidate| candidate["cursor"].as_str().map(str::to_owned));
    }
    let returned = response["candidates"].as_array().map_or(0, Vec::len);
    let has_more = next_cursor.is_some();
    response["pagination"] = json!({
        "requested_top_k": args.top_k,
        "after_cursor": args.cursor.as_deref(),
        "returned": returned,
        "possibly_truncated": has_more,
        "cursor_supported": true,
        "next_cursor": next_cursor.as_deref(),
        "cursor_note": "Use next_cursor as --cursor on the CLI or cursor in session JSON to request the next page with the same query/filter inputs. Cursor paging walks the ranked relevance pool, not an exhaustive corpus scan.",
        "guidance": if has_more {
            Some("Use next_cursor as the next cursor value, or increase top_k (or --top-k on the CLI) to inspect a wider page.")
        } else {
            None
        }
    });
    if matches!(output_format, OutputFormat::Detailed) {
        response["diagnostics"] = json!({
            "query_input": args.query,
            "lexical_query_text": if retrieval_mode.uses_lexical() {
                Some(query_text.as_str())
            } else {
                None
            },
            "retrieval": {
                "mode": retrieval_mode.as_str(),
                "uses_lexical": retrieval_mode.uses_lexical(),
                "uses_dense": retrieval_mode.uses_dense(),
                "lexical_limit": lexical_limit,
                "dense_limit": dense_limit,
                "query_limit": query_limit,
                "embedding_fingerprint": embedding_fingerprint.as_deref(),
                "kind_filter": kind_filter,
                "after_cursor": args.cursor.as_deref(),
            }
        });
    }
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

fn emit_cite(args: CiteArgs, index_dir: Option<&Path>) -> anyhow::Result<()> {
    match cite_payload(args, index_dir) {
        Ok(response) => write_json(&response),
        Err(error) => emit_error(error),
    }
}

fn emit_context(args: ContextArgs, index_dir: Option<&Path>) -> anyhow::Result<()> {
    match context_payload(args, index_dir) {
        Ok(response) => write_json(&response),
        Err(error) => emit_error(error),
    }
}

fn emit_expand(args: QueryArgs) -> anyhow::Result<()> {
    match expand_payload(args) {
        Ok(response) => write_json(&response),
        Err(error) => emit_error(error),
    }
}

fn emit_model(args: ModelCommand) -> anyhow::Result<()> {
    match args.command {
        Some(ModelSubcommand::Fetch {
            model,
            allow_download,
        }) => match model_fetch_payload(model, allow_download) {
            Ok(response) => write_json(&response),
            Err(error) => emit_error(error),
        },
        None => emit_error(ErrorObject::bad_input(
            "model requires a subcommand; supported subcommand: `fetch`",
        )),
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

fn cite_payload(args: CiteArgs, index_dir: Option<&Path>) -> Result<Value, ErrorObject> {
    validate_as_of(args.as_of.as_deref())?;
    let parsed = parse_citation_target(&args.cite);
    let effective_as_of = args.as_of.clone().unwrap_or_else(today_utc);
    let mut lookup = json!({ "matches": [] });
    if let Some(lookup_target) = parsed.lookup() {
        let index_dir = require_existing_index_dir(index_dir)?;
        let postgres = open_index(index_dir.as_path())?;
        ensure_query_readiness(&postgres, QueryReadinessGate::Fetch)?;
        let response = citation_lookup_json(
            &postgres,
            &CitationLookupQuery {
                lookup: lookup_target,
                limit: 25,
            },
        )
        .map_err(storage_error_object)?;
        lookup = serde_json::from_str(&response)
            .map_err(|error| dependency_unavailable(error.to_string()))?;
    }

    let local_state = classify_citation_state(
        &parsed,
        &lookup,
        effective_as_of.as_str(),
        args.as_of.as_deref(),
    );
    let state = if args.online
        && !matches!(&parsed, ParsedCitationTarget::Malformed { .. })
        && lookup["matches"]
            .as_array()
            .is_none_or(|matches| matches.is_empty())
    {
        CitationState::SourceUnavailable
    } else {
        local_state
    };
    let mut response = json!({
        "query": args.cite,
        "input_class": parsed.input_class(),
        "normalized": parsed.normalized_value(),
        "as_of": effective_as_of,
        "requested_as_of": args.as_of.as_deref(),
        "state": citation_state_name(state),
        "local_state": citation_state_name(local_state),
        "strict": args.strict,
        "online": {
            "requested": args.online,
            "checked": false,
            "state": null,
            "note": null
        },
        "match_count": lookup["matches"].as_array().map_or(0, Vec::len),
        "matches": lookup["matches"].clone(),
    });
    annotate_valid_matches(&mut response, &effective_as_of);
    if args.online && matches!(&parsed, ParsedCitationTarget::Malformed { .. }) {
        response["online"] = json!({
            "requested": true,
            "checked": false,
            "state": citation_state_name(CitationState::NotFound),
            "note": "Malformed citations are classified locally and are not sent to the online Légifrance probe."
        });
    } else if args.online {
        apply_online_citation_confirmation(&mut response, &args.cite)?;
    }

    if args.strict && !matches!(state, CitationState::Exact | CitationState::Normalized) {
        return Err(strict_citation_error(&args.cite, state));
    }
    Ok(response)
}

fn context_payload(args: ContextArgs, index_dir: Option<&Path>) -> Result<Value, ErrorObject> {
    validate_as_of(args.as_of.as_deref())?;
    let index_dir = require_existing_index_dir(index_dir)?;
    let postgres = open_index(index_dir.as_path())?;
    ensure_query_readiness(&postgres, QueryReadinessGate::Fetch)?;
    let response = context_documents_json(
        &postgres,
        &ContextDocumentsQuery {
            document_id: args.id.as_str(),
            as_of: args.as_of.as_deref(),
            include_siblings: args.siblings,
        },
    )
    .map_err(storage_error_object)?;
    let response: Value = serde_json::from_str(&response)
        .map_err(|error| dependency_unavailable(error.to_string()))?;
    if response["target"].is_null() {
        Err(no_results(
            "context returned no valid document for the requested ID and --as-of date",
        ))
    } else {
        Ok(response)
    }
}

fn expand_payload(args: QueryArgs) -> Result<Value, ErrorObject> {
    if args.query.trim().is_empty() {
        return Err(ErrorObject::bad_input("expand query must not be empty"));
    }
    serde_json::to_value(expand_query(&args.query))
        .map_err(|error| dependency_unavailable(error.to_string()))
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
            mode: args.mode,
            format: args.format,
            top_k: args.top_k,
            cursor: args.cursor,
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

fn session_cite_payload(args: Value) -> Result<Value, ErrorObject> {
    let args = serde_json::from_value::<SessionCiteArgs>(args)
        .map_err(|error| ErrorObject::bad_input(format!("invalid cite args: {error}")))?;
    if args.cite.trim().is_empty() {
        return Err(ErrorObject::bad_input("cite requires a non-empty citation"));
    }
    let index_dir = args.index_dir;
    cite_payload(
        CiteArgs {
            cite: args.cite,
            strict: args.strict,
            online: args.online,
            as_of: args.as_of,
        },
        index_dir.as_deref(),
    )
}

fn session_context_payload(args: Value) -> Result<Value, ErrorObject> {
    let args = serde_json::from_value::<SessionContextArgs>(args)
        .map_err(|error| ErrorObject::bad_input(format!("invalid context args: {error}")))?;
    if args.id.trim().is_empty() {
        return Err(ErrorObject::bad_input(
            "context requires a non-empty stable ID",
        ));
    }
    let index_dir = args.index_dir;
    context_payload(
        ContextArgs {
            id: args.id,
            siblings: args.siblings,
            as_of: args.as_of,
        },
        index_dir.as_deref(),
    )
}

fn session_expand_payload(args: Value) -> Result<Value, ErrorObject> {
    let args = serde_json::from_value::<QueryArgs>(args)
        .map_err(|error| ErrorObject::bad_input(format!("invalid expand args: {error}")))?;
    expand_payload(args)
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
    skipped_no_text_articles: usize,
    failed_members: usize,
    quarantined_payloads: usize,
    parsed_metadata_roots: BTreeMap<String, usize>,
    unsupported_roots: BTreeMap<String, usize>,
    processed_article_document_ids: BTreeSet<String>,
    processed_section_source_uids: BTreeSet<String>,
    processed_text_source_uids: BTreeSet<String>,
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
            "hierarchy_backfill_scoped_texts": counters.processed_text_source_uids.len(),
            "hierarchy_backfilled_documents": counters.hierarchy_backfilled_documents,
            "hierarchy_backfill_invalidated_embeddings": counters.hierarchy_backfill_invalidated_embeddings,
            "skipped_members": counters.skipped_members,
            "skipped_compatible_members": counters.skipped_compatible_members,
            "skipped_no_text_articles": counters.skipped_no_text_articles,
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
            text_source_uids: counters
                .processed_text_source_uids
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
        "hierarchy_backfill_scoped_texts": counters.processed_text_source_uids.len(),
        "hierarchy_backfilled_documents": counters.hierarchy_backfilled_documents,
        "hierarchy_backfill_invalidated_embeddings": counters.hierarchy_backfill_invalidated_embeddings,
        "skipped_members": counters.skipped_members,
        "skipped_compatible_members": counters.skipped_compatible_members,
        "skipped_no_text_articles": counters.skipped_no_text_articles,
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
            let text_source_uid = text_struct.text_id.clone();
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
            counters.processed_text_source_uids.insert(text_source_uid);
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
            if is_no_text_article_error(&error) {
                record_legi_member(
                    postgres,
                    run_id,
                    LegiMemberRecordInput {
                        archive_name,
                        member_path: member.member_path.as_str(),
                        source_entity: legi_article_id_from_member_path(
                            member.member_path.as_str(),
                        ),
                        date_anchor: None,
                        status: IngestMemberStatus::Skipped,
                        compatibility,
                    },
                )?;
                counters.skipped_members += 1;
                counters.skipped_no_text_articles += 1;
                return Ok(());
            }
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

fn is_no_text_article_error(error: &LegiParseError) -> bool {
    matches!(
        error,
        LegiParseError::MissingRequiredField { entity, field }
            if *entity == "article" && *field == "BLOC_TEXTUEL/CONTENU"
    )
}

fn legi_article_id_from_member_path(member_path: &str) -> Option<&str> {
    // Best-effort provenance for skipped ARTICLE members: official archive paths
    // end with the LEGIARTI source UID filename.
    let start = member_path.find("LEGIARTI")?;
    let end = start + "LEGIARTI".len() + 12;
    let candidate = member_path.get(start..end)?;
    let suffix = candidate.strip_prefix("LEGIARTI")?;
    if suffix.len() == 12 && suffix.chars().all(|character| character.is_ascii_digit()) {
        Some(candidate)
    } else {
        None
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
        "embedding_rebuild_required": report.embeddings_invalidated > 0,
        "recommended_next_command": if report.embeddings_invalidated > 0 {
            Some("jurisearch ingest embed-chunks")
        } else {
            None::<&str>
        },
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
    ensure_embedding_runtime_ready(&embedding_config, false)?;
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
            "token_count_method": embedding_config.configured_token_count_method(),
            "tokenizer_path": embedding_config.tokenizer_path.as_ref().map(|path| path.display().to_string()),
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

#[derive(Debug)]
struct LoadedEmbeddingConfig {
    config: EmbeddingConfig,
    config_path: Option<PathBuf>,
    config_loaded: bool,
    config_error: Option<String>,
}

#[derive(Debug)]
struct RuntimeConfigLocation {
    path: PathBuf,
    explicit: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct RuntimeConfigFile {
    embedding: Option<EmbeddingConfigFile>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddingConfigFile {
    #[serde(default, deserialize_with = "deserialize_embedding_provider_option")]
    provider: Option<EmbeddingProvider>,
    base_url: Option<String>,
    api_key: Option<String>,
    model: Option<String>,
    dimension: Option<usize>,
    normalize: Option<bool>,
    pooling: Option<String>,
    max_input_chars: Option<usize>,
    max_estimated_tokens: Option<usize>,
    estimated_chars_per_token: Option<usize>,
    tokenizer_json: Option<PathBuf>,
    tokenizer_path: Option<PathBuf>,
    provisional: Option<bool>,
    reembeddable: Option<bool>,
}

#[derive(Debug, Clone)]
struct ModelCacheStatus {
    required: bool,
    model_dir: PathBuf,
    model_cache_key: String,
    model_path: Option<PathBuf>,
    required_files: Vec<String>,
    missing_files: Vec<String>,
}

impl ModelCacheStatus {
    fn model_present(&self) -> bool {
        self.required && self.missing_files.is_empty()
    }

    fn state(&self) -> &'static str {
        if !self.required {
            "not_required"
        } else if self.model_present() {
            "ready"
        } else {
            "missing"
        }
    }
}

fn embedding_config_from_env() -> EmbeddingConfig {
    loaded_embedding_config().config
}

fn loaded_embedding_config() -> LoadedEmbeddingConfig {
    let mut embedding_config = EmbeddingConfig::phase0_bge_m3("http://127.0.0.1:8097/v1", None);
    let mut config_path = None;
    let mut config_loaded = false;
    let mut config_error = None;

    if let Some(location) = runtime_config_location() {
        match fs::read_to_string(&location.path) {
            Ok(contents) => {
                config_path = Some(location.path.clone());
                match toml::from_str::<RuntimeConfigFile>(&contents) {
                    Ok(runtime_config) => {
                        if let Some(embedding) = runtime_config.embedding {
                            apply_embedding_file_config(&mut embedding_config, embedding);
                        }
                        config_loaded = true;
                    }
                    Err(error) => {
                        config_error =
                            Some(toml_parse_error_message(&location.path, &contents, &error));
                    }
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound && !location.explicit => {
                // The default config path is optional.
            }
            Err(error) => {
                config_path = Some(location.path.clone());
                config_error = Some(format!(
                    "failed to read `{}`: {error}",
                    location.path.display()
                ));
            }
        }
    }

    apply_embedding_env_overrides(&mut embedding_config);

    LoadedEmbeddingConfig {
        config: embedding_config,
        config_path,
        config_loaded,
        config_error,
    }
}

fn runtime_config_location() -> Option<RuntimeConfigLocation> {
    if let Some(path) = std::env::var_os("JURISEARCH_CONFIG") {
        let text = path.to_string_lossy();
        let trimmed = text.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("none") || trimmed == "0" {
            return None;
        }
        return Some(RuntimeConfigLocation {
            path: PathBuf::from(trimmed),
            explicit: true,
        });
    }

    if let Some(config_home) = std::env::var_os("XDG_CONFIG_HOME")
        && !config_home.is_empty()
    {
        return Some(RuntimeConfigLocation {
            path: PathBuf::from(config_home)
                .join("jurisearch")
                .join("config.toml"),
            explicit: false,
        });
    }

    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(|home| RuntimeConfigLocation {
            path: PathBuf::from(home)
                .join(".config")
                .join("jurisearch")
                .join("config.toml"),
            explicit: false,
        })
}

fn apply_embedding_file_config(config: &mut EmbeddingConfig, file_config: EmbeddingConfigFile) {
    if let Some(provider) = file_config.provider {
        config.provider = provider;
        if matches!(provider, EmbeddingProvider::InProcess) {
            config.base_url = None;
            config.api_key = None;
        }
    }
    if let Some(base_url) = nonempty_string(file_config.base_url) {
        config.provider = EmbeddingProvider::OpenAiCompatible;
        config.base_url = Some(base_url);
    }
    if let Some(api_key) = nonempty_string(file_config.api_key) {
        config.api_key = Some(api_key);
    }
    if let Some(model) = nonempty_string(file_config.model) {
        config.model = model;
    }
    if let Some(dimension) = file_config.dimension {
        config.dimension = dimension;
    }
    if let Some(normalize) = file_config.normalize {
        config.normalize = normalize;
    }
    if let Some(pooling) = nonempty_string(file_config.pooling) {
        config.pooling = pooling;
    }
    if let Some(max_input_chars) = file_config.max_input_chars {
        config.max_input_chars = nonzero_usize(max_input_chars);
    }
    if let Some(max_estimated_tokens) = file_config.max_estimated_tokens {
        config.max_estimated_tokens = nonzero_usize(max_estimated_tokens);
    }
    if let Some(estimated_chars_per_token) = file_config.estimated_chars_per_token
        && estimated_chars_per_token != 0
    {
        config.estimated_chars_per_token = estimated_chars_per_token;
    }
    if file_config.tokenizer_json.is_some() {
        config.tokenizer_path = file_config.tokenizer_json;
    }
    if file_config.tokenizer_path.is_some() {
        config.tokenizer_path = file_config.tokenizer_path;
    }
    if let Some(provisional) = file_config.provisional {
        config.provisional = provisional;
    }
    if let Some(reembeddable) = file_config.reembeddable {
        config.reembeddable = reembeddable;
    }
    clear_unused_in_process_secret_fields(config);
}

fn apply_embedding_env_overrides(embedding_config: &mut EmbeddingConfig) {
    if let Ok(provider) = std::env::var("JURISEARCH_EMBED_PROVIDER")
        && let Some(provider) = parse_embedding_provider(&provider)
    {
        embedding_config.provider = provider;
        if matches!(provider, EmbeddingProvider::InProcess) {
            embedding_config.base_url = None;
            embedding_config.api_key = None;
        }
    }
    if let Ok(base_url) = std::env::var("JURISEARCH_EMBED_BASE_URL")
        && let Some(base_url) = nonempty_string(Some(base_url))
    {
        embedding_config.provider = EmbeddingProvider::OpenAiCompatible;
        embedding_config.base_url = Some(base_url);
    }
    if let Ok(api_key) = std::env::var("JURISEARCH_EMBED_API_KEY")
        && let Some(api_key) = nonempty_string(Some(api_key))
    {
        embedding_config.api_key = Some(api_key);
    }
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
    if let Ok(tokenizer_path) = std::env::var("JURISEARCH_EMBED_TOKENIZER_JSON") {
        embedding_config.tokenizer_path = parse_optional_path_buf(&tokenizer_path);
    }
    clear_unused_in_process_secret_fields(embedding_config);
}

fn parse_embedding_provider(value: &str) -> Option<EmbeddingProvider> {
    match value.trim().to_ascii_lowercase().as_str() {
        "openai_compatible" | "openai-compatible" | "openai" | "remote" => {
            Some(EmbeddingProvider::OpenAiCompatible)
        }
        "in_process" | "in-process" | "local" => Some(EmbeddingProvider::InProcess),
        _ => None,
    }
}

fn nonempty_string(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim().to_owned();
        if value.is_empty() { None } else { Some(value) }
    })
}

fn deserialize_embedding_provider_option<'de, D>(
    deserializer: D,
) -> Result<Option<EmbeddingProvider>, D::Error>
where
    D: Deserializer<'de>,
{
    let Some(value) = Option::<String>::deserialize(deserializer)? else {
        return Ok(None);
    };
    parse_embedding_provider(&value)
        .ok_or_else(|| {
            serde::de::Error::custom(format!("unsupported embedding provider `{value}`"))
        })
        .map(Some)
}

fn nonzero_usize(value: usize) -> Option<usize> {
    if value == 0 { None } else { Some(value) }
}

fn clear_unused_in_process_secret_fields(config: &mut EmbeddingConfig) {
    if matches!(config.provider, EmbeddingProvider::InProcess) {
        config.base_url = None;
        config.api_key = None;
    }
}

fn model_cache_status(config: &EmbeddingConfig) -> ModelCacheStatus {
    let model_dir = model_cache_dir();
    let required = matches!(config.provider, EmbeddingProvider::InProcess);
    let model_cache_key = model_cache_key(&config.model);
    let required_files = MODEL_CACHE_REQUIRED_FILES
        .iter()
        .map(|file| (*file).to_owned())
        .collect::<Vec<_>>();

    if !required {
        return ModelCacheStatus {
            required,
            model_dir,
            model_cache_key,
            model_path: None,
            required_files,
            missing_files: Vec::new(),
        };
    }

    let model_path = model_dir.join("embeddings").join(&model_cache_key);
    let missing_files = MODEL_CACHE_REQUIRED_FILES
        .iter()
        .filter(|file| !model_path.join(file).is_file())
        .map(|file| (*file).to_owned())
        .collect::<Vec<_>>();

    ModelCacheStatus {
        required,
        model_dir,
        model_cache_key,
        model_path: Some(model_path),
        required_files,
        missing_files,
    }
}

fn model_cache_status_json(status: &ModelCacheStatus) -> Value {
    json!({
        "required": status.required,
        "state": status.state(),
        "model_dir": status.model_dir.display().to_string(),
        "model_cache_key": status.model_cache_key,
        "model_path": status.model_path.as_ref().map(|path| path.display().to_string()),
        "model_present": if status.required { Some(status.model_present()) } else { None },
        "required_files": status.required_files,
        "missing_files": status.missing_files,
    })
}

fn model_cache_dir() -> PathBuf {
    if let Some(model_dir) = std::env::var_os("JURISEARCH_MODEL_DIR")
        && !model_dir.is_empty()
    {
        return PathBuf::from(model_dir);
    }

    if let Some(cache_home) = std::env::var_os("XDG_CACHE_HOME")
        && !cache_home.is_empty()
    {
        return PathBuf::from(cache_home).join("jurisearch").join("models");
    }

    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(|home| {
            PathBuf::from(home)
                .join(".cache")
                .join("jurisearch")
                .join("models")
        })
        .unwrap_or_else(|| PathBuf::from(".jurisearch").join("models"))
}

fn model_cache_key(model: &str) -> String {
    let mut key = String::with_capacity(model.len());
    for character in model.trim().chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
            key.push(character);
        } else if character == '/' || character == '\\' {
            key.push_str("__");
        } else {
            key.push('_');
        }
    }
    if key.is_empty() {
        "model".to_owned()
    } else {
        key
    }
}

fn embedding_endpoint_status_json(config: &EmbeddingConfig) -> Value {
    if !matches!(config.provider, EmbeddingProvider::OpenAiCompatible) {
        return json!({
            "checked": false,
            "state": "not_applicable",
            "reachable": Value::Null,
            "message": "in-process embedding providers do not use an HTTP endpoint"
        });
    }

    let Some(base_url) = config.base_url.as_deref() else {
        return json!({
            "checked": true,
            "state": "invalid",
            "reachable": false,
            "message": "embedding base_url is not configured"
        });
    };

    let fingerprint = config.fingerprint();
    if !matches!(
        fingerprint.base_url_class,
        jurisearch_embed::BaseUrlClass::LocalLoopback
    ) {
        return json!({
            "checked": false,
            "state": "not_checked",
            "reachable": Value::Null,
            "message": "hosted endpoints are not probed by status to avoid unsolicited external network calls"
        });
    }

    match loopback_endpoint_reachable(base_url) {
        Ok(true) => json!({
            "checked": true,
            "state": "reachable",
            "reachable": true,
            "message": "loopback embedding endpoint accepted a TCP connection"
        }),
        Ok(false) => json!({
            "checked": true,
            "state": "unreachable",
            "reachable": false,
            "message": "loopback embedding endpoint did not accept a TCP connection"
        }),
        Err(message) => json!({
            "checked": true,
            "state": "invalid",
            "reachable": false,
            "message": message
        }),
    }
}

fn loopback_endpoint_reachable(base_url: &str) -> Result<bool, String> {
    let parsed =
        Url::parse(base_url).map_err(|error| format!("invalid embedding base_url: {error}"))?;
    let Some(host) = parsed.host_str() else {
        return Err("embedding base_url has no host".to_owned());
    };
    let port = parsed.port_or_known_default().ok_or_else(|| {
        format!(
            "embedding base_url scheme `{}` has no default port",
            parsed.scheme()
        )
    })?;
    let addresses = (host, port)
        .to_socket_addrs()
        .map_err(|error| format!("failed to resolve embedding endpoint `{host}:{port}`: {error}"))?
        .collect::<Vec<_>>();
    if addresses.is_empty() {
        return Err(format!(
            "embedding endpoint `{host}:{port}` resolved no addresses"
        ));
    }
    Ok(addresses.into_iter().any(|address| {
        TcpStream::connect_timeout(&address, LOOPBACK_ENDPOINT_CONNECT_TIMEOUT).is_ok()
    }))
}

fn toml_parse_error_message(path: &Path, contents: &str, error: &toml::de::Error) -> String {
    if let Some(span) = error.span() {
        let (line, column) = line_column_for_offset(contents, span.start);
        format!(
            "failed to parse `{}`: TOML syntax error at line {line}, column {column}",
            path.display()
        )
    } else {
        format!("failed to parse `{}`: TOML syntax error", path.display())
    }
}

fn line_column_for_offset(contents: &str, byte_offset: usize) -> (usize, usize) {
    let mut line = 1;
    let mut column = 1;
    for (index, character) in contents.char_indices() {
        if index >= byte_offset {
            break;
        }
        if character == '\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }
    (line, column)
}

fn run_jsonl(args: JsonlArgs) -> anyhow::Result<()> {
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
        "cite" => session_cite_payload(args),
        "context" => session_context_payload(args),
        "expand" => session_expand_payload(args),
        "model fetch" => session_model_fetch_payload(args),
        "setup" => Ok(setup_payload()),
        "related" | "ingest" | "sync" => Err(ErrorObject::not_implemented(command)),
        _ => Err(ErrorObject::bad_input(format!(
            "unknown session command `{command}`"
        ))),
    };

    match result {
        Ok(result) => (SessionResponse::ok(id, result), false),
        Err(error) => (SessionResponse::err(id, error), false),
    }
}

#[derive(Debug, Default, Deserialize)]
struct SessionModelFetchArgs {
    model: Option<String>,
    #[serde(default)]
    allow_download: bool,
}

fn session_model_fetch_payload(args: Value) -> Result<Value, ErrorObject> {
    let args = serde_json::from_value::<SessionModelFetchArgs>(args)
        .map_err(|error| ErrorObject::bad_input(format!("invalid model fetch args: {error}")))?;
    model_fetch_payload(args.model, args.allow_download)
}

fn model_fetch_payload(model: Option<String>, allow_download: bool) -> Result<Value, ErrorObject> {
    let mut embedding_config = embedding_config_from_env();
    if let Some(model) = nonempty_string(model) {
        embedding_config.model = model;
    }
    let model_cache = model_cache_status(&embedding_config);
    let provider = embedding_config.provider;

    if !model_cache.required {
        return Ok(json!({
            "schema_version": SCHEMA_VERSION,
            "provider": provider,
            "model": embedding_config.model,
            "action": "not_required",
            "allow_download": allow_download,
            "model_cache": model_cache_status_json(&model_cache),
            "message": "the configured embedding provider does not use the in-process model cache"
        }));
    }

    if model_cache.model_present() {
        return Ok(json!({
            "schema_version": SCHEMA_VERSION,
            "provider": provider,
            "model": embedding_config.model,
            "action": "already_cached",
            "allow_download": allow_download,
            "model_cache": model_cache_status_json(&model_cache),
            "message": "in-process embedding model cache is already populated"
        }));
    }

    let missing = model_cache.missing_files.join(", ");
    if !allow_download {
        return Err(ErrorObject::bad_input(format!(
            "in-process embedding model `{}` is missing required cache files ({missing}); rerun with `--allow-download` once a download backend is packaged, or pre-stage the files under `{}`",
            embedding_config.model,
            model_cache
                .model_path
                .as_ref()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| model_cache.model_dir.display().to_string())
        )));
    }

    Err(dependency_unavailable(format!(
        "automatic download for in-process embedding model `{}` is not packaged yet; pre-stage model.onnx and tokenizer.json under `{}`",
        embedding_config.model,
        model_cache
            .model_path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| model_cache.model_dir.display().to_string())
    )))
}

fn setup_payload() -> Value {
    let loaded_embedding = loaded_embedding_config();
    let embedding_config = loaded_embedding.config;
    let model_cache = model_cache_status(&embedding_config);
    let endpoint = embedding_endpoint_status_json(&embedding_config);
    let endpoint_ready = endpoint["state"]
        .as_str()
        .is_none_or(|state| !matches!(state, "unreachable" | "invalid"));
    let model_ready = !model_cache.required || model_cache.model_present();
    let ready = loaded_embedding.config_error.is_none() && endpoint_ready && model_ready;

    json!({
        "schema_version": SCHEMA_VERSION,
        "ready": ready,
        "embedding": {
            "provider": embedding_config.provider,
            "model": embedding_config.model,
            "dimension": embedding_config.dimension,
            "config_path": loaded_embedding.config_path.as_ref().map(|path| path.display().to_string()),
            "config_loaded": loaded_embedding.config_loaded,
            "config_error": loaded_embedding.config_error,
            "model_cache": model_cache_status_json(&model_cache),
            "endpoint": endpoint
        }
    })
}

fn ensure_embedding_runtime_ready(
    embedding_config: &EmbeddingConfig,
    allow_download: bool,
) -> Result<(), ErrorObject> {
    let model_cache = model_cache_status(embedding_config);
    embedding_config
        .ensure_in_process_ready(model_cache.model_present(), allow_download)
        .map_err(embedding_error_object)
}

fn status_payload(index_dir: Option<&Path>) -> Value {
    let loaded_embedding = loaded_embedding_config();
    let embedding_config = loaded_embedding.config;
    let model_cache = model_cache_status(&embedding_config);
    let endpoint = embedding_endpoint_status_json(&embedding_config);
    let embedding_base_url = embedding_config.base_url.clone().unwrap_or_default();
    let embedding_manifest = embedding_config.manifest();
    let embedding_fingerprint = embedding_manifest.fingerprint.clone();
    let (index, ingest_health) = status_index_and_ingest_health(index_dir);
    let phase1_gate = phase1_gate_payload(&index, &ingest_health, &embedding_manifest);

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
            "token_count_method": embedding_config.configured_token_count_method(),
            "tokenizer_path": embedding_config.tokenizer_path.as_ref().map(|path| path.display().to_string()),
            "provisional": embedding_manifest.provisional,
            "reembeddable": embedding_manifest.reembeddable,
            "config_path": loaded_embedding.config_path.as_ref().map(|path| path.display().to_string()),
            "config_loaded": loaded_embedding.config_loaded,
            "config_error": loaded_embedding.config_error,
            "model_cache": model_cache_status_json(&model_cache),
            "endpoint": endpoint
        },
        "ingest_health": ingest_health,
        "phase1_gate": phase1_gate
    })
}

fn phase1_gate_payload(
    index: &Value,
    ingest_health: &Value,
    embedding_manifest: &jurisearch_embed::EmbeddingManifest,
) -> Value {
    let eval_summary = phase1_eval_fixture_summary();
    let ingest_available = ingest_health["state"] == "available";
    let query_ready = index["query_ready"].as_bool().unwrap_or(false);

    let checks = vec![
        phase1_gate_check(
            "index_query_ready",
            if query_ready { "pass" } else { "pending" },
            if query_ready {
                "index reports query_ready=true"
            } else {
                "index is not query-ready; inspect ingest health and coverage gates"
            },
        ),
        phase1_gate_check(
            "latest_completed_ingest_run",
            if ingest_available && ingest_health["latest_completed_run"].is_string() {
                "pass"
            } else {
                "pending"
            },
            "a completed official-source ingest run is required before a Phase 1 claim",
        ),
        phase1_gate_check(
            "failed_members",
            if ingest_available && ingest_health["failed_members"].as_i64() == Some(0) {
                "pass"
            } else if ingest_available {
                "fail"
            } else {
                "pending"
            },
            "failed ingest members must be zero for the Phase 1 release gate",
        ),
        phase1_gate_check(
            "projection_coverage",
            coverage_value_complete(&ingest_health["projection_coverage"]),
            "projection coverage must be complete and non-empty",
        ),
        phase1_gate_check(
            "embedding_coverage",
            coverage_value_complete(&ingest_health["embedding_coverage"]),
            "embedding coverage must be complete and non-empty for the selected fingerprint",
        ),
        phase1_gate_check(
            "replay_snapshot",
            if ingest_available && ingest_health["replay_snapshot_status"] == "available" {
                "pass"
            } else {
                "pending"
            },
            "replay snapshot signatures over canonical projections must be available",
        ),
        phase1_gate_check(
            "release_gating_eval_fixtures",
            if eval_summary.release_gating > 0 {
                "pass"
            } else {
                "pending"
            },
            "release-gating fixtures require official-source verification and named human review",
        ),
        phase1_gate_check(
            "final_embedding_model",
            if embedding_manifest.provisional {
                "pending"
            } else {
                "pass"
            },
            "final embedding model must be selected by Phase 1 legal retrieval metrics",
        ),
        phase1_gate_check(
            "reranker_decision",
            "pending",
            // TODO(phase1): wire this to stored benchmark evidence before the
            // Phase 1 statutory-search claim can open.
            "reranker adoption or deferral must be recorded from the benchmark gate",
        ),
    ];
    let claim_allowed = checks
        .iter()
        .all(|check| check["status"].as_str() == Some("pass"));

    json!({
        "state": if claim_allowed { "ready" } else { "not_ready" },
        "claim_allowed": claim_allowed,
        "scope": "phase1_legi_statutory_search",
        "checks": checks,
        "eval_fixtures": eval_summary,
    })
}

fn phase1_gate_check(name: &str, status: impl Into<Phase1GateStatus>, message: &str) -> Value {
    let status = status.into().as_str();
    json!({
        "name": name,
        "status": status,
        "message": message
    })
}

enum Phase1GateStatus {
    Static(&'static str),
    Boolean(bool),
}

impl Phase1GateStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Static(status) => status,
            Self::Boolean(true) => "pass",
            Self::Boolean(false) => "pending",
        }
    }
}

impl From<&'static str> for Phase1GateStatus {
    fn from(value: &'static str) -> Self {
        Self::Static(value)
    }
}

impl From<bool> for Phase1GateStatus {
    fn from(value: bool) -> Self {
        Self::Boolean(value)
    }
}

fn coverage_value_complete(coverage: &Value) -> bool {
    let covered = coverage["covered"].as_i64();
    let total = coverage["total"].as_i64();
    matches!((covered, total), (Some(covered), Some(total)) if total > 0 && covered == total)
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
    SearchLexical,
    Search,
}

impl QueryReadinessGate {
    fn command(self) -> &'static str {
        match self {
            Self::Fetch => "fetch",
            Self::SearchLexical => "search --mode bm25",
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

    if matches!(
        gate,
        QueryReadinessGate::Fetch | QueryReadinessGate::SearchLexical
    ) {
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

fn validate_as_of(as_of: Option<&str>) -> Result<(), ErrorObject> {
    if let Some(as_of) = as_of
        && !is_valid_iso_date(as_of)
    {
        return Err(ErrorObject::bad_input(format!(
            "--as-of must be a valid ISO date in YYYY-MM-DD format, got `{as_of}`"
        )));
    }
    Ok(())
}

#[derive(Debug)]
enum ParsedCitationTarget {
    DocumentId {
        document_id: String,
        source_uid: Option<String>,
    },
    ArticleSourceUid(String),
    TextSourceUid(String),
    SectionSourceUid(String),
    Nor(String),
    FreeTextArticle {
        article_number: String,
        code_hint: Option<String>,
    },
    Malformed {
        normalized: String,
    },
}

impl ParsedCitationTarget {
    fn lookup(&self) -> Option<CitationLookup<'_>> {
        match self {
            Self::DocumentId {
                document_id,
                source_uid,
            } => Some(CitationLookup::DocumentId {
                document_id,
                source_uid: source_uid.as_deref(),
            }),
            Self::ArticleSourceUid(source_uid) => {
                Some(CitationLookup::ArticleSourceUid(source_uid))
            }
            Self::TextSourceUid(source_uid) => Some(CitationLookup::TextSourceUid(source_uid)),
            Self::SectionSourceUid(source_uid) => {
                Some(CitationLookup::SectionSourceUid(source_uid))
            }
            Self::Nor(nor) => Some(CitationLookup::Nor(nor)),
            Self::FreeTextArticle {
                article_number,
                code_hint,
            } => Some(CitationLookup::FreeTextArticle {
                article_number,
                code_hint: code_hint.as_deref(),
            }),
            Self::Malformed { .. } => None,
        }
    }

    fn input_class(&self) -> &'static str {
        match self {
            Self::DocumentId { .. } => "document_id",
            Self::ArticleSourceUid(_) => "legiarti",
            Self::TextSourceUid(_) => "legitext",
            Self::SectionSourceUid(_) => "legiscta",
            Self::Nor(_) => "nor",
            Self::FreeTextArticle { .. } => "free_text_article",
            Self::Malformed { .. } => "malformed",
        }
    }

    fn normalized_value(&self) -> Option<&str> {
        match self {
            Self::DocumentId { document_id, .. } => Some(document_id),
            Self::ArticleSourceUid(source_uid)
            | Self::TextSourceUid(source_uid)
            | Self::SectionSourceUid(source_uid)
            | Self::Nor(source_uid) => Some(source_uid),
            Self::FreeTextArticle { article_number, .. } => Some(article_number),
            Self::Malformed { normalized } if !normalized.is_empty() => Some(normalized),
            Self::Malformed { .. } => None,
        }
        .map(String::as_str)
    }
}

fn parse_citation_target(input: &str) -> ParsedCitationTarget {
    let trimmed = input.trim();
    if trimmed.starts_with("legi:") {
        return ParsedCitationTarget::DocumentId {
            document_id: trimmed.to_owned(),
            source_uid: extract_known_source_uid(trimmed, "LEGIARTI"),
        };
    }
    if let Some(source_uid) = extract_known_source_uid(trimmed, "LEGIARTI") {
        return ParsedCitationTarget::ArticleSourceUid(source_uid);
    }
    if let Some(source_uid) = extract_known_source_uid(trimmed, "LEGITEXT") {
        return ParsedCitationTarget::TextSourceUid(source_uid);
    }
    if let Some(source_uid) = extract_known_source_uid(trimmed, "LEGISCTA") {
        return ParsedCitationTarget::SectionSourceUid(source_uid);
    }
    let normalized = normalize_citation_text(trimmed);
    if let Some(article_number) = parse_article_number(&normalized) {
        return ParsedCitationTarget::FreeTextArticle {
            article_number,
            code_hint: detect_code_hint(&normalized),
        };
    }
    let compact_upper = trimmed
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(|character| character.to_uppercase())
        .collect::<String>();
    if looks_like_nor(&compact_upper) {
        return ParsedCitationTarget::Nor(compact_upper);
    }
    ParsedCitationTarget::Malformed { normalized }
}

fn extract_known_source_uid(value: &str, prefix: &str) -> Option<String> {
    let upper = value.to_ascii_uppercase();
    let start = upper.find(prefix)?;
    let suffix = upper[start + prefix.len()..]
        .chars()
        .take_while(|character| character.is_ascii_digit())
        .take(12)
        .collect::<String>();
    (suffix.len() == 12).then(|| format!("{prefix}{suffix}"))
}

fn parse_article_number(normalized: &str) -> Option<String> {
    let tokens = normalized.split_whitespace().collect::<Vec<_>>();
    let mut index = 0usize;
    const ARTICLE_PREFIXES: &[&str] = &["l", "lo", "r", "d"];
    while let Some(token) = tokens.get(index) {
        if *token == "article"
            && let Some(candidate) = tokens.get(index + 1)
        {
            if let Some(number) = article_number_token(candidate) {
                return Some(number);
            }
            if ARTICLE_PREFIXES.contains(candidate)
                && let Some(number) = tokens
                    .get(index + 2)
                    .and_then(|candidate| article_number_token(candidate))
            {
                return Some(format!("{candidate}{number}"));
            }
        }
        index += 1;
    }
    None
}

fn article_number_token(candidate: &str) -> Option<String> {
    (candidate
        .chars()
        .any(|character| character.is_ascii_digit())
        && candidate
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-'))
    .then(|| candidate.to_owned())
}

fn detect_code_hint(normalized: &str) -> Option<String> {
    const CODE_HINTS: &[&str] = &[
        "code civil",
        "code penal",
        "code de procedure civile",
        "code de procedure penale",
        "code du travail",
        "code de la consommation",
        "code des assurances",
        "code de commerce",
        "code de l environnement",
        "code de la sante publique",
        "code general des impots",
    ];
    CODE_HINTS
        .iter()
        .find(|hint| contains_normalized_phrase(normalized, hint))
        .map(|hint| (*hint).to_owned())
}

fn contains_normalized_phrase(normalized: &str, phrase: &str) -> bool {
    let normalized = format!(" {normalized} ");
    let phrase = format!(" {phrase} ");
    normalized.contains(&phrase)
}

fn looks_like_nor(value: &str) -> bool {
    let chars = value.chars().collect::<Vec<_>>();
    chars.len() == 12
        && chars[0..4]
            .iter()
            .all(|character| character.is_ascii_alphabetic())
        && chars[4..11]
            .iter()
            .all(|character| character.is_ascii_digit())
        && chars[11].is_ascii_alphabetic()
}

fn normalize_citation_text(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len());
    let mut previous_was_space = true;
    for character in value.chars().flat_map(|character| character.to_lowercase()) {
        let replacement = match character {
            'à' | 'â' | 'ä' => "a",
            'ç' => "c",
            'é' | 'è' | 'ê' | 'ë' => "e",
            'î' | 'ï' => "i",
            'ô' | 'ö' => "o",
            'ù' | 'û' | 'ü' => "u",
            'œ' => "oe",
            'æ' => "ae",
            '-' => {
                normalized.push('-');
                previous_was_space = false;
                continue;
            }
            ascii if ascii.is_ascii_alphanumeric() => {
                normalized.push(ascii);
                previous_was_space = false;
                continue;
            }
            _ => "",
        };
        if !replacement.is_empty() {
            normalized.push_str(replacement);
            previous_was_space = false;
        } else if !previous_was_space {
            normalized.push(' ');
            previous_was_space = true;
        }
    }
    normalized.trim().to_owned()
}

fn classify_citation_state(
    parsed: &ParsedCitationTarget,
    lookup: &Value,
    effective_as_of: &str,
    requested_as_of: Option<&str>,
) -> CitationState {
    if matches!(parsed, ParsedCitationTarget::Malformed { .. }) {
        return CitationState::NotFound;
    }
    let Some(matches) = lookup["matches"].as_array() else {
        return CitationState::NotFound;
    };
    if matches.is_empty() {
        return CitationState::NotFound;
    }
    let valid_match_count = matches
        .iter()
        .filter(|candidate| candidate_valid_on(candidate, effective_as_of))
        .count();
    match parsed {
        ParsedCitationTarget::DocumentId { .. } => {
            let exact_valid = matches.iter().any(|candidate| {
                candidate["exact_identifier_match"].as_bool() == Some(true)
                    && (requested_as_of.is_none()
                        || candidate_valid_on(candidate, requested_as_of.unwrap_or_default()))
            });
            if exact_valid {
                CitationState::Exact
            } else {
                CitationState::StaleVersion
            }
        }
        ParsedCitationTarget::FreeTextArticle { .. } => match valid_match_count {
            0 => CitationState::StaleVersion,
            1 => CitationState::Normalized,
            _ => CitationState::Ambiguous,
        },
        ParsedCitationTarget::ArticleSourceUid(_)
        | ParsedCitationTarget::TextSourceUid(_)
        | ParsedCitationTarget::SectionSourceUid(_)
        | ParsedCitationTarget::Nor(_) => match valid_match_count {
            0 => CitationState::StaleVersion,
            1 => CitationState::Exact,
            _ => CitationState::Ambiguous,
        },
        ParsedCitationTarget::Malformed { .. } => CitationState::NotFound,
    }
}

fn annotate_valid_matches(response: &mut Value, effective_as_of: &str) {
    let mut valid_count = 0usize;
    if let Some(matches) = response["matches"].as_array_mut() {
        for candidate in matches {
            let valid = candidate_valid_on(candidate, effective_as_of);
            candidate["valid_on_as_of"] = json!(valid);
            if valid {
                valid_count += 1;
            }
        }
    }
    response["valid_match_count"] = json!(valid_count);
}

fn candidate_valid_on(candidate: &Value, as_of: &str) -> bool {
    let validity = &candidate["validity"];
    let valid_from_ok = validity["from"]
        .as_str()
        .is_none_or(|valid_from| valid_from <= as_of);
    let valid_to_ok = validity["to"]
        .as_str()
        .is_none_or(|valid_to| as_of < valid_to);
    valid_from_ok && valid_to_ok
}

fn citation_state_name(state: CitationState) -> &'static str {
    match state {
        CitationState::Exact => "exact",
        CitationState::Normalized => "normalized",
        CitationState::Ambiguous => "ambiguous",
        CitationState::StaleVersion => "stale_version",
        CitationState::NotFound => "not_found",
        CitationState::SourceUnavailable => "source_unavailable",
    }
}

fn strict_citation_error(input: &str, state: CitationState) -> ErrorObject {
    ErrorObject {
        code: ErrorCode::NoResults,
        message: format!(
            "strict citation verification failed for `{input}` with state `{}`",
            citation_state_name(state)
        ),
        suggestions: vec![
            "Retry without --strict to inspect candidate matches and citation state.".into(),
            "Pass --as-of for historical statutory versions.".into(),
        ],
    }
}

fn apply_online_citation_confirmation(
    response: &mut Value,
    query: &str,
) -> Result<(), ErrorObject> {
    let mut client = PisteClient::new(OfficialApiConfig::from_env());
    let upstream = client
        .legifrance_search(&json!({
            "query": query,
            "pageSize": 1,
        }))
        .map_err(|error| error.to_error_object())?;
    response["online"] = json!({
        "requested": true,
        "checked": true,
        "provider": "legifrance",
        "state": response["state"].as_str(),
        "response_summary": summarize_online_response(&upstream),
        "note": "Online Légifrance search completed; citation state remains based on local index resolution until response-shape matching is specified."
    });
    Ok(())
}

fn summarize_online_response(response: &Value) -> Value {
    let top_level_keys = response
        .as_object()
        .map(|object| object.keys().take(8).cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    let result_count = response
        .get("results")
        .and_then(Value::as_array)
        .map(Vec::len)
        .or_else(|| {
            response
                .get("items")
                .and_then(Value::as_array)
                .map(Vec::len)
        });
    json!({
        "top_level_keys": top_level_keys,
        "result_count": result_count,
    })
}

#[derive(Debug)]
struct ParsedSearchCursor {
    score: String,
    chunk_id: String,
}

fn parse_search_cursor(cursor: &str) -> Result<ParsedSearchCursor, ErrorObject> {
    let (score, chunk_id) = cursor.split_once(':').ok_or_else(|| {
        ErrorObject::bad_input(
            "search --cursor must use the cursor value returned by a previous search candidate",
        )
    })?;
    let parsed_score = score.parse::<f64>().map_err(|_| {
        ErrorObject::bad_input(
            "search --cursor must start with a numeric score followed by ':' and a chunk id",
        )
    })?;
    if !parsed_score.is_finite() || parsed_score < 0.0 || chunk_id.trim().is_empty() {
        return Err(ErrorObject::bad_input(
            "search --cursor must start with a finite non-negative score followed by ':' and a chunk id",
        ));
    }
    Ok(ParsedSearchCursor {
        score: score.to_owned(),
        chunk_id: chunk_id.to_owned(),
    })
}

fn is_valid_iso_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    let valid_shape = bytes.len() == 10
        && bytes[0..4].iter().all(u8::is_ascii_digit)
        && bytes[4] == b'-'
        && bytes[5..7].iter().all(u8::is_ascii_digit)
        && bytes[7] == b'-'
        && bytes[8..10].iter().all(u8::is_ascii_digit);
    if !valid_shape {
        return false;
    }

    let year = value[0..4].parse::<u16>().unwrap_or_default();
    let month = value[5..7].parse::<u8>().unwrap_or_default();
    let day = value[8..10].parse::<u8>().unwrap_or_default();
    day > 0 && day <= days_in_month(year, month).unwrap_or_default()
}

fn days_in_month(year: u16, month: u8) -> Option<u8> {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => Some(31),
        4 | 6 | 9 | 11 => Some(30),
        2 if is_leap_year(year) => Some(29),
        2 => Some(28),
        _ => None,
    }
}

fn is_leap_year(year: u16) -> bool {
    (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400)
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

fn parse_optional_path_buf(value: &str) -> Option<PathBuf> {
    let value = value.trim();
    if value.is_empty() || value.eq_ignore_ascii_case("none") || value == "0" {
        None
    } else {
        Some(PathBuf::from(value))
    }
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

fn default_search_mode() -> CliSearchMode {
    CliSearchMode::Hybrid
}

fn default_output_format() -> CliOutputFormat {
    CliOutputFormat::Concise
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_embedding_manifest(provisional: bool) -> jurisearch_embed::EmbeddingManifest {
        jurisearch_embed::EmbeddingManifest {
            fingerprint: jurisearch_embed::EmbeddingFingerprint {
                provider: jurisearch_embed::EmbeddingProvider::OpenAiCompatible,
                base_url_class: jurisearch_embed::BaseUrlClass::LocalLoopback,
                model: "bge-m3".to_string(),
                dimension: 1024,
                normalize: true,
                pooling: "cls".to_string(),
            },
            provisional,
            reembeddable: true,
        }
    }

    fn check_status<'a>(payload: &'a Value, name: &str) -> &'a str {
        payload["checks"]
            .as_array()
            .and_then(|checks| checks.iter().find(|check| check["name"] == name))
            .and_then(|check| check["status"].as_str())
            .expect("phase1 gate check status exists")
    }

    #[test]
    fn phase1_gate_payload_maps_ready_inputs_and_failed_members() {
        let index = json!({ "query_ready": true });
        let ingest_health = json!({
            "state": "available",
            "latest_completed_run": "2026-06-21T20:00:00Z",
            "failed_members": 0,
            "projection_coverage": { "covered": 2, "total": 2 },
            "embedding_coverage": { "covered": 2, "total": 2 },
            "replay_snapshot_status": "available"
        });
        let manifest = test_embedding_manifest(false);

        let payload = phase1_gate_payload(&index, &ingest_health, &manifest);

        assert_eq!(check_status(&payload, "index_query_ready"), "pass");
        assert_eq!(
            check_status(&payload, "latest_completed_ingest_run"),
            "pass"
        );
        assert_eq!(check_status(&payload, "failed_members"), "pass");
        assert_eq!(check_status(&payload, "projection_coverage"), "pass");
        assert_eq!(check_status(&payload, "embedding_coverage"), "pass");
        assert_eq!(check_status(&payload, "replay_snapshot"), "pass");
        assert_eq!(check_status(&payload, "final_embedding_model"), "pass");
        assert_eq!(
            check_status(&payload, "release_gating_eval_fixtures"),
            "pending"
        );
        assert_eq!(check_status(&payload, "reranker_decision"), "pending");
        assert_eq!(payload["state"], "not_ready");
        assert_eq!(payload["claim_allowed"], false);

        let mut failed_ingest_health = ingest_health.clone();
        failed_ingest_health["failed_members"] = json!(2);
        let failed_payload = phase1_gate_payload(&index, &failed_ingest_health, &manifest);

        assert_eq!(check_status(&failed_payload, "failed_members"), "fail");
        assert_eq!(failed_payload["state"], "not_ready");
        assert_eq!(failed_payload["claim_allowed"], false);
    }
}
