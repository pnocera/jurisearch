//! Phase 2 release gate: jurisprudence + decision-citation benchmark floors/claims and status derivation.

use crate::*;

/// The fail-closed Phase 2 gate: the "best-in-class French juridic search" claim is allowed only when
/// jurisprudence is ingested, the index is query-ready, bulk zone provenance is reported honestly, and
/// a passing jurisprudence eval benchmark (re-derived from per-category floors, not self-reported) is
/// supplied via `JURISEARCH_PHASE2_BENCHMARK`. Until then `claim_allowed=false` / `state=not_ready`.
pub(crate) fn phase2_gate_payload(
    index: &Value,
    ingest_health: &Value,
    corpus_sources: &Value,
) -> Value {
    let benchmark = phase2_benchmark_payload();
    phase2_gate_payload_with(index, ingest_health, corpus_sources, benchmark)
}

pub(crate) fn phase2_gate_payload_with(
    index: &Value,
    ingest_health: &Value,
    corpus_sources: &Value,
    benchmark: Value,
) -> Value {
    let query_ready = index["query_ready"].as_bool() == Some(true);
    let ingest_available = ingest_health["state"] == "available";

    // Which DILA bulk jurisprudence sources have a freshness-advancing completed run (status reports
    // them in corpus_sources). cass/capp/inca are judicial; jade is administrative.
    let juri_sources: Vec<&str> = ["cass", "capp", "inca", "jade"]
        .into_iter()
        .filter(|source| corpus_sources.get(source).is_some_and(Value::is_object))
        .collect();
    let judicial_present = juri_sources
        .iter()
        .any(|s| matches!(*s, "cass" | "capp" | "inca"));
    let administrative_present = juri_sources.contains(&"jade");
    let corpus_present = judicial_present && administrative_present;

    // Honest provenance: every present bulk source must report zone_accurate=false (it must never
    // claim official Judilibre zones without enrichment).
    let honest_zones = !juri_sources.is_empty()
        && juri_sources
            .iter()
            .all(|s| corpus_sources[*s]["zone_accurate"].as_bool() == Some(false));

    let benchmark_status = phase2_benchmark_check_status(&benchmark);

    let checks = vec![
        phase1_gate_check(
            "jurisprudence_corpus_present",
            corpus_present,
            "both judicial (cass/capp/inca) and administrative (jade) DILA bulk jurisprudence must have a completed ingest run",
        ),
        phase1_gate_check(
            "index_query_ready",
            if query_ready { "pass" } else { "pending" },
            "the index must be query-ready (projection + embedding coverage gates pass)",
        ),
        phase1_gate_check(
            "honest_zone_provenance",
            if honest_zones { "pass" } else { "pending" },
            "bulk jurisprudence must report zone_accurate=false; the official-zone fetch gate is met only by Judilibre zone enrichment",
        ),
        phase1_gate_check_advisory(
            "pseudonymisation_preserved",
            if ingest_available { "pass" } else { "pending" },
            "source pseudonymisation is preserved verbatim by the juri parser (unit + real-archive tests); advisory until the release benchmark asserts no re-identification",
        ),
        phase1_gate_check(
            "jurisprudence_eval_benchmark",
            benchmark_status,
            "a passing jurisprudence eval benchmark — Cassation + administrative retrieval AND decision-citation verification through the production pipeline, re-derived against policy floors — is required before the full-juridic claim",
        ),
    ];

    let claim_allowed = checks
        .iter()
        .filter(|check| check["gating"].as_bool() != Some(false))
        .all(|check| check["status"].as_str() == Some("pass"));

    json!({
        "state": if claim_allowed { "ready" } else { "not_ready" },
        "claim_allowed": claim_allowed,
        "scope": "phase2_full_french_juridic_search",
        "checks": checks,
        "jurisprudence_corpus_sources": juri_sources,
        "benchmark": benchmark
    })
}

pub(crate) fn phase2_benchmark_payload() -> Value {
    let artifact_path = std::env::var_os(PHASE2_BENCHMARK_ENV).map(PathBuf::from);
    phase2_benchmark_payload_with_path(artifact_path.as_deref())
}

