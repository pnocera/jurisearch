//! Measured-only France-juris AUTHORITY benchmark: pairwise authority-lift + recall@10 OFF/ON sweep.
//!
//! Honest, publisher-label-only ordering signal (no LLM, no human): for a sample of decisions it reuses
//! the same official decision-headnote gold recipe as `eval france-juris`, then measures whether the
//! authority re-rank moves the higher-authority of two NEAR-TIED, SAME-ORDER decisions above the lower
//! one (pairwise authority-lift, ON minus OFF), per order AND per source-pair, plus recall@10 OFF vs ON,
//! swept over `--authority-weights`. Optionally (`--include-zones`) folds the official Cassation-zone
//! recall OFF/ON into the regression guard. MEASURED-ONLY: emits a SEPARATE `phase2_authority_benchmark`
//! artifact (`gate_input: false`) that is NOT a Phase 2 gate input and never inflates the corpus claim.

use std::collections::{BTreeMap, HashMap};

use crate::*;

/// The benchmark-only widened window (unique decisions per query) used to FORM pairs. This is not the
/// production OFF path: it is fetched with `project_authority=true` and is left UN-reranked so the
/// metric can derive both the natural (OFF) order and the re-ranked (ON) order from the same window.
const AUTHORITY_BENCHMARK_WINDOW: u32 = 40;

/// Recall@10 is recomputed with the SAME recipe/grouping as `eval france-juris` (document grouping,
/// overfetch 40, take 10), so the regression guard compares like with like against the Phase 2 gate.
const AUTHORITY_RECALL_OVERFETCH: u32 = 40;

pub(crate) fn eval_france_juris_authority_payload(
    args: EvalFranceJurisAuthorityArgs,
    index_dir: Option<&Path>,
) -> Result<Value, ErrorObject> {
    let weights = parse_authority_weights(&args.authority_weights)?;
    let index_dir = require_existing_index_dir(index_dir)?;
    let postgres = open_index(index_dir.as_path())?;
    ensure_query_readiness(&postgres, QueryReadinessGate::Search)?;

    // Reuse the france-juris retrieval gold ONLY (no citation gold needed): official decision headnotes.
    let limits = FranceJurisGoldLimits {
        judicial_retrieval: args.judicial_retrieval,
        administrative_retrieval: args.administrative_retrieval,
        ecli: 0,
        pourvoi: 0,
        cetatext: 0,
    };
    let gold_json = france_juris_gold_json(&postgres, limits).map_err(storage_error_object)?;
    let gold: Value = serde_json::from_str(&gold_json)
        .map_err(|error| dependency_unavailable(error.to_string()))?;

    let embedder = PreparedQueryEmbedder::from_env()?;
    let judicial = authority_category(&postgres, &embedder, &gold["judicial_retrieval"], &weights)?;
    let administrative = authority_category(
        &postgres,
        &embedder,
        &gold["administrative_retrieval"],
        &weights,
    )?;

    // Optional official-zone recall OFF/ON, folded into the regression guard (no absolute floor — zones
    // are a measured-only overlay; the guard only requires no regression vs OFF).
    let zones = if args.include_zones {
        Some(authority_zone_recall(
            &postgres,
            &embedder,
            args.zone_mode,
            &weights,
        )?)
    } else {
        None
    };

    let index_revision = france_juris_index_revision(&postgres).map_err(storage_error_object)?;
    let source_revision = args
        .source_revision
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| format!("index:{index_revision}"));

    Ok(authority_benchmark_artifact(
        &judicial,
        &administrative,
        zones.as_ref(),
        &weights,
        args.judicial_retrieval,
        args.administrative_retrieval,
        &index_revision,
        &source_revision,
    ))
}

/// Parse the comma-separated `--authority-weights` sweep; each must be finite and in `[0.0, 1.0]`.
/// Duplicates are dropped (order preserved). `0.0` is a valid OFF baseline column.
fn parse_authority_weights(spec: &str) -> Result<Vec<f64>, ErrorObject> {
    let mut weights = Vec::new();
    for token in spec
        .split(',')
        .map(str::trim)
        .filter(|token| !token.is_empty())
    {
        let weight: f64 = token.parse().map_err(|_| {
            ErrorObject::bad_input(format!(
                "invalid authority weight `{token}` (expected a number)"
            ))
        })?;
        if !weight.is_finite() || !(0.0..=1.0).contains(&weight) {
            return Err(ErrorObject::bad_input(format!(
                "authority weight `{token}` must be a finite value in [0.0, 1.0]"
            )));
        }
        if !weights.contains(&weight) {
            weights.push(weight);
        }
    }
    if weights.is_empty() {
        return Err(ErrorObject::bad_input(
            "--authority-weights must list at least one weight in [0.0, 1.0]",
        ));
    }
    Ok(weights)
}

