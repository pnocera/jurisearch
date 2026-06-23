//! France-LEGI official-evidence qrel extraction from the built index.
//!
//! Builds gold for three retrieval categories directly from the index tables, with NO archive
//! re-parse and NO human/LLM in the labels — every label is a structural fact the legislator
//! authored (article identity, version validity windows, and CITATION links):
//!
//! - known-item: `documents` rows (query = citation, gold = document_id, as_of = valid_from).
//! - temporal: each article's own VERSIONS list, stored as `LIEN_ART` publisher edges carrying
//!   `debut`/`fin`/`num`/`etat`, resolved to documents — multi-version families with as-of windows.
//! - cross-reference: `LIEN typelien="CITATION" sens="cible"` publisher edges; target
//!   `to_source_uid` resolved to a corpus document. Query = the citing article text, gold = cited.
//!
//! `graph_edges.to_document_id` is unresolved (NULL) and `documents.version_group` is the
//! per-version LEGIARTI id (no shared chronicle key), so temporal and cross-reference resolve
//! targets via the edge `payload->>'to_source_uid'` joined to `documents.source_uid`.

use crate::runtime::{ManagedPostgres, StorageError};

/// Per-category caps on how many qrels to extract from the index.
#[derive(Debug, Clone, Copy)]
pub struct FranceLegiGoldLimits {
    pub known_item: u32,
    pub temporal: u32,
    pub cross_reference: u32,
}

impl Default for FranceLegiGoldLimits {
    fn default() -> Self {
        Self {
            known_item: 60,
            temporal: 12,
            cross_reference: 120,
        }
    }
}

/// Maximum characters of a citing article's text used as a cross-reference query.
const CROSS_REFERENCE_QUERY_CHARS: u32 = 2000;

/// Extract France-LEGI gold qrels from the open index as a JSON object with three arrays:
/// `{"known_item":[...],"temporal":[...],"cross_reference":[...]}`.
///
/// Each known-item entry is `{query, gold_document_id, as_of}`; each temporal entry is
/// `{query, as_of, gold_document_id}`; each cross-reference entry is
/// `{query_document_id, query, gold_document_ids}`.
pub fn france_legi_gold_json(
    postgres: &ManagedPostgres,
    limits: FranceLegiGoldLimits,
) -> Result<String, StorageError> {
    let known_item = postgres.execute_sql(&known_item_sql(limits.known_item))?;
    let temporal = postgres.execute_sql(&temporal_sql(limits.temporal))?;
    let cross_reference = postgres.execute_sql(&cross_reference_sql(limits.cross_reference))?;
    Ok(format!(
        "{{\"known_item\":{},\"temporal\":{},\"cross_reference\":{}}}",
        known_item.trim(),
        temporal.trim(),
        cross_reference.trim()
    ))
}

fn known_item_sql(limit: u32) -> String {
    format!(
        r#"
SELECT coalesce(jsonb_agg(jsonb_build_object(
    'query', d.citation,
    'gold_document_id', d.document_id,
    'as_of', d.valid_from::text
) ORDER BY d.document_id), '[]'::jsonb)
FROM (
    SELECT document_id, citation, valid_from
    FROM documents
    WHERE source = 'legi' AND kind = 'article'
      AND citation IS NOT NULL AND btrim(citation) <> ''
      AND valid_from IS NOT NULL
    ORDER BY document_id
    LIMIT {limit}
) d
"#
    )
}