pub(crate) fn phase2_benchmark_payload_with_path(artifact_path: Option<&Path>) -> Value {
    let mut payload = phase2_benchmark_default_payload();
    let Some(artifact_path) = artifact_path else {
        return payload;
    };
    payload["artifact_path"] = json!(artifact_path.to_string_lossy());
    payload["source"] = json!(PHASE2_BENCHMARK_ENV);
    let contents = match fs::read_to_string(artifact_path) {
        Ok(contents) => contents,
        Err(error) => {
            payload["state"] = json!("failed");
            payload["artifact_error"] = json!(format!(
                "failed to read Phase 2 benchmark artifact `{}`: {error}",
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
                "failed to parse Phase 2 benchmark artifact `{}` as JSON: {error}",
                artifact_path.display()
            ));
            return payload;
        }
    };
    // Normalize every diagnostic field to its schema-declared shape so the emitted payload always
    // matches the published schema, even for a parseable-but-malformed artifact (e.g. a top-level
    // `[]`/`false`, or an object whose `categories`/`provenance` are not objects).
    let object_or_null = |value: &Value| -> Value {
        if value.is_object() {
            value.clone()
        } else {
            Value::Null
        }
    };
    payload["artifact"] = object_or_null(&artifact);
    payload["categories"] = object_or_null(&artifact["categories"]);
    payload["provenance"] = object_or_null(&artifact["provenance"]);
    payload["evidence"] = artifact["evidence"]
        .as_array()
        .map_or(json!([]), |_| artifact["evidence"].clone());

    let errors = phase2_benchmark_artifact_errors(&artifact);
    // Re-derive the state from the validation, never the artifact's self-reported `state` (which is
    // preserved only as a string-or-null diagnostic). Empty errors over the full contract == passed.
    payload["artifact_reported_state"] = artifact["state"]
        .as_str()
        .map_or(Value::Null, |state| json!(state));
    if errors.is_empty() {
        payload["state"] = json!("passed");
        payload["artifact_error"] = Value::Null;
    } else {
        payload["state"] = json!("failed");
        payload["artifact_error"] = json!(errors.join("; "));
    }
    payload
}

pub(crate) fn phase2_benchmark_default_payload() -> Value {
    json!({
        "state": "pending",
        "source": "not_configured",
        "artifact_path": null,
        "artifact_error": null,
        "jurisdiction": "france",
        "fingerprint": "bge-m3:1024:normalize:true",
        "claim_scope": "full French juridic search (statutes + jurisprudence): judicial (Cassation/appeal) AND administrative retrieval AND ECLI/pourvoi/CETATEXT decision-citation verification, through the production pipeline",
        "required_evidence": [
            "judicial_retrieval AND administrative_retrieval categories, each with metric=recall_at_10 and independent query floors, run through the production search pipeline",
            "decision_citation.by_identifier with a MEASURED breakdown for each of ecli, pourvoi, cetatext (metric=decision_citation_accuracy, per-identifier queries + accuracy at/above floors)",
            "per-category metrics with query counts and the locked bge-m3 fingerprint, at or above policy floors",
            "structured provenance: pipeline='production', non-empty code_version + index_revision, sampled=false, boolean human_in_gold + llm_in_gold",
            "pseudonymisation preservation asserted (no re-identification, no cross-source linking)"
        ],
        "floors": {
            "retrieval_recall_at_10": PHASE2_MIN_RETRIEVAL_RECALL_AT_10,
            "min_judicial_retrieval_queries": PHASE2_MIN_JUDICIAL_RETRIEVAL_QUERIES,
            "min_administrative_retrieval_queries": PHASE2_MIN_ADMINISTRATIVE_RETRIEVAL_QUERIES,
            "decision_citation_accuracy": PHASE2_MIN_DECISION_CITATION_ACCURACY,
            "min_citation_queries_per_identifier": PHASE2_MIN_CITATION_QUERIES_PER_IDENTIFIER,
            "required_citation_identifiers": PHASE2_REQUIRED_CITATION_IDENTIFIERS
        },
        "categories": null,
        "provenance": null,
        "evidence": [],
        "reason": "no Phase 2 jurisprudence eval benchmark has been run yet; the full-juridic claim is fail-closed until a jurisdiction-correct passing artifact is supplied"
    })
}

