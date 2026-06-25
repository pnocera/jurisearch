//! CLI argument parsing surface: the clap `Parser`/`Subcommand`/`Args` types, the
//! value enums and their conversions into domain types, the shared serde/clap
//! `default_*` helpers, and the boundary validation for retrieval tuning.
//!
//! This module owns only argument *definitions*; command behaviour lives in the
//! dispatch/command modules. Items are `pub(crate)` so the rest of the binary can
//! consume them.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};
use serde::Deserialize;

use jurisearch_core::contract::{LegalKind, OutputFormat};
use jurisearch_ingest::archive::{ArchiveSource, DEFAULT_MEMBER_BYTE_LIMIT};
use jurisearch_storage::retrieval::{GroupBy, RetrievalMode};
use jurisearch_storage::zone_units::EnrichZoneOrder;

use crate::{
    EMBED_CHUNKS_DEFAULT_BATCH_SIZE, EMBED_CHUNKS_DEFAULT_POOL_CONCURRENCY,
    ENRICH_ZONES_DEFAULT_CONCURRENCY,
};

#[derive(Debug, Parser)]
#[command(name = "jurisearch")]
#[command(version, about = "Local-first French legal search CLI for AI agents.")]
#[command(disable_help_subcommand = true)]
pub(crate) struct Cli {
    /// Path to the index directory (overrides $JURISEARCH_INDEX_DIR). Use an ABSOLUTE path.
    #[arg(long, env = "JURISEARCH_INDEX_DIR", global = true)]
    pub(crate) index_dir: Option<PathBuf>,
    #[command(subcommand)]
    pub(crate) command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Return compact ranked candidates for a legal research query.
    ///
    /// Modes: `hybrid` (default) fuses BM25 + dense and auto-routes citation-shaped queries
    /// (e.g. "Article 1240") through the structured citation resolver; `bm25` is lexical-only;
    /// `dense` is vector-only and is strongest for conceptual/paraphrased queries. Output schema:
    /// `SearchResponse` (see `help schema --json`). Example:
    ///   jurisearch search "résiliation d'un bail d'habitation" --mode dense --top-k 10 --as-of 2026-06-23
    Search(SearchArgs),
    /// Compare bm25/dense/hybrid retrievers for one query (document grouping).
    ///
    /// Returns aligned per-mode top-k, the pooled union with per-mode ranks, and pairwise overlap.
    /// Single-page (no cursor). Output: `CompareResponse`. Example:
    ///   jurisearch compare "résiliation d'un bail" --top-k 10 --as-of 2026-06-23
    Compare(CompareArgs),
    /// Return full source text for selected exact, version-pinned stable IDs.
    ///
    /// IDs are version-pinned (e.g. `legi:LEGIARTI000006850948@1994-08-21`). Output: `FetchResponse`.
    /// Example:  jurisearch fetch legi:LEGIARTI000006850948@1994-08-21
    Fetch(FetchArgs),
    /// Verify a citation or identifier and report its citation state.
    ///
    /// Output: `CiteResponse` (with `state` exact/normalized/ambiguous/stale_version/not_found).
    /// Example:  jurisearch cite "article 1240 du code civil" --as-of 2026-06-23
    Cite(CiteArgs),
    /// Return depth-1 graph neighbours of a document with authority signals.
    ///
    /// `--rel cites` (outgoing citations, default), `cited_by` (incoming), `temporal` (version
    /// family), or `interpreted_by` (decisions that cite a statute article). Output:
    /// `RelatedResponse`. Example:
    ///   jurisearch related legi:LEGIARTI000006850948@1994-08-21 --rel cites
    Related(RelatedArgs),
    /// Return structural neighbourhood (ancestry, siblings) for a document.
    ///
    /// Output: `ContextResponse`. Example:  jurisearch context legi:LEGIARTI000006419298@2002-01-01 --siblings
    Context(ContextArgs),
    /// Return curated legal-vocabulary expansions for a query.
    ///
    /// Output: `ExpandResponse`. Example:  jurisearch expand "bail commercial"
    Expand(QueryArgs),
    /// Report corpus coverage, model fingerprints, and index health.
    ///
    /// Output: `StatusResponse` (includes the Phase-1 release gate). Example:  jurisearch status --deep
    Status(StatusArgs),
    /// Explicit model-cache operations (subcommand: `fetch`).
    ///
    /// Models are never downloaded implicitly during search. Example:  jurisearch model fetch --allow-download
    Model(ModelCommand),
    /// Check or prepare local configuration and optional model caches.
    ///
    /// Output: `SetupResponse`. Example:  jurisearch setup
    Setup,
    /// Run a non-owning dependency preflight (embedding, models, PG runtime, extensions, index dir).
    ///
    /// Does NOT start the index Postgres. Output: `DoctorResponse`. Example:  jurisearch doctor
    Doctor,
    /// Report corpus/graph/embedding counts (replaces ad-hoc psql for introspection).
    ///
    /// Output: `StatsResponse`. Example:  jurisearch stats
    Stats,
    /// Return the raw canonical record for one document id (full row, chunk count, edge count).
    ///
    /// Output: `InspectResponse`. Example:  jurisearch inspect legi:LEGIARTI000006850948@1994-08-21
    Inspect(InspectArgs),
    /// List an article's version timeline (every member of its version family, by validity start).
    ///
    /// Output: `VersionsResponse`. Example:  jurisearch versions legi:LEGIARTI000006419298@2002-01-01
    Versions(VersionsArgs),
    /// Compare the article versions in force on two dates (which version, and whether it changed).
    ///
    /// Output: `DiffResponse`. Example:  jurisearch diff legi:LEGIARTI...@2002-01-01 --from 2002-01-01 --to 2010-01-01
    Diff(DiffArgs),
    /// Warm JSONL subprocess protocol for order-preserving agent workflows.
    ///
    /// Reads one JSON request per line on stdin, writes one JSON response per line. Example:
    ///   echo '{"id":"1","command":"search","args":{"query":"article 1240"}}' | jurisearch session --jsonl
    Session(JsonlArgs),
    /// Finite JSONL protocol for eval and bulk verification runs.
    ///
    /// Like `session` but terminates at end-of-input. Example:
    ///   jurisearch batch --jsonl < requests.jsonl
    Batch(JsonlArgs),
    /// Serve the JSONL request protocol over a TCP or Unix socket (single-client, sequential).
    ///
    /// Exposes the SAME handlers as the warm session, so a thin client gets byte-identical results
    /// to the one-shot CLI; capability discovery via `{"command":"help schema"}`. The bound
    /// `--index-dir` is injected into requests that omit it. Example:
    ///   jurisearch serve --socket /tmp/jurisearch.sock --index-dir /abs/index
    Serve(ServeArgs),
    /// Official-source ingestion helpers (subcommands: plan-archives, legi-archives, embed-chunks, ...).
    ///
    /// Builds the canonical index from official archives. Example:
    ///   jurisearch ingest legi-archives --archives-dir ./archives
    Ingest(IngestCommand),
    /// Run built-in retrieval evaluation fixtures (subcommands: phase1, france-legi).
    ///
    /// Example:  jurisearch eval phase1 --list
    Eval(EvalCommand),
    /// Synchronize official sources through deltas or transactional histories (STUB).
    ///
    /// Example:  jurisearch sync --source legi
    Sync(SyncArgs),
    /// Compiled agent help and schemas (subcommands: agent, schema).
    ///
    /// Example:  jurisearch help schema --json
    Help(HelpCommand),
}

