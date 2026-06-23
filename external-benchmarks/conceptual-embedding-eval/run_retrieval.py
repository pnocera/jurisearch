#!/usr/bin/env python3
"""Conceptual-query embedding ablation — retrieval stage.

Each question carries a `source` tag: "codex" (lay questions Codex wrote from the seed) and
"authored" (independently phrased by the engineer). Both phrasings target the same seed topic, so
the comparison can be sliced by source. For each question, run the production search in
bm25 / dense / hybrid modes and record:
  - whether the SEED article (the one the question targets) is in top-k                [objective]
  - the pooled candidate set across all three modes                                    [for LLM judging]

EVALUATION UNIT = DOCUMENT (article). The CLI returns chunk-level candidates, so we fetch deeper
(`--fetch-k`, default 4x top_k) and dedupe by source article UID preserving rank order, taking the
first `top_k` UNIQUE documents as each mode's top-k. (Without this, an article with several matching
chunks would occupy multiple top-k slots and double-count in P@k / recall / nDCG.)

Robustness: a search is only "no results" when the CLI says so — and the CLI turns empty candidates
into an ERROR (`no_results`), exiting non-zero with `{"ok":false,"error":...}`. So ANY non-zero exit,
non-JSON stdout, error envelope, or missing/empty candidates is a HARD FAILURE that aborts the run
(never silently recorded as an empty pool). We also assert the response actually used the requested
retriever and that conceptual questions are NOT diverted by citation routing.

This faithfully exercises the real retrieval pipeline (no Python reimplementation) by shelling out
to `jurisearch search --mode {bm25,dense,hybrid}`.

Usage:
  python3 run_retrieval.py --index-dir <ABS> --questions questions.json --seeds seeds.json \
      --top-k 10 --fetch-k 40 --as-of 2026-06-23 --out retrieval.json
"""
import argparse, json, os, subprocess, sys

MODES = ["bm25", "dense", "hybrid"]


class SearchError(Exception):
    pass


def uid_of(document_id: str):
    # legi:LEGIARTI...@YYYY-MM-DD -> LEGIARTI...
    if not document_id or not document_id.startswith("legi:"):
        return None
    return document_id[len("legi:"):].split("@", 1)[0]


def run_search(binary, index_dir, mode, fetch_k, as_of, query):
    """Run one search; raise SearchError on ANY failure. Returns the parsed response dict."""
    cmd = [binary, "search", "--index-dir", index_dir, "--kind", "code",
           "--mode", mode, "--top-k", str(fetch_k), "--as-of", as_of, query]
    # Inherit the full environment (HOME, PATH, ...) so the binary can locate ~/.pgrx and start
    # embedded PG; only override the PISTE retry knob.
    env = dict(os.environ)
    env["JURISEARCH_PISTE_MAX_RETRIES"] = "0"
    p = subprocess.run(cmd, capture_output=True, text=True, env=env)

    def fail(msg):
        raise SearchError(
            f"{msg}\n    mode={mode} rc={p.returncode} query={query!r}\n"
            f"    stderr: {(p.stderr or '').strip()[-400:]}\n"
            f"    stdout: {(p.stdout or '').strip()[-400:]}")

    if p.returncode != 0:
        fail("CLI exited non-zero")
    try:
        d = json.loads(p.stdout)
    except Exception as e:
        fail(f"stdout is not valid JSON ({e})")
    if isinstance(d, dict) and (d.get("ok") is False or "error" in d):
        fail(f"CLI returned an error envelope: {d.get('error')}")
    cands = d.get("candidates")
    if not isinstance(cands, list) or not cands:
        fail("response had no candidates (CLI treats empty as an error, so this is a real failure)")

    # Assert we actually exercised the requested retriever and were not diverted by citation routing.
    rmode = d.get("retrieval_mode")
    if rmode != mode:
        fail(f"retrieval_mode={rmode!r} != requested mode={mode!r}")
    routing = d.get("routing") or {}
    qt, backend = routing.get("query_type"), routing.get("chosen_backend")
    if qt != "semantic":
        fail(f"conceptual question routed as query_type={qt!r} (citation-shaped?) — rephrase it")
    if backend != mode:
        fail(f"chosen_backend={backend!r} != mode={mode!r} (unexpected routing/fallback)")
    return d


def dedupe_to_docs(cands, k):
    """Chunk-level candidates -> first k UNIQUE documents, preserving rank order."""
    seen, docs = set(), []
    for c in cands:
        uid = uid_of(c.get("document_id") or "")
        if not uid or uid in seen:
            continue
        seen.add(uid)
        docs.append({
            "source_uid": uid,
            "citation": c.get("citation"),
            "title": c.get("title"),
            "snippet": c.get("snippet"),
        })
        if len(docs) >= k:
            break
    return docs


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--index-dir", required=True)
    ap.add_argument("--binary", default="./target/debug/jurisearch")
    ap.add_argument("--questions", required=True)
    ap.add_argument("--seeds", required=True)
    ap.add_argument("--top-k", type=int, default=10)
    ap.add_argument("--fetch-k", type=int, default=None, help="chunks to fetch before doc-dedup (default 4x top-k)")
    ap.add_argument("--as-of", default="2026-06-23")
    ap.add_argument("--out", required=True)
    a = ap.parse_args()
    fetch_k = a.fetch_k if a.fetch_k else a.top_k * 4

    questions = json.load(open(a.questions))
    seeds = {s["document_id"]: s for s in json.load(open(a.seeds))}

    results = []
    for q in questions:
        did = q["document_id"]
        seed = seeds.get(did)
        seed_uid = uid_of(did)
        question = q["question"]
        qid = q.get("id")
        qsource = q.get("source")
        per_mode = {}
        pool = {}  # source_uid -> doc fields (deduped across modes)
        for mode in MODES:
            d = run_search(a.binary, a.index_dir, mode, fetch_k, a.as_of, question)
            docs = dedupe_to_docs(d["candidates"], a.top_k)
            uids = [x["source_uid"] for x in docs]
            assert len(uids) == len(set(uids)), f"dup uids in {mode} top_uids for {qid}"
            per_mode[mode] = {
                "top_uids": uids,
                "seed_in_topk": seed_uid in uids,
                "retrieval_mode": d.get("retrieval_mode"),
                "routing": d.get("routing"),
            }
            for x in docs:
                pool.setdefault(x["source_uid"], x)
        results.append({
            "id": qid,
            "source": qsource,
            "document_id": did,
            "seed_uid": seed_uid,
            "seed_citation": (seed or {}).get("citation"),
            "question": question,
            "modes": per_mode,
            "pool": list(pool.values()),
        })
        sys.stderr.write(f"  q done: {qid or did} [{qsource}] (pool={len(pool)})\n")

    json.dump(results, open(a.out, "w"), ensure_ascii=False, indent=2)
    # Objective headline: seed-recall@k per mode, overall and sliced by question source.
    def recall_table(rows, label):
        n = len(rows)
        if not n:
            return
        print(f"=== seed-recall@{a.top_k} (doc-level) over {n} {label} ===")
        for mode in MODES:
            hits = sum(1 for r in rows if r["modes"][mode]["seed_in_topk"])
            print(f"  {mode:7} {hits}/{n} = {hits/n:.3f}")
    recall_table(results, "conceptual questions (all)")
    for src in sorted({r.get("source") for r in results if r.get("source")}):
        recall_table([r for r in results if r.get("source") == src], f"questions [source={src}]")


if __name__ == "__main__":
    main()
