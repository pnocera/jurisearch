//! Phase 1 release gate: external-benchmark + France-LEGI floors/claims and status derivation.

use crate::*;

pub(crate) fn phase1_gate_payload(index: &Value, ingest_health: &Value) -> Value {
    let external_benchmark = phase1_external_benchmark_payload();
    let france_legi = phase1_france_legi_payload();
    phase1_gate_payload_with(index, ingest_health, external_benchmark, france_legi)
}

// Pure gate builder: takes the already-resolved benchmark payloads so tests do not depend on the
// `JURISEARCH_PHASE1_*_BENCHMARK` ambient env vars. The public `phase1_gate_payload` resolves
// those from the environment and delegates here.
pub(crate) fn phase1_gate_payload_with(
    index: &Value,
    ingest_health: &Value,
    external_benchmark: Value,
    france_legi: Value,
) -> Value {
    let eval_summary = phase1_eval_fixture_summary();
    let ingest_available = ingest_health["state"] == "available";
    let query_ready = index["query_ready"].as_bool().unwrap_or(false);
    let locked_embedding_model = phase1_embedding_model_locked(ingest_health);
    let reranker_decision = phase1_reranker_decision_payload();
    let external_benchmark_status = phase1_external_benchmark_check_status(&external_benchmark);
    let france_legi_status = phase1_france_legi_check_status(&france_legi);
    let replay_snapshot_status = ingest_health["replay_snapshot_status"]
        .as_str()
        .unwrap_or("unknown");
    let replay_snapshot_source = ingest_health["replay_snapshot_source"]
        .as_str()
        .unwrap_or("unknown");
    let replay_snapshot_message = format!(
        "replay snapshot signatures over canonical projections must be available; status={replay_snapshot_status}, source={replay_snapshot_source}"
    );

    let checks = vec![
        phase1_gate_check(
            "index_query_ready",
            if query_ready { "pass" } else { "pending" },
            if query_ready {
                "index reports query_ready=true"
            } else {
                "index is not query-ready; inspect ingest health and coverage gates"
            },
        ),
        phase1_gate_check(
            "latest_completed_ingest_run",
            if ingest_available && ingest_health["latest_completed_run"].is_string() {
                "pass"
            } else {
                "pending"
            },
            "a completed official-source ingest run is required before a Phase 1 claim",
        ),
        phase1_gate_check(
            "failed_members",
            if ingest_available && ingest_health["failed_members"].as_i64() == Some(0) {
                "pass"
            } else if ingest_available {
                "fail"
            } else {
                "pending"
            },
            "failed ingest members must be zero for the Phase 1 release gate",
        ),
        phase1_gate_check(
            "projection_coverage",
            coverage_value_complete(&ingest_health["projection_coverage"]),
            "projection coverage must be complete and non-empty",
        ),
        phase1_gate_check(
            "embedding_coverage",
            coverage_value_complete(&ingest_health["embedding_coverage"]),
            "embedding coverage must be complete and non-empty for the selected fingerprint",
        ),
        phase1_gate_check(
            "replay_snapshot",
            if ingest_available && ingest_health["replay_snapshot_status"] == "available" {
                "pass"
            } else {
                "pending"
            },
            replay_snapshot_message,
        ),
        phase1_gate_check_advisory(
            "external_expert_annotated_eval",
            external_benchmark_status,
            "Advisory cross-lingual robustness signal (BSARD, Belgian statutory). Not a Phase 1 release gate: jurisdiction-correct release evidence is `france_legi_official_eval`",
        ),
        phase1_gate_check(
            "france_legi_official_eval",
            france_legi_status,
            "Phase 1 requires a passing France-LEGI official-evidence benchmark — gating on intent-routed structured citation resolution and temporal version pinning, with full-body semantic retrieval advisory — run through the production pipeline; jurisdiction-correct release evidence, unlike the Belgian BSARD proxy",
        ),
        phase1_gate_check(
            "final_embedding_model",
            if locked_embedding_model {
                "pass"
            } else {
                "fail"
            },
            if locked_embedding_model {
                "stored embedding manifest matches the locked D21 bge-m3 v1 model"
            } else {
                "stored embedding manifest must match D21: bge-m3, 1024 dimensions, normalized embeddings"
            },
        ),
        phase1_gate_check(
            "reranker_decision",
            "pass",
            "reranker adoption is deferred for Phase 1; disabled provider remains the default until legal eval proves a material rerank gain",
        ),
    ];
    // Advisory checks (`gating: false`) are reported but do not block the claim.
    let claim_allowed = checks
        .iter()
        .filter(|check| check["gating"].as_bool() != Some(false))
        .all(|check| check["status"].as_str() == Some("pass"));

    json!({
        "state": if claim_allowed { "ready" } else { "not_ready" },
        "claim_allowed": claim_allowed,
        "scope": "phase1_legi_statutory_search",
        "checks": checks,
        "eval_fixtures": eval_summary,
        "external_benchmark": external_benchmark,
        "france_legi_benchmark": france_legi,
        "reranker_decision": reranker_decision,
    })
}

