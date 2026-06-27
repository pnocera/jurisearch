//! Pure query-text helpers shared by the retrieval builders, the CLI adapters, and the eval runners
//! (work/09 P4-4B): ParadeDB lexical-text normalization, case-insensitive ASCII substring search, ISO
//! date shape, and the `search` intent-routing citation classifier. No I/O, no snapshot — moved here so
//! `build_search` (and the site search handler) need no CLI helper; the CLI re-exports them unchanged.

/// Normalize free query text into the BM25 lexical query string: alphanumeric terms, space-joined.
/// `None` when the query has no searchable token (the caller reports `bad_input` precedence).
pub fn parade_query_text(query: &str) -> Option<String> {
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

/// Case-insensitive (ASCII) first-occurrence search; the needle must be ASCII. Byte index into
/// `haystack`, which is a valid char boundary because matched bytes are ASCII.
pub fn find_ascii_ci(haystack: &str, needle: &str) -> Option<usize> {
    let (haystack, needle) = (haystack.as_bytes(), needle.as_bytes());
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    (0..=haystack.len() - needle.len()).find(|&start| {
        haystack[start..start + needle.len()]
            .iter()
            .zip(needle)
            .all(|(left, right)| left.eq_ignore_ascii_case(right))
    })
}

/// Case-insensitive (ASCII) last-occurrence search; see [`find_ascii_ci`].
pub fn rfind_ascii_ci(haystack: &str, needle: &str) -> Option<usize> {
    let (haystack, needle) = (haystack.as_bytes(), needle.as_bytes());
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    (0..=haystack.len() - needle.len()).rev().find(|&start| {
        haystack[start..start + needle.len()]
            .iter()
            .zip(needle)
            .all(|(left, right)| left.eq_ignore_ascii_case(right))
    })
}

pub fn is_iso_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 10
        && bytes.iter().enumerate().all(|(index, &byte)| {
            if index == 4 || index == 7 {
                byte == b'-'
            } else {
                byte.is_ascii_digit()
            }
        })
}

/// A citation-shaped query parsed for structured resolution: an `Article <n>` reference plus the
/// as-of date that pins the version (from an `en vigueur au <date>` suffix, else the caller default).
pub struct LegiCitationRouting {
    /// The citation text with any `en vigueur au <date>` suffix stripped, used for the resolver's
    /// exact-citation-match ranking (so a temporal query still matches the stored citation).
    pub citation_query: String,
    pub article_number: String,
    pub code_hint: Option<String>,
    pub as_of: String,
}

/// Classify a query for intent routing. Returns `Some` when the query is a citation-shaped LEGI
/// lookup (contains an `Article <n>` reference, optionally with an `en vigueur au <date>` temporal
/// suffix) — those route to structured citation resolution. `None` means a conceptual query that
/// goes to hybrid semantic search. This classification is production-visible (the shared search
/// path), so the gate measures the same routing users hit.
pub fn legi_citation_routing(query: &str, default_as_of: &str) -> Option<LegiCitationRouting> {
    const EN_VIGUEUR: &str = " en vigueur au ";
    let (article_part, as_of) = match find_ascii_ci(query, EN_VIGUEUR) {
        Some(idx) => {
            let after = query[idx + EN_VIGUEUR.len()..].trim();
            let date = after.split_whitespace().next().unwrap_or(after);
            let as_of = if is_iso_date(date) {
                date.to_owned()
            } else {
                default_as_of.to_owned()
            };
            (query[..idx].trim(), as_of)
        }
        None => (query.trim(), default_as_of.to_owned()),
    };
    const ARTICLE: &str = "article ";
    let pos = rfind_ascii_ci(article_part, ARTICLE)?;
    let article_number = article_part[pos + ARTICLE.len()..].trim();
    if article_number.is_empty() {
        return None;
    }
    let code_hint = article_part[..pos].trim();
    Some(LegiCitationRouting {
        citation_query: article_part.to_owned(),
        article_number: article_number.to_owned(),
        code_hint: (!code_hint.is_empty()).then(|| code_hint.to_owned()),
        as_of,
    })
}
