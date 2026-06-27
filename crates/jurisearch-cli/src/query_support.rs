//! Shared retrieval-query helpers used by both the retrieval commands and the eval runners:
//! ParadeDB query-text normalization and boundary validation of retrieval tuning options.

use jurisearch_core::error::ErrorObject;
use jurisearch_storage::retrieval::RetrievalOptions;

/// ParadeDB lexical-text normalization moved to `jurisearch-query` (work/09 P4-4B) so `build_search`
/// and the site search handler share it; re-exported so the eval runners' `crate::parade_query_text`
/// references resolve unchanged.
pub(crate) use jurisearch_query::parade_query_text;

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
        return Err(ErrorObject::bad_input(
            "--probes must be between 1 and 4096",
        ));
    }
    // Authority weight is valid in [0.0, 1.0]; `0.0` is allowed but normalized to inert
    // (effective_authority_weight returns None), so it is a clean OFF baseline rather than a reject.
    if let Some(weight) = options.authority_weight
        && (!weight.is_finite() || !(0.0..=1.0).contains(&weight))
    {
        return Err(ErrorObject::bad_input(
            "--authority-weight must be a finite value in [0.0, 1.0]",
        ));
    }
    Ok(())
}
