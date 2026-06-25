//! Legal-authority model and the pure, deterministic post-SQL re-rank helper for jurisprudence
//! decisions. This module is **pure**: it has no SQL/DB knowledge and is wired into neither path here
//! (A4 calls `authority_rerank` from the main and zone payload builders). It operates on the
//! `serde_json::Value` candidate objects that `hybrid_candidates_json` / `zone_candidates_json` emit.
//!
//! Two separate, never-cross-compared ordered scales (design §3.2):
//! - judicial (`cass`/`inca`/`capp`): `tier ∈ 0..=3`
//! - administrative (`jade`): `tier ∈ 0..=2`
//!
//! The re-rank is **relevance-dominant** (design §3.4): it only reorders within-order neighbours that
//! are already near-tied on the fused relevance score (inside a band relative to the local relevance
//! leader), and `weight <= 0.0` / non-finite is an inert no-op (the OFF path is byte-identical).

use serde_json::{Value, json};

use crate::retrieval::RetrievalOptions;

/// Default relevance band: a candidate may only be lifted by authority over a more-relevant neighbour
/// when their rounded fused scores are within `band * leader_score` (5%).
pub const AUTHORITY_DEFAULT_BAND: f64 = 0.05;

/// Default re-rank window factor `W`: the ON path widens `query_limit` to `top_k * W` so the re-rank
/// has a deeper pool. A4 clamps it per-grouping (`min(W, pool_multiplier)`) so it never outruns the
/// RRF arm pool feeding the candidate set.
pub const AUTHORITY_RERANK_WINDOW: u32 = 8;

/// The two French legal orders. Their authority tiers are NEVER compared on one number: a `jade` and a
/// `cass` result are ordered purely by relevance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorityOrder {
    Judicial,
    Administrative,
}

impl AuthorityOrder {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Judicial => "judicial",
            Self::Administrative => "administrative",
        }
    }
}

/// A decision's per-order authority tier, derived from the reliable `source` axis refined by the coarse
/// `publication` marker. `marker_absent` is an honesty flag (provenance only — it never excludes a row
/// from re-ranking): it is set when the marker that would refine the tier is missing (e.g. `capp`, which
/// carries no Bulletin flag, or a `jade`/`cass` with no publication value).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuthorityTier {
    pub order: AuthorityOrder,
    pub tier: u8,
    pub tier_max: u8,
    pub marker_absent: bool,
}

impl AuthorityTier {
    /// Normalized per-order authority fraction `a ∈ [0,1]` (`tier / tier_max`). `tier_max` is 3
    /// (judicial) or 2 (administrative), never 0.
    fn fraction(self) -> f64 {
        f64::from(self.tier) / f64::from(self.tier_max)
    }
}

/// The ON/OFF primitive. Returns `Some(w)` only for a finite `w > 0.0`; `None` when the field is unset,
/// non-finite, or `<= 0.0`. This is load-bearing: `rerank_on = effective_authority_weight(..).is_some()`
/// must never treat `0.0` as ON, so the OFF path stays byte-identical. No environment fallback in v1.
pub fn effective_authority_weight(options: &RetrievalOptions) -> Option<f64> {
    options
        .authority_weight
        .filter(|weight| weight.is_finite() && *weight > 0.0)
}

/// Per-order authority tier from `documents.source` refined by `canonical_json->>'publication'`.
/// Returns `None` for non-decision / unknown sources (the caller leaves those rows untouched).
///
/// - judicial `publication` is the `PUBLI_BULL@publie` flag (`oui`/`non`/absent);
/// - administrative (`jade`) `publication` is the `PUBLI_RECUEIL` Lebon class letter (e.g. `C`/absent).
pub fn authority_tier(source: &str, publication: Option<&str>) -> Option<AuthorityTier> {
    let marker = publication
        .map(str::trim)
        .filter(|value| !value.is_empty());
    match source {
        "cass" => {
            let (tier, marker_absent) = match marker {
                Some(value) if value.eq_ignore_ascii_case("oui") => (3, false),
                Some(_) => (2, false),
                None => (2, true),
            };
            Some(AuthorityTier {
                order: AuthorityOrder::Judicial,
                tier,
                tier_max: 3,
                marker_absent,
            })
        }
        "inca" => Some(AuthorityTier {
            order: AuthorityOrder::Judicial,
            tier: 1,
            tier_max: 3,
            marker_absent: false,
        }),
        "capp" => Some(AuthorityTier {
            order: AuthorityOrder::Judicial,
            tier: 0,
            tier_max: 3,
            // Cour d'appel carries no Bulletin marker; flag tier 0 so a reader knows it is not a
            // publication-based penalty.
            marker_absent: true,
        }),
        "jade" => {
            let (tier, marker_absent) =
                match marker.and_then(|value| value.chars().next()).map(|c| c.to_ascii_uppercase()) {
                    Some('A') => (2, false),
                    Some('B') => (1, false),
                    Some(_) => (0, false),
                    None => (0, true),
                };
            Some(AuthorityTier {
                order: AuthorityOrder::Administrative,
                tier,
                tier_max: 2,
                marker_absent,
            })
        }
        _ => None,
    }
}