pub(crate) fn phase1_external_benchmark_payload() -> Value {
    let artifact_path = std::env::var_os(PHASE1_EXTERNAL_BENCHMARK_ENV).map(PathBuf::from);
    phase1_external_benchmark_payload_with_path(artifact_path.as_deref())
}

pub(crate) fn phase1_external_benchmark_payload_with_path(artifact_path: Option<&Path>) -> Value {
    let mut payload = phase1_external_benchmark_default_payload();
    let Some(artifact_path) = artifact_path else {
        return payload;
    };

    payload["artifact_path"] = json!(artifact_path.to_string_lossy());
    payload["source"] = json!(PHASE1_EXTERNAL_BENCHMARK_ENV);
    let contents = match fs::read_to_string(artifact_path) {
        Ok(contents) => contents,
        Err(error) => {
            payload["state"] = json!("failed");
            payload["artifact_error"] = json!(format!(
                "failed to read external benchmark artifact `{}`: {error}",
                artifact_path.display()
            ));
            return payload;
        }
    };
    let artifact = match serde_json::from_str::<Value>(&contents) {
        Ok(artifact) => artifact,
        Err(error) => {
            payload["state"] = json!("failed");
            payload["artifact_error"] = json!(format!(
                "failed to parse external benchmark artifact `{}` as JSON: {error}",
                artifact_path.display()
            ));
            return payload;
        }
    };

    payload["artifact"] = artifact.clone();
    payload["evidence"] = artifact["evidence"]
        .as_array()
        .map(|_| artifact["evidence"].clone())
        .unwrap_or_else(|| json!([]));
    payload["metrics"] = artifact["metrics"].clone();
    payload["thresholds"] = artifact["thresholds"].clone();
    payload["dataset"] = artifact["dataset"].clone();
    payload["artifact_error"] = Value::Null;

    let validation_errors = phase1_external_benchmark_artifact_errors(&artifact);
    if validation_errors.is_empty() {
        payload["state"] = json!(artifact["state"].as_str().unwrap_or("pending"));
    } else {
        payload["state"] = json!("failed");
        payload["artifact_error"] = json!(validation_errors.join("; "));
    }

    payload
}

pub(crate) fn phase1_external_benchmark_default_payload() -> Value {
    json!({
        "state": "pending",
        "source": "not_configured",
        "artifact_path": null,
        "artifact_error": null,
        "decision_date": "2026-06-22",
        "primary_candidate": "maastrichtlawtech/bsard",
        "claim_scope": "external expert-annotated French-language statutory retrieval benchmark, not France-LEGI human-reviewed gold",
        "jurisdiction": "belgium",
        "usage_scope": "eval_only",
        "required_evidence": [
            "dataset access and license recorded",
            "dataset corpus/questions/qrels imported or adapted without training leakage; the runner may be an external Python harness",
            "bm25, dense, and hybrid retrieval metrics recorded with top-k, recall, and nDCG",
            "metrics artifact path recorded for status to consume before this gate can pass",
            "Phase 1 adoption threshold documented before claim_allowed can become true"
        ],
        "dataset": null,
        "metrics": null,
        "thresholds": null,
        "artifact": null,
        "evidence": [],
        "candidate_datasets": [
            {
                "id": "maastrichtlawtech/bsard",
                "role": "primary",
                "task": "French statutory article retrieval",
                "labels": "experienced jurists",
                "license": "cc-by-nc-sa-4.0",
                "limitation": "Belgian law, not French LEGI; still French-native statutory retrieval with expert qrels"
            },
            {
                "id": "maastrichtlawtech/lleqa",
                "role": "secondary",
                "task": "French legal QA and retrieval",
                "labels": "seasoned legal professionals",
                "license": "cc-by-nc-sa-4.0 gated research access",
                "limitation": "Belgian law and gated access; useful if access is granted"
            },
            {
                "id": "mteb-private/FrenchLegal1Retrieval-sample",
                "role": "supplemental",
                "task": "French legal retrieval",
                "labels": "sample is public; full task access unclear",
                "license": "private/sample",
                "limitation": "sample-only public dataset cannot be the sole release gate"
            },
            {
                "id": "louisbrulenaudet/tax-retrieval-benchmark",
                "role": "supplemental",
                "task": "French tax retrieval",
                "labels": "domain-specific benchmark labels",
                "license": "gated",
                "limitation": "tax-only scope and gated access"
            }
        ],
        "non_gating_inputs": [
            {
                "id": "internal_legi_release_candidates",
                "reason": "source-checked against DILA LEGI but not independently expert-annotated; remains smoke/regression coverage"
            },
            {
                "id": "AgentPublic/legi",
                "reason": "useful LEGI corpus context but no expert retrieval qrels"
            }
        ],
        "reason": "local human legal-domain review is unavailable, so Phase 1 promotion must rely on a passing external expert-annotated legal retrieval benchmark plus internal LEGI smoke evidence"
    })
}

