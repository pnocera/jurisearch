//! Shared benchmark-runner scaffolding: the single-gold qrel scoring loop and the document-retrieval
//! `SearchRequest` builder reused by the France-LEGI / France-juris / zone benchmark families. Only
//! the per-qrel document RESOLVER and the HIT predicate vary between families; the hits/scored
//! bookkeeping and the request shape live here once. Artifact assembly stays in each family (each
//! artifact has its own gate contract).

use crate::*;

/// The hit-counting outcome of scoring one benchmark category's single-gold qrels.
pub(crate) struct QrelScore {
    /// hits / scored (recall@k or citation accuracy, depending on the caller's hit predicate); 0.0
    /// when no qrel was scored.
    pub(crate) metric: f64,
    /// Number of qrels actually scored (skipped qrels are not counted).
    pub(crate) queries: usize,
    /// Per-routing-backend query counts, accumulated from the resolver's optional backend label
    /// (empty when the resolver reports none). The France-LEGI gate audit consumes this; the
    /// France-juris / zone categories ignore it.
    pub(crate) backends: BTreeMap<String, usize>,
}

/// Score single-gold "known-item" qrels. For each qrel, `resolve` returns the ranked document ids
/// plus an optional routing-backend label, or `None` to SKIP the qrel (a required field — e.g.
/// `query`, `as_of` — is absent on this qrel); skipped qrels are not counted toward the metric.
/// `is_hit(ranked_docs, gold_id)` decides a hit (the caller chooses the ranking window, e.g.
/// `take(top_k).any` vs `any`). A qrel with no `gold_document_id` is skipped before `resolve` runs.
/// The metric is hits / scored.
pub(crate) fn score_known_item_qrels<R>(
    qrels: &Value,
    mut resolve: R,
    is_hit: impl Fn(&[String], &str) -> bool,
) -> Result<QrelScore, ErrorObject>
where
    R: FnMut(&Value) -> Result<Option<(Vec<String>, Option<String>)>, ErrorObject>,
{
    let mut hits = 0usize;
    let mut done = 0usize;
    let mut backends = BTreeMap::<String, usize>::new();
    for qrel in qrels.as_array().into_iter().flatten() {
        let Some(gold_id) = qrel["gold_document_id"].as_str() else {
            continue;
        };
        let Some((docs, backend)) = resolve(qrel)? else {
            continue;
        };
        if let Some(backend) = backend {
            *backends.entry(backend).or_default() += 1;
        }
        done += 1;
        if is_hit(&docs, gold_id) {
            hits += 1;
        }
    }
    Ok(QrelScore {
        metric: mean(hits, done),
        queries: done,
        backends,
    })
}

/// Build the `SearchRequest` the benchmark document-retrieval runners feed to `search_with_postgres`.
/// Only `kind`, `group_by`, `as_of`, and `top_k` differ between the France-LEGI (code/chunk) and
/// France-juris (decision/document) runners; everything else is the production hybrid default.
/// `index_dir` is `None` because the runners pass an already-open Postgres to `search_with_postgres`.
pub(crate) fn benchmark_search_request(
    query: &str,
    kind: CliKind,
    group_by: CliGroupBy,
    as_of: Option<String>,
    top_k: u32,
) -> SearchRequest {
    SearchRequest {
        query: query.to_owned(),
        kind,
        mode: CliSearchMode::Hybrid,
        format: CliOutputFormat::Concise,
        group_by,
        top_k,
        cursor: None,
        as_of,
        rrf_lexical_weight: None,
        rrf_dense_weight: None,
        probes: None,
        court: None,
        formation: None,
        publication: None,
        decided_from: None,
        decided_to: None,
        zone: None,
        index_dir: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scores_hits_over_scored_and_skips_qrels_missing_required_fields() {
        let qrels = json!([
            { "query": "q1", "gold_document_id": "A" }, // scored, hit
            { "query": "q2", "gold_document_id": "B" }, // scored, miss
            { "gold_document_id": "C" },                // skipped: resolver returns None (no query)
            { "query": "q4" },                          // skipped: no gold_document_id
        ]);
        let score = score_known_item_qrels(
            &qrels,
            |qrel| {
                let Some(query) = qrel["query"].as_str() else {
                    return Ok(None);
                };
                let docs = if query == "q1" {
                    vec!["A".to_owned()]
                } else {
                    vec!["other".to_owned()]
                };
                Ok(Some((docs, None)))
            },
            |docs, gold_id| docs.iter().any(|doc| doc == gold_id),
        )
        .expect("scoring");
        assert_eq!(score.queries, 2);
        assert!((score.metric - 0.5).abs() < 1e-9);
        assert!(score.backends.is_empty());
    }

    #[test]
    fn accumulates_backends_and_honours_the_hit_predicate_window() {
        let qrels = json!([
            { "query": "q1", "gold_document_id": "A" },
            { "query": "q2", "gold_document_id": "B" },
        ]);
        let score = score_known_item_qrels(
            &qrels,
            |qrel| {
                let query = qrel["query"].as_str().unwrap();
                // q1 ranks gold at position 1; q2 ranks gold at position 3.
                let (docs, backend) = if query == "q1" {
                    (vec!["A".to_owned(), "z".to_owned(), "z".to_owned()], "structured")
                } else {
                    (vec!["z".to_owned(), "z".to_owned(), "B".to_owned()], "hybrid")
                };
                Ok(Some((docs, Some(backend.to_owned()))))
            },
            // top-1 window: only q1's rank-1 gold counts.
            |docs, gold_id| docs.iter().take(1).any(|doc| doc == gold_id),
        )
        .expect("scoring");
        assert_eq!(score.queries, 2);
        assert!((score.metric - 0.5).abs() < 1e-9);
        assert_eq!(score.backends.get("structured"), Some(&1));
        assert_eq!(score.backends.get("hybrid"), Some(&1));
    }

    #[test]
    fn empty_qrels_yield_zero_metric_and_zero_queries() {
        let score = score_known_item_qrels(
            &json!([]),
            |_qrel| Ok(Some((Vec::new(), None))),
            |_docs, _gold| true,
        )
        .expect("scoring");
        assert_eq!(score.queries, 0);
        assert_eq!(score.metric, 0.0);
        assert!(score.backends.is_empty());
    }

    #[test]
    fn a_resolver_error_propagates() {
        let qrels = json!([{ "query": "q1", "gold_document_id": "A" }]);
        let result = score_known_item_qrels(
            &qrels,
            |_qrel| Err(ErrorObject::bad_input("boom")),
            |_docs, _gold| true,
        );
        assert!(result.is_err());
    }
}
