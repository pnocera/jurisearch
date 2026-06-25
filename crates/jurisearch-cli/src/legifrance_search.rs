//! CLI-side Legifrance request-body shaping: sanitize a free-text query and build the
//! `/search` JSON body. Shared by `cite --online` (retrieval) and legislation-citation
//! enrichment. Distinct from the official-api crate's generic exchange client.

use serde_json::{Value, json};

/// Cap (in chars) on the Legifrance `/search` `valeur`. The engine returns HTTP 500 on very long values
/// (empirically anything past ~450 chars — pathological multi-article visa concatenations like
/// "S16DELADÉCLARATION…ET695-9-22"); any real "article X of code Y" query is well under 100 chars. We
/// truncate rather than skip so every citation still gets a real, archived attempt (the over-long garbage
/// simply resolves to `not_found` instead of a noisy `upstream_error` + wasted `--retry-errors` reruns).
pub(crate) const LEGIFRANCE_QUERY_MAX_CHARS: usize = 200;

/// Narrow input hygiene for a Legifrance `/search` value: replace control chars with spaces, collapse
/// whitespace runs, trim, and cap the length (see [`LEGIFRANCE_QUERY_MAX_CHARS`]). Deliberately does NOT
/// rewrite article prefixes (`511-8`→`R.511-8`), split multi-article citations, or strip prose tails —
/// those are `parse_visa_citation` concerns (the dominant recall lever) handled in a separate pass, not
/// here. The unsanitized `canonical_query` stays on the resolution row, and the sanitized body is what
/// the archive records, so the transform is auditable by comparing the two.
pub(crate) fn sanitize_legifrance_query(query: &str) -> String {
    let cleaned: String = query
        .chars()
        .map(|ch| if ch.is_control() { ' ' } else { ch })
        .collect();
    cleaned
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(LEGIFRANCE_QUERY_MAX_CHARS)
        .collect()
}

/// Build the Legifrance `/search` request body for a code-article citation query. Uses the REAL
/// Legifrance contract (`fond=CODE_DATE` + `recherche.champs` with `TOUS_LES_MOTS_DANS_UN_CHAMP` over
/// all fields). The previous `{query, pageSize}` shape was rejected by the Legifrance engine with HTTP
/// 500 ("Une exception non gérée"); `TOUS_LES_MOTS_DANS_UN_CHAMP` is also far more precise and ~1s vs
/// ~10s for `UN_DES_MOTS`, and beat separate `NUM_ARTICLE`+`TITLE` champs on a live 120-citation recall
/// sample (validated against the live API). Our citations are all "code …" (collect skips non-code
/// legislation), so `CODE_DATE` is the right fond; no date filter ⇒ current version. The value is run
/// through [`sanitize_legifrance_query`] first to avoid the engine's HTTP-500-on-very-long-input.
pub(crate) fn legifrance_code_search_body(query: &str) -> Value {
    json!({
        "fond": "CODE_DATE",
        "recherche": {
            "operateur": "ET",
            "sort": "PERTINENCE",
            "typePagination": "DEFAUT",
            "pageNumber": 1,
            "pageSize": 5,
            "champs": [{
                "typeChamp": "ALL",
                "operateur": "ET",
                "criteres": [{
                    "typeRecherche": "TOUS_LES_MOTS_DANS_UN_CHAMP",
                    "valeur": sanitize_legifrance_query(query),
                    "operateur": "ET"
                }]
            }]
        }
    })
}