pub(crate) fn phase1_external_benchmark_artifact_errors(artifact: &Value) -> Vec<String> {
    let mut errors = Vec::new();
    let state = artifact["state"].as_str();
    match state {
        Some("pending" | "passed" | "failed") => {}
        Some(other) => errors.push(format!("invalid state `{other}`")),
        None => errors.push("missing state".to_owned()),
    }
    if artifact["kind"].as_str() != Some("phase1_external_expert_benchmark") {
        errors.push("kind must be `phase1_external_expert_benchmark`".to_owned());
    }
    if artifact["schema_version"].as_u64() != Some(1) {
        errors.push("schema_version must be 1".to_owned());
    }
    if state == Some("passed")
        && !artifact["evidence"]
            .as_array()
            .is_some_and(|evidence| !evidence.is_empty())
    {
        errors.push("passed artifact must include non-empty evidence".to_owned());
    }
    for (path, expected) in [
        ("dataset.id", "maastrichtlawtech/bsard"),
        ("dataset.question_split", "test"),
        ("dataset.jurisdiction", "belgium"),
        ("dataset.usage_scope", "eval_only"),
        ("dataset.license", "cc-by-nc-sa-4.0"),
        ("embedding.fingerprint_model", PHASE0_EMBEDDING_MODEL),
    ] {
        if artifact_pointer_str(artifact, path) != Some(expected) {
            errors.push(format!("{path} must be `{expected}`"));
        }
    }
    if artifact_pointer_value(artifact, "embedding.dimension").and_then(Value::as_u64)
        != Some(PHASE0_EMBEDDING_DIMENSION as u64)
    {
        errors.push(format!(
            "embedding.dimension must be {}",
            PHASE0_EMBEDDING_DIMENSION
        ));
    }
    if artifact_pointer_value(artifact, "embedding.normalize").and_then(Value::as_bool)
        != Some(true)
    {
        errors.push("embedding.normalize must be true".to_owned());
    }
    for path in ["dataset.revision", "claim_scope", "applicability"] {
        if artifact_pointer_str(artifact, path).is_none_or(|value| value.trim().is_empty()) {
            errors.push(format!("{path} is required"));
        }
    }
    if artifact_pointer_str(artifact, "dataset.revision") == Some("unknown") {
        errors.push("dataset.revision must be pinned, not `unknown`".to_owned());
    }
    for path in ["thresholds", "metrics"] {
        if artifact_pointer_value(artifact, path).is_none_or(Value::is_null) {
            errors.push(format!("{path} is required"));
        }
    }
    for path in ["dataset.limit_corpus", "dataset.limit_questions"] {
        if artifact_pointer_value(artifact, path).is_some_and(|value| !value.is_null()) {
            errors.push(format!("{path} must be null for a gate artifact"));
        }
    }
    if artifact_pointer_value(artifact, "dataset.corpus_documents")
        .and_then(Value::as_u64)
        .is_none_or(|count| count < PHASE1_EXTERNAL_MIN_BSARD_DOCUMENTS)
    {
        errors.push(format!(
            "dataset.corpus_documents must be at least {}",
            PHASE1_EXTERNAL_MIN_BSARD_DOCUMENTS
        ));
    }
    if artifact_pointer_value(artifact, "dataset.questions")
        .and_then(Value::as_u64)
        .is_none_or(|count| count < PHASE1_EXTERNAL_MIN_BSARD_QUESTIONS)
    {
        errors.push(format!(
            "dataset.questions must be at least {}",
            PHASE1_EXTERNAL_MIN_BSARD_QUESTIONS
        ));
    }
    phase1_validate_external_benchmark_metric(
        artifact,
        "recall_at_20",
        PHASE1_EXTERNAL_MIN_HYBRID_RECALL_AT_20,
        &mut errors,
    );
    phase1_validate_external_benchmark_metric(
        artifact,
        "ndcg_at_20",
        PHASE1_EXTERNAL_MIN_HYBRID_NDCG_AT_20,
        &mut errors,
    );
    phase1_validate_external_benchmark_metric(
        artifact,
        "mrr_at_20",
        PHASE1_EXTERNAL_MIN_HYBRID_MRR_AT_20,
        &mut errors,
    );
    errors
}

