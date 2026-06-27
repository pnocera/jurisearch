//! cite command: the thin CLI adapter over `jurisearch_query::build_cite`. Boundary validation, the
//! conditional snapshot citation lookup (a MALFORMED citation never opens an index — only a resolvable
//! target does), the optional online Légifrance probe (a network side effect, CLI-only), and the strict
//! gate live here; citation-state classification and response construction live in `jurisearch-query`.

use jurisearch_query::{CiteInput, ParsedCitationTarget, build_cite, enforce_strict_citation};
use jurisearch_storage::citation::citation_lookup_in_snapshot;
use jurisearch_storage::query::QueryStore;

use crate::*;

pub(crate) fn emit_cite(req: CiteRequest) -> anyhow::Result<()> {
    match cite_payload(req) {
        Ok(response) => write_json(&response),
        Err(error) => emit_error(error),
    }
}

pub(crate) fn cite_payload(req: CiteRequest) -> Result<Value, ErrorObject> {
    // Boundary validation shared by the one-shot and session paths.
    if req.cite.trim().is_empty() {
        return Err(ErrorObject::bad_input("cite requires a non-empty citation"));
    }
    validate_as_of(req.as_of.as_deref())?;
    let parsed = parse_citation_target(&req.cite);
    let effective_as_of = req.as_of.clone().unwrap_or_else(today_utc);
    // Only a resolvable citation touches the index: a malformed citation is classified locally without
    // opening an index or running the readiness gate (preserved from the pre-4B behaviour). The lookup
    // is the one snapshot-bound read; everything after is pure response construction.
    let mut lookup = json!({ "matches": [] });
    if let Some(lookup_target) = parsed.lookup() {
        let postgres = open_query_index(req.index_dir.as_deref(), QueryReadinessGate::Fetch)?;
        let mut snapshot = postgres.begin_snapshot().map_err(storage_error_object)?;
        let response = citation_lookup_in_snapshot(
            &mut *snapshot,
            &CitationLookupQuery {
                lookup: lookup_target,
                limit: 25,
            },
        )
        .map_err(storage_error_object)?;
        lookup = parse_storage_json(&response)?;
    }

    let input = CiteInput {
        cite: req.cite.clone(),
        parsed,
        effective_as_of,
        requested_as_of: req.as_of.clone(),
        strict: req.strict,
        online_requested: req.online,
    };
    let mut response = build_cite(&input, &lookup);
    // The actual online Légifrance probe is a network side effect (CLI-only). build_cite already wrote
    // the malformed/decision online notes, so only the resolvable-statute online case runs the probe.
    if req.online
        && !input.parsed.is_decision()
        && !matches!(&input.parsed, ParsedCitationTarget::Malformed { .. })
    {
        apply_online_citation_confirmation(&mut response, &req.cite)?;
    }
    // Strict gate runs AFTER the optional online probe (which never changes `state`), preserving the
    // prior order (an online error surfaces before a strict error).
    enforce_strict_citation(&response, &req.cite, req.strict)?;
    Ok(response)
}

pub(crate) fn apply_online_citation_confirmation(
    response: &mut Value,
    query: &str,
) -> Result<(), ErrorObject> {
    let mut client = PisteClient::new(OfficialApiConfig::from_env());
    // Share the real-contract body builder with the enrichment path. The old inline `{query,pageSize}`
    // shape was live-validated to return HTTP 500 from the Legifrance engine, so cite --online would have
    // surfaced an online-check failure instead of a summary.
    let upstream = client
        .legifrance_search(&legifrance_code_search_body(query))
        .map_err(|error| error.to_error_object())?;
    response["online"] = json!({
        "requested": true,
        "checked": true,
        "provider": "legifrance",
        "state": response["state"].as_str(),
        "response_summary": summarize_online_response(&upstream),
        "note": "Online Légifrance search completed; citation state remains based on local index resolution until response-shape matching is specified."
    });
    Ok(())
}

pub(crate) fn summarize_online_response(response: &Value) -> Value {
    let top_level_keys = response
        .as_object()
        .map(|object| object.keys().take(8).cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    let result_count = response
        .get("results")
        .and_then(Value::as_array)
        .map(Vec::len)
        .or_else(|| {
            response
                .get("items")
                .and_then(Value::as_array)
                .map(Vec::len)
        });
    json!({
        "top_level_keys": top_level_keys,
        "result_count": result_count,
    })
}