#[derive(Debug, Args)]
pub(crate) struct SearchArgs {
    /// Free-text research query (a topic, paraphrase, or citation like "Article 1240").
    pub(crate) query: String,
    /// Corpus filter: `code` (statutes), `decision` (case law), or `all`.
    #[arg(long, default_value = "all")]
    pub(crate) kind: CliKind,
    /// Retrieval mode: `hybrid` (BM25+dense, auto-routes citations), `bm25` (lexical), `dense` (vector).
    #[arg(long, default_value = "hybrid")]
    pub(crate) mode: CliSearchMode,
    /// Output verbosity: `concise` (ranked candidates) or `detailed`.
    #[arg(long, default_value = "concise")]
    pub(crate) format: CliOutputFormat,
    /// Result granularity: `chunk` (default, one row per passage) or `document` (one row per article).
    #[arg(long, default_value = "chunk")]
    pub(crate) group_by: CliGroupBy,
    /// Maximum number of candidates to return.
    #[arg(long, default_value_t = 10)]
    pub(crate) top_k: u32,
    /// Opaque pagination cursor from a previous response's `pagination.next_cursor`.
    #[arg(long)]
    pub(crate) cursor: Option<String>,
    /// Pin temporal validity to this date (`YYYY-MM-DD`); only versions in force on that date match.
    #[arg(long)]
    pub(crate) as_of: Option<String>,
    /// Override the hybrid RRF lexical weight (per-request; default from env, else 1.0).
    #[arg(long)]
    pub(crate) rrf_lexical_weight: Option<f64>,
    /// Override the hybrid RRF dense weight (per-request; default from env, else 0.3).
    #[arg(long)]
    pub(crate) rrf_dense_weight: Option<f64>,
    /// Override ivfflat.probes for dense ANN (per-request; default 4; higher = more recall, slower).
    #[arg(long)]
    pub(crate) probes: Option<u32>,
    /// Decision filter: court / jurisdiction substring (e.g. "Cour de cassation", "CAA de PARIS").
    #[arg(long)]
    pub(crate) court: Option<String>,
    /// Decision filter: chamber / formation substring (e.g. "chambre sociale").
    #[arg(long)]
    pub(crate) formation: Option<String>,
    /// Decision filter: publication level (e.g. "oui" for published, "C" for recueil class).
    #[arg(long)]
    pub(crate) publication: Option<String>,
    /// Decision filter: earliest decision date (inclusive, `YYYY-MM-DD`).
    #[arg(long)]
    pub(crate) decided_from: Option<String>,
    /// Decision filter: latest decision date (inclusive, `YYYY-MM-DD`).
    #[arg(long)]
    pub(crate) decided_to: Option<String>,
    /// Official-zone scope (Cour de cassation ONLY): restrict retrieval to a decision part —
    /// `motivations` (the court's reasoning), `moyens` (grounds raised), or `dispositif` (holding).
    /// Coverage-bounded: searches only resolver-reachable cass/inca decisions with official Judilibre
    /// zones, ranked within that zone. Cannot be combined with `--kind code`.
    #[arg(long)]
    pub(crate) zone: Option<CliZone>,
    /// Decision-only authority re-rank weight in `[0.0, 1.0]` (default off; `0.0` is treated as off).
    /// Re-orders near-tied jurisprudence results within the same legal order by publication authority.
    /// EXPERIMENTAL: first-page-only — a positive weight disables cursor paging for the response and
    /// requires `--kind decision` (or `--zone`); it cannot be combined with an inbound `--cursor`.
    #[arg(long)]
    pub(crate) authority_weight: Option<f64>,
}