/// Re-derive whether a Phase 2 benchmark artifact PASSES the full contract against the policy floors
/// (never trust a self-reported `state`). Returns the list of reasons it is NOT a valid pass (empty =
/// valid). Enforces jurisdiction, locked fingerprint, non-empty evidence, production provenance,
/// BOTH jurisprudence families' retrieval, and ECLI/pourvoi/CETATEXT citation coverage.
pub(crate) fn phase2_benchmark_artifact_errors(artifact: &Value) -> Vec<String> {
    let mut errors = Vec::new();
    if artifact["jurisdiction"].as_str() != Some("france") {
        errors.push("jurisdiction must be `france`".to_owned());
    }
    if artifact["fingerprint"].as_str() != Some("bge-m3:1024:normalize:true") {
        errors.push("fingerprint must be the locked bge-m3:1024:normalize:true".to_owned());
    }
    if !artifact["evidence"]
        .as_array()
        .is_some_and(|evidence| !evidence.is_empty())
    {
        errors.push("evidence must be a non-empty array".to_owned());
    }

    // Production provenance: the benchmark must run through the production pipeline, with pinned
    // code/index revisions, `sampled=false`, and disclosed human/LLM gold booleans.
    let provenance = &artifact["provenance"];
    if provenance["pipeline"].as_str() != Some(PHASE2_PRODUCTION_PIPELINE) {
        errors.push(format!(
            "provenance.pipeline must be `{PHASE2_PRODUCTION_PIPELINE}` (run through the production pipeline)"
        ));
    }
    for field in ["code_version", "index_revision"] {
        if !provenance[field]
            .as_str()
            .is_some_and(|value| !value.trim().is_empty())
        {
            errors.push(format!("provenance.{field} must be a non-empty string"));
        }
    }
    // Recorded as booleans (the policy does not forbid LLM-drafted/human-reviewed gold, only hidden
    // sampling): sampled must be false; human_in_gold / llm_in_gold are disclosed booleans.
    for flag in ["sampled", "human_in_gold", "llm_in_gold"] {
        if !provenance[flag].is_boolean() {
            errors.push(format!("provenance.{flag} must be a boolean"));
        }
    }
    if provenance["sampled"].as_bool() == Some(true) {
        errors.push("provenance.sampled must be false (full benchmark, not a sample)".to_owned());
    }

    // Both jurisprudence families must be retrieved, with the named metric and independent floors.
    phase2_benchmark_validate_category(
        &artifact["categories"]["judicial_retrieval"],
        "judicial_retrieval",
        "recall_at_10",
        PHASE2_MIN_RETRIEVAL_RECALL_AT_10,
        PHASE2_MIN_JUDICIAL_RETRIEVAL_QUERIES,
        &mut errors,
    );
    phase2_benchmark_validate_category(
        &artifact["categories"]["administrative_retrieval"],
        "administrative_retrieval",
        "recall_at_10",
        PHASE2_MIN_RETRIEVAL_RECALL_AT_10,
        PHASE2_MIN_ADMINISTRATIVE_RETRIEVAL_QUERIES,
        &mut errors,
    );

    // Decision-citation verification must be MEASURED per identifier kind (not just declared): each of
    // ECLI/pourvoi/CETATEXT needs its own metric, query count, and accuracy at/above the floors, so an
    // ECLI-only run cannot open the "ECLI/pourvoi/CETATEXT verification" claim.
    let decision_citation = &artifact["categories"]["decision_citation"];
    if decision_citation["metric"].as_str() != Some("decision_citation_accuracy") {
        errors.push(
            "category `decision_citation` metric must be `decision_citation_accuracy`".to_owned(),
        );
    }
    for identifier in PHASE2_REQUIRED_CITATION_IDENTIFIERS {
        phase2_benchmark_validate_category(
            &decision_citation["by_identifier"][identifier],
            &format!("decision_citation.by_identifier.{identifier}"),
            "decision_citation_accuracy",
            PHASE2_MIN_DECISION_CITATION_ACCURACY,
            PHASE2_MIN_CITATION_QUERIES_PER_IDENTIFIER,
            &mut errors,
        );
    }
    errors
}

pub(crate) fn phase2_benchmark_validate_category(
    category: &Value,
    name: &str,
    expected_metric: &str,
    floor: f64,
    min_queries: u64,
    errors: &mut Vec<String>,
) {
    if !category.is_object() {
        errors.push(format!("category `{name}` is missing"));
        return;
    }
    if category["metric"].as_str() != Some(expected_metric) {
        errors.push(format!(
            "category `{name}` metric must be `{expected_metric}`"
        ));
    }
    let Some(value) = category["value"].as_f64() else {
        errors.push(format!("category `{name}` is missing a numeric `value`"));
        return;
    };
    if value < floor {
        errors.push(format!(
            "category `{name}` value {value} is below floor {floor}"
        ));
    }
    match category["queries"].as_u64() {
        Some(queries) if queries >= min_queries => {}
        Some(queries) => errors.push(format!(
            "category `{name}` has {queries} queries, below the minimum {min_queries}"
        )),
        None => errors.push(format!("category `{name}` is missing a `queries` count")),
    }
}

pub(crate) fn phase2_benchmark_check_status(benchmark: &Value) -> &'static str {
    match benchmark["state"].as_str() {
        Some("passed") => "pass",
        Some("failed") => "fail",
        _ => "pending",
    }
}
