#!/usr/bin/env python3
"""Bring the live index PG up via `jurisearch session` (forced open with a status command),
inspect the data shape for temporal + known-item qrels, run a search smoke, shut down cleanly."""
import subprocess, time, os, json, sys, threading, queue
IDX = "/home/pierre/Work/jurisearch/index/phase1-freemium-20250713"; DATA = f"{IDX}/pg/data"
os.makedirs(f"{IDX}/pg/sock", exist_ok=True)   # jurisearch derives the socket dir from --index-dir; ensure it exists
PB = os.path.expanduser("~/.pgrx/18.4/pgrx-install/bin/psql")
env = dict(os.environ)
env.setdefault("JURISEARCH_EMBED_BASE_URL", "http://127.0.0.1:8097/v1")
env.setdefault("JURISEARCH_INDEX_DIR", IDX)
sess = subprocess.Popen(["target/debug/jurisearch","--index-dir",IDX,"session","--jsonl"],
    stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True, env=env, bufsize=1)
outq, errq = queue.Queue(), queue.Queue()
threading.Thread(target=lambda:[outq.put(l) for l in sess.stdout], daemon=True).start()
threading.Thread(target=lambda:[errq.put(l) for l in sess.stderr], daemon=True).start()
def send(o): sess.stdin.write(json.dumps(o)+"\n"); sess.stdin.flush()
def recv_id(want, timeout):
    end=time.time()+timeout
    while time.time()<end:
        try: line=outq.get(timeout=end-time.time())
        except queue.Empty: return None
        try:
            j=json.loads(line)
            if j.get("id")==want: return j
        except Exception: pass
    return None
def psql(sql):
    r=subprocess.run([PB,"-h",sock,"-p",port,"-U","postgres","-d","jurisearch","-At","-F"," | ","-c",sql],
                     capture_output=True,text=True)
    return (r.stdout.strip() or r.stderr.strip())
def find_pg():
    out=subprocess.run(["ps","-eo","cmd"],capture_output=True,text=True).stdout
    for line in out.splitlines():
        if "postgres -D" in line and "phase1-freemium" in line:
            dd=line.split("postgres -D",1)[1].strip().split()[0]
            pf=os.path.join(dd,"postmaster.pid")
            if os.path.exists(pf):
                ls=open(pf).read().splitlines()
                if len(ls)>=5 and ls[3].strip().isdigit(): return ls[3].strip(), ls[4].strip()
    return None, None
try:
    print("forcing PG up with a search (first search includes PG startup + ANN load) ...")
    send({"id":"w1","command":"search","args":{"query":"responsabilite du fait des produits defectueux","top_k":3}})
    resp=recv_id("w1",200)
    if resp is None:
        print("no search response in 200s; session stderr:")
        while not errq.empty(): print("  ", errq.get_nowait().rstrip())
        sys.exit(1)
    print("search ok=", resp.get("ok"), " | sample:", json.dumps(resp)[:500])
    port=sock=None
    for _ in range(20):
        if os.path.exists(f"{DATA}/postmaster.pid"):
            ls=open(f"{DATA}/postmaster.pid").read().splitlines()
            if len(ls)>=5 and ls[3].strip().isdigit(): port,sock=ls[3].strip(),ls[4].strip(); break
        time.sleep(1)
    if not port: print("PG up (search worked) but postmaster.pid not at",DATA); sys.exit(1)
    print(f"\nPG up: port={port}\n")
    print("tables:", psql("SELECT relname||':'||n_live_tup FROM pg_stat_user_tables WHERE n_live_tup>0 ORDER BY n_live_tup DESC LIMIT 14;"))
    print("\nembedding/vector tables:", psql("SELECT relname||':'||n_live_tup FROM pg_stat_user_tables WHERE relname ~ 'embed|vector|ann';"))
    print("\nmulti-version families (version_group>1):", psql("SELECT count(*) FROM (SELECT version_group FROM documents GROUP BY version_group HAVING count(*)>1) t;"))
    print("sample family:")
    print(psql("WITH g AS (SELECT version_group FROM documents GROUP BY version_group HAVING count(*)>=2 LIMIT 1) "
               "SELECT document_id, left(citation,55), valid_from, valid_to FROM documents JOIN g USING(version_group) ORDER BY valid_from;"))
    print("\ncanonical_json keys:", psql("SELECT string_agg(DISTINCT k,', ') FROM (SELECT jsonb_object_keys(canonical_json) k FROM documents WHERE canonical_json IS NOT NULL LIMIT 60) s;"))
    print("sample article rows:", psql("SELECT document_id||' :: '||left(citation,70) FROM documents WHERE kind='article' LIMIT 3;"))
    print("\n=== production search smoke ===")
    send({"id":"q1","command":"search","args":{"query":"responsabilite du fait des produits defectueux","top_k":3}})
    r=recv_id("q1",60)
    print(json.dumps(r)[:700] if r else "no search response")
finally:
    try: send({"command":"exit"}); sess.wait(timeout=25)
    except Exception:
        sess.terminate()
        try: sess.wait(timeout=10)
        except Exception: sess.kill()
    print("\nsession closed")
