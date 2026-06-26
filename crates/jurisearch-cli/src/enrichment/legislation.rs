//! Legislation-citation collection (from archived Judilibre /decision visas) and Legifrance resolution.

use crate::*;

/// A legislation citation extracted from a Judilibre `visa` entry. Normalized so that the SAME article
/// across many decisions dedups to one `citation_key` (resolved against Legifrance exactly once).
pub(crate) struct ParsedVisaCitation {
    pub(crate) article_number_raw: String,
    pub(crate) article_number_norm: String,
    pub(crate) code_name_raw: String,
    pub(crate) code_name_norm: String,
    pub(crate) canonical_query: String,
    pub(crate) citation_key: String,
    pub(crate) legifrance_url: Option<String>,
    pub(crate) extraction_method: &'static str,
}

/// Byte index of the first case-insensitive (ASCII) occurrence of `needle` in `haystack`.
pub(crate) fn find_ci(haystack: &str, needle: &str) -> Option<usize> {
    let (hay, ndl) = (haystack.as_bytes(), needle.as_bytes());
    if ndl.is_empty() || hay.len() < ndl.len() {
        return None;
    }
    (0..=hay.len() - ndl.len()).find(|&i| hay[i..i + ndl.len()].eq_ignore_ascii_case(ndl))
}

/// First `href="…"` value in an HTML fragment.
pub(crate) fn extract_first_href(html: &str) -> Option<String> {
    let start = find_ci(html, "href=\"")? + 6;
    let rest = &html[start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_owned())
}

/// Strip HTML tags to plain text (for the regex fallback when no Legifrance URL is present).
pub(crate) fn strip_html_tags(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut in_tag = false;
    for ch in html.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(ch),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Split a citation query ("609 code de procédure civile") into (article, code) at the first "code".
/// Returns None for non-code legislation (loi/décret/…) or a malformed split.
pub(crate) fn split_article_code(query: &str) -> Option<(String, String)> {
    let query = query.trim();
    let idx = find_ci(query, "code")?;
    if idx == 0 {
        return None;
    }
    let mut article = query[..idx].trim().trim_end_matches([',', ' ']).to_owned();
    // Strip a leading "Article" label and trailing connectors ("du", "de la", "de l'", "des", "de").
    if let Some(rest) = article
        .strip_prefix("Article")
        .or_else(|| article.strip_prefix("article"))
    {
        article = rest.trim().to_owned();
    }
    let lower = article.to_lowercase();
    for connector in [" de la", " de l'", " des", " du", " de"] {
        if lower.ends_with(connector) {
            article.truncate(article.len() - connector.len());
            article = article.trim().to_owned();
            break;
        }
    }
    let code = query[idx..]
        .trim()
        .trim_end_matches(['.', ',', ' '])
        .to_owned();
    if article.is_empty() || code.chars().count() < 4 {
        return None;
    }
    Some((article, code))
}

/// Normalize an article number for dedup: uppercase, whitespace removed ("L. 121-1" -> "L.121-1").
pub(crate) fn normalize_article_number(raw: &str) -> String {
    raw.chars()
        .filter(|ch| !ch.is_whitespace())
        .collect::<String>()
        .to_uppercase()
}

/// Normalize a code name for dedup: lowercase, single-spaced, trailing punctuation stripped.
pub(crate) fn normalize_code_name(raw: &str) -> String {
    raw.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
        .trim_end_matches(['.', ','])
        .trim()
        .to_owned()
}

/// Stable dedup key for a normalized (article, code) citation.
pub(crate) fn legislation_citation_key(article_norm: &str, code_norm: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(b"legi-citation:v1\0");
    hasher.update(article_norm.as_bytes());
    hasher.update([0u8]);
    hasher.update(code_norm.as_bytes());
    let hex: String = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("legi-cite:{hex}")
}

/// Parse one Judilibre `visa` title into a normalized citation. Prefers the embedded Legifrance URL's
/// `query` param (exactly what Judilibre meant); falls back to a conservative parse of the plain title.
pub(crate) fn parse_visa_citation(title: &str) -> Option<ParsedVisaCitation> {
    let build = |article: String,
                 code: String,
                 legifrance_url: Option<String>,
                 extraction_method: &'static str| {
        let article_number_norm = normalize_article_number(&article);
        let code_name_norm = normalize_code_name(&code);
        if article_number_norm.is_empty() || code_name_norm.is_empty() {
            return None;
        }
        let canonical_query = format!("{article_number_norm} {code_name_norm}");
        let citation_key = legislation_citation_key(&article_number_norm, &code_name_norm);
        Some(ParsedVisaCitation {
            article_number_raw: article,
            article_number_norm,
            code_name_raw: code,
            code_name_norm,
            canonical_query,
            citation_key,
            legifrance_url,
            extraction_method,
        })
    };

    // 1. Preferred: the Legifrance URL `query` parameter.
    if let Some(url) = extract_first_href(title)
        && url.contains("legifrance")
        && let Ok(parsed) = Url::parse(&url)
        && let Some(query) = parsed
            .query_pairs()
            .find(|(key, _)| key == "query")
            .map(|(_, value)| value.into_owned())
        && let Some((article, code)) = split_article_code(&query)
    {
        return build(article, code, Some(url), "legifrance_url_query");
    }

    // 2. Fallback: strip the HTML and parse the plain title ("Article 609 du code de procédure civile.").
    let plain = strip_html_tags(title);
    if let Some((article, code)) = split_article_code(&plain) {
        return build(article, code, None, "visa_title_regex");
    }
    None
}