/// Official Judilibre zone scopes available to `search --zone` (Cour de cassation only).
#[derive(Debug, Clone, Copy, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CliZone {
    Motivations,
    Moyens,
    Dispositif,
}

impl CliZone {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Motivations => "motivations",
            Self::Moyens => "moyens",
            Self::Dispositif => "dispositif",
        }
    }
}

/// Direction `ingest enrich-zones` walks the candidate set. Official Judilibre zones exist only for
/// recent decisions, so `recent` reaches them first; `oldest` keeps the original keyset order.
#[derive(Debug, Clone, Copy, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CliEnrichZoneOrder {
    Oldest,
    Recent,
}

impl CliEnrichZoneOrder {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Oldest => "oldest",
            Self::Recent => "recent",
        }
    }
}

impl From<CliEnrichZoneOrder> for EnrichZoneOrder {
    fn from(order: CliEnrichZoneOrder) -> Self {
        match order {
            CliEnrichZoneOrder::Oldest => EnrichZoneOrder::Oldest,
            CliEnrichZoneOrder::Recent => EnrichZoneOrder::Recent,
        }
    }
}

#[derive(Debug, Args)]
pub(crate) struct CompareArgs {
    /// Query to compare across retrievers (bm25 vs dense vs hybrid).
    pub(crate) query: String,
    /// Corpus filter: `code` (default) or `all`.
    #[arg(long, default_value = "code")]
    pub(crate) kind: CliKind,
    /// Number of top documents to compare per retriever.
    #[arg(long, default_value_t = 10)]
    pub(crate) top_k: u32,
    /// Pin temporal validity to this date (`YYYY-MM-DD`).
    #[arg(long)]
    pub(crate) as_of: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct FetchArgs {
    /// One or more exact, version-pinned stable IDs (e.g. `legi:LEGIARTI000006850948@1994-08-21`).
    pub(crate) ids: Vec<String>,
    /// Decision part to extract: `summary`, `visa`, `dispositif`, `motivations`, or `moyens`.
    /// DILA bulk decisions carry no official Judilibre zones, so non-summary parts are best-effort
    /// `heuristic` (or `unavailable`); each part reports its `zone_provenance`.
    #[arg(long)]
    pub(crate) part: Option<String>,
    /// Consult Judilibre for OFFICIAL Cour de cassation decision zones (lazy, cached). Network is used
    /// only with this flag and only for `cass`/`inca` decisions resolvable by pourvoi; otherwise the
    /// heuristic/unavailable fallback is returned. `capp` (Cour d'appel) and `jade` (administrative)
    /// are not resolvable on Judilibre here.
    #[arg(long)]
    pub(crate) online: bool,
}

pub(crate) fn default_group_by() -> CliGroupBy {
    CliGroupBy::Chunk
}

pub(crate) fn default_related_rel() -> String {
    "cites".to_string()
}

pub(crate) fn default_related_limit() -> u32 {
    50
}

pub(crate) fn default_related_depth() -> u32 {
    1
}

pub(crate) fn default_compare_kind() -> CliKind {
    CliKind::Code
}

#[derive(Debug, Args)]
pub(crate) struct StatusArgs {
    /// Recompute and cache full replay-snapshot signatures (slower); default reads cached signatures.
    #[arg(long)]
    pub(crate) deep: bool,
}

#[derive(Debug, Args)]
pub(crate) struct InspectArgs {
    /// Document id to inspect (e.g. legi:LEGIARTI000006850948@1994-08-21).
    pub(crate) id: String,
}

#[derive(Debug, Args)]
pub(crate) struct VersionsArgs {
    /// Any version's document id; returns the whole version family timeline.
    pub(crate) id: String,
}

#[derive(Debug, Args)]
pub(crate) struct DiffArgs {
    /// Any version's document id (used to resolve the version family).
    pub(crate) id: String,
    /// First date (`YYYY-MM-DD`): the version in force on this date.
    #[arg(long)]
    pub(crate) from: String,
    /// Second date (`YYYY-MM-DD`): the version in force on this date.
    #[arg(long)]
    pub(crate) to: String,
}

#[derive(Debug, Args)]
pub(crate) struct CiteArgs {
    /// Citation or identifier to verify (e.g. "article 1240 du code civil" or a stable ID).
    pub(crate) cite: String,
    /// Fail (exit 2) unless the citation resolves to an exact, valid match.
    #[arg(long)]
    pub(crate) strict: bool,
    /// Also consult the official online source (network) to corroborate the local result.
    #[arg(long)]
    pub(crate) online: bool,
    /// Pin temporal validity to this date (`YYYY-MM-DD`) when resolving the citation.
    #[arg(long)]
    pub(crate) as_of: Option<String>,
}

#[derive(Debug, Args)]
pub(crate) struct RelatedArgs {
    /// Exact, version-pinned stable ID of the document whose graph neighbours to return.
    pub(crate) id: String,
    /// Relation: `cites` (outgoing, default), `cited_by` (incoming), `temporal` (version family), or
    /// `interpreted_by` (decisions citing a statute article).
    #[arg(long, default_value = "cites")]
    pub(crate) rel: String,
    /// Maximum number of neighbours to return.
    #[arg(long, default_value_t = 50)]
    pub(crate) limit: u32,
    /// Traversal depth — only `1` is supported (multi-hop is a later feature).
    #[arg(long, default_value_t = 1)]
    pub(crate) depth: u32,
}

#[derive(Debug, Args)]
pub(crate) struct ContextArgs {
    /// Stable ID of the document whose structural neighbourhood to return.
    pub(crate) id: String,
    /// Include sibling documents (same parent) in the response.
    #[arg(long)]
    pub(crate) siblings: bool,
    /// Pin temporal validity to this date (`YYYY-MM-DD`).
    #[arg(long)]
    pub(crate) as_of: Option<String>,
}

#[derive(Debug, Args, Deserialize)]
pub(crate) struct QueryArgs {
    /// Query whose curated legal-vocabulary expansions to return.
    pub(crate) query: String,
}

#[derive(Debug, Args)]
pub(crate) struct ServeArgs {
    /// Bind a TCP listener at host:port (e.g. 127.0.0.1:8099). Provide this OR --socket.
    #[arg(long)]
    pub(crate) tcp: Option<String>,
    /// Bind a Unix-domain socket at this path. Provide this OR --tcp.
    #[arg(long)]
    pub(crate) socket: Option<PathBuf>,
    /// Allow a non-loopback TCP bind (off-host exposure). Off by default; the protocol is unauthenticated.
    #[arg(long)]
    pub(crate) allow_remote: bool,
}

#[derive(Debug, Args)]
pub(crate) struct JsonlArgs {
    /// Read newline-delimited JSON requests from stdin and write JSONL responses to stdout.
    #[arg(long)]
    pub(crate) jsonl: bool,
    /// Treat a malformed request as fatal (exit non-zero) instead of replying with a JSONL error.
    #[arg(long)]
    pub(crate) fatal: bool,
}

#[derive(Debug, Args)]
pub(crate) struct SyncArgs {
    /// Source to synchronize: `legi`, `cass`, `capp`, `inca`, or `jade`.
    #[arg(long)]
    pub(crate) source: Option<String>,
    /// Directory containing the source's delta archives (and baseline).
    #[arg(long)]
    pub(crate) archives_dir: Option<PathBuf>,
    /// Only ingest delta archives at/after this date (`YYYY-MM-DD`) or compact `YYYYMMDDHHMMSS`.
    #[arg(long)]
    pub(crate) since: Option<String>,
    /// Write skipped/oversized/invalid members to this directory for inspection.
    #[arg(long)]
    pub(crate) quarantine_dir: Option<PathBuf>,
    /// Conservative mode: quarantine on any parse anomaly instead of best-effort recovery.
    #[arg(long)]
    pub(crate) safe_mode: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ModelCommand {
    #[command(subcommand)]
    pub(crate) command: Option<ModelSubcommand>,
}

#[derive(Debug, Args)]
pub(crate) struct EvalCommand {
    #[command(subcommand)]
    pub(crate) command: Option<EvalSubcommand>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum EvalSubcommand {
    /// Run or list Phase 1 LEGI statutory-search fixtures.
    Phase1(EvalPhase1Args),
    /// Run the France-LEGI official-evidence benchmark and emit a phase1_france_legi_benchmark artifact.
    FranceLegi(EvalFranceLegiArgs),
    /// Run the France-jurisprudence benchmark and emit a phase2_france_juris_benchmark artifact.
    ///
    /// Extracts judicial + administrative retrieval gold (decision headnotes) and ECLI/pourvoi/CETATEXT
    /// citation gold from the index, measures recall@10 + citation accuracy through the production
    /// search/cite pipeline, and emits the artifact consumed by the Phase 2 gate.
    FranceJuris(EvalFranceJurisArgs),
    /// Run the official-zone retrieval benchmark and emit a SEPARATE phase2_zone_benchmark artifact.
    ///
    /// Measures recall@10 of `search --zone <motivations|moyens|dispositif>` over the parallel zone
    /// subsystem (`zone_units`): gold = an identifier-stripped excerpt of a decision's OFFICIAL zone
    /// text → the source decision. Measured-only (NOT a Phase 2 gate input): the artifact records the
    /// measured recall against a PROPOSED floor, and never inflates the full-juridic corpus claim.
    FranceJurisZones(EvalFranceJurisZonesArgs),
    /// Run a custom retrieval eval over your own questions with qrels or an external judge.
    ///
    /// Retrieves each question through the chosen modes (document grouping), pools candidates,
    /// gets relevance labels from --qrels or an external --judge-cmd, and scores P@k / recall@k /
    /// nDCG@k / MRR per mode with optional seed-free bootstrap CIs for between-mode deltas.
    Run(EvalRunArgs),
    /// Sweep a hybrid retrieval parameter against a fixture and report the metric-maximizing value.
    ///
    /// Re-runs the eval (hybrid mode) at each sweep point with request-scoped options (no env
    /// mutation). Example:  eval tune --questions q.json --qrels qrels.json --sweep rrf-dense=0.1:1.5:0.1
    Tune(EvalTuneArgs),
}

#[derive(Debug, Clone, Args)]
pub(crate) struct EvalTuneArgs {
    /// JSON file: array of {"id","query","as_of"?}.
    #[arg(long)]
    pub(crate) questions: PathBuf,
    /// JSON qrels file (provide this OR --judge-cmd).
    #[arg(long)]
    pub(crate) qrels: Option<PathBuf>,
    /// External judge command (provide this OR --qrels).
    #[arg(long)]
    pub(crate) judge_cmd: Option<String>,
    /// Parameter sweep `PARAM=start:stop:step`; PARAM in {rrf-dense, rrf-lexical, probes}.
    #[arg(long)]
    pub(crate) sweep: String,
    /// Metric to maximize (p@K, recall@K, ndcg@K, mrr@K).
    #[arg(long, default_value = "ndcg@10")]
    pub(crate) metric: String,
    /// Candidates retrieved per question.
    #[arg(long, default_value_t = 10)]
    pub(crate) top_k: u32,
    /// Minimum relevance label counted as relevant.
    #[arg(long, default_value_t = 1)]
    pub(crate) rel_min: i64,
    /// Write the eval_tune artifact JSON to this path (also printed to stdout).
    #[arg(long)]
    pub(crate) out: Option<PathBuf>,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct EvalRunArgs {
    /// JSON file: array of {"id","query","as_of"?}.
    #[arg(long)]
    pub(crate) questions: PathBuf,
    /// JSON qrels file: array of {"query_id","document_id","label"}. Provide this OR --judge-cmd.
    #[arg(long)]
    pub(crate) qrels: Option<PathBuf>,
    /// External judge command (run via `sh -c`): reads a blind JSON task on stdin, writes
    /// {"<question_id>":{"<key>":label}} on stdout. Provide this OR --qrels.
    #[arg(long)]
    pub(crate) judge_cmd: Option<String>,
    /// Comma-separated retrievers to compare.
    #[arg(long, default_value = "bm25,dense,hybrid")]
    pub(crate) modes: String,
    /// Comma-separated metrics (p@K, recall@K, ndcg@K, mrr@K).
    #[arg(long, default_value = "ndcg@10,recall@10,p@10,mrr@10")]
    pub(crate) metrics: String,
    /// Candidates retrieved per question per mode.
    #[arg(long, default_value_t = 10)]
    pub(crate) top_k: u32,
    /// Minimum relevance label counted as relevant.
    #[arg(long, default_value_t = 1)]
    pub(crate) rel_min: i64,
    /// Bootstrap resamples for between-mode delta CIs (0 = skip).
    #[arg(long, default_value_t = 0)]
    pub(crate) bootstrap: u32,
    /// Write the eval_run artifact JSON to this path (also printed to stdout).
    #[arg(long)]
    pub(crate) out: Option<PathBuf>,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct EvalFranceLegiArgs {
    /// Max known-item qrels to extract from the index.
    #[arg(long, default_value_t = 60)]
    pub(crate) known_item: u32,
    /// Max temporal qrels to extract from the index.
    #[arg(long, default_value_t = 12)]
    pub(crate) temporal: u32,
    /// Max cross-reference qrels to extract from the index.
    #[arg(long, default_value_t = 120)]
    pub(crate) cross_reference: u32,
    /// Pinned official source revision (e.g. archive timestamp) recorded in artifact provenance.
    #[arg(long)]
    pub(crate) source_revision: Option<String>,
    /// Write the phase1_france_legi_benchmark artifact JSON to this path (also printed to stdout).
    #[arg(long)]
    pub(crate) out: Option<PathBuf>,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct EvalFranceJurisArgs {
    /// Max judicial (cass/capp/inca) retrieval qrels to extract from the index.
    #[arg(long, default_value_t = 60)]
    pub(crate) judicial_retrieval: u32,
    /// Max administrative (jade) retrieval qrels to extract from the index.
    #[arg(long, default_value_t = 60)]
    pub(crate) administrative_retrieval: u32,
    /// Max ECLI citation qrels to extract from the index.
    #[arg(long, default_value_t = 30)]
    pub(crate) ecli: u32,
    /// Max pourvoi citation qrels to extract from the index.
    #[arg(long, default_value_t = 30)]
    pub(crate) pourvoi: u32,
    /// Max CETATEXT citation qrels to extract from the index.
    #[arg(long, default_value_t = 30)]
    pub(crate) cetatext: u32,
    /// Pinned official source revision (e.g. archive timestamp) recorded in artifact provenance.
    #[arg(long)]
    pub(crate) source_revision: Option<String>,
    /// Write the phase2_france_juris_benchmark artifact JSON to this path (also printed to stdout).
    #[arg(long)]
    pub(crate) out: Option<PathBuf>,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct EvalFranceJurisZonesArgs {
    /// Max motivations-zone retrieval qrels to extract from `zone_units` (0 skips the zone).
    #[arg(long, default_value_t = 60)]
    pub(crate) motivations: u32,
    /// Max moyens-zone retrieval qrels to extract from `zone_units` (0 skips the zone).
    #[arg(long, default_value_t = 60)]
    pub(crate) moyens: u32,
    /// Max dispositif-zone retrieval qrels to extract from `zone_units` (0 skips the zone).
    #[arg(long, default_value_t = 60)]
    pub(crate) dispositif: u32,
    /// Retrieval mode for the zone search path.
    #[arg(long, default_value = "hybrid")]
    pub(crate) mode: CliSearchMode,
    /// PROPOSED recall@10 floor recorded in the artifact (measured-only; advisory, never asserted).
    #[arg(long, default_value_t = 0.8)]
    pub(crate) floor: f64,
    /// Pinned official source revision (e.g. archive timestamp) recorded in artifact provenance.
    #[arg(long)]
    pub(crate) source_revision: Option<String>,
    /// Write the phase2_zone_benchmark artifact JSON to this path (also printed to stdout).
    #[arg(long)]
    pub(crate) out: Option<PathBuf>,
}

#[derive(Debug, Clone, Args)]
pub(crate) struct EvalPhase1Args {
    /// List selected fixtures without opening an index.
    #[arg(long)]
    pub(crate) list: bool,
    /// Include development fixtures as well as release candidates.
    #[arg(long)]
    pub(crate) include_dev: bool,
    /// Retrieval mode used when executing fixtures.
    #[arg(long, default_value = "hybrid")]
    pub(crate) mode: CliSearchMode,
    /// Number of candidates to inspect per fixture.
    #[arg(long, default_value_t = 10)]
    pub(crate) top_k: u32,
}

#[derive(Debug, Subcommand)]
pub(crate) enum ModelSubcommand {
    /// Ensure a local in-process model is cached (never downloads implicitly during search).
    Fetch {
        /// Model key to fetch; defaults to the configured embedding model when omitted.
        model: Option<String>,
        /// Permit a network download if the model is not already cached.
        #[arg(long)]
        allow_download: bool,
    },
}

#[derive(Debug, Args)]
pub(crate) struct IngestCommand {
    #[command(subcommand)]
    pub(crate) command: Option<IngestSubcommand>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum IngestSubcommand {
    /// Dry-run official archive precedence and delta ordering.
    PlanArchives {
        /// Official source whose archives to plan (e.g. `legi`).
        #[arg(long, default_value = "legi")]
        source: CliArchiveSource,
        /// Directory containing the downloaded official archives to plan.
        #[arg(long)]
        archives_dir: PathBuf,
    },
    /// Stream official LEGI archives into canonical storage with ingest accounting.
    LegiArchives {
        /// Directory containing the LEGI archives to ingest.
        #[arg(long)]
        archives_dir: PathBuf,
        /// Resume/extend an existing ingest run by ID (otherwise a new run is started).
        #[arg(long)]
        run_id: Option<String>,
        /// Process at most this many archive members (for smoke/partial runs).
        #[arg(long)]
        limit_members: Option<u32>,
        /// Skip any single archive member larger than this many bytes.
        #[arg(long, default_value_t = DEFAULT_MEMBER_BYTE_LIMIT)]
        max_member_bytes: u64,
        /// Write skipped/oversized/invalid members to this directory for inspection.
        #[arg(long)]
        quarantine_dir: Option<PathBuf>,
        /// Conservative mode: quarantine on any parse anomaly instead of best-effort recovery.
        #[arg(long)]
        safe_mode: bool,
    },
    /// Stream DILA bulk jurisprudence archives (cass/capp/inca/jade) into canonical decisions.
    JuriArchives {
        /// Jurisprudence dataset to ingest.
        #[arg(long)]
        source: CliJuriSource,
        /// Directory containing the jurisprudence archives to ingest.
        #[arg(long)]
        archives_dir: PathBuf,
        /// Resume/extend an existing ingest run by ID (otherwise a new run is started).
        #[arg(long)]
        run_id: Option<String>,
        /// Process at most this many archive members (for smoke/partial runs).
        #[arg(long)]
        limit_members: Option<u32>,
        /// Skip any single archive member larger than this many bytes.
        #[arg(long, default_value_t = DEFAULT_MEMBER_BYTE_LIMIT)]
        max_member_bytes: u64,
        /// Write skipped/oversized/invalid members to this directory for inspection.
        #[arg(long)]
        quarantine_dir: Option<PathBuf>,
        /// Conservative mode: quarantine on any parse anomaly instead of best-effort recovery.
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
        /// Number of chunk texts sent per embeddings request.
        #[arg(long, default_value_t = EMBED_CHUNKS_DEFAULT_BATCH_SIZE)]
        batch_size: usize,
        /// Maximum concurrent embedding requests across the endpoint pool.
        #[arg(long, default_value_t = EMBED_CHUNKS_DEFAULT_POOL_CONCURRENCY)]
        pool_concurrency: usize,
    },
    /// Eagerly backfill official Judilibre zones for Cour de cassation decisions into `decision_zones`
    /// (the per-decision overlay that also powers `fetch --part --online`). Resumable; honors PISTE
    /// rate limits via a conservative bounded concurrency. Source must be `cass` or `inca`.
    EnrichZones {
        /// Decision source family to enrich: `cass` (published) or `inca` (inédit).
        #[arg(long)]
        source: String,
        /// Maximum decisions to attempt this run (omit to process the whole resolver-reachable set).
        #[arg(long)]
        limit: Option<u32>,
        /// Refresh mode: also re-enrich decisions whose cache was fetched before this ISO timestamp.
        #[arg(long)]
        since: Option<String>,
        /// Conservative bound on concurrent Judilibre requests (stay well under ~20 req/s).
        #[arg(long, default_value_t = ENRICH_ZONES_DEFAULT_CONCURRENCY)]
        concurrency: usize,
        /// Walk order over the candidate set. `recent` reaches zoned (newer) decisions first; the
        /// default `oldest` preserves the original keyset order.
        #[arg(long, default_value = "oldest")]
        order: CliEnrichZoneOrder,
    },
    /// Derive `zone_units` retrieval units from the cached official Judilibre zones in `decision_zones`
    /// (after `enrich-zones`). Idempotent; re-derives only stale decisions unless `--rebuild`.
    BuildZoneUnits {
        /// Maximum decisions to derive this run (omit to process all derivable).
        #[arg(long)]
        limit: Option<u32>,
        /// Re-derive every eligible decision regardless of existing-unit state (builder-version bump).
        #[arg(long)]
        rebuild: bool,
    },
    /// Embed `zone_units` and finalize the zone-unit dense ANN index (the parallel zone retrieval index;
    /// uses the same embedding pool + fingerprint as `embed-chunks`, separate physical tables/index).
    EmbedZoneUnits {
        /// Maximum zone-unit count allowed for this run; refuses larger sets instead of partial finalize.
        #[arg(long)]
        limit: Option<u32>,
        /// Number of ivfflat lists for the zone-unit dense index.
        #[arg(long, default_value_t = 32)]
        index_lists: u32,
        /// Number of zone-unit texts sent per embeddings request.
        #[arg(long, default_value_t = EMBED_CHUNKS_DEFAULT_BATCH_SIZE)]
        batch_size: usize,
        /// Maximum concurrent embedding requests across the endpoint pool.
        #[arg(long, default_value_t = EMBED_CHUNKS_DEFAULT_POOL_CONCURRENCY)]
        pool_concurrency: usize,
    },
    /// Extract legislation citations from the archived Judilibre `/decision` responses (visa) into
    /// per-decision occurrences + deduped pending resolutions. No network (reads `official_api_responses`).
    CollectLegislationCitations {
        /// Maximum archived decisions to scan this run (omit to scan all).
        #[arg(long)]
        limit: Option<u32>,
    },
    /// Resolve the deduped legislation citations against the Legifrance API (once per unique citation),
    /// archiving each raw Legifrance response in `official_api_responses`. Run after collect.
    EnrichLegislationCitations {
        /// Maximum unique citations to resolve this run (omit to resolve all pending).
        #[arg(long)]
        limit: Option<u32>,
        /// Also retry citations previously left in upstream_error/parse_error.
        #[arg(long)]
        retry_errors: bool,
    },
    /// Rebuild LEGI article hierarchy from persisted metadata across the full index.
    BackfillLegiHierarchy,
}

#[derive(Debug, Args)]
pub(crate) struct HelpCommand {
    #[command(subcommand)]
    pub(crate) command: Option<HelpSubcommand>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum HelpSubcommand {
    /// Print the compiled agent-facing contract (commands, exit codes, session protocol).
    Agent,
    /// Print machine-readable JSON schemas for command requests, responses, and errors.
    Schema {
        /// Emit the schema as JSON (machine-readable).
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Clone, Copy, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CliKind {
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
pub(crate) enum CliSearchMode {
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

#[derive(Debug, Clone, Copy, Deserialize, ValueEnum, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CliGroupBy {
    Chunk,
    Document,
}

impl From<CliGroupBy> for GroupBy {
    fn from(group_by: CliGroupBy) -> Self {
        match group_by {
            CliGroupBy::Chunk => Self::Chunk,
            CliGroupBy::Document => Self::Document,
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CliOutputFormat {
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
pub(crate) enum CliArchiveSource {
    Legi,
    Cass,
    Capp,
    Inca,
    Jade,
}

impl From<CliArchiveSource> for ArchiveSource {
    fn from(source: CliArchiveSource) -> Self {
        match source {
            CliArchiveSource::Legi => Self::Legi,
            CliArchiveSource::Cass => Self::Cass,
            CliArchiveSource::Capp => Self::Capp,
            CliArchiveSource::Inca => Self::Inca,
            CliArchiveSource::Jade => Self::Jade,
        }
    }
}

/// The four DILA bulk jurisprudence datasets ingested by `ingest juri-archives`.
#[derive(Debug, Clone, Copy, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CliJuriSource {
    Cass,
    Capp,
    Inca,
    Jade,
}

impl From<CliJuriSource> for ArchiveSource {
    fn from(source: CliJuriSource) -> Self {
        match source {
            CliJuriSource::Cass => Self::Cass,
            CliJuriSource::Capp => Self::Capp,
            CliJuriSource::Inca => Self::Inca,
            CliJuriSource::Jade => Self::Jade,
        }
    }
}

pub(crate) fn default_cli_kind() -> CliKind {
    CliKind::All
}

pub(crate) fn default_search_mode() -> CliSearchMode {
    CliSearchMode::Hybrid
}

pub(crate) fn default_output_format() -> CliOutputFormat {
    CliOutputFormat::Concise
}

pub(crate) fn default_top_k() -> u32 {
    10
}
