//! Hybrid (BM25+dense RRF) candidate retrieval JSON.

use super::*;
use crate::query::ActiveCorpus;

/// Fail-closed embedding-fingerprint preflight (work/09 P3A): before any dense/hybrid retrieval,
/// require that the query's embedding fingerprint matches the **active generation's** fingerprint
/// (`corpus_state.embedding_fingerprint`). Today a mismatch is merely a SQL filter that returns zero
/// dense rows, so hybrid silently degrades to lexical and explicit-dense returns a false no-results;
/// this turns that into a clear error BEFORE retrieval. Single-corpus in 3A: no active corpus keeps the
/// legacy `public` path, and more than one active corpus is deferred to 3C (multi-corpus fan-out).
fn ensure_embedding_fingerprint_compatible(
    snapshot: &dyn ReadSnapshot,
    query: &HybridCandidateQuery<'_>,
) -> Result<(), StorageError> {
    if !query.retrieval_mode.uses_dense() {
        return Ok(());
    }
    match snapshot.active_corpora() {
        // No active corpus: the `public` producer/local working set — keep the legacy path (the
        // active-generation preflight is a site/client concern).
        [] => Ok(()),
        [active] => {
            if query.embedding_fingerprint == Some(active.fingerprint.as_str()) {
                Ok(())
            } else {
                Err(StorageError::Retrieval {
                    message: format!(
                        "embedding_fingerprint_mismatch: query fingerprint {:?} does not match the \
                         active generation fingerprint `{}`; refusing dense/hybrid retrieval \
                         (it would silently degrade to lexical or return false no-results)",
                        query.embedding_fingerprint, active.fingerprint
                    ),
                })
            }
        }
        // A query snapshot never opens over >1 corpus in 3B (`begin_snapshot` refuses it); kept total.
        _ => Err(StorageError::Retrieval {
            message: "multi-corpus embedding-fingerprint preflight is deferred to work/09 3C \
                      (multi-corpus fan-out)"
                .to_owned(),
        }),
    }
}

/// Hybrid (BM25⊕dense RRF) candidate retrieval through the request snapshot. A single active corpus (or
/// the `public` working set) runs the byte-identical legacy SQL; MORE than one active corpus fans out
/// over each physical generation and fuses in Rust (work/09 P3C). The single-corpus path is unchanged.
pub fn hybrid_candidates_in_snapshot(
    snapshot: &mut dyn ReadSnapshot,
    query: &HybridCandidateQuery<'_>,
) -> Result<String, StorageError> {
    let corpora = snapshot.active_corpora().to_vec();
    if corpora.len() > 1 {
        return hybrid_candidates_fanout(snapshot, &corpora, query);
    }
    // Single-corpus (0 or 1 active corpus): byte-identical to before 3C. A multi-corpus cursor here means
    // the active topology shrank from multi- to single-corpus mid-pagination — reject it (don't mis-page).
    if matches!(
        query.after_cursor,
        Some(RetrievalCursor::MultiCorpus { .. })
    ) {
        return Err(StorageError::Retrieval {
            message:
                "a multi-corpus cursor was supplied to a single-corpus search; the active topology \
                      changed — restart the search without a cursor"
                    .to_owned(),
        });
    }
    ensure_embedding_fingerprint_compatible(snapshot, query)?;
    let set_ivfflat_probes = resolve_ivfflat_probes(snapshot, query, "embedding")?;
    let sql = build_hybrid_sql(query, &set_ivfflat_probes)?;
    snapshot.read_text(&sql)
}

/// Resolve the `SET ivfflat.probes = N;\n\n` prefix for a dense/hybrid query (empty for BM25). The probe
/// count comes from the GLOBAL `index_manifest` — advisory tuning only (work/09 P3C: not a per-corpus
/// authority), so the fan-out resolves it once and reuses it for every arm.
fn resolve_ivfflat_probes(
    snapshot: &mut dyn ReadSnapshot,
    query: &HybridCandidateQuery<'_>,
    manifest_key: &str,
) -> Result<String, StorageError> {
    if query.retrieval_mode.uses_dense() {
        let stored_probes = manifest_default_probes(snapshot, manifest_key)?;
        let probes = effective_probes(&query.options, stored_probes);
        Ok(format!("SET ivfflat.probes = {probes};\n\n"))
    } else {
        Ok(String::new())
    }
}

