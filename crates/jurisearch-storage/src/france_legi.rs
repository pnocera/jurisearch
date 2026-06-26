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
    // Read role: LEGI gold is extracted from the served legislation documents (active generation on a
    // client), so it must follow the same resolved search path as retrieval, never stale `public`.
    let known_item = postgres.execute_read_sql(&known_item_sql(limits.known_item))?;
    let temporal = postgres.execute_read_sql(&temporal_sql(limits.temporal))?;
    let cross_reference =
        postgres.execute_read_sql(&cross_reference_sql(limits.cross_reference))?;
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
    // Deterministic pool of citing articles to resolve. Citation edges are plentiful; resolving
    // every one against `documents` before taking `limit` groups was wasteful. We pre-select the
    // first `seed_pool` citing articles (by document_id) and resolve only those. Some seeds may
    // have no target that resolves to a corpus article, so the pool is several times `limit`.
    let seed_pool = (limit as u64).saturating_mul(20).max(2000);
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
      -- JSONB containment (single C-level test, no jsonb_array_elements SRF per edge):
      -- the attributes array must hold an element with typelien=CITATION AND one with sens=cible.
      AND e.payload->'attributes' @> '[{{"key":"typelien","value":"CITATION"}},{{"key":"sens","value":"cible"}}]'::jsonb
),
cite_seeds AS (
    SELECT from_document_id
    FROM cite
    GROUP BY from_document_id
    ORDER BY from_document_id
    LIMIT {seed_pool}
),
resolved AS (
    SELECT DISTINCT c.from_document_id, td.document_id AS gold_document_id
    FROM cite c
    JOIN cite_seeds cs ON cs.from_document_id = c.from_document_id
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
    // Generous deterministic pool of seed articles to resolve fully. The corpus has ~4.18M
    // version edges; resolving and aggregating all of them to emit `limit` cases ran for minutes.
    // We instead pre-select the first `seed_pool` articles (by document_id) that carry >=2 version
    // edges, then run the expensive document joins / family aggregation only over that pool. The
    // exact family checks (>=2 distinct resolved versions + self-inclusion) still run downstream,
    // so a seed that turns out not to form a valid family is simply dropped.
    let seed_pool = (limit as u64).saturating_mul(200).max(4000);
    format!(
        r#"
WITH candidate_seeds AS (
    -- One scan of graph_edges: the first `seed_pool` articles (by document_id) that carry >=2
    -- version edges. A VERSIONS edge carries all four version attributes; JSONB containment (@>)
    -- checks key presence with one C-level test per edge, replacing a per-edge jsonb_object_agg
    -- over jsonb_array_elements. count(*) here is a loose pre-filter; the strict
    -- count(DISTINCT resolved version) >= 2 + self-inclusion check is in `families`.
    SELECT e.from_document_id
    FROM graph_edges e
    WHERE e.edge_source = 'publisher'
      AND e.from_document_id IS NOT NULL
      AND e.payload->>'source_tag' = 'LIEN_ART'
      AND e.payload->>'to_source_uid' LIKE 'LEGIARTI%'
      AND e.payload->'attributes' @> '[{{"key":"debut"}},{{"key":"fin"}},{{"key":"num"}},{{"key":"etat"}}]'::jsonb
    GROUP BY e.from_document_id
    HAVING count(*) >= 2
    ORDER BY e.from_document_id
    LIMIT {seed_pool}
),
resolved AS (
    -- Resolve only the seed articles' version edges. Joining graph_edges on from_document_id uses
    -- the graph_edges_from_idx index (seed_pool lookups), so this never re-scans the ~4.18M-edge
    -- corpus. Postgres cannot estimate @> selectivity (it guesses 1 row), which previously turned
    -- a CTE self-join over all version edges into a 4000 x 4.18M nested loop.
    SELECT
        cs.from_document_id,
        fd.source_uid AS from_source_uid,
        d.document_id AS gold_document_id,
        d.source_uid AS gold_source_uid,
        d.citation,
        d.valid_from,
        d.valid_to
    FROM candidate_seeds cs
    JOIN documents fd ON fd.document_id = cs.from_document_id
    JOIN graph_edges e
      ON e.from_document_id = cs.from_document_id
     AND e.edge_source = 'publisher'
     AND e.payload->>'source_tag' = 'LIEN_ART'
     AND e.payload->>'to_source_uid' LIKE 'LEGIARTI%'
     AND e.payload->'attributes' @> '[{{"key":"debut"}},{{"key":"fin"}},{{"key":"num"}},{{"key":"etat"}}]'::jsonb
    JOIN documents d
      ON d.source = 'legi' AND d.kind = 'article'
     AND d.source_uid = e.payload->>'to_source_uid'
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
