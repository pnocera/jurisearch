use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    fs,
    io::{self, BufRead, Write},
    net::{TcpStream, ToSocketAddrs},
    path::{Path, PathBuf},
    process::ExitCode,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use jurisearch_core::{
    SCHEMA_VERSION,
    contract::{CitationState, LegalKind, OutputFormat, agent_help},
    error::{ErrorCode, ErrorObject, ProcessExit},
    eval::{
        LegalRetrievalFixture, phase1_eval_fixture_summary, phase1_eval_fixtures,
        phase1_release_candidate_fixtures,
    },
    expand::expand_query,
    schema::compiled_schema,
};
use jurisearch_embed::{
    EmbeddingConfig, EmbeddingFingerprint, EmbeddingProvider, OpenAiCompatibleClient,
    PHASE0_EMBEDDING_DIMENSION, PHASE0_EMBEDDING_MODEL,
};
use jurisearch_ingest::{
    archive::{
        ArchiveMember, ArchivePlan, ArchiveSource, ArchiveVisit, DEFAULT_MEMBER_BYTE_LIMIT,
        PlannedArchive, for_each_xml_member_until, plan_from_dir,
    },
    juri::{JuriParseError, ParsedJuriXml, parse_juri_member},
    legi::{LegiParseError, ParsedLegiXml, parse_legi_member, source_payload_hash},
};
use jurisearch_official_api::{
    OfficialApiConfig, OfficialApiExchange, OfficialApiOutcome, PisteClient,
};
use jurisearch_storage::dense::ChunkEmbeddingInput;
use jurisearch_storage::{
    citation::{CitationLookupQuery, citation_lookup_json},
    decision_zones::{
        UpsertDecisionZones, decision_resolution_metadata_with_client, decision_zones_json,
        upsert_decision_zones_with_client,
    },
    dense::{
        DENSE_VECTOR_DIMENSION, DenseRebuildSpec, finalize_dense_rebuild,
        load_chunk_embedding_inputs,
    },
    ingest_accounting::{
        IngestCompatibility, IngestErrorInput, IngestHealthReport,
        IngestMemberInput, IngestMemberStatus, IngestResumeAction, IngestRunInput, IngestRunStatus,
        ReplaySnapshotMode, ReplaySnapshotReport, finish_ingest_run_with_client,
        ingest_resume_decision_with_client, invalidate_cached_query_readiness,
        load_ingest_health_with_replay_snapshot_mode, load_or_compute_query_readiness,
        record_ingest_error_with_client, record_ingest_member_with_client, refresh_replay_snapshot,
        start_ingest_run_with_client, update_ingest_member_status_with_client,
        update_ingest_run_manifest_with_client,
    },
    france_juris::{
        FranceJurisGoldLimits, FranceJurisZoneGoldLimits, france_juris_gold_json,
        france_juris_index_revision, france_juris_zone_gold_json,
    },
    france_legi::{FranceLegiGoldLimits, france_legi_gold_json},
    legislation_citations::{
        InsertCitationOccurrence, finalize_citation_occurrence_counts,
        insert_citation_occurrence_with_client, legislation_citations_coverage_json,
        load_archived_decisions_with_visa_json, load_pending_citation_resolutions_json,
        update_citation_resolution_with_client, upsert_citation_resolution_pending_with_client,
    },
    migrations::CURRENT_SCHEMA_VERSION,
    official_api_archive::{InsertOfficialApiResponse, insert_official_api_response_with_client},
    projection::{
        ChunkEmbeddingInsert, DocumentProjectionStatements, LegiHierarchyBackfillScope,
        LegiMetadataRoot, LegiProjectionStatements, backfill_legi_article_hierarchy_from_metadata,
        backfill_legi_article_hierarchy_from_metadata_scoped, insert_chunk_embeddings,
        insert_decision_documents_with_statements, insert_legi_documents_with_statements,
        insert_legi_metadata_roots_with_client, prepare_document_projection_statements,
        prepare_legi_projection_statements,
    },
    retrieval::{
        CitationResolutionQuery, ContextDocumentsQuery, DecisionFilters, GroupBy,
        HybridCandidateQuery, RelatedQuery, RelatedRelation, RetrievalCursor, RetrievalMode,
        RetrievalOptions, context_documents_json, corpus_source_coverage_json, corpus_stats_json,
        document_diff_json, document_versions_json, hybrid_candidates_json, inspect_document_json,
        related_neighbours_json, resolve_legi_citation_json, rrf_weights,
    },
    runtime::{ManagedPostgres, PgConfig, PostgresRuntimeProfile, StorageError},
    zone_retrieval::{ZoneCandidateQuery, zone_candidates_json},
    zone_units::{
        ZoneUnitEmbeddingInsert, ZoneUnitRow, enrich_zone_candidates_json,
        finalize_zone_dense_rebuild, insert_zone_unit_embeddings, load_derivable_decision_zones_json,
        load_zone_unit_embedding_inputs, replace_zone_units_for_document,
        zone_resolver_reachable_json, zone_retrieval_coverage_json,
    },
};
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};
use url::Url;

mod args;
mod ascii;
mod citation;
mod date;
mod dispatch;
mod embedding_runtime;
mod enrichment;
mod errors;
mod legifrance_search;
mod output;
mod query_support;
mod retrieval;
mod serve;
mod session;

use crate::args::*;
use crate::ascii::*;
use crate::citation::*;
use crate::date::*;
use crate::embedding_runtime::*;
use crate::enrichment::*;
use crate::errors::*;
use crate::legifrance_search::*;
use crate::output::*;
use crate::query_support::*;
use crate::retrieval::*;

const LEGI_PARSER_VERSION: &str = "legi_article_metadata_parser:v4";
const CANONICAL_SCHEMA_VERSION: &str = "canonical_record:v3";
const CLI_CODE_VERSION: &str = concat!("jurisearch-cli:", env!("CARGO_PKG_VERSION"));
const MODEL_CACHE_REQUIRED_FILES: &[&str] = &["model.onnx", "tokenizer.json"];
const EMBEDDING_ENDPOINT_MAX_ATTEMPTS: usize = 3;
const LOOPBACK_ENDPOINT_CONNECT_TIMEOUT: Duration = Duration::from_millis(250);
const LEGI_INGEST_TRANSACTION_BATCH_SIZE: usize = 128;
const LEGI_INGEST_TRANSACTION_BATCH_BYTE_LIMIT: usize = 64 * 1024 * 1024;
pub(crate) const EMBED_CHUNKS_DEFAULT_BATCH_SIZE: usize = 32;
pub(crate) const EMBED_CHUNKS_DEFAULT_POOL_CONCURRENCY: usize = 4;
/// Conservative default for concurrent Judilibre requests during zone backfill (each decision is ~2
/// calls; stay well under the live ~20 req/s burst limit). `--concurrency 1` is the deterministic
/// sequential fallback.
pub(crate) const ENRICH_ZONES_DEFAULT_CONCURRENCY: usize = 6;
/// Candidate page size for the zone backfill keyset scan.
const ENRICH_ZONES_PAGE_SIZE: u32 = 200;
/// Page size for scanning archived decisions during legislation-citation collection (no network).
const COLLECT_CITATIONS_PAGE_SIZE: u32 = 500;
/// Page size for resolving deduped legislation citations against Legifrance (sequential, network).
const ENRICH_CITATIONS_PAGE_SIZE: u32 = 100;
/// Derivation-logic version stamped on `zone_units`; bump to force a full re-derive on a logic change.
const ZONE_UNIT_BUILDER_VERSION: &str = "zone-units:v1";
/// Candidate page size for the zone-unit derivation keyset scan.
const BUILD_ZONE_UNITS_PAGE_SIZE: u32 = 500;
const PHASE1_EXTERNAL_BENCHMARK_ENV: &str = "JURISEARCH_PHASE1_EXTERNAL_BENCHMARK";
const PHASE1_EXTERNAL_MIN_BSARD_DOCUMENTS: u64 = 22_000;
const PHASE1_EXTERNAL_MIN_BSARD_QUESTIONS: u64 = 200;
const PHASE1_EXTERNAL_MIN_HYBRID_RECALL_AT_20: f64 = 0.75;
const PHASE1_EXTERNAL_MIN_HYBRID_NDCG_AT_20: f64 = 0.60;
const PHASE1_EXTERNAL_MIN_HYBRID_MRR_AT_20: f64 = 0.50;
const PHASE1_FRANCE_LEGI_BENCHMARK_ENV: &str = "JURISEARCH_PHASE1_FRANCE_LEGI_BENCHMARK";
// France-LEGI split gate. Structured-fact queries (citation resolution, temporal version pinning)
// route to the structured resolver; conceptual queries to hybrid search. The two structured
// categories GATE the claim at high floors; full-body semantic retrieval is an ADVISORY stress test
// (it mostly measures accidental topical similarity, so it does not gate). Calibrated 2026-06-23 on
// index/phase1-freemium-20250713: structured_citation 1.00, temporal 1.00, semantic 0.116. See
// work/03-implementation/02-evidence/2026-06-23-france-legi-gate-split.md
const PHASE1_FRANCE_LEGI_MIN_STRUCTURED_CITATION_RECALL_AT_10: f64 = 0.95;
const PHASE1_FRANCE_LEGI_MIN_TEMPORAL_VERSION_EXACTNESS_AT_10: f64 = 0.90;
const PHASE1_FRANCE_LEGI_ADVISORY_SEMANTIC_RECALL_AT_10: f64 = 0.40;
const PHASE1_FRANCE_LEGI_MIN_STRUCTURED_CITATION_QUERIES: u64 = 10;
const PHASE1_FRANCE_LEGI_MIN_TEMPORAL_QUERIES: u64 = 4;
const PHASE1_FRANCE_LEGI_MIN_SEMANTIC_QUERIES: u64 = 50;
// The gate validates recall/exactness @10, so the runner is fixed at top-10 (document-level).
const FRANCE_LEGI_GATE_TOP_K: u32 = 10;

// Phase 2 full-french-juridic gate. Fail-closed: the "best-in-class French juridic search" claim is
// allowed only once a passing jurisprudence eval benchmark (Cassation + administrative retrieval AND
// decision-citation verification, through the production pipeline) is supplied. Floors are the
// release policy; status re-derives pass from the artifact's per-category metrics, never trusting a
// self-reported `state`.
const PHASE2_BENCHMARK_ENV: &str = "JURISEARCH_PHASE2_BENCHMARK";
// The benchmark must prove BOTH jurisprudence families (judicial Cassation/appeal AND administrative)
// AND decision-citation verification across all three identifier kinds — through the production
// pipeline. Each is re-derived against these floors; the artifact's self-reported `state` is ignored.
const PHASE2_PRODUCTION_PIPELINE: &str = "production";
const PHASE2_MIN_RETRIEVAL_RECALL_AT_10: f64 = 0.50;
const PHASE2_MIN_JUDICIAL_RETRIEVAL_QUERIES: u64 = 15;
const PHASE2_MIN_ADMINISTRATIVE_RETRIEVAL_QUERIES: u64 = 15;
const PHASE2_MIN_DECISION_CITATION_ACCURACY: f64 = 0.95;
// Per-identifier floor: each of ECLI/pourvoi/CETATEXT must be MEASURED (not just declared), so the
// "ECLI/pourvoi/CETATEXT verification" claim cannot pass on an ECLI-only benchmark.
const PHASE2_MIN_CITATION_QUERIES_PER_IDENTIFIER: u64 = 10;
const PHASE2_REQUIRED_CITATION_IDENTIFIERS: [&str; 3] = ["ecli", "pourvoi", "cetatext"];