/// One authority pair within a single query's window: two near-tied, same-order decisions of different
/// tiers. `higher_*` is the higher-AUTHORITY side; `*_natural_idx` is its position in the OFF window.
struct AuthorityPair {
    higher_doc: String,
    lower_doc: String,
    higher_natural_idx: usize,
    lower_natural_idx: usize,
    source_pair: String,
    gap: f64,
}

/// Pair counts for one source-pair key (e.g. `cass+oui>inca`), or aggregated for a whole order: the
/// total pair count, how many the higher-authority side led in the OFF order, and the same per weight ON.
#[derive(Clone)]
struct PairStats {
    total: usize,
    off_above: usize,
    on_above: Vec<usize>,
}

impl PairStats {
    fn new(weights: usize) -> Self {
        Self {
            total: 0,
            off_above: 0,
            on_above: vec![0; weights],
        }
    }
}

/// Recall@10 OFF + per-weight ON hit counts over a set of scored qrels.
#[derive(Clone)]
struct RecallStats {
    scored: usize,
    off_hits: usize,
    on_hits: Vec<usize>,
}

impl RecallStats {
    fn new(weights: usize) -> Self {
        Self {
            scored: 0,
            off_hits: 0,
            on_hits: vec![0; weights],
        }
    }
    fn recall_off(&self) -> f64 {
        mean(self.off_hits, self.scored)
    }
    fn recall_on(&self, index: usize) -> f64 {
        mean(self.on_hits[index], self.scored)
    }
}

/// The weight-independent baseline + per-weight ON results for one legal order.
struct AuthorityCategory {
    queries: usize,
    weights: Vec<f64>,
    gaps: Vec<f64>,
    /// Per source-pair stats; the category aggregate is the sum across keys.
    source_pairs: BTreeMap<String, PairStats>,
    recall: RecallStats,
}

impl AuthorityCategory {
    fn aggregate(&self) -> PairStats {
        let mut total = PairStats::new(self.weights.len());
        for stats in self.source_pairs.values() {
            total.total += stats.total;
            total.off_above += stats.off_above;
            for (index, count) in stats.on_above.iter().enumerate() {
                total.on_above[index] += count;
            }
        }
        total
    }
}

fn candidate_tier(candidate: &Value) -> Option<AuthorityTier> {
    let source = candidate["source"].as_str()?;
    authority_tier(source, candidate["publication"].as_str())
}

fn candidate_rrf(candidate: &Value) -> f64 {
    candidate["scores"]["rrf"].as_f64().unwrap_or(0.0)
}

