## Recommendation

Use the proposed mapping, with one clarification: `marker_absent` means "the authoritative marker for this source is missing or not available", not "do not compute a tier". It should be surfaced in JSON and usable by eval filters, but it should not by itself exclude a candidate from the production rerank.

Source facts that decide this:

- The design defines separate judicial/admin scales and explicitly says `capp`/`jade` lack comparable Bulletin/Rapport markers, so the two orders are never compared on one number (`work/03-implementation/05-ranking/2026-06-24-authority-aware-ranking-design.md:99-139`).
- The implementation plan says A1 must cover missing publication, case-insensitive markers, unknown sources, `marker_absent`, same-order-only movement, and deterministic id fallback (`work/03-implementation/05-ranking/2026-06-25-authority-aware-ranking-implementation-plan-v2.md:116-139`).
- The parser stores judicial `publication` from `PUBLI_BULL_publie` and administrative `publication` from `PUBLI_RECUEIL`, so `documents.source` is the required discriminator (`crates/jurisearch-ingest/src/juri/parser.rs:319-321`).

## Q1 - `marker_absent`

Implement `authority_tier(source, publication)` this way:

| source | publication | order | tier | tier_max | marker_absent |
|---|---:|---|---:|---:|---|
| `cass` | trimmed/lowercase == `oui` | Judicial | 3 | 3 | false |
| `cass` | present and not `oui`, including `non` | Judicial | 2 | 3 | false |
| `cass` | absent or blank | Judicial | 2 | 3 | true |
| `inca` | any | Judicial | 1 | 3 | false |
| `capp` | any | Judicial | 0 | 3 | true |
| `jade` | present, first trimmed char `A`/`a` | Administrative | 2 | 2 | false |
| `jade` | present, first trimmed char `B`/`b` | Administrative | 1 | 2 | false |
| `jade` | present, other nonblank value | Administrative | 0 | 2 | false |
| `jade` | absent or blank | Administrative | 0 | 2 | true |
| other | any | none | - | - | return `None` |

So yes: `cass` with absent publication should be `marker_absent=true`, because tier 2 is a conservative default but the input did not actually say `non`. Also yes: `inca` should be `marker_absent=false`, because the source itself is the signal for "inédit Cassation"; no missing Bulletin marker is being inferred.

For `jade`, use a case-insensitive first character of the trimmed value, not strict single-letter equality. Real `PUBLI_RECUEIL` is expected to be a bare class letter such as `C`, but first-char matching makes the helper robust to whitespace or harmless labels while still treating blank as absent. Unit tests should cover `"A"`, `" b "`, `"C"`, `""`, and `None`.

## Q2 - Deterministic `authority_rerank`

Use leader-relative clusters, not pairwise-adjacent clusters. Start a cluster at the current relevance leader and include the maximal following run whose rounded score remains within `band * leader_score` of that leader. If a long descending chain has each adjacent pair in band but the tail is outside the head's band, split it. That prevents transitive "band creep" and matches the design's "fraction of the leader's score" rule.

Do not implement this as one global comparator that asks whether two rows are in band. That comparator would be non-transitive. First form clusters, then sort only inside each cluster.

Concrete algorithm:

```rust
fn authority_rerank(candidates: &mut [Value], weight: f64, band: f64) {
    if weight <= 0.0 || !weight.is_finite() {
        return;
    }

    annotate_known_authority_blocks(candidates);

    let mut start = 0;
    while start < candidates.len() {
        let leader_score = rounded_score_8(&candidates[start]);
        let mut end = start + 1;

        while end < candidates.len()
            && leader_score > 0.0
            && leader_score - rounded_score_8(&candidates[end]) <= band * leader_score
        {
            end += 1;
        }

        rerank_cluster_by_order(&mut candidates[start..end], weight);
        start = end;
    }
}

fn rerank_cluster_by_order(cluster: &mut [Value], weight: f64) {
    for order in [AuthorityOrder::Judicial, AuthorityOrder::Administrative] {
        let slots: Vec<usize> = cluster
            .iter()
            .enumerate()
            .filter_map(|(idx, c)| authority_tier_for_candidate(c).filter(|t| t.order == order).map(|_| idx))
            .collect();

        let mut items: Vec<Value> = slots.iter().map(|&idx| cluster[idx].clone()).collect();
        items.sort_by(|a, b| {
            adjusted_score(b, weight)
                .total_cmp(&adjusted_score(a, weight))
                .then_with(|| stable_candidate_id(a).cmp(&stable_candidate_id(b)))
        });

        for (slot, item) in slots.into_iter().zip(items.into_iter()) {
            cluster[slot] = item;
        }
    }
}
```

The important invariant is "sort same-order subsequences into their existing same-order slots". In a mixed cluster like `[judicial, admin, judicial]`, only the two judicial candidates may swap; the administrative slot remains where relevance placed it. Unknown/non-decision candidates whose `authority_tier` is `None` stay in their exact slot and should not be used as authority-sort items.

Rows with `marker_absent=true` should still participate with their assigned tier. `marker_absent` is provenance/honesty, not an exclusion flag. That means `cass` with absent publication can still use tier 2, `jade` with absent class can use tier 0, and `capp` can use tier 0. Eval code may choose to exclude marker-absent pairs from a judgement set, but production reranking should remain the deterministic source/tier mapping above.

Use the same rounded fused score as the cursor path (`round(s, 8)`) for cluster membership and adjusted scoring. For ties, use the stable id that matches the candidate shape's existing final tiebreaker: chunk id for chunk candidates, zone unit id for zone-unit candidates, otherwise document id. If none exists, fall back to the original index only as a last-resort deterministic guard.
