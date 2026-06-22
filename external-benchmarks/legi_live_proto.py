#!/usr/bin/env python3
"""France-LEGI known-item + temporal benchmark — FULL run through the LIVE production pipeline.

SUPERSEDED (2026-06-22): use `jurisearch eval phase1` for the live known-item/temporal gate
instead of this ad-hoc 100-query `session` driver. Reason (verified in source): `search_payload`
cold-starts embedded PG per query (`crates/jurisearch-cli/src/main.rs:770`,`:2763`), so a
100-query `session` sweep is hours of repeated cold starts and timed out. A codex review of this
script also flagged metric-counting bugs (no_results excluded from the denominator; chunk-vs-
document dedupe). Kept only as a record of the attempt. To scale beyond the curated `eval phase1`
fixtures, add a persistent-PG batch-eval path (one PG lifecycle, N queries). See
`work/03-implementation/02-evidence/2026-06-22-france-legi-official-evidence-benchmark-feasibility.md`.

Gold from OFFICIAL evidence only (no human, no LLM):
  - known-item gold = legi:{LEGIARTI}@{DATE_DEBUT}, retrieved with --as-of=DATE_DEBUT so the
    in-force version is version-matched to the query (otherwise we'd score today's version
    against an archived version id and count a correct hit as a miss).
  - temporal gold = the RC R*242-40 version chain; the version whose official
    [DATE_DEBUT, DATE_FIN) window contains the as-of date.
Retrieval = the real `jurisearch search` (BM25 + dense + RRF) over the 1.85M-chunk index,
driven through a warm `session --jsonl`. Run with `python3 -u` so progress is unbuffered.
"""
import subprocess, time, os, json, sys, re, threading, queue, tarfile
import xml.etree.ElementTree as ET

IDX = "/home/pierre/Work/jurisearch/index/phase1-freemium-20250713"
os.makedirs(f"{IDX}/pg/sock", exist_ok=True)   # jurisearch derives the socket dir from --index-dir
ARCH = "index/phase1-eval-archives/Freemium_legi_global_20250713-140000.tar.gz"
N_KNOWN = 100
SCAN_MAX = 6000   # archive members to read while collecting N_KNOWN gold articles
WARM_T = 300      # cold PG start + ANN load over 1.85M chunks can take minutes
Q_T = 90          # per-query response timeout once warm

def ftext(r, t):
    e = r.find(f".//{t}")
    return (e.text or "").strip() if e is not None and e.text else ""

print("parsing official archive front for known-item gold ...", flush=True)
items = []
n = 0
with tarfile.open(ARCH, "r:gz") as tf:
    for m in tf:
        if not (m.isfile() and "/article/" in m.name and m.name.endswith(".xml")):
            continue
        n += 1
        if n > SCAN_MAX or len(items) >= N_KNOWN:
            break
        f = tf.extractfile(m)
        if not f:
            continue
        try:
            root = ET.fromstring(f.read())
        except Exception:
            continue
        aid = ftext(root, "ID"); num = ftext(root, "NUM"); dd = ftext(root, "DATE_DEBUT")
        if not (aid.startswith("LEGIARTI") and num and re.match(r"\d{4}-\d{2}-\d{2}$", dd)):
            continue
        titles = [(e.text or "").strip() for e in root.iter() if e.tag == "TITRE_TXT" and e.text]
        code = titles[0] if titles else ""
        if not code:
            continue
        items.append({"gold": f"legi:{aid}@{dd}", "query": f"{code} article {num}", "asof": dd})
items = items[:N_KNOWN]
print(f"  built {len(items)} known-item queries (gold = legi:LEGIARTI@DATE_DEBUT, as-of pinned)\n", flush=True)
if not items:
    print("no known-item gold collected; aborting", flush=True); sys.exit(1)

# Temporal cases from the RC fixtures (official R*242-40 version chain)
TEMPORAL = [
    ("Article R*242-40 du code rural sur les contraventions dans une reserve naturelle",
     "1990-01-01", "legi:LEGIARTI000006590697@1989-11-04", ["legi:LEGIARTI000006590698@2003-08-07"]),
    ("Article R*242-40 du code rural code de deontologie veterinaire",
     "2003-09-01", "legi:LEGIARTI000006590698@2003-08-07", ["legi:LEGIARTI000006590697@1989-11-04"]),
]

