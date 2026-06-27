//! `cite` response construction (work/09 P4-4B): citation-state classification, validity annotation,
//! and the local citation response envelope — all snapshot-bound and side-effect-free. The optional
//! online Légifrance probe is NOT here; it stays in the CLI adapter (it is a network side effect), which
//! calls [`build_cite`] first and may overwrite `response["online"]`. The site `cite` handler rejects
//! `online: true` and uses [`build_cite`] + [`enforce_strict_citation`] only.

use jurisearch_core::contract::CitationState;
use jurisearch_core::error::{ErrorCode, ErrorObject};
use serde_json::{Value, json};

use crate::citation::ParsedCitationTarget;

/// `cite` input: the parsed citation target plus the resolved/requested as-of dates and the request
/// flags that shape the response envelope. `online_requested` only affects response CONSTRUCTION (the
/// `source_unavailable` relabel and the `online` block / notes); the actual network probe is the CLI
/// adapter's job, after this builder.
pub struct CiteInput {
    /// The raw citation query text (echoed in the response).
    pub cite: String,
    /// The parsed citation target (classification + storage lookup mapping).
    pub parsed: ParsedCitationTarget,
    /// The effective as-of date (request `as_of` or today), used for validity classification.
    pub effective_as_of: String,
    /// The request's explicit `as_of`, if any (distinguishes "pinned" from "today").
    pub requested_as_of: Option<String>,
    /// Echoed `strict` flag (the strict GATE itself is enforced adapter-side via
    /// [`enforce_strict_citation`], after the optional online probe).
    pub strict: bool,
    /// Whether the caller requested online corroboration. Shapes the `online` block + the
    /// `source_unavailable` state, but never performs I/O here.
    pub online_requested: bool,
}

/// Build the local `cite` response body from the (already fetched) citation `lookup`: classify the
/// citation state (online-aware for the `source_unavailable` relabel), annotate per-match validity, and
/// assemble the response envelope INCLUDING the `online` block and the malformed/decision online notes
/// (both pure). PURE by design: the adapter does the conditional snapshot lookup
/// (`citation_lookup_in_snapshot`) — a malformed citation (`parsed.lookup() == None`) never touches the
/// DB, so the CLI keeps NOT opening an index for one, and the site handler runs the readiness gate +
/// lookup on its own snapshot. The CLI adapter runs the real Légifrance probe afterwards (and may
/// overwrite `online`); both adapters then call [`enforce_strict_citation`].
pub fn build_cite(input: &CiteInput, lookup: &Value) -> Value {
    let local_state = classify_citation_state(
        &input.parsed,
        lookup,
        input.effective_as_of.as_str(),
        input.requested_as_of.as_deref(),
    );
    // Decision identifiers are not corroborated against the Légifrance statutory probe, so an empty
    // decision lookup must stay `not_found` rather than being relabelled `source_unavailable`.
    let state = if input.online_requested
        && !input.parsed.is_decision()
        && !matches!(&input.parsed, ParsedCitationTarget::Malformed { .. })
        && lookup["matches"]
            .as_array()
            .is_none_or(|matches| matches.is_empty())
    {
        CitationState::SourceUnavailable
    } else {
        local_state
    };
    let mut response = json!({
        "query": input.cite,
        "input_class": input.parsed.input_class(),
        "normalized": input.parsed.normalized_value(),
        "as_of": input.effective_as_of,
        "requested_as_of": input.requested_as_of.as_deref(),
        "state": citation_state_name(state),
        "local_state": citation_state_name(local_state),
        "strict": input.strict,
        "online": {
            "requested": input.online_requested,
            "checked": false,
            "state": null,
            "note": null
        },
        "match_count": lookup["matches"].as_array().map_or(0, Vec::len),
        "matches": lookup["matches"].clone(),
    });
    annotate_valid_matches(&mut response, &input.effective_as_of);
    if input.online_requested && matches!(&input.parsed, ParsedCitationTarget::Malformed { .. }) {
        response["online"] = json!({
            "requested": true,
            "checked": false,
            "state": citation_state_name(CitationState::NotFound),
            "note": "Malformed citations are classified locally and are not sent to the online Légifrance probe."
        });
    } else if input.online_requested && input.parsed.is_decision() {
        // The online probe targets Légifrance (statutes). Decision verification belongs to Judilibre,
        // which is not yet wired here — never send a decision identifier to the statutory probe.
        response["online"] = json!({
            "requested": true,
            "checked": false,
            "provider": "judilibre",
            "state": null,
            "note": "Online decision verification uses the Judilibre API and is not yet wired; the state above is from the local index."
        });
    }
    response
}

/// Enforce the `--strict` citation gate over a built `cite` response (work/09 P4-4B): a non-exact /
/// non-normalized state is a hard failure. Runs adapter-side AFTER the optional online probe (which
/// never changes `response["state"]`), so both the CLI and the site `cite` paths share one gate. The
/// error text is byte-identical to the previous inline check.
pub fn enforce_strict_citation(
    response: &Value,
    input: &str,
    strict: bool,
) -> Result<(), ErrorObject> {
    if !strict {
        return Ok(());
    }
    let state = response["state"].as_str().unwrap_or("not_found");
    if matches!(state, "exact" | "normalized") {
        return Ok(());
    }
    Err(ErrorObject {
        code: ErrorCode::NoResults,
        message: format!("strict citation verification failed for `{input}` with state `{state}`"),
        suggestions: vec![
            "Retry without --strict to inspect candidate matches and citation state.".into(),
            "Pass --as-of for historical statutory versions.".into(),
        ],
    })
}

pub fn classify_citation_state(
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

pub fn annotate_valid_matches(response: &mut Value, effective_as_of: &str) {
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

pub fn candidate_valid_on(candidate: &Value, as_of: &str) -> bool {
    let validity = &candidate["validity"];
    let valid_from_ok = validity["from"]
        .as_str()
        .is_none_or(|valid_from| valid_from <= as_of);
    let valid_to_ok = validity["to"]
        .as_str()
        .is_none_or(|valid_to| as_of < valid_to);
    valid_from_ok && valid_to_ok
}

pub fn citation_state_name(state: CitationState) -> &'static str {
    match state {
        CitationState::Exact => "exact",
        CitationState::Normalized => "normalized",
        CitationState::Ambiguous => "ambiguous",
        CitationState::StaleVersion => "stale_version",
        CitationState::NotFound => "not_found",
        CitationState::SourceUnavailable => "source_unavailable",
    }
}