fn cross_reference_sql(limit: u32) -> String {
    format!(
        r#"
WITH cite AS (
    SELECT
        e.from_document_id,
        e.payload->>'to_source_uid' AS target_source_uid
    FROM graph_edges e
    WHERE e.edge_source = 'publisher'
      AND e.from_document_id IS NOT NULL
      AND e.payload->>'to_source_uid' LIKE 'LEGIARTI%'
      AND EXISTS (
          SELECT 1 FROM jsonb_array_elements(coalesce(e.payload->'attributes', '[]'::jsonb)) a
          WHERE a->>'key' = 'typelien' AND a->>'value' = 'CITATION'
      )
      AND EXISTS (
          SELECT 1 FROM jsonb_array_elements(coalesce(e.payload->'attributes', '[]'::jsonb)) a
          WHERE a->>'key' = 'sens' AND a->>'value' = 'cible'
      )
),
resolved AS (
    SELECT DISTINCT c.from_document_id, td.document_id AS gold_document_id
    FROM cite c
    JOIN documents td
      ON td.source = 'legi' AND td.kind = 'article'
     AND td.source_uid = c.target_source_uid
    WHERE td.document_id <> c.from_document_id
),
grouped AS (
    SELECT from_document_id,
           jsonb_agg(gold_document_id ORDER BY gold_document_id) AS gold_document_ids
    FROM resolved
    GROUP BY from_document_id
    ORDER BY from_document_id
    LIMIT {limit}
)
SELECT coalesce(jsonb_agg(jsonb_build_object(
    'query_document_id', g.from_document_id,
    'query', left(regexp_replace(ch.contextualized_body, '\s+', ' ', 'g'), {CROSS_REFERENCE_QUERY_CHARS}),
    'gold_document_ids', g.gold_document_ids
) ORDER BY g.from_document_id), '[]'::jsonb)
FROM grouped g
JOIN LATERAL (
    SELECT contextualized_body
    FROM chunks
    WHERE document_id = g.from_document_id
    ORDER BY chunk_index
    LIMIT 1
) ch ON true
"#
    )
}

fn temporal_sql(limit: u32) -> String {
    format!(
        r#"
WITH version_edges AS (
    SELECT
        e.from_document_id,
        e.payload->>'to_source_uid' AS version_source_uid
    FROM graph_edges e
    CROSS JOIN LATERAL (
        SELECT jsonb_object_agg(a->>'key', a->>'value') AS attrs
        FROM jsonb_array_elements(coalesce(e.payload->'attributes', '[]'::jsonb)) a
    ) attrs
    WHERE e.edge_source = 'publisher'
      AND e.from_document_id IS NOT NULL
      AND e.payload->>'source_tag' = 'LIEN_ART'
      AND e.payload->>'to_source_uid' LIKE 'LEGIARTI%'
      AND attrs.attrs ?& ARRAY['debut', 'fin', 'num', 'etat']
),
resolved AS (
    SELECT
        v.from_document_id,
        fd.source_uid AS from_source_uid,
        d.document_id AS gold_document_id,
        d.source_uid AS gold_source_uid,
        d.citation,
        d.valid_from,
        d.valid_to
    FROM version_edges v
    JOIN documents fd ON fd.document_id = v.from_document_id
    JOIN documents d
      ON d.source = 'legi' AND d.kind = 'article'
     AND d.source_uid = v.version_source_uid
    -- only well-formed validity windows yield a usable as-of date
    WHERE d.valid_from IS NOT NULL
      AND (d.valid_to IS NULL OR d.valid_to > d.valid_from)
),
families AS (
    -- A real VERSIONS family has >=2 distinct resolved versions AND lists the seed article itself
    -- (so a stray non-VERSIONS LIEN_ART that happens to carry the four version attributes but
    -- points only at OTHER articles cannot masquerade as a version chronicle).
    SELECT from_document_id
    FROM resolved
    GROUP BY from_document_id
    HAVING count(DISTINCT gold_document_id) >= 2
       AND bool_or(gold_source_uid = from_source_uid)
),
family_keys AS (
    SELECT
        r.from_document_id,
        md5(string_agg(DISTINCT r.gold_document_id, ',' ORDER BY r.gold_document_id)) AS family_key
    FROM resolved r
    JOIN families f USING (from_document_id)
    GROUP BY r.from_document_id
),
chosen_families AS (
    SELECT DISTINCT ON (family_key) family_key, from_document_id
    FROM family_keys
    ORDER BY family_key, from_document_id
),
cases AS (
    SELECT DISTINCT
        r.gold_document_id,
        r.citation,
        r.valid_from,
        r.valid_to,
        CASE
            WHEN r.valid_to IS NOT NULL AND r.valid_to > r.valid_from + 2
                THEN (r.valid_from + ((r.valid_to - r.valid_from) / 2))
            ELSE r.valid_from
        END AS as_of
    FROM chosen_families cf
    JOIN resolved r USING (from_document_id)
    ORDER BY r.valid_from, r.gold_document_id
    LIMIT {limit}
)
SELECT coalesce(jsonb_agg(jsonb_build_object(
    'query', coalesce(c.citation, 'Article') || ' en vigueur au ' || c.as_of::text,
    'as_of', c.as_of::text,
    'gold_document_id', c.gold_document_id
) ORDER BY c.valid_from, c.gold_document_id), '[]'::jsonb)
FROM cases c
"#
    )
}
