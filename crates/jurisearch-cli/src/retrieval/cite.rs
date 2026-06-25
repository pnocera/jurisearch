//! cite command: citation-state classification, validity, and optional online confirmation.

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
    let mut lookup = json!({ "matches": [] });
    if let Some(lookup_target) = parsed.lookup() {
        let index_dir = require_existing_index_dir(req.index_dir.as_deref())?;
        let postgres = open_index(index_dir.as_path())?;
        ensure_query_readiness(&postgres, QueryReadinessGate::Fetch)?;
        let response = citation_lookup_json(
            &postgres,
            &CitationLookupQuery {
                lookup: lookup_target,
                limit: 25,
            },
        )
        .map_err(storage_error_object)?;
        lookup = serde_json::from_str(&response)
            .map_err(|error| dependency_unavailable(error.to_string()))?;
    }

    let local_state = classify_citation_state(
        &parsed,
        &lookup,
        effective_as_of.as_str(),
        req.as_of.as_deref(),
    );
    // Decision identifiers are not corroborated against the Légifrance statutory probe, so an empty
    // decision lookup must stay `not_found` rather than being relabelled `source_unavailable`.
    let state = if req.online
        && !parsed.is_decision()
        && !matches!(&parsed, ParsedCitationTarget::Malformed { .. })
        && lookup["matches"]
            .as_array()
            .is_none_or(|matches| matches.is_empty())
    {
        CitationState::SourceUnavailable
    } else {
        local_state
    };
    let mut response = json!({
        "query": req.cite,
        "input_class": parsed.input_class(),
        "normalized": parsed.normalized_value(),
        "as_of": effective_as_of,
        "requested_as_of": req.as_of.as_deref(),
        "state": citation_state_name(state),
        "local_state": citation_state_name(local_state),
        "strict": req.strict,
        "online": {
            "requested": req.online,
            "checked": false,
            "state": null,
            "note": null
        },
        "match_count": lookup["matches"].as_array().map_or(0, Vec::len),
        "matches": lookup["matches"].clone(),
    });
    annotate_valid_matches(&mut response, &effective_as_of);
    if req.online && matches!(&parsed, ParsedCitationTarget::Malformed { .. }) {
        response["online"] = json!({
            "requested": true,
            "checked": false,
            "state": citation_state_name(CitationState::NotFound),
            "note": "Malformed citations are classified locally and are not sent to the online Légifrance probe."
        });
    } else if req.online && parsed.is_decision() {
        // The online probe targets Légifrance (statutes). Decision verification belongs to Judilibre,
        // which is not yet wired here — never send a decision identifier to the statutory probe.
        response["online"] = json!({
            "requested": true,
            "checked": false,
            "provider": "judilibre",
            "state": null,
            "note": "Online decision verification uses the Judilibre API and is not yet wired; the state above is from the local index."
        });
    } else if req.online {
        apply_online_citation_confirmation(&mut response, &req.cite)?;
    }

    if req.strict && !matches!(state, CitationState::Exact | CitationState::Normalized) {
        return Err(strict_citation_error(&req.cite, state));
    }
    Ok(response)
}

pub(crate) fn classify_citation_state(
    parsed: &ParsedCitationTarget,
    lookup: &Value,
    effective_as_of: &str,
    requested_as_of: Option<&str>,
) -> CitationState {
    if matches!(parsed, ParsedCitationTarget::Malformed { .. }) {
        return CitationState::NotFound;
    }
    let Some(matches) = lookup["matches"].as_array() else {
        return CitationState::NotFound;
    };
    if matches.is_empty() {
        return CitationState::NotFound;
    }
    let valid_match_count = matches
        .iter()
        .filter(|candidate| candidate_valid_on(candidate, effective_as_of))
        .count();
    match parsed {
        ParsedCitationTarget::DocumentId { .. } => {
            let exact_valid = matches.iter().any(|candidate| {
                candidate["exact_identifier_match"].as_bool() == Some(true)
                    && (requested_as_of.is_none()
                        || candidate_valid_on(candidate, requested_as_of.unwrap_or_default()))
            });
            if exact_valid {
                CitationState::Exact
            } else {
                CitationState::StaleVersion
            }
        }
        ParsedCitationTarget::FreeTextArticle { .. } => match valid_match_count {
            0 => CitationState::StaleVersion,
            1 => CitationState::Normalized,
            _ => CitationState::Ambiguous,
        },
        ParsedCitationTarget::ArticleSourceUid(_)
        | ParsedCitationTarget::TextSourceUid(_)
        | ParsedCitationTarget::SectionSourceUid(_)
        | ParsedCitationTarget::Nor(_) => match valid_match_count {
            0 => CitationState::StaleVersion,
            1 => CitationState::Exact,
            _ => CitationState::Ambiguous,
        },
        // Decisions are dated, not versioned: existence (raw match count), not as-of validity,
        // determines the state. A decision is not "stale" — it either exists in the corpus or not.
        ParsedCitationTarget::DecisionDocumentId { .. }
        | ParsedCitationTarget::DecisionSourceUid(_)
        | ParsedCitationTarget::DecisionEcli(_) => match matches.len() {
            0 => CitationState::NotFound,
            1 => CitationState::Exact,
            _ => CitationState::Ambiguous,
        },
        ParsedCitationTarget::DecisionPourvoi(_) => match matches.len() {
            0 => CitationState::NotFound,
            1 => CitationState::Normalized,
            _ => CitationState::Ambiguous,
        },
        ParsedCitationTarget::Malformed { .. } => CitationState::NotFound,
    }
}

pub(crate) fn annotate_valid_matches(response: &mut Value, effective_as_of: &str) {
    let mut valid_count = 0usize;
    if let Some(matches) = response["matches"].as_array_mut() {
        for candidate in matches {
            let valid = candidate_valid_on(candidate, effective_as_of);
            candidate["valid_on_as_of"] = json!(valid);
            if valid {
                valid_count += 1;
            }
        }
    }
    response["valid_match_count"] = json!(valid_count);
}

pub(crate) fn candidate_valid_on(candidate: &Value, as_of: &str) -> bool {
    let validity = &candidate["validity"];
    let valid_from_ok = validity["from"]
        .as_str()
        .is_none_or(|valid_from| valid_from <= as_of);
    let valid_to_ok = validity["to"]
        .as_str()
        .is_none_or(|valid_to| as_of < valid_to);
    valid_from_ok && valid_to_ok
}

pub(crate) fn citation_state_name(state: CitationState) -> &'static str {
    match state {
        CitationState::Exact => "exact",
        CitationState::Normalized => "normalized",
        CitationState::Ambiguous => "ambiguous",
        CitationState::StaleVersion => "stale_version",
        CitationState::NotFound => "not_found",
        CitationState::SourceUnavailable => "source_unavailable",
    }
}

pub(crate) fn strict_citation_error(input: &str, state: CitationState) -> ErrorObject {
    ErrorObject {
        code: ErrorCode::NoResults,
        message: format!(
            "strict citation verification failed for `{input}` with state `{}`",
            citation_state_name(state)
        ),
        suggestions: vec![
            "Retry without --strict to inspect candidate matches and citation state.".into(),
            "Pass --as-of for historical statutory versions.".into(),
        ],
    }
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
