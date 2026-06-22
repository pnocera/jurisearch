#!/usr/bin/env python3
"""Compare embedding models on a small French-legal retrieval benchmark.

Model-agnostic: every model is reached through an OpenAI-compatible
POST {base_url}/embeddings. Each model must be served with ITS correct pooling
(bge-m3 = cls via llama.cpp; sentence-transformers models apply their own).

Pure standard library (urllib + math) — no numpy / requests needed.

Usage:
    python3 eval.py                      # uses endpoints.json / corpus.jsonl / queries.jsonl
    python3 eval.py --k 10 --out results.json
"""
import argparse, json, math, sys, time, urllib.request, urllib.error

def load_jsonl(path):
    rows = []
    with open(path, encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if line:
                rows.append(json.loads(line))
    return rows

def embed(base_url, model, texts, batch=32, timeout=180):
    """Return a list of vectors (one per text), order preserved."""
    out = [None] * len(texts)
    url = base_url.rstrip("/") + "/embeddings"
    i = 0
    while i < len(texts):
        chunk = texts[i:i + batch]
        body = json.dumps({"model": model, "input": chunk}).encode("utf-8")
        req = urllib.request.Request(url, data=body, headers={"Content-Type": "application/json"})
        with urllib.request.urlopen(req, timeout=timeout) as r:
            data = json.load(r)
        items = sorted(data["data"], key=lambda d: d.get("index", 0))
        if len(items) != len(chunk):
            raise RuntimeError(f"endpoint returned {len(items)} vectors for {len(chunk)} inputs")
        for j, it in enumerate(items):
            out[i + j] = it["embedding"]
        i += batch
    return out

def normalize(v):
    n = math.sqrt(sum(x * x for x in v)) or 1.0
    return [x / n for x in v]

def cosine(a, b):  # a, b assumed normalized
    return sum(x * y for x, y in zip(a, b))

def dcg(rels):
    return sum(rel / math.log2(idx + 2) for idx, rel in enumerate(rels))

def evaluate(endpoint, corpus, queries, k):
    doc_ids = [d["id"] for d in corpus]
    qp = endpoint.get("query_prefix", "")   # e.g. Solon needs "query : " on queries
    dp = endpoint.get("doc_prefix", "")     # passage prefix if a model needs one
    doc_vecs = [normalize(v) for v in embed(endpoint["base_url"], endpoint["model"], [dp + d["text"] for d in corpus])]
    q_vecs = [normalize(v) for v in embed(endpoint["base_url"], endpoint["model"], [qp + q["query"] for q in queries])]
    dim = len(doc_vecs[0])
    per_query = []
    for q, qv in zip(queries, q_vecs):
        sims = sorted(((cosine(qv, dv), did) for dv, did in zip(doc_vecs, doc_ids)), reverse=True)
        ranking = [did for _, did in sims]
        gold = set(q["relevant_ids"])
        gold_rank = next((r for r, did in enumerate(ranking, 1) if did in gold), None)
        rr = 1.0 / gold_rank if gold_rank else 0.0
        top = ranking[:k]
        rels = [1 if did in gold else 0 for did in top]
        idcg = dcg(sorted([1] * len(gold), reverse=True)[:k]) or 1.0
        per_query.append({
            "id": q["id"], "category": q.get("category", "?"),
            "gold_rank": gold_rank, "rr": rr,
            "hit@1": int(bool(gold_rank == 1)),
            "hit@5": int(bool(gold_rank and gold_rank <= 5)),
            "hit@10": int(bool(gold_rank and gold_rank <= 10)),
            "ndcg@%d" % k: dcg(rels) / idcg,
            "top": top[:5],
        })
    n = len(per_query)
    agg = {
        "dim": dim,
        "MRR@10": sum(p["rr"] for p in per_query) / n,
        "Recall@1": sum(p["hit@1"] for p in per_query) / n,
        "Recall@5": sum(p["hit@5"] for p in per_query) / n,
        "Recall@10": sum(p["hit@10"] for p in per_query) / n,
        "nDCG@%d" % k: sum(p["ndcg@%d" % k] for p in per_query) / n,
    }
    return {"endpoint": endpoint, "aggregate": agg, "per_query": per_query}

def sign_test_p(wins, losses):
    """Two-sided sign test p-value (exact binomial, p=0.5)."""
    n = wins + losses
    if n == 0:
        return 1.0
    k = min(wins, losses)
    c = lambda a, b: math.comb(a, b)
    tail = sum(c(n, i) for i in range(0, k + 1)) / (2 ** n)
    return min(1.0, 2 * tail)

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--endpoints", default="endpoints.json")
    ap.add_argument("--corpus", default="corpus.jsonl")
    ap.add_argument("--queries", default="queries.jsonl")
    ap.add_argument("--k", type=int, default=10)
    ap.add_argument("--out", default="results.json")
    args = ap.parse_args()

    corpus = load_jsonl(args.corpus)
    queries = load_jsonl(args.queries)
    eps = json.load(open(args.endpoints, encoding="utf-8"))["endpoints"]
    print(f"corpus={len(corpus)} passages | queries={len(queries)} | k={args.k}\n")

    results = []
    for ep in eps:
        sys.stdout.write(f"→ {ep['name']:<32} ")
        sys.stdout.flush()
        try:
            t0 = time.time()
            res = evaluate(ep, corpus, queries, args.k)
            res["seconds"] = round(time.time() - t0, 1)
            results.append(res)
            a = res["aggregate"]
            print(f"ok  dim={a['dim']}  MRR@10={a['MRR@10']:.3f}  R@1={a['Recall@1']:.3f}  ({res['seconds']}s)")
        except (urllib.error.URLError, ConnectionError, OSError) as e:
            print(f"SKIP (endpoint unreachable: {e})")
        except Exception as e:  # noqa
            print(f"ERROR ({type(e).__name__}: {e})")

    if not results:
        print("\nNo endpoints answered. Start the servers (see README) and re-run.")
        return

    # summary table
    print("\n" + "=" * 78)
    print(f"{'model':<34}{'MRR@10':>9}{'R@1':>7}{'R@5':>7}{'R@10':>7}{'nDCG':>7}")
    print("-" * 78)
    for r in results:
        a = r["aggregate"]
        print(f"{r['endpoint']['name']:<34}{a['MRR@10']:>9.3f}{a['Recall@1']:>7.3f}"
              f"{a['Recall@5']:>7.3f}{a['Recall@10']:>7.3f}{a['nDCG@%d' % args.k]:>7.3f}")

    # head-to-head: baseline (first endpoint, e.g. bge-m3) vs each of the others
    if len(results) >= 2:
        A = results[0]
        qa = {p["id"]: p for p in A["per_query"]}
        for B in results[1:]:
            qb = {p["id"]: p for p in B["per_query"]}
            wins = losses = ties = 0
            detail = []
            for qid in qa:
                ra = qa[qid]["gold_rank"] or 9999
                rb = qb[qid]["gold_rank"] or 9999
                if ra < rb: wins += 1; w = "A"
                elif rb < ra: losses += 1; w = "B"
                else: ties += 1; w = "="
                if ra != rb:
                    detail.append((qid, ra, rb, w))
            p = sign_test_p(wins, losses)
            print("\n" + "=" * 78)
            print(f"HEAD-TO-HEAD  A={A['endpoint']['name']}   vs   B={B['endpoint']['name']}")
            print(f"  per-query gold-rank: A better={wins}  B better={losses}  tie={ties}"
                  f"   (sign-test p={p:.3f}, N={len(qa)})")
            verdict = ("≈ statistically indistinguishable on this set"
                       if p > 0.10 else
                       ("A is better" if wins > losses else "B is better"))
            print(f"  verdict: {verdict}")
            if detail:
                print("  disagreements (gold rank A|B):")
                for qid, ra, rb, w in sorted(detail, key=lambda d: abs(d[1] - d[2]), reverse=True):
                    print(f"    {qid:<24} A={ra if ra<9999 else '—':<5} B={rb if rb<9999 else '—':<5} -> {w}")

    json.dump(results, open(args.out, "w", encoding="utf-8"), ensure_ascii=False, indent=2)
    print(f"\nwrote {args.out}")

if __name__ == "__main__":
    main()
