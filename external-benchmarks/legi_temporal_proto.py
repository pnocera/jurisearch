#!/usr/bin/env python3
"""Prototype: a France-LEGI *temporal known-item* retrieval benchmark built entirely
from OFFICIAL LEGI evidence (no human annotator), run through bge-m3 dense retrieval.

Official evidence used as ground truth:
  - CID        : stable cross-version article id  -> groups temporal versions
  - DATE_DEBUT / DATE_FIN : version validity window -> determines which version is gold at a date
  - NUM / CONTEXTE titles  : article number + code/hierarchy

Source: the official DILA LEGI archive (Licence Ouverte). bge-m3 via OpenRouter.

This demonstrates the construction + end-to-end run. Interpretation is printed at the end.
"""
import os, re, sys, time, json, math, queue, threading, tarfile, datetime as dt
import urllib.request, urllib.error
import xml.etree.ElementTree as ET
from collections import defaultdict

ARCHIVE = "index/phase1-eval-archives/Freemium_legi_global_20250713-140000.tar.gz"
TARGET_FAMILIES = 40          # multi-version article families to use as queries
MAX_CORPUS = 2500             # retrieval pool size
SCAN_CAP = 60000              # article members to scan at most
TIME_CAP = 150               # seconds for the scan

def ftext(root, tag):
    e = root.find(f".//{tag}")
    return (e.text or "").strip() if e is not None and e.text else ""

def parse_article(xml_bytes):
    try:
        root = ET.fromstring(xml_bytes)
    except Exception:
        return None
    aid = ftext(root, "ID"); cid = ftext(root, "CID"); num = ftext(root, "NUM")
    dd = ftext(root, "DATE_DEBUT"); df = ftext(root, "DATE_FIN"); etat = ftext(root, "ETAT")
    if not (aid.startswith("LEGIARTI") and cid and num):
        return None
    # code / hierarchy from CONTEXTE titles
    titles = [ (e.text or "").strip() for e in root.iter() if e.tag in ("TITRE_TXT","TITRE_TM") and e.text ]
    code = titles[0] if titles else ""
    hierarchy = " > ".join(t for t in titles[:4] if t)
    bt = root.find(".//BLOC_TEXTUEL")
    body = re.sub(r"\s+", " ", " ".join(bt.itertext())).strip() if bt is not None else ""
    return {"id": aid, "cid": cid, "num": num, "dd": dd, "df": df, "etat": etat,
            "code": code, "hierarchy": hierarchy, "body": body}

def to_date(s):
    try: return dt.date.fromisoformat(s)
    except Exception: return None

print(f"scanning official archive: {ARCHIVE}")
fams = defaultdict(list)      # cid -> [versions]
scanned = 0; t0 = time.time()
with tarfile.open(ARCHIVE, "r:gz") as tf:
    for m in tf:
        if not (m.isfile() and "/article/" in m.name and m.name.endswith(".xml")):
            continue
        scanned += 1
        if scanned > SCAN_CAP or time.time()-t0 > TIME_CAP:
            break
        f = tf.extractfile(m)
        if f is None: continue
        rec = parse_article(f.read())
        if rec is None or not rec["body"]:
            continue
        fams[rec["cid"]].append(rec)
        if scanned % 5000 == 0:
            multi = sum(1 for v in fams.values() if len(v) >= 2)
            print(f"  scanned {scanned} articles, {len(fams)} families, {multi} multi-version  ({time.time()-t0:.0f}s)")
        multi = [c for c,v in fams.items() if len(v) >= 2]
        if len(multi) >= TARGET_FAMILIES and scanned >= 1500:
            # keep scanning a little to fill distractors, but cap
            if scanned >= 6000:
                break

multi_families = {c: sorted(v, key=lambda r: r["dd"] or "") for c,v in fams.items() if len(v) >= 2}
print(f"\ncollected {scanned} article versions, {len(multi_families)} multi-version families")

# Build corpus (all collected versions, capped) + queries from multi-version families
corpus = []
seen = set()
for c, versions in fams.items():
    for r in versions:
        if r["id"] not in seen:
            seen.add(r["id"]); corpus.append(r)
corpus = corpus[:MAX_CORPUS]
corpus_ids = {r["id"]: i for i,r in enumerate(corpus)}
def ctx(r): return f"{r['hierarchy']} > Article {r['num']}\n{r['body']}"[:6000]

# queries: for each multi-version family, pick a version with a finite window, query at a date inside it
queries = []
for c, versions in multi_families.items():
    fam_in_corpus = [v for v in versions if v["id"] in corpus_ids]
    if len(fam_in_corpus) < 2:   # need siblings present to test discrimination
        continue
    # pick the version with both dd and df (a historical, superseded version) for a clean as-of
    cand = [v for v in fam_in_corpus if to_date(v["dd"]) and to_date(v["df"])]
    target = cand[0] if cand else fam_in_corpus[0]
    d0 = to_date(target["dd"])
    if not d0: continue
    asof = (d0 + dt.timedelta(days=30)).isoformat()
    # official gold = the version whose [dd, df) contains asof  (recompute, don't trust pick)
    a = to_date(asof)
    gold = None
    for v in fam_in_corpus:
        vd, ve = to_date(v["dd"]), to_date(v["df"])
        if vd and vd <= a and (ve is None or a < ve):
            gold = v; break
    if gold is None: continue
    queries.append({
        "cid": c, "num": target["num"], "code": target["code"],
        "query": f"Article {target['num']} {target['code']} en vigueur au {asof}",
        "asof": asof, "gold_id": gold["id"],
        "family_ids": [v["id"] for v in fam_in_corpus],
    })
    if len(queries) >= TARGET_FAMILIES:
        break

