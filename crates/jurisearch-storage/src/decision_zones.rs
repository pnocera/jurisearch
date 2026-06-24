//! Lazy Judilibre official-zone cache (migration v12 `decision_zones`).
//!
//! A per-decision overlay so `fetch --part --online` can serve official Cassation zones without
//! mutating the immutable canonical records or the corpus-level `zone_accurate=false` honesty. Reads
//! go through `execute_sql` (JSON, like the other read helpers); the network-fed upsert uses a
//! parameterized client (like the ingestion writes).

use postgres::GenericClient;
use serde_json::Value;

use crate::runtime::{ManagedPostgres, StorageError, sql_string_literal};

/// Return the cached zone row for `document_id` as JSON, or `null` when absent. Includes an `expired`
/// flag (TTL elapsed) so callers can decide whether to refresh.
pub fn decision_zones_json(
    postgres: &ManagedPostgres,
    document_id: &str,
) -> Result<String, StorageError> {
    let id = sql_string_literal(document_id);
    postgres.execute_sql(&format!(
        r#"
SELECT coalesce((
    SELECT jsonb_build_object(
        'document_id', document_id,
        'provider', provider,
        'provider_decision_id', provider_decision_id,
        'source_uid', source_uid,
        'ecli', ecli,
        'status', status,
        'fetched_at', fetched_at,
        'expires_at', expires_at,
        'expired', (expires_at IS NOT NULL AND expires_at <= now()),
        'upstream_update_date', upstream_update_date,
        'upstream_decision_date', upstream_decision_date,
        'text_hash', text_hash,
        'offset_unit', offset_unit,
        'zone_schema_version', zone_schema_version,
        'zones', zones_json,
        'error', error
    )
    FROM decision_zones
    WHERE document_id = {id}
), 'null'::jsonb)::text;
"#
    ))
}

/// Resolution metadata for a Judilibre lookup, as JSON: `{source_uid, ecli, decision_date, pourvoi}`
/// where `pourvoi` is the first parser-valid (`NN-NNNN..`) case number, or `null` if the id is not a
/// decision. `decision_date` is the decision's `valid_from` (decisions are dated, not versioned).
pub fn decision_resolution_metadata_json(
    postgres: &ManagedPostgres,
    document_id: &str,
) -> Result<String, StorageError> {
    let id = sql_string_literal(document_id);
    postgres.execute_sql(&format!(
        r#"
SELECT coalesce((
    SELECT jsonb_build_object(
        'source_uid', source_uid,
        'ecli', canonical_json->>'ecli',
        'decision_date', valid_from::text,
        'pourvoi', (
            SELECT cn
            FROM jsonb_array_elements_text(coalesce(canonical_json->'case_numbers', '[]'::jsonb)) AS cn
            WHERE replace(replace(cn, '.', ''), ' ', '') ~ '^[0-9]{{2}}-[0-9]{{4,6}}$'
            ORDER BY cn
            LIMIT 1
        )
    )
    FROM documents
    WHERE document_id = {id} AND kind = 'decision'
), 'null'::jsonb)::text;
"#
    ))
}

/// One row to upsert into `decision_zones`. `zones_json`/`raw_json` are stored as jsonb; `ttl_seconds`
/// (when set) yields `expires_at = now() + ttl`.
pub struct UpsertDecisionZones<'a> {
    pub document_id: &'a str,
    pub provider: &'a str,
    pub provider_decision_id: Option<&'a str>,
    pub source_uid: &'a str,
    pub ecli: Option<&'a str>,
    pub status: &'a str,
    pub upstream_update_date: Option<&'a str>,
    pub upstream_decision_date: Option<&'a str>,
    pub text_hash: Option<&'a str>,
    pub offset_unit: Option<&'a str>,
    pub zones_json: &'a Value,
    pub raw_json: &'a Value,
    pub error: Option<&'a str>,
    pub ttl_seconds: Option<i64>,
}

/// Upsert a cached zone row, opening a client from the managed Postgres (the ingestion-write pattern).
pub fn upsert_decision_zones(
    postgres: &ManagedPostgres,
    row: &UpsertDecisionZones<'_>,
) -> Result<(), StorageError> {
    let mut client = postgres::Client::connect(&postgres.connection_string(), postgres::NoTls)
        .map_err(StorageError::PostgresClient)?;
    upsert_decision_zones_with_client(&mut client, row)
}

/// Upsert a cached zone row (parameterized; safe for jsonb/text values).
pub fn upsert_decision_zones_with_client<C: GenericClient>(
    client: &mut C,
    row: &UpsertDecisionZones<'_>,
) -> Result<(), StorageError> {
    let zones = row.zones_json.to_string();
    let raw = row.raw_json.to_string();
    client
        .execute(
            r#"
INSERT INTO decision_zones (
    document_id, provider, provider_decision_id, source_uid, ecli, status,
    fetched_at, expires_at, upstream_update_date, upstream_decision_date,
    text_hash, offset_unit, zones_json, raw_json, error
) VALUES (
    $1, $2, $3, $4, $5, $6,
    now(),
    CASE WHEN $7::bigint IS NULL THEN NULL ELSE now() + ($7::bigint * interval '1 second') END,
    $8, $9, $10, $11, $12::text::jsonb, $13::text::jsonb, $14
)
ON CONFLICT (document_id) DO UPDATE SET
    provider = EXCLUDED.provider,
    provider_decision_id = EXCLUDED.provider_decision_id,
    source_uid = EXCLUDED.source_uid,
    ecli = EXCLUDED.ecli,
    status = EXCLUDED.status,
    fetched_at = EXCLUDED.fetched_at,
    expires_at = EXCLUDED.expires_at,
    upstream_update_date = EXCLUDED.upstream_update_date,
    upstream_decision_date = EXCLUDED.upstream_decision_date,
    text_hash = EXCLUDED.text_hash,
    offset_unit = EXCLUDED.offset_unit,
    zones_json = EXCLUDED.zones_json,
    raw_json = EXCLUDED.raw_json,
    error = EXCLUDED.error
"#,
            &[
                &row.document_id,
                &row.provider,
                &row.provider_decision_id,
                &row.source_uid,
                &row.ecli,
                &row.status,
                &row.ttl_seconds,
                &row.upstream_update_date,
                &row.upstream_decision_date,
                &row.text_hash,
                &row.offset_unit,
                &zones,
                &raw,
                &row.error,
            ],
        )
        .map_err(StorageError::PostgresClient)?;

    // Refresh invalidation: if the (possibly just-updated) row is NOT derivable into zone units — any
    // non-`ok` status, or an `ok` row with no content hash — drop any already-materialized `zone_units`
    // for this decision so retrieval never serves official zones the cache has just invalidated
    // (zone_unit_embeddings cascade from zone_units). A fresh `ok`+hash row keeps its units; an `ok`
    // content change is handled by re-derivation via the text_hash/builder-version staleness check.
    if row.status != "ok" || row.text_hash.is_none() {
        client
            .execute(
                "DELETE FROM zone_units WHERE document_id = $1;",
                &[&row.document_id],
            )
            .map_err(StorageError::PostgresClient)?;
    }
    Ok(())
}
