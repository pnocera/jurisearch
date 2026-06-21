use std::{
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
use jurisearch_ingest::archive::{ArchiveSource, plan_from_dir};
use jurisearch_storage::{
    dense::{
        DENSE_VECTOR_DIMENSION, DenseRebuildSpec, finalize_dense_rebuild,
        load_chunk_embedding_inputs,
    },
    ingest_accounting::{IngestHealthReport, load_ingest_health},
    projection::{ChunkEmbeddingInsert, insert_chunk_embeddings},
    retrieval::{
        FetchDocumentsQuery, HybridCandidateQuery, fetch_documents_json, hybrid_candidates_json,
    },
    runtime::{ManagedPostgres, PgConfig, StorageError},
};
use serde::Deserialize;
use serde_json::{Value, json};

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
    /// Embed stored canonical chunks and finalize the dense ANN index.
    EmbedChunks {
        /// Maximum chunk count allowed for this run; refuses larger indexes instead of finalizing partial coverage.
        #[arg(long)]
        limit: Option<u32>,
        /// Number of ivfflat lists to use when rebuilding the dense vector index.
        #[arg(long, default_value_t = 32)]
        index_lists: u32,
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
        None => emit_error(ErrorObject::not_implemented("ingest")),
    }
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

fn today_utc() -> String {
    let days_since_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        / 86_400;
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
