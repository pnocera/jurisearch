//! Option B parallel zone retrieval query path.
//!
//! `zone_candidates_json` is the zone-scoped sibling of `retrieval::hybrid_candidates_json`: it runs the
//! same RRF(BM25⊕dense)/probes machinery, but over the official `zone_units` / `zone_unit_embeddings` /
//! `zone_units_bm25_idx` tables filtered to a single zone, grouped to one best fragment per decision.
//! It is a SEPARATE query builder (Option B isolation) reusing the shared `pub(crate)` helpers from
//! `retrieval` — it never touches the whole-decision `chunks` path. Every candidate is official
//! (`zone_accurate=true`, provider `judilibre`) for a resolver-reachable Cour de cassation decision.
//!
//! All scope predicates (zone, decision filters, as-of validity) are pushed INTO each candidate arm
//! before ranking and limiting, so an out-of-scope high scorer can never consume a pool slot and starve
//! an in-scope hit (and the dense ANN pool is selected from the requested zone, not globally).

use crate::query::{QueryStore, ReadSnapshot};
use crate::retrieval::{
    DecisionFilters, RRF_K, RetrievalCursor, RetrievalMode, RetrievalOptions,
    document_cursor_predicate, effective_probes, effective_rrf_weights, format_sql_f64,
    manifest_default_probes,
};
use crate::runtime::{ManagedPostgres, StorageError, sql_string_literal};

/// Dense candidates are selected from the requested zone+filter scope, so a modest over-fetch suffices
/// to absorb the approximate-ANN/grouping slack before ranking.
const ZONE_DENSE_OVERFETCH_FACTOR: u32 = 4;

#[derive(Debug, Clone, Copy)]
pub struct ZoneCandidateQuery<'a> {
    pub query_text: &'a str,
    pub query_embedding: Option<&'a str>,
    pub embedding_fingerprint: Option<&'a str>,
    pub retrieval_mode: RetrievalMode,
    pub options: RetrievalOptions,
    pub after_cursor: Option<RetrievalCursor<'a>>,
    /// The official zone to scope to: `motivations` | `moyens` | `dispositif`.
    pub zone: &'a str,
    /// As-of validity anchor (ISO `YYYY-MM-DD`): only decisions in force on that date match (for
    /// decisions, `valid_from = decision_date`, `valid_to = NULL`, so this bounds the decision date).
    pub as_of: &'a str,
    /// Court/formation/publication/decision-date filters (reused verbatim from the main path).
    pub decision_filters: DecisionFilters<'a>,
    /// Gate (A2): when `true`, project `canonical_json->>'publication'` into the candidate JSON for the
    /// authority re-rank (A4). When `false` the emitted SQL/payload are byte-identical to before.
    pub project_authority: bool,
    pub lexical_limit: u32,
    pub dense_limit: u32,
    pub limit: u32,
}

fn zone_dense_inputs<'a>(
    query: &'a ZoneCandidateQuery<'a>,
) -> Result<(&'a str, &'a str), StorageError> {
    let embedding = query
        .query_embedding
        .ok_or_else(|| StorageError::Retrieval {
            message: format!(
                "{} zone retrieval requires a query embedding",
                query.retrieval_mode.as_str()
            ),
        })?;
    let fingerprint = query
        .embedding_fingerprint
        .ok_or_else(|| StorageError::Retrieval {
            message: format!(
                "{} zone retrieval requires an embedding fingerprint",
                query.retrieval_mode.as_str()
            ),
        })?;
    Ok((embedding, fingerprint))
}

/// The as-of validity predicate against `documents d` (decisions: `valid_to` is NULL, so this bounds the
/// decision date `valid_from <= as_of`). Mirrors the main path's temporal predicate.
fn as_of_predicate(as_of: &str) -> String {
    let as_of = sql_string_literal(as_of);
    format!(
        " AND (d.valid_from IS NULL OR d.valid_from <= {as_of}::date) AND (d.valid_to IS NULL OR d.valid_to > {as_of}::date)"
    )
}