/// Build the single-corpus hybrid candidate SQL. PURE (no snapshot): `set_ivfflat_probes` is the
/// already-resolved probe prefix. Reused verbatim by each fan-out arm (run against that arm's physical
/// generation via `read_text_for_corpus`), so the per-corpus result shape stays identical.
fn build_hybrid_sql(
    query: &HybridCandidateQuery<'_>,
    set_ivfflat_probes: &str,
) -> Result<String, StorageError> {
    let query_text = sql_string_literal(query.query_text);
    let as_of = sql_string_literal(query.as_of);
    let retrieval_mode = sql_string_literal(query.retrieval_mode.as_str());
    let kind_predicate = query
        .kind_filter
        .map(|kind| format!("AND d.kind = {}", sql_string_literal(kind)))
        .unwrap_or_default();
    // Decision-metadata filters are applied alongside the kind filter in every candidate CTE.
    let filter_predicate = format!("{kind_predicate}{}", query.decision_filters.predicate());
    let ranked_ctes = ranked_candidate_ctes(query, &query_text, &as_of, &filter_predicate)?;
    let limit = query.limit;

    // A2 gate: project `publication` ONLY when the authority re-rank needs it. Both fragments are the
    // empty string when OFF, so the emitted SQL is byte-identical to before this field existed (the
    // OFF-path invariant). Shared by the chunk and document branches so the two stay in lockstep.
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

    let sql = match query.group_by {
        GroupBy::Chunk => {
            let cursor_predicate = cursor_predicate(query.after_cursor);
            format!(
                r#"
{set_ivfflat_probes}WITH {ranked_ctes},
limited AS (
    SELECT
        r.chunk_id, c.document_id, d.source, d.kind, d.citation, d.title, d.source_url,
        d.valid_from::text AS valid_from, d.valid_to::text AS valid_to,
        left(regexp_replace(c.body, '\s+', ' ', 'g'), 280) AS snippet,{publication_select}
        r.lexical_rank, r.dense_rank, r.fused_score
    FROM ranked r
    JOIN chunks c ON c.chunk_id = r.chunk_id
    JOIN documents d ON d.document_id = c.document_id
    {cursor_predicate}
    ORDER BY round(r.fused_score::numeric, 8) DESC, r.chunk_id
    LIMIT {limit}
)
SELECT jsonb_build_object(
    'query', {query_text},
    'retrieval_mode', {retrieval_mode},
    'as_of', {as_of},
    'group_by', 'chunk',
    'limit', {limit},
    'candidates', COALESCE((
        SELECT jsonb_agg(jsonb_build_object(
            'chunk_id', chunk_id,
            'document_id', document_id,
            'source', source, 'kind', kind, 'citation', citation, 'title', title,
            'source_url', source_url, 'snippet', snippet,{publication_json}
            'validity', jsonb_build_object('from', valid_from, 'to', valid_to, 'to_exclusive', true),
            'scores', jsonb_build_object('rrf', round(fused_score::numeric, 8), 'lexical_rank', lexical_rank, 'dense_rank', dense_rank),
            'cursor', concat(round(fused_score::numeric, 8)::text, ':', chunk_id)
        ) ORDER BY round(fused_score::numeric, 8) DESC, chunk_id)
        FROM limited
    ), '[]'::jsonb)
)::text;
"#
            )
        }
        GroupBy::Document => {
            // Group BEFORE paging: pick each document's best chunk, then rank documents and apply the
            // document keyset cursor over that grouped rowset (never post-page dedupe).
            let cursor_predicate = document_cursor_predicate(query.after_cursor);
            format!(
                r#"
{set_ivfflat_probes}WITH {ranked_ctes},
scored AS (
    SELECT
        r.chunk_id, c.document_id, d.source, d.kind, d.citation, d.title, d.source_url,
        d.valid_from::text AS valid_from, d.valid_to::text AS valid_to,
        left(regexp_replace(c.body, '\s+', ' ', 'g'), 280) AS snippet,{publication_select}
        r.lexical_rank, r.dense_rank,
        round(r.fused_score::numeric, 8) AS cursor_score
    FROM ranked r
    JOIN chunks c ON c.chunk_id = r.chunk_id
    JOIN documents d ON d.document_id = c.document_id
),
best_document_chunks AS (
    SELECT DISTINCT ON (document_id) *
    FROM scored
    ORDER BY document_id, cursor_score DESC, chunk_id
),
limited AS (
    SELECT *
    FROM best_document_chunks
    {cursor_predicate}
    ORDER BY cursor_score DESC, document_id
    LIMIT {limit}
)
SELECT jsonb_build_object(
    'query', {query_text},
    'retrieval_mode', {retrieval_mode},
    'as_of', {as_of},
    'group_by', 'document',
    'limit', {limit},
    'candidates', COALESCE((
        SELECT jsonb_agg(jsonb_build_object(
            'document_id', document_id,
            'chunk_id', chunk_id,
            'best_chunk_id', chunk_id,
            'source', source, 'kind', kind, 'citation', citation, 'title', title,
            'source_url', source_url, 'snippet', snippet,{publication_json}
            'validity', jsonb_build_object('from', valid_from, 'to', valid_to, 'to_exclusive', true),
            'scores', jsonb_build_object('rrf', cursor_score, 'lexical_rank', lexical_rank, 'dense_rank', dense_rank),
            'cursor', concat('doc:', cursor_score::text, ':', document_id)
        ) ORDER BY cursor_score DESC, document_id)
        FROM limited
    ), '[]'::jsonb)
)::text;
"#
            )
        }
    };
    Ok(sql)
}