pub(crate) fn phase1_validate_external_benchmark_metric(
    artifact: &Value,
    metric_name: &str,
    policy_floor: f64,
    errors: &mut Vec<String>,
) {
    let threshold_path = format!("thresholds.hybrid_{metric_name}_min");
    let metric_path = format!("metrics.hybrid.{metric_name}");
    let threshold = artifact_pointer_f64(artifact, &threshold_path);
    let metric = artifact_pointer_f64(artifact, &metric_path);
    match threshold {
        Some(threshold) if threshold >= policy_floor => {}
        Some(threshold) => errors.push(format!(
            "{threshold_path} must be at least {policy_floor:.3}, got {threshold:.3}"
        )),
        None => errors.push(format!("{threshold_path} is required")),
    }
    if let (Some(metric), Some(threshold)) = (metric, threshold) {
        if metric < threshold {
            errors.push(format!(
                "{metric_path} must be at least threshold {threshold:.3}, got {metric:.3}"
            ));
        }
    } else if metric.is_none() {
        errors.push(format!("{metric_path} is required"));
    }
}

pub(crate) fn phase1_external_benchmark_check_status(external_benchmark: &Value) -> &'static str {
    match external_benchmark["state"].as_str() {
        Some("passed")
            if external_benchmark["evidence"]
                .as_array()
                .is_some_and(|evidence| !evidence.is_empty()) =>
        {
            "pass"
        }
        Some("passed" | "failed") => "fail",
        _ => "pending",
    }
}

pub(crate) fn phase1_france_legi_payload() -> Value {
    let artifact_path = std::env::var_os(PHASE1_FRANCE_LEGI_BENCHMARK_ENV).map(PathBuf::from);
    phase1_france_legi_payload_with_path(artifact_path.as_deref())
}

pub(crate) fn phase1_france_legi_payload_with_path(artifact_path: Option<&Path>) -> Value {
    let mut payload = phase1_france_legi_default_payload();
    let Some(artifact_path) = artifact_path else {
        return payload;
    };

    payload["artifact_path"] = json!(artifact_path.to_string_lossy());
    payload["source"] = json!(PHASE1_FRANCE_LEGI_BENCHMARK_ENV);
    let contents = match fs::read_to_string(artifact_path) {
        Ok(contents) => contents,
        Err(error) => {
            payload["state"] = json!("failed");
            payload["artifact_error"] = json!(format!(
                "failed to read France-LEGI benchmark artifact `{}`: {error}",
                artifact_path.display()
            ));
            return payload;
        }
    };
    let artifact = match serde_json::from_str::<Value>(&contents) {
        Ok(artifact) => artifact,
        Err(error) => {
            payload["state"] = json!("failed");
            payload["artifact_error"] = json!(format!(
                "failed to parse France-LEGI benchmark artifact `{}` as JSON: {error}",
                artifact_path.display()
            ));
            return payload;
        }
    };

    payload["artifact"] = artifact.clone();
    payload["evidence"] = artifact["evidence"]
        .as_array()
        .map(|_| artifact["evidence"].clone())
        .unwrap_or_else(|| json!([]));
    payload["categories"] = artifact["categories"].clone();
    payload["thresholds"] = artifact["thresholds"].clone();
    payload["provenance"] = artifact["provenance"].clone();
    payload["artifact_error"] = Value::Null;

    let validation_errors = phase1_france_legi_artifact_errors(&artifact);
    if validation_errors.is_empty() {
        payload["state"] = json!(artifact["state"].as_str().unwrap_or("pending"));
    } else {
        payload["state"] = json!("failed");
        payload["artifact_error"] = json!(validation_errors.join("; "));
    }

    payload
}

