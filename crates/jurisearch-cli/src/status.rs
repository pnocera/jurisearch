//! Status / health / introspection commands: status, doctor, stats, inspect, versions,
//! diff, setup, model fetch, and the ingest/zone-retrieval health blocks they compose.
//! Release-gate logic lives in `crate::gates`.

use crate::*;

pub(crate) fn model_fetch_payload(
    model: Option<String>,
    allow_download: bool,
) -> Result<Value, ErrorObject> {
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

pub(crate) fn replay_snapshot_mode(deep: bool) -> ReplaySnapshotMode {
    if deep {
        ReplaySnapshotMode::Refresh
    } else {
        ReplaySnapshotMode::Cached
    }
}

pub(crate) fn status_payload(
    index_dir: Option<&Path>,
    replay_snapshot_mode: ReplaySnapshotMode,
) -> Value {
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

pub(crate) fn doctor_check(name: &str, status: &str, detail: Value) -> Value {
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
    checks.push(doctor_check(
        "embedding_endpoint",
        endpoint_status,
        endpoint,
    ));

    // 3. Model cache present when an in-process model is required.
    let model_cache = model_cache_status(&loaded.config);
    if !model_cache.required {
        checks.push(doctor_check(
            "model_cache",
            "not_required",
            json!("in-process model not required"),
        ));
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
            checks.push(doctor_check(
                "pg_config",
                "pass",
                json!(pg_config.version.trim()),
            ));
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
        Some(path) if path.exists() => checks.push(doctor_check(
            "index_dir",
            "pass",
            json!(path.display().to_string()),
        )),
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
    let stats: Value = parse_storage_json(&response)?;
    Ok(json!({ "schema_version": SCHEMA_VERSION, "stats": stats }))
}

pub(crate) fn inspect_payload(req: InspectRequest) -> Result<Value, ErrorObject> {
    // Boundary validation shared by the one-shot and session paths.
    if req.id.trim().is_empty() {
        return Err(ErrorObject::bad_input("inspect requires a document id"));
    }
    let postgres = open_query_index(req.index_dir.as_deref(), QueryReadinessGate::Fetch)?;
    let response = inspect_document_json(&postgres, &req.id).map_err(storage_error_object)?;
    let value: Value = parse_storage_json(&response)?;
    if value["document"].is_null() {
        return Err(no_results(format!("no document with id `{}`", req.id)));
    }
    Ok(value)
}

pub(crate) fn versions_payload(req: VersionsRequest) -> Result<Value, ErrorObject> {
    // Boundary validation shared by the one-shot and session paths.
    if req.id.trim().is_empty() {
        return Err(ErrorObject::bad_input("versions requires a document id"));
    }
    let postgres = open_query_index(req.index_dir.as_deref(), QueryReadinessGate::Fetch)?;
    let response = document_versions_json(&postgres, &req.id).map_err(storage_error_object)?;
    let value: Value = parse_storage_json(&response)?;
    // An empty family means the id is unknown (the target is always its own family member).
    if value["count"].as_u64() == Some(0) {
        return Err(no_results(format!(
            "no document/version family for id `{}`",
            req.id
        )));
    }
    Ok(value)
}

pub(crate) fn diff_payload(req: DiffRequest) -> Result<Value, ErrorObject> {
    if req.id.trim().is_empty() {
        return Err(ErrorObject::bad_input("diff requires a document id"));
    }
    if !is_valid_iso_date(&req.from) || !is_valid_iso_date(&req.to) {
        return Err(ErrorObject::bad_input(
            "diff --from and --to must be YYYY-MM-DD dates",
        ));
    }
    let postgres = open_query_index(req.index_dir.as_deref(), QueryReadinessGate::Fetch)?;
    let response =
        document_diff_json(&postgres, &req.id, &req.from, &req.to).map_err(storage_error_object)?;
    let mut value: Value = parse_storage_json(&response)?;
    if value["family_count"].as_u64() == Some(0) {
        return Err(no_results(format!(
            "no document/version family for id `{}`",
            req.id
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

pub(crate) fn coverage_value_complete(coverage: &Value) -> bool {
    let covered = coverage["covered"].as_i64();
    let total = coverage["total"].as_i64();
    matches!((covered, total), (Some(covered), Some(total)) if total > 0 && covered == total)
}

/// The `status.zone_retrieval` block (T5.1): the cheap overlay coverage report joined with the
/// resolver-reachable denominator, so the reported numbers are honest fractions of what the backfill
/// can ever reach — never inflating the corpus claim. Each half degrades to `null` independently so a
/// failure in one (e.g. the denominator scan) never blanks the whole block or breaks `status`.
pub(crate) fn zone_retrieval_status_block(postgres: &ManagedPostgres) -> Value {
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

pub(crate) fn status_index_and_ingest_health(
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

pub(crate) fn ingest_health_payload(report: IngestHealthReport) -> Value {
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

pub(crate) fn pending_ingest_health() -> Value {
    json!({
        "state": "pending",
        "latest_completed_run": null,
        "projection_coverage": null,
        "embedding_coverage": null,
        "recovery_warnings": []
    })
}
