//! Shared retrieval-query helpers used by both the retrieval commands and the eval runners:
//! ParadeDB query-text normalization and boundary validation of retrieval tuning options.

use jurisearch_core::error::ErrorObject;
use jurisearch_storage::retrieval::RetrievalOptions;

pub(crate) fn parade_query_text(query: &str) -> Option<String> {
    let terms = query
        .split(|character: char| !character.is_alphanumeric())
        .map(str::trim)
        .filter(|term| !term.is_empty())
        .collect::<Vec<_>>();
    if terms.is_empty() {
        None
    } else {
        Some(terms.join(" "))
    }
}

/// Validate user-supplied tuning at the CLI/session boundary (before SQL): weights must be finite
/// and >= 0; probes in [1, 4096]. Invalid input is a `bad_input` error, not a silent clamp.
pub(crate) fn validate_retrieval_options(options: &RetrievalOptions) -> Result<(), ErrorObject> {
    for (name, weight) in [
        ("rrf-lexical-weight", options.rrf_lexical_weight),
        ("rrf-dense-weight", options.rrf_dense_weight),
    ] {
        if let Some(weight) = weight
            && (!weight.is_finite() || weight < 0.0)
        {
            return Err(ErrorObject::bad_input(format!(
                "--{name} must be a finite value >= 0"
            )));
        }
    }
    if let Some(probes) = options.ivfflat_probes
        && !(1..=4096).contains(&probes)
    {
        return Err(ErrorObject::bad_input("--probes must be between 1 and 4096"));
    }
    Ok(())
}
