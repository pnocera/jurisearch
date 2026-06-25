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
    authority::{
        AUTHORITY_DEFAULT_BAND, AUTHORITY_RERANK_WINDOW, authority_rerank,
        effective_authority_weight,
    },
    citation::{CitationLookupQuery, citation_lookup_json},
    decision_zones::{
        UpsertDecisionZones, decision_resolution_metadata_with_client, decision_zones_json,
        upsert_decision_zones_with_client,
    },
    dense::{
        DENSE_VECTOR_DIMENSION, DenseRebuildSpec, finalize_dense_rebuild,
        load_chunk_embedding_inputs,
    },
    france_juris::{
        FranceJurisGoldLimits, FranceJurisZoneGoldLimits, france_juris_gold_json,
        france_juris_index_revision, france_juris_zone_gold_json,
    },
    france_legi::{FranceLegiGoldLimits, france_legi_gold_json},
    ingest_accounting::{
        IngestCompatibility, IngestErrorInput, IngestHealthReport, IngestMemberInput,
        IngestMemberStatus, IngestResumeAction, IngestRunInput, IngestRunStatus,
        ReplaySnapshotMode, ReplaySnapshotReport, finish_ingest_run_with_client,
        ingest_resume_decision_with_client, invalidate_cached_query_readiness,
        load_ingest_health_with_replay_snapshot_mode, load_or_compute_query_readiness,
        record_ingest_error_with_client, record_ingest_member_with_client, refresh_replay_snapshot,
        start_ingest_run_with_client, update_ingest_member_status_with_client,
        update_ingest_run_manifest_with_client,
    },
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
        finalize_zone_dense_rebuild, insert_zone_unit_embeddings,
        load_derivable_decision_zones_json, load_zone_unit_embedding_inputs,
        replace_zone_units_for_document, zone_resolver_reachable_json,
        zone_retrieval_coverage_json,
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
mod eval;
mod gates;
mod index_runtime;
mod ingest;
mod legifrance_search;
mod output;
mod query_support;
mod request;
mod retrieval;
mod serve;
mod session;
mod status;

use crate::args::*;
use crate::ascii::*;
use crate::citation::*;
use crate::date::*;
use crate::embedding_runtime::*;
use crate::enrichment::*;
use crate::errors::*;
use crate::eval::*;
use crate::gates::*;
use crate::index_runtime::*;
use crate::ingest::*;
use crate::legifrance_search::*;
use crate::output::*;
use crate::query_support::*;
use crate::request::*;
use crate::retrieval::*;
use crate::status::*;

const LEGI_PARSER_VERSION: &str = "legi_article_metadata_parser:v4";
const CANONICAL_SCHEMA_VERSION: &str = "canonical_record:v3";
const CLI_CODE_VERSION: &str = concat!("jurisearch-cli:", env!("CARGO_PKG_VERSION"));
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

// ---- General retrieval eval harness (`eval run`) -----------------------------------------------

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

// ===== DILA bulk jurisprudence (decision) ingestion ==========================================

/// Number of pending chunks loaded per page when streaming the full embed run, bounding peak memory.
const EMBED_STREAM_PAGE_SIZE: u32 = 20_000;

// ===== Phase 2 gate (full French juridic search) ==============================================

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
mod tests;