/// Whether a Legifrance search response reports at least one hit.
pub(crate) fn legifrance_response_has_results(response: &Value) -> bool {
    if let Some(total) = response["totalResultNumber"].as_i64() {
        return total > 0;
    }
    response["results"]
        .as_array()
        .is_some_and(|results| !results.is_empty())
}

/// `ingest collect-legislation-citations`: extract citations from the archived Judilibre `/decision`
/// responses (visa) into per-decision occurrences + deduped pending resolutions. No network.
pub(crate) fn collect_legislation_citations_payload(
    index_dir: Option<&Path>,
    limit: Option<u32>,
) -> Result<Value, ErrorObject> {
    let index_dir = require_existing_index_dir(index_dir)?;
    let postgres = open_index(index_dir.as_path())?;
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(|error| storage_error_object(StorageError::PostgresClient(error)))?;
    let run_id = crate::ingest::producer_run_id("collect-legislation-citations");
    let outbox = jurisearch_storage::outbox::OutboxContext::new(
        &run_id,
        jurisearch_storage::migrations::CURRENT_SCHEMA_VERSION,
    );

    let mut decisions_scanned: u64 = 0;
    let mut occurrences_inserted: u64 = 0;
    let mut parse_failures: u64 = 0;
    let mut cursor: Option<String> = None;
    loop {
        let page_limit = match limit {
            Some(limit) => {
                let done = u32::try_from(decisions_scanned).unwrap_or(u32::MAX);
                if done >= limit {
                    break;
                }
                (limit - done).min(COLLECT_CITATIONS_PAGE_SIZE)
            }
            None => COLLECT_CITATIONS_PAGE_SIZE,
        };
        let page_json =
            load_archived_decisions_with_visa_json(&postgres, cursor.as_deref(), page_limit)
                .map_err(storage_error_object)?;
        let page: Value = serde_json::from_str(&page_json)
            .map_err(|error| dependency_unavailable(error.to_string()))?;
        let Some(decisions) = page["decisions"].as_array().filter(|d| !d.is_empty()) else {
            break;
        };
        for decision in decisions {
            decisions_scanned += 1;
            let (Some(document_id), Some(source_uid), Some(response_id)) = (
                decision["document_id"].as_str(),
                decision["source_uid"].as_str(),
                decision["response_id"].as_i64(),
            ) else {
                continue;
            };
            for (visa_index, item) in decision["visa"]
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
            {
                let Some(title) = item["title"].as_str() else {
                    continue;
                };
                let Some(citation) = parse_visa_citation(title) else {
                    parse_failures += 1;
                    continue;
                };
                // The occurrence and its deduped pending resolution (and both outbox emits) commit
                // together, so an occurrence is never recorded without its resolution row or ledger.
                let inserted = in_outbox_txn(&mut client, |tx| {
                    let inserted = insert_citation_occurrence_with_client(
                        tx,
                        &InsertCitationOccurrence {
                            decision_document_id: document_id,
                            decision_source_uid: source_uid,
                            source_response_id: response_id,
                            visa_index: i32::try_from(visa_index).unwrap_or(i32::MAX),
                            citation_key: &citation.citation_key,
                            article_number_raw: Some(&citation.article_number_raw),
                            article_number_norm: &citation.article_number_norm,
                            code_name_raw: Some(&citation.code_name_raw),
                            code_name_norm: &citation.code_name_norm,
                            canonical_query: &citation.canonical_query,
                            legifrance_url: citation.legifrance_url.as_deref(),
                            raw_title: title,
                            extraction_method: citation.extraction_method,
                        },
                        Some(&outbox),
                    )
                    .map_err(storage_error_object)?;
                    upsert_citation_resolution_pending_with_client(
                        tx,
                        &citation.citation_key,
                        &citation.article_number_norm,
                        &citation.code_name_norm,
                        &citation.canonical_query,
                        document_id,
                        Some(&outbox),
                    )
                    .map_err(storage_error_object)?;
                    Ok(inserted)
                })?;
                if inserted {
                    occurrences_inserted += 1;
                }
            }
        }
        cursor = page["next_cursor"].as_str().map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    finalize_citation_occurrence_counts(&postgres, Some(&outbox)).map_err(storage_error_object)?;
    let coverage: Value = serde_json::from_str(
        &legislation_citations_coverage_json(&postgres).map_err(storage_error_object)?,
    )
    .map_err(|error| dependency_unavailable(error.to_string()))?;
    Ok(json!({
        "schema_version": SCHEMA_VERSION,
        "command": "ingest collect-legislation-citations",
        "index_dir": index_dir.display().to_string(),
        "decisions_scanned": decisions_scanned,
        "occurrences_inserted": occurrences_inserted,
        "parse_failures": parse_failures,
        "coverage": coverage,
    }))
}

/// `ingest enrich-legislation-citations`: resolve each deduped pending citation against the Legifrance
/// API exactly once, archiving the raw Legifrance response in `official_api_responses`. Sequential (the
/// PisteClient OAuth token cache is not shared across threads); resumable (each resolution row records
/// its outcome, so a re-run skips resolved citations).
pub(crate) fn enrich_legislation_citations_payload(
    index_dir: Option<&Path>,
    limit: Option<u32>,
    retry_errors: bool,
) -> Result<Value, ErrorObject> {
    // No preflight credential guard: `legifrance_search_exchange` converts a missing OAuth client or a
    // token-acquisition failure into an archivable `UpstreamError` exchange, so EVERY attempt (incl.
    // missing-credential) is durably recorded in official_api_responses + the resolution row — uniform
    // with token/HTTP failures (slice-2 review fix). The command summary surfaces the error count.
    let index_dir = require_existing_index_dir(index_dir)?;
    let postgres = open_index(index_dir.as_path())?;
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(|error| storage_error_object(StorageError::PostgresClient(error)))?;
    let run_id = crate::ingest::producer_run_id("enrich-legislation-citations");
    let outbox = jurisearch_storage::outbox::OutboxContext::new(
        &run_id,
        jurisearch_storage::migrations::CURRENT_SCHEMA_VERSION,
    );
    let mut piste = PisteClient::new(OfficialApiConfig::from_env());
    let api_environment = piste.api_environment();

    let mut considered: u64 = 0;
    let mut resolved_ok: u64 = 0;
    let mut not_found: u64 = 0;
    let mut errors: u64 = 0;
    let mut cursor: Option<String> = None;
    loop {
        let page_limit = match limit {
            Some(limit) => {
                let done = u32::try_from(considered).unwrap_or(u32::MAX);
                if done >= limit {
                    break;
                }
                (limit - done).min(ENRICH_CITATIONS_PAGE_SIZE)
            }
            None => ENRICH_CITATIONS_PAGE_SIZE,
        };
        let page_json = load_pending_citation_resolutions_json(
            &postgres,
            cursor.as_deref(),
            retry_errors,
            page_limit,
        )
        .map_err(storage_error_object)?;
        let page: Value = serde_json::from_str(&page_json)
            .map_err(|error| dependency_unavailable(error.to_string()))?;
        let Some(citations) = page["citations"].as_array().filter(|c| !c.is_empty()) else {
            break;
        };
        for citation in citations {
            let (Some(corpus), Some(citation_key), Some(canonical_query)) = (
                citation["corpus"].as_str(),
                citation["citation_key"].as_str(),
                citation["canonical_query"].as_str(),
            ) else {
                continue;
            };
            considered += 1;
            let body = legifrance_code_search_body(canonical_query);
            // The Legifrance HTTP call happens BEFORE the transaction (never hold a txn over I/O).
            let exchange = piste.legifrance_search_exchange(&body);
            let (status, error) = match exchange.outcome {
                OfficialApiOutcome::Ok => {
                    let has_results = exchange
                        .response_json
                        .as_ref()
                        .is_some_and(legifrance_response_has_results);
                    if has_results {
                        resolved_ok += 1;
                        ("ok", None)
                    } else {
                        not_found += 1;
                        ("not_found", None)
                    }
                }
                OfficialApiOutcome::ParseError => {
                    errors += 1;
                    ("parse_error", exchange.error.as_deref())
                }
                OfficialApiOutcome::UpstreamError => {
                    errors += 1;
                    ("upstream_error", exchange.error.as_deref())
                }
            };
            // Archive the Legifrance response AND record the resolution result + both outbox emits in
            // ONE transaction, so the resolution's `legifrance_response_id` can never reference an
            // archive row that did not commit, and neither mutation is left without its ledger row.
            in_outbox_txn(&mut client, |tx| {
                // The Legifrance lookup has no subject document; it belongs to this corpus.
                let response_id = archive_exchange(
                    tx,
                    &exchange,
                    api_environment,
                    None,
                    None,
                    None,
                    Some(citation_key),
                    Some(corpus),
                    Some(&outbox),
                )?;
                update_citation_resolution_with_client(
                    tx,
                    corpus,
                    citation_key,
                    status,
                    Some(response_id),
                    Some(&exchange.request_fingerprint),
                    error,
                    Some(&outbox),
                )
                .map_err(storage_error_object)
            })?;
        }
        cursor = page["next_cursor"].as_str().map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    let coverage: Value = serde_json::from_str(
        &legislation_citations_coverage_json(&postgres).map_err(storage_error_object)?,
    )
    .map_err(|error| dependency_unavailable(error.to_string()))?;
    // Operator hint when every attempt failed (commonly missing/invalid Legifrance OAuth creds) — the
    // failures are still archived as upstream_error rows; this just points at the likely cause.
    let note = (considered > 0 && resolved_ok == 0 && not_found == 0 && errors == considered).then(|| {
        "all Legifrance calls failed (archived as upstream_error); check PISTE_OAUTH_CLIENT_ID / \
         PISTE_OAUTH_CLIENT_SECRET and the Legifrance subscription"
    });
    Ok(json!({
        "schema_version": SCHEMA_VERSION,
        "command": "ingest enrich-legislation-citations",
        "index_dir": index_dir.display().to_string(),
        "considered": considered,
        "resolved_ok": resolved_ok,
        "not_found": not_found,
        "errors": errors,
        "note": note,
        "coverage": coverage,
    }))
}