/// Round to 8 decimals to match the cursor/SQL fused-score rounding (`round(fused_score, 8)`), so band
/// membership and adjusted scoring use the exact same value the relevance order was built on.
fn rounded_rrf(candidate: &Value) -> f64 {
    let raw = candidate
        .get("scores")
        .and_then(|scores| scores.get("rrf"))
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    (raw * 1e8).round() / 1e8
}

/// The candidate's authority tier from its projected `source` + optional `publication` fields.
fn candidate_tier(candidate: &Value) -> Option<AuthorityTier> {
    let source = candidate.get("source").and_then(Value::as_str)?;
    let publication = candidate.get("publication").and_then(Value::as_str);
    authority_tier(source, publication)
}

/// `adjusted(c) = s_c * (1 + weight * a_c)` over the rounded fused score. Used only to ORDER same-order
/// neighbours inside a band; it is never written back as the score.
fn adjusted_score(candidate: &Value, weight: f64) -> f64 {
    let fraction = candidate_tier(candidate).map_or(0.0, AuthorityTier::fraction);
    rounded_rrf(candidate) * (1.0 + weight * fraction)
}

/// Annotate every candidate with a known authority tier (ON path only) so a client/eval can see why a
/// row was or was not boosted. Additive: rows with no known tier (unknown source) are left untouched.
fn annotate_authority_blocks(candidates: &mut [Value], weight: f64) {
    for candidate in candidates.iter_mut() {
        if let Some(tier) = candidate_tier(candidate) {
            candidate["authority"] = json!({
                "order": tier.order.as_str(),
                "tier": tier.tier,
                "tier_max": tier.tier_max,
                "signal": "source+publication",
                "marker_absent": tier.marker_absent,
                "applied_weight": weight,
            });
        }
    }
}

/// Reorder within ONE band cluster, same-order rows only. The slots a given order occupies stay fixed
/// (their interleaving is relevance-determined); within each order's own slots the members are sorted
/// by `adjusted` desc, ties broken by their incoming relative order (so relevance order is preserved on
/// ties and the operation is idempotent when authority is uniform). Unknown-tier rows never move.
fn rerank_cluster(cluster: &mut [Value], weight: f64) {
    for order in [AuthorityOrder::Judicial, AuthorityOrder::Administrative] {
        let slots: Vec<usize> = cluster
            .iter()
            .enumerate()
            .filter(|(_, candidate)| candidate_tier(candidate).map(|tier| tier.order) == Some(order))
            .map(|(index, _)| index)
            .collect();
        if slots.len() < 2 {
            continue;
        }
        // `ranking[i] = adjusted` for the i-th member, in incoming order; sort indices by adjusted desc
        // then incoming position so the reorder is a stable, deterministic permutation of the members.
        let originals: Vec<Value> = slots.iter().map(|&slot| cluster[slot].clone()).collect();
        let scores: Vec<f64> = originals
            .iter()
            .map(|candidate| adjusted_score(candidate, weight))
            .collect();
        let mut order_indices: Vec<usize> = (0..slots.len()).collect();
        order_indices.sort_by(|&a, &b| scores[b].total_cmp(&scores[a]).then(a.cmp(&b)));
        for (&target, &source) in slots.iter().zip(order_indices.iter()) {
            cluster[target] = originals[source].clone();
        }
    }
}

