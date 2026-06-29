//! JURI (jurisprudence) archive ingestion — thin CLI consumer. The archive-run implementation moved to
//! `jurisearch-pipeline` (work/10 M1-C seam S4); this delegates to
//! [`jurisearch_pipeline::ingest_archives`] and renders the historical CLI JSON.

use crate::*;

#[allow(clippy::too_many_arguments)]
pub(crate) fn ingest_juri_archives_payload(
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
    let index_dir = require_configured_index_dir(index_dir)?;
    let postgres = open_index_for_bulk_ingest(index_dir.as_path())?;
    let report = jurisearch_pipeline::ingest_archives(
        &postgres,
        jurisearch_pipeline::IngestArchivesRequest {
            source,
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
