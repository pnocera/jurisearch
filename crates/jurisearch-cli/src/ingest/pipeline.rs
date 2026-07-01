//! Embedding + zone-unit pipeline — thin CLI consumers over `jurisearch-pipeline` (work/10 M1-C):
//! `enrich-zones` (S5), `build-zone-units`, `embed-chunks` / `embed-zone-units` (S6). The zone-unit
//! DERIVATION now lives in `jurisearch-pipeline` (so the producer `update` cycle can run it in-process);
//! `build_zone_units_payload` opens the managed index and delegates to
//! [`jurisearch_pipeline::build_zone_units`], preserving the historical CLI response JSON shape.

use crate::*;

/// `ingest enrich-zones`: official Judilibre zone backfill. Delegates to
/// [`jurisearch_pipeline::enrich_zones`]; preserves the CLI's fail-fast on absent PISTE credentials
/// (the library's honest `SkippedNoCredentials` outcome is for the producer cycle).
pub(crate) fn enrich_zones_payload(
    index_dir: Option<&Path>,
    source: &str,
    limit: Option<u32>,
    since: Option<&str>,
    min_decision_date: Option<&str>,
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
            min_decision_date,
            concurrency,
            order: order.into(),
        },
    )
    .map_err(jurisearch_pipeline::EnrichError::into_error_object)?;
    Ok(with_index_dir(outcome.body, index_dir.as_path()))
}

/// `ingest build-zone-units`: derive `zone_units` from the cached official zones in `decision_zones`.
/// Opens the managed index and delegates to [`jurisearch_pipeline::build_zone_units`] (the shared
/// derivation the producer also runs in-process), preserving the historical CLI response JSON shape.
pub(crate) fn build_zone_units_payload(
    index_dir: Option<&Path>,
    limit: Option<u32>,
    rebuild: bool,
) -> Result<Value, ErrorObject> {
    let index_dir = require_existing_index_dir(index_dir)?;
    let postgres = open_index(index_dir.as_path())?;
    let outcome = jurisearch_pipeline::build_zone_units(
        &postgres,
        jurisearch_pipeline::BuildZoneUnitsRequest { limit, rebuild },
    )
    .map_err(jurisearch_pipeline::BuildZoneUnitsError::into_error_object)?;
    Ok(json!({
        "schema_version": SCHEMA_VERSION,
        "command": "ingest build-zone-units",
        "index_dir": index_dir.display().to_string(),
        "builder_version": jurisearch_pipeline::ZONE_UNIT_BUILDER_VERSION,
        "rebuild": rebuild,
        "decisions_derived": outcome.decisions_derived,
        "zone_units_written": outcome.zone_units_written,
        "coverage": outcome.coverage,
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