print(f"built {len(queries)} temporal as-of queries over a {len(corpus)}-article corpus\n")
if not queries:
    print("no usable multi-version families found in the scan window; increase SCAN_CAP"); sys.exit(0)

# ---- embed via OpenRouter bge-m3 ----
KEY = os.environ.get("OPENROUTER_API_KEY","")
if not KEY: print("OPENROUTER_API_KEY missing"); sys.exit(1)
H = {"Authorization": f"Bearer {KEY}", "Content-Type": "application/json", "X-Title": "jurisearch"}
def embed_batch(texts):
    req = urllib.request.Request("https://openrouter.ai/api/v1/embeddings",
        data=json.dumps({"model":"baai/bge-m3","input":texts}).encode(), headers=H)
    for attempt in range(4):
        try:
            with urllib.request.urlopen(req, timeout=120) as r:
                d = json.load(r)
            return [it["embedding"] for it in sorted(d["data"], key=lambda x:x["index"])]
        except Exception as e:
            if attempt == 3: raise
            time.sleep(1.5*(attempt+1))

def embed_all(texts, B=32, C=12):
    out = [None]*len(texts)
    idx = list(range(0,len(texts),B)); q = queue.Queue()
    for i in idx: q.put(i)
    lock = threading.Lock(); errs=[0]
    def w():
        while True:
            try: i = q.get_nowait()
            except queue.Empty: return
            try:
                vs = embed_batch(texts[i:i+B])
                for j,v in enumerate(vs): out[i+j]=v
            except Exception:
                with lock: errs[0]+=1
            finally: q.task_done()
    ts=[threading.Thread(target=w) for _ in range(C)]
    [t.start() for t in ts]; [t.join() for t in ts]
    return out, errs[0]

print("embedding corpus + queries via OpenRouter bge-m3 ...")
te = time.time()
cvecs, e1 = embed_all([ctx(r) for r in corpus])
qvecs, e2 = embed_all([q["query"] for q in queries])
print(f"  embedded {len(corpus)} corpus + {len(queries)} queries in {time.time()-te:.0f}s (batch errors: {e1+e2})\n")

def norm(v):
    n=math.sqrt(sum(x*x for x in v)) or 1.0; return [x/n for x in v]
cN = [norm(v) for v in cvecs if v]
# keep only successfully-embedded corpus
ok_corpus = [(corpus[i], norm(cvecs[i])) for i in range(len(corpus)) if cvecs[i]]
def cos(a,b): return sum(x*y for x,y in zip(a,b))

# ---- retrieve + score ----
K=10
fam_hit=0; gold_top1=0; gold_in_topk=0; version_isolation=0; ranks=[]
for q,qv in zip(queries, qvecs):
    if not qv: continue
    qn = norm(qv)
    scored = sorted(((cos(qn, cv), r["id"]) for r,cv in ok_corpus), reverse=True)
    ranking = [rid for _,rid in scored]
    fam = set(q["family_ids"]); gold=q["gold_id"]
    # family hit@k: any version of the right article in top-k
    if any(rid in fam for rid in ranking[:K]): fam_hit+=1
    # gold exact version
    gr = next((i for i,rid in enumerate(ranking,1) if rid==gold), None)
    if gr: ranks.append(gr)
    if gr==1: gold_top1+=1
    if gr and gr<=K: gold_in_topk+=1
    # version isolation: among this family's versions, is the date-correct gold ranked highest?
    fam_ranked = [rid for rid in ranking if rid in fam]
    if fam_ranked and fam_ranked[0]==gold: version_isolation+=1

n=len(queries)
print("="*70)
print(f"FRANCE-LEGI temporal as-of prototype  —  {n} queries / {len(ok_corpus)} corpus")
print("="*70)
print(f"  family hit@{K}            : {fam_hit}/{n} = {fam_hit/n:.2f}   (dense finds the right ARTICLE)")
print(f"  gold exact-version @1     : {gold_top1}/{n} = {gold_top1/n:.2f}")
print(f"  gold exact-version in@{K}  : {gold_in_topk}/{n} = {gold_in_topk/n:.2f}")
print(f"  version isolation         : {version_isolation}/{n} = {version_isolation/n:.2f}   (dense alone picks the DATE-CORRECT version over its siblings)")
print()
print("INTERPRETATION")
print("  - Official evidence (CID + DATE_DEBUT/DATE_FIN) produced the gold with NO human annotator.")
print("  - High family-hit but low version-isolation = dense embeddings find the article but")
print("    cannot reliably pick the temporally-correct version (siblings are near-identical text).")
print("  - That is exactly why jurisearch's --as-of date PREFILTER exists: temporal correctness")
print("    is an exact-filter property, not an embedding property. This benchmark measures it.")
