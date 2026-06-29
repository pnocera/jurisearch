//! LEGI archive ingestion — thin CLI consumer. The archive-run implementation (per-member LEGI
//! processing, accounting, manifest, scoped hierarchy backfill) moved to `jurisearch-pipeline`
//! (work/10 M1-C seam S4); this delegates to [`jurisearch_pipeline::ingest_archives`]. The FULL
//! hierarchy backfill payload (`ingest backfill-legi-hierarchy`) stays here — it is a maintenance
//! command, not a named producer seam.

use crate::*;

/// Thin wrapper over the library ingest seam: open the producer index (managed, BulkIngest profile),
/// run the LEGI archive ingestion against it, and render the historical CLI JSON (the library report's
/// `body` plus the CLI-owned `index_dir`).
#[allow(clippy::too_many_arguments)]
pub(crate) fn ingest_legi_archives_payload(
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
    let report = jurisearch_pipeline::ingest_archives(
        &postgres,
        jurisearch_pipeline::IngestArchivesRequest {
            source: ArchiveSource::Legi,
            archives_dir,
            run_id,
            limit_members,
            max_member_bytes,
            quarantine_dir,
            safe_mode,
            filter: archive_filter,
        },
    )
    .map_err(jurisearch_pipeline::IngestError::into_error_object)?;
    Ok(with_index_dir(report.body, index_dir.as_path()))
}

pub(crate) fn backfill_legi_hierarchy_payload(
    index_dir: Option<&Path>,
) -> Result<Value, ErrorObject> {
    let index_dir = require_existing_index_dir(index_dir)?;
    let postgres = open_index(index_dir.as_path())?;
    // Hierarchy backfill can delete chunk_embeddings / clear embedding fingerprints, making the
    // index no longer query-ready; drop the readiness cache up front so a stale "ready" entry can
    // never let a subsequent search skip the live coverage check.
    invalidate_cached_query_readiness(&postgres).map_err(storage_error_object)?;
    let run_id = crate::ingest::producer_run_id("backfill-legi-hierarchy");
    let outbox = jurisearch_storage::outbox::OutboxContext::new(
        &run_id,
        jurisearch_storage::migrations::CURRENT_SCHEMA_VERSION,
    );
    let report = backfill_legi_article_hierarchy_from_metadata(&postgres, Some(&outbox))
        .map_err(storage_error_object)?;
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
