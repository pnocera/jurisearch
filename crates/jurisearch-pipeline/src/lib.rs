//! `jurisearch-pipeline` — reusable corpus pipeline library (work/10 milestone M1-C, seams S4–S6).
//!
//! Hosts the producer's ingest → enrich → embed library APIs, extracted from `jurisearch-cli` so the
//! future `jurisearch-producer` can run them **in-process** against any [`DbClientSource`] (a
//! self-managed `ManagedPostgres` OR an external operator-run PostgreSQL), not just the embedded
//! managed server. Each entrypoint takes a typed request and returns a typed report/error:
//!
//! - [`ingest_archives`] (S4) — DILA LEGI/JURI `.tar.gz` archive ingestion.
//! - [`enrich_zones`] (S5) — official Judilibre zone backfill (honest `EnrichmentMode`).
//! - [`embed_documents`] (S6) — document/chunk + zone-unit embedding over the endpoint pool.
//!
//! Dependency direction is one-way: `jurisearch-cli` → `jurisearch-pipeline`. This crate never
//! depends on `jurisearch-cli`; it uses `jurisearch_core::error::ErrorObject` (a shared core protocol
//! type) directly and defines its own typed errors at the boundary.

// ---- Crate prelude -----------------------------------------------------------------------------
// The extracted modules use `use crate::*;` exactly as they did inside `jurisearch-cli`. Re-export
// the same external + internal symbol surface from the crate root so those globs resolve here.

pub(crate) use std::borrow::Cow;
pub(crate) use std::collections::{BTreeMap, BTreeSet, VecDeque};
pub(crate) use std::net::{TcpStream, ToSocketAddrs};
pub(crate) use std::path::{Path, PathBuf};
pub(crate) use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
pub(crate) use std::sync::{Arc, Mutex, mpsc};
pub(crate) use std::time::{Duration, SystemTime, UNIX_EPOCH};
pub(crate) use std::{fs, thread};

pub(crate) use jurisearch_core::SCHEMA_VERSION;
pub(crate) use jurisearch_core::error::ErrorObject;
pub(crate) use jurisearch_embed::{
    EmbeddingConfig, EmbeddingFingerprint, EmbeddingProvider, OpenAiCompatibleClient,
};
pub(crate) use jurisearch_ingest::archive::{
    ArchiveMember, ArchivePlan, ArchiveSource, ArchiveVisit, PlannedArchive,
    for_each_xml_member_until, plan_from_dir,
};
pub(crate) use jurisearch_ingest::juri::{JuriParseError, ParsedJuriXml, parse_juri_member};
pub(crate) use jurisearch_ingest::legi::{
    LegiParseError, ParsedLegiXml, parse_legi_member, source_payload_hash,
};
pub(crate) use jurisearch_official_api::{
    OfficialApiConfig, OfficialApiExchange, OfficialApiOutcome, PisteClient,
};
pub(crate) use jurisearch_storage::backend::DbClientSource;
pub(crate) use jurisearch_storage::decision_zones::{
    UpsertDecisionZones, decision_resolution_metadata_json,
    decision_resolution_metadata_with_client, decision_zones_json,
    upsert_decision_zones_with_client,
};
pub(crate) use jurisearch_storage::dense::{
    ChunkEmbeddingInput, DENSE_VECTOR_DIMENSION, DenseRebuildSpec,
    finalize_dense_rebuild_with_client, load_chunk_embedding_inputs_with_client,
};
pub(crate) use jurisearch_storage::ingest_accounting::{
    IngestCompatibility, IngestErrorInput, IngestMemberInput, IngestMemberStatus,
    IngestResumeAction, IngestRunInput, IngestRunStatus, ReplaySnapshotReport,
    finish_ingest_run_with_client, ingest_resume_decision_with_client, invalidate_query_readiness,
    record_ingest_error_with_client, record_ingest_member_with_client,
    refresh_replay_snapshot_with_client, start_ingest_run_with_client,
    update_ingest_member_status_with_client, update_ingest_run_manifest_with_client,
};
pub(crate) use jurisearch_storage::official_api_archive::{
    InsertOfficialApiResponse, insert_official_api_response_with_client,
};
pub(crate) use jurisearch_storage::projection::{
    ChunkEmbeddingInsert, DocumentProjectionStatements, LegiHierarchyBackfillScope,
    LegiMetadataRoot, LegiProjectionStatements,
    backfill_legi_article_hierarchy_from_metadata_scoped_with_client,
    insert_chunk_embeddings_with_client, insert_decision_documents_with_statements,
    insert_legi_documents_with_statements, insert_legi_metadata_roots_with_client,
    prepare_document_projection_statements, prepare_legi_projection_statements,
};
pub(crate) use jurisearch_storage::runtime::{ManagedPostgres, StorageError};
pub(crate) use jurisearch_storage::zone_units::{
    EnrichZoneOrder, ZoneUnitEmbeddingInput, ZoneUnitEmbeddingInsert,
    enrich_zone_candidates_json_with_client, finalize_zone_dense_rebuild_with_client,
    insert_zone_unit_embeddings_with_client, load_zone_unit_embedding_inputs_with_client,
    zone_retrieval_coverage_with_client,
};
pub(crate) use serde_json::{Value, json};
pub(crate) use url::Url;

