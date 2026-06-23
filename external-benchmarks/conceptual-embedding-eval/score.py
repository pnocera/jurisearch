#!/usr/bin/env python3
"""Score the conceptual-retrieval ablation from Codex's relevance judgments.

Inputs:
  retrieval.json    : per question, each mode's ordered top_uids (doc-level) + the pooled candidates
  judge_output.json : Codex's labels  {question_id: {candidate_key: 0|1|2}}
                        0 = unrelated, 1 = related/partial, 2 = directly answers
  judge_keymap.json : {question_id: {candidate_key: source_uid}}  (maps labels back to docs)

Per question we know, for every pooled doc, Codex's graded relevance. We score each retriever
(bm25/dense/hybrid) on its own top-10:
  P@10        fraction of top-10 judged relevant (label>=REL_MIN)
  recall@10   pooled recall: |top10 ∩ Relevant| / |Relevant in pool|   (None if no relevant in pool)
  nDCG@10     graded, gain = 2^label-1, ideal from the pooled labels
And the decisive ablation signal:
  rel-only-dense / rel-only-bm25 : relevant docs one retriever returns that the other does not.

Pool = union of all three retrievers' top-10, so Relevant ⊆ pool: "recall" is recall *of the
relevant docs some retriever surfaced*, not absolute corpus recall.

Uncertainty: the two phrasings of a seed are NOT independent, so the PRIMARY aggregate is
seed-clustered (average within seed, then across the 12 seeds), and 95% CIs for between-retriever
deltas are bootstrapped by RESAMPLING SEEDS (not individual phrasings).

Usage: python3 score.py --retrieval retrieval.json --judgments judge_output.json \
         --keymap judge_keymap.json [--rel-min 1] [--bootstrap 5000] [--allow-missing-as-zero]
"""
import argparse, json, math
from collections import defaultdict

MODES = ["bm25", "dense", "hybrid"]
METRICS = ["p", "recall", "ndcg"]


def dcg(labels):
    return sum((2 ** g - 1) / math.log2(i + 2) for i, g in enumerate(labels))


def ndcg_at(ordered_labels, pool_labels, k):
    actual = dcg(ordered_labels[:k])
    ideal = dcg(sorted(pool_labels, reverse=True)[:k])
    return (actual / ideal) if ideal > 0 else 0.0


def seed_of(qid):
    return qid.split("-", 1)[0]  # "s01-codex" -> "s01"


def validate_completeness(keymap, judg, allow_missing):
    problems = []
    for qid, km in keymap.items():
        labels = judg.get(qid, {})
        for key in km:
            if key not in labels:
                problems.append(f"missing label: {qid}/{key}")
            elif labels[key] not in (0, 1, 2):
                problems.append(f"bad label {labels[key]!r}: {qid}/{key}")
        for key in labels:
            if key not in km:
                problems.append(f"extra label: {qid}/{key}")
    for qid in judg:
        if qid not in keymap:
            problems.append(f"extra question in judgments: {qid}")
    if problems and not allow_missing:
        raise SystemExit("JUDGMENT VALIDATION FAILED ({} problems):\n  {}".format(
            len(problems), "\n  ".join(problems[:20])))
    return problems


