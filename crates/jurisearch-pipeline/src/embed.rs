//! Embed seam (S6): document/chunk + zone-unit embedding over a [`DbClientSource`].
//!
//! [`embed_documents`] embeds the pending chunk OR zone-unit set across the bulk endpoint pool and
//! finalizes the corresponding dense ANN index, against any producer DB. The injected
//! [`EmbeddingConfig`] keeps the storage fingerprint fields (`model_name`/`dimension`/`normalize`)
//! strictly separate from the wire-only `request_model`/`base_url` — see the regression test at the
//! bottom of this module asserting `request_model` NEVER enters `storage_embedding_fingerprint()`.

use crate::*;

use crate::embedding::EmbeddingPoolEndpoint;

/// Which pending set to embed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbedTarget {
    /// Document chunks → `chunk_embeddings` + the chunk dense index.
    Chunks,
    /// Derived zone units → `zone_unit_embeddings` + the zone dense index.
    ZoneUnits,
}

/// Inputs for one [`embed_documents`] pass. `pool_endpoints` is the resolved endpoint pool (empty =
/// derive from the config's `base_url(s)`); the env/TOML loader that produces it stays in the CLI.
#[derive(Debug, Clone)]
pub struct EmbedRequest {
    pub target: EmbedTarget,
    pub limit: Option<u32>,
    pub index_lists: u32,
    pub batch_size: usize,
    pub pool_concurrency: usize,
    pub pool_endpoints: Vec<EmbeddingPoolEndpoint>,
}

/// What one embed pass produced. `body` carries the historical `ingest embed-*` payload (minus
/// `index_dir`, injected by the CLI); the typed counters are the producer's contract surface.
#[derive(Debug, Clone)]
pub struct EmbedReport {
    pub target: EmbedTarget,
    pub chunks_considered: u64,
    pub embeddings_inserted: u64,
    pub embedding_inputs_truncated: u64,
    pub endpoint_stats: Vec<Value>,
    pub replay_snapshot: Option<ReplaySnapshotReport>,
    pub body: Value,
}

/// Embed the pending chunk/zone-unit set (S6) and finalize its dense index.
///
/// # Errors
/// [`EmbedError`] on an embedding-endpoint failure, a fingerprint/dimension mismatch, or a DB failure.
pub fn embed_documents(
    db: &impl DbClientSource,
    cfg: &EmbeddingConfig,
    req: EmbedRequest,
) -> Result<EmbedReport, EmbedError> {
    // `embed_documents` is now the public seam, so it MUST reject the zero-valued request fields the
    // CLI validated before delegating (`jurisearch-cli/src/ingest.rs`): `batch_size == 0` panics in
    // `inputs.chunks(0)` and `pool_concurrency == 0` spawns no pool workers (the no-limit streaming
    // path then reloads the same pending page forever). Fire before any DB/embedder work.
    validate_embed_request(&req).map_err(EmbedError::from)?;
    let result = match req.target {
        EmbedTarget::Chunks => embed_chunks_inner(db, cfg, req),
        EmbedTarget::ZoneUnits => embed_zone_units_inner(db, cfg, req),
    };
    result.map_err(EmbedError::from)
}

/// Reject zero-valued request fields with the CLI's exact `bad_input` contract (per [`EmbedTarget`]).
/// `index_lists == 0` stays valid (auto-scale the ivfflat lists to the corpus size).
fn validate_embed_request(req: &EmbedRequest) -> Result<(), ErrorObject> {
    let command = match req.target {
        EmbedTarget::Chunks => "ingest embed-chunks",
        EmbedTarget::ZoneUnits => "ingest embed-zone-units",
    };
    if req.limit == Some(0) {
        return Err(ErrorObject::bad_input(format!(
            "{command} --limit must be at least 1 when provided"
        )));
    }
    if req.batch_size == 0 {
        return Err(ErrorObject::bad_input(format!(
            "{command} --batch-size must be at least 1"
        )));
    }
    if req.pool_concurrency == 0 {
        return Err(ErrorObject::bad_input(format!(
            "{command} --pool-concurrency must be at least 1"
        )));
    }
    Ok(())
}

fn dense_dimension(embedding_config: &EmbeddingConfig) -> Result<i32, ErrorObject> {
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
    Ok(dimension)
}