/// Build the `ranked` CTE(s) producing `(zone_unit_id, lexical_rank, dense_rank, fused_score)` for the
/// requested mode. `zone` and `doc_scope` (decision filters + as-of, each clause prefixed with ` AND`,
/// against `documents d`) are applied INSIDE each arm before ranking/limiting.
fn ranked_zone_ctes(
    query: &ZoneCandidateQuery<'_>,
    query_text: &str,
    zone: &str,
    doc_scope: &str,
) -> Result<String, StorageError> {
    let lexical_limit = query.lexical_limit.max(1);
    let dense_limit = query.dense_limit.max(1);
    let lexical_cte = format!(
        r#"
lexical AS (
    SELECT
        u.zone_unit_id,
        row_number() OVER (ORDER BY paradedb.score(u.zone_unit_id) DESC, u.zone_unit_id) AS lexical_rank
    FROM zone_units u
    JOIN documents d ON d.document_id = u.document_id
    WHERE u.search_body @@@ {query_text} AND u.zone = {zone}{doc_scope}
    ORDER BY paradedb.score(u.zone_unit_id) DESC, u.zone_unit_id
    LIMIT {lexical_limit}
)"#
    );
    match query.retrieval_mode {
        RetrievalMode::Bm25 => Ok(format!(
            r#"{lexical_cte},
ranked AS (
    SELECT
        l.zone_unit_id,
        l.lexical_rank,
        NULL::bigint AS dense_rank,
        1.0 / (60.0 + l.lexical_rank) AS fused_score
    FROM lexical l
)"#
        )),
        RetrievalMode::Dense | RetrievalMode::Hybrid => {
            let (embedding, fingerprint) = zone_dense_inputs(query)?;
            let embedding = sql_string_literal(embedding);
            let fingerprint = sql_string_literal(fingerprint);
            let dense_pool_limit = dense_limit
                .saturating_mul(ZONE_DENSE_OVERFETCH_FACTOR)
                .max(1);
            // The dense ANN pool is selected from the requested zone + decision-filter + as-of scope, so
            // wrong-zone / out-of-filter units never consume the pool before in-scope hits.
            let dense_ctes = format!(
                r#"
dense_pool AS (
    SELECT
        scored.zone_unit_id,
        row_number() OVER (ORDER BY scored.distance) AS dense_rank
    FROM (
        SELECT e.zone_unit_id, e.embedding <-> {embedding}::vector AS distance
        FROM zone_unit_embeddings e
        JOIN zone_units u ON u.zone_unit_id = e.zone_unit_id
        JOIN documents d ON d.document_id = u.document_id
        WHERE e.embedding_fingerprint = {fingerprint} AND u.zone = {zone}{doc_scope}
        ORDER BY distance
        LIMIT {dense_pool_limit}
    ) scored
    ORDER BY scored.distance
    LIMIT {dense_pool_limit}
),
dense AS (
    SELECT
        dp.zone_unit_id,
        row_number() OVER (ORDER BY dp.dense_rank, dp.zone_unit_id) AS dense_rank
    FROM dense_pool dp
    ORDER BY dp.dense_rank, dp.zone_unit_id
    LIMIT {dense_limit}
)"#
            );
            if matches!(query.retrieval_mode, RetrievalMode::Dense) {
                Ok(format!(
                    r#"{dense_ctes},
ranked AS (
    SELECT
        d.zone_unit_id,
        NULL::bigint AS lexical_rank,
        d.dense_rank,
        1.0 / (60.0 + d.dense_rank) AS fused_score
    FROM dense d
)"#
                ))
            } else {
                let (lexical_weight, dense_weight) = effective_rrf_weights(&query.options);
                Ok(format!(
                    r#"{lexical_cte},{dense_ctes},
fused AS (
    SELECT
        zone_unit_id,
        min(lexical_rank) AS lexical_rank,
        min(dense_rank) AS dense_rank
    FROM (
        SELECT zone_unit_id, lexical_rank, NULL::bigint AS dense_rank FROM lexical
        UNION ALL
        SELECT zone_unit_id, NULL::bigint AS lexical_rank, dense_rank FROM dense
    ) ranks
    GROUP BY zone_unit_id
),
ranked AS (
    SELECT
        f.zone_unit_id,
        f.lexical_rank,
        f.dense_rank,
        (
            CASE WHEN f.lexical_rank IS NULL THEN 0.0 ELSE {lexical_weight} / ({rrf_k} + f.lexical_rank) END
            + CASE WHEN f.dense_rank IS NULL THEN 0.0 ELSE {dense_weight} / ({rrf_k} + f.dense_rank) END
        ) AS fused_score
    FROM fused f
)"#,
                    rrf_k = format_sql_f64(RRF_K),
                    lexical_weight = format_sql_f64(lexical_weight),
                    dense_weight = format_sql_f64(dense_weight),
                ))
            }
        }
    }
}

