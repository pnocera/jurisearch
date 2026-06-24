//! France jurisprudence official-evidence qrel extraction from the built index.
//!
//! Builds gold for the Phase 2 benchmark directly from official indexed fields, with NO archive
//! re-parse and NO human/LLM in the labels — every label is a structural fact authored by the
//! publisher (the SOMMAIRE/headnote text, the ECLI, the pourvoi/NUMERO_AFFAIRE, the CETATEXT id):
//!
//! - judicial_retrieval (cass/capp/inca) and administrative_retrieval (jade): query = a cleaned
//!   excerpt of the decision's `decision_summary` chunk (the official headnote, NOT the title — titles
//!   are near-identifier strings), gold = the containing `documents.document_id`. Obvious document
//!   identifiers are stripped from the query so it stays a semantic-retrieval task, not a lookup.
//! - decision_citation.{ecli,pourvoi,cetatext}: real identifiers extracted from the corpus, shaped to
//!   what the production citation resolver accepts; gold = the decision document they belong to.
//!
//! Extraction is deterministic (ordered by `document_id`, bounded by per-category caps), so the
//! benchmark provenance can honestly record `sampled=false, human_in_gold=false, llm_in_gold=false`.

use crate::runtime::{ManagedPostgres, StorageError};

/// Per-category caps on how many qrels to extract from the index.
#[derive(Debug, Clone, Copy)]
pub struct FranceJurisGoldLimits {
    pub judicial_retrieval: u32,
    pub administrative_retrieval: u32,
    pub ecli: u32,
    pub pourvoi: u32,
    pub cetatext: u32,
}

impl Default for FranceJurisGoldLimits {
    fn default() -> Self {
        Self {
            judicial_retrieval: 60,
            administrative_retrieval: 60,
            ecli: 30,
            pourvoi: 30,
            cetatext: 30,
        }
    }
}

/// Maximum characters of the summary/headnote text used as a retrieval query.
const RETRIEVAL_QUERY_CHARS: u32 = 500;

/// Extract France-jurisprudence gold qrels from the open index as a JSON object:
/// `{"judicial_retrieval":[...],"administrative_retrieval":[...],
///   "decision_citation":{"ecli":[...],"pourvoi":[...],"cetatext":[...]}}`.
///
/// Each retrieval entry is `{query, gold_document_id, source}`; each citation entry is
/// `{query, gold_document_id}`.
pub fn france_juris_gold_json(
    postgres: &ManagedPostgres,
    limits: FranceJurisGoldLimits,
) -> Result<String, StorageError> {
    let judicial =
        postgres.execute_sql(&retrieval_sql("'cass','capp','inca'", limits.judicial_retrieval))?;
    let administrative =
        postgres.execute_sql(&retrieval_sql("'jade'", limits.administrative_retrieval))?;
    let ecli = postgres.execute_sql(&ecli_sql(limits.ecli))?;
    let pourvoi = postgres.execute_sql(&pourvoi_sql(limits.pourvoi))?;
    let cetatext = postgres.execute_sql(&cetatext_sql(limits.cetatext))?;
    Ok(format!(
        "{{\"judicial_retrieval\":{},\"administrative_retrieval\":{},\"decision_citation\":{{\"ecli\":{},\"pourvoi\":{},\"cetatext\":{}}}}}",
        judicial.trim(),
        administrative.trim(),
        ecli.trim(),
        pourvoi.trim(),
        cetatext.trim()
    ))
}

/// Known-item retrieval gold: query = the official headnote (decision_summary chunk) with obvious
/// document identifiers stripped, gold = the decision document. `sources_in` is a SQL IN-list literal.
fn retrieval_sql(sources_in: &str, limit: u32) -> String {
    format!(
        r#"
SELECT coalesce(jsonb_agg(jsonb_build_object(
    'query', query,
    'gold_document_id', document_id,
    'source', source
) ORDER BY document_id), '[]'::jsonb)
FROM (
    -- Filter on the CLEANED query length BEFORE the cap, so identifier-stripping a few early
    -- summaries to below the floor does not underfill the requested qrel count.
    SELECT document_id, source, query
    FROM (
        SELECT d.document_id, d.source,
               left(
                 btrim(regexp_replace(
                   -- strip unambiguous document identifiers so the query stays semantic, not a lookup
                   regexp_replace(c.body, '(ECLI:[A-Z]{{2}}:[A-Za-z0-9.:_-]+|JURITEXT[0-9]+|CETATEXT[0-9]+)', ' ', 'g'),
                   '\s+', ' ', 'g'
                 )),
                 {RETRIEVAL_QUERY_CHARS}
               ) AS query
        FROM documents d
        JOIN chunks c ON c.document_id = d.document_id
        WHERE d.kind = 'decision'
          AND d.source IN ({sources_in})
          AND c.chunk_kind = 'decision_summary'
          AND length(c.body) BETWEEN 120 AND 2000
    ) cleaned
    WHERE length(btrim(query)) >= 60
    ORDER BY document_id
    LIMIT {limit}
) q
"#
    )
}