fn main() -> ExitCode {
    match dispatch::run() {
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

/// Incremental sync: pull a source's new delta archives into the existing index. Reuses the proven
/// per-source ingest path (and its compatibility-based resume, which skips already-ingested members
/// and blocks parser/schema/code/source-payload mismatches — so sync can never silently mix
/// incompatible versions). `--since` bounds which delta archives are scanned so a sync never
/// re-reads the full baseline corpus; `status.corpus_sources` then reports the new freshness.
pub(crate) fn sync_payload(args: SyncArgs, index_dir: Option<&Path>) -> Result<Value, ErrorObject> {
    let source_token = args.source.as_deref().ok_or_else(|| {
        ErrorObject::bad_input("sync requires --source (legi|cass|capp|inca|jade)")
    })?;
    let source = ArchiveSource::from_token(source_token).ok_or_else(|| {
        ErrorObject::bad_input(format!(
            "unknown sync --source `{source_token}`; expected legi|cass|capp|inca|jade"
        ))
    })?;
    let archives_dir = args
        .archives_dir
        .as_deref()
        .ok_or_else(|| ErrorObject::bad_input("sync requires --archives-dir"))?;
    let since_compact = match args.since.as_deref() {
        None => None,
        Some(raw) => Some(normalize_since(raw).ok_or_else(|| {
            ErrorObject::bad_input(format!(
                "invalid --since `{raw}`; expected YYYY-MM-DD or YYYYMMDDHHMMSS"
            ))
        })?),
    };
    // Incremental: a prior full build already ingested the baseline; only newer deltas are pulled.
    let archive_filter = ArchiveSyncFilter {
        incremental: true,
        since_compact: since_compact.as_deref(),
    };

    let mut response = if source.is_jurisprudence() {
        ingest_juri_archives_payload(
            index_dir,
            source,
            archives_dir,
            None,
            None,
            DEFAULT_MEMBER_BYTE_LIMIT,
            args.quarantine_dir.as_deref(),
            args.safe_mode,
            archive_filter,
        )?
    } else {
        ingest_legi_archives_payload(
            index_dir,
            archives_dir,
            None,
            None,
            DEFAULT_MEMBER_BYTE_LIMIT,
            args.quarantine_dir.as_deref(),
            args.safe_mode,
            archive_filter,
        )?
    };

    // Re-frame the ingest result as a sync result.
    if let Value::Object(map) = &mut response {
        map.insert("command".to_owned(), json!("sync"));
        map.insert("mode".to_owned(), json!("incremental"));
        map.insert("source".to_owned(), json!(source.as_str()));
        map.insert("synced_since".to_owned(), json!(args.since));
    }
    Ok(response)
}

pub(crate) fn emit_eval(eval: EvalCommand, index_dir: Option<&Path>) -> anyhow::Result<()> {
    match eval.command {
        Some(EvalSubcommand::Phase1(args)) => match eval_phase1_payload(args, index_dir) {
            Ok(response) => write_json(&response),
            Err(error) => emit_error(error),
        },
        Some(EvalSubcommand::FranceLegi(args)) => {
            let out_path = args.out.clone();
            match eval_france_legi_payload(args, index_dir) {
                Ok(response) => emit_artifact(response, out_path),
                Err(error) => emit_error(error),
            }
        }
        Some(EvalSubcommand::FranceJuris(args)) => {
            let out_path = args.out.clone();
            match eval_france_juris_payload(args, index_dir) {
                Ok(response) => emit_artifact(response, out_path),
                Err(error) => emit_error(error),
            }
        }
        Some(EvalSubcommand::FranceJurisZones(args)) => {
            let out_path = args.out.clone();
            match eval_france_juris_zones_payload(args, index_dir) {
                Ok(response) => emit_artifact(response, out_path),
                Err(error) => emit_error(error),
            }
        }
        Some(EvalSubcommand::Run(args)) => {
            let out_path = args.out.clone();
            match eval_run_payload(args, RetrievalOptions::default(), index_dir) {
                Ok(response) => emit_artifact(response, out_path),
                Err(error) => emit_error(error),
            }
        }
        Some(EvalSubcommand::Tune(args)) => {
            let out_path = args.out.clone();
            match eval_tune_payload(args, index_dir) {
                Ok(response) => emit_artifact(response, out_path),
                Err(error) => emit_error(error),
            }
        }
        None => emit_error(ErrorObject::bad_input(
            "eval requires a subcommand; try `eval phase1`, `eval france-legi`, or `eval run`",
        )),
    }
}

// ---- General retrieval eval harness (`eval run`) -----------------------------------------------

#[derive(Debug, Deserialize)]
struct EvalQuestion {
    id: String,
    query: String,
    #[serde(default)]
    as_of: Option<String>,
}

#[derive(Debug, Deserialize)]
struct EvalQrel {
    query_id: String,
    document_id: String,
    label: i64,
}

#[derive(Debug, Clone, Copy)]
enum MetricKind {
    Precision,
    Recall,
    Ndcg,
    Mrr,
}

#[derive(Debug, Clone)]
struct MetricSpec {
    kind: MetricKind,
    k: usize,
    name: String,
}

struct PoolCandidate {
    uid: String,
    title: Value,
    snippet: Value,
}

struct EvalQuestionResult {
    id: String,
    query: String,
    per_mode: HashMap<&'static str, Vec<String>>,
    pool: Vec<PoolCandidate>,
    labels: HashMap<String, i64>,
}

/// Deterministic xorshift64 RNG so bootstrap CIs are reproducible (no rand dependency, and
/// `Math.random`-style nondeterminism would make eval artifacts unstable).
struct XorShift64(u64);
impl XorShift64 {
    fn new(seed: u64) -> Self {
        Self(seed | 1)
    }
    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

/// FNV-1a fold of a question id → a stable bootstrap/shuffle seed (reproducible across runs).
fn eval_question_seed(id: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in id.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn load_eval_json<T: serde::de::DeserializeOwned>(path: &Path, what: &str) -> Result<T, ErrorObject> {
    let bytes = fs::read(path)
        .map_err(|error| ErrorObject::bad_input(format!("failed to read {what} file {}: {error}", path.display())))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| ErrorObject::bad_input(format!("invalid {what} JSON in {}: {error}", path.display())))
}

fn parse_eval_modes(value: &str) -> Result<Vec<RetrievalMode>, ErrorObject> {
    let mut modes = Vec::new();
    for token in value.split(',').map(str::trim).filter(|token| !token.is_empty()) {
        let mode = match token {
            "bm25" => RetrievalMode::Bm25,
            "dense" => RetrievalMode::Dense,
            "hybrid" => RetrievalMode::Hybrid,
            other => {
                return Err(ErrorObject::bad_input(format!(
                    "unknown mode `{other}`; expected bm25, dense, or hybrid"
                )));
            }
        };
        if !modes.contains(&mode) {
            modes.push(mode);
        }
    }
    if modes.is_empty() {
        return Err(ErrorObject::bad_input(
            "--modes must list at least one of bm25, dense, hybrid",
        ));
    }
    Ok(modes)
}

fn parse_eval_metric(value: &str) -> Result<MetricSpec, ErrorObject> {
    let value = value.trim();
    let (name, k_str) = value.split_once('@').unwrap_or((value, "10"));
    let k: usize = k_str.parse().map_err(|_| {
        ErrorObject::bad_input(format!("metric `{value}` has a non-numeric @k"))
    })?;
    if k == 0 {
        return Err(ErrorObject::bad_input(format!("metric `{value}` @k must be >= 1")));
    }
    let kind = match name {
        "p" | "precision" => MetricKind::Precision,
        "recall" => MetricKind::Recall,
        "ndcg" => MetricKind::Ndcg,
        "mrr" => MetricKind::Mrr,
        other => {
            return Err(ErrorObject::bad_input(format!(
                "unknown metric `{other}`; expected p, recall, ndcg, or mrr"
            )));
        }
    };
    Ok(MetricSpec {
        kind,
        k,
        name: format!("{name}@{k}"),
    })
}

/// Per-question metric value over a mode's ranked doc list. `recall` returns `None` when the pool
/// has no relevant document (so it is excluded from the mean, not counted as 0).
fn compute_eval_metric(
    spec: &MetricSpec,
    top: &[String],
    labels: &HashMap<String, i64>,
    pool: &[String],
    rel_min: i64,
) -> Option<f64> {
    let label_of = |uid: &String| *labels.get(uid).unwrap_or(&0);
    let topk: Vec<&String> = top.iter().take(spec.k).collect();
    let relevant: HashSet<&String> = pool.iter().filter(|uid| label_of(uid) >= rel_min).collect();
    match spec.kind {
        MetricKind::Precision => {
            // Standard P@k: divide by k (missing ranks count as non-relevant), so a short page does
            // not inflate precision (document grouping can exhaust the pool before k).
            let hits = topk.iter().filter(|uid| label_of(uid) >= rel_min).count();
            Some(hits as f64 / spec.k as f64)
        }
        MetricKind::Recall => {
            if relevant.is_empty() {
                None
            } else {
                let hits = topk.iter().filter(|uid| relevant.contains(*uid)).count();
                Some(hits as f64 / relevant.len() as f64)
            }
        }
        MetricKind::Ndcg => {
            let gain = |label: i64| (2f64.powi(label.max(0) as i32)) - 1.0;
            let dcg: f64 = topk
                .iter()
                .enumerate()
                .map(|(i, uid)| gain(label_of(uid)) / ((i as f64) + 2.0).log2())
                .sum();
            let mut ideal: Vec<i64> = pool.iter().map(|uid| label_of(uid)).collect();
            ideal.sort_unstable_by(|a, b| b.cmp(a));
            let idcg: f64 = ideal
                .iter()
                .take(spec.k)
                .enumerate()
                .map(|(i, label)| gain(*label) / ((i as f64) + 2.0).log2())
                .sum();
            Some(if idcg > 0.0 { dcg / idcg } else { 0.0 })
        }
        MetricKind::Mrr => {
            for (i, uid) in topk.iter().enumerate() {
                if label_of(uid) >= rel_min {
                    return Some(1.0 / ((i as f64) + 1.0));
                }
            }
            Some(0.0)
        }
    }
}

fn mean_of(values: &[Option<f64>]) -> Option<f64> {
    let present: Vec<f64> = values.iter().filter_map(|value| *value).collect();
    if present.is_empty() {
        None
    } else {
        Some(present.iter().sum::<f64>() / present.len() as f64)
    }
}

/// Bootstrap a 95% CI for the mean difference (a - b), resampling QUESTIONS with replacement.
fn bootstrap_delta_ci(a: &[Option<f64>], b: &[Option<f64>], resamples: u32) -> (f64, f64, f64) {
    let n = a.len();
    let resample_mean = |idx: &[usize], values: &[Option<f64>]| -> Option<f64> {
        let present: Vec<f64> = idx.iter().filter_map(|&i| values[i]).collect();
        if present.is_empty() {
            None
        } else {
            Some(present.iter().sum::<f64>() / present.len() as f64)
        }
    };
    let all: Vec<usize> = (0..n).collect();
    let point = match (resample_mean(&all, a), resample_mean(&all, b)) {
        (Some(x), Some(y)) => x - y,
        _ => f64::NAN,
    };
    let mut rng = XorShift64::new(0x6a75_7269_7365_6172 ^ n as u64);
    let mut deltas: Vec<f64> = Vec::with_capacity(resamples as usize);
    for _ in 0..resamples {
        let sample: Vec<usize> = (0..n)
            .map(|_| (rng.next_u64() % n.max(1) as u64) as usize)
            .collect();
        if let (Some(x), Some(y)) = (resample_mean(&sample, a), resample_mean(&sample, b)) {
            deltas.push(x - y);
        }
    }
    if deltas.is_empty() {
        return (point, f64::NAN, f64::NAN);
    }
    deltas.sort_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
    let lo = deltas[((0.025 * deltas.len() as f64) as usize).min(deltas.len() - 1)];
    let hi = deltas[((0.975 * deltas.len() as f64) as usize).min(deltas.len() - 1)];
    (point, lo, hi)
}

fn run_external_judge(command: &str, input: &Value) -> Result<Value, ErrorObject> {
    use std::process::{Command, Stdio};
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| dependency_unavailable(format!("failed to spawn judge: {error}")))?;
    let payload = serde_json::to_vec(input)
        .map_err(|error| dependency_unavailable(format!("failed to encode judge input: {error}")))?;
    child
        .stdin
        .take()
        .ok_or_else(|| dependency_unavailable("judge stdin unavailable"))?
        .write_all(&payload)
        .map_err(|error| dependency_unavailable(format!("failed to write judge stdin: {error}")))?;
    let output = child
        .wait_with_output()
        .map_err(|error| dependency_unavailable(format!("judge did not complete: {error}")))?;
    if !output.status.success() {
        return Err(dependency_unavailable(format!(
            "judge command failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    serde_json::from_slice(&output.stdout).map_err(|error| {
        ErrorObject::bad_input(format!("judge stdout was not a JSON label map: {error}"))
    })
}

/// Custom retrieval eval: retrieve each question through the chosen modes (document grouping), pool
/// candidates, get relevance labels from qrels or an external judge, score per mode, and optionally
/// bootstrap between-mode delta CIs. Opens the index once.
fn eval_run_payload(
    args: EvalRunArgs,
    options: RetrievalOptions,
    index_dir: Option<&Path>,
) -> Result<Value, ErrorObject> {
    if args.qrels.is_none() && args.judge_cmd.is_none() {
        return Err(ErrorObject::bad_input(
            "eval run needs relevance labels: provide --qrels or --judge-cmd",
        ));
    }
    if args.qrels.is_some() && args.judge_cmd.is_some() {
        return Err(ErrorObject::bad_input(
            "provide --qrels OR --judge-cmd, not both",
        ));
    }
    if args.top_k == 0 {
        return Err(ErrorObject::bad_input("--top-k must be at least 1"));
    }
    validate_retrieval_options(&options)?;
    let modes = parse_eval_modes(&args.modes)?;
    let metrics: Vec<MetricSpec> = args
        .metrics
        .split(',')
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(parse_eval_metric)
        .collect::<Result<Vec<_>, _>>()?;
    if metrics.is_empty() {
        return Err(ErrorObject::bad_input("--metrics must list at least one metric"));
    }
    let questions: Vec<EvalQuestion> = load_eval_json(&args.questions, "questions")?;
    if questions.is_empty() {
        return Err(ErrorObject::bad_input("questions file is empty"));
    }

    // A BM25-only eval must not require the embedding runtime: only build the embedder and embed
    // when a dense/hybrid mode is requested, and use the lexical readiness gate otherwise.
    let needs_dense = modes.iter().any(|mode| mode.uses_dense());
    let index_dir = require_existing_index_dir(index_dir)?;
    let postgres = open_index(index_dir.as_path())?;
    ensure_query_readiness(
        &postgres,
        if needs_dense {
            QueryReadinessGate::Search
        } else {
            QueryReadinessGate::SearchLexical
        },
    )?;
    let embedder = if needs_dense {
        Some(PreparedQueryEmbedder::from_env()?)
    } else {
        None
    };
    let pool_limit = args.top_k.saturating_mul(20);

    // 1. Retrieval: per question, each mode's top docs + the pooled candidate set.
    let mut results: Vec<EvalQuestionResult> = Vec::with_capacity(questions.len());
    for question in &questions {
        let normalized = parade_query_text(&question.query).ok_or_else(|| {
            ErrorObject::bad_input(format!(
                "question `{}` has no searchable token: {:?}",
                question.id, question.query
            ))
        })?;
        let as_of = question.as_of.clone().unwrap_or_else(today_utc);
        let embedded = match &embedder {
            Some(embedder) => Some(embedder.embed(question.query.as_str())?),
            None => None,
        };
        let mut per_mode: HashMap<&'static str, Vec<String>> = HashMap::new();
        let mut pool: Vec<PoolCandidate> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for mode in &modes {
            let (embedding, fingerprint) = match (&embedded, mode.uses_dense()) {
                (Some((literal, fingerprint)), true) => {
                    (Some(literal.as_str()), Some(fingerprint.as_str()))
                }
                _ => (None, None),
            };
            let response = hybrid_candidates_json(
                &postgres,
                &HybridCandidateQuery {
                    query_text: &normalized,
                    query_embedding: embedding,
                    embedding_fingerprint: fingerprint,
                    retrieval_mode: *mode,
                    group_by: GroupBy::Document,
                    options,
                    after_cursor: None,
                    as_of: as_of.as_str(),
                    kind_filter: Some("article"),
                    decision_filters: DecisionFilters::default(),
                    lexical_limit: pool_limit,
                    dense_limit: pool_limit,
                    limit: args.top_k,
                },
            )
            .map_err(storage_error_object)?;
            let response: Value = serde_json::from_str(&response)
                .map_err(|error| dependency_unavailable(error.to_string()))?;
            let candidates = response["candidates"].as_array().cloned().unwrap_or_default();
            let mut top = Vec::new();
            for candidate in &candidates {
                let Some(uid) = candidate["document_id"].as_str() else {
                    continue;
                };
                top.push(uid.to_owned());
                if seen.insert(uid.to_owned()) {
                    pool.push(PoolCandidate {
                        uid: uid.to_owned(),
                        title: candidate.get("title").cloned().unwrap_or(Value::Null),
                        snippet: candidate.get("snippet").cloned().unwrap_or(Value::Null),
                    });
                }
            }
            per_mode.insert(mode.as_str(), top);
        }
        results.push(EvalQuestionResult {
            id: question.id.clone(),
            query: question.query.clone(),
            per_mode,
            pool,
            labels: HashMap::new(),
        });
    }

    // 2. Relevance labels: qrels lookup, or a single blind external-judge invocation.
    let judge_source;
    if let Some(qrels_path) = &args.qrels {
        let qrels: Vec<EvalQrel> = load_eval_json(qrels_path, "qrels")?;
        let mut by_query: HashMap<String, HashMap<String, i64>> = HashMap::new();
        for qrel in qrels {
            by_query
                .entry(qrel.query_id)
                .or_default()
                .insert(qrel.document_id, qrel.label);
        }
        for result in &mut results {
            if let Some(labels) = by_query.get(&result.id) {
                result.labels = labels.clone();
            }
        }
        judge_source = "qrels".to_owned();
    } else {
        let command = args.judge_cmd.as_deref().unwrap_or_default();
        let mut judge_questions = Vec::new();
        let mut keymaps: HashMap<String, HashMap<String, String>> = HashMap::new();
        for result in &results {
            let mut candidates = Vec::new();
            let mut keymap = HashMap::new();
            // Deterministic per-question shuffle: the pool is built mode-by-mode (bm25 first), so
            // unshuffled keys would leak provenance and bias a position-sensitive judge. Seeded by
            // the question id for reproducibility.
            let mut order: Vec<usize> = (0..result.pool.len()).collect();
            let mut rng = XorShift64::new(eval_question_seed(&result.id));
            for i in (1..order.len()).rev() {
                let j = (rng.next_u64() % (i as u64 + 1)) as usize;
                order.swap(i, j);
            }
            for (slot, &pool_index) in order.iter().enumerate() {
                let candidate = &result.pool[pool_index];
                let key = format!("c{:02}", slot + 1);
                keymap.insert(key.clone(), candidate.uid.clone());
                candidates.push(json!({
                    "key": key,
                    "title": candidate.title,
                    "snippet": candidate.snippet,
                }));
            }
            judge_questions.push(json!({
                "question_id": result.id,
                "question": result.query,
                "candidates": candidates,
            }));
            keymaps.insert(result.id.clone(), keymap);
        }
        let judge_output = run_external_judge(command, &json!({ "questions": judge_questions }))?;
        for result in &mut results {
            let Some(per_key) = judge_output.get(&result.id).and_then(Value::as_object) else {
                continue;
            };
            let keymap = &keymaps[&result.id];
            for (key, label) in per_key {
                if let (Some(uid), Some(label)) = (keymap.get(key), label.as_i64()) {
                    result.labels.insert(uid.clone(), label);
                }
            }
        }
        judge_source = format!("external:{command}");
    }

    // 3. Score per metric per mode (per-question values, then mean).
    let mut per_question: HashMap<(String, &'static str), Vec<Option<f64>>> = HashMap::new();
    for spec in &metrics {
        for mode in &modes {
            let values: Vec<Option<f64>> = results
                .iter()
                .map(|result| {
                    // Relevance universe for recall/IDCG = pooled candidates UNION every labeled
                    // doc. For qrels this includes judged-relevant docs no retriever returned (so
                    // recall/nDCG can't look perfect when a retriever missed gold); for an external
                    // judge it equals the pool (the judge only labels pooled candidates).
                    let mut universe: HashSet<String> =
                        result.pool.iter().map(|candidate| candidate.uid.clone()).collect();
                    universe.extend(result.labels.keys().cloned());
                    let universe: Vec<String> = universe.into_iter().collect();
                    let empty = Vec::new();
                    let top = result.per_mode.get(mode.as_str()).unwrap_or(&empty);
                    compute_eval_metric(spec, top, &result.labels, &universe, args.rel_min)
                })
                .collect();
            per_question.insert((spec.name.clone(), mode.as_str()), values);
        }
    }

    let mut metrics_out = serde_json::Map::new();
    for mode in &modes {
        let mut mode_metrics = serde_json::Map::new();
        for spec in &metrics {
            let values = &per_question[&(spec.name.clone(), mode.as_str())];
            let value = mean_of(values).map(|v| (v * 1000.0).round() / 1000.0);
            mode_metrics.insert(
                spec.name.clone(),
                value.map(Value::from).unwrap_or(Value::Null),
            );
        }
        metrics_out.insert(mode.as_str().to_owned(), Value::Object(mode_metrics));
    }

    // 4. Optional bootstrap CIs for between-mode deltas on each metric.
    let bootstrap_out = if args.bootstrap > 0 && modes.len() >= 2 {
        let mut entries = Vec::new();
        for spec in &metrics {
            for i in 0..modes.len() {
                for j in (i + 1)..modes.len() {
                    let a = modes[i].as_str();
                    let b = modes[j].as_str();
                    let (point, lo, hi) = bootstrap_delta_ci(
                        &per_question[&(spec.name.clone(), a)],
                        &per_question[&(spec.name.clone(), b)],
                        args.bootstrap,
                    );
                    let round = |x: f64| (x * 1000.0).round() / 1000.0;
                    entries.push(json!({
                        "metric": spec.name,
                        "a": a,
                        "b": b,
                        "delta": round(point),
                        "ci_lo": round(lo),
                        "ci_hi": round(hi),
                        "significant": !(lo <= 0.0 && 0.0 <= hi),
                    }));
                }
            }
        }
        json!({ "resamples": args.bootstrap, "method": "question-resampled percentile", "deltas": entries })
    } else {
        Value::Null
    };

    let total_pool: usize = results.iter().map(|result| result.pool.len()).sum();
    let (env_lexical, env_dense) = rrf_weights();
    Ok(json!({
        "schema_version": SCHEMA_VERSION,
        "kind": "eval_run_benchmark",
        "questions": results.len(),
        "modes": modes.iter().map(|mode| mode.as_str()).collect::<Vec<_>>(),
        "group_by": "document",
        "top_k": args.top_k,
        "rel_min": args.rel_min,
        "judge": judge_source,
        "retrieval_options": {
            "rrf_lexical_weight": options.rrf_lexical_weight.unwrap_or(env_lexical),
            "rrf_dense_weight": options.rrf_dense_weight.unwrap_or(env_dense),
            "ivfflat_probes": options.ivfflat_probes.unwrap_or(4),
        },
        "pool": { "total_pairs": total_pool },
        "metrics": Value::Object(metrics_out),
        "bootstrap": bootstrap_out,
    }))
}

/// Sweep one hybrid retrieval parameter against a fixture and report the metric-maximizing value.
/// Re-runs `eval_run_payload` (hybrid only) per sweep point with request-scoped options.
fn eval_tune_payload(args: EvalTuneArgs, index_dir: Option<&Path>) -> Result<Value, ErrorObject> {
    let (param, range) = args.sweep.split_once('=').ok_or_else(|| {
        ErrorObject::bad_input("--sweep must be PARAM=start:stop:step (e.g. rrf-dense=0.1:1.5:0.1)")
    })?;
    let bounds: Vec<&str> = range.split(':').collect();
    if bounds.len() != 3 {
        return Err(ErrorObject::bad_input("--sweep range must be start:stop:step"));
    }
    let parse = |s: &str| -> Result<f64, ErrorObject> {
        s.trim()
            .parse::<f64>()
            .map_err(|_| ErrorObject::bad_input(format!("--sweep value `{s}` is not a number")))
    };
    let (start, stop, step) = (parse(bounds[0])?, parse(bounds[1])?, parse(bounds[2])?);
    if !start.is_finite() || !stop.is_finite() || !step.is_finite() {
        return Err(ErrorObject::bad_input("--sweep start/stop/step must be finite"));
    }
    if step <= 0.0 || stop < start {
        return Err(ErrorObject::bad_input(
            "--sweep requires step > 0 and stop >= start",
        ));
    }
    if !matches!(param, "rrf-dense" | "rrf-lexical" | "probes") {
        return Err(ErrorObject::bad_input(format!(
            "unknown sweep param `{param}`; expected rrf-dense, rrf-lexical, or probes"
        )));
    }
    if param == "probes" && [start, stop, step].iter().any(|value| value.fract() != 0.0) {
        return Err(ErrorObject::bad_input(
            "--sweep probes=start:stop:step requires integer start/stop/step",
        ));
    }
    if param == "probes" && start < 1.0 {
        return Err(ErrorObject::bad_input("--sweep probes start must be >= 1"));
    }

    let mut values = Vec::new();
    let mut value = start;
    while value <= stop + 1e-9 {
        values.push((value * 1e6).round() / 1e6);
        value += step;
    }
    if values.is_empty() {
        return Err(ErrorObject::bad_input("--sweep produced no values"));
    }

    let mut points = Vec::new();
    for value in &values {
        let options = match param {
            "rrf-dense" => RetrievalOptions {
                rrf_dense_weight: Some(*value),
                ..Default::default()
            },
            "rrf-lexical" => RetrievalOptions {
                rrf_lexical_weight: Some(*value),
                ..Default::default()
            },
            // probes
            _ => RetrievalOptions {
                ivfflat_probes: Some(value.max(1.0) as u32),
                ..Default::default()
            },
        };
        let run_args = EvalRunArgs {
            questions: args.questions.clone(),
            qrels: args.qrels.clone(),
            judge_cmd: args.judge_cmd.clone(),
            modes: "hybrid".to_owned(),
            metrics: args.metric.clone(),
            top_k: args.top_k,
            rel_min: args.rel_min,
            bootstrap: 0,
            out: None,
        };
        let result = eval_run_payload(run_args, options, index_dir)?;
        let metric_value = result["metrics"]["hybrid"][&args.metric].as_f64();
        points.push(json!({ "value": value, "metric": metric_value }));
    }

    let best = points
        .iter()
        .filter(|point| point["metric"].is_f64())
        .max_by(|a, b| {
            a["metric"]
                .as_f64()
                .partial_cmp(&b["metric"].as_f64())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .cloned()
        .unwrap_or(Value::Null);

    Ok(json!({
        "schema_version": SCHEMA_VERSION,
        "kind": "eval_tune",
        "mode": "hybrid",
        "sweep": { "param": param, "start": start, "stop": stop, "step": step },
        "metric": args.metric,
        "points": points,
        "best": best,
        "note": "Re-opens the index per sweep point; query-readiness is cached after the first."
    }))
}

/// Run the France-LEGI official-evidence benchmark over the production pipeline and assemble a
/// `phase1_france_legi_benchmark` artifact. Opens the index ONCE and runs every qrel through
/// `search_with_postgres` (single Postgres lifecycle). Gold comes from `france_legi_gold_json`
/// (no archive re-parse, no human/LLM).
fn eval_france_legi_payload(
    args: EvalFranceLegiArgs,
    index_dir: Option<&Path>,
) -> Result<Value, ErrorObject> {
    let index_dir = require_existing_index_dir(index_dir)?;
    let postgres = open_index(index_dir.as_path())?;
    // Verify query readiness ONCE for the whole sweep (the index is static during the run), so the
    // per-query searches can skip the expensive coverage re-count. The runner uses hybrid search,
    // which needs the dense `Search` readiness gate.
    ensure_query_readiness(&postgres, QueryReadinessGate::Search)?;

    let limits = FranceLegiGoldLimits {
        known_item: args.known_item,
        temporal: args.temporal,
        cross_reference: args.cross_reference,
    };
    let gold_json = france_legi_gold_json(&postgres, limits).map_err(storage_error_object)?;
    let gold: Value = serde_json::from_str(&gold_json)
        .map_err(|error| dependency_unavailable(error.to_string()))?;

    // Fixed at top-10 (document-level): the gate validates @10, so the runner must measure @10.
    let top_k = FRANCE_LEGI_GATE_TOP_K as usize;
    let overfetch = FRANCE_LEGI_GATE_TOP_K.saturating_mul(4);

    // Build the query embedder once for the whole sweep (the runner always uses hybrid/dense).
    let embedder = PreparedQueryEmbedder::from_env()?;

    // Each category runs its gold qrels through the production search pipeline and records which
    // routing backend served each query (the gate audit). known-item -> structured_citation_resolution
    // and temporal -> temporal_version_pinning resolve structurally; cross-reference is the advisory
    // semantic stress test (full body -> cited article, via hybrid).
    let mut known_hits = 0usize;
    let mut known_done = 0usize;
    let mut known_backends = std::collections::BTreeMap::<String, usize>::new();
    for qrel in gold["known_item"].as_array().into_iter().flatten() {
        let (Some(query), Some(gold_id), Some(as_of)) = (
            qrel["query"].as_str(),
            qrel["gold_document_id"].as_str(),
            qrel["as_of"].as_str(),
        ) else {
            continue;
        };
        let (docs, backend) =
            france_legi_search_documents(&postgres, &embedder, query, as_of, overfetch)?;
        *known_backends.entry(backend).or_default() += 1;
        known_done += 1;
        if docs.iter().take(top_k).any(|doc| doc == gold_id) {
            known_hits += 1;
        }
    }

    let mut temporal_hits = 0usize;
    let mut temporal_done = 0usize;
    let mut temporal_backends = std::collections::BTreeMap::<String, usize>::new();
    for qrel in gold["temporal"].as_array().into_iter().flatten() {
        let (Some(query), Some(gold_id), Some(as_of)) = (
            qrel["query"].as_str(),
            qrel["gold_document_id"].as_str(),
            qrel["as_of"].as_str(),
        ) else {
            continue;
        };
        let (docs, backend) =
            france_legi_search_documents(&postgres, &embedder, query, as_of, overfetch)?;
        *temporal_backends.entry(backend).or_default() += 1;
        temporal_done += 1;
        if docs.iter().take(top_k).any(|doc| doc == gold_id) {
            temporal_hits += 1;
        }
    }

    // cross-reference (advisory semantic): production search applies a temporal prefilter, so match
    // the cited ARTICLE (any version, by source_uid) rather than the exact cited version; as_of =
    // the citing article's own date.
    let mut cross_recall_sum = 0.0f64;
    let mut cross_done = 0usize;
    let mut cross_backends = std::collections::BTreeMap::<String, usize>::new();
    for qrel in gold["cross_reference"].as_array().into_iter().flatten() {
        let (Some(query), Some(query_doc), Some(gold_ids)) = (
            qrel["query"].as_str(),
            qrel["query_document_id"].as_str(),
            qrel["gold_document_ids"].as_array(),
        ) else {
            continue;
        };
        let gold_uids: Vec<String> = gold_ids
            .iter()
            .filter_map(|value| value.as_str().and_then(legi_source_uid_of).map(str::to_owned))
            .collect();
        if gold_uids.is_empty() {
            continue;
        }
        let as_of = legi_document_as_of(query_doc)
            .map(str::to_owned)
            .unwrap_or_else(today_utc);
        let (docs, backend) =
            france_legi_search_documents(&postgres, &embedder, query, &as_of, overfetch)?;
        *cross_backends.entry(backend).or_default() += 1;
        let top_uids: std::collections::HashSet<&str> = docs
            .iter()
            .take(top_k)
            .filter_map(|doc| legi_source_uid_of(doc))
            .collect();
        let matched = gold_uids
            .iter()
            .filter(|uid| top_uids.contains(uid.as_str()))
            .count();
        cross_recall_sum += matched as f64 / gold_uids.len() as f64;
        cross_done += 1;
    }

    let structured = FranceLegiCategoryResult {
        metric: mean(known_hits, known_done),
        queries: known_done,
        backends: json!(known_backends),
    };
    let temporal = FranceLegiCategoryResult {
        metric: mean(temporal_hits, temporal_done),
        queries: temporal_done,
        backends: json!(temporal_backends),
    };
    let semantic = FranceLegiCategoryResult {
        metric: if cross_done > 0 {
            cross_recall_sum / cross_done as f64
        } else {
            0.0
        },
        queries: cross_done,
        backends: json!(cross_backends),
    };

    let index_revision = index_dir
        .as_path()
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unknown".to_owned());
    let source_revision = args
        .source_revision
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| format!("index:{index_revision}"));

    Ok(france_legi_artifact(
        structured,
        temporal,
        semantic,
        limits,
        &index_revision,
        &source_revision,
    ))
}

/// One France-LEGI gate category: the @10 metric over its qrels, the query count, and the per-query
/// routing-backend audit (proving structured categories were resolved structurally, input-driven —
/// not because the evaluator knew the answer).
struct FranceLegiCategoryResult {
    metric: f64,
    queries: usize,
    backends: Value,
}

/// Assemble the `phase1_france_legi_benchmark` artifact from the three split-gate category results.
/// The two structured categories (citation resolution, temporal version pinning) GATE the claim at
/// high floors; `semantic_retrieval` is ADVISORY (recorded, never gating). `state` is `passed` only
/// when BOTH gating categories clear their floor + minimum query count; the status gate re-derives
/// pass from the recorded metrics either way.
fn france_legi_artifact(
    structured: FranceLegiCategoryResult,
    temporal: FranceLegiCategoryResult,
    semantic: FranceLegiCategoryResult,
    limits: FranceLegiGoldLimits,
    index_revision: &str,
    source_revision: &str,
) -> Value {
    let passed = structured.metric >= PHASE1_FRANCE_LEGI_MIN_STRUCTURED_CITATION_RECALL_AT_10
        && structured.queries as u64 >= PHASE1_FRANCE_LEGI_MIN_STRUCTURED_CITATION_QUERIES
        && temporal.metric >= PHASE1_FRANCE_LEGI_MIN_TEMPORAL_VERSION_EXACTNESS_AT_10
        && temporal.queries as u64 >= PHASE1_FRANCE_LEGI_MIN_TEMPORAL_QUERIES;

    json!({
        "schema_version": 1,
        "kind": "phase1_france_legi_benchmark",
        "state": if passed { "passed" } else { "failed" },
        "jurisdiction": "france",
        "claim_scope": "France-LEGI official-evidence retrieval with intent routing: structured citation resolution and temporal version pinning (gating), plus advisory full-body semantic retrieval, through the production pipeline",
        "source": "DILA LEGI (Licence Ouverte) official fields, extracted from the built index",
        "retriever": "jurisearch search (intent-routed: structured citation resolver + BM25/dense/RRF hybrid)",
        "embedding": {
            "fingerprint_model": PHASE0_EMBEDDING_MODEL,
            "dimension": PHASE0_EMBEDDING_DIMENSION,
            "normalize": true
        },
        "thresholds": {
            "structured_citation_recall_at_10_min": PHASE1_FRANCE_LEGI_MIN_STRUCTURED_CITATION_RECALL_AT_10,
            "temporal_version_exactness_at_10_min": PHASE1_FRANCE_LEGI_MIN_TEMPORAL_VERSION_EXACTNESS_AT_10,
            "semantic_retrieval_recall_at_10_advisory": PHASE1_FRANCE_LEGI_ADVISORY_SEMANTIC_RECALL_AT_10
        },
        "categories": {
            "structured_citation_resolution": {
                "metric_value": floor_metric(structured.metric),
                "queries": structured.queries,
                "gating": true,
                "routing_backends": structured.backends
            },
            "temporal_version_pinning": {
                "metric_value": floor_metric(temporal.metric),
                "queries": temporal.queries,
                "gating": true,
                "routing_backends": temporal.backends
            },
            "semantic_retrieval": {
                "metric_value": floor_metric(semantic.metric),
                "queries": semantic.queries,
                "gating": false,
                "advisory": true,
                "routing_backends": semantic.backends
            }
        },
        "provenance": {
            "official_source": "DILA LEGI (Licence Ouverte)",
            "source_revision": source_revision,
            "pipeline": "jurisearch search (intent-routed structured + hybrid)",
            // Record the exact fusion weights so the gate evidence is honest about the retrieval
            // configuration it measured (dense is down-weighted as a recall-expander).
            "fusion": {
                "rrf_lexical_weight": rrf_weights().0,
                "rrf_dense_weight": rrf_weights().1
            },
            "code_version": CLI_CODE_VERSION,
            "index_revision": index_revision,
            // The qrel set is a deterministic, reproducible ORDER BY + LIMIT bound (not random or
            // cherry-picked), so `sampled` is false; the per-category caps are recorded for audit.
            "qrel_selection": "deterministic_bounded_by_document_id",
            "qrel_limits": {
                "structured_citation_resolution": limits.known_item,
                "temporal_version_pinning": limits.temporal,
                "semantic_retrieval": limits.cross_reference
            },
            "sampled": false,
            "human_in_gold": false,
            "llm_in_gold": false
        },
        "evidence": [
            format!(
                "France-LEGI intent-routed runner over index `{index_revision}`: {} structured-citation, {} temporal, {} semantic (advisory) qrels through the production search pipeline",
                structured.queries, temporal.queries, semantic.queries
            )
        ]
    })
}

/// Run one France-LEGI query through the production search pipeline and return the ranked unique
/// document IDs plus the routing backend that served it (`structured_citation`/`hybrid`/`none`), for
/// the gate's routing audit. A `no_results` outcome is an empty list (a miss), not an error.
fn france_legi_search_documents(
    postgres: &ManagedPostgres,
    embedder: &PreparedQueryEmbedder,
    query: &str,
    as_of: &str,
    top_k: u32,
) -> Result<(Vec<String>, String), ErrorObject> {
    let Some(query_text) = parade_query_text(query) else {
        return Ok((Vec::new(), "none".to_owned()));
    };
    let args = SearchArgs {
        query: query.to_owned(),
        kind: CliKind::Code,
        mode: CliSearchMode::Hybrid,
        format: CliOutputFormat::Concise,
        group_by: CliGroupBy::Chunk,
        top_k,
        cursor: None,
        as_of: Some(as_of.to_owned()),
        rrf_lexical_weight: None,
        rrf_dense_weight: None,
        probes: None,
        court: None,
        formation: None,
        publication: None,
        decided_from: None,
        decided_to: None,
        zone: None,
    };
    let response = match search_with_postgres(
        postgres,
        &args,
        RetrievalMode::Hybrid,
        OutputFormat::Concise,
        None,
        &query_text,
        LegalKind::Code,
        // The runner verifies query readiness once before the loop, so skip the per-query check.
        false,
        // Reuse the embedder built once by the runner instead of rebuilding it per query.
        Some(embedder),
    ) {
        Ok(response) => response,
        Err(error) if error.code == ErrorCode::NoResults => {
            return Ok((Vec::new(), "none".to_owned()));
        }
        Err(error) => return Err(error),
    };
    let backend = response["routing"]["chosen_backend"]
        .as_str()
        .unwrap_or("unknown")
        .to_owned();
    let mut documents = Vec::new();
    if let Some(candidates) = response["candidates"].as_array() {
        for candidate in candidates {
            if let Some(document_id) = candidate["document_id"].as_str()
                && !documents.iter().any(|existing| existing == document_id)
            {
                documents.push(document_id.to_owned());
            }
        }
    }
    Ok((documents, backend))
}

/// `legi:LEGIARTI...@YYYY-MM-DD` -> `LEGIARTI...`
fn legi_source_uid_of(document_id: &str) -> Option<&str> {
    document_id.strip_prefix("legi:")?.split('@').next()
}

/// `legi:LEGIARTI...@YYYY-MM-DD` -> `YYYY-MM-DD`
fn legi_document_as_of(document_id: &str) -> Option<&str> {
    document_id.rsplit_once('@').map(|(_, date)| date)
}

fn mean(hits: usize, total: usize) -> f64 {
    if total > 0 {
        hits as f64 / total as f64
    } else {
        0.0
    }
}

/// Truncate (floor) a gate metric to 3 decimals for the artifact. Flooring, not rounding, so the
/// RECORDED metric can never exceed the raw value: the status gate re-derives pass from the recorded
/// 3-decimal `metric_value` against a 3-decimal floor, and `floor(raw*1000) >= floor*1000` holds iff
/// `raw >= floor`, so the recorded value passes exactly when the runner's raw decision passes (a
/// below-floor raw metric can never round up into a passing recorded value).
fn floor_metric(value: f64) -> f64 {
    (value * 1000.0).floor() / 1000.0
}

/// One Phase-2 jurisprudence benchmark category: the @10 / accuracy metric over its qrels and the
/// query count.
struct FranceJurisCategoryResult {
    metric: f64,
    queries: usize,
}

/// Run the France-jurisprudence benchmark and emit the `phase2_france_juris_benchmark` artifact.
/// Opens the index ONCE; runs retrieval qrels through `search_with_postgres` (Hybrid, kind=decision)
/// and citation qrels through the same `citation_lookup_json` path as CLI `cite`. Gold comes from
/// `france_juris_gold_json` (official indexed fields; NO archive re-parse, NO human/LLM).
fn eval_france_juris_payload(
    args: EvalFranceJurisArgs,
    index_dir: Option<&Path>,
) -> Result<Value, ErrorObject> {
    let index_dir = require_existing_index_dir(index_dir)?;
    let postgres = open_index(index_dir.as_path())?;
    ensure_query_readiness(&postgres, QueryReadinessGate::Search)?;

    let limits = FranceJurisGoldLimits {
        judicial_retrieval: args.judicial_retrieval,
        administrative_retrieval: args.administrative_retrieval,
        ecli: args.ecli,
        pourvoi: args.pourvoi,
        cetatext: args.cetatext,
    };
    let gold_json = france_juris_gold_json(&postgres, limits).map_err(storage_error_object)?;
    let gold: Value = serde_json::from_str(&gold_json)
        .map_err(|error| dependency_unavailable(error.to_string()))?;

    // Fixed at top-10 (document-level): the gate validates recall@10, so the runner must measure @10.
    let top_k = 10u32;
    let overfetch = top_k.saturating_mul(4);
    let embedder = PreparedQueryEmbedder::from_env()?;

    let judicial = france_juris_retrieval_category(
        &postgres,
        &embedder,
        &gold["judicial_retrieval"],
        top_k,
        overfetch,
    )?;
    let administrative = france_juris_retrieval_category(
        &postgres,
        &embedder,
        &gold["administrative_retrieval"],
        top_k,
        overfetch,
    )?;
    let ecli = france_juris_citation_category(&postgres, &gold["decision_citation"]["ecli"])?;
    let pourvoi = france_juris_citation_category(&postgres, &gold["decision_citation"]["pourvoi"])?;
    let cetatext =
        france_juris_citation_category(&postgres, &gold["decision_citation"]["cetatext"])?;

    let index_revision = france_juris_index_revision(&postgres).map_err(storage_error_object)?;
    let source_revision = args
        .source_revision
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| format!("index:{index_revision}"));

    Ok(france_juris_artifact(
        judicial,
        administrative,
        ecli,
        pourvoi,
        cetatext,
        limits,
        &index_revision,
        &source_revision,
    ))
}

/// Retrieval category: recall@10 over known-item qrels through the production hybrid search,
/// restricted to `kind=decision`.
fn france_juris_retrieval_category(
    postgres: &ManagedPostgres,
    embedder: &PreparedQueryEmbedder,
    qrels: &Value,
    top_k: u32,
    overfetch: u32,
) -> Result<FranceJurisCategoryResult, ErrorObject> {
    let mut hits = 0usize;
    let mut done = 0usize;
    for qrel in qrels.as_array().into_iter().flatten() {
        let (Some(query), Some(gold_id)) =
            (qrel["query"].as_str(), qrel["gold_document_id"].as_str())
        else {
            continue;
        };
        let docs = france_juris_search_documents(postgres, embedder, query, overfetch)?;
        done += 1;
        if docs.iter().take(top_k as usize).any(|doc| doc == gold_id) {
            hits += 1;
        }
    }
    Ok(FranceJurisCategoryResult {
        metric: mean(hits, done),
        queries: done,
    })
}

/// Run one decision query through the production search pipeline (Hybrid, kind=decision) and return
/// the ranked UNIQUE decision document ids. Errors if a non-decision candidate is returned: the
/// `kind=decision` filter must hold for the benchmark to be an honest judicial/administrative measure.
fn france_juris_search_documents(
    postgres: &ManagedPostgres,
    embedder: &PreparedQueryEmbedder,
    query: &str,
    top_k: u32,
) -> Result<Vec<String>, ErrorObject> {
    let Some(query_text) = parade_query_text(query) else {
        return Ok(Vec::new());
    };
    let args = SearchArgs {
        query: query.to_owned(),
        kind: CliKind::Decision,
        mode: CliSearchMode::Hybrid,
        format: CliOutputFormat::Concise,
        group_by: CliGroupBy::Document,
        top_k,
        cursor: None,
        as_of: None,
        rrf_lexical_weight: None,
        rrf_dense_weight: None,
        probes: None,
        court: None,
        formation: None,
        publication: None,
        decided_from: None,
        decided_to: None,
        zone: None,
    };
    let response = match search_with_postgres(
        postgres,
        &args,
        RetrievalMode::Hybrid,
        OutputFormat::Concise,
        None,
        &query_text,
        LegalKind::Decision,
        false,
        Some(embedder),
    ) {
        Ok(response) => response,
        Err(error) if error.code == ErrorCode::NoResults => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut documents = Vec::new();
    if let Some(candidates) = response["candidates"].as_array() {
        for candidate in candidates {
            if candidate["kind"].as_str() != Some("decision") {
                return Err(dependency_unavailable(
                    "france-juris retrieval returned a non-decision candidate; the kind=decision filter is not holding".to_owned(),
                ));
            }
            if let Some(document_id) = candidate["document_id"].as_str()
                && !documents.iter().any(|existing| existing == document_id)
            {
                documents.push(document_id.to_owned());
            }
        }
    }
    Ok(documents)
}

/// Citation category: decision_citation_accuracy over identifier qrels, resolved through the SAME
/// production citation path as CLI `cite` (`citation_lookup_json`). A qrel is a hit when the gold
/// document is among the resolved matches.
fn france_juris_citation_category(
    postgres: &ManagedPostgres,
    qrels: &Value,
) -> Result<FranceJurisCategoryResult, ErrorObject> {
    let mut hits = 0usize;
    let mut done = 0usize;
    for qrel in qrels.as_array().into_iter().flatten() {
        let (Some(query), Some(gold_id)) =
            (qrel["query"].as_str(), qrel["gold_document_id"].as_str())
        else {
            continue;
        };
        let docs = france_juris_cite_documents(postgres, query)?;
        done += 1;
        if docs.iter().any(|doc| doc == gold_id) {
            hits += 1;
        }
    }
    Ok(FranceJurisCategoryResult {
        metric: mean(hits, done),
        queries: done,
    })
}

/// Resolve one citation identifier through the production `citation_lookup_json` path and return the
/// matched document ids.
fn france_juris_cite_documents(
    postgres: &ManagedPostgres,
    query: &str,
) -> Result<Vec<String>, ErrorObject> {
    let parsed = parse_citation_target(query);
    let Some(lookup) = parsed.lookup() else {
        return Ok(Vec::new());
    };
    let response = citation_lookup_json(postgres, &CitationLookupQuery { lookup, limit: 25 })
        .map_err(storage_error_object)?;
    let parsed_response: Value = serde_json::from_str(&response)
        .map_err(|error| dependency_unavailable(error.to_string()))?;
    let mut documents = Vec::new();
    if let Some(matches) = parsed_response["matches"].as_array() {
        for entry in matches {
            if let Some(document_id) = entry["document_id"].as_str() {
                documents.push(document_id.to_owned());
            }
        }
    }
    Ok(documents)
}

/// Assemble the `phase2_france_juris_benchmark` artifact in the exact shape the Phase 2 gate
/// re-derives (`phase2_benchmark_artifact_errors`): category `metric`/`value`/`queries`,
/// `decision_citation.by_identifier`, and production provenance. Metrics are floored to 3 decimals so
/// the RECORDED value can never exceed the measured one; the gate re-derives pass/fail from the fields.
fn france_juris_artifact(
    judicial: FranceJurisCategoryResult,
    administrative: FranceJurisCategoryResult,
    ecli: FranceJurisCategoryResult,
    pourvoi: FranceJurisCategoryResult,
    cetatext: FranceJurisCategoryResult,
    limits: FranceJurisGoldLimits,
    index_revision: &str,
    source_revision: &str,
) -> Value {
    let citation_pass = |category: &FranceJurisCategoryResult| {
        floor_metric(category.metric) >= PHASE2_MIN_DECISION_CITATION_ACCURACY
            && category.queries as u64 >= PHASE2_MIN_CITATION_QUERIES_PER_IDENTIFIER
    };
    let passed = floor_metric(judicial.metric) >= PHASE2_MIN_RETRIEVAL_RECALL_AT_10
        && judicial.queries as u64 >= PHASE2_MIN_JUDICIAL_RETRIEVAL_QUERIES
        && floor_metric(administrative.metric) >= PHASE2_MIN_RETRIEVAL_RECALL_AT_10
        && administrative.queries as u64 >= PHASE2_MIN_ADMINISTRATIVE_RETRIEVAL_QUERIES
        && citation_pass(&ecli)
        && citation_pass(&pourvoi)
        && citation_pass(&cetatext);

    let citation_category = |category: &FranceJurisCategoryResult| {
        json!({
            "metric": "decision_citation_accuracy",
            "value": floor_metric(category.metric),
            "queries": category.queries
        })
    };

    json!({
        "schema_version": 1,
        "kind": "phase2_france_juris_benchmark",
        "state": if passed { "passed" } else { "failed" },
        "jurisdiction": "france",
        "fingerprint": "bge-m3:1024:normalize:true",
        "claim_scope": "full French juridic search (statutes + jurisprudence): judicial (Cassation/appeal) AND administrative retrieval AND ECLI/pourvoi/CETATEXT decision-citation verification, through the production pipeline",
        "source": "DILA CASS/CAPP/INCA/JADE bulk XML (Licence Ouverte) official fields, extracted from the built index",
        "retriever": "jurisearch search (hybrid BM25/dense/RRF, kind=decision) + citation resolver",
        "categories": {
            "judicial_retrieval": {
                "metric": "recall_at_10",
                "value": floor_metric(judicial.metric),
                "queries": judicial.queries
            },
            "administrative_retrieval": {
                "metric": "recall_at_10",
                "value": floor_metric(administrative.metric),
                "queries": administrative.queries
            },
            "decision_citation": {
                "metric": "decision_citation_accuracy",
                "by_identifier": {
                    "ecli": citation_category(&ecli),
                    "pourvoi": citation_category(&pourvoi),
                    "cetatext": citation_category(&cetatext)
                }
            }
        },
        "provenance": {
            "official_source": "DILA CASS/CAPP/INCA/JADE bulk XML (Licence Ouverte), extracted from the built index",
            "pipeline": PHASE2_PRODUCTION_PIPELINE,
            "code_version": CLI_CODE_VERSION,
            "index_revision": index_revision,
            "source_revision": source_revision,
            "qrel_selection": "deterministic_bounded_by_document_id_from_official_index_fields",
            "qrel_limits": {
                "judicial_retrieval": limits.judicial_retrieval,
                "administrative_retrieval": limits.administrative_retrieval,
                "ecli": limits.ecli,
                "pourvoi": limits.pourvoi,
                "cetatext": limits.cetatext
            },
            "sampled": false,
            "human_in_gold": false,
            "llm_in_gold": false,
            "pseudonymisation": "preserved: gold and identifiers come from the pseudonymised official bulk fields; no re-identification, no cross-source linking"
        },
        "evidence": [
            format!(
                "France-jurisprudence runner over index `{index_revision}`: {} judicial + {} administrative retrieval recall@10, {} ECLI / {} pourvoi / {} CETATEXT citation-accuracy qrels through the production search/cite pipeline",
                judicial.queries, administrative.queries, ecli.queries, pourvoi.queries, cetatext.queries
            )
        ],
        "reason": if passed {
            "all Phase 2 categories cleared their floors through the production pipeline"
        } else {
            "one or more Phase 2 categories did not clear the floor or minimum query count"
        }
    })
}

/// Run the SEPARATE official-zone retrieval benchmark and emit the `phase2_zone_benchmark` artifact
/// (Z5/T5.2). Measures recall@10 of `search --zone <zone>` over the parallel `zone_units` subsystem;
/// gold = an identifier-stripped excerpt of a decision's OFFICIAL zone text → the source decision.
/// MEASURED-ONLY: it is NOT a Phase 2 gate input and its artifact (distinct `kind`, distinct `--out`)
/// never inflates the full-juridic corpus claim. Opens the index ONCE; gates on `zone` readiness (not
/// the chunk corpus), so it works independently of whether the bulk chunk index is query-ready.
fn eval_france_juris_zones_payload(
    args: EvalFranceJurisZonesArgs,
    index_dir: Option<&Path>,
) -> Result<Value, ErrorObject> {
    let index_dir = require_existing_index_dir(index_dir)?;
    let postgres = open_index(index_dir.as_path())?;

    let retrieval_mode: RetrievalMode = args.mode.into();
    let needs_dense = retrieval_mode.uses_dense();
    // Reject a zone dense index finalized under a different embedder before running queries that would
    // match nothing — and gate on the ZONE subsystem only (independent of chunk readiness).
    let expected_fingerprint =
        needs_dense.then(|| embedding_config_from_env().storage_embedding_fingerprint());
    ensure_zone_retrieval_readiness(&postgres, needs_dense, expected_fingerprint.as_deref())?;

    let limits = FranceJurisZoneGoldLimits {
        motivations: args.motivations,
        moyens: args.moyens,
        dispositif: args.dispositif,
    };
    let gold_json =
        france_juris_zone_gold_json(&postgres, limits).map_err(storage_error_object)?;
    let gold: Value = serde_json::from_str(&gold_json)
        .map_err(|error| dependency_unavailable(error.to_string()))?;

    let top_k = 10u32;
    let embedder = needs_dense
        .then(PreparedQueryEmbedder::from_env)
        .transpose()?;

    let mut categories = serde_json::Map::new();
    for zone in [CliZone::Motivations, CliZone::Moyens, CliZone::Dispositif] {
        let result = france_juris_zone_retrieval_category(
            &postgres,
            embedder.as_ref(),
            retrieval_mode,
            zone,
            &gold[zone.as_str()],
            top_k,
        )?;
        categories.insert(
            zone.as_str().to_owned(),
            zone_benchmark_category(&result, args.floor),
        );
    }

    let index_revision = france_juris_index_revision(&postgres).map_err(storage_error_object)?;
    let source_revision = args
        .source_revision
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| format!("index:{index_revision}"));

    Ok(zone_benchmark_artifact(
        Value::Object(categories),
        retrieval_mode,
        needs_dense,
        expected_fingerprint.as_deref(),
        args.floor,
        limits,
        &index_revision,
        &source_revision,
    ))
}

/// One zone-retrieval category: recall@10 over the zone's known-item qrels through the official-zone
/// search path (`zone_candidates_json`), restricted to that zone.
fn france_juris_zone_retrieval_category(
    postgres: &ManagedPostgres,
    embedder: Option<&PreparedQueryEmbedder>,
    retrieval_mode: RetrievalMode,
    zone: CliZone,
    qrels: &Value,
    top_k: u32,
) -> Result<FranceJurisCategoryResult, ErrorObject> {
    let mut hits = 0usize;
    let mut done = 0usize;
    for qrel in qrels.as_array().into_iter().flatten() {
        let (Some(query), Some(gold_id)) =
            (qrel["query"].as_str(), qrel["gold_document_id"].as_str())
        else {
            continue;
        };
        let docs =
            france_juris_zone_search_documents(postgres, embedder, retrieval_mode, zone, query, top_k)?;
        done += 1;
        if docs.iter().take(top_k as usize).any(|doc| doc == gold_id) {
            hits += 1;
        }
    }
    Ok(FranceJurisCategoryResult {
        metric: mean(hits, done),
        queries: done,
    })
}

/// Run one zone query through the official-zone retrieval path (`zone_candidates_json`, grouped by
/// decision) and return the ranked UNIQUE decision document ids. Mirrors
/// [`france_juris_search_documents`] but on the zone subsystem; reuses the already-open index (no second
/// `open_index`). Errors if a candidate is not zone-accurate or is in the wrong zone — the zone scope
/// must hold for the benchmark to be honest.
fn france_juris_zone_search_documents(
    postgres: &ManagedPostgres,
    embedder: Option<&PreparedQueryEmbedder>,
    retrieval_mode: RetrievalMode,
    zone: CliZone,
    query: &str,
    top_k: u32,
) -> Result<Vec<String>, ErrorObject> {
    let Some(query_text) = parade_query_text(query) else {
        return Ok(Vec::new());
    };
    let (query_embedding, embedding_fingerprint) = match embedder {
        Some(embedder) => {
            let (literal, fingerprint) = embedder.embed(query)?;
            (Some(literal), Some(fingerprint))
        }
        None => (None, None),
    };
    let response = zone_candidates_json(
        postgres,
        &ZoneCandidateQuery {
            query_text: query_text.as_str(),
            query_embedding: query_embedding.as_deref(),
            embedding_fingerprint: embedding_fingerprint.as_deref(),
            retrieval_mode,
            options: RetrievalOptions::default(),
            after_cursor: None,
            zone: zone.as_str(),
            as_of: &today_utc(),
            decision_filters: DecisionFilters::default(),
            lexical_limit: top_k.saturating_mul(20),
            dense_limit: top_k.saturating_mul(20),
            limit: top_k,
        },
    )
    .map_err(storage_error_object)?;
    let response: Value = serde_json::from_str(&response)
        .map_err(|error| dependency_unavailable(error.to_string()))?;
    let mut documents = Vec::new();
    if let Some(candidates) = response["candidates"].as_array() {
        for candidate in candidates {
            if candidate["zone"].as_str() != Some(zone.as_str())
                || candidate["zone_accurate"].as_bool() != Some(true)
            {
                return Err(dependency_unavailable(
                    "zone retrieval returned an off-zone or non-zone-accurate candidate; the zone scope is not holding".to_owned(),
                ));
            }
            if let Some(document_id) = candidate["document_id"].as_str()
                && !documents.iter().any(|existing| existing == document_id)
            {
                documents.push(document_id.to_owned());
            }
        }
    }
    Ok(documents)
}

/// One `phase2_zone_benchmark` category: measured recall@10 + whether it meets the PROPOSED floor.
/// A zone with no qrels reports `value:null, queries:0` (skipped/empty) and is excluded from the floor
/// verdict — never a misleading 0.0.
fn zone_benchmark_category(result: &FranceJurisCategoryResult, floor: f64) -> Value {
    if result.queries == 0 {
        return json!({
            "metric": "recall_at_10",
            "value": null,
            "queries": 0,
            "meets_proposed_floor": null
        });
    }
    let value = floor_metric(result.metric);
    json!({
        "metric": "recall_at_10",
        "value": value,
        "queries": result.queries,
        "meets_proposed_floor": value >= floor
    })
}

/// Assemble the `phase2_zone_benchmark` artifact. MEASURED-ONLY: `state:"measured"` (never a
/// pass/fail gate), records each zone's measured recall@10 against the PROPOSED floor, and is scoped to
/// the Cassation-only zone overlay so it can never inflate the full-juridic corpus claim. The recorded
/// `fingerprint` is the ACTUAL dense fingerprint used (`None` → `null` for a lexical-only BM25 run), so
/// the artifact's provenance never claims an embedder it did not use.
#[allow(clippy::too_many_arguments)]
fn zone_benchmark_artifact(
    categories: Value,
    retrieval_mode: RetrievalMode,
    uses_dense: bool,
    fingerprint: Option<&str>,
    proposed_floor: f64,
    limits: FranceJurisZoneGoldLimits,
    index_revision: &str,
    source_revision: &str,
) -> Value {
    // Advisory only: do all the zones that actually had qrels meet the proposed floor?
    let measured: Vec<&Value> = categories
        .as_object()
        .into_iter()
        .flat_map(|map| map.values())
        .filter(|category| category["queries"].as_u64().unwrap_or(0) > 0)
        .collect();
    let all_meet_proposed_floor = !measured.is_empty()
        && measured
            .iter()
            .all(|category| category["meets_proposed_floor"].as_bool() == Some(true));

    json!({
        "schema_version": 1,
        "kind": "phase2_zone_benchmark",
        "state": "measured",
        "gate_input": false,
        "jurisdiction": "france",
        "uses_dense": uses_dense,
        "fingerprint": fingerprint,
        "claim_scope": "official Cour de cassation zone retrieval (cass+inca) ONLY — a coverage-bounded overlay, NOT corpus-wide French juridic search; this benchmark is measured-only and is NOT an input to the Phase 2 full-juridic gate",
        "source": "official Judilibre decision zones (motivations/moyens/dispositif) materialized as zone_units, extracted from the built index",
        "retriever": format!("jurisearch search --zone (zone_units {} retrieval)", retrieval_mode.as_str()),
        "retrieval_mode": retrieval_mode.as_str(),
        "proposed_floor": proposed_floor,
        "all_meet_proposed_floor": all_meet_proposed_floor,
        "categories": categories,
        "provenance": {
            "official_source": "Judilibre official decision zones (Cour de cassation), materialized as zone_units from the built index",
            "pipeline": "jurisearch search --zone (official_zone_retrieval) over zone_units / zone_unit_embeddings / zone_units_bm25_idx",
            "code_version": CLI_CODE_VERSION,
            "index_revision": index_revision,
            "source_revision": source_revision,
            "qrel_selection": "deterministic_first_fragment_per_decision_by_document_id_from_official_zone_units",
            "qrel_limits": {
                "motivations": limits.motivations,
                "moyens": limits.moyens,
                "dispositif": limits.dispositif
            },
            "sampled": false,
            "human_in_gold": false,
            "llm_in_gold": false,
            "pseudonymisation": "preserved: gold comes from the pseudonymised official Judilibre zone fields; no re-identification, no cross-source linking"
        },
        "reason": "measured-only official-zone retrieval recall@10; the proposed floor is advisory (calibrate on the first clone run), never asserted as a gate"
    })
}

pub(crate) fn eval_phase1_payload(
    args: EvalPhase1Args,
    index_dir: Option<&Path>,
) -> Result<Value, ErrorObject> {
    if !args.list && args.top_k == 0 {
        return Err(ErrorObject::bad_input(
            "eval phase1 --top-k must be at least 1 when executing fixtures",
        ));
    }

    let fixtures = selected_phase1_eval_fixtures(args.include_dev);
    let fixture_summary = phase1_eval_fixture_summary();
    if args.list {
        return Ok(json!({
            "schema_version": SCHEMA_VERSION,
            "command": "eval phase1",
            "action": "list",
            "include_dev": args.include_dev,
            "fixture_count": fixtures.len(),
            "eval_fixtures": fixture_summary,
            "fixtures": fixtures,
        }));
    }

    let mut results = Vec::with_capacity(fixtures.len());
    for fixture in &fixtures {
        results.push(eval_phase1_fixture_result(
            fixture, args.mode, args.top_k, index_dir,
        )?);
    }
    let passed = results
        .iter()
        .filter(|result| result["passed"].as_bool() == Some(true))
        .count();
    let failed = results.len().saturating_sub(passed);
    let retrieval_mode: RetrievalMode = args.mode.into();

    Ok(json!({
        "schema_version": SCHEMA_VERSION,
        "command": "eval phase1",
        "action": "run",
        "include_dev": args.include_dev,
        "retrieval_mode": retrieval_mode.as_str(),
        "top_k": args.top_k,
        "eval_fixtures": fixture_summary,
        "summary": {
            "fixture_count": results.len(),
            "passed": passed,
            "failed": failed,
            "all_passed": failed == 0,
        },
        "results": results,
    }))
}

fn selected_phase1_eval_fixtures(include_dev: bool) -> Vec<LegalRetrievalFixture> {
    if include_dev {
        phase1_eval_fixtures()
    } else {
        phase1_release_candidate_fixtures()
    }
}

fn eval_phase1_fixture_result(
    fixture: &LegalRetrievalFixture,
    mode: CliSearchMode,
    top_k: u32,
    index_dir: Option<&Path>,
) -> Result<Value, ErrorObject> {
    let search_result = search_payload(
        SearchArgs {
            query: fixture.query.clone(),
            kind: CliKind::Code,
            mode,
            format: CliOutputFormat::Detailed,
            group_by: CliGroupBy::Chunk,
            top_k,
            cursor: None,
            as_of: fixture.as_of.clone(),
            rrf_lexical_weight: None,
            rrf_dense_weight: None,
            probes: None,
            court: None,
            formation: None,
            publication: None,
            decided_from: None,
            decided_to: None,
            zone: None,
        },
        index_dir,
    );

    match search_result {
        Ok(search) => Ok(eval_phase1_fixture_search_result(fixture, search)),
        Err(error) if error.code == ErrorCode::NoResults => Ok(json!({
            "id": fixture.id.as_str(),
            "tier": &fixture.tier,
            "category": fixture.category.as_str(),
            "query": fixture.query.as_str(),
            "as_of": fixture.as_of.as_deref(),
            "expected_ids": &fixture.expected_ids,
            "allowed_alternates": &fixture.allowed_alternates,
            "status": "fail",
            "passed": false,
            "best_expected_rank": null,
            "best_allowed_alternate_rank": null,
            "matched_document_id": null,
            "candidate_count": 0,
            "top_document_ids": [],
            "error": error,
        })),
        Err(error) => Err(error),
    }
}

fn eval_phase1_fixture_search_result(fixture: &LegalRetrievalFixture, search: Value) -> Value {
    let expected_ids = fixture
        .expected_ids
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let allowed_alternates = fixture
        .allowed_alternates
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let candidates = search["candidates"].as_array().cloned().unwrap_or_default();
    let mut top_document_ids = Vec::with_capacity(candidates.len());
    let mut best_expected_rank = None::<usize>;
    let mut best_allowed_alternate_rank = None::<usize>;
    let mut matched_document_id = None::<String>;

    for candidate in &candidates {
        let Some(document_id) = candidate["document_id"].as_str() else {
            continue;
        };
        top_document_ids.push(document_id.to_owned());
        let rank = top_document_ids.len();
        if best_expected_rank.is_none() && expected_ids.contains(document_id) {
            best_expected_rank = Some(rank);
            matched_document_id = Some(document_id.to_owned());
        }
        if best_allowed_alternate_rank.is_none() && allowed_alternates.contains(document_id) {
            best_allowed_alternate_rank = Some(rank);
            matched_document_id.get_or_insert_with(|| document_id.to_owned());
        }
    }

    let status = if best_expected_rank.is_some() {
        "pass"
    } else if best_allowed_alternate_rank.is_some() {
        "pass_allowed_alternate"
    } else {
        "fail"
    };

    json!({
        "id": fixture.id.as_str(),
        "tier": &fixture.tier,
        "category": fixture.category.as_str(),
        "query": fixture.query.as_str(),
        "as_of": fixture.as_of.as_deref(),
        "expected_ids": &fixture.expected_ids,
        "allowed_alternates": &fixture.allowed_alternates,
        "status": status,
        "passed": status != "fail",
        "best_expected_rank": best_expected_rank,
        "best_allowed_alternate_rank": best_allowed_alternate_rank,
        "matched_document_id": matched_document_id,
        "candidate_count": candidates.len(),
        "top_document_ids": top_document_ids,
        "search": {
            "retrieval_mode": search["retrieval_mode"].clone(),
            "pagination": search["pagination"].clone(),
            "diagnostics": search["diagnostics"]["retrieval"].clone(),
        }
    })
}

pub(crate) fn emit_model(args: ModelCommand) -> anyhow::Result<()> {
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

pub(crate) fn emit_help(help: HelpCommand) -> anyhow::Result<()> {
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

pub(crate) fn emit_ingest(ingest: IngestCommand, index_dir: Option<&Path>) -> anyhow::Result<()> {
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
                ArchiveSyncFilter::default(),
            ) {
                Ok(response) => write_json(&response),
                Err(error) => emit_error(error),
            }
        }
        Some(IngestSubcommand::JuriArchives {
            source,
            archives_dir,
            run_id,
            limit_members,
            max_member_bytes,
            quarantine_dir,
            safe_mode,
        }) => {
            if limit_members == Some(0) {
                return emit_error(ErrorObject::bad_input(
                    "ingest juri-archives --limit-members must be at least 1 when provided",
                ));
            }
            if max_member_bytes == 0 {
                return emit_error(ErrorObject::bad_input(
                    "ingest juri-archives --max-member-bytes must be at least 1",
                ));
            }
            match ingest_juri_archives_payload(
                index_dir,
                ArchiveSource::from(source),
                archives_dir.as_path(),
                run_id,
                limit_members,
                max_member_bytes,
                quarantine_dir.as_deref(),
                safe_mode,
                ArchiveSyncFilter::default(),
            ) {
                Ok(response) => write_json(&response),
                Err(error) => emit_error(error),
            }
        }
        Some(IngestSubcommand::EmbedChunks {
            limit,
            index_lists,
            batch_size,
            pool_concurrency,
        }) => {
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
            if batch_size == 0 {
                return emit_error(ErrorObject::bad_input(
                    "ingest embed-chunks --batch-size must be at least 1",
                ));
            }
            if pool_concurrency == 0 {
                return emit_error(ErrorObject::bad_input(
                    "ingest embed-chunks --pool-concurrency must be at least 1",
                ));
            }
            match embed_chunks_payload(index_dir, limit, index_lists, batch_size, pool_concurrency)
            {
                Ok(response) => write_json(&response),
                Err(error) => emit_error(error),
            }
        }
        Some(IngestSubcommand::EnrichZones {
            source,
            limit,
            since,
            concurrency,
            order,
        }) => {
            if limit == Some(0) {
                return emit_error(ErrorObject::bad_input(
                    "ingest enrich-zones --limit must be at least 1 when provided",
                ));
            }
            if concurrency == 0 {
                return emit_error(ErrorObject::bad_input(
                    "ingest enrich-zones --concurrency must be at least 1",
                ));
            }
            match enrich_zones_payload(
                index_dir,
                &source,
                limit,
                since.as_deref(),
                concurrency,
                order,
            ) {
                Ok(response) => write_json(&response),
                Err(error) => emit_error(error),
            }
        }
        Some(IngestSubcommand::BuildZoneUnits { limit, rebuild }) => {
            if limit == Some(0) {
                return emit_error(ErrorObject::bad_input(
                    "ingest build-zone-units --limit must be at least 1 when provided",
                ));
            }
            match build_zone_units_payload(index_dir, limit, rebuild) {
                Ok(response) => write_json(&response),
                Err(error) => emit_error(error),
            }
        }
        Some(IngestSubcommand::EmbedZoneUnits {
            limit,
            index_lists,
            batch_size,
            pool_concurrency,
        }) => {
            if limit == Some(0) {
                return emit_error(ErrorObject::bad_input(
                    "ingest embed-zone-units --limit must be at least 1 when provided",
                ));
            }
            if index_lists == 0 {
                return emit_error(ErrorObject::bad_input(
                    "ingest embed-zone-units --index-lists must be at least 1",
                ));
            }
            if batch_size == 0 {
                return emit_error(ErrorObject::bad_input(
                    "ingest embed-zone-units --batch-size must be at least 1",
                ));
            }
            if pool_concurrency == 0 {
                return emit_error(ErrorObject::bad_input(
                    "ingest embed-zone-units --pool-concurrency must be at least 1",
                ));
            }
            match embed_zone_units_payload(index_dir, limit, index_lists, batch_size, pool_concurrency)
            {
                Ok(response) => write_json(&response),
                Err(error) => emit_error(error),
            }
        }
        Some(IngestSubcommand::CollectLegislationCitations { limit }) => {
            if limit == Some(0) {
                return emit_error(ErrorObject::bad_input(
                    "ingest collect-legislation-citations --limit must be at least 1 when provided",
                ));
            }
            match collect_legislation_citations_payload(index_dir, limit) {
                Ok(response) => write_json(&response),
                Err(error) => emit_error(error),
            }
        }
        Some(IngestSubcommand::EnrichLegislationCitations { limit, retry_errors }) => {
            if limit == Some(0) {
                return emit_error(ErrorObject::bad_input(
                    "ingest enrich-legislation-citations --limit must be at least 1 when provided",
                ));
            }
            match enrich_legislation_citations_payload(index_dir, limit, retry_errors) {
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

impl LegiArchiveIngestCounters {
    fn merge_committed(&mut self, committed: Self) {
        self.inserted_documents += committed.inserted_documents;
        self.inserted_chunks += committed.inserted_chunks;
        self.inserted_publisher_edges += committed.inserted_publisher_edges;
        self.parsed_metadata_members += committed.parsed_metadata_members;
        self.persisted_metadata_members += committed.persisted_metadata_members;
        self.skipped_members += committed.skipped_members;
        self.skipped_compatible_members += committed.skipped_compatible_members;
        self.skipped_no_text_articles += committed.skipped_no_text_articles;
        self.failed_members += committed.failed_members;
        self.quarantined_payloads += committed.quarantined_payloads;
        for (root, count) in committed.parsed_metadata_roots {
            *self.parsed_metadata_roots.entry(root).or_default() += count;
        }
        for (root, count) in committed.unsupported_roots {
            *self.unsupported_roots.entry(root).or_default() += count;
        }
        self.processed_article_document_ids
            .extend(committed.processed_article_document_ids);
        self.processed_section_source_uids
            .extend(committed.processed_section_source_uids);
        self.processed_text_source_uids
            .extend(committed.processed_text_source_uids);
    }
}

fn legi_archive_manifest(
    plan: &ArchivePlan,
    latest_processed: Option<&PlannedArchive>,
    counters: &LegiArchiveIngestCounters,
    run_status: &str,
) -> Value {
    // Freshness/source_version reflect the latest archive ACTUALLY processed (so an incremental or
    // no-op sync never advances reported corpus freshness for archives it did not read).
    let freshness = latest_processed.map_or(Value::Null, |archive| {
        json!({
            "latest_archive": archive.file_name.as_str(),
            "latest_archive_kind": archive.kind,
            "latest_archive_timestamp": archive.timestamp.to_string(),
            "latest_archive_timestamp_compact": archive.timestamp.compact()
        })
    });
    json!({
        "source": "legi",
        "dataset": "LEGI",
        "run_status": run_status,
        "complete": run_status == IngestRunStatus::Completed.as_str(),
        "parser_version": LEGI_PARSER_VERSION,
        "canonical_schema_version": CANONICAL_SCHEMA_VERSION,
        "code_version": CLI_CODE_VERSION,
        "source_version": latest_processed.map(|archive| archive.timestamp.to_string()),
        "freshness": freshness,
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

/// Which archives in a plan to process. The default (`incremental=false`, no `since`) processes the
/// baseline plus every delta — the full-build behavior. `sync` uses `incremental=true` (a prior full
/// build already ingested the baseline) plus an optional `since_compact` lower bound on delta
/// timestamps so a sync never re-scans the entire baseline corpus.
#[derive(Debug, Clone, Copy, Default)]
struct ArchiveSyncFilter<'a> {
    incremental: bool,
    since_compact: Option<&'a str>,
}

/// Ordered list of plan archives to process under `filter` (baseline first when not incremental,
/// then deltas at/after `since_compact`). Deltas keep the planner's deterministic order.
fn select_archives_to_process<'a>(
    plan: &'a ArchivePlan,
    filter: ArchiveSyncFilter<'_>,
) -> Vec<&'a PlannedArchive> {
    let mut archives = Vec::new();
    if !filter.incremental {
        archives.push(&plan.baseline);
    }
    for delta in &plan.deltas {
        if filter
            .since_compact
            .is_none_or(|since| delta.timestamp.compact() >= since)
        {
            archives.push(delta);
        }
    }
    archives
}

/// Normalize a `--since` value to the 14-digit compact archive-timestamp form for lexicographic
/// comparison. Accepts ONLY the two documented shapes — `YYYY-MM-DD` or compact `YYYYMMDDHHMMSS` —
/// and returns `None` for anything else (e.g. `2025/01/15`, `2025-01-15T00:00:00`, noise).
fn normalize_since(since: &str) -> Option<String> {
    let bytes = since.as_bytes();
    if bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
    {
        let digits: String = since.chars().filter(char::is_ascii_digit).collect();
        return Some(format!("{digits}000000"));
    }
    if since.len() == 14 && since.bytes().all(|byte| byte.is_ascii_digit()) {
        return Some(since.to_owned());
    }
    None
}

fn ingest_legi_archives_payload(
    index_dir: Option<&Path>,
    archives_dir: &Path,
    run_id: Option<String>,
    limit_members: Option<u32>,
    max_member_bytes: u64,
    quarantine_dir: Option<&Path>,
    safe_mode: bool,
    archive_filter: ArchiveSyncFilter<'_>,
) -> Result<Value, ErrorObject> {
    let index_dir = require_configured_index_dir(index_dir)?;
    let postgres = open_index_for_bulk_ingest(index_dir.as_path())?;
    let mut ingest_client =
        postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
            .map_err(|error| storage_error_object(StorageError::PostgresClient(error)))?;
    ingest_client
        .batch_execute("SET synchronous_commit TO off;")
        .map_err(|error| storage_error_object(StorageError::PostgresClient(error)))?;
    let plan = plan_from_dir(ArchiveSource::Legi, archives_dir).map_err(|error| {
        ErrorObject::bad_input(format!("failed to plan LEGI archives: {error}"))
    })?;
    let run_id = run_id.unwrap_or_else(default_legi_run_id);
    let archive_plan_json =
        serde_json::to_string(&plan).map_err(|error| dependency_unavailable(error.to_string()))?;
    let archives = select_archives_to_process(&plan, archive_filter);
    let latest_processed = archives.last().copied();
    let initial_manifest = legi_archive_manifest(
        &plan,
        latest_processed,
        &LegiArchiveIngestCounters::default(),
        IngestRunStatus::Running.as_str(),
    );
    let initial_manifest_json = initial_manifest.to_string();

    start_ingest_run_with_client(
        &mut ingest_client,
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

    'archives: for archive in &archives {
        let archive_name = archive.file_name.as_str();
        let mut pending_members = Vec::with_capacity(LEGI_INGEST_TRANSACTION_BATCH_SIZE);
        let mut pending_member_bytes = 0usize;
        let read_result = for_each_xml_member_until(&archive.path, max_member_bytes, |member| {
            if limit_members.is_some_and(|limit| counters.visited_members >= limit) {
                return Ok(ArchiveVisit::Stop);
            }
            counters.visited_members += 1;
            let member_bytes = member.bytes.len();
            if !pending_members.is_empty()
                && pending_member_bytes.saturating_add(member_bytes)
                    > LEGI_INGEST_TRANSACTION_BATCH_BYTE_LIMIT
                && let Err(error) = flush_legi_archive_member_batch(
                    &mut ingest_client,
                    run_id.as_str(),
                    archive_name,
                    &mut pending_members,
                    &mut pending_member_bytes,
                    quarantine_dir,
                    &mut counters,
                )
            {
                fatal_error = Some(storage_error_object(error));
                return Ok(ArchiveVisit::Stop);
            }
            pending_members.push(member);
            pending_member_bytes = pending_member_bytes.saturating_add(member_bytes);
            if (pending_members.len() >= LEGI_INGEST_TRANSACTION_BATCH_SIZE
                || pending_member_bytes >= LEGI_INGEST_TRANSACTION_BATCH_BYTE_LIMIT)
                && let Err(error) = flush_legi_archive_member_batch(
                    &mut ingest_client,
                    run_id.as_str(),
                    archive_name,
                    &mut pending_members,
                    &mut pending_member_bytes,
                    quarantine_dir,
                    &mut counters,
                )
            {
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

        if fatal_error.is_none()
            && read_result.is_ok()
            && !pending_members.is_empty()
            && let Err(error) = flush_legi_archive_member_batch(
                &mut ingest_client,
                run_id.as_str(),
                archive_name,
                &mut pending_members,
                &mut pending_member_bytes,
                quarantine_dir,
                &mut counters,
            )
        {
            fatal_error = Some(storage_error_object(error));
        }

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
        let scoped_backfill = LegiHierarchyBackfillScope {
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
        let full_resume_backfill = counters.skipped_compatible_members > 0;
        let backfill_scope = if full_resume_backfill {
            LegiHierarchyBackfillScope::default()
        } else {
            scoped_backfill
        };
        if full_resume_backfill || !backfill_scope.is_empty() {
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
    let final_manifest =
        legi_archive_manifest(&plan, latest_processed, &counters, manifest_run_status.as_str());
    let final_manifest_json = final_manifest.to_string();
    if let Err(error) = update_ingest_run_manifest_with_client(
        &mut ingest_client,
        run_id.as_str(),
        &final_manifest_json,
    ) {
        fatal_error.get_or_insert_with(|| storage_error_object(error));
    }

    let run_status = if counters.failed_members == 0 && fatal_error.is_none() {
        IngestRunStatus::Completed
    } else {
        IngestRunStatus::Failed
    };
    let error_message = fatal_error.as_ref().map(|error| error.message.as_str());
    finish_ingest_run_with_client(
        &mut ingest_client,
        run_id.as_str(),
        run_status,
        error_message,
    )
    .map_err(storage_error_object)?;
    if let Some(error) = fatal_error {
        return Err(error);
    }
    let replay_snapshot_cache = if run_status == IngestRunStatus::Completed {
        Some(maybe_refresh_replay_snapshot(&postgres)?)
    } else {
        None
    };

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
        "quarantine_dir": quarantine_dir,
        "replay_snapshot_cache": replay_snapshot_cache
            .as_ref()
            .map(|snapshot| replay_snapshot_cache_value(snapshot.as_ref()))
    }))
}

// ===== DILA bulk jurisprudence (decision) ingestion ==========================================

const JURI_PARSER_VERSION: &str = "juri_decision_parser:v1";
const JURI_CANONICAL_SCHEMA_VERSION: &str = "juri_decision:v1";

#[derive(Default)]
struct JuriArchiveIngestCounters {
    visited_members: usize,
    inserted_documents: usize,
    inserted_chunks: usize,
    inserted_publisher_edges: usize,
    inserted_inferred_edges: usize,
    skipped_members: usize,
    skipped_compatible_members: usize,
    skipped_empty_body_members: usize,
    failed_members: usize,
    quarantined_payloads: usize,
    unsupported_roots: BTreeMap<String, usize>,
}

impl JuriArchiveIngestCounters {
    fn merge_committed(&mut self, committed: Self) {
        self.inserted_documents += committed.inserted_documents;
        self.inserted_chunks += committed.inserted_chunks;
        self.inserted_publisher_edges += committed.inserted_publisher_edges;
        self.inserted_inferred_edges += committed.inserted_inferred_edges;
        self.skipped_members += committed.skipped_members;
        self.skipped_compatible_members += committed.skipped_compatible_members;
        self.skipped_empty_body_members += committed.skipped_empty_body_members;
        self.failed_members += committed.failed_members;
        self.quarantined_payloads += committed.quarantined_payloads;
        for (root, count) in committed.unsupported_roots {
            *self.unsupported_roots.entry(root).or_default() += count;
        }
    }
}

/// Monotonic in-process counter making default run IDs unique even within the same nanosecond.
static RUN_ID_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// A collision-resistant run-id suffix. `ingest`/`sync` runs without an explicit `--run-id` must not
/// share an id: `start_ingest_run_with_client` upserts on `ON CONFLICT (run_id)`, so a collision lets
/// a later run overwrite an earlier completed run's manifest (e.g. two rapid same-source syncs in the
/// same second erasing the first run's freshness). Nanosecond clock + PID + an in-process counter
/// makes the id unique across rapid same-process and separate-process invocations.
fn unique_run_suffix() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let pid = std::process::id();
    let sequence = RUN_ID_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    format!("{nanos}-{pid}-{sequence}")
}

fn default_juri_run_id(source: ArchiveSource) -> String {
    format!("{}-{}", source.as_str(), unique_run_suffix())
}

fn juri_archive_manifest(
    source: ArchiveSource,
    plan: &ArchivePlan,
    latest_processed: Option<&PlannedArchive>,
    counters: &JuriArchiveIngestCounters,
    run_status: &str,
) -> Value {
    // Freshness/source_version reflect the latest archive ACTUALLY processed by this run, not the
    // newest archive in the directory — so an incremental/`--since`-filtered or no-op sync never
    // advances reported corpus freshness for archives it did not read.
    let freshness = latest_processed.map_or(Value::Null, |archive| {
        json!({
            "latest_archive": archive.file_name.as_str(),
            "latest_archive_kind": archive.kind,
            "latest_archive_timestamp": archive.timestamp.to_string(),
            "latest_archive_timestamp_compact": archive.timestamp.compact()
        })
    });
    json!({
        "source": source.as_str(),
        "dataset": source.as_str().to_uppercase(),
        // Honest provenance: bulk jurisprudence carries NO official Judilibre zone offsets, so all
        // decision chunking is heuristic and never satisfies the official-zone gate by assertion.
        "chunking_provenance": "heuristic",
        "zone_accurate": false,
        "run_status": run_status,
        "complete": run_status == IngestRunStatus::Completed.as_str(),
        "parser_version": JURI_PARSER_VERSION,
        "canonical_schema_version": JURI_CANONICAL_SCHEMA_VERSION,
        "code_version": CLI_CODE_VERSION,
        "source_version": latest_processed.map(|archive| archive.timestamp.to_string()),
        "freshness": freshness,
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
            "inserted_inferred_edges": counters.inserted_inferred_edges,
            "skipped_members": counters.skipped_members,
            "skipped_compatible_members": counters.skipped_compatible_members,
            "skipped_empty_body_members": counters.skipped_empty_body_members,
            "failed_members": counters.failed_members,
            "quarantined_payloads": counters.quarantined_payloads,
            "unsupported_roots": &counters.unsupported_roots
        }
    })
}

#[allow(clippy::too_many_arguments)]
fn ingest_juri_archives_payload(
    index_dir: Option<&Path>,
    source: ArchiveSource,
    archives_dir: &Path,
    run_id: Option<String>,
    limit_members: Option<u32>,
    max_member_bytes: u64,
    quarantine_dir: Option<&Path>,
    safe_mode: bool,
    archive_filter: ArchiveSyncFilter<'_>,
) -> Result<Value, ErrorObject> {
    if !source.is_jurisprudence() {
        return Err(ErrorObject::bad_input(format!(
            "ingest juri-archives source `{}` is not a jurisprudence dataset",
            source.as_str()
        )));
    }
    let index_dir = require_configured_index_dir(index_dir)?;
    let postgres = open_index_for_bulk_ingest(index_dir.as_path())?;
    let mut ingest_client =
        postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
            .map_err(|error| storage_error_object(StorageError::PostgresClient(error)))?;
    ingest_client
        .batch_execute("SET synchronous_commit TO off;")
        .map_err(|error| storage_error_object(StorageError::PostgresClient(error)))?;
    let plan = plan_from_dir(source, archives_dir).map_err(|error| {
        ErrorObject::bad_input(format!(
            "failed to plan {} archives: {error}",
            source.as_str()
        ))
    })?;
    let run_id = run_id.unwrap_or_else(|| default_juri_run_id(source));
    let archive_plan_json =
        serde_json::to_string(&plan).map_err(|error| dependency_unavailable(error.to_string()))?;
    let archives = select_archives_to_process(&plan, archive_filter);
    let latest_processed = archives.last().copied();
    let initial_manifest = juri_archive_manifest(
        source,
        &plan,
        latest_processed,
        &JuriArchiveIngestCounters::default(),
        IngestRunStatus::Running.as_str(),
    );
    let initial_manifest_json = initial_manifest.to_string();

    start_ingest_run_with_client(
        &mut ingest_client,
        &IngestRunInput {
            run_id: run_id.as_str(),
            source: source.as_str(),
            parser_version: JURI_PARSER_VERSION,
            schema_version: JURI_CANONICAL_SCHEMA_VERSION,
            code_version: CLI_CODE_VERSION,
            safe_mode,
            archive_plan_json: Some(archive_plan_json.as_str()),
            manifest_json: Some(initial_manifest_json.as_str()),
        },
    )
    .map_err(storage_error_object)?;

    let mut counters = JuriArchiveIngestCounters::default();
    let mut fatal_error = None::<ErrorObject>;
    let limit_members = limit_members.map(|limit| limit as usize);

    'archives: for archive in &archives {
        let archive_name = archive.file_name.as_str();
        let mut pending_members = Vec::with_capacity(LEGI_INGEST_TRANSACTION_BATCH_SIZE);
        let mut pending_member_bytes = 0usize;
        let read_result = for_each_xml_member_until(&archive.path, max_member_bytes, |member| {
            if limit_members.is_some_and(|limit| counters.visited_members >= limit) {
                return Ok(ArchiveVisit::Stop);
            }
            counters.visited_members += 1;
            let member_bytes = member.bytes.len();
            if !pending_members.is_empty()
                && pending_member_bytes.saturating_add(member_bytes)
                    > LEGI_INGEST_TRANSACTION_BATCH_BYTE_LIMIT
                && let Err(error) = flush_juri_archive_member_batch(
                    &mut ingest_client,
                    source,
                    run_id.as_str(),
                    archive_name,
                    &mut pending_members,
                    &mut pending_member_bytes,
                    quarantine_dir,
                    &mut counters,
                )
            {
                fatal_error = Some(storage_error_object(error));
                return Ok(ArchiveVisit::Stop);
            }
            pending_members.push(member);
            pending_member_bytes = pending_member_bytes.saturating_add(member_bytes);
            if (pending_members.len() >= LEGI_INGEST_TRANSACTION_BATCH_SIZE
                || pending_member_bytes >= LEGI_INGEST_TRANSACTION_BATCH_BYTE_LIMIT)
                && let Err(error) = flush_juri_archive_member_batch(
                    &mut ingest_client,
                    source,
                    run_id.as_str(),
                    archive_name,
                    &mut pending_members,
                    &mut pending_member_bytes,
                    quarantine_dir,
                    &mut counters,
                )
            {
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

        if fatal_error.is_none()
            && read_result.is_ok()
            && !pending_members.is_empty()
            && let Err(error) = flush_juri_archive_member_batch(
                &mut ingest_client,
                source,
                run_id.as_str(),
                archive_name,
                &mut pending_members,
                &mut pending_member_bytes,
                quarantine_dir,
                &mut counters,
            )
        {
            fatal_error = Some(storage_error_object(error));
        }

        if let Err(error) = read_result {
            fatal_error = Some(ErrorObject::bad_input(format!(
                "failed to read {} archive `{}`: {error}",
                source.as_str(),
                archive.path.display()
            )));
        }
        if fatal_error.is_some()
            || limit_members.is_some_and(|limit| counters.visited_members >= limit)
        {
            break 'archives;
        }
    }

    // Build the manifest from the pre-finalization state, then RECOMPUTE the terminal run_status
    // after the manifest update so a fatal manifest-update failure cannot persist `completed`
    // (mirrors the LEGI reference; review 2026-06-23 phase2-1bc WARN).
    let manifest_run_status = if counters.failed_members == 0 && fatal_error.is_none() {
        IngestRunStatus::Completed
    } else {
        IngestRunStatus::Failed
    };
    let final_manifest =
        juri_archive_manifest(source, &plan, latest_processed, &counters, manifest_run_status.as_str());
    let final_manifest_json = final_manifest.to_string();
    if let Err(error) = update_ingest_run_manifest_with_client(
        &mut ingest_client,
        run_id.as_str(),
        &final_manifest_json,
    ) {
        fatal_error.get_or_insert_with(|| storage_error_object(error));
    }

    let run_status = if counters.failed_members == 0 && fatal_error.is_none() {
        IngestRunStatus::Completed
    } else {
        IngestRunStatus::Failed
    };
    let error_message = fatal_error.as_ref().map(|error| error.message.as_str());
    finish_ingest_run_with_client(&mut ingest_client, run_id.as_str(), run_status, error_message)
        .map_err(storage_error_object)?;
    if let Some(error) = fatal_error {
        return Err(error);
    }
    let replay_snapshot_cache = if run_status == IngestRunStatus::Completed {
        Some(maybe_refresh_replay_snapshot(&postgres)?)
    } else {
        None
    };

    Ok(json!({
        "schema_version": SCHEMA_VERSION,
        "command": "ingest juri-archives",
        "source": source.as_str(),
        "run_id": run_id,
        "run_status": run_status.as_str(),
        "safe_mode": safe_mode,
        "zone_accurate": false,
        "chunking_provenance": "heuristic",
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
        "inserted_inferred_edges": counters.inserted_inferred_edges,
        "skipped_members": counters.skipped_members,
        "skipped_compatible_members": counters.skipped_compatible_members,
        "skipped_empty_body_members": counters.skipped_empty_body_members,
        "failed_members": counters.failed_members,
        "quarantined_payloads": counters.quarantined_payloads,
        "unsupported_roots": counters.unsupported_roots,
        "quarantine_dir": quarantine_dir,
        "replay_snapshot_cache": replay_snapshot_cache
            .as_ref()
            .map(|snapshot| replay_snapshot_cache_value(snapshot.as_ref()))
    }))
}

#[allow(clippy::too_many_arguments)]
fn flush_juri_archive_member_batch(
    client: &mut postgres::Client,
    source: ArchiveSource,
    run_id: &str,
    archive_name: &str,
    pending_members: &mut Vec<ArchiveMember>,
    pending_member_bytes: &mut usize,
    quarantine_dir: Option<&Path>,
    counters: &mut JuriArchiveIngestCounters,
) -> Result<(), StorageError> {
    if pending_members.is_empty() {
        return Ok(());
    }
    let mut transaction = client.transaction().map_err(StorageError::PostgresClient)?;
    transaction
        .batch_execute("SET LOCAL synchronous_commit TO off;")
        .map_err(StorageError::PostgresClient)?;
    let projection_statements = prepare_document_projection_statements(&mut transaction)?;
    let mut committed = JuriArchiveIngestCounters::default();
    for member in pending_members.iter() {
        process_juri_archive_member(
            &mut transaction,
            source,
            run_id,
            archive_name,
            member,
            &projection_statements,
            quarantine_dir,
            &mut committed,
        )?;
    }
    transaction.commit().map_err(StorageError::PostgresClient)?;
    counters.merge_committed(committed);
    pending_members.clear();
    *pending_member_bytes = 0;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn process_juri_archive_member<C: postgres::GenericClient>(
    client: &mut C,
    source: ArchiveSource,
    run_id: &str,
    archive_name: &str,
    member: &ArchiveMember,
    projection_statements: &DocumentProjectionStatements,
    quarantine_dir: Option<&Path>,
    counters: &mut JuriArchiveIngestCounters,
) -> Result<(), StorageError> {
    let source_payload_hash = source_payload_hash(&member.bytes);
    let compatibility = IngestCompatibility {
        parser_version: JURI_PARSER_VERSION,
        schema_version: JURI_CANONICAL_SCHEMA_VERSION,
        code_version: CLI_CODE_VERSION,
        source_payload_hash: source_payload_hash.as_str(),
    };
    let resume = ingest_resume_decision_with_client(
        client,
        archive_name,
        member.member_path.as_str(),
        compatibility,
    )?;
    match resume.action {
        IngestResumeAction::Skip => {
            if resume.previous_run_id.as_deref() != Some(run_id) {
                record_juri_member(
                    client,
                    source,
                    run_id,
                    JuriMemberRecordInput {
                        archive_name,
                        member_path: member.member_path.as_str(),
                        source_entity: None,
                        date_anchor: None,
                        status: IngestMemberStatus::Skipped,
                        compatibility,
                    },
                )?;
            }
            counters.skipped_members += 1;
            counters.skipped_compatible_members += 1;
            return Ok(());
        }
        IngestResumeAction::BlockedIncompatible => {
            let message = format!(
                "resume blocked by compatibility mismatch on fields [{}]",
                resume.mismatched_fields.join(", ")
            );
            let record = record_juri_member(
                client,
                source,
                run_id,
                JuriMemberRecordInput {
                    archive_name,
                    member_path: member.member_path.as_str(),
                    source_entity: None,
                    date_anchor: None,
                    status: IngestMemberStatus::Failed,
                    compatibility,
                },
            )?;
            record_juri_member_error(
                client,
                run_id,
                Some(record.member_id),
                "validation_error",
                "compatibility_mismatch",
                message.as_str(),
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

    match parse_juri_member(source, member) {
        Ok(ParsedJuriXml::Decision(decision)) => {
            let decision = *decision;
            let record = record_juri_member(
                client,
                source,
                run_id,
                JuriMemberRecordInput {
                    archive_name,
                    member_path: member.member_path.as_str(),
                    source_entity: Some(decision.source_uid.as_str()),
                    date_anchor: Some(decision.decision_date.as_str()),
                    status: IngestMemberStatus::Parsed,
                    compatibility,
                },
            )?;
            let report =
                insert_decision_documents_with_statements(client, projection_statements, &[decision], None)?;
            update_ingest_member_status_with_client(
                client,
                record.member_id,
                IngestMemberStatus::Inserted,
                None,
            )?;
            counters.inserted_documents += report.documents;
            counters.inserted_chunks += report.chunks;
            counters.inserted_publisher_edges += report.publisher_edges;
            counters.inserted_inferred_edges += report.inferred_edges;
        }
        Ok(ParsedJuriXml::UnsupportedRoot { root }) => {
            *counters.unsupported_roots.entry(root.clone()).or_default() += 1;
            record_juri_member(
                client,
                source,
                run_id,
                JuriMemberRecordInput {
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
        // A decision with no textual body is not corrupt — there is just nothing to index. Record it
        // as a SKIP (not a failure/quarantine) so the run completes cleanly, matching the LEGI
        // no-text-article handling.
        Err(JuriParseError::EmptyBody { source_uid }) => {
            record_juri_member(
                client,
                source,
                run_id,
                JuriMemberRecordInput {
                    archive_name,
                    member_path: member.member_path.as_str(),
                    source_entity: Some(source_uid.as_str()),
                    date_anchor: None,
                    status: IngestMemberStatus::Skipped,
                    compatibility,
                },
            )?;
            counters.skipped_members += 1;
            counters.skipped_empty_body_members += 1;
        }
        Err(error) => {
            let (error_class, error_code) = juri_parse_error_class(&error);
            let message = error.to_string();
            let record = record_juri_member(
                client,
                source,
                run_id,
                JuriMemberRecordInput {
                    archive_name,
                    member_path: member.member_path.as_str(),
                    source_entity: None,
                    date_anchor: None,
                    status: IngestMemberStatus::Failed,
                    compatibility,
                },
            )?;
            record_juri_member_error(
                client,
                run_id,
                Some(record.member_id),
                error_class,
                error_code,
                message.as_str(),
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

struct JuriMemberRecordInput<'a> {
    archive_name: &'a str,
    member_path: &'a str,
    source_entity: Option<&'a str>,
    date_anchor: Option<&'a str>,
    status: IngestMemberStatus,
    compatibility: IngestCompatibility<'a>,
}

fn record_juri_member<C: postgres::GenericClient>(
    client: &mut C,
    source: ArchiveSource,
    run_id: &str,
    input: JuriMemberRecordInput<'_>,
) -> Result<jurisearch_storage::ingest_accounting::IngestMemberRecord, StorageError> {
    record_ingest_member_with_client(
        client,
        &IngestMemberInput {
            run_id,
            archive_name: input.archive_name,
            member_path: input.member_path,
            source: source.as_str(),
            source_entity: input.source_entity,
            date_anchor: input.date_anchor,
            status: input.status,
            compatibility: input.compatibility,
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn record_juri_member_error<C: postgres::GenericClient>(
    client: &mut C,
    run_id: &str,
    member_id: Option<i64>,
    error_class: &str,
    error_code: &str,
    message: &str,
    archive_name: &str,
    member: &ArchiveMember,
    quarantine_dir: Option<&Path>,
    counters: &mut JuriArchiveIngestCounters,
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
    record_ingest_error_with_client(
        client,
        &IngestErrorInput {
            run_id,
            member_id,
            error_class,
            error_code,
            message,
            retry_policy: "none",
            context_json: Some(context.as_str()),
        },
    )?;
    Ok(())
}

fn juri_parse_error_class(error: &JuriParseError) -> (&'static str, &'static str) {
    match error {
        JuriParseError::Xml { .. } => ("parse_error", "parse_malformed_xml"),
        JuriParseError::NotUtf8 { .. } => ("parse_error", "parse_not_utf8"),
        JuriParseError::MissingRequiredField { .. } => {
            ("validation_error", "validation_missing_required_field")
        }
        JuriParseError::InvalidDate { .. } => ("validation_error", "validation_invalid_date"),
        JuriParseError::InvalidId { .. } => ("validation_error", "validation_invalid_id"),
        // EmptyBody is handled as a skip before this classifier; map it for completeness.
        JuriParseError::EmptyBody { .. } => ("validation_error", "validation_empty_body"),
        JuriParseError::UnknownSource { .. } | JuriParseError::SourceFamilyMismatch { .. } => {
            ("validation_error", "validation_source_mismatch")
        }
    }
}

fn flush_legi_archive_member_batch(
    client: &mut postgres::Client,
    run_id: &str,
    archive_name: &str,
    pending_members: &mut Vec<ArchiveMember>,
    pending_member_bytes: &mut usize,
    quarantine_dir: Option<&Path>,
    counters: &mut LegiArchiveIngestCounters,
) -> Result<(), StorageError> {
    if pending_members.is_empty() {
        return Ok(());
    }
    process_legi_archive_member_batch(
        client,
        run_id,
        archive_name,
        pending_members,
        quarantine_dir,
        counters,
    )?;
    pending_members.clear();
    *pending_member_bytes = 0;
    Ok(())
}

fn process_legi_archive_member_batch(
    client: &mut postgres::Client,
    run_id: &str,
    archive_name: &str,
    members: &[ArchiveMember],
    quarantine_dir: Option<&Path>,
    counters: &mut LegiArchiveIngestCounters,
) -> Result<(), StorageError> {
    let mut transaction = client.transaction().map_err(StorageError::PostgresClient)?;
    transaction
        .batch_execute("SET LOCAL synchronous_commit TO off;")
        .map_err(StorageError::PostgresClient)?;
    // Prepare the document/chunk/edge upsert statements once for the whole batch instead of
    // re-parsing them for every member's insert.
    let projection_statements = prepare_legi_projection_statements(&mut transaction)?;
    let mut committed = LegiArchiveIngestCounters::default();
    for member in members {
        process_legi_archive_member(
            &mut transaction,
            run_id,
            archive_name,
            member,
            &projection_statements,
            quarantine_dir,
            &mut committed,
        )?;
    }
    transaction.commit().map_err(StorageError::PostgresClient)?;
    counters.merge_committed(committed);
    Ok(())
}

fn process_legi_archive_member<C: postgres::GenericClient>(
    client: &mut C,
    run_id: &str,
    archive_name: &str,
    member: &ArchiveMember,
    projection_statements: &LegiProjectionStatements,
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
    let resume = ingest_resume_decision_with_client(
        client,
        archive_name,
        member.member_path.as_str(),
        compatibility,
    )?;
    match resume.action {
        IngestResumeAction::Skip => {
            // Same-run skips would collide with the existing member row and demote inserted work.
            if resume.previous_run_id.as_deref() != Some(run_id) {
                record_legi_member(
                    client,
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
            }
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
                client,
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
                client,
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
                client,
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
            let report = insert_legi_documents_with_statements(
                client,
                projection_statements,
                &[document],
                None,
            )?;
            update_ingest_member_status_with_client(
                client,
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
                client,
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
                client,
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
                client,
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
                client,
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
                    client,
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
                client,
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
                client,
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
fn process_legi_metadata_root<C: postgres::GenericClient>(
    client: &mut C,
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
    let report = insert_legi_metadata_roots_with_client(client, &[metadata_root])?;
    *counters
        .parsed_metadata_roots
        .entry(root.to_owned())
        .or_default() += 1;
    record_legi_member(
        client,
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

fn record_legi_member<C: postgres::GenericClient>(
    client: &mut C,
    run_id: &str,
    input: LegiMemberRecordInput<'_>,
) -> Result<jurisearch_storage::ingest_accounting::IngestMemberRecord, StorageError> {
    record_ingest_member_with_client(
        client,
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
fn record_legi_member_error<C: postgres::GenericClient>(
    client: &mut C,
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
    record_ingest_error_with_client(
        client,
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
    // Hierarchy backfill can delete chunk_embeddings / clear embedding fingerprints, making the
    // index no longer query-ready; drop the readiness cache up front so a stale "ready" entry can
    // never let a subsequent search skip the live coverage check.
    invalidate_cached_query_readiness(&postgres).map_err(storage_error_object)?;
    let report =
        backfill_legi_article_hierarchy_from_metadata(&postgres).map_err(storage_error_object)?;
    let replay_snapshot = maybe_refresh_replay_snapshot(&postgres)?;

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
        "replay_snapshot_cache": replay_snapshot_cache_value(replay_snapshot.as_ref())
    }))
}

fn default_legi_run_id() -> String {
    format!("legi-{}", unique_run_suffix())
}

/// Whether maintenance commands should skip the (expensive, full-table MD5) replay-snapshot refresh
/// at their command boundary. Default false: the refresh keeps `status` cheap via the cached
/// signature. Setting `JURISEARCH_SKIP_REPLAY_SNAPSHOT` skips it (hundreds of seconds on a large
/// index) at the cost of a stale cached signature until the next `status --deep`.
fn replay_snapshot_refresh_skipped() -> bool {
    std::env::var_os("JURISEARCH_SKIP_REPLAY_SNAPSHOT").is_some()
}

/// Refresh the replay snapshot unless skipped via env. Returns `None` when skipped.
fn maybe_refresh_replay_snapshot(
    postgres: &ManagedPostgres,
) -> Result<Option<ReplaySnapshotReport>, ErrorObject> {
    if replay_snapshot_refresh_skipped() {
        Ok(None)
    } else {
        Ok(Some(
            refresh_replay_snapshot(postgres).map_err(storage_error_object)?,
        ))
    }
}

/// Report value for a maybe-refreshed snapshot: the full cache JSON when refreshed, else `skipped`.
fn replay_snapshot_cache_value(snapshot: Option<&ReplaySnapshotReport>) -> Value {
    match snapshot {
        Some(snapshot) => replay_snapshot_cache_json("refreshed", snapshot),
        None => json!({ "source": "skipped" }),
    }
}

fn replay_snapshot_cache_json(source: &str, snapshot: &ReplaySnapshotReport) -> Value {
    json!({
        "source": source,
        "status": snapshot.status(),
        "signature": snapshot.signature.as_str(),
        "documents": snapshot.documents.count,
        "chunks": snapshot.chunks.count,
        "publisher_edges": snapshot.publisher_edges.count,
        "embeddings": snapshot.embeddings.count,
        "manifests": snapshot.manifests.count
    })
}

/// Outcome of a single decision enrichment attempt, for backfill accounting.
#[derive(Clone, Copy)]
enum ZoneEnrichOutcome {
    /// Resolved with official zones (a fresh `ok` `decision_zones` row).
    Official,
    /// No official zone (not_found / unsupported / invalid_offsets) — cached, not an error.
    Fallback,
    /// A storage/transport failure during enrichment (logged, never aborts the backfill).
    Error,
}

/// Eagerly backfill official Judilibre zones for a Cassation source (`cass`/`inca`) into
/// `decision_zones`, paging the resolver-reachable candidate set and resolving each decision via the
/// shipped `enrich_decision_from_judilibre` (now `text_hash`-populating). Resumable: every attempt
/// writes a `decision_zones` row, so a re-run skips fresh rows. Conservative bounded concurrency keeps
/// the Judilibre request rate well under the live limit.
fn enrich_zones_payload(
    index_dir: Option<&Path>,
    source: &str,
    limit: Option<u32>,
    since: Option<&str>,
    concurrency: usize,
    order: CliEnrichZoneOrder,
) -> Result<Value, ErrorObject> {
    if !matches!(source, "cass" | "inca") {
        return Err(ErrorObject::bad_input(
            "ingest enrich-zones --source must be 'cass' or 'inca' (Judilibre covers only Cour de cassation)",
        ));
    }
    // Preflight: validate Judilibre (KeyId) credentials via the SAME config the workers use
    // (`OfficialApiConfig::from_env`), which accepts `JURISEARCH_PISTE_JUDILIBRE_KEY_ID` / `PISTE_API_KEY`
    // in production and `PISTE_SANDBOX_API_KEY` in sandbox — so a supported deployment is not rejected up
    // front and the message matches the real credential contract.
    if OfficialApiConfig::from_env().judilibre_key_id.is_none() {
        return Err(dependency_unavailable(
            "no Judilibre (PISTE) API key configured; set JURISEARCH_PISTE_JUDILIBRE_KEY_ID or \
             PISTE_API_KEY (PISTE_SANDBOX_API_KEY in sandbox) before running zone enrichment",
        ));
    }
    let index_dir = require_existing_index_dir(index_dir)?;
    let postgres = open_index(index_dir.as_path())?;

    let mut considered: u64 = 0;
    let mut official: u64 = 0;
    let mut fallback: u64 = 0;
    let mut errors: u64 = 0;
    let mut cursor: Option<String> = None;
    loop {
        // Respect --limit across pages.
        let page_limit = match limit {
            Some(limit) => {
                let done = u32::try_from(considered).unwrap_or(u32::MAX);
                if done >= limit {
                    break;
                }
                (limit - done).min(ENRICH_ZONES_PAGE_SIZE)
            }
            None => ENRICH_ZONES_PAGE_SIZE,
        };
        let page_json = enrich_zone_candidates_json(
            &postgres,
            source,
            cursor.as_deref(),
            since,
            page_limit,
            order.into(),
        )
        .map_err(storage_error_object)?;
        let page: Value = serde_json::from_str(&page_json)
            .map_err(|error| dependency_unavailable(error.to_string()))?;
        let doc_ids: Vec<String> = page["candidates"]
            .as_array()
            .map(|candidates| {
                candidates
                    .iter()
                    .filter_map(|candidate| candidate["document_id"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        if doc_ids.is_empty() {
            break;
        }
        for outcome in enrich_zone_page_concurrently(&postgres, &doc_ids, concurrency) {
            considered += 1;
            match outcome {
                ZoneEnrichOutcome::Official => official += 1,
                ZoneEnrichOutcome::Fallback => fallback += 1,
                ZoneEnrichOutcome::Error => errors += 1,
            }
        }
        cursor = page["next_cursor"].as_str().map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }

    let coverage: Value =
        serde_json::from_str(&zone_retrieval_coverage_json(&postgres).map_err(storage_error_object)?)
            .map_err(|error| dependency_unavailable(error.to_string()))?;
    Ok(json!({
        "schema_version": SCHEMA_VERSION,
        "command": "ingest enrich-zones",
        "index_dir": index_dir.display().to_string(),
        "source": source,
        "since": since,
        "concurrency": concurrency,
        "order": order.as_str(),
        "considered": considered,
        "official_ok": official,
        "fallback": fallback,
        "errors": errors,
        "coverage": coverage,
    }))
}

/// Enrich one page of decisions with bounded concurrency (codex-recommended model (b)): one owning
/// `ManagedPostgres` stays on the main thread; each scoped worker opens its OWN `postgres::Client` +
/// `PisteClient` from the `Send` connection string and resolves a contiguous slice via the thread-safe
/// core. A worker that cannot even connect, or panics, drops only its slice from accounting (counted as
/// errors) rather than aborting the whole backfill.
fn enrich_zone_page_concurrently(
    postgres: &ManagedPostgres,
    doc_ids: &[String],
    concurrency: usize,
) -> Vec<ZoneEnrichOutcome> {
    let workers = concurrency.max(1).min(doc_ids.len().max(1));
    let connection_string = postgres.connection_string();
    let mut groups: Vec<Vec<&str>> = (0..workers).map(|_| Vec::new()).collect();
    for (index, doc_id) in doc_ids.iter().enumerate() {
        groups[index % workers].push(doc_id.as_str());
    }
    std::thread::scope(|scope| {
        let connection_string = &connection_string;
        let handles: Vec<(usize, _)> = groups
            .into_iter()
            .map(|group| {
                let group_len = group.len();
                let handle = scope.spawn(move || {
                    let mut db =
                        match postgres::Client::connect(connection_string, postgres::NoTls) {
                            Ok(db) => db,
                            // Whole slice fails to connect -> count as errors, don't abort the run.
                            Err(_) => return vec![ZoneEnrichOutcome::Error; group.len()],
                        };
                    let piste = PisteClient::new(OfficialApiConfig::from_env());
                    group
                        .into_iter()
                        .map(|doc_id| {
                            match enrich_decision_from_judilibre_with_client(&mut db, &piste, doc_id)
                            {
                                Ok(Some(_)) => ZoneEnrichOutcome::Official,
                                Ok(None) => ZoneEnrichOutcome::Fallback,
                                Err(_) => ZoneEnrichOutcome::Error,
                            }
                        })
                        .collect::<Vec<_>>()
                });
                (group_len, handle)
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|(group_len, handle)| {
                worker_outcomes_or_errors(handle.join().ok(), group_len)
            })
            .collect()
    })
}

/// Map a scoped worker's join result to per-decision outcomes. A panicked worker (join `None`) counts
/// its WHOLE slice as errors rather than silently dropping those decisions from the backfill accounting.
fn worker_outcomes_or_errors(
    returned: Option<Vec<ZoneEnrichOutcome>>,
    group_len: usize,
) -> Vec<ZoneEnrichOutcome> {
    returned.unwrap_or_else(|| vec![ZoneEnrichOutcome::Error; group_len])
}

/// Derive a decision's `zone_units` rows from its cached `zones_json` object (motivations/moyens/
/// dispositif fragment text). One row per non-empty fragment with a contiguous per-zone `fragment_index`.
/// Borrows the fragment text from `zones`, so the returned rows must be used before `zones` is dropped.
fn derive_zone_unit_rows<'a>(
    document_id: &'a str,
    source: &'a str,
    text_hash: &'a str,
    zones: &'a Value,
) -> Vec<ZoneUnitRow<'a>> {
    let mut rows = Vec::new();
    for zone in ["motivations", "moyens", "dispositif"] {
        let Some(fragments) = zones[zone].as_array() else {
            continue;
        };
        let mut fragment_index = 0i32;
        for fragment in fragments {
            let Some(text) = fragment["text"].as_str() else {
                continue;
            };
            if text.trim().is_empty() {
                continue;
            }
            rows.push(ZoneUnitRow {
                document_id,
                zone,
                fragment_index,
                body: text,
                search_body: text,
                source,
                text_hash,
                builder_version: ZONE_UNIT_BUILDER_VERSION,
            });
            fragment_index += 1;
        }
    }
    rows
}

/// `ingest build-zone-units`: derive `zone_units` from the cached official zones in `decision_zones`.
/// Pages the derivable set (fresh `ok` Cassation rows with stale/absent units), deriving each decision's
/// units in one idempotent `replace_zone_units_for_document` transaction.
fn build_zone_units_payload(
    index_dir: Option<&Path>,
    limit: Option<u32>,
    rebuild: bool,
) -> Result<Value, ErrorObject> {
    let index_dir = require_existing_index_dir(index_dir)?;
    let postgres = open_index(index_dir.as_path())?;

    let mut decisions: u64 = 0;
    let mut units_written: u64 = 0;
    let mut cursor: Option<String> = None;
    loop {
        let page_limit = match limit {
            Some(limit) => {
                let done = u32::try_from(decisions).unwrap_or(u32::MAX);
                if done >= limit {
                    break;
                }
                (limit - done).min(BUILD_ZONE_UNITS_PAGE_SIZE)
            }
            None => BUILD_ZONE_UNITS_PAGE_SIZE,
        };
        let page_json = load_derivable_decision_zones_json(
            &postgres,
            ZONE_UNIT_BUILDER_VERSION,
            rebuild,
            cursor.as_deref(),
            page_limit,
        )
        .map_err(storage_error_object)?;
        let page: Value = serde_json::from_str(&page_json)
            .map_err(|error| dependency_unavailable(error.to_string()))?;
        let candidates = page["candidates"].as_array().cloned().unwrap_or_default();
        if candidates.is_empty() {
            break;
        }
        for candidate in &candidates {
            let document_id = candidate["document_id"].as_str().unwrap_or_default();
            if document_id.is_empty() {
                continue;
            }
            let source = candidate["source"].as_str().unwrap_or_default();
            let text_hash = candidate["text_hash"].as_str().unwrap_or_default();
            let rows = derive_zone_unit_rows(document_id, source, text_hash, &candidate["zones"]);
            replace_zone_units_for_document(&postgres, document_id, &rows)
                .map_err(storage_error_object)?;
            decisions += 1;
            units_written += rows.len() as u64;
            if let Some(limit) = limit
                && decisions >= u64::from(limit)
            {
                break;
            }
        }
        cursor = page["next_cursor"].as_str().map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }

    let coverage: Value =
        serde_json::from_str(&zone_retrieval_coverage_json(&postgres).map_err(storage_error_object)?)
            .map_err(|error| dependency_unavailable(error.to_string()))?;
    Ok(json!({
        "schema_version": SCHEMA_VERSION,
        "command": "ingest build-zone-units",
        "index_dir": index_dir.display().to_string(),
        "builder_version": ZONE_UNIT_BUILDER_VERSION,
        "rebuild": rebuild,
        "decisions_derived": decisions,
        "zone_units_written": units_written,
        "coverage": coverage,
    }))
}

/// `ingest embed-zone-units`: embed `zone_units` via the SAME OpenRouter pool + fingerprint as
/// `embed-chunks`, then finalize the separate zone-unit dense ANN index. Mirrors the embed-chunks
/// streaming/finalize flow against the zone tables; the chunk dense path is untouched.
fn embed_zone_units_payload(
    index_dir: Option<&Path>,
    limit: Option<u32>,
    index_lists: u32,
    batch_size: usize,
    pool_concurrency: usize,
) -> Result<Value, ErrorObject> {
    let index_dir = require_existing_index_dir(index_dir)?;
    let postgres = open_index(index_dir.as_path())?;
    let loaded_embedding = loaded_embedding_config();
    let embedding_config = loaded_embedding.config;
    ensure_embedding_runtime_ready(&embedding_config, false)?;
    let expected_fingerprint = embedding_config.fingerprint();
    let embedding_fingerprint = embedding_config.storage_embedding_fingerprint();
    let endpoint_configs = embedding_endpoint_pool_configs(
        &embedding_config,
        &loaded_embedding.pool_endpoints,
        &expected_fingerprint,
        embedding_fingerprint.as_str(),
    )?;
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

    let to_chunk_inputs = |inputs: Vec<jurisearch_storage::zone_units::ZoneUnitEmbeddingInput>| {
        inputs
            .into_iter()
            .map(|input| ChunkEmbeddingInput {
                chunk_id: input.zone_unit_id,
                embedding_text: input.embedding_text,
            })
            .collect::<Vec<_>>()
    };

    let embedding_run = if let Some(limit) = limit {
        let inputs = load_zone_unit_embedding_inputs(
            &postgres,
            embedding_fingerprint.as_str(),
            embedding_config.model.as_str(),
            dimension,
            Some(limit.saturating_add(1)),
        )
        .map_err(storage_error_object)?;
        if inputs.len() > usize::try_from(limit).unwrap_or(usize::MAX) {
            return Err(ErrorObject::bad_input(
                "ingest embed-zone-units --limit would leave zone units unembedded; run on a smaller smoke index or omit --limit to finalize the full zone index",
            ));
        }
        if inputs.is_empty() {
            return Err(no_results("no zone units are available to embed"));
        }
        embed_and_insert_zone_units_with_pool(
            &postgres,
            to_chunk_inputs(inputs),
            &endpoint_configs,
            embedding_fingerprint.as_str(),
            &embedding_config,
            batch_size,
            pool_concurrency,
        )?
    } else {
        let mut run = EmbeddingPoolRun {
            chunks_considered: 0,
            embeddings_inserted: 0,
            embedding_inputs_truncated: 0,
            endpoint_stats: Vec::new(),
        };
        loop {
            let page = load_zone_unit_embedding_inputs(
                &postgres,
                embedding_fingerprint.as_str(),
                embedding_config.model.as_str(),
                dimension,
                Some(EMBED_STREAM_PAGE_SIZE),
            )
            .map_err(storage_error_object)?;
            if page.is_empty() {
                break;
            }
            let page_run = embed_and_insert_zone_units_with_pool(
                &postgres,
                to_chunk_inputs(page),
                &endpoint_configs,
                embedding_fingerprint.as_str(),
                &embedding_config,
                batch_size,
                pool_concurrency,
            )?;
            run.chunks_considered += page_run.chunks_considered;
            run.embeddings_inserted += page_run.embeddings_inserted;
            run.embedding_inputs_truncated += page_run.embedding_inputs_truncated;
            merge_embedding_endpoint_stats(&mut run.endpoint_stats, page_run.endpoint_stats);
        }
        if run.chunks_considered == 0 {
            return Err(no_results("no zone units are available to embed"));
        }
        run
    };

    let rebuild = finalize_zone_dense_rebuild(
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
        "command": "ingest embed-zone-units",
        "index_dir": index_dir.display().to_string(),
        "embedding_fingerprint": rebuild.embedding_fingerprint,
        "zone_units": rebuild.zone_units,
        "embeddings": rebuild.embeddings,
        "zone_units_considered": embedding_run.chunks_considered,
        "embeddings_inserted": embedding_run.embeddings_inserted,
        "embedding_inputs_truncated": embedding_run.embedding_inputs_truncated,
        "vector_index": {
            "name": rebuild.index_name,
            "index_lists": rebuild.index_lists
        },
        "endpoint_stats": embedding_run.endpoint_stats,
    }))
}

fn embed_chunks_payload(
    index_dir: Option<&Path>,
    limit: Option<u32>,
    index_lists: u32,
    batch_size: usize,
    pool_concurrency: usize,
) -> Result<Value, ErrorObject> {
    let index_dir = require_existing_index_dir(index_dir)?;
    let postgres = open_index(index_dir.as_path())?;
    // Re-embedding changes embedding coverage; drop the readiness cache up front so the next query
    // recomputes (it is repopulated only when the index is fully ready again).
    invalidate_cached_query_readiness(&postgres).map_err(storage_error_object)?;
    let loaded_embedding = loaded_embedding_config();
    let embedding_config = loaded_embedding.config;
    ensure_embedding_runtime_ready(&embedding_config, false)?;
    let expected_fingerprint = embedding_config.fingerprint();
    let embedding_fingerprint = embedding_config.storage_embedding_fingerprint();
    let endpoint_configs = embedding_endpoint_pool_configs(
        &embedding_config,
        &loaded_embedding.pool_endpoints,
        &expected_fingerprint,
        embedding_fingerprint.as_str(),
    )?;
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

    // Embedding upserts and dense finalization are separate recoverable steps: re-running the
    // command converges before the manifest/index is advertised.
    let embedding_run = if let Some(limit) = limit {
        // --limit is a bounded smoke path on a small index: load the whole pending set (capped at
        // limit + 1), refuse if it would leave chunks unembedded, then embed it in one pass.
        let inputs = load_chunk_embedding_inputs(
            &postgres,
            embedding_fingerprint.as_str(),
            embedding_config.model.as_str(),
            dimension,
            Some(limit.saturating_add(1)),
        )
        .map_err(storage_error_object)?;
        if inputs.len() > usize::try_from(limit).unwrap_or(usize::MAX) {
            return Err(ErrorObject::bad_input(
                "ingest embed-chunks --limit would leave chunks unembedded; run on a smaller smoke index or omit --limit to finalize the full dense index",
            ));
        }
        if inputs.is_empty() {
            return Err(no_results("no chunks are available to embed"));
        }
        embed_and_insert_chunks_with_pool(
            &postgres,
            inputs,
            &endpoint_configs,
            embedding_fingerprint.as_str(),
            &embedding_config,
            batch_size,
            pool_concurrency,
        )?
    } else {
        // Production path: stream pending chunks in bounded pages so peak memory is one page, not
        // the full ~1.85M-chunk set (each input can hold up to ~6k chars of contextualized text).
        // Each batch's embeddings are inserted as it completes, so an embedded chunk leaves the
        // pending set and the next page query returns the next slice; a failed page aborts and is
        // recoverable (re-running converges). Embedding generation (the HTTP round-trips) dominates
        // runtime, so the repeated bounded page queries are negligible.
        let mut run = EmbeddingPoolRun {
            chunks_considered: 0,
            embeddings_inserted: 0,
            embedding_inputs_truncated: 0,
            endpoint_stats: Vec::new(),
        };
        loop {
            let page = load_chunk_embedding_inputs(
                &postgres,
                embedding_fingerprint.as_str(),
                embedding_config.model.as_str(),
                dimension,
                Some(EMBED_STREAM_PAGE_SIZE),
            )
            .map_err(storage_error_object)?;
            if page.is_empty() {
                break;
            }
            let page_run = embed_and_insert_chunks_with_pool(
                &postgres,
                page,
                &endpoint_configs,
                embedding_fingerprint.as_str(),
                &embedding_config,
                batch_size,
                pool_concurrency,
            )?;
            run.chunks_considered += page_run.chunks_considered;
            run.embeddings_inserted += page_run.embeddings_inserted;
            run.embedding_inputs_truncated += page_run.embedding_inputs_truncated;
            merge_embedding_endpoint_stats(&mut run.endpoint_stats, page_run.endpoint_stats);
        }
        if run.chunks_considered == 0 {
            return Err(no_results("no chunks are available to embed"));
        }
        run
    };
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
    let replay_snapshot = maybe_refresh_replay_snapshot(&postgres)?;

    Ok(json!({
        "schema_version": SCHEMA_VERSION,
        "command": "ingest embed-chunks",
        "index_dir": index_dir,
        "limit": limit,
        "chunks_considered": embedding_run.chunks_considered,
        "embeddings_inserted": embedding_run.embeddings_inserted,
        "embedding_inputs_truncated": embedding_run.embedding_inputs_truncated,
        "embedding": {
            "model": embedding_config.model,
            "dimension": embedding_config.dimension,
            "normalize": embedding_config.normalize,
            "pooling": embedding_config.pooling,
            "base_urls": embedding_config.base_urls.clone(),
            "pool": embedding_pool_endpoints_status_json(&loaded_embedding.pool_endpoints),
            "pool_overrides_base_urls": !loaded_embedding.pool_endpoints.is_empty(),
            "max_input_chars": embedding_config.max_input_chars,
            "max_estimated_tokens": embedding_config.max_estimated_tokens,
            "estimated_chars_per_token": embedding_config.estimated_chars_per_token,
            "token_count_method": embedding_config.configured_token_count_method(),
            "tokenizer_path": embedding_config.tokenizer_path.as_ref().map(|path| path.display().to_string()),
            "fingerprint": embedding_fingerprint,
            "provisional": embedding_config.provisional,
            "reembeddable": embedding_config.reembeddable
        },
        "endpoint_pool": {
            "strategy": "least_outstanding_requests",
            "batch_size": batch_size,
            "pool_concurrency": pool_concurrency,
            "endpoints": embedding_run.endpoint_stats
        },
        "dense_rebuild": {
            "chunks": rebuild.chunks,
            "embeddings": rebuild.embeddings,
            "embedding_fingerprint": rebuild.embedding_fingerprint,
            "index_name": rebuild.index_name,
            "index_lists": rebuild.index_lists
        },
        "replay_snapshot_cache": replay_snapshot_cache_value(replay_snapshot.as_ref())
    }))
}

#[derive(Debug, Clone)]
struct EmbeddingEndpointPoolConfig {
    base_url: String,
    request_model: Option<String>,
    config: EmbeddingConfig,
    expected_fingerprint: EmbeddingFingerprint,
}

#[derive(Debug, Clone)]
struct EmbeddingEndpointState {
    base_url: String,
    request_model: Option<String>,
    outstanding: usize,
    requests: usize,
    chunks: usize,
    truncated_inputs: usize,
    failures: usize,
}

#[derive(Debug, Clone)]
struct EmbeddingBatchWork {
    inputs: Vec<ChunkEmbeddingInput>,
}

#[derive(Debug, Clone)]
struct OwnedChunkEmbedding {
    chunk_id: String,
    embedding_literal: String,
}

#[derive(Debug, Clone)]
struct EmbeddingBatchSuccess {
    embeddings: Vec<OwnedChunkEmbedding>,
    truncated_inputs: usize,
}

#[derive(Debug, Clone)]
struct EmbeddingBatchFailure {
    error: ErrorObject,
}

#[derive(Debug, Clone)]
struct EmbeddingPoolRun {
    chunks_considered: usize,
    embeddings_inserted: usize,
    embedding_inputs_truncated: usize,
    endpoint_stats: Vec<Value>,
}

fn embedding_endpoint_pool_configs(
    config: &EmbeddingConfig,
    pool_endpoints: &[EmbeddingPoolEndpoint],
    expected_fingerprint: &EmbeddingFingerprint,
    storage_embedding_fingerprint: &str,
) -> Result<Vec<EmbeddingEndpointPoolConfig>, ErrorObject> {
    if !matches!(config.provider, EmbeddingProvider::OpenAiCompatible) {
        return Err(embedding_error_object(
            jurisearch_embed::EmbeddingError::UnsupportedProvider {
                provider: config.provider,
            },
        ));
    }

    let endpoint_specs = if pool_endpoints.is_empty() {
        legacy_embedding_pool_endpoints(config)
    } else {
        dedupe_embedding_pool_endpoints(pool_endpoints.to_vec())
    };
    if endpoint_specs.is_empty() {
        return Err(embedding_error_object(
            jurisearch_embed::EmbeddingError::MissingBaseUrl,
        ));
    }

    endpoint_specs
        .into_iter()
        .map(|endpoint| {
            let mut endpoint_config = config.clone();
            endpoint_config.base_url = Some(endpoint.base_url.clone());
            endpoint_config.base_urls = vec![endpoint.base_url.clone()];
            endpoint_config.request_model = endpoint.request_model.clone();
            if pool_endpoints.is_empty() {
                endpoint_config.api_key = config.api_key.clone();
            } else if endpoint.api_key_env.is_some() && endpoint.api_key.is_none() {
                let api_key_env = endpoint.api_key_env.as_deref().unwrap_or_default();
                return Err(dependency_unavailable(format!(
                    "embedding pool endpoint `{}` requires non-empty environment variable `{api_key_env}`",
                    endpoint.base_url
                )));
            } else {
                endpoint_config.api_key = endpoint.api_key.clone();
            }
            let endpoint_fingerprint = endpoint_config.fingerprint();
            if endpoint_fingerprint.provider != expected_fingerprint.provider
                || endpoint_fingerprint.model != expected_fingerprint.model
                || endpoint_fingerprint.dimension != expected_fingerprint.dimension
                || endpoint_fingerprint.normalize != expected_fingerprint.normalize
                || endpoint_fingerprint.pooling != expected_fingerprint.pooling
                || endpoint_fingerprint.storage_embedding_fingerprint()
                    != storage_embedding_fingerprint
            {
                return Err(dependency_unavailable(format!(
                    "embedding endpoint `{}` does not match the selected model fingerprint",
                    endpoint.base_url
                )));
            }
            Ok(EmbeddingEndpointPoolConfig {
                base_url: endpoint.base_url,
                request_model: endpoint.request_model,
                config: endpoint_config,
                expected_fingerprint: endpoint_fingerprint,
            })
        })
        .collect()
}

fn legacy_embedding_pool_endpoints(config: &EmbeddingConfig) -> Vec<EmbeddingPoolEndpoint> {
    let mut endpoints = config
        .base_urls
        .iter()
        .filter_map(|base_url| nonempty_string(Some(base_url.clone())))
        .map(|base_url| EmbeddingPoolEndpoint {
            base_url,
            request_model: None,
            api_key_env: None,
            api_key: config.api_key.clone(),
        })
        .collect::<Vec<_>>();
    if endpoints.is_empty()
        && let Some(base_url) = config
            .base_url
            .clone()
            .and_then(|base_url| nonempty_string(Some(base_url)))
    {
        endpoints.push(EmbeddingPoolEndpoint {
            base_url,
            request_model: None,
            api_key_env: None,
            api_key: config.api_key.clone(),
        });
    }
    dedupe_embedding_pool_endpoints(endpoints)
}

fn dedupe_embedding_pool_endpoints(
    endpoints: Vec<EmbeddingPoolEndpoint>,
) -> Vec<EmbeddingPoolEndpoint> {
    let mut deduped = Vec::new();
    for endpoint in endpoints {
        if !deduped.iter().any(|existing: &EmbeddingPoolEndpoint| {
            existing.base_url.trim_end_matches('/') == endpoint.base_url.trim_end_matches('/')
                && existing.request_model == endpoint.request_model
                && existing.api_key_env == endpoint.api_key_env
        }) {
            deduped.push(endpoint);
        }
    }
    deduped
}

/// Number of pending chunks loaded per page when streaming the full embed run, bounding peak memory.
const EMBED_STREAM_PAGE_SIZE: u32 = 20_000;

/// Accumulate per-endpoint embedding stats across streamed pages, summing counters per `base_url`.
fn merge_embedding_endpoint_stats(accumulator: &mut Vec<Value>, page: Vec<Value>) {
    for stat in page {
        let base_url = stat
            .get("base_url")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let existing = accumulator.iter_mut().find(|entry| {
            entry.get("base_url").and_then(Value::as_str).map(str::to_owned) == base_url
        });
        match existing {
            Some(entry) => {
                for field in ["requests", "chunks", "truncated_inputs", "failures"] {
                    let sum = entry.get(field).and_then(Value::as_u64).unwrap_or(0)
                        + stat.get(field).and_then(Value::as_u64).unwrap_or(0);
                    entry[field] = json!(sum);
                }
            }
            None => accumulator.push(stat),
        }
    }
}

/// Generic embedding-pool driver: embeds `inputs` across the endpoint pool and applies `insert_batch`
/// to each completed batch's `(id, literal)` results. Identical for chunks and zone units (the workers
/// are id/text-agnostic); only the storage insert differs, so it is injected by the caller.
fn embed_and_insert_with_pool<F>(
    inputs: Vec<ChunkEmbeddingInput>,
    endpoint_configs: &[EmbeddingEndpointPoolConfig],
    batch_size: usize,
    pool_concurrency: usize,
    insert_batch: F,
) -> Result<EmbeddingPoolRun, ErrorObject>
where
    F: Fn(&[OwnedChunkEmbedding]) -> Result<usize, ErrorObject>,
{
    let chunks_considered = inputs.len();
    let work_queue = inputs
        .chunks(batch_size)
        .map(|inputs| EmbeddingBatchWork {
            inputs: inputs.to_vec(),
        })
        .collect::<VecDeque<_>>();
    let worker_count = pool_concurrency.min(work_queue.len().max(1));
    let work_queue = Arc::new(Mutex::new(work_queue));
    let endpoint_configs = Arc::new(endpoint_configs.to_vec());
    let endpoint_states = Arc::new(Mutex::new(
        endpoint_configs
            .iter()
            .map(|config| EmbeddingEndpointState {
                base_url: config.base_url.clone(),
                request_model: config.request_model.clone(),
                outstanding: 0,
                requests: 0,
                chunks: 0,
                truncated_inputs: 0,
                failures: 0,
            })
            .collect::<Vec<_>>(),
    ));
    let stop_requested = Arc::new(AtomicBool::new(false));
    let (sender, receiver) =
        mpsc::channel::<Result<EmbeddingBatchSuccess, EmbeddingBatchFailure>>();
    let mut handles = Vec::with_capacity(worker_count);

    for _ in 0..worker_count {
        let work_queue = Arc::clone(&work_queue);
        let endpoint_configs = Arc::clone(&endpoint_configs);
        let endpoint_states = Arc::clone(&endpoint_states);
        let stop_requested = Arc::clone(&stop_requested);
        let sender = sender.clone();
        handles.push(thread::spawn(move || {
            embedding_pool_worker(
                work_queue,
                endpoint_configs,
                endpoint_states,
                stop_requested,
                sender,
            );
        }));
    }
    drop(sender);

    let mut embeddings_inserted = 0usize;
    let mut embedding_inputs_truncated = 0usize;
    let mut first_error = None::<ErrorObject>;
    for message in receiver {
        match message {
            Ok(success) => {
                if first_error.is_some() {
                    continue;
                }
                embedding_inputs_truncated += success.truncated_inputs;
                match insert_batch(&success.embeddings) {
                    Ok(inserted) => {
                        embeddings_inserted += inserted;
                    }
                    Err(error) => {
                        stop_requested.store(true, Ordering::SeqCst);
                        first_error.get_or_insert(error);
                    }
                }
            }
            Err(failure) => {
                stop_requested.store(true, Ordering::SeqCst);
                first_error.get_or_insert(failure.error);
            }
        }
    }

    for handle in handles {
        if handle.join().is_err() && first_error.is_none() {
            first_error = Some(dependency_unavailable(
                "embedding endpoint pool worker panicked".to_owned(),
            ));
        }
    }

    if let Some(error) = first_error {
        return Err(error);
    }

    let endpoint_stats = endpoint_states
        .lock()
        .expect("embedding endpoint state lock")
        .iter()
        .map(|state| {
            json!({
                "base_url": state.base_url.as_str(),
                "request_model": state.request_model.as_deref(),
                "requests": state.requests,
                "chunks": state.chunks,
                "truncated_inputs": state.truncated_inputs,
                "failures": state.failures
            })
        })
        .collect();

    Ok(EmbeddingPoolRun {
        chunks_considered,
        embeddings_inserted,
        embedding_inputs_truncated,
        endpoint_stats,
    })
}

/// Embed chunk inputs across the pool and upsert into `chunk_embeddings` (thin wrapper over the generic
/// driver; behaviour unchanged).
fn embed_and_insert_chunks_with_pool(
    postgres: &ManagedPostgres,
    inputs: Vec<ChunkEmbeddingInput>,
    endpoint_configs: &[EmbeddingEndpointPoolConfig],
    embedding_fingerprint: &str,
    embedding_config: &EmbeddingConfig,
    batch_size: usize,
    pool_concurrency: usize,
) -> Result<EmbeddingPoolRun, ErrorObject> {
    embed_and_insert_with_pool(
        inputs,
        endpoint_configs,
        batch_size,
        pool_concurrency,
        |embeddings| {
            let inserts = embeddings
                .iter()
                .map(|embedding| ChunkEmbeddingInsert {
                    chunk_id: embedding.chunk_id.as_str(),
                    embedding_fingerprint,
                    embedding_literal: embedding.embedding_literal.as_str(),
                    model: embedding_config.model.as_str(),
                    dimension: embedding_config.dimension,
                })
                .collect::<Vec<_>>();
            insert_chunk_embeddings(postgres, &inserts).map_err(storage_error_object)
        },
    )
}

/// Embed zone-unit inputs across the SAME pool and upsert into `zone_unit_embeddings` (parallel to the
/// chunk wrapper; the only difference is the storage target). `OwnedChunkEmbedding.chunk_id` carries the
/// `zone_unit_id` here.
fn embed_and_insert_zone_units_with_pool(
    postgres: &ManagedPostgres,
    inputs: Vec<ChunkEmbeddingInput>,
    endpoint_configs: &[EmbeddingEndpointPoolConfig],
    embedding_fingerprint: &str,
    embedding_config: &EmbeddingConfig,
    batch_size: usize,
    pool_concurrency: usize,
) -> Result<EmbeddingPoolRun, ErrorObject> {
    embed_and_insert_with_pool(
        inputs,
        endpoint_configs,
        batch_size,
        pool_concurrency,
        |embeddings| {
            let inserts = embeddings
                .iter()
                .map(|embedding| ZoneUnitEmbeddingInsert {
                    zone_unit_id: embedding.chunk_id.as_str(),
                    embedding_fingerprint,
                    embedding_literal: embedding.embedding_literal.as_str(),
                    model: embedding_config.model.as_str(),
                    dimension: embedding_config.dimension,
                })
                .collect::<Vec<_>>();
            insert_zone_unit_embeddings(postgres, &inserts).map_err(storage_error_object)
        },
    )
}

fn embedding_pool_worker(
    work_queue: Arc<Mutex<VecDeque<EmbeddingBatchWork>>>,
    endpoint_configs: Arc<Vec<EmbeddingEndpointPoolConfig>>,
    endpoint_states: Arc<Mutex<Vec<EmbeddingEndpointState>>>,
    stop_requested: Arc<AtomicBool>,
    sender: mpsc::Sender<Result<EmbeddingBatchSuccess, EmbeddingBatchFailure>>,
) {
    let clients = match endpoint_configs
        .iter()
        .map(|config| OpenAiCompatibleClient::new(config.config.clone()))
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(clients) => clients,
        Err(error) => {
            stop_requested.store(true, Ordering::SeqCst);
            let _ = sender.send(Err(EmbeddingBatchFailure {
                error: embedding_error_object(error),
            }));
            return;
        }
    };

    while !stop_requested.load(Ordering::SeqCst) {
        let Some(work) = work_queue
            .lock()
            .expect("embedding work queue lock")
            .pop_front()
        else {
            return;
        };
        let endpoint_index = acquire_least_outstanding_endpoint(&endpoint_states);
        let result = embed_batch_on_endpoint(
            &clients[endpoint_index],
            &endpoint_configs[endpoint_index],
            &work,
        );
        let truncated_inputs = match &result {
            Ok(success) => success.truncated_inputs,
            Err(_) => 0,
        };
        release_embedding_endpoint(
            &endpoint_states,
            endpoint_index,
            work.inputs.len(),
            truncated_inputs,
            &result,
        );
        if sender.send(result).is_err() {
            return;
        }
    }
}

fn acquire_least_outstanding_endpoint(
    endpoint_states: &Arc<Mutex<Vec<EmbeddingEndpointState>>>,
) -> usize {
    let mut states = endpoint_states
        .lock()
        .expect("embedding endpoint state lock");
    let endpoint_index = states
        .iter()
        .enumerate()
        .min_by_key(|(_, state)| (state.outstanding, state.requests))
        .map(|(index, _)| index)
        .expect("at least one embedding endpoint");
    states[endpoint_index].outstanding += 1;
    states[endpoint_index].requests += 1;
    endpoint_index
}

fn release_embedding_endpoint(
    endpoint_states: &Arc<Mutex<Vec<EmbeddingEndpointState>>>,
    endpoint_index: usize,
    chunk_count: usize,
    truncated_inputs: usize,
    result: &Result<EmbeddingBatchSuccess, EmbeddingBatchFailure>,
) {
    let mut states = endpoint_states
        .lock()
        .expect("embedding endpoint state lock");
    let state = &mut states[endpoint_index];
    state.outstanding = state.outstanding.saturating_sub(1);
    match result {
        Ok(_) => {
            state.chunks += chunk_count;
            state.truncated_inputs += truncated_inputs;
        }
        Err(_) => state.failures += 1,
    }
}

fn embed_batch_on_endpoint(
    client: &OpenAiCompatibleClient,
    endpoint_config: &EmbeddingEndpointPoolConfig,
    work: &EmbeddingBatchWork,
) -> Result<EmbeddingBatchSuccess, EmbeddingBatchFailure> {
    let mut truncated_inputs = 0usize;
    let input_texts = work
        .inputs
        .iter()
        .map(|input| {
            let (text, truncated) =
                embedding_request_text(input.embedding_text.as_str(), &endpoint_config.config);
            if truncated {
                truncated_inputs += 1;
            }
            text
        })
        .collect::<Vec<_>>();
    let input_text_refs = input_texts
        .iter()
        .map(|input| input.as_ref())
        .collect::<Vec<_>>();
    let embeddings = embed_batch_with_retries(
        client,
        &input_text_refs,
        &endpoint_config.expected_fingerprint,
    )
    .map_err(|error| {
        let chunk_id = work
            .inputs
            .first()
            .map(|input| input.chunk_id.as_str())
            .unwrap_or("<empty-batch>");
        let mut object = embedding_error_object_with_context(error, chunk_id);
        object.message = format!(
            "embedding endpoint `{}` failed: {}",
            endpoint_config.base_url, object.message
        );
        EmbeddingBatchFailure { error: object }
    })?;
    let embeddings = work
        .inputs
        .iter()
        .zip(embeddings)
        .map(|(input, embedding)| OwnedChunkEmbedding {
            chunk_id: input.chunk_id.clone(),
            embedding_literal: pgvector_literal(&embedding.values),
        })
        .collect();
    Ok(EmbeddingBatchSuccess {
        embeddings,
        truncated_inputs,
    })
}

fn embed_batch_with_retries(
    client: &OpenAiCompatibleClient,
    input_texts: &[&str],
    expected_fingerprint: &EmbeddingFingerprint,
) -> Result<Vec<jurisearch_embed::EmbeddingVector>, jurisearch_embed::EmbeddingError> {
    let attempts = EMBEDDING_ENDPOINT_MAX_ATTEMPTS.max(1);
    let mut attempt = 1usize;
    loop {
        match client.embed_batch(input_texts, expected_fingerprint) {
            Ok(embeddings) => return Ok(embeddings),
            Err(error) if attempt < attempts && retryable_embedding_error(&error) => {
                thread::sleep(Duration::from_millis(250 * attempt as u64));
                attempt += 1;
            }
            Err(error) => return Err(error),
        }
    }
}

fn retryable_embedding_error(error: &jurisearch_embed::EmbeddingError) -> bool {
    matches!(
        error,
        jurisearch_embed::EmbeddingError::Endpoint(_)
            | jurisearch_embed::EmbeddingError::InvalidResponse(_)
    )
}

fn embedding_request_text<'a>(input: &'a str, config: &EmbeddingConfig) -> (Cow<'a, str>, bool) {
    let Some(max_input_chars) = embedding_request_char_budget(config) else {
        return (Cow::Borrowed(input), false);
    };
    if max_input_chars == 0 {
        return (Cow::Borrowed(input), false);
    }
    for (chars, (index, _)) in input.char_indices().enumerate() {
        if chars == max_input_chars {
            return (Cow::Owned(input[..index].to_owned()), true);
        }
    }
    (Cow::Borrowed(input), false)
}

fn embedding_request_char_budget(config: &EmbeddingConfig) -> Option<usize> {
    let token_char_budget = config
        .max_estimated_tokens
        .map(|tokens| tokens.saturating_mul(config.estimated_chars_per_token.max(1)));
    match (config.max_input_chars, token_char_budget) {
        (Some(chars), Some(token_chars)) => Some(chars.min(token_chars)),
        (Some(chars), None) => Some(chars),
        (None, Some(token_chars)) => Some(token_chars),
        (None, None) => None,
    }
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

fn open_index_for_bulk_ingest(index_dir: &Path) -> Result<ManagedPostgres, ErrorObject> {
    let pg_config = PgConfig::discover().map_err(storage_error_object)?;
    ManagedPostgres::start_durable_with_profile(
        pg_config,
        index_dir,
        PostgresRuntimeProfile::BulkIngest,
    )
    .map_err(storage_error_object)
}

#[derive(Debug)]
struct LoadedEmbeddingConfig {
    config: EmbeddingConfig,
    pool_endpoints: Vec<EmbeddingPoolEndpoint>,
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
    base_urls: Option<Vec<String>>,
    pool: Option<Vec<EmbeddingPoolEndpointConfigFile>>,
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

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct EmbeddingPoolEndpointConfigFile {
    base_url: String,
    request_model: Option<String>,
    api_key_env: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EmbeddingPoolEndpoint {
    base_url: String,
    request_model: Option<String>,
    api_key_env: Option<String>,
    api_key: Option<String>,
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

pub(crate) fn embedding_config_from_env() -> EmbeddingConfig {
    loaded_embedding_config().config
}

fn loaded_embedding_config() -> LoadedEmbeddingConfig {
    let mut embedding_config = EmbeddingConfig::phase0_bge_m3("http://127.0.0.1:8097/v1", None);
    let mut pool_endpoints = Vec::new();
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
                            apply_embedding_file_config(
                                &mut embedding_config,
                                &mut pool_endpoints,
                                embedding,
                            );
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

    apply_embedding_env_overrides(&mut embedding_config, &mut pool_endpoints);

    LoadedEmbeddingConfig {
        config: embedding_config,
        pool_endpoints,
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

fn apply_embedding_file_config(
    config: &mut EmbeddingConfig,
    pool_endpoints: &mut Vec<EmbeddingPoolEndpoint>,
    file_config: EmbeddingConfigFile,
) {
    if let Some(provider) = file_config.provider {
        config.provider = provider;
        if matches!(provider, EmbeddingProvider::InProcess) {
            config.base_url = None;
            config.base_urls.clear();
            config.api_key = None;
        }
    }
    if let Some(base_url) = nonempty_string(file_config.base_url) {
        config.provider = EmbeddingProvider::OpenAiCompatible;
        config.base_url = Some(base_url.clone());
        config.base_urls = vec![base_url];
    }
    if let Some(base_urls) = nonempty_string_list(file_config.base_urls) {
        config.provider = EmbeddingProvider::OpenAiCompatible;
        config.base_urls = base_urls;
        if config.base_url.is_none() {
            config.base_url = config.base_urls.first().cloned();
        }
    }
    if let Some(pool) = parse_embedding_pool_file_config(file_config.pool) {
        // A pool is an HTTP transport choice; it deliberately overrides local
        // in-process mode in the same config layer.
        config.provider = EmbeddingProvider::OpenAiCompatible;
        *pool_endpoints = pool;
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
    if matches!(config.provider, EmbeddingProvider::InProcess) {
        pool_endpoints.clear();
    }
}

fn apply_embedding_env_overrides(
    embedding_config: &mut EmbeddingConfig,
    pool_endpoints: &mut Vec<EmbeddingPoolEndpoint>,
) {
    if let Ok(provider) = std::env::var("JURISEARCH_EMBED_PROVIDER")
        && let Some(provider) = parse_embedding_provider(&provider)
    {
        embedding_config.provider = provider;
        if matches!(provider, EmbeddingProvider::InProcess) {
            embedding_config.base_url = None;
            embedding_config.base_urls.clear();
            embedding_config.api_key = None;
        }
    }
    if let Ok(base_url) = std::env::var("JURISEARCH_EMBED_BASE_URL")
        && let Some(base_url) = nonempty_string(Some(base_url))
    {
        embedding_config.provider = EmbeddingProvider::OpenAiCompatible;
        embedding_config.base_url = Some(base_url.clone());
        embedding_config.base_urls = vec![base_url];
    }
    if let Ok(base_urls) = std::env::var("JURISEARCH_EMBED_BASE_URLS")
        && let Some(base_urls) = parse_embedding_base_urls_env(&base_urls)
    {
        embedding_config.provider = EmbeddingProvider::OpenAiCompatible;
        embedding_config.base_urls = base_urls;
        if embedding_config.base_url.is_none() {
            embedding_config.base_url = embedding_config.base_urls.first().cloned();
        }
    }
    if let Ok(pool) = std::env::var("JURISEARCH_EMBED_POOL")
        && let Some(pool) = parse_embedding_pool_env(&pool)
    {
        // A pool is an HTTP transport choice; it deliberately overrides local
        // in-process mode in the same env layer.
        embedding_config.provider = EmbeddingProvider::OpenAiCompatible;
        *pool_endpoints = pool;
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
    if matches!(embedding_config.provider, EmbeddingProvider::InProcess) {
        pool_endpoints.clear();
    }
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

fn nonempty_string_list(values: Option<Vec<String>>) -> Option<Vec<String>> {
    let values = values?
        .into_iter()
        .filter_map(|value| nonempty_string(Some(value)))
        .collect::<Vec<_>>();
    if values.is_empty() {
        None
    } else {
        Some(values)
    }
}

fn parse_embedding_base_urls_env(value: &str) -> Option<Vec<String>> {
    let values = value
        .split(|character: char| character == ',' || character == ';' || character.is_whitespace())
        .filter_map(|value| nonempty_string(Some(value.to_owned())))
        .collect::<Vec<_>>();
    if values.is_empty() {
        None
    } else {
        Some(values)
    }
}

fn parse_embedding_pool_file_config(
    endpoints: Option<Vec<EmbeddingPoolEndpointConfigFile>>,
) -> Option<Vec<EmbeddingPoolEndpoint>> {
    let endpoints = endpoints?
        .into_iter()
        .filter_map(|endpoint| {
            let base_url = nonempty_string(Some(endpoint.base_url))?;
            let request_model = nonempty_string(endpoint.request_model);
            let api_key_env = nonempty_string(endpoint.api_key_env);
            Some(embedding_pool_endpoint(
                base_url,
                request_model,
                api_key_env,
            ))
        })
        .collect::<Vec<_>>();
    if endpoints.is_empty() {
        None
    } else {
        Some(endpoints)
    }
}

fn parse_embedding_pool_env(value: &str) -> Option<Vec<EmbeddingPoolEndpoint>> {
    let endpoints = value
        .split([';', '\n'])
        .filter_map(|endpoint| {
            let mut parts = endpoint.split('|');
            let base_url = nonempty_string(parts.next().map(str::to_owned))?;
            let request_model = nonempty_string(parts.next().map(str::to_owned));
            let api_key_env = nonempty_string(parts.next().map(str::to_owned));
            Some(embedding_pool_endpoint(
                base_url,
                request_model,
                api_key_env,
            ))
        })
        .collect::<Vec<_>>();
    if endpoints.is_empty() {
        None
    } else {
        Some(endpoints)
    }
}

fn embedding_pool_endpoint(
    base_url: String,
    request_model: Option<String>,
    api_key_env: Option<String>,
) -> EmbeddingPoolEndpoint {
    let api_key = api_key_env
        .as_deref()
        .and_then(|env_name| std::env::var(env_name).ok())
        .and_then(|api_key| nonempty_string(Some(api_key)));
    EmbeddingPoolEndpoint {
        base_url,
        request_model,
        api_key_env,
        api_key,
    }
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
        config.base_urls.clear();
        config.api_key = None;
        config.request_model = None;
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

fn embedding_pool_endpoints_status_json(endpoints: &[EmbeddingPoolEndpoint]) -> Vec<Value> {
    endpoints
        .iter()
        .map(|endpoint| {
            json!({
                "base_url": endpoint.base_url,
                "request_model": endpoint.request_model,
                "api_key_env": endpoint.api_key_env,
                "api_key_configured": endpoint.api_key.is_some()
            })
        })
        .collect()
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

pub(crate) fn model_fetch_payload(model: Option<String>, allow_download: bool) -> Result<Value, ErrorObject> {
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

pub(crate) fn setup_payload() -> Value {
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
            "pool": embedding_pool_endpoints_status_json(&loaded_embedding.pool_endpoints),
            "pool_overrides_base_urls": !loaded_embedding.pool_endpoints.is_empty(),
            "config_path": loaded_embedding.config_path.as_ref().map(|path| path.display().to_string()),
            "config_loaded": loaded_embedding.config_loaded,
            "config_error": loaded_embedding.config_error,
            "model_cache": model_cache_status_json(&model_cache),
            "endpoint": endpoint
        }
    })
}

pub(crate) fn ensure_embedding_runtime_ready(
    embedding_config: &EmbeddingConfig,
    allow_download: bool,
) -> Result<(), ErrorObject> {
    let model_cache = model_cache_status(embedding_config);
    embedding_config
        .ensure_in_process_ready(model_cache.model_present(), allow_download)
        .map_err(embedding_error_object)
}

pub(crate) fn replay_snapshot_mode(deep: bool) -> ReplaySnapshotMode {
    if deep {
        ReplaySnapshotMode::Refresh
    } else {
        ReplaySnapshotMode::Cached
    }
}

pub(crate) fn status_payload(index_dir: Option<&Path>, replay_snapshot_mode: ReplaySnapshotMode) -> Value {
    let loaded_embedding = loaded_embedding_config();
    let embedding_config = loaded_embedding.config;
    let model_cache = model_cache_status(&embedding_config);
    let endpoint = embedding_endpoint_status_json(&embedding_config);
    let embedding_base_url = embedding_config.base_url.clone().unwrap_or_default();
    let embedding_manifest = embedding_config.manifest();
    let embedding_fingerprint = embedding_manifest.fingerprint.clone();
    let (index, ingest_health, corpus_sources, zone_retrieval) =
        status_index_and_ingest_health(index_dir, replay_snapshot_mode);
    let phase1_gate = phase1_gate_payload(&index, &ingest_health);
    let phase2_gate = phase2_gate_payload(&index, &ingest_health, &corpus_sources);

    json!({
        "schema_version": SCHEMA_VERSION,
        "index": index,
        "embedding": {
            "provider": embedding_fingerprint.provider,
            "base_url": embedding_base_url,
            "base_urls": embedding_config.base_urls.clone(),
            "base_url_class": embedding_fingerprint.base_url_class,
            "model": embedding_fingerprint.model,
            "request_model": embedding_config.request_model.clone(),
            "pool_overrides_base_urls": !loaded_embedding.pool_endpoints.is_empty(),
            "dimension": embedding_fingerprint.dimension,
            "normalize": embedding_fingerprint.normalize,
            "pooling": embedding_fingerprint.pooling,
            "max_input_chars": embedding_config.max_input_chars,
            "max_estimated_tokens": embedding_config.max_estimated_tokens,
            "estimated_chars_per_token": embedding_config.estimated_chars_per_token,
            "token_count_method": embedding_config.configured_token_count_method(),
            "tokenizer_path": embedding_config.tokenizer_path.as_ref().map(|path| path.display().to_string()),
            "pool": embedding_pool_endpoints_status_json(&loaded_embedding.pool_endpoints),
            "provisional": embedding_manifest.provisional,
            "reembeddable": embedding_manifest.reembeddable,
            "config_path": loaded_embedding.config_path.as_ref().map(|path| path.display().to_string()),
            "config_loaded": loaded_embedding.config_loaded,
            "config_error": loaded_embedding.config_error,
            "model_cache": model_cache_status_json(&model_cache),
            "endpoint": endpoint
        },
        "ingest_health": ingest_health,
        "corpus_sources": corpus_sources,
        "zone_retrieval": zone_retrieval,
        "phase1_gate": phase1_gate,
        "phase2_gate": phase2_gate
    })
}

fn doctor_check(name: &str, status: &str, detail: Value) -> Value {
    json!({ "name": name, "status": status, "detail": detail })
}

/// Non-owning dependency preflight: verifies the embedding config/endpoint/model, the Postgres
/// runtime + required extension assets (pg_search, vector), and index-dir presence — WITHOUT
/// starting or owning the index Postgres (so it never fights a running instance). For deep
/// index/ingest readiness (migrations, query-readiness) run `status`.
pub(crate) fn doctor_payload(index_dir: Option<&Path>) -> Value {
    let mut checks: Vec<Value> = Vec::new();
    let mut ready = true;

    let loaded = loaded_embedding_config();

    // 1. Embedding configuration loads cleanly.
    match &loaded.config_error {
        None => checks.push(doctor_check("embedding_config", "pass", json!("loaded"))),
        Some(error) => {
            ready = false;
            checks.push(doctor_check("embedding_config", "fail", json!(error)));
        }
    }

    // 2. Embedding endpoint reachability (TCP probe; non-applicable for in-process).
    let endpoint = embedding_endpoint_status_json(&loaded.config);
    let endpoint_state = endpoint["state"].as_str().unwrap_or("not_checked");
    let endpoint_status = match endpoint_state {
        "reachable" => "pass",
        "unreachable" | "invalid" => "fail",
        _ => "warn",
    };
    if endpoint_status == "fail" {
        ready = false;
    }
    checks.push(doctor_check("embedding_endpoint", endpoint_status, endpoint));

    // 3. Model cache present when an in-process model is required.
    let model_cache = model_cache_status(&loaded.config);
    if !model_cache.required {
        checks.push(doctor_check("model_cache", "not_required", json!("in-process model not required")));
    } else if model_cache.model_present() {
        checks.push(doctor_check("model_cache", "pass", json!("model present")));
    } else {
        ready = false;
        checks.push(doctor_check(
            "model_cache",
            "fail",
            json!("model not cached; run `jurisearch model fetch --allow-download`"),
        ));
    }

    // 4. Postgres runtime + required extension assets (filesystem only — no server start).
    match PgConfig::discover() {
        Ok(pg_config) => {
            checks.push(doctor_check("pg_config", "pass", json!(pg_config.version.trim())));
            for extension in ["pg_search", "vector"] {
                if pg_config.has_extension_assets(extension) {
                    checks.push(doctor_check(
                        "extension_assets",
                        "pass",
                        json!(format!("{extension} assets present")),
                    ));
                } else {
                    ready = false;
                    checks.push(doctor_check(
                        "extension_assets",
                        "fail",
                        json!(format!("{extension} assets missing")),
                    ));
                }
            }
        }
        Err(error) => {
            ready = false;
            checks.push(doctor_check("pg_config", "fail", json!(error.to_string())));
        }
    }

    // 5. Index directory presence (does not open it).
    match index_dir {
        Some(path) if path.exists() => {
            checks.push(doctor_check("index_dir", "pass", json!(path.display().to_string())))
        }
        Some(path) => {
            ready = false;
            checks.push(doctor_check(
                "index_dir",
                "fail",
                json!(format!("index directory not found: {}", path.display())),
            ));
        }
        None => checks.push(doctor_check(
            "index_dir",
            "warn",
            json!("no --index-dir / $JURISEARCH_INDEX_DIR set"),
        )),
    }

    // 6. Configured embedding fingerprint (non-owning config read). The index-side compatibility
    // (stored vs configured fingerprint) requires opening the index, so it is deferred to `status`.
    let fingerprint = loaded.config.manifest().fingerprint;
    checks.push(doctor_check(
        "embedding_fingerprint",
        "pass",
        json!({
            "model": fingerprint.model,
            "dimension": fingerprint.dimension,
            "normalize": fingerprint.normalize,
            "pooling": fingerprint.pooling,
            "index_compatibility": "deferred — verified by `status` (opens the index)"
        }),
    ));

    // 7. Index schema/migrations & query-readiness require opening the index (which doctor must not
    // do), so they are reported explicitly as deferred rather than silently omitted.
    checks.push(doctor_check(
        "index_schema_and_readiness",
        "warn",
        json!(format!(
            "migration version (binary expects {CURRENT_SCHEMA_VERSION}) and query/replay readiness require opening the index; run `status --deep`"
        )),
    ));

    json!({
        "schema_version": SCHEMA_VERSION,
        "ready": ready,
        "checks": checks,
        "note": "Non-owning preflight: the index Postgres is not started. Checks that require opening the index (schema/migrations, query-readiness, fingerprint compatibility) are deferred to `status --deep`."
    })
}

pub(crate) fn stats_payload(index_dir: Option<&Path>) -> Result<Value, ErrorObject> {
    let index_dir = require_existing_index_dir(index_dir)?;
    let postgres = open_index(index_dir.as_path())?;
    let response = corpus_stats_json(&postgres).map_err(storage_error_object)?;
    let stats: Value = serde_json::from_str(&response)
        .map_err(|error| dependency_unavailable(error.to_string()))?;
    Ok(json!({ "schema_version": SCHEMA_VERSION, "stats": stats }))
}

pub(crate) fn inspect_payload(args: InspectArgs, index_dir: Option<&Path>) -> Result<Value, ErrorObject> {
    let index_dir = require_existing_index_dir(index_dir)?;
    let postgres = open_index(index_dir.as_path())?;
    ensure_query_readiness(&postgres, QueryReadinessGate::Fetch)?;
    let response = inspect_document_json(&postgres, &args.id).map_err(storage_error_object)?;
    let value: Value = serde_json::from_str(&response)
        .map_err(|error| dependency_unavailable(error.to_string()))?;
    if value["document"].is_null() {
        return Err(no_results(format!("no document with id `{}`", args.id)));
    }
    Ok(value)
}

pub(crate) fn versions_payload(args: VersionsArgs, index_dir: Option<&Path>) -> Result<Value, ErrorObject> {
    let index_dir = require_existing_index_dir(index_dir)?;
    let postgres = open_index(index_dir.as_path())?;
    ensure_query_readiness(&postgres, QueryReadinessGate::Fetch)?;
    let response = document_versions_json(&postgres, &args.id).map_err(storage_error_object)?;
    let value: Value = serde_json::from_str(&response)
        .map_err(|error| dependency_unavailable(error.to_string()))?;
    // An empty family means the id is unknown (the target is always its own family member).
    if value["count"].as_u64() == Some(0) {
        return Err(no_results(format!(
            "no document/version family for id `{}`",
            args.id
        )));
    }
    Ok(value)
}

pub(crate) fn diff_payload(args: DiffArgs, index_dir: Option<&Path>) -> Result<Value, ErrorObject> {
    if args.id.trim().is_empty() {
        return Err(ErrorObject::bad_input("diff requires a document id"));
    }
    if !is_valid_iso_date(&args.from) || !is_valid_iso_date(&args.to) {
        return Err(ErrorObject::bad_input(
            "diff --from and --to must be YYYY-MM-DD dates",
        ));
    }
    let index_dir = require_existing_index_dir(index_dir)?;
    let postgres = open_index(index_dir.as_path())?;
    ensure_query_readiness(&postgres, QueryReadinessGate::Fetch)?;
    let response = document_diff_json(&postgres, &args.id, &args.from, &args.to)
        .map_err(storage_error_object)?;
    let mut value: Value = serde_json::from_str(&response)
        .map_err(|error| dependency_unavailable(error.to_string()))?;
    if value["family_count"].as_u64() == Some(0) {
        return Err(no_results(format!(
            "no document/version family for id `{}`",
            args.id
        )));
    }
    // Distinguish "no version in force on a date" from "version unchanged".
    if let Some(map) = value.as_object_mut() {
        let missing_from = map.get("from_version").map(Value::is_null).unwrap_or(true);
        let missing_to = map.get("to_version").map(Value::is_null).unwrap_or(true);
        map.insert("missing_from".to_owned(), Value::Bool(missing_from));
        map.insert("missing_to".to_owned(), Value::Bool(missing_to));
    }
    Ok(value)
}

fn phase1_gate_payload(index: &Value, ingest_health: &Value) -> Value {
    let external_benchmark = phase1_external_benchmark_payload();
    let france_legi = phase1_france_legi_payload();
    phase1_gate_payload_with(index, ingest_health, external_benchmark, france_legi)
}

// Pure gate builder: takes the already-resolved benchmark payloads so tests do not depend on the
// `JURISEARCH_PHASE1_*_BENCHMARK` ambient env vars. The public `phase1_gate_payload` resolves
// those from the environment and delegates here.
fn phase1_gate_payload_with(
    index: &Value,
    ingest_health: &Value,
    external_benchmark: Value,
    france_legi: Value,
) -> Value {
    let eval_summary = phase1_eval_fixture_summary();
    let ingest_available = ingest_health["state"] == "available";
    let query_ready = index["query_ready"].as_bool().unwrap_or(false);
    let locked_embedding_model = phase1_embedding_model_locked(ingest_health);
    let reranker_decision = phase1_reranker_decision_payload();
    let external_benchmark_status = phase1_external_benchmark_check_status(&external_benchmark);
    let france_legi_status = phase1_france_legi_check_status(&france_legi);
    let replay_snapshot_status = ingest_health["replay_snapshot_status"]
        .as_str()
        .unwrap_or("unknown");
    let replay_snapshot_source = ingest_health["replay_snapshot_source"]
        .as_str()
        .unwrap_or("unknown");
    let replay_snapshot_message = format!(
        "replay snapshot signatures over canonical projections must be available; status={replay_snapshot_status}, source={replay_snapshot_source}"
    );

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
            replay_snapshot_message,
        ),
        phase1_gate_check_advisory(
            "external_expert_annotated_eval",
            external_benchmark_status,
            "Advisory cross-lingual robustness signal (BSARD, Belgian statutory). Not a Phase 1 release gate: jurisdiction-correct release evidence is `france_legi_official_eval`",
        ),
        phase1_gate_check(
            "france_legi_official_eval",
            france_legi_status,
            "Phase 1 requires a passing France-LEGI official-evidence benchmark — gating on intent-routed structured citation resolution and temporal version pinning, with full-body semantic retrieval advisory — run through the production pipeline; jurisdiction-correct release evidence, unlike the Belgian BSARD proxy",
        ),
        phase1_gate_check(
            "final_embedding_model",
            if locked_embedding_model {
                "pass"
            } else {
                "fail"
            },
            if locked_embedding_model {
                "stored embedding manifest matches the locked D21 bge-m3 v1 model"
            } else {
                "stored embedding manifest must match D21: bge-m3, 1024 dimensions, normalized embeddings"
            },
        ),
        phase1_gate_check(
            "reranker_decision",
            "pass",
            "reranker adoption is deferred for Phase 1; disabled provider remains the default until legal eval proves a material rerank gain",
        ),
    ];
    // Advisory checks (`gating: false`) are reported but do not block the claim.
    let claim_allowed = checks
        .iter()
        .filter(|check| check["gating"].as_bool() != Some(false))
        .all(|check| check["status"].as_str() == Some("pass"));

    json!({
        "state": if claim_allowed { "ready" } else { "not_ready" },
        "claim_allowed": claim_allowed,
        "scope": "phase1_legi_statutory_search",
        "checks": checks,
        "eval_fixtures": eval_summary,
        "external_benchmark": external_benchmark,
        "france_legi_benchmark": france_legi,
        "reranker_decision": reranker_decision,
    })
}

fn phase1_external_benchmark_payload() -> Value {
    let artifact_path = std::env::var_os(PHASE1_EXTERNAL_BENCHMARK_ENV).map(PathBuf::from);
    phase1_external_benchmark_payload_with_path(artifact_path.as_deref())
}

fn phase1_external_benchmark_payload_with_path(artifact_path: Option<&Path>) -> Value {
    let mut payload = phase1_external_benchmark_default_payload();
    let Some(artifact_path) = artifact_path else {
        return payload;
    };

    payload["artifact_path"] = json!(artifact_path.to_string_lossy());
    payload["source"] = json!(PHASE1_EXTERNAL_BENCHMARK_ENV);
    let contents = match fs::read_to_string(artifact_path) {
        Ok(contents) => contents,
        Err(error) => {
            payload["state"] = json!("failed");
            payload["artifact_error"] = json!(format!(
                "failed to read external benchmark artifact `{}`: {error}",
                artifact_path.display()
            ));
            return payload;
        }
    };
    let artifact = match serde_json::from_str::<Value>(&contents) {
        Ok(artifact) => artifact,
        Err(error) => {
            payload["state"] = json!("failed");
            payload["artifact_error"] = json!(format!(
                "failed to parse external benchmark artifact `{}` as JSON: {error}",
                artifact_path.display()
            ));
            return payload;
        }
    };

    payload["artifact"] = artifact.clone();
    payload["evidence"] = artifact["evidence"]
        .as_array()
        .map(|_| artifact["evidence"].clone())
        .unwrap_or_else(|| json!([]));
    payload["metrics"] = artifact["metrics"].clone();
    payload["thresholds"] = artifact["thresholds"].clone();
    payload["dataset"] = artifact["dataset"].clone();
    payload["artifact_error"] = Value::Null;

    let validation_errors = phase1_external_benchmark_artifact_errors(&artifact);
    if validation_errors.is_empty() {
        payload["state"] = json!(artifact["state"].as_str().unwrap_or("pending"));
    } else {
        payload["state"] = json!("failed");
        payload["artifact_error"] = json!(validation_errors.join("; "));
    }

    payload
}

fn phase1_external_benchmark_default_payload() -> Value {
    json!({
        "state": "pending",
        "source": "not_configured",
        "artifact_path": null,
        "artifact_error": null,
        "decision_date": "2026-06-22",
        "primary_candidate": "maastrichtlawtech/bsard",
        "claim_scope": "external expert-annotated French-language statutory retrieval benchmark, not France-LEGI human-reviewed gold",
        "jurisdiction": "belgium",
        "usage_scope": "eval_only",
        "required_evidence": [
            "dataset access and license recorded",
            "dataset corpus/questions/qrels imported or adapted without training leakage; the runner may be an external Python harness",
            "bm25, dense, and hybrid retrieval metrics recorded with top-k, recall, and nDCG",
            "metrics artifact path recorded for status to consume before this gate can pass",
            "Phase 1 adoption threshold documented before claim_allowed can become true"
        ],
        "dataset": null,
        "metrics": null,
        "thresholds": null,
        "artifact": null,
        "evidence": [],
        "candidate_datasets": [
            {
                "id": "maastrichtlawtech/bsard",
                "role": "primary",
                "task": "French statutory article retrieval",
                "labels": "experienced jurists",
                "license": "cc-by-nc-sa-4.0",
                "limitation": "Belgian law, not French LEGI; still French-native statutory retrieval with expert qrels"
            },
            {
                "id": "maastrichtlawtech/lleqa",
                "role": "secondary",
                "task": "French legal QA and retrieval",
                "labels": "seasoned legal professionals",
                "license": "cc-by-nc-sa-4.0 gated research access",
                "limitation": "Belgian law and gated access; useful if access is granted"
            },
            {
                "id": "mteb-private/FrenchLegal1Retrieval-sample",
                "role": "supplemental",
                "task": "French legal retrieval",
                "labels": "sample is public; full task access unclear",
                "license": "private/sample",
                "limitation": "sample-only public dataset cannot be the sole release gate"
            },
            {
                "id": "louisbrulenaudet/tax-retrieval-benchmark",
                "role": "supplemental",
                "task": "French tax retrieval",
                "labels": "domain-specific benchmark labels",
                "license": "gated",
                "limitation": "tax-only scope and gated access"
            }
        ],
        "non_gating_inputs": [
            {
                "id": "internal_legi_release_candidates",
                "reason": "source-checked against DILA LEGI but not independently expert-annotated; remains smoke/regression coverage"
            },
            {
                "id": "AgentPublic/legi",
                "reason": "useful LEGI corpus context but no expert retrieval qrels"
            }
        ],
        "reason": "local human legal-domain review is unavailable, so Phase 1 promotion must rely on a passing external expert-annotated legal retrieval benchmark plus internal LEGI smoke evidence"
    })
}

fn phase1_external_benchmark_artifact_errors(artifact: &Value) -> Vec<String> {
    let mut errors = Vec::new();
    let state = artifact["state"].as_str();
    match state {
        Some("pending" | "passed" | "failed") => {}
        Some(other) => errors.push(format!("invalid state `{other}`")),
        None => errors.push("missing state".to_owned()),
    }
    if artifact["kind"].as_str() != Some("phase1_external_expert_benchmark") {
        errors.push("kind must be `phase1_external_expert_benchmark`".to_owned());
    }
    if artifact["schema_version"].as_u64() != Some(1) {
        errors.push("schema_version must be 1".to_owned());
    }
    if state == Some("passed")
        && !artifact["evidence"]
            .as_array()
            .is_some_and(|evidence| !evidence.is_empty())
    {
        errors.push("passed artifact must include non-empty evidence".to_owned());
    }
    for (path, expected) in [
        ("dataset.id", "maastrichtlawtech/bsard"),
        ("dataset.question_split", "test"),
        ("dataset.jurisdiction", "belgium"),
        ("dataset.usage_scope", "eval_only"),
        ("dataset.license", "cc-by-nc-sa-4.0"),
        ("embedding.fingerprint_model", PHASE0_EMBEDDING_MODEL),
    ] {
        if artifact_pointer_str(artifact, path) != Some(expected) {
            errors.push(format!("{path} must be `{expected}`"));
        }
    }
    if artifact_pointer_value(artifact, "embedding.dimension").and_then(Value::as_u64)
        != Some(PHASE0_EMBEDDING_DIMENSION as u64)
    {
        errors.push(format!(
            "embedding.dimension must be {}",
            PHASE0_EMBEDDING_DIMENSION
        ));
    }
    if artifact_pointer_value(artifact, "embedding.normalize").and_then(Value::as_bool)
        != Some(true)
    {
        errors.push("embedding.normalize must be true".to_owned());
    }
    for path in ["dataset.revision", "claim_scope", "applicability"] {
        if artifact_pointer_str(artifact, path).is_none_or(|value| value.trim().is_empty()) {
            errors.push(format!("{path} is required"));
        }
    }
    if artifact_pointer_str(artifact, "dataset.revision") == Some("unknown") {
        errors.push("dataset.revision must be pinned, not `unknown`".to_owned());
    }
    for path in ["thresholds", "metrics"] {
        if artifact_pointer_value(artifact, path).is_none_or(Value::is_null) {
            errors.push(format!("{path} is required"));
        }
    }
    for path in ["dataset.limit_corpus", "dataset.limit_questions"] {
        if artifact_pointer_value(artifact, path).is_some_and(|value| !value.is_null()) {
            errors.push(format!("{path} must be null for a gate artifact"));
        }
    }
    if artifact_pointer_value(artifact, "dataset.corpus_documents")
        .and_then(Value::as_u64)
        .is_none_or(|count| count < PHASE1_EXTERNAL_MIN_BSARD_DOCUMENTS)
    {
        errors.push(format!(
            "dataset.corpus_documents must be at least {}",
            PHASE1_EXTERNAL_MIN_BSARD_DOCUMENTS
        ));
    }
    if artifact_pointer_value(artifact, "dataset.questions")
        .and_then(Value::as_u64)
        .is_none_or(|count| count < PHASE1_EXTERNAL_MIN_BSARD_QUESTIONS)
    {
        errors.push(format!(
            "dataset.questions must be at least {}",
            PHASE1_EXTERNAL_MIN_BSARD_QUESTIONS
        ));
    }
    phase1_validate_external_benchmark_metric(
        artifact,
        "recall_at_20",
        PHASE1_EXTERNAL_MIN_HYBRID_RECALL_AT_20,
        &mut errors,
    );
    phase1_validate_external_benchmark_metric(
        artifact,
        "ndcg_at_20",
        PHASE1_EXTERNAL_MIN_HYBRID_NDCG_AT_20,
        &mut errors,
    );
    phase1_validate_external_benchmark_metric(
        artifact,
        "mrr_at_20",
        PHASE1_EXTERNAL_MIN_HYBRID_MRR_AT_20,
        &mut errors,
    );
    errors
}

fn phase1_validate_external_benchmark_metric(
    artifact: &Value,
    metric_name: &str,
    policy_floor: f64,
    errors: &mut Vec<String>,
) {
    let threshold_path = format!("thresholds.hybrid_{metric_name}_min");
    let metric_path = format!("metrics.hybrid.{metric_name}");
    let threshold = artifact_pointer_f64(artifact, &threshold_path);
    let metric = artifact_pointer_f64(artifact, &metric_path);
    match threshold {
        Some(threshold) if threshold >= policy_floor => {}
        Some(threshold) => errors.push(format!(
            "{threshold_path} must be at least {policy_floor:.3}, got {threshold:.3}"
        )),
        None => errors.push(format!("{threshold_path} is required")),
    }
    if let (Some(metric), Some(threshold)) = (metric, threshold) {
        if metric < threshold {
            errors.push(format!(
                "{metric_path} must be at least threshold {threshold:.3}, got {metric:.3}"
            ));
        }
    } else if metric.is_none() {
        errors.push(format!("{metric_path} is required"));
    }
}

fn artifact_pointer_value<'a>(value: &'a Value, dotted_path: &str) -> Option<&'a Value> {
    let mut current = value;
    for segment in dotted_path.split('.') {
        current = current.get(segment)?;
    }
    Some(current)
}

fn artifact_pointer_str<'a>(value: &'a Value, dotted_path: &str) -> Option<&'a str> {
    artifact_pointer_value(value, dotted_path)?.as_str()
}

fn artifact_pointer_f64(value: &Value, dotted_path: &str) -> Option<f64> {
    artifact_pointer_value(value, dotted_path)?.as_f64()
}

fn phase1_external_benchmark_check_status(external_benchmark: &Value) -> &'static str {
    match external_benchmark["state"].as_str() {
        Some("passed")
            if external_benchmark["evidence"]
                .as_array()
                .is_some_and(|evidence| !evidence.is_empty()) =>
        {
            "pass"
        }
        Some("passed" | "failed") => "fail",
        _ => "pending",
    }
}

fn phase1_france_legi_payload() -> Value {
    let artifact_path = std::env::var_os(PHASE1_FRANCE_LEGI_BENCHMARK_ENV).map(PathBuf::from);
    phase1_france_legi_payload_with_path(artifact_path.as_deref())
}

fn phase1_france_legi_payload_with_path(artifact_path: Option<&Path>) -> Value {
    let mut payload = phase1_france_legi_default_payload();
    let Some(artifact_path) = artifact_path else {
        return payload;
    };

    payload["artifact_path"] = json!(artifact_path.to_string_lossy());
    payload["source"] = json!(PHASE1_FRANCE_LEGI_BENCHMARK_ENV);
    let contents = match fs::read_to_string(artifact_path) {
        Ok(contents) => contents,
        Err(error) => {
            payload["state"] = json!("failed");
            payload["artifact_error"] = json!(format!(
                "failed to read France-LEGI benchmark artifact `{}`: {error}",
                artifact_path.display()
            ));
            return payload;
        }
    };
    let artifact = match serde_json::from_str::<Value>(&contents) {
        Ok(artifact) => artifact,
        Err(error) => {
            payload["state"] = json!("failed");
            payload["artifact_error"] = json!(format!(
                "failed to parse France-LEGI benchmark artifact `{}` as JSON: {error}",
                artifact_path.display()
            ));
            return payload;
        }
    };

    payload["artifact"] = artifact.clone();
    payload["evidence"] = artifact["evidence"]
        .as_array()
        .map(|_| artifact["evidence"].clone())
        .unwrap_or_else(|| json!([]));
    payload["categories"] = artifact["categories"].clone();
    payload["thresholds"] = artifact["thresholds"].clone();
    payload["provenance"] = artifact["provenance"].clone();
    payload["artifact_error"] = Value::Null;

    let validation_errors = phase1_france_legi_artifact_errors(&artifact);
    if validation_errors.is_empty() {
        payload["state"] = json!(artifact["state"].as_str().unwrap_or("pending"));
    } else {
        payload["state"] = json!("failed");
        payload["artifact_error"] = json!(validation_errors.join("; "));
    }

    payload
}

fn phase1_france_legi_default_payload() -> Value {
    json!({
        "state": "pending",
        "source": "not_configured",
        "artifact_path": null,
        "artifact_error": null,
        "decision_date": "2026-06-22",
        "claim_scope": "France-LEGI official-evidence retrieval with intent routing: structured citation resolution and temporal version pinning (gating), plus advisory full-body semantic retrieval, through the production pipeline",
        "jurisdiction": "france",
        "retriever": "production jurisearch search (BM25 + dense + RRF)",
        "required_evidence": [
            "gold derived only from official DILA/Légifrance fields (no human, no LLM): ID/NUM/TITRE_TXT for structured citation resolution, CID/DATE_DEBUT/DATE_FIN for temporal version pinning, LIEN CITATION targets for advisory semantic retrieval",
            "retrieval executed through the production search pipeline, not a proxy harness",
            "per-category metrics recorded with query counts and the locked bge-m3 fingerprint",
            "per-category thresholds at or above policy floors recorded for status to consume before this gate can pass",
            "structured provenance: pinned official_source + source_revision, production pipeline + code_version + index_revision, and sampled=false / human_in_gold=false / llm_in_gold=false"
        ],
        "categories": null,
        "thresholds": null,
        "provenance": null,
        "artifact": null,
        "evidence": [],
        "reason": "BSARD is a Belgian proxy; a jurisdiction-correct France-LEGI official-evidence benchmark over the production pipeline is the release-gating signal. Gold is structurally derived from official Légifrance fields, so it needs no human annotation. See work/03-implementation/02-evidence/2026-06-22-france-legi-official-evidence-benchmark-feasibility.md"
    })
}

fn phase1_france_legi_artifact_errors(artifact: &Value) -> Vec<String> {
    let mut errors = Vec::new();
    match artifact["state"].as_str() {
        Some("pending" | "passed" | "failed") => {}
        Some(other) => errors.push(format!("invalid state `{other}`")),
        None => errors.push("missing state".to_owned()),
    }
    if artifact["kind"].as_str() != Some("phase1_france_legi_benchmark") {
        errors.push("kind must be `phase1_france_legi_benchmark`".to_owned());
    }
    if artifact["schema_version"].as_u64() != Some(1) {
        errors.push("schema_version must be 1".to_owned());
    }
    if artifact["jurisdiction"].as_str() != Some("france") {
        errors.push("jurisdiction must be `france`".to_owned());
    }
    if artifact["state"].as_str() == Some("passed")
        && !artifact["evidence"]
            .as_array()
            .is_some_and(|evidence| !evidence.is_empty())
    {
        errors.push("passed artifact must include non-empty evidence".to_owned());
    }
    if artifact_pointer_str(artifact, "embedding.fingerprint_model") != Some(PHASE0_EMBEDDING_MODEL)
    {
        errors.push(format!(
            "embedding.fingerprint_model must be `{PHASE0_EMBEDDING_MODEL}`"
        ));
    }
    if artifact_pointer_value(artifact, "embedding.dimension").and_then(Value::as_u64)
        != Some(PHASE0_EMBEDDING_DIMENSION as u64)
    {
        errors.push(format!(
            "embedding.dimension must be {}",
            PHASE0_EMBEDDING_DIMENSION
        ));
    }
    if artifact_pointer_value(artifact, "embedding.normalize").and_then(Value::as_bool)
        != Some(true)
    {
        errors.push("embedding.normalize must be true".to_owned());
    }
    for path in ["claim_scope", "source", "retriever"] {
        if artifact_pointer_str(artifact, path).is_none_or(|value| value.trim().is_empty()) {
            errors.push(format!("{path} is required"));
        }
    }
    // Structured provenance: the gate must not accept a proxy runner that only supplies
    // good-looking category metrics. Require pinned official-source + production-pipeline
    // identity, and assert the gold is structurally derived (no human, no LLM) over a full,
    // unsampled qrel set.
    for path in [
        "provenance.official_source",
        "provenance.source_revision",
        "provenance.pipeline",
        "provenance.code_version",
        "provenance.index_revision",
    ] {
        if artifact_pointer_str(artifact, path).is_none_or(|value| value.trim().is_empty()) {
            errors.push(format!("{path} is required"));
        }
    }
    if artifact_pointer_str(artifact, "provenance.source_revision")
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("unknown"))
    {
        errors.push("provenance.source_revision must be pinned, not `unknown`".to_owned());
    }
    for (path, message) in [
        (
            "provenance.sampled",
            "provenance.sampled must be false (qrels must be deterministic, not randomly sampled or cherry-picked; a reproducible bounded set recorded under provenance.qrel_limits is acceptable)",
        ),
        (
            "provenance.human_in_gold",
            "provenance.human_in_gold must be false (France-LEGI gold is structurally derived from official fields)",
        ),
        (
            "provenance.llm_in_gold",
            "provenance.llm_in_gold must be false (France-LEGI gold is structurally derived from official fields)",
        ),
    ] {
        if artifact_pointer_value(artifact, path).and_then(Value::as_bool) != Some(false) {
            errors.push(message.to_owned());
        }
    }
    for path in ["categories", "thresholds"] {
        if artifact_pointer_value(artifact, path).is_none_or(Value::is_null) {
            errors.push(format!("{path} is required"));
        }
    }
    // Two structured categories GATE the claim at high floors; semantic_retrieval is advisory.
    phase1_france_legi_validate_category(
        artifact,
        "structured_citation_resolution",
        "structured_citation_recall_at_10",
        PHASE1_FRANCE_LEGI_MIN_STRUCTURED_CITATION_RECALL_AT_10,
        PHASE1_FRANCE_LEGI_MIN_STRUCTURED_CITATION_QUERIES,
        false,
        &mut errors,
    );
    phase1_france_legi_validate_category(
        artifact,
        "temporal_version_pinning",
        "temporal_version_exactness_at_10",
        PHASE1_FRANCE_LEGI_MIN_TEMPORAL_VERSION_EXACTNESS_AT_10,
        PHASE1_FRANCE_LEGI_MIN_TEMPORAL_QUERIES,
        false,
        &mut errors,
    );
    phase1_france_legi_validate_category(
        artifact,
        "semantic_retrieval",
        "semantic_retrieval_recall_at_10",
        PHASE1_FRANCE_LEGI_ADVISORY_SEMANTIC_RECALL_AT_10,
        PHASE1_FRANCE_LEGI_MIN_SEMANTIC_QUERIES,
        true,
        &mut errors,
    );
    errors
}

fn phase1_france_legi_validate_category(
    artifact: &Value,
    category: &str,
    threshold_key: &str,
    policy_floor: f64,
    min_queries: u64,
    // Gating categories must clear their recorded threshold; advisory categories record their
    // metric but never fail the gate on it (they still require the metric + a minimum query count).
    advisory: bool,
    errors: &mut Vec<String>,
) {
    let suffix = if advisory { "advisory" } else { "min" };
    let threshold_path = format!("thresholds.{threshold_key}_{suffix}");
    let value_path = format!("categories.{category}.metric_value");
    let queries_path = format!("categories.{category}.queries");
    let threshold = artifact_pointer_f64(artifact, &threshold_path);
    let value = artifact_pointer_f64(artifact, &value_path);
    match threshold {
        Some(threshold) if threshold >= policy_floor => {}
        Some(threshold) => errors.push(format!(
            "{threshold_path} must be at least {policy_floor:.3}, got {threshold:.3}"
        )),
        None => errors.push(format!("{threshold_path} is required")),
    }
    if advisory {
        if value.is_none() {
            errors.push(format!("{value_path} is required"));
        }
    } else if let (Some(value), Some(threshold)) = (value, threshold) {
        if value < threshold {
            errors.push(format!(
                "{value_path} must be at least threshold {threshold:.3}, got {value:.3}"
            ));
        }
    } else if value.is_none() {
        errors.push(format!("{value_path} is required"));
    }
    if artifact_pointer_value(artifact, &queries_path)
        .and_then(Value::as_u64)
        .is_none_or(|count| count < min_queries)
    {
        errors.push(format!("{queries_path} must be at least {min_queries}"));
    }
    // Routing-backend audit: the per-query backend accounting must cover EVERY query, and a GATING
    // category must have been resolved entirely by the structured citation resolver. This is the
    // proof the split relies on — that the structured metrics came from input-driven structured
    // resolution, not an answer-aware or fuzzy harness reporting high numbers.
    let backends_path = format!("categories.{category}.routing_backends");
    let queries = artifact_pointer_value(artifact, &queries_path).and_then(Value::as_u64);
    match artifact_pointer_value(artifact, &backends_path).and_then(Value::as_object) {
        Some(backends) => {
            if let Some(queries) = queries {
                let total: u64 = backends.values().filter_map(Value::as_u64).sum();
                if total != queries {
                    errors.push(format!(
                        "{backends_path} must account for all {queries} queries (counted {total})"
                    ));
                }
                if !advisory {
                    let structured = backends
                        .get("structured_citation")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    if structured != queries {
                        errors.push(format!(
                            "{backends_path}.structured_citation must equal queries ({queries}) for a gating category: every query must resolve via the structured citation resolver (got {structured})"
                        ));
                    }
                }
            }
        }
        None => errors.push(format!("{backends_path} is required")),
    }
}

fn phase1_france_legi_check_status(france_legi: &Value) -> &'static str {
    match france_legi["state"].as_str() {
        Some("passed")
            if france_legi["evidence"]
                .as_array()
                .is_some_and(|evidence| !evidence.is_empty()) =>
        {
            "pass"
        }
        Some("passed" | "failed") => "fail",
        _ => "pending",
    }
}

// ===== Phase 2 gate (full French juridic search) ==============================================

/// The fail-closed Phase 2 gate: the "best-in-class French juridic search" claim is allowed only when
/// jurisprudence is ingested, the index is query-ready, bulk zone provenance is reported honestly, and
/// a passing jurisprudence eval benchmark (re-derived from per-category floors, not self-reported) is
/// supplied via `JURISEARCH_PHASE2_BENCHMARK`. Until then `claim_allowed=false` / `state=not_ready`.
fn phase2_gate_payload(index: &Value, ingest_health: &Value, corpus_sources: &Value) -> Value {
    let benchmark = phase2_benchmark_payload();
    phase2_gate_payload_with(index, ingest_health, corpus_sources, benchmark)
}

fn phase2_gate_payload_with(
    index: &Value,
    ingest_health: &Value,
    corpus_sources: &Value,
    benchmark: Value,
) -> Value {
    let query_ready = index["query_ready"].as_bool() == Some(true);
    let ingest_available = ingest_health["state"] == "available";

    // Which DILA bulk jurisprudence sources have a freshness-advancing completed run (status reports
    // them in corpus_sources). cass/capp/inca are judicial; jade is administrative.
    let juri_sources: Vec<&str> = ["cass", "capp", "inca", "jade"]
        .into_iter()
        .filter(|source| corpus_sources.get(source).is_some_and(Value::is_object))
        .collect();
    let judicial_present = juri_sources.iter().any(|s| matches!(*s, "cass" | "capp" | "inca"));
    let administrative_present = juri_sources.contains(&"jade");
    let corpus_present = judicial_present && administrative_present;

    // Honest provenance: every present bulk source must report zone_accurate=false (it must never
    // claim official Judilibre zones without enrichment).
    let honest_zones = !juri_sources.is_empty()
        && juri_sources
            .iter()
            .all(|s| corpus_sources[*s]["zone_accurate"].as_bool() == Some(false));

    let benchmark_status = phase2_benchmark_check_status(&benchmark);

    let checks = vec![
        phase1_gate_check(
            "jurisprudence_corpus_present",
            corpus_present,
            "both judicial (cass/capp/inca) and administrative (jade) DILA bulk jurisprudence must have a completed ingest run",
        ),
        phase1_gate_check(
            "index_query_ready",
            if query_ready { "pass" } else { "pending" },
            "the index must be query-ready (projection + embedding coverage gates pass)",
        ),
        phase1_gate_check(
            "honest_zone_provenance",
            if honest_zones { "pass" } else { "pending" },
            "bulk jurisprudence must report zone_accurate=false; the official-zone fetch gate is met only by Judilibre zone enrichment",
        ),
        phase1_gate_check_advisory(
            "pseudonymisation_preserved",
            if ingest_available { "pass" } else { "pending" },
            "source pseudonymisation is preserved verbatim by the juri parser (unit + real-archive tests); advisory until the release benchmark asserts no re-identification",
        ),
        phase1_gate_check(
            "jurisprudence_eval_benchmark",
            benchmark_status,
            "a passing jurisprudence eval benchmark — Cassation + administrative retrieval AND decision-citation verification through the production pipeline, re-derived against policy floors — is required before the full-juridic claim",
        ),
    ];

    let claim_allowed = checks
        .iter()
        .filter(|check| check["gating"].as_bool() != Some(false))
        .all(|check| check["status"].as_str() == Some("pass"));

    json!({
        "state": if claim_allowed { "ready" } else { "not_ready" },
        "claim_allowed": claim_allowed,
        "scope": "phase2_full_french_juridic_search",
        "checks": checks,
        "jurisprudence_corpus_sources": juri_sources,
        "benchmark": benchmark
    })
}

fn phase2_benchmark_payload() -> Value {
    let artifact_path = std::env::var_os(PHASE2_BENCHMARK_ENV).map(PathBuf::from);
    phase2_benchmark_payload_with_path(artifact_path.as_deref())
}

fn phase2_benchmark_payload_with_path(artifact_path: Option<&Path>) -> Value {
    let mut payload = phase2_benchmark_default_payload();
    let Some(artifact_path) = artifact_path else {
        return payload;
    };
    payload["artifact_path"] = json!(artifact_path.to_string_lossy());
    payload["source"] = json!(PHASE2_BENCHMARK_ENV);
    let contents = match fs::read_to_string(artifact_path) {
        Ok(contents) => contents,
        Err(error) => {
            payload["state"] = json!("failed");
            payload["artifact_error"] = json!(format!(
                "failed to read Phase 2 benchmark artifact `{}`: {error}",
                artifact_path.display()
            ));
            return payload;
        }
    };
    let artifact = match serde_json::from_str::<Value>(&contents) {
        Ok(artifact) => artifact,
        Err(error) => {
            payload["state"] = json!("failed");
            payload["artifact_error"] = json!(format!(
                "failed to parse Phase 2 benchmark artifact `{}` as JSON: {error}",
                artifact_path.display()
            ));
            return payload;
        }
    };
    // Normalize every diagnostic field to its schema-declared shape so the emitted payload always
    // matches the published schema, even for a parseable-but-malformed artifact (e.g. a top-level
    // `[]`/`false`, or an object whose `categories`/`provenance` are not objects).
    let object_or_null = |value: &Value| -> Value {
        if value.is_object() {
            value.clone()
        } else {
            Value::Null
        }
    };
    payload["artifact"] = object_or_null(&artifact);
    payload["categories"] = object_or_null(&artifact["categories"]);
    payload["provenance"] = object_or_null(&artifact["provenance"]);
    payload["evidence"] = artifact["evidence"].as_array().map_or(json!([]), |_| artifact["evidence"].clone());

    let errors = phase2_benchmark_artifact_errors(&artifact);
    // Re-derive the state from the validation, never the artifact's self-reported `state` (which is
    // preserved only as a string-or-null diagnostic). Empty errors over the full contract == passed.
    payload["artifact_reported_state"] =
        artifact["state"].as_str().map_or(Value::Null, |state| json!(state));
    if errors.is_empty() {
        payload["state"] = json!("passed");
        payload["artifact_error"] = Value::Null;
    } else {
        payload["state"] = json!("failed");
        payload["artifact_error"] = json!(errors.join("; "));
    }
    payload
}

fn phase2_benchmark_default_payload() -> Value {
    json!({
        "state": "pending",
        "source": "not_configured",
        "artifact_path": null,
        "artifact_error": null,
        "jurisdiction": "france",
        "fingerprint": "bge-m3:1024:normalize:true",
        "claim_scope": "full French juridic search (statutes + jurisprudence): judicial (Cassation/appeal) AND administrative retrieval AND ECLI/pourvoi/CETATEXT decision-citation verification, through the production pipeline",
        "required_evidence": [
            "judicial_retrieval AND administrative_retrieval categories, each with metric=recall_at_10 and independent query floors, run through the production search pipeline",
            "decision_citation.by_identifier with a MEASURED breakdown for each of ecli, pourvoi, cetatext (metric=decision_citation_accuracy, per-identifier queries + accuracy at/above floors)",
            "per-category metrics with query counts and the locked bge-m3 fingerprint, at or above policy floors",
            "structured provenance: pipeline='production', non-empty code_version + index_revision, sampled=false, boolean human_in_gold + llm_in_gold",
            "pseudonymisation preservation asserted (no re-identification, no cross-source linking)"
        ],
        "floors": {
            "retrieval_recall_at_10": PHASE2_MIN_RETRIEVAL_RECALL_AT_10,
            "min_judicial_retrieval_queries": PHASE2_MIN_JUDICIAL_RETRIEVAL_QUERIES,
            "min_administrative_retrieval_queries": PHASE2_MIN_ADMINISTRATIVE_RETRIEVAL_QUERIES,
            "decision_citation_accuracy": PHASE2_MIN_DECISION_CITATION_ACCURACY,
            "min_citation_queries_per_identifier": PHASE2_MIN_CITATION_QUERIES_PER_IDENTIFIER,
            "required_citation_identifiers": PHASE2_REQUIRED_CITATION_IDENTIFIERS
        },
        "categories": null,
        "provenance": null,
        "evidence": [],
        "reason": "no Phase 2 jurisprudence eval benchmark has been run yet; the full-juridic claim is fail-closed until a jurisdiction-correct passing artifact is supplied"
    })
}

/// Re-derive whether a Phase 2 benchmark artifact PASSES the full contract against the policy floors
/// (never trust a self-reported `state`). Returns the list of reasons it is NOT a valid pass (empty =
/// valid). Enforces jurisdiction, locked fingerprint, non-empty evidence, production provenance,
/// BOTH jurisprudence families' retrieval, and ECLI/pourvoi/CETATEXT citation coverage.
fn phase2_benchmark_artifact_errors(artifact: &Value) -> Vec<String> {
    let mut errors = Vec::new();
    if artifact["jurisdiction"].as_str() != Some("france") {
        errors.push("jurisdiction must be `france`".to_owned());
    }
    if artifact["fingerprint"].as_str() != Some("bge-m3:1024:normalize:true") {
        errors.push("fingerprint must be the locked bge-m3:1024:normalize:true".to_owned());
    }
    if !artifact["evidence"].as_array().is_some_and(|evidence| !evidence.is_empty()) {
        errors.push("evidence must be a non-empty array".to_owned());
    }

    // Production provenance: the benchmark must run through the production pipeline, with pinned
    // code/index revisions, `sampled=false`, and disclosed human/LLM gold booleans.
    let provenance = &artifact["provenance"];
    if provenance["pipeline"].as_str() != Some(PHASE2_PRODUCTION_PIPELINE) {
        errors.push(format!(
            "provenance.pipeline must be `{PHASE2_PRODUCTION_PIPELINE}` (run through the production pipeline)"
        ));
    }
    for field in ["code_version", "index_revision"] {
        if !provenance[field].as_str().is_some_and(|value| !value.trim().is_empty()) {
            errors.push(format!("provenance.{field} must be a non-empty string"));
        }
    }
    // Recorded as booleans (the policy does not forbid LLM-drafted/human-reviewed gold, only hidden
    // sampling): sampled must be false; human_in_gold / llm_in_gold are disclosed booleans.
    for flag in ["sampled", "human_in_gold", "llm_in_gold"] {
        if !provenance[flag].is_boolean() {
            errors.push(format!("provenance.{flag} must be a boolean"));
        }
    }
    if provenance["sampled"].as_bool() == Some(true) {
        errors.push("provenance.sampled must be false (full benchmark, not a sample)".to_owned());
    }

    // Both jurisprudence families must be retrieved, with the named metric and independent floors.
    phase2_benchmark_validate_category(
        &artifact["categories"]["judicial_retrieval"],
        "judicial_retrieval",
        "recall_at_10",
        PHASE2_MIN_RETRIEVAL_RECALL_AT_10,
        PHASE2_MIN_JUDICIAL_RETRIEVAL_QUERIES,
        &mut errors,
    );
    phase2_benchmark_validate_category(
        &artifact["categories"]["administrative_retrieval"],
        "administrative_retrieval",
        "recall_at_10",
        PHASE2_MIN_RETRIEVAL_RECALL_AT_10,
        PHASE2_MIN_ADMINISTRATIVE_RETRIEVAL_QUERIES,
        &mut errors,
    );

    // Decision-citation verification must be MEASURED per identifier kind (not just declared): each of
    // ECLI/pourvoi/CETATEXT needs its own metric, query count, and accuracy at/above the floors, so an
    // ECLI-only run cannot open the "ECLI/pourvoi/CETATEXT verification" claim.
    let decision_citation = &artifact["categories"]["decision_citation"];
    if decision_citation["metric"].as_str() != Some("decision_citation_accuracy") {
        errors.push("category `decision_citation` metric must be `decision_citation_accuracy`".to_owned());
    }
    for identifier in PHASE2_REQUIRED_CITATION_IDENTIFIERS {
        phase2_benchmark_validate_category(
            &decision_citation["by_identifier"][identifier],
            &format!("decision_citation.by_identifier.{identifier}"),
            "decision_citation_accuracy",
            PHASE2_MIN_DECISION_CITATION_ACCURACY,
            PHASE2_MIN_CITATION_QUERIES_PER_IDENTIFIER,
            &mut errors,
        );
    }
    errors
}

fn phase2_benchmark_validate_category(
    category: &Value,
    name: &str,
    expected_metric: &str,
    floor: f64,
    min_queries: u64,
    errors: &mut Vec<String>,
) {
    if !category.is_object() {
        errors.push(format!("category `{name}` is missing"));
        return;
    }
    if category["metric"].as_str() != Some(expected_metric) {
        errors.push(format!("category `{name}` metric must be `{expected_metric}`"));
    }
    let Some(value) = category["value"].as_f64() else {
        errors.push(format!("category `{name}` is missing a numeric `value`"));
        return;
    };
    if value < floor {
        errors.push(format!("category `{name}` value {value} is below floor {floor}"));
    }
    match category["queries"].as_u64() {
        Some(queries) if queries >= min_queries => {}
        Some(queries) => errors.push(format!(
            "category `{name}` has {queries} queries, below the minimum {min_queries}"
        )),
        None => errors.push(format!("category `{name}` is missing a `queries` count")),
    }
}

fn phase2_benchmark_check_status(benchmark: &Value) -> &'static str {
    match benchmark["state"].as_str() {
        Some("passed") => "pass",
        Some("failed") => "fail",
        _ => "pending",
    }
}

fn phase1_reranker_decision_payload() -> Value {
    // TODO(phase1-reranker): when the reranker provider seam lands, derive this
    // from runtime config/manifests instead of the Phase 1 static deferral.
    json!({
        "state": "deferred",
        "provider": "disabled",
        "adopted": false,
        "decision_date": "2026-06-22",
        "model_candidate": "BAAI/bge-reranker-v2-m3",
        "evidence": [
            "work/03-implementation/02-evidence/2026-06-21-reranker-feasibility.md",
            "work/03-implementation/02-evidence/2026-06-22-phase1-eval-benchmark-summary.md",
            "work/03-implementation/02-evidence/2026-06-22-reranker-deferral-decision.md"
        ],
        "reason": "current Phase 1 release-candidate fixtures cannot measure a material rerank gain, no reranker provider is packaged, and cross-encoder latency/packaging remain unmeasured",
        "future_adoption_gate": "hybrid+rerank must show material legal-retrieval quality gain on the external expert benchmark or future project-owned release-gating fixtures, with measured latency and graceful fallback to hybrid order"
    })
}

fn phase1_embedding_model_locked(ingest_health: &Value) -> bool {
    const LOCKED_PHASE1_EMBEDDING_FINGERPRINT: &str = "bge-m3:1024:normalize:true";
    let manifest = &ingest_health["embedding_manifest"];
    manifest["embedding_fingerprint"].as_str() == Some(LOCKED_PHASE1_EMBEDDING_FINGERPRINT)
        && manifest["model"].as_str() == Some(PHASE0_EMBEDDING_MODEL)
        && manifest["dimension"].as_u64() == Some(PHASE0_EMBEDDING_DIMENSION as u64)
        && manifest["normalize"].as_bool() == Some(true)
}

fn phase1_gate_check(
    name: &str,
    status: impl Into<Phase1GateStatus>,
    message: impl Into<String>,
) -> Value {
    let status = status.into().as_str();
    let message = message.into();
    json!({
        "name": name,
        "status": status,
        "message": message,
        "gating": true
    })
}

// An advisory check is reported in `checks[]` but does NOT block `claim_allowed`.
fn phase1_gate_check_advisory(
    name: &str,
    status: impl Into<Phase1GateStatus>,
    message: impl Into<String>,
) -> Value {
    let status = status.into().as_str();
    let message = message.into();
    json!({
        "name": name,
        "status": status,
        "message": message,
        "gating": false
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

/// The `status.zone_retrieval` block (T5.1): the cheap overlay coverage report joined with the
/// resolver-reachable denominator, so the reported numbers are honest fractions of what the backfill
/// can ever reach — never inflating the corpus claim. Each half degrades to `null` independently so a
/// failure in one (e.g. the denominator scan) never blanks the whole block or breaks `status`.
fn zone_retrieval_status_block(postgres: &ManagedPostgres) -> Value {
    let mut block = match zone_retrieval_coverage_json(postgres) {
        Ok(json_text) => serde_json::from_str(&json_text).unwrap_or(Value::Null),
        Err(_) => Value::Null,
    };
    let resolver_reachable = match zone_resolver_reachable_json(postgres) {
        Ok(json_text) => serde_json::from_str(&json_text).unwrap_or(Value::Null),
        Err(_) => Value::Null,
    };
    if let Value::Object(map) = &mut block {
        map.insert("resolver_reachable".to_owned(), resolver_reachable);
    }
    block
}

fn status_index_and_ingest_health(
    index_dir: Option<&Path>,
    replay_snapshot_mode: ReplaySnapshotMode,
) -> (Value, Value, Value, Value) {
    let Some(index_dir) = configured_index_dir(index_dir) else {
        return (
            json!({
                "state": "not_configured",
                "query_ready": false,
                "message": "No index has been built yet; Phase 0 scaffold is installed."
            }),
            pending_ingest_health(),
            Value::Null,
            Value::Null,
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
            Value::Null,
            Value::Null,
        );
    }

    match open_index(&index_dir) {
        Ok(postgres) => {
            // Per-source coverage + freshness from each source's latest completed run manifest.
            // Cheap (small ingest_run table); null if it cannot be read so status still renders.
            let corpus_sources = match corpus_source_coverage_json(&postgres) {
                Ok(json_text) => serde_json::from_str(&json_text).unwrap_or(Value::Null),
                Err(_) => Value::Null,
            };
            // Zone overlay coverage + resolver-reachable denominator (T5.1). A SEPARATE surface from
            // the corpus gate; degrades to null so status still renders if the zone tables are absent.
            let zone_retrieval = zone_retrieval_status_block(&postgres);
            match load_ingest_health_with_replay_snapshot_mode(&postgres, replay_snapshot_mode) {
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
                        corpus_sources,
                        zone_retrieval,
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
                        corpus_sources,
                        zone_retrieval,
                    )
                }
            }
        }
        Err(error) => (
            json!({
                "state": "unavailable",
                "query_ready": false,
                "path": index_path,
                "message": "Index exists but could not be opened.",
                "error": error
            }),
            pending_ingest_health(),
            Value::Null,
            Value::Null,
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
pub(crate) enum QueryReadinessGate {
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
    // One round-trip on the hot path: a manifest cache hit skips the full-corpus coverage
    // aggregations (a count(DISTINCT) over ~1.74M documents plus a count over ~1.85M chunks). The
    // cache is only populated when the index is fully ready and is invalidated by ingest/embed runs.
    let (readiness, _from_cache) =
        load_or_compute_query_readiness(postgres).map_err(storage_error_object)?;
    let projection_coverage = readiness.projection_coverage;
    let embedding_coverage = readiness.embedding_coverage;

    if !coverage_complete(projection_coverage.covered, projection_coverage.total) {
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

    if matches!(gate, QueryReadinessGate::Search)
        && !coverage_complete(embedding_coverage.covered, embedding_coverage.total)
    {
        return Err(index_not_query_ready(
            gate,
            "embedding coverage gate is incomplete",
            &projection_coverage,
            Some(&embedding_coverage),
        ));
    }

    Ok(())
}

pub(crate) fn pgvector_literal(values: &[f32]) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::dispatch_session_request;
    use jurisearch_core::contract::SESSION_EXCLUDED_COMMANDS;
    use jurisearch_core::eval::{FixtureTier, ReviewStatus};
    use jurisearch_core::session::{SessionRequest, SessionResponse};

    fn phase2_index_ready() -> Value {
        json!({ "query_ready": true })
    }
    fn phase2_ingest_available() -> Value {
        json!({ "state": "available" })
    }
    fn phase2_corpus_both_families() -> Value {
        json!({
            "cass": { "zone_accurate": false },
            "jade": { "zone_accurate": false }
        })
    }
    fn phase2_valid_benchmark_json() -> String {
        json!({
            "state": "passed",
            "jurisdiction": "france",
            "fingerprint": "bge-m3:1024:normalize:true",
            "evidence": ["work/03-implementation/02-evidence/phase2-eval.json"],
            "provenance": {
                "pipeline": "production", "code_version": "jurisearch-cli:0.1.0", "index_revision": "freemium-20250713",
                "sampled": false, "human_in_gold": false, "llm_in_gold": true
            },
            "categories": {
                "judicial_retrieval": { "metric": "recall_at_10", "value": 0.62, "queries": 20 },
                "administrative_retrieval": { "metric": "recall_at_10", "value": 0.58, "queries": 18 },
                "decision_citation": {
                    "metric": "decision_citation_accuracy",
                    "by_identifier": {
                        "ecli": { "metric": "decision_citation_accuracy", "value": 0.98, "queries": 14 },
                        "pourvoi": { "metric": "decision_citation_accuracy", "value": 0.96, "queries": 12 },
                        "cetatext": { "metric": "decision_citation_accuracy", "value": 0.97, "queries": 11 }
                    }
                }
            }
        })
        .to_string()
    }

    #[test]
    fn phase2_gate_is_fail_closed_without_a_benchmark() {
        // Even with corpus present + query ready + honest zones, the claim stays closed until a
        // passing jurisprudence benchmark is supplied.
        let gate = phase2_gate_payload_with(
            &phase2_index_ready(),
            &phase2_ingest_available(),
            &phase2_corpus_both_families(),
            phase2_benchmark_default_payload(),
        );
        assert_eq!(gate["claim_allowed"], false);
        assert_eq!(gate["state"], "not_ready");
        let benchmark_check = gate["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == "jurisprudence_eval_benchmark")
            .unwrap();
        assert_eq!(benchmark_check["status"], "pending");
        assert_eq!(benchmark_check["gating"], true);
    }

    #[test]
    fn phase2_gate_requires_both_judicial_and_administrative() {
        // Only judicial (cass), no administrative (jade) -> corpus check fails.
        let gate = phase2_gate_payload_with(
            &phase2_index_ready(),
            &phase2_ingest_available(),
            &json!({ "cass": { "zone_accurate": false } }),
            phase2_benchmark_default_payload(),
        );
        let corpus_check = gate["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == "jurisprudence_corpus_present")
            .unwrap();
        // Missing administrative corpus -> not yet satisfied (pending), and the claim stays closed.
        assert_eq!(corpus_check["status"], "pending");
        assert_eq!(gate["claim_allowed"], false);
    }

    #[test]
    fn phase2_gate_rejects_dishonest_zone_provenance() {
        // A bulk source claiming zone_accurate=true must fail the honesty check.
        let gate = phase2_gate_payload_with(
            &phase2_index_ready(),
            &phase2_ingest_available(),
            &json!({ "cass": { "zone_accurate": true }, "jade": { "zone_accurate": false } }),
            phase2_benchmark_default_payload(),
        );
        let honest = gate["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["name"] == "honest_zone_provenance")
            .unwrap();
        assert_eq!(honest["status"], "pending");
    }

    #[test]
    fn legifrance_code_search_body_uses_real_contract() {
        // Regression: the Legifrance /search engine rejects {query,pageSize} with HTTP 500; the body
        // must use fond=CODE_DATE + recherche.champs with TOUS_LES_MOTS_DANS_UN_CHAMP (validated live).
        let body = legifrance_code_search_body("609 code de procédure civile");
        assert_eq!(body["fond"], "CODE_DATE");
        assert!(body.get("query").is_none(), "the bogus top-level query field must be gone");
        let critere = &body["recherche"]["champs"][0]["criteres"][0];
        assert_eq!(critere["typeRecherche"], "TOUS_LES_MOTS_DANS_UN_CHAMP");
        assert_eq!(critere["valeur"], "609 code de procédure civile");
        assert_eq!(body["recherche"]["champs"][0]["typeChamp"], "ALL");
    }

    #[test]
    fn cite_online_shares_real_contract_body() {
        // WARN#2 regression: cite --online (apply_online_citation_confirmation) now builds its Legifrance
        // body via the shared legifrance_code_search_body, so the known-bad {query,pageSize} shape (live
        // HTTP 500) cannot reappear on that user-facing path.
        let body = legifrance_code_search_body("L. 121-1 du code de la consommation");
        assert!(body.get("query").is_none(), "no top-level query (the bad cite --online shape)");
        assert!(body.get("pageSize").is_none(), "no top-level pageSize (the bad cite --online shape)");
        assert_eq!(body["fond"], "CODE_DATE");
    }

    #[test]
    fn sanitize_legifrance_query_caps_length_and_collapses_whitespace() {
        // Whitespace/control runs collapse to single spaces and trim (a clean citation is untouched).
        assert_eq!(
            sanitize_legifrance_query("  609 \t code de\nprocédure   civile  "),
            "609 code de procédure civile"
        );
        // The HTTP-500 trigger: an over-long multi-article concatenation is capped to the safe max,
        // so it reaches Legifrance as a (non-matching) 200 instead of a 500 / wasted upstream_error.
        let huge = format!("{} code pénal", "L.123-456,".repeat(80)); // ~880 chars
        let sanitized = sanitize_legifrance_query(&huge);
        assert!(huge.chars().count() > LEGIFRANCE_QUERY_MAX_CHARS);
        assert_eq!(sanitized.chars().count(), LEGIFRANCE_QUERY_MAX_CHARS);
        // Truncation respects char boundaries (no panic on multi-byte input).
        let accents = "é".repeat(LEGIFRANCE_QUERY_MAX_CHARS + 50);
        assert_eq!(
            sanitize_legifrance_query(&accents).chars().count(),
            LEGIFRANCE_QUERY_MAX_CHARS
        );
    }

    #[test]
    fn parse_visa_citation_prefers_url_query_and_dedups() {
        // Slice 2: the Legifrance URL `query` param is the primary extraction; HTML title is the fallback;
        // the same (article, code) across decisions dedups to one citation_key.
        let url_title = "Article <a href=\"https://www.legifrance.gouv.fr/search/code?tab_selection=code&searchField=ALL&query=609+code+de+proc%C3%A9dure+civile&page=1&init=true\" target=\"_blank\">609</a> du code de procédure civile.";
        let parsed = parse_visa_citation(url_title).expect("url citation");
        assert_eq!(parsed.extraction_method, "legifrance_url_query");
        assert_eq!(parsed.article_number_norm, "609");
        assert_eq!(parsed.code_name_norm, "code de procédure civile");
        assert_eq!(parsed.canonical_query, "609 code de procédure civile");
        assert!(parsed.legifrance_url.is_some());

        // Fallback path (no usable URL) parses the plain title to the SAME normalized citation.
        let plain_title = "Article 609 du code de procédure civile.";
        let fallback = parse_visa_citation(plain_title).expect("fallback citation");
        assert_eq!(fallback.extraction_method, "visa_title_regex");
        assert_eq!(fallback.article_number_norm, "609");
        assert_eq!(fallback.code_name_norm, "code de procédure civile");
        // Dedup: URL and fallback forms of the same citation share one key.
        assert_eq!(parsed.citation_key, fallback.citation_key);

        // Article-number normalization collapses spaces and uppercases.
        let lettered = parse_visa_citation("Article L. 121-1 du code de la consommation").expect("lettered");
        assert_eq!(lettered.article_number_norm, "L.121-1");
        assert_eq!(lettered.code_name_norm, "code de la consommation");

        // Non-code legislation (no "code") is skipped, not mis-extracted.
        assert!(parse_visa_citation("Loi n° 2008-561 du 17 juin 2008").is_none());
    }

    #[test]
    fn zone_benchmark_artifact_records_actual_fingerprint_and_never_gates() {
        // Z5/T5.2: the measured-only zone benchmark records the ACTUAL dense fingerprint (null for a
        // lexical-only BM25 run), is flagged as a non-gate input, and reports an empty zone as null.
        let categories = json!({
            "motivations": { "metric": "recall_at_10", "value": 0.9, "queries": 50, "meets_proposed_floor": true },
            "moyens": { "metric": "recall_at_10", "value": null, "queries": 0, "meets_proposed_floor": null }
        });

        // BM25 run: no embedder was used, so the artifact must NOT claim a dense fingerprint.
        let bm25 = zone_benchmark_artifact(
            categories.clone(),
            RetrievalMode::Bm25,
            false,
            None,
            0.8,
            FranceJurisZoneGoldLimits::default(),
            "rev",
            "src",
        );
        assert_eq!(bm25["kind"], "phase2_zone_benchmark");
        assert_eq!(bm25["gate_input"], false);
        assert_eq!(bm25["uses_dense"], false);
        assert!(bm25["fingerprint"].is_null(), "BM25 run must not claim a dense fingerprint");
        // Only the zone with qrels counts toward the advisory floor verdict (empty zone excluded).
        assert_eq!(bm25["all_meet_proposed_floor"], true);

        // Hybrid run: the artifact records the exact fingerprint readiness verified.
        let hybrid = zone_benchmark_artifact(
            categories,
            RetrievalMode::Hybrid,
            true,
            Some("bge-m3:1024:normalize:true"),
            0.8,
            FranceJurisZoneGoldLimits::default(),
            "rev",
            "src",
        );
        assert_eq!(hybrid["uses_dense"], true);
        assert_eq!(hybrid["fingerprint"], "bge-m3:1024:normalize:true");
    }

    #[test]
    fn phase2_benchmark_re_derives_pass_and_rejects_bad_artifacts() {
        let dir = tempfile::tempdir().unwrap();

        // A valid artifact re-derives to passed.
        let valid = dir.path().join("valid.json");
        std::fs::write(&valid, phase2_valid_benchmark_json()).unwrap();
        let payload = phase2_benchmark_payload_with_path(Some(&valid));
        assert_eq!(payload["state"], "passed");
        assert!(payload["artifact_error"].is_null());

        // A passing state is RE-DERIVED, not trusted: an artifact reporting state="failed" but
        // otherwise valid still re-derives to passed (artifact state kept only as a diagnostic).
        let mut reported_failed: Value = serde_json::from_str(&phase2_valid_benchmark_json()).unwrap();
        reported_failed["state"] = json!("failed");
        let rf_path = dir.path().join("reported_failed.json");
        std::fs::write(&rf_path, reported_failed.to_string()).unwrap();
        let payload = phase2_benchmark_payload_with_path(Some(&rf_path));
        assert_eq!(payload["state"], "passed");
        assert_eq!(payload["artifact_reported_state"], "failed");

        // Helper: mutate the valid artifact, write it, and return the re-derived state.
        let derived = |name: &str, mutate: &dyn Fn(&mut Value)| -> Value {
            let mut artifact: Value = serde_json::from_str(&phase2_valid_benchmark_json()).unwrap();
            mutate(&mut artifact);
            let path = dir.path().join(name);
            std::fs::write(&path, artifact.to_string()).unwrap();
            phase2_benchmark_payload_with_path(Some(&path))["state"].clone()
        };

        // Below-floor retrieval recall is rejected.
        assert_eq!(
            derived("low.json", &|a| a["categories"]["judicial_retrieval"]["value"] = json!(0.10)),
            "failed"
        );
        // Wrong jurisdiction rejected.
        assert_eq!(derived("juris.json", &|a| a["jurisdiction"] = json!("belgium")), "failed");
        // Sampled artifact rejected.
        assert_eq!(derived("sampled.json", &|a| a["provenance"]["sampled"] = json!(true)), "failed");
        // Missing production provenance (pipeline/code_version/index_revision) rejected (BLOCKER 1).
        assert_eq!(derived("pipe.json", &|a| a["provenance"]["pipeline"] = json!("proxy")), "failed");
        assert_eq!(derived("cv.json", &|a| a["provenance"]["code_version"] = json!("")), "failed");
        assert_eq!(derived("ir.json", &|a| { a["provenance"]["index_revision"] = Value::Null; }), "failed");
        // Missing administrative family rejected (BLOCKER 2: both families required).
        assert_eq!(
            derived("judonly.json", &|a| { a["categories"]["administrative_retrieval"] = Value::Null; }),
            "failed"
        );
        // Wrong citation metric rejected (BLOCKER 2).
        assert_eq!(
            derived("metric.json", &|a| a["categories"]["decision_citation"]["metric"] = json!("f1")),
            "failed"
        );
        // A declared-but-unmeasured identifier (pourvoi breakdown removed) is rejected (r2 BLOCKER):
        // coverage must be MEASURED, not just listed.
        assert_eq!(
            derived("ids.json", &|a| { a["categories"]["decision_citation"]["by_identifier"]["pourvoi"] = Value::Null; }),
            "failed"
        );
        // A below-per-identifier-query-floor breakdown (cetatext = 2 queries) is rejected.
        assert_eq!(
            derived("idq.json", &|a| a["categories"]["decision_citation"]["by_identifier"]["cetatext"]["queries"] = json!(2)),
            "failed"
        );
        // A non-string artifact `state` does not crash; it re-derives and coerces the diagnostic.
        let mut weird: Value = serde_json::from_str(&phase2_valid_benchmark_json()).unwrap();
        weird["state"] = json!(false);
        let wpath = dir.path().join("weird_state.json");
        std::fs::write(&wpath, weird.to_string()).unwrap();
        let payload = phase2_benchmark_payload_with_path(Some(&wpath));
        assert_eq!(payload["state"], "passed");
        assert!(payload["artifact_reported_state"].is_null());

        // A parseable but non-object artifact (`[]`) is rejected and the emitted `artifact`
        // diagnostic is normalized to null so the payload still matches the published schema.
        let arr_path = dir.path().join("array.json");
        std::fs::write(&arr_path, "[]").unwrap();
        let payload = phase2_benchmark_payload_with_path(Some(&arr_path));
        assert_eq!(payload["state"], "failed");
        assert!(payload["artifact"].is_null());

        // An object artifact whose `categories`/`provenance` are non-objects is rejected, and those
        // diagnostic fields are normalized to null so the failure payload stays schema-shaped.
        let malformed_path = dir.path().join("malformed_members.json");
        std::fs::write(&malformed_path, json!({ "categories": [], "provenance": false }).to_string())
            .unwrap();
        let payload = phase2_benchmark_payload_with_path(Some(&malformed_path));
        assert_eq!(payload["state"], "failed");
        assert!(payload["artifact"].is_object()); // the artifact itself IS an object
        assert!(payload["categories"].is_null());
        assert!(payload["provenance"].is_null());
    }

    #[test]
    fn france_juris_artifact_matches_the_phase2_gate_contract() {
        // The new producer (`eval france-juris`) must emit an artifact the gate consumer accepts:
        // a passing run re-derives with NO errors; below-floor recall and too-few citation queries
        // are rejected. Locks the producer<->consumer shape against future drift.
        let cat = |metric: f64, queries: usize| FranceJurisCategoryResult { metric, queries };

        let pass = france_juris_artifact(
            cat(0.80, 60),
            cat(0.70, 60),
            cat(1.0, 30),
            cat(1.0, 30),
            cat(1.0, 30),
            FranceJurisGoldLimits::default(),
            "phase2-juris:md5:deadbeefdeadbeefdeadbeefdeadbeef",
            "index:phase2-juris:md5:deadbeefdeadbeefdeadbeefdeadbeef",
        );
        assert_eq!(pass["state"], "passed");
        assert!(
            phase2_benchmark_artifact_errors(&pass).is_empty(),
            "produced artifact must satisfy the gate contract, got: {:?}",
            phase2_benchmark_artifact_errors(&pass)
        );

        // Below-floor judicial recall: producer marks failed AND the gate re-derives errors.
        let low_recall = france_juris_artifact(
            cat(0.10, 60),
            cat(0.70, 60),
            cat(1.0, 30),
            cat(1.0, 30),
            cat(1.0, 30),
            FranceJurisGoldLimits::default(),
            "rev",
            "src",
        );
        assert_eq!(low_recall["state"], "failed");
        assert!(!phase2_benchmark_artifact_errors(&low_recall).is_empty());

        // Too few ECLI citation queries (below the per-identifier minimum) is rejected.
        let few_citations = france_juris_artifact(
            cat(0.80, 60),
            cat(0.70, 60),
            cat(1.0, 3),
            cat(1.0, 30),
            cat(1.0, 30),
            FranceJurisGoldLimits::default(),
            "rev",
            "src",
        );
        assert!(!phase2_benchmark_artifact_errors(&few_citations).is_empty());
    }

    #[test]
    fn derive_zone_unit_rows_handles_multi_fragment_and_skips_empty() {
        // T3.1: one row per non-empty fragment, contiguous per-zone fragment_index; empty zones/blank
        // fragments produce no rows.
        let zones = json!({
            "motivations": [{ "text": "premier motif" }, { "text": "  " }, { "text": "second motif" }],
            "moyens": [{ "text": "un moyen" }],
            "dispositif": []
        });
        let rows = derive_zone_unit_rows("cass:X", "cass", "h", &zones);
        // 2 motivations (the blank one skipped) + 1 moyens + 0 dispositif.
        assert_eq!(rows.len(), 3);
        let motivations: Vec<_> = rows.iter().filter(|r| r.zone == "motivations").collect();
        assert_eq!(motivations.len(), 2);
        assert_eq!(motivations[0].fragment_index, 0);
        assert_eq!(motivations[0].body, "premier motif");
        assert_eq!(motivations[1].fragment_index, 1); // contiguous despite the skipped blank
        assert_eq!(motivations[1].body, "second motif");
        assert!(rows.iter().all(|r| r.builder_version == ZONE_UNIT_BUILDER_VERSION));
        assert!(rows.iter().all(|r| r.body == r.search_body && r.source == "cass" && r.text_hash == "h"));
    }

    #[test]
    fn worker_join_error_counts_whole_slice_as_errors() {
        // Z2-fix: a panicked backfill worker (join -> None) must count its whole slice as errors, not
        // silently drop those decisions from accounting.
        let panicked = worker_outcomes_or_errors(None, 3);
        assert_eq!(panicked.len(), 3);
        assert!(panicked.iter().all(|o| matches!(o, ZoneEnrichOutcome::Error)));
        let returned = vec![ZoneEnrichOutcome::Official, ZoneEnrichOutcome::Fallback];
        assert_eq!(worker_outcomes_or_errors(Some(returned), 2).len(), 2);
    }

    #[test]
    fn zone_text_hash_is_deterministic_and_change_sensitive() {
        // T2.1: the snapshot hash must be stable for identical inputs and change when the text or
        // update_date changes (it keys derivation/refresh of zone_units).
        let decision = json!({ "text": "MOTIVATIONS de la cour.", "update_date": "2024-01-01" });
        let zones = json!({ "motivations": [{ "start": 0, "end": 11, "text": "MOTIVATIONS" }] });
        let h1 = zone_text_hash(&decision, &zones, "jdl-1");
        let h2 = zone_text_hash(&decision, &zones, "jdl-1");
        assert_eq!(h1, h2, "same inputs -> same hash");
        assert!(h1.starts_with("sha256:"));

        let other_text = json!({ "text": "CHANGED.", "update_date": "2024-01-01" });
        assert_ne!(h1, zone_text_hash(&other_text, &zones, "jdl-1"), "text change -> new hash");
        let other_date = json!({ "text": "MOTIVATIONS de la cour.", "update_date": "2024-02-02" });
        assert_ne!(h1, zone_text_hash(&other_date, &zones, "jdl-1"), "update_date change -> new hash");
        assert_ne!(h1, zone_text_hash(&decision, &zones, "jdl-2"), "provider id change -> new hash");
    }

    #[test]
    fn judilibre_zones_normalize_with_char_safe_offsets() {
        // Multibyte text: Judilibre offsets are CHARACTER indices, so slicing must be char-safe.
        // "Évidence motivée" — accented leading chars shift byte offsets vs char offsets.
        let text = "Évidence. MOTIVATIONS: la cour. DISPOSITIF: rejette.";
        let chars: Vec<char> = text.chars().collect();
        let m_start = text.chars().position(|c| c == 'M').unwrap(); // "MOTIVATIONS" begins here (char index)
        let m_end = text.chars().position(|c| c == 'D').unwrap() - 1; // up to before " DISPOSITIF"
        let d_start = text.chars().position(|c| c == 'D').unwrap();
        let d_end = chars.len();
        let decision = json!({
            "text": text,
            "zones": {
                "motivations": [{ "start": m_start, "end": m_end }],
                "dispositif": [{ "start": d_start, "end": d_end }],
                // out-of-range fragment must be skipped, not panic
                "moyens": [{ "start": 1000, "end": 2000 }],
            }
        });
        let (zones, any_valid) = normalize_judilibre_zones(&decision);
        assert!(any_valid);
        let mot = zones["motivations"][0]["text"].as_str().unwrap();
        assert_eq!(mot, &chars[m_start..m_end].iter().collect::<String>());
        assert!(mot.starts_with("MOTIVATIONS"));
        assert!(zones["dispositif"][0]["text"].as_str().unwrap().contains("DISPOSITIF"));
        // moyens had only an out-of-range fragment -> empty array
        assert_eq!(zones["moyens"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn judilibre_match_requires_number_and_date() {
        let search = json!({"results": [
            {"id": "wrong_date", "numbers": ["24-13.470"], "decision_date": "2020-01-01"},
            {"id": "right", "numbers": ["24-13.470"], "decision_date": "2025-06-04"},
            {"id": "other", "numbers": ["99-99.999"], "decision_date": "2025-06-04"},
        ]});
        // Normalization strips dots/spaces but KEEPS the hyphen ("24-13.470" -> "24-13470").
        // Date provided -> only the number+date match wins (guards pourvoi collisions across years).
        assert_eq!(
            find_matching_judilibre_id(&search, "24-13470", Some("2025-06-04")).as_deref(),
            Some("right")
        );
        // No local date -> first number match accepted (date-agnostic fallback).
        assert_eq!(
            find_matching_judilibre_id(&search, "24-13470", None).as_deref(),
            Some("wrong_date")
        );
        // Unknown number -> no match.
        assert!(find_matching_judilibre_id(&search, "11-11111", Some("2025-06-04")).is_none());
    }

    #[test]
    fn cached_zone_part_block_is_official_only_when_present() {
        let cached = json!({
            "status": "ok",
            "provider": "judilibre",
            "provider_decision_id": "abc",
            "fetched_at": "2026-06-24T00:00:00Z",
            "zones": {
                "motivations": [{ "start": 0, "end": 5, "text": "Motif" }],
                "dispositif": []
            }
        });
        let block = part_block_from_cached_zones(&cached, DecisionPart::Motivations, "motivations").unwrap();
        assert_eq!(block["official_zones"], json!(true));
        assert_eq!(block["zone_accurate"], json!(true));
        assert_eq!(block["zone_provenance"], json!("judilibre"));
        assert_eq!(block["text"], json!("Motif"));
        // dispositif present but empty -> not an official part
        assert!(part_block_from_cached_zones(&cached, DecisionPart::Dispositif, "dispositif").is_none());
        // summary/visa are not Judilibre-zone parts
        assert!(judilibre_zone_key(DecisionPart::Summary).is_none());
        assert!(judilibre_zone_key(DecisionPart::Visa).is_none());
        assert_eq!(judilibre_zone_key(DecisionPart::Motivations), Some("motivations"));
    }

    #[test]
    fn zone_cache_action_honors_status_and_ttl() {
        let part = DecisionPart::Motivations;
        let key = "motivations";
        let ok_fresh = json!({"status":"ok","expired":false,"zones":{"motivations":[{"start":0,"end":3,"text":"abc"}]}});
        let ok_no_zone = json!({"status":"ok","expired":false,"zones":{"motivations":[]}});
        let ok_expired = json!({"status":"ok","expired":true,"zones":{"motivations":[{"start":0,"end":3,"text":"abc"}]}});
        let neg_fresh = json!({"status":"not_found","expired":false,"zones":{}});
        let err_fresh = json!({"status":"upstream_error","expired":false,"zones":{}});
        let err_expired = json!({"status":"upstream_error","expired":true,"zones":{}});
        let no_row = Value::Null; // decision_zones_json returns `null` when uncached

        let is = |a: ZoneCacheAction, want: &str| match (a, want) {
            (ZoneCacheAction::Official(_), "official") => true,
            (ZoneCacheAction::Fallback, "fallback") => true,
            (ZoneCacheAction::Enrich, "enrich") => true,
            _ => false,
        };

        // Fresh ok with the zone -> official, regardless of --online.
        assert!(is(zone_cache_action(&ok_fresh, part, key, false, "cass"), "official"));
        // Fresh ok but that zone is empty -> fallback (decision genuinely has no such zone; no re-fetch).
        assert!(is(zone_cache_action(&ok_no_zone, part, key, true, "cass"), "fallback"));
        // Expired ok -> re-enrich when online+cass, else fallback.
        assert!(is(zone_cache_action(&ok_expired, part, key, true, "cass"), "enrich"));
        assert!(is(zone_cache_action(&ok_expired, part, key, false, "cass"), "fallback"));
        // Fresh negative -> suppress network even when online.
        assert!(is(zone_cache_action(&neg_fresh, part, key, true, "cass"), "fallback"));
        // Fresh upstream error -> suppress (short TTL); expired upstream error -> retry.
        assert!(is(zone_cache_action(&err_fresh, part, key, true, "cass"), "fallback"));
        assert!(is(zone_cache_action(&err_expired, part, key, true, "cass"), "enrich"));
        // No cache row -> enrich only when online + a Judilibre-resolvable Cour de cassation source.
        assert!(is(zone_cache_action(&no_row, part, key, true, "cass"), "enrich"));
        assert!(is(zone_cache_action(&no_row, part, key, false, "cass"), "fallback"));
        // INCA (inédit Cassation) enriches like cass; CAPP (Cour d'appel) and JADE fall back.
        assert!(is(zone_cache_action(&no_row, part, key, true, "inca"), "enrich"));
        assert!(is(zone_cache_action(&no_row, part, key, true, "capp"), "fallback"));
        assert!(is(zone_cache_action(&no_row, part, key, true, "jade"), "fallback"));

        assert!(is_judilibre_cassation_source(Some("cass")));
        assert!(is_judilibre_cassation_source(Some("inca")));
        assert!(!is_judilibre_cassation_source(Some("capp")));
        assert!(!is_judilibre_cassation_source(Some("jade")));
        assert!(!is_judilibre_cassation_source(None));
    }

    #[test]
    fn phase2_gate_opens_with_a_passing_benchmark() {
        let dir = tempfile::tempdir().unwrap();
        let valid = dir.path().join("valid.json");
        std::fs::write(&valid, phase2_valid_benchmark_json()).unwrap();
        let benchmark = phase2_benchmark_payload_with_path(Some(&valid));
        let gate = phase2_gate_payload_with(
            &phase2_index_ready(),
            &phase2_ingest_available(),
            &phase2_corpus_both_families(),
            benchmark,
        );
        assert_eq!(gate["claim_allowed"], true);
        assert_eq!(gate["state"], "ready");
    }

    #[test]
    fn default_run_ids_are_unique_across_rapid_calls() {
        // Two rapid default run ids must differ, or ON CONFLICT(run_id) would let one run overwrite
        // another's manifest. Generate many in a tight loop (same second) and require all distinct.
        let ids: std::collections::HashSet<String> =
            (0..1000).map(|_| default_juri_run_id(ArchiveSource::Cass)).collect();
        assert_eq!(ids.len(), 1000);
        assert_ne!(default_legi_run_id(), default_legi_run_id());
    }

    #[test]
    fn normalize_since_accepts_date_and_compact_forms() {
        assert_eq!(normalize_since("2025-01-15").as_deref(), Some("20250115000000"));
        assert_eq!(normalize_since("20250201000000").as_deref(), Some("20250201000000"));
        // Only the two documented shapes are accepted; separators/noise/extra precision are rejected.
        assert_eq!(normalize_since("not-a-date"), None);
        assert_eq!(normalize_since("2025"), None);
        assert_eq!(normalize_since("2025/01/15"), None);
        assert_eq!(normalize_since("2025-01-15T00:00:00"), None);
        assert_eq!(normalize_since("abc20250115xyz"), None);
        assert_eq!(normalize_since("2025-1-5"), None);
    }

    #[test]
    fn heuristic_dispositif_is_utf8_safe_with_accents_before_marker() {
        // Accented French text before the marker must not panic or mis-slice (no to_uppercase).
        let body = "Considérant qu'il résulte des éléments versés aux débats que la décision est fondée. \
            PAR CES MOTIFS, la Cour REJETTE le pourvoi.";
        let dispositif = heuristic_dispositif(body).expect("dispositif found");
        assert!(dispositif.starts_with("PAR CES MOTIFS"));
        assert!(dispositif.contains("REJETTE"));
        // No marker -> None.
        assert_eq!(heuristic_dispositif("Texte sans marqueur de dispositif."), None);
    }

    #[test]
    fn heuristic_visa_collects_only_the_leading_block() {
        // A later reasoning line starting with "Vu" must NOT be included in the opening visa.
        let body = "En-tête de l'arrêt\nVu les articles 1240 et 1241 du code civil ;\nVu le code de procédure civile ;\nFaits et procédure\n1. Le demandeur soutient. Vu ce qui précède, il conclut.";
        let visa = heuristic_visa(body).expect("visa found");
        assert!(visa.contains("1240"));
        assert!(visa.contains("procédure civile"));
        assert!(!visa.contains("Faits"));
        assert!(!visa.contains("conclut"), "a later 'Vu' line leaked: {visa}");
    }

    #[test]
    fn heuristic_dispositif_matches_accented_decide() {
        let body = "Considérant ce qui suit.\nDécide, la Cour annule l'arrêt attaqué.";
        let dispositif = heuristic_dispositif(body).expect("accented dispositif found");
        assert!(dispositif.starts_with("Décide"));
        assert!(dispositif.contains("annule"));
    }

    #[test]
    fn decision_part_parse_is_lenient() {
        assert_eq!(DecisionPart::parse("Summary"), Some(DecisionPart::Summary));
        assert_eq!(DecisionPart::parse("sommaire"), Some(DecisionPart::Summary));
        assert_eq!(DecisionPart::parse("dispositif"), Some(DecisionPart::Dispositif));
        assert_eq!(DecisionPart::parse("MOYEN"), Some(DecisionPart::Moyens));
        assert_eq!(DecisionPart::parse("bogus"), None);
    }

    #[test]
    fn parse_pourvoi_accepts_dotted_and_plain_forms() {
        assert_eq!(parse_pourvoi("22-21.812").as_deref(), Some("22-21812"));
        assert_eq!(parse_pourvoi("22-21812").as_deref(), Some("22-21812"));
        assert_eq!(parse_pourvoi("57-10.110").as_deref(), Some("57-10110"));
        // Too few/many digits or wrong shape are rejected (conservative).
        assert_eq!(parse_pourvoi("1-2"), None);
        assert_eq!(parse_pourvoi("article 1240"), None);
        assert_eq!(parse_pourvoi("2024-01-01"), None); // date-like, right group too long
    }

    #[test]
    fn parse_citation_target_detects_decision_identifiers() {
        assert!(matches!(
            parse_citation_target("JURITEXT000051824029"),
            ParsedCitationTarget::DecisionSourceUid(uid) if uid == "JURITEXT000051824029"
        ));
        assert!(matches!(
            parse_citation_target("CETATEXT000051549953"),
            ParsedCitationTarget::DecisionSourceUid(uid) if uid == "CETATEXT000051549953"
        ));
        assert!(matches!(
            parse_citation_target("cass:JURITEXT000051824029"),
            ParsedCitationTarget::DecisionDocumentId { source_uid: Some(uid), .. }
                if uid == "JURITEXT000051824029"
        ));
        assert!(matches!(
            parse_citation_target("ECLI:FR:CCASS:2025:AP00683"),
            ParsedCitationTarget::DecisionEcli(ecli) if ecli == "ECLI:FR:CCASS:2025:AP00683"
        ));
        assert!(matches!(
            parse_citation_target("ecli:fr:ccass:2025:ap00683"),
            ParsedCitationTarget::DecisionEcli(ecli) if ecli == "ECLI:FR:CCASS:2025:AP00683"
        ));
        assert!(matches!(
            parse_citation_target("22-21.812"),
            ParsedCitationTarget::DecisionPourvoi(p) if p == "22-21812"
        ));
        // A statutory citation still routes to the article path, not a decision path.
        assert!(matches!(
            parse_citation_target("article 1240 du code civil"),
            ParsedCitationTarget::FreeTextArticle { .. }
        ));
    }

    /// Full command-matrix help guard (T0.1): every subcommand path must have an `about`, and every
    /// user-facing argument must have help text. Walks the entire clap tree so a new command/flag
    /// without help fails CI instead of shipping an undocumented surface.
    #[test]
    fn every_command_and_arg_has_help() {
        use clap::CommandFactory;
        fn check(cmd: &clap::Command, path: &str) {
            for arg in cmd.get_arguments() {
                let id = arg.get_id().as_str();
                if id == "help" || id == "version" || arg.is_hide_set() {
                    continue;
                }
                assert!(
                    arg.get_help().is_some() || arg.get_long_help().is_some(),
                    "{path}: argument `{id}` has no help text"
                );
            }
            for sub in cmd.get_subcommands() {
                assert!(
                    sub.get_about().is_some() || sub.get_long_about().is_some(),
                    "{path}: subcommand `{}` has no about text",
                    sub.get_name()
                );
                check(sub, &format!("{path} {}", sub.get_name()));
            }
        }
        check(&Cli::command(), "jurisearch");
    }

    /// Session-parity invariant: the warm protocol must reject exactly the one-shot-only commands
    /// with `not_implemented`, and must route (not reject) a handled command. Guards the dispatch
    /// arm against drift relative to `SESSION_EXCLUDED_COMMANDS`.
    #[test]
    fn session_dispatch_matches_one_shot_only_set() {
        // Iterate the contract's source of truth so the dispatcher and the constant cannot drift
        // (this is exactly the `eval france-legi` gap a hard-coded list missed).
        for cmd in SESSION_EXCLUDED_COMMANDS {
            let request = SessionRequest {
                id: None,
                command: cmd.to_string(),
                args: serde_json::json!({}),
            };
            let (response, exit) = dispatch_session_request(request);
            assert!(!exit, "session command `{cmd}` must not terminate the session");
            match response {
                SessionResponse::Err { error, .. } => assert!(
                    matches!(error.code, ErrorCode::NotImplemented),
                    "`{cmd}` should be not_implemented in session, got {:?}",
                    error.code
                ),
                SessionResponse::Ok { .. } => {
                    panic!("session command `{cmd}` should be not_implemented, got Ok")
                }
            }
        }
        // A handled command is routed: empty args yield bad_input (missing query), NOT not_implemented.
        let (response, _) = dispatch_session_request(SessionRequest {
            id: None,
            command: "search".to_string(),
            args: serde_json::json!({}),
        });
        match response {
            SessionResponse::Err { error, .. } => assert!(
                !matches!(error.code, ErrorCode::NotImplemented),
                "`search` must be routed, not not_implemented"
            ),
            SessionResponse::Ok { .. } => {}
        }
    }

    /// The `eval france-legi` artifact must be fully described by its registered schema (no
    /// emitted-but-unschema'd top-level key). Guards the contract's truthfulness for that command.
    #[test]
    fn france_legi_artifact_keys_are_schema_documented() {
        let artifact = france_legi_artifact(
            france_legi_category(1.0, 60, "structured_citation"),
            france_legi_category(1.0, 12, "structured_citation"),
            france_legi_category(0.5, 120, "hybrid"),
            FranceLegiGoldLimits {
                known_item: 60,
                temporal: 12,
                cross_reference: 120,
            },
            "phase1-freemium-20250713",
            "20250713-140000",
        );
        let schema = compiled_schema();
        let props = schema["schemas"]["EvalFranceLegiResponse"]["properties"]
            .as_object()
            .expect("EvalFranceLegiResponse.properties");
        let missing: Vec<String> = artifact
            .as_object()
            .unwrap()
            .keys()
            .filter(|key| !props.contains_key(key.as_str()))
            .cloned()
            .collect();
        assert!(
            missing.is_empty(),
            "france_legi_artifact keys absent from EvalFranceLegiResponse schema: {missing:?}"
        );
    }

    /// doctor is a non-owning preflight: it returns a ready flag + per-dependency checks and must NOT
    /// open the index (no ingest_health / phase1_gate, which would require starting Postgres).
    #[test]
    fn doctor_payload_is_a_non_owning_preflight() {
        let payload = doctor_payload(None);
        assert!(payload["ready"].is_boolean(), "doctor must report `ready`");
        let checks = payload["checks"].as_array().expect("doctor `checks` array");
        assert!(!checks.is_empty(), "doctor must run at least one check");
        for check in checks {
            assert!(check["name"].is_string(), "each check has a name");
            assert!(check["status"].is_string(), "each check has a status");
        }
        assert!(
            payload.get("ingest_health").is_none() && payload.get("phase1_gate").is_none(),
            "doctor must not open the index (no ingest_health/phase1_gate)"
        );
    }

    #[test]
    fn legi_citation_routing_parses_article_and_temporal_suffix() {
        // Plain citation: article number + parent-text hint, as-of from the caller default.
        let known = legi_citation_routing(
            "Décret n°73-645 du 18 juin 1973 COMPTABLE Article 33",
            "1973-07-14",
        )
        .expect("citation-shaped");
        assert_eq!(known.article_number, "33");
        assert_eq!(
            known.code_hint.as_deref(),
            Some("Décret n°73-645 du 18 juin 1973 COMPTABLE")
        );
        assert_eq!(known.as_of, "1973-07-14");

        // Temporal suffix overrides the as-of and is stripped from the article part.
        let temporal = legi_citation_routing(
            "Code de la sécurité sociale Article R242-40 en vigueur au 1990-06-01",
            "2026-01-01",
        )
        .expect("citation-shaped");
        assert_eq!(temporal.article_number, "R242-40");
        assert_eq!(
            temporal.code_hint.as_deref(),
            Some("Code de la sécurité sociale")
        );
        assert_eq!(temporal.as_of, "1990-06-01");
        // The temporal suffix is stripped from the citation used for exact-citation ranking.
        assert_eq!(
            temporal.citation_query,
            "Code de la sécurité sociale Article R242-40"
        );

        // Article reference with no leading text → no code hint.
        let bare = legi_citation_routing("Article L. 242-1", "2026-01-01").expect("citation-shaped");
        assert_eq!(bare.article_number, "L. 242-1");
        assert_eq!(bare.code_hint, None);

        // A non-date "en vigueur au" target falls back to the default as-of.
        let bad_date =
            legi_citation_routing("X Article 5 en vigueur au demain", "2026-01-01").expect("shaped");
        assert_eq!(bad_date.as_of, "2026-01-01");
        assert_eq!(bad_date.article_number, "5");

        // Conceptual queries (no article reference) are not citation-shaped.
        assert!(legi_citation_routing("responsabilité civile pour faute", "2026-01-01").is_none());
        assert!(legi_citation_routing("", "2026-01-01").is_none());
    }

    #[test]
    fn ascii_ci_search_handles_non_ascii_haystack() {
        assert_eq!(find_ascii_ci("Décret Article 1", "article "), Some(8));
        assert_eq!(rfind_ascii_ci("Article 1 Article 2", "article "), Some(10));
        assert_eq!(rfind_ascii_ci("no match here", "article "), None);
        assert!(is_iso_date("1990-06-01"));
        assert!(!is_iso_date("1990/06/01"));
        assert!(!is_iso_date("demain"));
    }

    #[test]
    fn replay_snapshot_cache_value_reports_skipped_when_absent() {
        assert_eq!(
            replay_snapshot_cache_value(None),
            json!({ "source": "skipped" })
        );
    }

    #[test]
    fn merge_embedding_endpoint_stats_sums_counters_per_base_url() {
        let mut accumulator = vec![json!({
            "base_url": "http://a", "request_model": "m",
            "requests": 2, "chunks": 10, "truncated_inputs": 1, "failures": 0
        })];
        merge_embedding_endpoint_stats(
            &mut accumulator,
            vec![
                json!({"base_url": "http://a", "request_model": "m", "requests": 3, "chunks": 15, "truncated_inputs": 0, "failures": 1}),
                json!({"base_url": "http://b", "request_model": "m", "requests": 1, "chunks": 5, "truncated_inputs": 0, "failures": 0}),
            ],
        );
        assert_eq!(accumulator.len(), 2);
        let a = accumulator
            .iter()
            .find(|entry| entry["base_url"] == "http://a")
            .expect("endpoint a present");
        assert_eq!(a["requests"], 5);
        assert_eq!(a["chunks"], 25);
        assert_eq!(a["truncated_inputs"], 1);
        assert_eq!(a["failures"], 1);
        let b = accumulator
            .iter()
            .find(|entry| entry["base_url"] == "http://b")
            .expect("endpoint b present");
        assert_eq!(b["requests"], 1);
        assert_eq!(b["chunks"], 5);
    }

    fn locked_embedding_manifest_json() -> Value {
        json!({
            "embedding_fingerprint": "bge-m3:1024:normalize:true",
            "model": "bge-m3",
            "dimension": 1024,
            "normalize": true,
            "provisional": true,
            "reembeddable": true
        })
    }

    fn check_status<'a>(payload: &'a Value, name: &str) -> &'a str {
        payload["checks"]
            .as_array()
            .and_then(|checks| checks.iter().find(|check| check["name"] == name))
            .and_then(|check| check["status"].as_str())
            .expect("phase1 gate check status exists")
    }

    fn gating_flag(payload: &Value, name: &str) -> Option<bool> {
        payload["checks"]
            .as_array()?
            .iter()
            .find(|check| check["name"] == name)
            .and_then(|check| check["gating"].as_bool())
    }

    #[test]
    fn external_benchmark_is_advisory_and_france_legi_gates() {
        let index = json!({ "query_ready": true });
        let ingest_health = json!({
            "state": "available",
            "latest_completed_run": "2026-06-21T20:00:00Z",
            "failed_members": 0,
            "projection_coverage": { "covered": 2, "total": 2 },
            "embedding_coverage": { "covered": 2, "total": 2 },
            "embedding_manifest": locked_embedding_manifest_json(),
            "replay_snapshot_status": "available",
            "replay_snapshot_source": "refreshed"
        });

        // Passing France-LEGI artifact + pending (advisory) BSARD external benchmark.
        let temp = tempfile::NamedTempFile::new().unwrap();
        fs::write(temp.path(), valid_france_legi_artifact().to_string()).unwrap();
        let france_legi = phase1_france_legi_payload_with_path(Some(temp.path()));
        let external = phase1_external_benchmark_default_payload();

        let payload = phase1_gate_payload_with(&index, &ingest_health, external, france_legi);

        // BSARD is advisory: its pending status must NOT block the claim.
        assert_eq!(
            check_status(&payload, "external_expert_annotated_eval"),
            "pending"
        );
        assert_eq!(
            gating_flag(&payload, "external_expert_annotated_eval"),
            Some(false)
        );
        // France-LEGI is the gating benchmark and it passed.
        assert_eq!(check_status(&payload, "france_legi_official_eval"), "pass");
        assert_eq!(
            gating_flag(&payload, "france_legi_official_eval"),
            Some(true)
        );
        // Claim opens because every GATING check passes.
        assert_eq!(payload["claim_allowed"], true);
        assert_eq!(payload["state"], "ready");

        // A failing France-LEGI artifact must re-close the claim even though BSARD is advisory.
        // Drop the gating structured-citation metric below its floor.
        let bad = tempfile::NamedTempFile::new().unwrap();
        let mut artifact = valid_france_legi_artifact();
        artifact["categories"]["structured_citation_resolution"]["metric_value"] = json!(0.10);
        fs::write(bad.path(), artifact.to_string()).unwrap();
        let failing_france_legi = phase1_france_legi_payload_with_path(Some(bad.path()));
        let reclosed = phase1_gate_payload_with(
            &index,
            &ingest_health,
            phase1_external_benchmark_default_payload(),
            failing_france_legi,
        );
        assert_eq!(check_status(&reclosed, "france_legi_official_eval"), "fail");
        assert_eq!(reclosed["claim_allowed"], false);
    }

    fn test_eval_fixture() -> LegalRetrievalFixture {
        LegalRetrievalFixture {
            id: "fixture".to_string(),
            tier: FixtureTier::ReleaseGating,
            category: "known_article_statutory".to_string(),
            query: "query".to_string(),
            expected_ids: vec!["legi:expected@2024-01-01".to_string()],
            allowed_alternates: vec!["legi:alternate@2024-01-01".to_string()],
            as_of: Some("2024-01-01".to_string()),
            temporal_expectation: None,
            hierarchy: None,
            drafted_by: "codex".to_string(),
            verified_against: "official source".to_string(),
            reviewer: None,
            review_status: ReviewStatus::OfficialSourceChecked,
            rationale: "test fixture".to_string(),
        }
    }

    fn search_with_candidate_ids(ids: &[Option<&str>]) -> Value {
        json!({
            "retrieval_mode": "bm25",
            "pagination": { "returned": ids.len() },
            "diagnostics": {
                "retrieval": {
                    "mode": "bm25",
                    "uses_lexical": true,
                    "uses_dense": false
                }
            },
            "candidates": ids
                .iter()
                .map(|id| match id {
                    Some(id) => json!({ "document_id": id }),
                    None => json!({ "chunk_id": "missing-document-id" }),
                })
                .collect::<Vec<_>>()
        })
    }

    #[test]
    fn eval_phase1_fixture_search_result_reports_expected_alternate_and_miss() {
        let fixture = test_eval_fixture();

        let expected_hit = eval_phase1_fixture_search_result(
            &fixture,
            search_with_candidate_ids(&[
                Some("legi:other@2024-01-01"),
                Some("legi:expected@2024-01-01"),
            ]),
        );
        assert_eq!(expected_hit["status"], "pass");
        assert_eq!(expected_hit["passed"], true);
        assert_eq!(expected_hit["best_expected_rank"], 2);
        assert_eq!(
            expected_hit["matched_document_id"],
            "legi:expected@2024-01-01"
        );

        let alternate_hit = eval_phase1_fixture_search_result(
            &fixture,
            search_with_candidate_ids(&[
                Some("legi:other@2024-01-01"),
                None,
                Some("legi:alternate@2024-01-01"),
            ]),
        );
        assert_eq!(alternate_hit["status"], "pass_allowed_alternate");
        assert_eq!(alternate_hit["passed"], true);
        assert_eq!(alternate_hit["best_allowed_alternate_rank"], 2);
        assert_eq!(
            alternate_hit["top_document_ids"],
            json!(["legi:other@2024-01-01", "legi:alternate@2024-01-01"])
        );

        let miss = eval_phase1_fixture_search_result(
            &fixture,
            search_with_candidate_ids(&[Some("legi:other@2024-01-01")]),
        );
        assert_eq!(miss["status"], "fail");
        assert_eq!(miss["passed"], false);
        assert!(miss["best_expected_rank"].is_null());
        assert!(miss["matched_document_id"].is_null());
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
            "embedding_manifest": locked_embedding_manifest_json(),
            "replay_snapshot_status": "available",
            "replay_snapshot_source": "refreshed"
        });

        // Use the pure builder with default (pending) benchmark payloads so the assertions do
        // not depend on the ambient JURISEARCH_PHASE1_*_BENCHMARK env vars.
        let payload = phase1_gate_payload_with(
            &index,
            &ingest_health,
            phase1_external_benchmark_default_payload(),
            phase1_france_legi_default_payload(),
        );

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
            check_status(&payload, "external_expert_annotated_eval"),
            "pending"
        );
        assert_eq!(payload["external_benchmark"]["state"], "pending");
        assert_eq!(
            payload["external_benchmark"]["primary_candidate"],
            "maastrichtlawtech/bsard"
        );
        assert!(
            payload["external_benchmark"]["evidence"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            check_status(&payload, "france_legi_official_eval"),
            "pending"
        );
        assert_eq!(payload["france_legi_benchmark"]["state"], "pending");
        assert_eq!(payload["france_legi_benchmark"]["jurisdiction"], "france");
        assert_eq!(check_status(&payload, "reranker_decision"), "pass");
        assert_eq!(payload["reranker_decision"]["state"], "deferred");
        assert_eq!(payload["reranker_decision"]["provider"], "disabled");
        assert_eq!(payload["reranker_decision"]["adopted"], false);
        assert!(
            payload["reranker_decision"]["reason"]
                .as_str()
                .unwrap()
                .contains("cannot measure a material rerank gain")
        );
        assert_eq!(payload["state"], "not_ready");
        assert_eq!(payload["claim_allowed"], false);

        let mut failed_ingest_health = ingest_health.clone();
        failed_ingest_health["failed_members"] = json!(2);
        let failed_payload = phase1_gate_payload(&index, &failed_ingest_health);

        assert_eq!(check_status(&failed_payload, "failed_members"), "fail");
        assert_eq!(failed_payload["state"], "not_ready");
        assert_eq!(failed_payload["claim_allowed"], false);

        let provisional_payload = phase1_gate_payload(&index, &ingest_health);
        assert_eq!(
            check_status(&provisional_payload, "final_embedding_model"),
            "pass"
        );

        let mut wrong_model_ingest_health = ingest_health.clone();
        wrong_model_ingest_health["embedding_manifest"]["model"] = json!("other-model");
        let wrong_model_payload = phase1_gate_payload(&index, &wrong_model_ingest_health);
        assert_eq!(
            check_status(&wrong_model_payload, "final_embedding_model"),
            "fail"
        );
        assert_eq!(wrong_model_payload["claim_allowed"], false);

        let mut wrong_dimension_ingest_health = ingest_health.clone();
        wrong_dimension_ingest_health["embedding_manifest"]["dimension"] = json!(768);
        let wrong_dimension_payload = phase1_gate_payload(&index, &wrong_dimension_ingest_health);
        assert_eq!(
            check_status(&wrong_dimension_payload, "final_embedding_model"),
            "fail"
        );

        let mut wrong_normalize_ingest_health = ingest_health.clone();
        wrong_normalize_ingest_health["embedding_manifest"]["normalize"] = json!(false);
        let wrong_normalize_payload = phase1_gate_payload(&index, &wrong_normalize_ingest_health);
        assert_eq!(
            check_status(&wrong_normalize_payload, "final_embedding_model"),
            "fail"
        );

        let mut wrong_fingerprint_ingest_health = ingest_health.clone();
        wrong_fingerprint_ingest_health["embedding_manifest"]["embedding_fingerprint"] =
            json!("bge-m3:768:normalize:true");
        let wrong_fingerprint_payload =
            phase1_gate_payload(&index, &wrong_fingerprint_ingest_health);
        assert_eq!(
            check_status(&wrong_fingerprint_payload, "final_embedding_model"),
            "fail"
        );

        let mut missing_manifest_ingest_health = ingest_health.clone();
        missing_manifest_ingest_health["embedding_manifest"] = json!({});
        let missing_manifest_payload = phase1_gate_payload(&index, &missing_manifest_ingest_health);
        assert_eq!(
            check_status(&missing_manifest_payload, "final_embedding_model"),
            "fail"
        );
    }

    #[test]
    fn external_benchmark_check_status_requires_evidence_for_pass() {
        assert_eq!(
            phase1_external_benchmark_check_status(&json!({
                "state": "pending",
                "evidence": []
            })),
            "pending"
        );
        assert_eq!(
            phase1_external_benchmark_check_status(&json!({
                "state": "failed",
                "evidence": ["work/03-implementation/02-evidence/failed.json"]
            })),
            "fail"
        );
        assert_eq!(
            phase1_external_benchmark_check_status(&json!({
                "state": "passed",
                "evidence": []
            })),
            "fail"
        );
        assert_eq!(
            phase1_external_benchmark_check_status(&json!({
                "state": "passed",
                "evidence": ["work/03-implementation/02-evidence/external-benchmark.json"]
            })),
            "pass"
        );
    }

    #[test]
    fn external_benchmark_payload_consumes_valid_metrics_artifact() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        let artifact = json!({
            "schema_version": 1,
            "kind": "phase1_external_expert_benchmark",
            "state": "passed",
            "dataset": {
                "id": "maastrichtlawtech/bsard",
                "revision": "test-revision",
                "question_split": "test",
                "jurisdiction": "belgium",
                "usage_scope": "eval_only",
                "license": "cc-by-nc-sa-4.0",
                "corpus_documents": 22633,
                "questions": 222,
                "limit_corpus": null,
                "limit_questions": null
            },
            "claim_scope": "external expert-annotated French-language statutory retrieval benchmark",
            "applicability": "Belgian statutory questions are used as a French-language statutory retrieval proxy, not as France-LEGI gold.",
            "embedding": {
                "fingerprint_model": "bge-m3",
                "request_model": "baai/bge-m3",
                "dimension": 1024,
                "normalize": true
            },
            "thresholds": {
                "hybrid_recall_at_20_min": 0.8,
                "hybrid_ndcg_at_20_min": 0.6,
                "hybrid_mrr_at_20_min": 0.5
            },
            "metrics": {
                "hybrid": {
                    "recall_at_20": 0.86,
                    "ndcg_at_20": 0.72,
                    "mrr_at_20": 0.58
                }
            },
            "evidence": [
                "work/03-implementation/02-evidence/phase1-external-benchmark.json"
            ]
        });
        fs::write(temp.path(), artifact.to_string()).unwrap();

        let payload = phase1_external_benchmark_payload_with_path(Some(temp.path()));

        assert_eq!(payload["state"], "passed");
        assert_eq!(payload["source"], json!(PHASE1_EXTERNAL_BENCHMARK_ENV));
        assert_eq!(payload["artifact_error"], Value::Null);
        assert_eq!(payload["dataset"]["revision"], "test-revision");
        assert_eq!(phase1_external_benchmark_check_status(&payload), "pass");
    }

    fn valid_france_legi_artifact() -> Value {
        json!({
            "schema_version": 1,
            "kind": "phase1_france_legi_benchmark",
            "state": "passed",
            "jurisdiction": "france",
            "claim_scope": "France-LEGI official-evidence statutory retrieval",
            "source": "DILA LEGI (Licence Ouverte) official fields",
            "retriever": "production jurisearch search (BM25+dense+RRF)",
            "embedding": {
                "fingerprint_model": "bge-m3",
                "dimension": 1024,
                "normalize": true
            },
            "thresholds": {
                "structured_citation_recall_at_10_min": 0.95,
                "temporal_version_exactness_at_10_min": 0.90,
                "semantic_retrieval_recall_at_10_advisory": 0.40
            },
            "categories": {
                "structured_citation_resolution": { "metric_value": 1.0, "queries": 60, "gating": true, "routing_backends": { "structured_citation": 60 } },
                "temporal_version_pinning": { "metric_value": 1.0, "queries": 12, "gating": true, "routing_backends": { "structured_citation": 12 } },
                "semantic_retrieval": { "metric_value": 0.12, "queries": 80, "gating": false, "advisory": true, "routing_backends": { "hybrid": 80 } }
            },
            "provenance": {
                "official_source": "DILA LEGI Freemium_legi_global_20250713 (Licence Ouverte)",
                "source_revision": "20250713-140000",
                "pipeline": "jurisearch search BM25+dense+RRF",
                "code_version": "test-commit",
                "index_revision": "phase1-freemium-20250713",
                "sampled": false,
                "human_in_gold": false,
                "llm_in_gold": false
            },
            "evidence": [
                "work/03-implementation/02-evidence/2026-06-22-france-legi-eval-phase1-live-hybrid.json"
            ]
        })
    }

    #[test]
    fn france_legi_payload_consumes_valid_artifact() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        fs::write(temp.path(), valid_france_legi_artifact().to_string()).unwrap();

        let payload = phase1_france_legi_payload_with_path(Some(temp.path()));

        assert_eq!(payload["state"], "passed");
        assert_eq!(payload["source"], json!(PHASE1_FRANCE_LEGI_BENCHMARK_ENV));
        assert_eq!(payload["artifact_error"], Value::Null);
        assert_eq!(payload["jurisdiction"], "france");
        assert_eq!(
            payload["categories"]["structured_citation_resolution"]["queries"],
            60
        );
        assert_eq!(payload["provenance"]["human_in_gold"], false);
        assert_eq!(phase1_france_legi_check_status(&payload), "pass");
    }

    #[test]
    fn france_legi_payload_rejects_bad_provenance() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        let mut artifact = valid_france_legi_artifact();
        artifact["provenance"]["sampled"] = json!(true);
        artifact["provenance"]["human_in_gold"] = json!(true);
        // whitespace + case variant must still be rejected as unpinned
        artifact["provenance"]["source_revision"] = json!("  UNKNOWN  ");
        artifact["provenance"]
            .as_object_mut()
            .unwrap()
            .remove("official_source");
        fs::write(temp.path(), artifact.to_string()).unwrap();

        let payload = phase1_france_legi_payload_with_path(Some(temp.path()));

        assert_eq!(payload["state"], "failed");
        assert_eq!(phase1_france_legi_check_status(&payload), "fail");
        let error = payload["artifact_error"].as_str().unwrap();
        assert!(error.contains("provenance.official_source is required"));
        assert!(error.contains("provenance.source_revision must be pinned, not `unknown`"));
        assert!(error.contains("provenance.sampled must be false"));
        assert!(error.contains("provenance.human_in_gold must be false"));
    }

    #[test]
    fn france_legi_payload_with_no_path_is_pending() {
        let payload = phase1_france_legi_payload_with_path(None);
        assert_eq!(payload["state"], "pending");
        assert_eq!(payload["jurisdiction"], "france");
        assert_eq!(phase1_france_legi_check_status(&payload), "pending");
    }

    #[test]
    fn france_legi_payload_rejects_low_metrics_wrong_jurisdiction_and_small_eval() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        fs::write(
            temp.path(),
            json!({
                "schema_version": 1,
                "kind": "phase1_france_legi_benchmark",
                "state": "passed",
                "jurisdiction": "belgium",
                "claim_scope": "x",
                "source": "x",
                "retriever": "x",
                "embedding": { "fingerprint_model": "bge-m3", "dimension": 1024, "normalize": true },
                "thresholds": {
                    "structured_citation_recall_at_10_min": 0.50,
                    "temporal_version_exactness_at_10_min": 0.90,
                    "semantic_retrieval_recall_at_10_advisory": 0.40
                },
                "categories": {
                    "structured_citation_resolution": { "metric_value": 0.40, "queries": 3 },
                    "temporal_version_pinning": { "metric_value": 0.95, "queries": 2 }
                },
                "evidence": []
            })
            .to_string(),
        )
        .unwrap();

        let payload = phase1_france_legi_payload_with_path(Some(temp.path()));

        assert_eq!(payload["state"], "failed");
        assert_eq!(phase1_france_legi_check_status(&payload), "fail");
        let error = payload["artifact_error"].as_str().unwrap();
        assert!(error.contains("passed artifact must include non-empty evidence"));
        assert!(error.contains("jurisdiction must be `france`"));
        assert!(
            error.contains("thresholds.structured_citation_recall_at_10_min must be at least 0.950")
        );
        assert!(
            error.contains("categories.structured_citation_resolution.metric_value must be at least threshold")
        );
        assert!(
            error.contains("categories.structured_citation_resolution.queries must be at least 10")
        );
        assert!(error.contains("categories.temporal_version_pinning.queries must be at least 4"));
        // The advisory semantic category still requires its metric to be recorded.
        assert!(error.contains("categories.semantic_retrieval.metric_value is required"));
    }

    #[test]
    fn france_legi_check_status_requires_evidence_for_pass() {
        assert_eq!(
            phase1_france_legi_check_status(&json!({ "state": "pending", "evidence": [] })),
            "pending"
        );
        assert_eq!(
            phase1_france_legi_check_status(&json!({ "state": "passed", "evidence": [] })),
            "fail"
        );
        assert_eq!(
            phase1_france_legi_check_status(&json!({ "state": "passed", "evidence": ["e"] })),
            "pass"
        );
        assert_eq!(
            phase1_france_legi_check_status(&json!({ "state": "failed", "evidence": ["e"] })),
            "fail"
        );
    }

    fn france_legi_category(metric: f64, queries: usize, backend: &str) -> FranceLegiCategoryResult {
        FranceLegiCategoryResult {
            metric,
            queries,
            backends: json!({ backend: queries }),
        }
    }

    #[test]
    fn france_legi_runner_artifact_passes_when_structured_floors_met_even_if_semantic_low() {
        let artifact = france_legi_artifact(
            france_legi_category(1.0, 60, "structured_citation"),
            france_legi_category(1.0, 12, "structured_citation"),
            // semantic well below its advisory floor (0.40) — must NOT block the claim.
            france_legi_category(0.116, 120, "hybrid"),
            FranceLegiGoldLimits {
                known_item: 60,
                temporal: 12,
                cross_reference: 120,
            },
            "phase1-freemium-20250713",
            "20250713-140000",
        );
        assert_eq!(artifact["state"], "passed");
        assert_eq!(artifact["jurisdiction"], "france");
        assert_eq!(artifact["provenance"]["source_revision"], "20250713-140000");
        assert_eq!(
            artifact["categories"]["structured_citation_resolution"]["queries"],
            60
        );
        assert_eq!(
            artifact["categories"]["structured_citation_resolution"]["gating"],
            true
        );
        assert_eq!(artifact["categories"]["semantic_retrieval"]["gating"], false);
        assert_eq!(
            artifact["categories"]["semantic_retrieval"]["advisory"],
            true
        );
        // The routing-backend audit is recorded per category.
        assert_eq!(
            artifact["categories"]["structured_citation_resolution"]["routing_backends"]
                ["structured_citation"],
            60
        );

        // The runner's output must be a VALID, passing artifact for the status gate.
        let errors = phase1_france_legi_artifact_errors(&artifact);
        assert!(
            errors.is_empty(),
            "runner artifact failed gate validation: {errors:?}"
        );
        let temp = tempfile::NamedTempFile::new().unwrap();
        fs::write(temp.path(), artifact.to_string()).unwrap();
        let payload = phase1_france_legi_payload_with_path(Some(temp.path()));
        assert_eq!(payload["state"], "passed");
        assert_eq!(phase1_france_legi_check_status(&payload), "pass");
    }

    #[test]
    fn france_legi_runner_artifact_fails_below_gating_floor_or_too_few_queries() {
        // below the structured-citation recall floor (0.95)
        assert_eq!(
            france_legi_artifact(
                france_legi_category(0.40, 60, "structured_citation"),
                france_legi_category(1.0, 12, "structured_citation"),
                france_legi_category(0.70, 120, "hybrid"),
                FranceLegiGoldLimits::default(),
                "idx",
                "rev"
            )["state"],
            "failed"
        );
        // too few temporal queries (a GATING category; min is 4)
        assert_eq!(
            france_legi_artifact(
                france_legi_category(1.0, 60, "structured_citation"),
                france_legi_category(1.0, 3, "structured_citation"),
                france_legi_category(0.70, 120, "hybrid"),
                FranceLegiGoldLimits::default(),
                "idx",
                "rev"
            )["state"],
            "failed"
        );
    }

    #[test]
    fn france_legi_gate_requires_structured_routing_audit() {
        // A gating category that claims structured metrics but was served by hybrid must be rejected.
        let mut hybrid_served = valid_france_legi_artifact();
        hybrid_served["categories"]["structured_citation_resolution"]["routing_backends"] =
            json!({ "hybrid": 60 });
        assert!(
            phase1_france_legi_artifact_errors(&hybrid_served)
                .iter()
                .any(|error| error.contains("structured_citation must equal queries")),
            "hybrid-served gating category must be rejected"
        );

        // A missing routing audit must be rejected.
        let mut no_audit = valid_france_legi_artifact();
        no_audit["categories"]["temporal_version_pinning"]
            .as_object_mut()
            .unwrap()
            .remove("routing_backends");
        assert!(
            phase1_france_legi_artifact_errors(&no_audit)
                .iter()
                .any(|error| error
                    .contains("categories.temporal_version_pinning.routing_backends is required")),
            "missing routing audit must be rejected"
        );

        // Backend accounting that does not cover every query must be rejected.
        let mut partial = valid_france_legi_artifact();
        partial["categories"]["structured_citation_resolution"]["routing_backends"] =
            json!({ "structured_citation": 40 });
        assert!(
            phase1_france_legi_artifact_errors(&partial)
                .iter()
                .any(|error| error.contains("must account for all 60 queries")),
            "incomplete backend accounting must be rejected"
        );
    }

    #[test]
    fn france_legi_runner_state_and_status_agree_at_floor_boundary() {
        // Just below the 0.95 structured floor: the runner fails on the RAW metric, and the floored
        // recorded metric (0.949) also fails status re-derivation — no divergence.
        let below = france_legi_artifact(
            france_legi_category(0.9496, 60, "structured_citation"),
            france_legi_category(1.0, 12, "structured_citation"),
            france_legi_category(0.116, 120, "hybrid"),
            FranceLegiGoldLimits::default(),
            "idx",
            "rev",
        );
        assert_eq!(below["state"], "failed");
        assert_eq!(
            below["categories"]["structured_citation_resolution"]["metric_value"],
            json!(0.949)
        );
        assert!(!phase1_france_legi_artifact_errors(&below).is_empty());

        // At/above the floor: the runner passes and status accepts.
        let at = france_legi_artifact(
            france_legi_category(0.9504, 60, "structured_citation"),
            france_legi_category(1.0, 12, "structured_citation"),
            france_legi_category(0.116, 120, "hybrid"),
            FranceLegiGoldLimits::default(),
            "idx",
            "rev",
        );
        assert_eq!(at["state"], "passed");
        assert_eq!(
            at["categories"]["structured_citation_resolution"]["metric_value"],
            json!(0.950)
        );
        let errors = phase1_france_legi_artifact_errors(&at);
        assert!(errors.is_empty(), "{errors:?}");
    }

    #[test]
    fn france_legi_document_id_helpers() {
        assert_eq!(
            legi_source_uid_of("legi:LEGIARTI000006284600@1998-05-21"),
            Some("LEGIARTI000006284600")
        );
        assert_eq!(
            legi_document_as_of("legi:LEGIARTI000006284600@1998-05-21"),
            Some("1998-05-21")
        );
        assert_eq!(legi_source_uid_of("nonsense"), None);
        assert_eq!(legi_document_as_of("nonsense"), None);
        // floor_metric truncates (never rounds up), so a below-floor raw metric cannot become a
        // passing recorded value: 0.9496 -> 0.949 (< 0.95 floor), 0.9504 -> 0.950 (>= floor).
        assert!((floor_metric(0.4284) - 0.428).abs() < 1e-9);
        assert!((floor_metric(0.9496) - 0.949).abs() < 1e-9);
        assert!((floor_metric(0.9504) - 0.950).abs() < 1e-9);
        assert!(floor_metric(0.9496) < 0.95);
        assert!(floor_metric(0.95) >= 0.95);
        assert!((mean(3, 4) - 0.75).abs() < 1e-9);
        assert_eq!(mean(0, 0), 0.0);
    }

    #[test]
    fn external_benchmark_payload_fails_invalid_pass_artifact() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        fs::write(
            temp.path(),
            json!({
                "schema_version": 1,
                "kind": "phase1_external_expert_benchmark",
                "state": "passed",
                "dataset": {
                    "id": "maastrichtlawtech/bsard",
                    "question_split": "test",
                    "jurisdiction": "belgium",
                    "usage_scope": "eval_only",
                    "license": "cc-by-nc-sa-4.0",
                    "limit_corpus": 10
                },
                "evidence": []
            })
            .to_string(),
        )
        .unwrap();

        let payload = phase1_external_benchmark_payload_with_path(Some(temp.path()));

        assert_eq!(payload["state"], "failed");
        assert_eq!(phase1_external_benchmark_check_status(&payload), "fail");
        let error = payload["artifact_error"].as_str().unwrap();
        assert!(error.contains("passed artifact must include non-empty evidence"));
        assert!(error.contains("dataset.revision is required"));
        assert!(error.contains("dataset.limit_corpus must be null"));
        assert!(error.contains("embedding.fingerprint_model must be `bge-m3`"));
        assert!(error.contains("metrics is required"));
    }

    #[test]
    fn external_benchmark_payload_rejects_zero_threshold_pass_artifact() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        fs::write(
            temp.path(),
            json!({
                "schema_version": 1,
                "kind": "phase1_external_expert_benchmark",
                "state": "passed",
                "dataset": {
                    "id": "maastrichtlawtech/bsard",
                    "revision": "test-revision",
                    "question_split": "test",
                    "jurisdiction": "belgium",
                    "usage_scope": "eval_only",
                    "license": "cc-by-nc-sa-4.0",
                    "corpus_documents": 22633,
                    "questions": 222,
                    "limit_corpus": null,
                    "limit_questions": null
                },
                "claim_scope": "external expert-annotated French-language statutory retrieval benchmark",
                "applicability": "Belgian statutory questions are used as a French-language statutory retrieval proxy, not as France-LEGI gold.",
                "embedding": {
                    "fingerprint_model": "bge-m3",
                    "request_model": "baai/bge-m3",
                    "dimension": 1024,
                    "normalize": true
                },
                "thresholds": {
                    "hybrid_recall_at_20_min": 0.0,
                    "hybrid_ndcg_at_20_min": 0.0,
                    "hybrid_mrr_at_20_min": 0.0
                },
                "metrics": {
                    "hybrid": {
                        "recall_at_20": 1.0,
                        "ndcg_at_20": 1.0,
                        "mrr_at_20": 1.0
                    }
                },
                "evidence": [
                    "work/03-implementation/02-evidence/phase1-external-benchmark.json"
                ]
            })
            .to_string(),
        )
        .unwrap();

        let payload = phase1_external_benchmark_payload_with_path(Some(temp.path()));

        assert_eq!(payload["state"], "failed");
        let error = payload["artifact_error"].as_str().unwrap();
        assert!(error.contains("thresholds.hybrid_recall_at_20_min must be at least 0.750"));
        assert!(error.contains("thresholds.hybrid_ndcg_at_20_min must be at least 0.600"));
        assert!(error.contains("thresholds.hybrid_mrr_at_20_min must be at least 0.500"));
    }

    #[test]
    fn external_benchmark_payload_rejects_unknown_dataset_revision() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        fs::write(
            temp.path(),
            json!({
                "schema_version": 1,
                "kind": "phase1_external_expert_benchmark",
                "state": "passed",
                "dataset": {
                    "id": "maastrichtlawtech/bsard",
                    "revision": "unknown",
                    "question_split": "test",
                    "jurisdiction": "belgium",
                    "usage_scope": "eval_only",
                    "license": "cc-by-nc-sa-4.0",
                    "corpus_documents": 22633,
                    "questions": 222,
                    "limit_corpus": null,
                    "limit_questions": null
                },
                "claim_scope": "external expert-annotated French-language statutory retrieval benchmark",
                "applicability": "Belgian statutory questions are used as a French-language statutory retrieval proxy, not as France-LEGI gold.",
                "embedding": {
                    "fingerprint_model": "bge-m3",
                    "request_model": "baai/bge-m3",
                    "dimension": 1024,
                    "normalize": true
                },
                "thresholds": {
                    "hybrid_recall_at_20_min": 0.8,
                    "hybrid_ndcg_at_20_min": 0.6,
                    "hybrid_mrr_at_20_min": 0.5
                },
                "metrics": {
                    "hybrid": {
                        "recall_at_20": 0.86,
                        "ndcg_at_20": 0.72,
                        "mrr_at_20": 0.58
                    }
                },
                "evidence": [
                    "work/03-implementation/02-evidence/phase1-external-benchmark.json"
                ]
            })
            .to_string(),
        )
        .unwrap();

        let payload = phase1_external_benchmark_payload_with_path(Some(temp.path()));

        assert_eq!(payload["state"], "failed");
        assert!(
            payload["artifact_error"]
                .as_str()
                .unwrap()
                .contains("dataset.revision must be pinned")
        );
    }
}