/// Zone-scoped hybrid retrieval: one best official-zone fragment per decision, ranked within `zone`.
/// Returns the same candidate JSON shape as the main path plus `zone`/`zone_accurate`/`provider`, with
/// a document keyset cursor (`doc:<score>:<document_id>`) shared with `GroupBy::Document`.
/// Legacy one-shot wrapper over [`zone_candidates_in_snapshot`]: open a read snapshot and delegate (for
/// deferred callers that hold a [`ManagedPostgres`]). The `search --zone` path uses the snapshot core.
pub fn zone_candidates_json(
    postgres: &ManagedPostgres,
    query: &ZoneCandidateQuery<'_>,
) -> Result<String, StorageError> {
    let mut snapshot = postgres.begin_snapshot()?;
    zone_candidates_in_snapshot(&mut *snapshot, query)
}

pub fn zone_candidates_in_snapshot(
    snapshot: &mut dyn ReadSnapshot,
    query: &ZoneCandidateQuery<'_>,
) -> Result<String, StorageError> {
    let query_text = sql_string_literal(query.query_text);
    let zone_literal = sql_string_literal(query.zone);
    let retrieval_mode = sql_string_literal(query.retrieval_mode.as_str());
    let as_of = sql_string_literal(query.as_of);
    // Scope applied inside each arm: decision filters (` AND d.…`) + as-of validity. Zone units are all
    // decisions, so the predicate's `d.kind = 'decision'` is a no-op.
    let doc_scope = format!(
        "{}{}",
        query.decision_filters.predicate(),
        as_of_predicate(query.as_of)
    );
    let ranked_ctes = ranked_zone_ctes(query, &query_text, &zone_literal, &doc_scope)?;
    let set_ivfflat_probes = if query.retrieval_mode.uses_dense() {
        let stored_probes = manifest_default_probes(snapshot, "zone_embedding")?;
        format!(
            "SET ivfflat.probes = {};\n\n",
            effective_probes(&query.options, stored_probes)
        )
    } else {
        String::new()
    };
    let cursor_predicate = document_cursor_predicate(query.after_cursor);
    let limit = query.limit;

    // A2 gate: mirror the main path — project `publication` ONLY for the authority re-rank; both
    // fragments are empty when OFF so the emitted zone SQL/payload are byte-identical to before.
    let publication_select = if query.project_authority {
        "\n        d.canonical_json->>'publication' AS publication,"
    } else {
        ""
    };
    let publication_json = if query.project_authority {
        "\n            'publication', publication,"
    } else {
        ""
    };

    let sql = format!(
        r#"
{set_ivfflat_probes}WITH {ranked_ctes},
scored AS (
    SELECT
        r.zone_unit_id, u.document_id, u.zone, d.source AS doc_source,
        d.citation, d.title, d.source_url,
        d.valid_from::text AS valid_from, d.valid_to::text AS valid_to,
        left(regexp_replace(u.body, '\s+', ' ', 'g'), 280) AS snippet,{publication_select}
        r.lexical_rank, r.dense_rank,
        round(r.fused_score::numeric, 8) AS cursor_score
    FROM ranked r
    JOIN zone_units u ON u.zone_unit_id = r.zone_unit_id
    JOIN documents d ON d.document_id = u.document_id
),
best_document AS (
    SELECT DISTINCT ON (document_id) *
    FROM scored
    ORDER BY document_id, cursor_score DESC, zone_unit_id
),
limited AS (
    SELECT *
    FROM best_document
    {cursor_predicate}
    ORDER BY cursor_score DESC, document_id
    LIMIT {limit}
)
SELECT jsonb_build_object(
    'query', {query_text},
    'retrieval_mode', {retrieval_mode},
    'as_of', {as_of},
    'group_by', 'document',
    'zone', {zone_literal},
    'limit', {limit},
    'candidates', COALESCE((
        SELECT jsonb_agg(jsonb_build_object(
            'document_id', document_id,
            'zone_unit_id', zone_unit_id,
            'best_chunk_id', zone_unit_id,
            'source', doc_source, 'kind', 'decision', 'citation', citation, 'title', title,
            'source_url', source_url, 'snippet', snippet,{publication_json}
            'zone', zone, 'zone_accurate', true, 'provider', 'judilibre',
            'validity', jsonb_build_object('from', valid_from, 'to', valid_to, 'to_exclusive', true),
            'scores', jsonb_build_object('rrf', cursor_score, 'lexical_rank', lexical_rank, 'dense_rank', dense_rank),
            'cursor', concat('doc:', cursor_score::text, ':', document_id)
        ) ORDER BY cursor_score DESC, document_id)
        FROM limited
    ), '[]'::jsonb)
)::text;
"#
    );
    snapshot.read_text(&sql)
}
