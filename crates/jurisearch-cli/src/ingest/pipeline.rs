//! Embedding + zone-unit pipeline: embed-chunks, enrich-zones (Judilibre backfill), build-zone-units derivation, and embed-zone-units.

use crate::*;

/// Outcome of a single decision enrichment attempt, for backfill accounting.
#[derive(Clone, Copy)]
pub(crate) enum ZoneEnrichOutcome {
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
pub(crate) fn enrich_zones_payload(
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

    let coverage: Value = serde_json::from_str(
        &zone_retrieval_coverage_json(&postgres).map_err(storage_error_object)?,
    )
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
pub(crate) fn enrich_zone_page_concurrently(
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
                    let mut db = match postgres::Client::connect(connection_string, postgres::NoTls)
                    {
                        Ok(db) => db,
                        // Whole slice fails to connect -> count as errors, don't abort the run.
                        Err(_) => return vec![ZoneEnrichOutcome::Error; group.len()],
                    };
                    let piste = PisteClient::new(OfficialApiConfig::from_env());
                    group
                        .into_iter()
                        .map(|doc_id| {
                            match enrich_decision_from_judilibre_with_client(
                                &mut db, &piste, doc_id,
                            ) {
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
pub(crate) fn worker_outcomes_or_errors(
    returned: Option<Vec<ZoneEnrichOutcome>>,
    group_len: usize,
) -> Vec<ZoneEnrichOutcome> {
    returned.unwrap_or_else(|| vec![ZoneEnrichOutcome::Error; group_len])
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

/// `ingest embed-zone-units`: embed `zone_units` via the SAME OpenRouter pool + fingerprint as
/// `embed-chunks`, then finalize the separate zone-unit dense ANN index. Mirrors the embed-chunks
/// streaming/finalize flow against the zone tables; the chunk dense path is untouched.
pub(crate) fn embed_zone_units_payload(
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

pub(crate) fn embed_chunks_payload(
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
