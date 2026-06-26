//! Legislation-citation enrichment storage (migration v17).
//!
//! Citations are extracted from the archived Judilibre `/decision` responses (`official_api_responses`)
//! into per-decision OCCURRENCES (`decision_legislation_citations`), then DEDUPED by a normalized
//! `citation_key` into unique RESOLUTIONS (`legislation_citation_resolutions`) that are resolved against
//! the Legifrance API exactly once each. Reads go through `execute_sql` (JSON); writes use a
//! parameterized client (like the other ingestion writes).

use crate::runtime::{ManagedPostgres, StorageError, sql_string_literal};

/// Page the LATEST archived Judilibre `/decision` response per decision that carries a `visa`, for
/// citation extraction. Keyset on `subject_document_id` (exclusive `after_cursor`); de-duplicates
/// re-fetches by taking the highest `response_id` per decision. Returns
/// `{ "decisions": [{response_id, document_id, source_uid, visa}], "next_cursor": <last id|null> }`.
pub fn load_archived_decisions_with_visa_json(
    postgres: &ManagedPostgres,
    after_cursor: Option<&str>,
    limit: u32,
) -> Result<String, StorageError> {
    let cursor_predicate = after_cursor
        .map(|cursor| format!("AND subject_document_id > {}", sql_string_literal(cursor)))
        .unwrap_or_default();
    let limit = limit.max(1);
    postgres.execute_sql(&format!(
        r#"
WITH latest AS (
    SELECT DISTINCT ON (subject_document_id)
           response_id, subject_document_id, subject_source_uid, response_json
    FROM official_api_responses
    WHERE provider = 'judilibre'
      AND endpoint = '/cassation/judilibre/v1.0/decision'
      AND outcome = 'ok'
      AND subject_document_id IS NOT NULL
      AND response_json ? 'visa'
      AND jsonb_typeof(response_json->'visa') = 'array'
      AND jsonb_array_length(response_json->'visa') > 0
      {cursor_predicate}
    ORDER BY subject_document_id, response_id DESC
)
SELECT jsonb_build_object(
    'decisions', COALESCE((
        SELECT jsonb_agg(jsonb_build_object(
            'response_id', response_id,
            'document_id', subject_document_id,
            'source_uid', subject_source_uid,
            'visa', response_json->'visa'
        ) ORDER BY subject_document_id)
        FROM (SELECT * FROM latest ORDER BY subject_document_id LIMIT {limit}) page
    ), '[]'::jsonb),
    'next_cursor', (SELECT max(subject_document_id) FROM (SELECT subject_document_id FROM latest ORDER BY subject_document_id LIMIT {limit}) p)
)::text;
"#
    ))
}

/// One per-decision citation occurrence to record (idempotent: ON CONFLICT DO NOTHING on the
/// `(decision_document_id, visa_index, citation_key)` unique).
pub struct InsertCitationOccurrence<'a> {
    pub decision_document_id: &'a str,
    pub decision_source_uid: &'a str,
    pub source_response_id: i64,
    pub visa_index: i32,
    pub citation_key: &'a str,
    pub article_number_raw: Option<&'a str>,
    pub article_number_norm: &'a str,
    pub code_name_raw: Option<&'a str>,
    pub code_name_norm: &'a str,
    pub canonical_query: &'a str,
    pub legifrance_url: Option<&'a str>,
    pub raw_title: &'a str,
    pub extraction_method: &'a str,
}

/// Insert one citation occurrence; returns `true` when a new row was written (idempotent re-collect).
pub fn insert_citation_occurrence_with_client<C: postgres::GenericClient>(
    client: &mut C,
    occurrence: &InsertCitationOccurrence<'_>,
    outbox: Option<&crate::outbox::OutboxContext<'_>>,
) -> Result<bool, StorageError> {
    let occurrence_id = format!(
        "{}#{}#{}",
        occurrence.decision_document_id, occurrence.visa_index, occurrence.citation_key
    );
    let affected = client
        .execute(
            "INSERT INTO decision_legislation_citations (\
                 citation_occurrence_id, decision_document_id, decision_source_uid, source_response_id, \
                 visa_index, citation_key, article_number_raw, article_number_norm, code_name_raw, \
                 code_name_norm, canonical_query, legifrance_url, raw_title, extraction_method) \
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14) \
             ON CONFLICT (decision_document_id, visa_index, citation_key) DO NOTHING;",
            &[
                &occurrence_id,
                &occurrence.decision_document_id,
                &occurrence.decision_source_uid,
                &occurrence.source_response_id,
                &occurrence.visa_index,
                &occurrence.citation_key,
                &occurrence.article_number_raw,
                &occurrence.article_number_norm,
                &occurrence.code_name_raw,
                &occurrence.code_name_norm,
                &occurrence.canonical_query,
                &occurrence.legifrance_url,
                &occurrence.raw_title,
                &occurrence.extraction_method,
            ],
        )
        .map_err(StorageError::PostgresClient)?;

    // Outbox (§5.1, plan P1): a new occurrence is a document-scoped `upsert`; its apply order trails
    // `official_api_responses` (it FKs `source_response_id`). Only on an actual insert.
    if affected > 0
        && let Some(ctx) = outbox
    {
        let corpus: String = client
            .query_one(
                "SELECT corpus FROM documents WHERE document_id = $1;",
                &[&occurrence.decision_document_id],
            )
            .map_err(StorageError::PostgresClient)?
            .get("corpus");
        crate::outbox::emit_change(
            client,
            ctx,
            &crate::outbox::OutboxEvent::scope(
                &corpus,
                "decision_legislation_citations",
                jurisearch_package::event::EventKind::Upsert,
                crate::outbox::scope_kind::DOCUMENT,
                occurrence.decision_document_id,
            ),
        )?;
    }
    Ok(affected > 0)
}