pub(crate) fn phase1_france_legi_default_payload() -> Value {
    json!({
        "state": "pending",
        "source": "not_configured",
        "artifact_path": null,
        "artifact_error": null,
        "decision_date": "2026-06-22",
        "claim_scope": "France-LEGI official-evidence retrieval with intent routing: structured citation resolution and temporal version pinning (gating), plus advisory full-body semantic retrieval, through the production pipeline",
        "jurisdiction": "france",
        "retriever": "production jurisearch search (BM25 + dense + RRF)",
        "required_evidence": [
            "gold derived only from official DILA/Légifrance fields (no human, no LLM): ID/NUM/TITRE_TXT for structured citation resolution, CID/DATE_DEBUT/DATE_FIN for temporal version pinning, LIEN CITATION targets for advisory semantic retrieval",
            "retrieval executed through the production search pipeline, not a proxy harness",
            "per-category metrics recorded with query counts and the locked bge-m3 fingerprint",
            "per-category thresholds at or above policy floors recorded for status to consume before this gate can pass",
            "structured provenance: pinned official_source + source_revision, production pipeline + code_version + index_revision, and sampled=false / human_in_gold=false / llm_in_gold=false"
        ],
        "categories": null,
        "thresholds": null,
        "provenance": null,
        "artifact": null,
        "evidence": [],
        "reason": "BSARD is a Belgian proxy; a jurisdiction-correct France-LEGI official-evidence benchmark over the production pipeline is the release-gating signal. Gold is structurally derived from official Légifrance fields, so it needs no human annotation. See work/03-implementation/02-evidence/2026-06-22-france-legi-official-evidence-benchmark-feasibility.md"
    })
}

pub(crate) fn phase1_france_legi_artifact_errors(artifact: &Value) -> Vec<String> {
    let mut errors = Vec::new();
    match artifact["state"].as_str() {
        Some("pending" | "passed" | "failed") => {}
        Some(other) => errors.push(format!("invalid state `{other}`")),
        None => errors.push("missing state".to_owned()),
    }
    if artifact["kind"].as_str() != Some("phase1_france_legi_benchmark") {
        errors.push("kind must be `phase1_france_legi_benchmark`".to_owned());
    }
    if artifact["schema_version"].as_u64() != Some(1) {
        errors.push("schema_version must be 1".to_owned());
    }
    if artifact["jurisdiction"].as_str() != Some("france") {
        errors.push("jurisdiction must be `france`".to_owned());
    }
    if artifact["state"].as_str() == Some("passed")
        && !artifact["evidence"]
            .as_array()
            .is_some_and(|evidence| !evidence.is_empty())
    {
        errors.push("passed artifact must include non-empty evidence".to_owned());
    }
    if artifact_pointer_str(artifact, "embedding.fingerprint_model") != Some(PHASE0_EMBEDDING_MODEL)
    {
        errors.push(format!(
            "embedding.fingerprint_model must be `{PHASE0_EMBEDDING_MODEL}`"
        ));
    }
    if artifact_pointer_value(artifact, "embedding.dimension").and_then(Value::as_u64)
        != Some(PHASE0_EMBEDDING_DIMENSION as u64)
    {
        errors.push(format!(
            "embedding.dimension must be {}",
            PHASE0_EMBEDDING_DIMENSION
        ));
    }
    if artifact_pointer_value(artifact, "embedding.normalize").and_then(Value::as_bool)
        != Some(true)
    {
        errors.push("embedding.normalize must be true".to_owned());
    }
    for path in ["claim_scope", "source", "retriever"] {
        if artifact_pointer_str(artifact, path).is_none_or(|value| value.trim().is_empty()) {
            errors.push(format!("{path} is required"));
        }
    }
    // Structured provenance: the gate must not accept a proxy runner that only supplies
    // good-looking category metrics. Require pinned official-source + production-pipeline
    // identity, and assert the gold is structurally derived (no human, no LLM) over a full,
    // unsampled qrel set.
    for path in [
        "provenance.official_source",
        "provenance.source_revision",
        "provenance.pipeline",
        "provenance.code_version",
        "provenance.index_revision",
    ] {
        if artifact_pointer_str(artifact, path).is_none_or(|value| value.trim().is_empty()) {
            errors.push(format!("{path} is required"));
        }
    }
    if artifact_pointer_str(artifact, "provenance.source_revision")
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("unknown"))
    {
        errors.push("provenance.source_revision must be pinned, not `unknown`".to_owned());
    }
    for (path, message) in [
        (
            "provenance.sampled",
            "provenance.sampled must be false (qrels must be deterministic, not randomly sampled or cherry-picked; a reproducible bounded set recorded under provenance.qrel_limits is acceptable)",
        ),
        (
            "provenance.human_in_gold",
            "provenance.human_in_gold must be false (France-LEGI gold is structurally derived from official fields)",
        ),
        (
            "provenance.llm_in_gold",
            "provenance.llm_in_gold must be false (France-LEGI gold is structurally derived from official fields)",
        ),
    ] {
        if artifact_pointer_value(artifact, path).and_then(Value::as_bool) != Some(false) {
            errors.push(message.to_owned());
        }
    }
    for path in ["categories", "thresholds"] {
        if artifact_pointer_value(artifact, path).is_none_or(Value::is_null) {
            errors.push(format!("{path} is required"));
        }
    }
    // Two structured categories GATE the claim at high floors; semantic_retrieval is advisory.
    phase1_france_legi_validate_category(
        artifact,
        "structured_citation_resolution",
        "structured_citation_recall_at_10",
        PHASE1_FRANCE_LEGI_MIN_STRUCTURED_CITATION_RECALL_AT_10,
        PHASE1_FRANCE_LEGI_MIN_STRUCTURED_CITATION_QUERIES,
        false,
        &mut errors,
    );
    phase1_france_legi_validate_category(
        artifact,
        "temporal_version_pinning",
        "temporal_version_exactness_at_10",
        PHASE1_FRANCE_LEGI_MIN_TEMPORAL_VERSION_EXACTNESS_AT_10,
        PHASE1_FRANCE_LEGI_MIN_TEMPORAL_QUERIES,
        false,
        &mut errors,
    );
    phase1_france_legi_validate_category(
        artifact,
        "semantic_retrieval",
        "semantic_retrieval_recall_at_10",
        PHASE1_FRANCE_LEGI_ADVISORY_SEMANTIC_RECALL_AT_10,
        PHASE1_FRANCE_LEGI_MIN_SEMANTIC_QUERIES,
        true,
        &mut errors,
    );
    errors
}

