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
mod eval;
mod gates;
mod index_runtime;
mod ingest;
mod legifrance_search;
mod output;
mod query_support;
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