/// Upsert the deduped resolution row for a citation_key as `pending` (no-op on the dedup fields if it
/// already exists; never resets a resolved row's Legifrance status).
pub fn upsert_citation_resolution_pending_with_client<C: postgres::GenericClient>(
    client: &mut C,
    citation_key: &str,
    article_number_norm: &str,
    code_name_norm: &str,
    canonical_query: &str,
    decision_document_id: &str,
    outbox: Option<&crate::outbox::OutboxContext<'_>>,
) -> Result<(), StorageError> {
    // Resolutions are keyed `(corpus, citation_key)` (per-corpus replicated data, INV-4), so the
    // corpus comes from THIS occurrence's decision (`decision_document_id` → `documents.corpus`,
    // design §4.1; P0), not from "all occurrences of the key" (which may legitimately span corpora).
    // `ON CONFLICT (corpus, citation_key)` means a later corpus citing the same article creates its
    // OWN resolution rather than silently inheriting the first corpus's attribution. The NOT NULL
    // corpus column FAILS LOUDLY if the decision is unknown (no runtime fallback).
    let affected = client
        .execute(
            "INSERT INTO legislation_citation_resolutions (\
                 corpus, citation_key, article_number_norm, code_name_norm, canonical_query) \
             VALUES ((SELECT corpus FROM documents WHERE document_id = $5), $1, $2, $3, $4) \
             ON CONFLICT (corpus, citation_key) DO NOTHING;",
            &[
                &citation_key,
                &article_number_norm,
                &code_name_norm,
                &canonical_query,
                &decision_document_id,
            ],
        )
        .map_err(StorageError::PostgresClient)?;

    // Outbox (§5.1, plan P1): a newly-created resolution is a `citation_resolution`-scoped `upsert`.
    if affected > 0
        && let Some(ctx) = outbox
    {
        let corpus: String = client
            .query_one(
                "SELECT corpus FROM documents WHERE document_id = $1;",
                &[&decision_document_id],
            )
            .map_err(StorageError::PostgresClient)?
            .get("corpus");
        crate::outbox::emit_change(
            client,
            ctx,
            &crate::outbox::OutboxEvent::scope(
                &corpus,
                "legislation_citation_resolutions",
                jurisearch_package::event::EventKind::Upsert,
                crate::outbox::scope_kind::CITATION_RESOLUTION,
                citation_key,
            ),
        )?;
    }
    Ok(())
}

/// Recompute `occurrence_count` on every resolution from the occurrence table (collect finalize).
///
/// `occurrence_count` is a replicated, non-volatile column, so this is a real `legislation_citation_
/// resolutions` mutation: the UPDATE only touches rows whose count actually changed (`IS DISTINCT
/// FROM`), and — in the same transaction — emits one `citation_resolution`-scoped `upsert` outbox row
/// per changed resolution (§5.1, P1). Without this, a new occurrence of an already-known citation key
/// (whose pending-upsert hit `ON CONFLICT DO NOTHING` and emitted nothing) would change the
/// authoritative count with no ledger entry.
pub fn finalize_citation_occurrence_counts(
    postgres: &ManagedPostgres,
    outbox: Option<&crate::outbox::OutboxContext<'_>>,
) -> Result<(), StorageError> {
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    let mut tx = client.transaction().map_err(StorageError::PostgresClient)?;
    // Count occurrences per (corpus, citation_key): an occurrence's corpus is its decision's corpus.
    let changed = tx
        .query(
            "UPDATE legislation_citation_resolutions r \
             SET occurrence_count = computed.n, updated_at = now() \
             FROM (\
                 SELECT res.corpus, res.citation_key, (\
                     SELECT count(*) FROM decision_legislation_citations c \
                     JOIN documents d ON d.document_id = c.decision_document_id \
                     WHERE c.citation_key = res.citation_key AND d.corpus = res.corpus\
                 ) AS n \
                 FROM legislation_citation_resolutions res\
             ) computed \
             WHERE r.corpus = computed.corpus AND r.citation_key = computed.citation_key \
               AND r.occurrence_count IS DISTINCT FROM computed.n \
             RETURNING r.corpus, r.citation_key;",
            &[],
        )
        .map_err(StorageError::PostgresClient)?;
    if let Some(ctx) = outbox {
        for row in &changed {
            let corpus: String = row.get("corpus");
            let citation_key: String = row.get("citation_key");
            crate::outbox::emit_change(
                &mut tx,
                ctx,
                &crate::outbox::OutboxEvent::scope(
                    &corpus,
                    "legislation_citation_resolutions",
                    jurisearch_package::event::EventKind::Upsert,
                    crate::outbox::scope_kind::CITATION_RESOLUTION,
                    &citation_key,
                ),
            )?;
        }
    }
    tx.commit().map_err(StorageError::PostgresClient)?;
    Ok(())
}