pub(crate) fn phase1_france_legi_validate_category(
    artifact: &Value,
    category: &str,
    threshold_key: &str,
    policy_floor: f64,
    min_queries: u64,
    // Gating categories must clear their recorded threshold; advisory categories record their
    // metric but never fail the gate on it (they still require the metric + a minimum query count).
    advisory: bool,
    errors: &mut Vec<String>,
) {
    let suffix = if advisory { "advisory" } else { "min" };
    let threshold_path = format!("thresholds.{threshold_key}_{suffix}");
    let value_path = format!("categories.{category}.metric_value");
    let queries_path = format!("categories.{category}.queries");
    let threshold = artifact_pointer_f64(artifact, &threshold_path);
    let value = artifact_pointer_f64(artifact, &value_path);
    match threshold {
        Some(threshold) if threshold >= policy_floor => {}
        Some(threshold) => errors.push(format!(
            "{threshold_path} must be at least {policy_floor:.3}, got {threshold:.3}"
        )),
        None => errors.push(format!("{threshold_path} is required")),
    }
    if advisory {
        if value.is_none() {
            errors.push(format!("{value_path} is required"));
        }
    } else if let (Some(value), Some(threshold)) = (value, threshold) {
        if value < threshold {
            errors.push(format!(
                "{value_path} must be at least threshold {threshold:.3}, got {value:.3}"
            ));
        }
    } else if value.is_none() {
        errors.push(format!("{value_path} is required"));
    }
    if artifact_pointer_value(artifact, &queries_path)
        .and_then(Value::as_u64)
        .is_none_or(|count| count < min_queries)
    {
        errors.push(format!("{queries_path} must be at least {min_queries}"));
    }
    // Routing-backend audit: the per-query backend accounting must cover EVERY query, and a GATING
    // category must have been resolved entirely by the structured citation resolver. This is the
    // proof the split relies on — that the structured metrics came from input-driven structured
    // resolution, not an answer-aware or fuzzy harness reporting high numbers.
    let backends_path = format!("categories.{category}.routing_backends");
    let queries = artifact_pointer_value(artifact, &queries_path).and_then(Value::as_u64);
    match artifact_pointer_value(artifact, &backends_path).and_then(Value::as_object) {
        Some(backends) => {
            if let Some(queries) = queries {
                let total: u64 = backends.values().filter_map(Value::as_u64).sum();
                if total != queries {
                    errors.push(format!(
                        "{backends_path} must account for all {queries} queries (counted {total})"
                    ));
                }
                if !advisory {
                    let structured = backends
                        .get("structured_citation")
                        .and_then(Value::as_u64)
                        .unwrap_or(0);
                    if structured != queries {
                        errors.push(format!(
                            "{backends_path}.structured_citation must equal queries ({queries}) for a gating category: every query must resolve via the structured citation resolver (got {structured})"
                        ));
                    }
                }
            }
        }
        None => errors.push(format!("{backends_path} is required")),
    }
}