/// One candidate from one fan-out arm, tagged with its corpus and its cross-corpus RRF score (over its
/// rank within that arm). A stable id (`chunk_id`/`document_id`) belongs to exactly one corpus, so there
/// are no cross-corpus duplicates to merge.
struct FusedCandidate {
    corpus: String,
    id: String,
    cross: f64,
    candidate: serde_json::Value,
}

/// Multi-corpus fan-out: run the single-corpus SQL against EACH active physical generation (hitting its
/// BM25/IVFFlat indexes, never the union views), then fuse above the arms by RRF over each candidate's
/// rank WITHIN its arm (the per-arm `scores.rrf` is a within-corpus product and is NOT calibrated across
/// corpora). Order by `(cross_rrf desc, corpus, id)`; the multi-corpus cursor keysets that stream.
fn hybrid_candidates_fanout(
    snapshot: &mut dyn ReadSnapshot,
    corpora: &[ActiveCorpus],
    query: &HybridCandidateQuery<'_>,
) -> Result<String, StorageError> {
    // 1. Fail-closed fingerprint preflight across EVERY touched corpus (dense/hybrid only): the query is
    //    embedded under ONE fingerprint, so every active corpus must carry it, else no partial results.
    if query.retrieval_mode.uses_dense() {
        for corpus in corpora {
            if query.embedding_fingerprint != Some(corpus.fingerprint.as_str()) {
                return Err(StorageError::Retrieval {
                    message: format!(
                        "embedding_fingerprint_mismatch: corpus `{}` is at fingerprint `{}` but the \
                         query fingerprint is {:?}; multi-corpus dense/hybrid retrieval fails closed \
                         (every active corpus must share the query fingerprint)",
                        corpus.corpus, corpus.fingerprint, query.embedding_fingerprint
                    ),
                });
            }
        }
    }

    // 2. Cursor-aware per-arm fetch depth. Proof: the top-`page` of a k-way merge of rank-sorted lists is
    //    contained in the union of each list's top-`depth`; a cursor page needs ranks up to
    //    `cursor_rank + page` in the worst single-arm case.
    let page = query.limit.max(1);
    let cursor = MultiCorpusCursor::from_retrieval(query.after_cursor)?;
    let depth = match &cursor {
        Some(cursor) => implied_rank(cursor.score)
            .saturating_add(page)
            .saturating_add(1),
        None => page.saturating_add(1),
    };

    // 3. Dense-probe tuning is global/advisory — resolve once, reuse for every arm.
    let set_ivfflat_probes = resolve_ivfflat_probes(snapshot, query, "embedding")?;

    // 4. Per arm: the existing SQL with NO cursor + the deep limit, against the corpus's physical gen.
    let group = query.group_by;
    let mut fused: Vec<FusedCandidate> = Vec::new();
    for corpus in corpora {
        let arm_query = HybridCandidateQuery {
            after_cursor: None,
            limit: depth,
            ..*query
        };
        let sql = build_hybrid_sql(&arm_query, &set_ivfflat_probes)?;
        let response = snapshot.read_text_for_corpus(corpus, &sql)?;
        let parsed: serde_json::Value =
            serde_json::from_str(&response).map_err(StorageError::Json)?;
        let candidates = parsed["candidates"].as_array().cloned().unwrap_or_default();
        for (index, candidate) in candidates.into_iter().enumerate() {
            let rank = (index + 1) as f64;
            let cross = 1.0 / (RRF_K + rank);
            let Some(id) = candidate_id(&candidate, group) else {
                continue;
            };
            fused.push(FusedCandidate {
                corpus: corpus.corpus.clone(),
                id,
                cross,
                candidate,
            });
        }
    }

    // 5. Fuse: order by (cross desc, corpus, id) — a deterministic total order.
    fused.sort_by(|a, b| {
        b.cross
            .partial_cmp(&a.cross)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.corpus.cmp(&b.corpus))
            .then_with(|| a.id.cmp(&b.id))
    });

    // 6. Keyset past the cursor in that order, then take the page.
    if let Some(cursor) = &cursor {
        fused.retain(|candidate| cursor.precedes(candidate));
    }
    fused.truncate(page as usize);

    // 7. Re-shape each candidate: cross-corpus `scores.rrf` (authority rerank reads it), keep the within-
    //    corpus score under `scores.local_rrf`, add the `corpus`, and emit the multi-corpus `mc:` cursor.
    let candidates: Vec<serde_json::Value> = fused
        .into_iter()
        .map(|candidate| candidate.into_response_value(group))
        .collect();

    Ok(serde_json::json!({
        "query": query.query_text,
        "retrieval_mode": query.retrieval_mode.as_str(),
        "as_of": query.as_of,
        "group_by": group.as_str(),
        "limit": query.limit,
        "candidates": candidates,
    })
    .to_string())
}