mod error;
pub(crate) use crate::error::*;

pub mod embedding;
pub(crate) use crate::embedding::*;

pub mod enrichment;
pub(crate) use crate::enrichment::*;

mod embed;
mod enrich;
mod ingest;

pub(crate) use crate::ingest::*;

// ---- Public API --------------------------------------------------------------------------------

pub use crate::embed::{EmbedReport, EmbedRequest, EmbedTarget, embed_documents};
pub use crate::enrich::{EnrichOutcome, EnrichRequest, EnrichmentMode, enrich_zones};
pub use crate::error::{EmbedError, EnrichError, IngestError};
pub use crate::ingest::{ArchiveSyncFilter, IngestArchivesRequest, IngestReport, ingest_archives};

// ---- Producer-pipeline constants (extracted verbatim from `jurisearch-cli`) --------------------

pub(crate) const LEGI_PARSER_VERSION: &str = "legi_article_metadata_parser:v4";
pub(crate) const CANONICAL_SCHEMA_VERSION: &str = "canonical_record:v3";
pub(crate) const CLI_CODE_VERSION: &str = concat!("jurisearch-cli:", env!("CARGO_PKG_VERSION"));
pub(crate) const LEGI_INGEST_TRANSACTION_BATCH_SIZE: usize = 128;
pub(crate) const LEGI_INGEST_TRANSACTION_BATCH_BYTE_LIMIT: usize = 64 * 1024 * 1024;
/// Candidate page size for the zone backfill keyset scan.
pub(crate) const ENRICH_ZONES_PAGE_SIZE: u32 = 200;
/// Number of pending rows loaded per page when streaming the full embed run, bounding peak memory.
pub(crate) const EMBED_STREAM_PAGE_SIZE: u32 = 20_000;

/// Case-insensitive (ASCII-only) reverse substring search — the last index of `needle` in `haystack`.
/// Byte-identical re-implementation of `jurisearch_query::rfind_ascii_ci` (kept local so the pipeline
/// avoids a `jurisearch-query` dependency edge). Folds only ASCII bytes, so accented forms differ.
pub(crate) fn rfind_ascii_ci(haystack: &str, needle: &str) -> Option<usize> {
    let (haystack, needle) = (haystack.as_bytes(), needle.as_bytes());
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    (0..=haystack.len() - needle.len()).rev().find(|&start| {
        haystack[start..start + needle.len()]
            .iter()
            .zip(needle)
            .all(|(left, right)| left.eq_ignore_ascii_case(right))
    })
}