def mean(xs):
    xs = [x for x in xs if x is not None]
    return (sum(xs) / len(xs)) if xs else None


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--retrieval", required=True)
    ap.add_argument("--judgments", required=True)
    ap.add_argument("--keymap", required=True)
    ap.add_argument("--rel-min", type=int, default=1, help="min label counted as relevant (1 or 2)")
    ap.add_argument("--k", type=int, default=10)
    ap.add_argument("--bootstrap", type=int, default=5000)
    ap.add_argument("--allow-missing-as-zero", action="store_true",
                    help="treat missing labels as 0 instead of failing (exploratory only)")
    a = ap.parse_args()

    results = json.load(open(a.retrieval))
    judg = json.load(open(a.judgments))
    keymap = json.load(open(a.keymap))

    probs = validate_completeness(keymap, judg, a.allow_missing_as_zero)
    if probs:
        print(f"!! WARNING: {len(probs)} judgment-completeness problems (missing treated as 0)")

    # Per-question, per-mode metrics; remember each question's source and seed.
    rows = []  # dict per question
    for r in results:
        qid = r["id"]
        labels_by_uid = {uid: int(judg.get(qid, {}).get(key, 0)) for key, uid in keymap.get(qid, {}).items()}
        pool_uids = [c["source_uid"] for c in r["pool"]]
        pool_labels = [labels_by_uid.get(u, 0) for u in pool_uids]
        relevant = {u for u in pool_uids if labels_by_uid.get(u, 0) >= a.rel_min}
        per_mode = {}
        for mode in MODES:
            top = r["modes"][mode]["top_uids"][:a.k]
            top_labels = [labels_by_uid.get(u, 0) for u in top]
            n_rel_top = sum(1 for u in top if u in relevant)
            per_mode[mode] = {
                "p": (n_rel_top / len(top)) if top else 0.0,
                "recall": (n_rel_top / len(relevant)) if relevant else None,
                "ndcg": ndcg_at(top_labels, pool_labels, a.k),
                "rel_returned": {u for u in top if u in relevant},
            }
        rows.append({"qid": qid, "seed": seed_of(qid), "source": r.get("source"),
                     "n_rel": len(relevant), "modes": per_mode})

    print(f"# rel-min={a.rel_min} (>= this label counts as relevant); k={a.k}; "
          f"recall = POOLED recall (within the depth-{a.k} union of retrievers), NOT absolute recall.")

    # ---- point-estimate tables (per slice) + relevant-set diffs ----
    def point_table(subset, label):
        if not subset:
            return
        n = len(subset)
        n_with_rel = sum(1 for x in subset if x["n_rel"] > 0)
        print(f"\n=== {label}  ({n} questions; {n_with_rel} with >=1 relevant doc in pool) ===")
        print(f"  {'mode':7} {'P@'+str(a.k):>7} {'recall@'+str(a.k):>10} {'nDCG@'+str(a.k):>9}")
        for mode in MODES:
            mp = mean([x['modes'][mode]['p'] for x in subset])
            mr = mean([x['modes'][mode]['recall'] for x in subset])
            mn = mean([x['modes'][mode]['ndcg'] for x in subset])
            print(f"  {mode:7} {mp:7.3f} {(float('nan') if mr is None else mr):10.3f} {mn:9.3f}")
        only_d = only_b = both = 0
        for x in subset:
            d, b = x['modes']['dense']['rel_returned'], x['modes']['bm25']['rel_returned']
            only_d += len(d - b); only_b += len(b - d); both += len(d & b)
        print(f"  relevant docs (pooled over questions): dense-only={only_d}  bm25-only={only_b}  both={both}")

    point_table(rows, "ALL questions (per-question mean)")
    for src in sorted({x['source'] for x in rows if x['source']}):
        point_table([x for x in rows if x['source'] == src], f"source={src} (per-question mean)")

    # ---- seed-clustered primary aggregate ----
    seeds = sorted({x['seed'] for x in rows})

    def seed_value(seed, mode, metric):
        # mean of this seed's question values for (mode, metric); None if undefined (recall w/o rel)
        return mean([x['modes'][mode][metric] for x in rows if x['seed'] == seed])

    def clustered_mean(seed_list, mode, metric):
        return mean([seed_value(s, mode, metric) for s in seed_list])

    print(f"\n=== SEED-CLUSTERED primary aggregate ({len(seeds)} seeds; 2 phrasings averaged per seed) ===")
    print(f"  {'mode':7} {'P@'+str(a.k):>7} {'recall@'+str(a.k):>10} {'nDCG@'+str(a.k):>9}")
    for mode in MODES:
        vals = {m: clustered_mean(seeds, mode, m) for m in METRICS}
        print(f"  {mode:7} {vals['p']:7.3f} {(float('nan') if vals['recall'] is None else vals['recall']):10.3f} {vals['ndcg']:9.3f}")

    # ---- bootstrap 95% CIs for between-retriever deltas, resampling SEEDS ----
    import random
    rng = random.Random(20260623)
    B = a.bootstrap

    def boot_delta_ci(mode_a, mode_b, metric):
        point = clustered_mean(seeds, mode_a, metric) - clustered_mean(seeds, mode_b, metric)
        deltas = []
        for _ in range(B):
            sample = [rng.choice(seeds) for _ in seeds]
            va = clustered_mean(sample, mode_a, metric)
            vb = clustered_mean(sample, mode_b, metric)
            if va is not None and vb is not None:
                deltas.append(va - vb)
        deltas.sort()
        lo = deltas[int(0.025 * len(deltas))]
        hi = deltas[min(len(deltas) - 1, int(0.975 * len(deltas)))]
        return point, lo, hi

    print(f"\n=== bootstrap 95% CI for delta (seed-resampled, B={B}); CI excluding 0 => significant ===")
    for metric in ("ndcg", "recall", "p"):
        print(f"  metric={metric}@{a.k}")
        for a_mode, b_mode in (("dense", "bm25"), ("dense", "hybrid"), ("hybrid", "bm25")):
            pt, lo, hi = boot_delta_ci(a_mode, b_mode, metric)
            sig = "" if (lo <= 0 <= hi) else "  *"
            print(f"    {a_mode:6} - {b_mode:6}: {pt:+.3f}  95%CI [{lo:+.3f}, {hi:+.3f}]{sig}")


if __name__ == "__main__":
    main()
