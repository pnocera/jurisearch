#!/usr/bin/env python3
"""Build the BLIND judging task for Codex (the LLM relevance judge).

From retrieval.json (which holds, per question, the deduped DOCUMENT pool across bm25/dense/hybrid),
emit:
  - judge_input.json : what Codex sees — per question, the question text + its candidates with
                       OPAQUE per-question keys (c01, c02, ...) and only title+snippet. No seed,
                       no retriever attribution, no LEGIARTI id. So Codex cannot tell which
                       retriever found a candidate, nor which doc the question was seeded from.
  - judge_keymap.json: PRIVATE (not given to Codex) — maps (question_id, key) -> source_uid, so we
                       map Codex's labels back onto each retriever's top-k afterwards.

Candidate ORDER is randomized per question with a deterministic seed derived from question_id + a
recorded salt, BEFORE keys are assigned. The retrieval pool is built by iterating modes in a fixed
order (bm25, dense, hybrid), so without shuffling the earliest keys would skew lexical — leaking
provenance and inviting position bias in the judge. The shuffle is reproducible (salt is stored in
judge_input meta) and the key->uid map is preserved, so scoring is unaffected.

Usage: python3 build_judge_input.py --retrieval retrieval.json \
         --judge-input judge_input.json --keymap judge_keymap.json [--salt conceptual-eval-2026-06-23]
"""
import argparse, hashlib, json, random


def seed_for(question_id, salt):
    # Stable 64-bit seed from (question_id, salt) — independent of Python's hash randomization.
    h = hashlib.sha256(f"{salt}|{question_id}".encode()).hexdigest()
    return int(h[:16], 16)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--retrieval", required=True)
    ap.add_argument("--judge-input", required=True)
    ap.add_argument("--keymap", required=True)
    ap.add_argument("--salt", default="conceptual-eval-2026-06-23")
    a = ap.parse_args()

    results = json.load(open(a.retrieval))
    judge_in, keymap = [], {}
    for r in results:
        qid = r["id"]
        cands = list(r["pool"])
        random.Random(seed_for(qid, a.salt)).shuffle(cands)  # deterministic, provenance-neutral order
        items, qmap = [], {}
        for i, c in enumerate(cands, 1):
            key = f"c{i:02d}"
            qmap[key] = c["source_uid"]
            items.append({"key": key, "title": c.get("title"), "snippet": c.get("snippet")})
        judge_in.append({"question_id": qid, "question": r["question"], "candidates": items})
        keymap[qid] = qmap

    # judge_input.json stays a PLAIN ARRAY (what the judge prompt iterates). The reproducibility
    # salt goes in a sidecar so the blind ordering is auditable without changing the judge's schema.
    json.dump(judge_in, open(a.judge_input, "w"), ensure_ascii=False, indent=2)
    json.dump(keymap, open(a.keymap, "w"), ensure_ascii=False, indent=2)
    sidecar = a.judge_input.rsplit(".", 1)[0] + "_meta.json"
    json.dump({"salt": a.salt, "n_questions": len(judge_in)}, open(sidecar, "w"), indent=2)
    npairs = sum(len(q["candidates"]) for q in judge_in)
    print(f"judge_input: {len(judge_in)} questions, {npairs} (question,candidate) pairs to judge "
          f"(shuffled with salt={a.salt!r})")


if __name__ == "__main__":
    main()