/// ECLI citation gold: real `ECLI:FR:...`-shaped values, gold = their decision document.
fn ecli_sql(limit: u32) -> String {
    format!(
        r#"
SELECT coalesce(jsonb_agg(jsonb_build_object(
    'query', ecli,
    'gold_document_id', document_id
) ORDER BY document_id), '[]'::jsonb)
FROM (
    SELECT document_id, canonical_json->>'ecli' AS ecli
    FROM documents
    WHERE kind = 'decision'
      AND upper(canonical_json->>'ecli') ~ '^ECLI:FR:[A-Z0-9.:_-]+$'
    ORDER BY document_id
    LIMIT {limit}
) e
"#
    )
}

/// Pourvoi citation gold: Cassation case numbers accepted by the production parser
/// (`^[0-9]{{2}}-[0-9]{{4,6}}$` once dots/spaces are stripped); gold = their decision document.
fn pourvoi_sql(limit: u32) -> String {
    format!(
        r#"
SELECT coalesce(jsonb_agg(jsonb_build_object(
    'query', pourvoi,
    'gold_document_id', document_id
) ORDER BY document_id), '[]'::jsonb)
FROM (
    SELECT DISTINCT ON (d.document_id) d.document_id, cn AS pourvoi
    FROM documents d,
         jsonb_array_elements_text(coalesce(d.canonical_json->'case_numbers', '[]'::jsonb)) AS cn
    WHERE d.kind = 'decision'
      AND d.source = 'cass'
      AND replace(replace(cn, '.', ''), ' ', '') ~ '^[0-9]{{2}}-[0-9]{{4,6}}$'
    ORDER BY d.document_id, cn
    LIMIT {limit}
) p
"#
    )
}

/// CETATEXT citation gold: administrative decisions keyed by their CETATEXT source UID.
fn cetatext_sql(limit: u32) -> String {
    format!(
        r#"
SELECT coalesce(jsonb_agg(jsonb_build_object(
    'query', source_uid,
    'gold_document_id', document_id
) ORDER BY document_id), '[]'::jsonb)
FROM (
    SELECT document_id, source_uid
    FROM documents
    WHERE kind = 'decision'
      AND source = 'jade'
      AND source_uid ~ '^CETATEXT[0-9]{{12}}$'
    ORDER BY document_id
    LIMIT {limit}
) c
"#
    )
}

/// A deterministic, lightweight revision string for the exact combined corpus, suitable as the
/// benchmark `provenance.index_revision`. Hashes stable manifest facts (schema + embedding manifest,
/// per-source completed `source_version`s, and the document/chunk/embedding counts) — far cheaper than
/// a full replay snapshot, and distinct for a merged corpus (where the directory basename is not).
pub fn france_juris_index_revision(postgres: &ManagedPostgres) -> Result<String, StorageError> {
    let digest = postgres.execute_sql(
        r#"
SELECT md5(jsonb_build_object(
    'schema', (SELECT value FROM index_manifest WHERE key = 'schema'),
    'embedding', (SELECT value FROM index_manifest WHERE key = 'embedding'),
    'sources', (
        SELECT coalesce(jsonb_object_agg(source, source_version), '{}'::jsonb)
        FROM (
            SELECT source, max(manifest->>'source_version') AS source_version
            FROM ingest_run
            WHERE status = 'completed'
            GROUP BY source
        ) s
    ),
    'counts', jsonb_build_object(
        'documents', (SELECT count(*) FROM documents),
        'chunks', (SELECT count(*) FROM chunks),
        'embeddings', (SELECT count(*) FROM chunk_embeddings)
    )
)::text)
"#,
    )?;
    Ok(format!("phase2-juris:md5:{}", digest.trim()))
}