impl FusedCandidate {
    /// Reshape the per-arm candidate into the multi-corpus response candidate: `scores.rrf` becomes the
    /// cross-corpus RRF score (rounded to 8 dp like the single-corpus path), the original per-arm score
    /// is preserved under `scores.local_rrf`, the owning `corpus` is added, and `cursor` becomes the
    /// `mc:<group>:<cross>:<corpus>:<id>` keyset cursor.
    fn into_response_value(self, group: GroupBy) -> serde_json::Value {
        let cross = round8(self.cross);
        let mut candidate = self.candidate;
        if let Some(scores) = candidate.get_mut("scores").and_then(|s| s.as_object_mut()) {
            let local = scores
                .get("rrf")
                .cloned()
                .unwrap_or(serde_json::Value::Null);
            scores.insert("local_rrf".to_owned(), local);
            scores.insert("rrf".to_owned(), serde_json::json!(cross));
        }
        if let Some(object) = candidate.as_object_mut() {
            object.insert("corpus".to_owned(), serde_json::json!(self.corpus));
            object.insert(
                "cursor".to_owned(),
                serde_json::json!(format!(
                    "mc:{}:{}:{}:{}",
                    group.as_str(),
                    cross,
                    self.corpus,
                    self.id
                )),
            );
        }
        candidate
    }
}

/// The id a candidate is keyed on for the given grouping (`chunk_id` for chunk, `document_id` for
/// document) — the cross-corpus tie-break + cursor identity.
fn candidate_id(candidate: &serde_json::Value, group: GroupBy) -> Option<String> {
    let key = match group {
        GroupBy::Chunk => "chunk_id",
        GroupBy::Document => "document_id",
    };
    candidate[key].as_str().map(str::to_owned)
}

/// Round to 8 decimal places, matching the single-corpus SQL's `round(…::numeric, 8)`.
fn round8(value: f64) -> f64 {
    (value * 1e8).round() / 1e8
}

/// The arm rank implied by a cross-corpus RRF cursor score (`cross = 1/(RRF_K + rank)` ⇒
/// `rank = 1/cross - RRF_K`), used to size the per-arm fetch depth for a cursor page.
fn implied_rank(score: f64) -> u32 {
    if score <= 0.0 {
        return 0;
    }
    let rank = (1.0 / score) - RRF_K;
    if rank.is_finite() && rank > 0.0 {
        rank.ceil() as u32
    } else {
        0
    }
}

/// A parsed multi-corpus keyset cursor (`mc:<group>:<cross_score>:<corpus>:<id>`).
struct MultiCorpusCursor {
    score: f64,
    corpus: String,
    id: String,
}

impl MultiCorpusCursor {
    /// Extract the multi-corpus cursor from a request's `after_cursor`. `None` (first page) is fine; a
    /// single-corpus chunk/document cursor passed to a multi-corpus request is a topology mismatch and is
    /// rejected (work/09 P3C).
    fn from_retrieval(
        after_cursor: Option<RetrievalCursor<'_>>,
    ) -> Result<Option<Self>, StorageError> {
        match after_cursor {
            None => Ok(None),
            Some(RetrievalCursor::MultiCorpus { score, corpus, id }) => {
                let score = score.parse::<f64>().map_err(|_| StorageError::Retrieval {
                    message: format!("malformed multi-corpus cursor score `{score}`"),
                })?;
                Ok(Some(Self {
                    score,
                    corpus: corpus.to_owned(),
                    id: id.to_owned(),
                }))
            }
            Some(_) => Err(StorageError::Retrieval {
                message: "a single-corpus cursor was supplied to a multi-corpus search; \
                          paginate a multi-corpus result with its `mc:` cursor"
                    .to_owned(),
            }),
        }
    }

    /// Whether `candidate` comes strictly AFTER this cursor in the total order (cross score DESC, then
    /// corpus ASC, then id ASC) — i.e. it belongs on a later page.
    fn precedes(&self, candidate: &FusedCandidate) -> bool {
        match round8(candidate.cross)
            .partial_cmp(&round8(self.score))
            .unwrap_or(std::cmp::Ordering::Equal)
        {
            std::cmp::Ordering::Greater => false, // higher score sorts earlier
            std::cmp::Ordering::Less => true,     // lower score sorts later
            std::cmp::Ordering::Equal => {
                (candidate.corpus.as_str(), candidate.id.as_str())
                    > (self.corpus.as_str(), self.id.as_str())
            }
        }
    }
}