fn candidate_doc(candidate: &Value) -> String {
    candidate["document_id"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

/// The structural source-pair label `higher>lower`, where each side is `source` refined by the marker
/// that set its tier (so e.g. `cass+oui` vs `cass` is visible). Publisher-authored, no invented label.
fn source_pair_label(candidate: &Value, tier: &AuthorityTier) -> String {
    let source = candidate["source"].as_str().unwrap_or_default();
    match (source, candidate["publication"].as_str()) {
        ("cass", Some(publication)) if publication.eq_ignore_ascii_case("oui") => {
            "cass+oui".to_owned()
        }
        ("jade", _) => format!("jade+{}", tier.tier),
        _ => source.to_owned(),
    }
}

fn round6(value: f64) -> f64 {
    (value * 1e6).round() / 1e6
}

/// Form the valid authority pairs in one OFF window (natural relevance order, sorted by rrf desc). A
/// pair counts ONLY if (design §7.2): same order, both tiered (known source), different tiers, neither
/// `marker_absent`, and inside the §3.4 relevance band of the more-relevant member.
fn window_pairs(candidates: &[Value], band: f64) -> Vec<AuthorityPair> {
    let mut pairs = Vec::new();
    for i in 0..candidates.len() {
        let Some(ti) = candidate_tier(&candidates[i]) else {
            continue;
        };
        if ti.marker_absent {
            continue;
        }
        let si = candidate_rrf(&candidates[i]);
        for j in (i + 1)..candidates.len() {
            let Some(tj) = candidate_tier(&candidates[j]) else {
                continue;
            };
            if tj.marker_absent || ti.order != tj.order || ti.tier == tj.tier {
                continue;
            }
            let sj = candidate_rrf(&candidates[j]);
            // i is the more-relevant member (window is rrf desc), so it is the band leader.
            let gap = si - sj;
            if !(si > 0.0 && gap <= band * si) {
                continue;
            }
            // The higher-AUTHORITY side may be either the more-relevant (i) or the less-relevant (j).
            let ((higher, ht), (lower, lt)) = if ti.tier > tj.tier {
                ((i, &ti), (j, &tj))
            } else {
                ((j, &tj), (i, &ti))
            };
            pairs.push(AuthorityPair {
                higher_doc: candidate_doc(&candidates[higher]),
                lower_doc: candidate_doc(&candidates[lower]),
                higher_natural_idx: higher,
                lower_natural_idx: lower,
                source_pair: format!(
                    "{}>{}",
                    source_pair_label(&candidates[higher], ht),
                    source_pair_label(&candidates[lower], lt)
                ),
                gap,
            });
        }
    }
    pairs
}

/// Score one legal order: iterate the order's gold qrels, fetch each query's OFF window, form pairs, and
/// accumulate OFF/ON lift (per source-pair) + recall@10 OFF/ON for every swept weight.
fn authority_category(
    postgres: &ManagedPostgres,
    embedder: &PreparedQueryEmbedder,
    qrels: &Value,
    weights: &[f64],
) -> Result<AuthorityCategory, ErrorObject> {
    let band = AUTHORITY_DEFAULT_BAND;
    let mut queries = 0usize;
    let mut gaps = Vec::new();
    let mut source_pairs = BTreeMap::<String, PairStats>::new();
    let mut recall = RecallStats::new(weights.len());

    for qrel in qrels.as_array().into_iter().flatten() {
        let Some(query) = qrel["query"].as_str() else {
            continue;
        };
        let Some(gold) = qrel["gold_document_id"].as_str() else {
            continue;
        };
        queries += 1;

        // Recall@10 OFF vs ON through the production search (same recipe as the Phase 2 gate).
        recall.scored += 1;
        let off_docs = authority_recall_documents(postgres, embedder, query, None)?;
        if off_docs.iter().take(10).any(|doc| doc == gold) {
            recall.off_hits += 1;
        }
        for (index, &weight) in weights.iter().enumerate() {
            let on_docs = if weight <= 0.0 {
                off_docs.clone()
            } else {
                authority_recall_documents(postgres, embedder, query, Some(weight))?
            };
            if on_docs.iter().take(10).any(|doc| doc == gold) {
                recall.on_hits[index] += 1;
            }
        }

        // Pairwise authority-lift over the benchmark window, accumulated per source-pair.
        let window = authority_benchmark_window(postgres, embedder, query)?;
        let pairs = window_pairs(&window, band);
        for pair in &pairs {
            gaps.push(pair.gap);
            let stats = source_pairs
                .entry(pair.source_pair.clone())
                .or_insert_with(|| PairStats::new(weights.len()));
            stats.total += 1;
            if pair.higher_natural_idx < pair.lower_natural_idx {
                stats.off_above += 1;
            }
        }
        if !pairs.is_empty() {
            for (index, &weight) in weights.iter().enumerate() {
                let mut reranked = window.clone();
                authority_rerank(&mut reranked, weight, band);
                let position: HashMap<String, usize> = reranked
                    .iter()
                    .enumerate()
                    .map(|(idx, candidate)| (candidate_doc(candidate), idx))
                    .collect();
                for pair in &pairs {
                    let higher = position
                        .get(&pair.higher_doc)
                        .copied()
                        .unwrap_or(usize::MAX);
                    let lower = position.get(&pair.lower_doc).copied().unwrap_or(usize::MAX);
                    if higher < lower {
                        source_pairs
                            .get_mut(&pair.source_pair)
                            .expect("source-pair was inserted in the OFF pass")
                            .on_above[index] += 1;
                    }
                }
            }
        }
    }

    Ok(AuthorityCategory {
        queries,
        weights: weights.to_vec(),
        gaps,
        source_pairs,
        recall,
    })
}

/// The benchmark-only widened window: top unique decisions for `query`, fetched with
/// `project_authority=true` and UN-reranked (natural relevance order). NOT the production OFF path.
fn authority_benchmark_window(
    postgres: &ManagedPostgres,
    embedder: &PreparedQueryEmbedder,
    query: &str,
) -> Result<Vec<Value>, ErrorObject> {
    let Some(query_text) = parade_query_text(query) else {
        return Ok(Vec::new());
    };
    let (embedding, fingerprint) = embedder.embed(query)?;
    let window = AUTHORITY_BENCHMARK_WINDOW;
    let response = hybrid_candidates_json(
        postgres,
        &HybridCandidateQuery {
            query_text: query_text.as_str(),
            query_embedding: Some(embedding.as_str()),
            embedding_fingerprint: Some(fingerprint.as_str()),
            retrieval_mode: RetrievalMode::Hybrid,
            group_by: GroupBy::Document,
            options: RetrievalOptions::default(),
            after_cursor: None,
            as_of: &today_utc(),
            kind_filter: Some("decision"),
            project_authority: true,
            decision_filters: DecisionFilters::default(),
            lexical_limit: window.saturating_mul(20),
            dense_limit: window.saturating_mul(20),
            limit: window,
        },
    )
    .map_err(storage_error_object)?;
    let parsed: Value = serde_json::from_str(&response)
        .map_err(|error| dependency_unavailable(error.to_string()))?;
    Ok(parsed["candidates"].as_array().cloned().unwrap_or_default())
}

/// Recall@10 documents through the PRODUCTION search (`search_with_postgres`), with `weight=None` the
/// exact OFF path `eval france-juris` measures and `Some(w)` the production authority-ON path. Returns
/// the ranked unique decision document ids.
fn authority_recall_documents(
    postgres: &ManagedPostgres,
    embedder: &PreparedQueryEmbedder,
    query: &str,
    weight: Option<f64>,
) -> Result<Vec<String>, ErrorObject> {
    let Some(query_text) = parade_query_text(query) else {
        return Ok(Vec::new());
    };
    let mut request = benchmark_search_request(
        query,
        CliKind::Decision,
        CliGroupBy::Document,
        None,
        AUTHORITY_RECALL_OVERFETCH,
    );
    request.authority_weight = weight;
    let response = match search_with_postgres(
        postgres,
        &request,
        RetrievalMode::Hybrid,
        OutputFormat::Concise,
        None,
        &query_text,
        LegalKind::Decision,
        false,
        Some(embedder),
    ) {
        Ok(response) => response,
        Err(error) if error.code == ErrorCode::NoResults => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    Ok(unique_decision_documents(&response))
}

fn unique_decision_documents(response: &Value) -> Vec<String> {
    let mut documents = Vec::new();
    if let Some(candidates) = response["candidates"].as_array() {
        for candidate in candidates {
            if let Some(document_id) = candidate["document_id"].as_str()
                && !documents.iter().any(|existing| existing == document_id)
            {
                documents.push(document_id.to_owned());
            }
        }
    }
    documents
}

/// Optional `--include-zones`: official Cassation-zone recall@10 OFF vs ON over `zone_units`, per zone.
/// OFF uses the production zone search (no rerank); ON re-ranks a widened zone window by authority.
fn authority_zone_recall(
    postgres: &ManagedPostgres,
    embedder: &PreparedQueryEmbedder,
    mode: CliSearchMode,
    weights: &[f64],
) -> Result<BTreeMap<String, RecallStats>, ErrorObject> {
    let retrieval_mode: RetrievalMode = mode.into();
    let needs_dense = retrieval_mode.uses_dense();
    let expected_fingerprint =
        needs_dense.then(|| embedding_config_from_env().storage_embedding_fingerprint());
    ensure_zone_retrieval_readiness(postgres, needs_dense, expected_fingerprint.as_deref())?;

    let gold_json = france_juris_zone_gold_json(postgres, FranceJurisZoneGoldLimits::default())
        .map_err(storage_error_object)?;
    let gold: Value = serde_json::from_str(&gold_json)
        .map_err(|error| dependency_unavailable(error.to_string()))?;
    let zone_embedder = needs_dense.then_some(embedder);

    let mut zones = BTreeMap::new();
    for zone in [CliZone::Motivations, CliZone::Moyens, CliZone::Dispositif] {
        let mut recall = RecallStats::new(weights.len());
        for qrel in gold[zone.as_str()].as_array().into_iter().flatten() {
            let Some(query) = qrel["query"].as_str() else {
                continue;
            };
            let Some(gold_id) = qrel["gold_document_id"].as_str() else {
                continue;
            };
            recall.scored += 1;
            let off_docs = france_juris_zone_search_documents(
                postgres,
                zone_embedder,
                retrieval_mode,
                zone,
                query,
                10,
            )?;
            if off_docs.iter().take(10).any(|doc| doc == gold_id) {
                recall.off_hits += 1;
            }
            for (index, &weight) in weights.iter().enumerate() {
                let on_docs = if weight <= 0.0 {
                    off_docs.clone()
                } else {
                    authority_zone_recall_documents(
                        postgres,
                        zone_embedder,
                        retrieval_mode,
                        zone,
                        query,
                        weight,
                    )?
                };
                if on_docs.iter().take(10).any(|doc| doc == gold_id) {
                    recall.on_hits[index] += 1;
                }
            }
        }
        zones.insert(zone.as_str().to_owned(), recall);
    }
    Ok(zones)
}

/// ON zone recall: re-rank a widened, `project_authority=true` zone window by authority and return the
/// ranked unique decision document ids (mirrors [`authority_recall_documents`] on the zone path).
fn authority_zone_recall_documents(
    postgres: &ManagedPostgres,
    embedder: Option<&PreparedQueryEmbedder>,
    retrieval_mode: RetrievalMode,
    zone: CliZone,
    query: &str,
    weight: f64,
) -> Result<Vec<String>, ErrorObject> {
    let Some(query_text) = parade_query_text(query) else {
        return Ok(Vec::new());
    };
    let (embedding, fingerprint) = match embedder {
        Some(embedder) if retrieval_mode.uses_dense() => {
            let (literal, fingerprint) = embedder.embed(query)?;
            (Some(literal), Some(fingerprint))
        }
        _ => (None, None),
    };
    let window = AUTHORITY_RECALL_OVERFETCH;
    let response = zone_candidates_json(
        postgres,
        &ZoneCandidateQuery {
            query_text: query_text.as_str(),
            query_embedding: embedding.as_deref(),
            embedding_fingerprint: fingerprint.as_deref(),
            retrieval_mode,
            options: RetrievalOptions::default(),
            after_cursor: None,
            zone: zone.as_str(),
            as_of: &today_utc(),
            project_authority: true,
            decision_filters: DecisionFilters::default(),
            lexical_limit: window.saturating_mul(20),
            dense_limit: window.saturating_mul(20),
            limit: window,
        },
    )
    .map_err(storage_error_object)?;
    let mut response: Value = serde_json::from_str(&response)
        .map_err(|error| dependency_unavailable(error.to_string()))?;
    if let Some(candidates) = response["candidates"].as_array_mut() {
        authority_rerank(candidates, weight, AUTHORITY_DEFAULT_BAND);
    }
    Ok(unique_decision_documents(&response))
}

fn score_gap_summary(gaps: &[f64]) -> Value {
    if gaps.is_empty() {
        return json!({ "count": 0, "min": null, "mean": null, "median": null, "max": null });
    }
    let mut sorted = gaps.to_vec();
    sorted.sort_by(f64::total_cmp);
    let count = sorted.len();
    let mean = sorted.iter().sum::<f64>() / count as f64;
    json!({
        "count": count,
        "min": round6(sorted[0]),
        "mean": round6(mean),
        "median": round6(sorted[count / 2]),
        "max": round6(sorted[count - 1]),
    })
}

/// The per-weight lift sweep for a `PairStats` (the order aggregate OR a single source-pair): OFF lift +
/// each weight's ON lift and the ON-minus-OFF delta. `coverage` is the pair count (reported so a tiny or
/// trivially-gapped pair set cannot masquerade as a strong signal).
fn lift_weights_json(stats: &PairStats, weights: &[f64]) -> Value {
    let lift_off = floor_metric(mean(stats.off_above, stats.total));
    let per_weight: Vec<Value> = weights
        .iter()
        .enumerate()
        .map(|(index, &weight)| {
            let lift_on = floor_metric(mean(stats.on_above[index], stats.total));
            json!({
                "authority_weight": weight,
                "authority_lift_on": lift_on,
                "authority_lift_delta": floor_metric(lift_on - lift_off),
            })
        })
        .collect();
    json!({
        "pair_coverage": stats.total,
        "authority_lift_off": lift_off,
        "weights": per_weight,
    })
}

/// The per-weight recall sweep for a `RecallStats`: OFF recall + each weight's ON recall, the delta, and
/// whether it regressed vs OFF (and, when `apply_floor`, whether it cleared the 0.50 floor).
fn recall_weights_json(recall: &RecallStats, weights: &[f64], apply_floor: bool) -> Value {
    let recall_off = floor_metric(recall.recall_off());
    let per_weight: Vec<Value> = weights
        .iter()
        .enumerate()
        .map(|(index, &weight)| {
            let recall_on = floor_metric(recall.recall_on(index));
            json!({
                "authority_weight": weight,
                "recall_at_10_on": recall_on,
                "recall_at_10_delta": floor_metric(recall_on - recall_off),
                "recall_no_regression": recall_on + 1e-9 >= recall_off,
                "recall_meets_floor": !apply_floor || recall_on >= PHASE2_MIN_RETRIEVAL_RECALL_AT_10,
            })
        })
        .collect();
    json!({
        "queries": recall.scored,
        "recall_at_10_off": recall_off,
        "weights": per_weight,
    })
}

/// Does `recall` hold ON at-or-above OFF for every weight (and, when `apply_floor`, at-or-above 0.50)?
fn recall_guard_ok(recall: &RecallStats, weights: &[f64], apply_floor: bool) -> bool {
    if recall.scored == 0 {
        return true; // an empty category does not vote
    }
    let recall_off = floor_metric(recall.recall_off());
    (0..weights.len()).all(|index| {
        let recall_on = floor_metric(recall.recall_on(index));
        recall_on + 1e-9 >= recall_off
            && (!apply_floor || recall_on >= PHASE2_MIN_RETRIEVAL_RECALL_AT_10)
    })
}

fn authority_category_json(category: &AuthorityCategory) -> Value {
    let aggregate = category.aggregate();
    let source_pairs: serde_json::Map<String, Value> = category
        .source_pairs
        .iter()
        .map(|(key, stats)| (key.clone(), lift_weights_json(stats, &category.weights)))
        .collect();
    json!({
        "metric": "pairwise_authority_lift",
        "queries": category.queries,
        "score_gap": score_gap_summary(&category.gaps),
        "lift": lift_weights_json(&aggregate, &category.weights),
        "lift_by_source_pair": source_pairs,
        "recall": recall_weights_json(&category.recall, &category.weights, true),
    })
}

#[allow(clippy::too_many_arguments)]
fn authority_benchmark_artifact(
    judicial: &AuthorityCategory,
    administrative: &AuthorityCategory,
    zones: Option<&BTreeMap<String, RecallStats>>,
    weights: &[f64],
    judicial_limit: u32,
    administrative_limit: u32,
    index_revision: &str,
    source_revision: &str,
) -> Value {
    // The recall guard votes over judicial + administrative (with the 0.50 floor) and, when present,
    // each zone (no absolute floor — measured-only overlay; only no-regression vs OFF is required).
    let mut guard_ok = recall_guard_ok(&judicial.recall, weights, true)
        && recall_guard_ok(&administrative.recall, weights, true);
    let zones_json = zones.map(|zones| {
        let mut block = serde_json::Map::new();
        for (zone, recall) in zones {
            guard_ok = guard_ok && recall_guard_ok(recall, weights, false);
            block.insert(zone.clone(), recall_weights_json(recall, weights, false));
        }
        Value::Object(block)
    });

    json!({
        "schema_version": 1,
        "kind": "phase2_authority_benchmark",
        "state": "measured",
        "gate_input": false,
        "jurisdiction": "france",
        "fingerprint": "bge-m3:1024:normalize:true",
        "metric": "pairwise_authority_lift",
        "authority_band": AUTHORITY_DEFAULT_BAND,
        "weights_swept": weights,
        "claim_scope": "MEASURED-ONLY within-order authority ORDERING signal (publisher source+publication labels only, no LLM/human): does the authority re-rank lift the higher-authority of two near-tied same-order decisions above the lower one. NOT an input to the Phase 2 full-juridic gate; never inflates the corpus claim.",
        "source": "DILA CASS/CAPP/INCA/JADE bulk XML (Licence Ouverte) official fields, extracted from the built index (same decision-headnote gold recipe as eval france-juris)",
        "retriever": "jurisearch authority re-rank over the hybrid kind=decision candidate window",
        "recall_regression_guard": {
            "floor": PHASE2_MIN_RETRIEVAL_RECALL_AT_10,
            "rule": "for every swept weight, recall@10 ON must be >= the OFF measurement (per order/zone) AND >= the 0.50 floor (judicial/administrative only)",
            "ok": guard_ok
        },
        "categories": {
            "judicial": authority_category_json(judicial),
            "administrative": authority_category_json(administrative)
        },
        "zones": zones_json,
        "provenance": {
            "official_source": "DILA CASS/CAPP/INCA/JADE bulk XML (Licence Ouverte), extracted from the built index",
            "pipeline": PHASE2_PRODUCTION_PIPELINE,
            "code_version": CLI_CODE_VERSION,
            "index_revision": index_revision,
            "source_revision": source_revision,
            "qrel_selection": "deterministic_bounded_by_document_id_from_official_index_fields",
            "qrel_limits": {
                "judicial_retrieval": judicial_limit,
                "administrative_retrieval": administrative_limit
            },
            "pair_rule": "same-order, both tiered, different tiers, neither marker_absent, both inside the §3.4 relevance band; labeled purely by source+publication",
            "sampled": false,
            "human_in_gold": false,
            "llm_in_gold": false,
            "pseudonymisation": "preserved: gold and authority labels come from the pseudonymised official bulk fields; no re-identification, no cross-source linking"
        },
        "reason": if guard_ok {
            "measured-only pairwise authority-lift (ON minus OFF), per order and source-pair, with coverage + score-gap; the recall regression guard found no weight that buried the gold below OFF or the 0.50 floor. Advisory only — never asserted as a gate."
        } else {
            "measured-only pairwise authority-lift (ON minus OFF), per order and source-pair, with coverage + score-gap; the recall regression guard FLAGGED at least one weight that regressed recall below OFF or the 0.50 floor (see recall_regression_guard.ok). Advisory only — never asserted as a gate."
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(doc: &str, source: &str, publication: Option<&str>, rrf: f64) -> Value {
        let mut value = json!({ "document_id": doc, "source": source, "scores": { "rrf": rrf } });
        if let Some(publication) = publication {
            value["publication"] = json!(publication);
        }
        value
    }

    #[test]
    fn parse_weights_validates_range_dedupes_and_rejects_garbage() {
        assert_eq!(
            parse_authority_weights("0.0,0.1,0.25,0.5").unwrap(),
            vec![0.0, 0.1, 0.25, 0.5]
        );
        assert_eq!(
            parse_authority_weights("0.5, 0.5 ,0.1").unwrap(),
            vec![0.5, 0.1]
        );
        assert!(parse_authority_weights("1.5").is_err());
        assert!(parse_authority_weights("-0.1").is_err());
        assert!(parse_authority_weights("nan").is_err());
        assert!(parse_authority_weights("abc").is_err());
        assert!(parse_authority_weights("").is_err());
    }

    #[test]
    fn window_pairs_forms_only_same_order_diff_tier_in_band_non_marker_absent_pairs() {
        let band = AUTHORITY_DEFAULT_BAND;

        let window = vec![
            cand("cass:A", "cass", Some("oui"), 0.99), // tier 3
            cand("cass:B", "cass", Some("non"), 0.98), // tier 2, in band
        ];
        let pairs = window_pairs(&window, band);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].higher_doc, "cass:A");
        assert_eq!(pairs[0].higher_natural_idx, 0);
        assert_eq!(pairs[0].source_pair, "cass+oui>cass");

        // Authority DISAGREES with relevance: published (tier 3) cass is the LESS relevant one.
        let disagree = vec![
            cand("cass:B", "cass", Some("non"), 0.99),
            cand("cass:A", "cass", Some("oui"), 0.98),
        ];
        let pairs = window_pairs(&disagree, band);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].higher_doc, "cass:A");
        assert_eq!(pairs[0].higher_natural_idx, 1);
        assert_eq!(pairs[0].lower_natural_idx, 0);

        // Cross-order, marker_absent, out-of-band, same-tier all produce no pair.
        assert!(
            window_pairs(
                &[
                    cand("cass:A", "cass", Some("oui"), 0.99),
                    cand("jade:C", "jade", Some("A"), 0.98)
                ],
                band
            )
            .is_empty()
        );
        assert!(
            window_pairs(
                &[
                    cand("cass:A", "cass", Some("oui"), 0.99),
                    cand("capp:D", "capp", None, 0.98)
                ],
                band
            )
            .is_empty()
        );
        assert!(
            window_pairs(
                &[
                    cand("cass:A", "cass", Some("oui"), 1.00),
                    cand("cass:B", "cass", Some("non"), 0.90)
                ],
                band
            )
            .is_empty()
        );
        assert!(
            window_pairs(
                &[
                    cand("cass:A", "cass", Some("oui"), 0.99),
                    cand("cass:E", "cass", Some("oui"), 0.98)
                ],
                band
            )
            .is_empty()
        );
    }

    fn pair_stats(total: usize, off_above: usize, on_above: Vec<usize>) -> PairStats {
        PairStats {
            total,
            off_above,
            on_above,
        }
    }

    fn recall_stats(scored: usize, off_hits: usize, on_hits: Vec<usize>) -> RecallStats {
        RecallStats {
            scored,
            off_hits,
            on_hits,
        }
    }

    fn category(
        weights: &[f64],
        source_pairs: Vec<(&str, PairStats)>,
        recall: RecallStats,
    ) -> AuthorityCategory {
        AuthorityCategory {
            queries: 10,
            weights: weights.to_vec(),
            gaps: vec![0.01, 0.02, 0.03],
            source_pairs: source_pairs
                .into_iter()
                .map(|(key, stats)| (key.to_owned(), stats))
                .collect(),
            recall,
        }
    }

    #[test]
    fn artifact_is_measured_only_with_per_source_lift_and_recall_guard() {
        let weights = vec![0.0, 0.25, 0.5];
        // judicial: two source-pairs aggregate to coverage 8; ON lift rises with weight.
        let judicial = category(
            &weights,
            vec![
                ("cass+oui>inca", pair_stats(5, 2, vec![2, 4, 5])),
                ("cass+oui>cass", pair_stats(3, 1, vec![1, 2, 3])),
            ],
            recall_stats(10, 6, vec![6, 6, 7]),
        );
        let administrative = category(
            &weights,
            vec![("jade+2>jade+0", pair_stats(4, 2, vec![2, 3, 4]))],
            recall_stats(10, 6, vec![6, 6, 6]),
        );
        let artifact = authority_benchmark_artifact(
            &judicial,
            &administrative,
            None,
            &weights,
            60,
            60,
            "rev1",
            "src1",
        );

        assert_eq!(artifact["kind"], "phase2_authority_benchmark");
        assert_eq!(artifact["state"], "measured");
        assert_eq!(artifact["gate_input"], false);
        assert_eq!(artifact["recall_regression_guard"]["ok"], true);
        assert!(artifact["zones"].is_null());

        let jud = &artifact["categories"]["judicial"];
        // Aggregate lift OFF = (2+1)/8 = 0.375; ON at 0.5 = (5+3)/8 = 1.0.
        assert_eq!(jud["lift"]["pair_coverage"], 8);
        assert_eq!(jud["lift"]["authority_lift_off"], floor_metric(3.0 / 8.0));
        let agg_half = jud["lift"]["weights"]
            .as_array()
            .unwrap()
            .iter()
            .find(|w| w["authority_weight"] == 0.5)
            .unwrap();
        assert_eq!(agg_half["authority_lift_on"], 1.0);
        // Per-source-pair breakdown is present and independent.
        let pair = &jud["lift_by_source_pair"]["cass+oui>inca"];
        assert_eq!(pair["pair_coverage"], 5);
        assert_eq!(pair["authority_lift_off"], floor_metric(2.0 / 5.0));
    }

    #[test]
    fn recall_guard_fails_when_on_recall_regresses_below_off_or_floor_and_reason_reflects_it() {
        let weights = vec![0.0, 0.5];
        let pairs = vec![("cass+oui>inca", pair_stats(4, 2, vec![2, 3]))];
        // ON recall 0.40 < 0.50 floor at weight 0.5 trips the guard.
        let below_floor = category(&weights, pairs.clone(), recall_stats(10, 6, vec![6, 4]));
        let ok = category(&weights, pairs.clone(), recall_stats(10, 6, vec![6, 6]));
        let artifact =
            authority_benchmark_artifact(&below_floor, &ok, None, &weights, 60, 60, "r", "s");
        assert_eq!(artifact["state"], "measured");
        assert_eq!(artifact["recall_regression_guard"]["ok"], false);
        assert!(
            artifact["reason"].as_str().unwrap().contains("FLAGGED"),
            "reason must reflect the failed guard, got {}",
            artifact["reason"]
        );

        // All-clear: guard ok and the reason says no weight regressed.
        let artifact2 = authority_benchmark_artifact(&ok, &ok, None, &weights, 60, 60, "r", "s");
        assert_eq!(artifact2["recall_regression_guard"]["ok"], true);
        assert!(artifact2["reason"].as_str().unwrap().contains("no weight"));
    }

    #[test]
    fn zone_recall_folds_into_the_guard_without_an_absolute_floor() {
        let weights = vec![0.0, 0.5];
        let pairs = vec![("cass+oui>inca", pair_stats(4, 2, vec![2, 3]))];
        let order = category(&weights, pairs, recall_stats(10, 6, vec![6, 6]));
        // Zone ON recall 0.30 is BELOW the 0.50 floor but does NOT regress vs its OFF (0.30) → guard ok
        // (zones have no absolute floor, only no-regression).
        let zones: BTreeMap<String, RecallStats> =
            [("motivations".to_owned(), recall_stats(10, 3, vec![3, 3]))]
                .into_iter()
                .collect();
        let artifact =
            authority_benchmark_artifact(&order, &order, Some(&zones), &weights, 60, 60, "r", "s");
        assert_eq!(artifact["recall_regression_guard"]["ok"], true);
        assert_eq!(artifact["zones"]["motivations"]["recall_at_10_off"], 0.3);

        // A zone regression (ON 0.20 < OFF 0.30) DOES trip the guard.
        let regressed: BTreeMap<String, RecallStats> =
            [("motivations".to_owned(), recall_stats(10, 3, vec![3, 2]))]
                .into_iter()
                .collect();
        let artifact2 = authority_benchmark_artifact(
            &order,
            &order,
            Some(&regressed),
            &weights,
            60,
            60,
            "r",
            "s",
        );
        assert_eq!(artifact2["recall_regression_guard"]["ok"], false);
    }
}