fn embed_chunks_inner(
    db: &impl DbClientSource,
    embedding_config: &EmbeddingConfig,
    req: EmbedRequest,
) -> Result<EmbedReport, ErrorObject> {
    let EmbedRequest {
        limit,
        index_lists,
        batch_size,
        pool_concurrency,
        pool_endpoints,
        ..
    } = req;
    let mut client = db.client().map_err(storage_error_object)?;
    let run_id = producer_run_id("embed-chunks");
    let outbox = jurisearch_storage::outbox::OutboxContext::new(
        &run_id,
        jurisearch_storage::migrations::CURRENT_SCHEMA_VERSION,
    );
    // Re-embedding changes embedding coverage; drop the readiness cache up front so the next query
    // recomputes (it is repopulated only when the index is fully ready again).
    invalidate_query_readiness(&mut client).map_err(storage_error_object)?;
    ensure_embedding_runtime_ready(embedding_config, false)?;
    let expected_fingerprint = embedding_config.fingerprint();
    let embedding_fingerprint = embedding_config.storage_embedding_fingerprint();
    let endpoint_configs = embedding_endpoint_pool_configs(
        embedding_config,
        &pool_endpoints,
        &expected_fingerprint,
        embedding_fingerprint.as_str(),
    )?;
    let dimension = dense_dimension(embedding_config)?;

    // Embedding upserts and dense finalization are separate recoverable steps: re-running the command
    // converges before the manifest/index is advertised.
    let embedding_run = if let Some(limit) = limit {
        let inputs = load_chunk_embedding_inputs_with_client(
            &mut client,
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
            &mut client,
            inputs,
            &endpoint_configs,
            embedding_fingerprint.as_str(),
            embedding_config,
            batch_size,
            pool_concurrency,
            Some(&outbox),
        )?
    } else {
        // Production path: stream pending chunks in bounded pages so peak memory is one page.
        let mut run = EmbeddingPoolRun {
            chunks_considered: 0,
            embeddings_inserted: 0,
            embedding_inputs_truncated: 0,
            endpoint_stats: Vec::new(),
        };
        loop {
            let page = load_chunk_embedding_inputs_with_client(
                &mut client,
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
                &mut client,
                page,
                &endpoint_configs,
                embedding_fingerprint.as_str(),
                embedding_config,
                batch_size,
                pool_concurrency,
                Some(&outbox),
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
    let rebuild = finalize_dense_rebuild_with_client(
        &mut client,
        &DenseRebuildSpec {
            embedding_fingerprint: embedding_fingerprint.as_str(),
            model: embedding_config.model.as_str(),
            dimension,
            normalize: embedding_config.normalize,
            provisional: embedding_config.provisional,
            reembeddable: embedding_config.reembeddable,
            index_lists,
        },
        Some(&outbox),
    )
    .map_err(storage_error_object)?;
    let replay_snapshot = maybe_refresh_replay_snapshot(&mut client)?;

    let body = json!({
        "schema_version": SCHEMA_VERSION,
        "command": "ingest embed-chunks",
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
            "pool": embedding_pool_endpoints_status_json(&pool_endpoints),
            "pool_overrides_base_urls": !pool_endpoints.is_empty(),
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
    });

    Ok(EmbedReport {
        target: EmbedTarget::Chunks,
        chunks_considered: embedding_run.chunks_considered as u64,
        embeddings_inserted: embedding_run.embeddings_inserted as u64,
        embedding_inputs_truncated: embedding_run.embedding_inputs_truncated as u64,
        endpoint_stats: embedding_run.endpoint_stats,
        replay_snapshot,
        body,
    })
}

fn embed_zone_units_inner(
    db: &impl DbClientSource,
    embedding_config: &EmbeddingConfig,
    req: EmbedRequest,
) -> Result<EmbedReport, ErrorObject> {
    let EmbedRequest {
        limit,
        index_lists,
        batch_size,
        pool_concurrency,
        pool_endpoints,
        ..
    } = req;
    let mut client = db.client().map_err(storage_error_object)?;
    let run_id = producer_run_id("embed-zone-units");
    let outbox = jurisearch_storage::outbox::OutboxContext::new(
        &run_id,
        jurisearch_storage::migrations::CURRENT_SCHEMA_VERSION,
    );
    ensure_embedding_runtime_ready(embedding_config, false)?;
    let expected_fingerprint = embedding_config.fingerprint();
    let embedding_fingerprint = embedding_config.storage_embedding_fingerprint();
    let endpoint_configs = embedding_endpoint_pool_configs(
        embedding_config,
        &pool_endpoints,
        &expected_fingerprint,
        embedding_fingerprint.as_str(),
    )?;
    let dimension = dense_dimension(embedding_config)?;

    let to_chunk_inputs = |inputs: Vec<ZoneUnitEmbeddingInput>| {
        inputs
            .into_iter()
            .map(|input| ChunkEmbeddingInput {
                chunk_id: input.zone_unit_id,
                embedding_text: input.embedding_text,
            })
            .collect::<Vec<_>>()
    };

    let embedding_run = if let Some(limit) = limit {
        let inputs = load_zone_unit_embedding_inputs_with_client(
            &mut client,
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
            &mut client,
            to_chunk_inputs(inputs),
            &endpoint_configs,
            embedding_fingerprint.as_str(),
            embedding_config,
            batch_size,
            pool_concurrency,
            Some(&outbox),
        )?
    } else {
        let mut run = EmbeddingPoolRun {
            chunks_considered: 0,
            embeddings_inserted: 0,
            embedding_inputs_truncated: 0,
            endpoint_stats: Vec::new(),
        };
        loop {
            let page = load_zone_unit_embedding_inputs_with_client(
                &mut client,
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
                &mut client,
                to_chunk_inputs(page),
                &endpoint_configs,
                embedding_fingerprint.as_str(),
                embedding_config,
                batch_size,
                pool_concurrency,
                Some(&outbox),
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

    let rebuild = finalize_zone_dense_rebuild_with_client(
        &mut client,
        &DenseRebuildSpec {
            embedding_fingerprint: embedding_fingerprint.as_str(),
            model: embedding_config.model.as_str(),
            dimension,
            normalize: embedding_config.normalize,
            provisional: embedding_config.provisional,
            reembeddable: embedding_config.reembeddable,
            index_lists,
        },
        Some(&outbox),
    )
    .map_err(storage_error_object)?;

    let body = json!({
        "schema_version": SCHEMA_VERSION,
        "command": "ingest embed-zone-units",
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
    });

    Ok(EmbedReport {
        target: EmbedTarget::ZoneUnits,
        chunks_considered: embedding_run.chunks_considered as u64,
        embeddings_inserted: embedding_run.embeddings_inserted as u64,
        embedding_inputs_truncated: embedding_run.embedding_inputs_truncated as u64,
        endpoint_stats: embedding_run.endpoint_stats,
        replay_snapshot: None,
        body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use jurisearch_core::error::ErrorCode;

    fn request_with(
        target: EmbedTarget,
        limit: Option<u32>,
        batch: usize,
        pool: usize,
    ) -> EmbedRequest {
        EmbedRequest {
            target,
            limit,
            index_lists: 0,
            batch_size: batch,
            pool_concurrency: pool,
            pool_endpoints: Vec::new(),
        }
    }

    /// The public seam MUST reject zero-valued request fields BEFORE any DB/embedder work, returning
    /// the CLI's exact `bad_input` contract — otherwise `inputs.chunks(0)` panics and a zero-worker
    /// pool hangs (`crates/jurisearch-cli/src/ingest.rs:199,205,210` and `:267,273,278`).
    #[test]
    fn validate_embed_request_rejects_zero_values_with_cli_contract() {
        // limit == Some(0) — both targets, exact per-command message.
        let err = validate_embed_request(&request_with(EmbedTarget::Chunks, Some(0), 32, 4))
            .expect_err("limit 0 must be rejected");
        assert_eq!(err.code, ErrorCode::BadInput);
        assert_eq!(
            err.message,
            "ingest embed-chunks --limit must be at least 1 when provided"
        );
        let err = validate_embed_request(&request_with(EmbedTarget::ZoneUnits, Some(0), 32, 4))
            .expect_err("limit 0 must be rejected");
        assert_eq!(err.code, ErrorCode::BadInput);
        assert_eq!(
            err.message,
            "ingest embed-zone-units --limit must be at least 1 when provided"
        );

        // batch_size == 0 — both targets.
        let err = validate_embed_request(&request_with(EmbedTarget::Chunks, None, 0, 4))
            .expect_err("batch_size 0 must be rejected");
        assert_eq!(err.code, ErrorCode::BadInput);
        assert_eq!(
            err.message,
            "ingest embed-chunks --batch-size must be at least 1"
        );
        let err = validate_embed_request(&request_with(EmbedTarget::ZoneUnits, None, 0, 4))
            .expect_err("batch_size 0 must be rejected");
        assert_eq!(err.code, ErrorCode::BadInput);
        assert_eq!(
            err.message,
            "ingest embed-zone-units --batch-size must be at least 1"
        );

        // pool_concurrency == 0 — both targets.
        let err = validate_embed_request(&request_with(EmbedTarget::Chunks, None, 32, 0))
            .expect_err("pool_concurrency 0 must be rejected");
        assert_eq!(err.code, ErrorCode::BadInput);
        assert_eq!(
            err.message,
            "ingest embed-chunks --pool-concurrency must be at least 1"
        );
        let err = validate_embed_request(&request_with(EmbedTarget::ZoneUnits, None, 32, 0))
            .expect_err("pool_concurrency 0 must be rejected");
        assert_eq!(err.code, ErrorCode::BadInput);
        assert_eq!(
            err.message,
            "ingest embed-zone-units --pool-concurrency must be at least 1"
        );

        // `--index-lists 0` stays valid (auto-scale), and a valid request passes.
        assert!(validate_embed_request(&request_with(EmbedTarget::Chunks, Some(1), 1, 1)).is_ok());
    }

    /// The pool driver itself is belt-and-suspenders: zero `batch_size`/`pool_concurrency` return a
    /// `bad_input` error rather than panicking in `inputs.chunks(0)` or spawning zero workers.
    #[test]
    fn embed_and_insert_with_pool_rejects_zero_values() {
        let mut insert = |_: &[crate::embedding::OwnedChunkEmbedding]| Ok(0usize);
        let err = crate::embedding::embed_and_insert_with_pool(
            Vec::new(),
            &[],
            0, // batch_size
            4,
            &mut insert,
        )
        .expect_err("batch_size 0 must be rejected, not panic");
        assert_eq!(err.code, ErrorCode::BadInput);

        let err = crate::embedding::embed_and_insert_with_pool(
            Vec::new(),
            &[],
            32,
            0, // pool_concurrency
            &mut insert,
        )
        .expect_err("pool_concurrency 0 must be rejected, not hang");
        assert_eq!(err.code, ErrorCode::BadInput);
    }

    /// CRITICAL confidentiality/portability invariant (work/10 S6): the wire-only `request_model`
    /// (e.g. an OpenRouter alias) AND the `base_url` must NEVER influence
    /// `storage_embedding_fingerprint()`. The storage fingerprint keys ONLY on `model`, `dimension`,
    /// and `normalize` (see `jurisearch-embed/src/fingerprint.rs`) — so two configs that differ ONLY
    /// in `request_model`, or ONLY in `base_url` (including a different base-url class), produce the
    /// SAME storage fingerprint, and a producer using a hosted alias still writes rows that an
    /// installed site (bge-m3 on a loopback endpoint) can read.
    #[test]
    fn request_model_and_base_url_never_enter_storage_embedding_fingerprint() {
        let base = EmbeddingConfig::openai_compatible(
            "https://openrouter.ai/api/v1",
            Some("secret".to_owned()),
            "bge-m3",
            DENSE_VECTOR_DIMENSION as usize,
            true,
            "cls",
        );
        let mut with_request_model = base.clone();
        with_request_model.request_model = Some("baai/bge-m3:free".to_owned());
        let mut with_other_request_model = base.clone();
        with_other_request_model.request_model = Some("some/other-alias".to_owned());

        // The storage fingerprint is invariant under `request_model`.
        assert_eq!(
            base.storage_embedding_fingerprint(),
            with_request_model.storage_embedding_fingerprint(),
        );
        assert_eq!(
            with_request_model.storage_embedding_fingerprint(),
            with_other_request_model.storage_embedding_fingerprint(),
        );

        // And it is ALSO invariant under `base_url` — same model/dimension/normalize across materially
        // different base URLs, including different base-url classes (loopback vs external host).
        let expected = base.storage_embedding_fingerprint();
        for base_url in [
            "http://127.0.0.1:8080/v1",       // local loopback
            "http://localhost:9999/v1",       // loopback by name
            "https://openrouter.ai/api/v1",   // external host
            "https://embeddings.example.com", // a different external host
        ] {
            let config = EmbeddingConfig::openai_compatible(
                base_url,
                Some("secret".to_owned()),
                "bge-m3",
                DENSE_VECTOR_DIMENSION as usize,
                true,
                "cls",
            );
            assert_eq!(
                config.storage_embedding_fingerprint(),
                expected,
                "base_url `{base_url}` leaked into storage_embedding_fingerprint",
            );
        }

        // And `request_model` is not a substring of the storage fingerprint string.
        let fingerprint = with_request_model.storage_embedding_fingerprint();
        assert!(
            !fingerprint.contains("baai/bge-m3:free"),
            "request_model leaked into storage_embedding_fingerprint: {fingerprint}"
        );

        // `request_model()` still resolves the wire model (so the request path is unaffected).
        assert_eq!(with_request_model.request_model(), "baai/bge-m3:free");
        assert_eq!(base.request_model(), "bge-m3");
    }
}