pub(crate) fn phase1_france_legi_check_status(france_legi: &Value) -> &'static str {
    match france_legi["state"].as_str() {
        Some("passed")
            if france_legi["evidence"]
                .as_array()
                .is_some_and(|evidence| !evidence.is_empty()) =>
        {
            "pass"
        }
        Some("passed" | "failed") => "fail",
        _ => "pending",
    }
}

pub(crate) fn phase1_reranker_decision_payload() -> Value {
    // TODO(phase1-reranker): when the reranker provider seam lands, derive this
    // from runtime config/manifests instead of the Phase 1 static deferral.
    json!({
        "state": "deferred",
        "provider": "disabled",
        "adopted": false,
        "decision_date": "2026-06-22",
        "model_candidate": "BAAI/bge-reranker-v2-m3",
        "evidence": [
            "work/03-implementation/02-evidence/2026-06-21-reranker-feasibility.md",
            "work/03-implementation/02-evidence/2026-06-22-phase1-eval-benchmark-summary.md",
            "work/03-implementation/02-evidence/2026-06-22-reranker-deferral-decision.md"
        ],
        "reason": "current Phase 1 release-candidate fixtures cannot measure a material rerank gain, no reranker provider is packaged, and cross-encoder latency/packaging remain unmeasured",
        "future_adoption_gate": "hybrid+rerank must show material legal-retrieval quality gain on the external expert benchmark or future project-owned release-gating fixtures, with measured latency and graceful fallback to hybrid order"
    })
}

pub(crate) fn phase1_embedding_model_locked(ingest_health: &Value) -> bool {
    const LOCKED_PHASE1_EMBEDDING_FINGERPRINT: &str = "bge-m3:1024:normalize:true";
    let manifest = &ingest_health["embedding_manifest"];
    manifest["embedding_fingerprint"].as_str() == Some(LOCKED_PHASE1_EMBEDDING_FINGERPRINT)
        && manifest["model"].as_str() == Some(PHASE0_EMBEDDING_MODEL)
        && manifest["dimension"].as_u64() == Some(PHASE0_EMBEDDING_DIMENSION as u64)
        && manifest["normalize"].as_bool() == Some(true)
}

pub(crate) fn phase1_gate_check(
    name: &str,
    status: impl Into<Phase1GateStatus>,
    message: impl Into<String>,
) -> Value {
    let status = status.into().as_str();
    let message = message.into();
    json!({
        "name": name,
        "status": status,
        "message": message,
        "gating": true
    })
}

// An advisory check is reported in `checks[]` but does NOT block `claim_allowed`.
pub(crate) fn phase1_gate_check_advisory(
    name: &str,
    status: impl Into<Phase1GateStatus>,
    message: impl Into<String>,
) -> Value {
    let status = status.into().as_str();
    let message = message.into();
    json!({
        "name": name,
        "status": status,
        "message": message,
        "gating": false
    })
}

pub(crate) enum Phase1GateStatus {
    Static(&'static str),
    Boolean(bool),
}

impl Phase1GateStatus {
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Static(status) => status,
            Self::Boolean(true) => "pass",
            Self::Boolean(false) => "pending",
        }
    }
}

impl From<&'static str> for Phase1GateStatus {
    fn from(value: &'static str) -> Self {
        Self::Static(value)
    }
}

impl From<bool> for Phase1GateStatus {
    fn from(value: bool) -> Self {
        Self::Boolean(value)
    }
}