env = dict(os.environ)
env.setdefault("JURISEARCH_EMBED_BASE_URL", "http://127.0.0.1:8097/v1")
env["JURISEARCH_INDEX_DIR"] = IDX
sess = subprocess.Popen(["target/debug/jurisearch", "--index-dir", IDX, "session", "--jsonl"],
    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, env=env, bufsize=1)
outq = queue.Queue()
threading.Thread(target=lambda: [outq.put(l) for l in sess.stdout], daemon=True).start()
errlines = []
threading.Thread(target=lambda: [errlines.append(l) for l in sess.stderr], daemon=True).start()

def send(o):
    sess.stdin.write(json.dumps(o) + "\n"); sess.stdin.flush()

def recv(want, timeout):
    end = time.time() + timeout
    while time.time() < end:
        try:
            line = outq.get(timeout=max(0.1, end - time.time()))
        except queue.Empty:
            return None
        try:
            j = json.loads(line)
            if j.get("id") == want:
                return j
        except Exception:
            pass
    return None

def search(qid, query, top_k=10, as_of=None, kind=None, timeout=Q_T):
    args = {"query": query, "top_k": top_k}
    if as_of:
        args["as_of"] = as_of
    if kind:
        args["kind"] = kind
    send({"id": qid, "command": "search", "args": args})
    r = recv(qid, timeout)
    if not r or not r.get("ok"):
        return None
    docs = []
    for c in r["result"].get("candidates", []):
        d = c.get("document_id")
        if d and d not in docs:
            docs.append(d)
    return docs

t_all = time.time()
try:
    print("warming session (cold PG start + ANN load) ...", flush=True)
    tw = time.time()
    if search("warm", "responsabilite du fait des produits defectueux", top_k=3, timeout=WARM_T) is None:
        print("warm search failed; stderr tail:\n" + "".join(errlines[-12:]), flush=True)
        sys.exit(1)
    print(f"  session warm in {time.time()-tw:.0f}s\n", flush=True)

    # ---- known-item ----
    hit = 0; ranks = []; done = 0; absent = 0; errs = 0
    for i, it in enumerate(items):
        docs = search(f"k{i}", it["query"], top_k=10, as_of=it["asof"])
        if docs is None:
            errs += 1
            print(f"  [known {i+1}/{len(items)}] search error", flush=True)
            continue
        done += 1
        r = next((j for j, d in enumerate(docs, 1) if d == it["gold"]), None)
        if r:
            ranks.append(r)
            if r <= 10:
                hit += 1
        else:
            absent += 1
        if (i + 1) % 10 == 0:
            print(f"  [known {i+1}/{len(items)}] hit@10 so far {hit}/{done}", flush=True)
    print()
    print("=" * 64)
    print(f"KNOWN-ITEM (live production search, as-of pinned)  —  {done} queries  ({errs} errors)")
    print("=" * 64)
    if done:
        print(f"  Recall@10 (exact version) : {hit}/{done} = {hit/done:.3f}")
        print(f"  MRR@10                    : {sum(1/r for r in ranks)/done:.3f}")
        print(f"  gold absent from top-10   : {absent}/{done}")
    else:
        print("  no successful queries")
    print(flush=True)

    # ---- temporal as-of ----
    print("=" * 64)
    print("TEMPORAL as-of (live production search, --as-of prefilter)")
    print("=" * 64)
    tcorrect = 0
    for q, asof, gold, wrong in TEMPORAL:
        docs = search(f"t_{asof}", q, top_k=10, as_of=asof)
        gold_in = gold in (docs or [])
        wrong_in = any(w in (docs or []) for w in wrong)
        ok = gold_in and not wrong_in
        tcorrect += 1 if ok else 0
        gr = (docs.index(gold) + 1) if docs and gold in docs else None
        print(f"  as_of {asof}: gold@{gr if gr else 'absent'}  wrong-version-present={wrong_in}  -> {'OK' if ok else 'check'}", flush=True)
    print(f"  temporal-correct: {tcorrect}/{len(TEMPORAL)}\n")

    print(f"[total run {time.time()-t_all:.0f}s]")
    print("INTERPRETATION: gold built from official LEGIARTI@DATE_DEBUT + official validity windows")
    print("(no human, no LLM); retrieval is the real production BM25+dense+RRF pipeline, with --as-of")
    print("exercising the temporal prefilter. This is a France-LEGI gate executing the production path.")
finally:
    try:
        send({"command": "exit"}); sess.wait(timeout=25)
    except Exception:
        sess.terminate()
        try:
            sess.wait(timeout=10)
        except Exception:
            sess.kill()
    print("\nsession closed (PG shut down)", flush=True)
