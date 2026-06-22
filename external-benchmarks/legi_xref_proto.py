#!/usr/bin/env python3
"""Prototype: a France-LEGI *cross-reference* retrieval benchmark built entirely from
OFFICIAL LEGI evidence (no human annotator), run through bge-m3 dense retrieval.

Official evidence used as ground truth:
  - <LIENS><LIEN id="LEGIARTI..." typelien="CITATION" sens="cible">  -> article A officially
    references article B. That LIEN *is* the relevance judgment (the legislator put it there).

Task: given article A, retrieve the articles A officially cites. qrels = A's CITATION targets.
Source: the official DILA LEGI archive (Licence Ouverte). bge-m3 via OpenRouter.
"""
import os, re, sys, time, json, math, queue, threading, tarfile
import urllib.request
import xml.etree.ElementTree as ET

ARCH = "index/phase1-eval-archives/Freemium_legi_global_20250713-140000.tar.gz"
SCAN = 6000           # article members to read from the front of the archive
MAXQ = 80             # queries (source articles with in-corpus citation targets)
K = 10

def ftext(r, t):
    e = r.find(f".//{t}")
    return (e.text or "").strip() if e is not None and e.text else ""

print(f"scanning official archive front ({SCAN} articles): {ARCH}")
arts = {}           # id -> {ctx, targets}
order = []
n = 0; t0 = time.time()
with tarfile.open(ARCH, "r:gz") as tf:
    for m in tf:
        if not (m.isfile() and "/article/" in m.name and m.name.endswith(".xml")): continue
        n += 1
        if n > SCAN: break
        f = tf.extractfile(m)
        if not f: continue
        try: root = ET.fromstring(f.read())
        except Exception: continue
        aid = ftext(root, "ID"); num = ftext(root, "NUM")
        if not aid.startswith("LEGIARTI"): continue
        bt = root.find(".//BLOC_TEXTUEL")
        body = re.sub(r"\s+", " ", " ".join(bt.itertext())).strip() if bt is not None else ""
        if not body: continue
        titles = [(e.text or "").strip() for e in root.iter() if e.tag in ("TITRE_TXT","TITRE_TM") and e.text]
        hierarchy = " > ".join(t for t in titles[:4] if t)
        targets = []
        L = root.find(".//LIENS")
        if L is not None:
            for ln in L.findall("LIEN"):
                tid = ln.get("id",""); typ = ln.get("typelien",""); sens = ln.get("sens","")
                if tid.startswith("LEGIARTI") and typ == "CITATION" and sens == "cible" and tid != aid:
                    targets.append(tid)
        arts[aid] = {"ctx": f"{hierarchy} > Article {num}\n{body}"[:6000], "targets": list(dict.fromkeys(targets))}
        order.append(aid)
print(f"  read {n} members, {len(arts)} usable articles  ({time.time()-t0:.0f}s)")

ids = set(arts)
# queries = articles whose CITATION targets are present in the collected corpus
queries = []
for aid in order:
    golds = [t for t in arts[aid]["targets"] if t in ids and t != aid]
    if golds:
        queries.append((aid, golds))
    if len(queries) >= MAXQ: break
print(f"built {len(queries)} cross-reference queries (gold = in-corpus CITATION targets) over {len(arts)} articles\n")
if not queries:
    print("no in-corpus citation pairs in this scan window; raise SCAN"); sys.exit(0)

# ---- embed corpus via OpenRouter bge-m3 (queries reuse their corpus vector) ----
KEY = os.environ.get("OPENROUTER_API_KEY","")
if not KEY: print("OPENROUTER_API_KEY missing"); sys.exit(1)
H = {"Authorization": f"Bearer {KEY}", "Content-Type":"application/json", "X-Title":"jurisearch"}
def embed_batch(texts):
    req = urllib.request.Request("https://openrouter.ai/api/v1/embeddings",
        data=json.dumps({"model":"baai/bge-m3","input":texts}).encode(), headers=H)
    for a in range(4):
        try:
            with urllib.request.urlopen(req, timeout=120) as r:
                d = json.load(r)
            return [it["embedding"] for it in sorted(d["data"], key=lambda x:x["index"])]
        except Exception:
            if a == 3: raise
            time.sleep(1.5*(a+1))
corpus_ids = list(arts)
texts = [arts[i]["ctx"] for i in corpus_ids]
vecs = [None]*len(texts); q = queue.Queue()
for i in range(0,len(texts),32): q.put(i)
def w():
    while True:
        try: i = q.get_nowait()
        except queue.Empty: return
        try:
            for j,v in enumerate(embed_batch(texts[i:i+32])): vecs[i+j]=v
        except Exception: pass
        finally: q.task_done()
print("embedding corpus via OpenRouter bge-m3 ...")
te=time.time(); ts=[threading.Thread(target=w) for _ in range(12)]
[t.start() for t in ts]; [t.join() for t in ts]
print(f"  embedded {sum(1 for v in vecs if v)}/{len(texts)} in {time.time()-te:.0f}s\n")

def norm(v):
    n=math.sqrt(sum(x*x for x in v)) or 1.0; return [x/n for x in v]
vec = {corpus_ids[i]: norm(vecs[i]) for i in range(len(corpus_ids)) if vecs[i]}
def cos(a,b): return sum(x*y for x,y in zip(a,b))

# ---- retrieve + score (exclude self) ----
recalls=[]; rrs=[]; hit=0; usable=0
for aid, golds in queries:
    if aid not in vec: continue
    golds = [g for g in golds if g in vec]
    if not golds: continue
    usable+=1
    qv = vec[aid]
    scored = sorted(((cos(qv, vec[o]), o) for o in vec if o != aid), reverse=True)
    ranking = [o for _,o in scored]
    topk = set(ranking[:K]); gs=set(golds)
    recalls.append(len(topk & gs)/len(gs))
    rr = next((1/i for i,o in enumerate(ranking,1) if o in gs), 0.0)
    rrs.append(rr)
    if rr>0 and 1/rr<=K: hit+=1

n=usable
print("="*70)
print(f"FRANCE-LEGI cross-reference prototype  —  {n} queries / {len(vec)} corpus articles")
print("="*70)
print(f"  Recall@{K} (cited targets in top-{K}) : {sum(recalls)/n:.3f}")
print(f"  MRR@{K}                              : {sum(rrs)/n:.3f}")
print(f"  hit@{K} (>=1 cited target found)      : {hit}/{n} = {hit/n:.2f}")
print()
print("INTERPRETATION")
print("  - Gold relevance = official LIEN CITATION targets. Built with NO human annotator,")
print("    NO LLM — straight from the DILA XML the legislator authored.")
print("  - This measures citation/'related-article' retrieval (the cite/related pillars).")
print("  - Caveat: cross-refs are navigational, not always topical, so this is a citation-")
print("    recall signal, not open-ended conceptual search quality. Each category measures a")
print("    distinct capability; report them separately.")
print("  - Scales directly: the live index already holds ~1.95M of these LIEN edges; the same")
print("    qrels over pgvector ANN would give a full France-LEGI cross-reference gate.")
