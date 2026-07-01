//! Embedding + zone-unit pipeline — thin CLI consumers over `jurisearch-pipeline` (work/10 M1-C):
//! `enrich-zones` (S5), `embed-chunks` / `embed-zone-units` (S6). The zone-unit DERIVATION
//! (`build-zone-units`) stays here — it is a producer maintenance step, not a named library seam — and
//! runs directly against the managed index via the storage helpers.

use crate::*;

/// `ingest enrich-zones`: official Judilibre zone backfill. Delegates to
/// [`jurisearch_pipeline::enrich_zones`]; preserves the CLI's fail-fast on absent PISTE credentials
/// (the library's honest `SkippedNoCredentials` outcome is for the producer cycle).
pub(crate) fn enrich_zones_payload(
    index_dir: Option<&Path>,
    source: &str,
    limit: Option<u32>,
    since: Option<&str>,
    concurrency: usize,
    order: CliEnrichZoneOrder,
) -> Result<Value, ErrorObject> {
    // Preflight credentials via the SAME config the workers use (`OfficialApiConfig::from_env`), which
    // accepts `JURISEARCH_PISTE_JUDILIBRE_KEY_ID` / `PISTE_API_KEY` (production) or
    // `PISTE_SANDBOX_API_KEY` (sandbox), so a supported deployment is not rejected up front.
    let api_config = OfficialApiConfig::from_env();
    if api_config.judilibre_key_id.is_none() {
        return Err(dependency_unavailable(
            "no Judilibre (PISTE) API key configured; set JURISEARCH_PISTE_JUDILIBRE_KEY_ID or \
             PISTE_API_KEY (PISTE_SANDBOX_API_KEY in sandbox) before running zone enrichment",
        ));
    }
    let index_dir = require_existing_index_dir(index_dir)?;
    let postgres = open_index(index_dir.as_path())?;
    let piste = PisteClient::new(api_config);
    let outcome = jurisearch_pipeline::enrich_zones(
        &postgres,
        Some(&piste),
        jurisearch_pipeline::EnrichRequest {
            source,
            limit,
            since,
            concurrency,
            order: order.into(),
        },
    )
    .map_err(jurisearch_pipeline::EnrichError::into_error_object)?;
    Ok(with_index_dir(outcome.body, index_dir.as_path()))
}

/// Derive a decision's `zone_units` rows from its cached `zones_json` object (motivations/moyens/
/// dispositif fragment text). One row per non-empty fragment with a contiguous per-zone `fragment_index`.
/// Borrows the fragment text from `zones`, so the returned rows must be used before `zones` is dropped.
pub(crate) fn derive_zone_unit_rows<'a>(
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
pub(crate) fn build_zone_units_payload(
    index_dir: Option<&Path>,
    limit: Option<u32>,
    rebuild: bool,
) -> Result<Value, ErrorObject> {
    let index_dir = require_existing_index_dir(index_dir)?;
    let postgres = open_index(index_dir.as_path())?;
    let run_id = crate::ingest::producer_run_id("build-zone-units");
    let outbox = jurisearch_storage::outbox::OutboxContext::new(
        &run_id,
        jurisearch_storage::migrations::CURRENT_SCHEMA_VERSION,
    );

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
            replace_zone_units_for_document(&postgres, document_id, &rows, Some(&outbox))
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

    let coverage: Value = serde_json::from_str(
        &zone_retrieval_coverage_json(&postgres).map_err(storage_error_object)?,
    )
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

/// `ingest embed-zone-units`: embed `zone_units` via the bulk endpoint pool + finalize the zone dense
/// index. Delegates to [`jurisearch_pipeline::embed_documents`].
pub(crate) fn embed_zone_units_payload(
    index_dir: Option<&Path>,
    limit: Option<u32>,
    index_lists: u32,
    batch_size: usize,
    pool_concurrency: usize,
) -> Result<Value, ErrorObject> {
    let index_dir = require_existing_index_dir(index_dir)?;
    let postgres = open_index(index_dir.as_path())?;
    let loaded = loaded_embedding_config();
    let report = jurisearch_pipeline::embed_documents(
        &postgres,
        &loaded.config,
        jurisearch_pipeline::EmbedRequest {
            target: jurisearch_pipeline::EmbedTarget::ZoneUnits,
            limit,
            index_lists,
            batch_size,
            pool_concurrency,
            pool_endpoints: loaded.pool_endpoints,
            // Zone embedding never refreshes the replay snapshot, but keep the field explicit/`true` for
            // a uniform CLI contract (no silent default-to-false).
            refresh_replay_snapshot: true,
        },
    )
    .map_err(jurisearch_pipeline::EmbedError::into_error_object)?;
    Ok(with_index_dir(report.body, index_dir.as_path()))
}

/// `ingest embed-chunks`: embed pending document chunks via the bulk endpoint pool + finalize the chunk
/// dense index. Delegates to [`jurisearch_pipeline::embed_documents`].
pub(crate) fn embed_chunks_payload(
    index_dir: Option<&Path>,
    limit: Option<u32>,
    index_lists: u32,
    batch_size: usize,
    pool_concurrency: usize,
) -> Result<Value, ErrorObject> {
    let index_dir = require_existing_index_dir(index_dir)?;
    let postgres = open_index(index_dir.as_path())?;
    let loaded = loaded_embedding_config();
    let report = jurisearch_pipeline::embed_documents(
        &postgres,
        &loaded.config,
        jurisearch_pipeline::EmbedRequest {
            target: jurisearch_pipeline::EmbedTarget::Chunks,
            limit,
            index_lists,
            batch_size,
            pool_concurrency,
            pool_endpoints: loaded.pool_endpoints,
            // CLI chunk embedding keeps the prior behavior: always refresh the replay snapshot (still
            // honoring the JURISEARCH_SKIP_REPLAY_SNAPSHOT env skip).
            refresh_replay_snapshot: true,
        },
    )
    .map_err(jurisearch_pipeline::EmbedError::into_error_object)?;
    Ok(with_index_dir(report.body, index_dir.as_path()))
}