/// Stable, deterministic authority re-rank of an ALREADY-RELEVANCE-SORTED candidate window
/// (`round(fused_score,8)` desc, id asc). Inert for non-finite or `weight <= 0.0` (no annotation, no
/// reorder — the OFF path). When ON it annotates known-authority candidates and reorders only
/// within-order neighbours inside a relevance band relative to the local leader. Cross-order rows are
/// never reordered against each other by authority; clearly-more-relevant rows (outside the band) are
/// never overtaken.
pub fn authority_rerank(candidates: &mut [Value], weight: f64, band: f64) {
    if !weight.is_finite() || weight <= 0.0 {
        return;
    }
    annotate_authority_blocks(candidates, weight);
    // A negative/non-finite band degrades to exact-tie clustering rather than a wild window.
    let band = if band.is_finite() { band.max(0.0) } else { 0.0 };

    let len = candidates.len();
    let mut start = 0;
    while start < len {
        let leader = rounded_rrf(&candidates[start]);
        // Leader-relative cluster: a maximal run within `band * leader` of the cluster's leader (NOT
        // pairwise-adjacent, which would be non-transitive band-creep).
        let mut end = start + 1;
        while end < len {
            let score = rounded_rrf(&candidates[end]);
            if leader > 0.0 && (leader - score) <= band * leader {
                end += 1;
            } else {
                break;
            }
        }
        if end - start > 1 {
            rerank_cluster(&mut candidates[start..end], weight);
        }
        start = end;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(id: &str, source: &str, publication: Option<&str>, rrf: f64) -> Value {
        let mut value = json!({
            "chunk_id": id,
            "document_id": id,
            "source": source,
            "scores": { "rrf": (rrf * 1e8).round() / 1e8 },
        });
        if let Some(publication) = publication {
            value["publication"] = json!(publication);
        }
        value
    }

    fn ids(candidates: &[Value]) -> Vec<String> {
        candidates
            .iter()
            .map(|candidate| candidate["chunk_id"].as_str().unwrap().to_owned())
            .collect()
    }

    // ---- authority_tier ----

    #[test]
    fn judicial_tiers_cover_published_unpublished_absent_inca_capp() {
        let published = authority_tier("cass", Some("oui")).unwrap();
        assert_eq!(
            (published.order, published.tier, published.tier_max, published.marker_absent),
            (AuthorityOrder::Judicial, 3, 3, false)
        );

        // Case-insensitive marker.
        assert_eq!(authority_tier("cass", Some("OUI")).unwrap().tier, 3);

        let unpublished = authority_tier("cass", Some("non")).unwrap();
        assert_eq!((unpublished.tier, unpublished.marker_absent), (2, false));

        // Absent publication on cass: conservative tier 2, but flagged because the input never said so.
        let absent = authority_tier("cass", None).unwrap();
        assert_eq!((absent.tier, absent.marker_absent), (2, true));
        // Blank is treated as absent.
        assert_eq!(authority_tier("cass", Some("   ")).unwrap().marker_absent, true);

        let inca = authority_tier("inca", None).unwrap();
        assert_eq!((inca.tier, inca.tier_max, inca.marker_absent), (1, 3, false));

        let capp = authority_tier("capp", Some("oui")).unwrap();
        // capp is tier 0 regardless of any flag, and always marker_absent.
        assert_eq!((capp.tier, capp.marker_absent), (0, true));
    }

    #[test]
    fn administrative_tiers_from_lebon_letter_case_insensitive_first_char() {
        let a = authority_tier("jade", Some("A")).unwrap();
        assert_eq!(
            (a.order, a.tier, a.tier_max, a.marker_absent),
            (AuthorityOrder::Administrative, 2, 2, false)
        );
        assert_eq!(authority_tier("jade", Some(" b ")).unwrap().tier, 1);
        // Other present class letter: low tier but NOT marker_absent (a marker is present).
        let c = authority_tier("jade", Some("C")).unwrap();
        assert_eq!((c.tier, c.marker_absent), (0, false));
        // Absent letter: tier 0 AND marker_absent.
        let absent = authority_tier("jade", None).unwrap();
        assert_eq!((absent.tier, absent.marker_absent), (0, true));
        assert_eq!(authority_tier("jade", Some("")).unwrap().marker_absent, true);
    }

    #[test]
    fn unknown_or_non_decision_sources_return_none() {
        assert!(authority_tier("legi", Some("oui")).is_none());
        assert!(authority_tier("", None).is_none());
        assert!(authority_tier("CASS", Some("oui")).is_none()); // source axis is exact lowercase
    }

    // ---- effective_authority_weight ----

    #[test]
    fn effective_weight_is_on_only_for_finite_positive() {
        let with = |weight| RetrievalOptions {
            authority_weight: weight,
            ..RetrievalOptions::default()
        };
        assert_eq!(effective_authority_weight(&with(None)), None);
        assert_eq!(effective_authority_weight(&with(Some(0.0))), None);
        assert_eq!(effective_authority_weight(&with(Some(-0.5))), None);
        assert_eq!(effective_authority_weight(&with(Some(f64::NAN))), None);
        assert_eq!(effective_authority_weight(&with(Some(f64::INFINITY))), None);
        assert_eq!(effective_authority_weight(&with(Some(0.25))), Some(0.25));
    }

    // ---- authority_rerank ----

    #[test]
    fn weight_zero_is_a_noop_and_adds_no_authority_block() {
        let mut candidates = vec![
            candidate("a", "cass", Some("non"), 1.000), // tier 2
            candidate("b", "cass", Some("oui"), 0.990), // tier 3, in band, more authoritative
        ];
        authority_rerank(&mut candidates, 0.0, AUTHORITY_DEFAULT_BAND);
        assert_eq!(ids(&candidates), vec!["a", "b"]);
        assert!(candidates.iter().all(|c| c.get("authority").is_none()));
    }

    #[test]
    fn same_order_in_band_higher_tier_overtakes_when_close_enough() {
        // a: unpublished cass (tier 2) at the top; b: published cass (tier 3) just below, within 5%.
        let mut candidates = vec![
            candidate("a", "cass", Some("non"), 1.000),
            candidate("b", "cass", Some("oui"), 0.990),
        ];
        authority_rerank(&mut candidates, 0.5, AUTHORITY_DEFAULT_BAND);
        // b's adjusted (0.990*1.5=1.485) beats a's (1.000*(1+0.5*2/3)=1.333) -> b rises.
        assert_eq!(ids(&candidates), vec!["b", "a"]);
        // Both annotated.
        assert_eq!(candidates[0]["authority"]["tier"], json!(3));
        assert_eq!(candidates[1]["authority"]["marker_absent"], json!(false));
    }

    #[test]
    fn out_of_band_higher_tier_does_not_move() {
        // b is published cass (higher tier) but 10% less relevant -> outside the 5% band -> no move.
        let mut candidates = vec![
            candidate("a", "cass", Some("non"), 1.000),
            candidate("b", "cass", Some("oui"), 0.900),
        ];
        authority_rerank(&mut candidates, 0.9, AUTHORITY_DEFAULT_BAND);
        assert_eq!(ids(&candidates), vec!["a", "b"]);
    }

    #[test]
    fn cross_order_rows_are_not_reordered_by_authority() {
        // a: cass tier 3; b: jade class A (tier 2) within band. Different orders -> never swapped by
        // authority even though both are high-authority and near-tied on relevance.
        let mut candidates = vec![
            candidate("a", "cass", Some("oui"), 1.000),
            candidate("b", "jade", Some("A"), 0.990),
        ];
        authority_rerank(&mut candidates, 0.9, AUTHORITY_DEFAULT_BAND);
        assert_eq!(ids(&candidates), vec!["a", "b"]);
    }

    #[test]
    fn mixed_cluster_reorders_only_same_order_slots() {
        // Relevance order: j1 (cass tier2), adm (jade A), j2 (cass tier3). All within band of j1.
        // Only the two judicial rows may swap; the administrative slot (position 1) stays put.
        let mut candidates = vec![
            candidate("j1", "cass", Some("non"), 1.000),
            candidate("adm", "jade", Some("A"), 0.985),
            candidate("j2", "cass", Some("oui"), 0.980),
        ];
        authority_rerank(&mut candidates, 0.5, AUTHORITY_DEFAULT_BAND);
        // j2 (adjusted 0.980*1.5=1.470) beats j1 (1.000*1.333=1.333); adm holds slot 1.
        assert_eq!(ids(&candidates), vec!["j2", "adm", "j1"]);
    }

    #[test]
    fn unknown_source_rows_stay_in_place_and_get_no_block() {
        let mut candidates = vec![
            candidate("u", "legi", None, 1.000), // unknown -> never moves, no block
            candidate("a", "cass", Some("non"), 0.995),
            candidate("b", "cass", Some("oui"), 0.990),
        ];
        authority_rerank(&mut candidates, 0.5, AUTHORITY_DEFAULT_BAND);
        // u holds slot 0; a/b are same-order in-band -> b overtakes a.
        assert_eq!(ids(&candidates), vec!["u", "b", "a"]);
        assert!(candidates[0].get("authority").is_none());
    }
}