/// Page unique citation resolutions still needing a Legifrance call (`pending`, or `upstream_error`
/// when `retry_errors`). Keyset on `(corpus, citation_key)` — resolutions are per-corpus, so each
/// citation carries its `corpus` and the cursor encodes both (delimited by the ASCII unit separator
/// `\x1f`, which appears in neither a corpus token nor a citation key). Returns
/// `{ "citations": [{corpus, citation_key, article_number_norm, code_name_norm, canonical_query}], "next_cursor" }`.
pub fn load_pending_citation_resolutions_json(
    postgres: &ManagedPostgres,
    after_cursor: Option<&str>,
    retry_errors: bool,
    limit: u32,
) -> Result<String, StorageError> {
    let cursor_predicate = after_cursor
        .and_then(|cursor| cursor.split_once('\u{1f}'))
        .map(|(corpus, citation_key)| {
            format!(
                "AND (corpus, citation_key) > ({}, {})",
                sql_string_literal(corpus),
                sql_string_literal(citation_key)
            )
        })
        .unwrap_or_default();
    let status_predicate = if retry_errors {
        "legifrance_status IN ('pending','upstream_error','parse_error')"
    } else {
        "legifrance_status = 'pending'"
    };
    let limit = limit.max(1);
    postgres.execute_sql(&format!(
        r#"
WITH page AS (
    SELECT corpus, citation_key, article_number_norm, code_name_norm, canonical_query
    FROM legislation_citation_resolutions
    WHERE {status_predicate}
      {cursor_predicate}
    ORDER BY corpus, citation_key
    LIMIT {limit}
)
SELECT jsonb_build_object(
    'citations', COALESCE((
        SELECT jsonb_agg(jsonb_build_object(
            'corpus', corpus,
            'citation_key', citation_key,
            'article_number_norm', article_number_norm,
            'code_name_norm', code_name_norm,
            'canonical_query', canonical_query
        ) ORDER BY corpus, citation_key)
        FROM page
    ), '[]'::jsonb),
    'next_cursor', (SELECT corpus || chr(31) || citation_key
                    FROM page ORDER BY corpus DESC, citation_key DESC LIMIT 1)
)::text;
"#
    ))
}

/// Record the result of a Legifrance call for one `(corpus, citation_key)` resolution.
#[allow(clippy::too_many_arguments)]
pub fn update_citation_resolution_with_client<C: postgres::GenericClient>(
    client: &mut C,
    corpus: &str,
    citation_key: &str,
    legifrance_status: &str,
    legifrance_response_id: Option<i64>,
    legifrance_request_fingerprint: Option<&str>,
    error: Option<&str>,
    outbox: Option<&crate::outbox::OutboxContext<'_>>,
) -> Result<(), StorageError> {
    client
        .execute(
            "UPDATE legislation_citation_resolutions \
             SET legifrance_status = $3, legifrance_response_id = $4, \
                 legifrance_request_fingerprint = $5, error = $6, fetched_at = now(), updated_at = now() \
             WHERE corpus = $1 AND citation_key = $2;",
            &[
                &corpus,
                &citation_key,
                &legifrance_status,
                &legifrance_response_id,
                &legifrance_request_fingerprint,
                &error,
            ],
        )
        .map_err(StorageError::PostgresClient)?;

    // Outbox (§5.1, plan P1): the resolution's Legifrance result is an in-place `upsert` of the
    // (corpus, citation_key) row.
    if let Some(ctx) = outbox {
        crate::outbox::emit_change(
            client,
            ctx,
            &crate::outbox::OutboxEvent::scope(
                corpus,
                "legislation_citation_resolutions",
                jurisearch_package::event::EventKind::Upsert,
                crate::outbox::scope_kind::CITATION_RESOLUTION,
                citation_key,
            ),
        )?;
    }
    Ok(())
}

/// Coverage report for the legislation-citation enrichment (`status` / command reports).
pub fn legislation_citations_coverage_json(
    postgres: &ManagedPostgres,
) -> Result<String, StorageError> {
    postgres.execute_sql(
        r#"
SELECT jsonb_build_object(
    'occurrences', (SELECT count(*) FROM decision_legislation_citations),
    'decisions_with_citations', (SELECT count(DISTINCT decision_document_id) FROM decision_legislation_citations),
    'unique_citations', (SELECT count(*) FROM legislation_citation_resolutions),
    'by_legifrance_status', COALESCE((
        SELECT jsonb_agg(jsonb_build_object('status', status, 'count', n) ORDER BY status)
        FROM (SELECT legifrance_status AS status, count(*) AS n
              FROM legislation_citation_resolutions GROUP BY legifrance_status) s
    ), '[]'::jsonb)
)::text;
"#,
    )
}
