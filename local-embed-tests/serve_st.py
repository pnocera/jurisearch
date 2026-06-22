#!/usr/bin/env python3
# /// script
# requires-python = ">=3.9"
# dependencies = ["sentence-transformers"]
# ///
"""Minimal OpenAI-compatible /v1/embeddings server for any sentence-transformers model.

Used to serve French specialists (CamemBERT, Solon, ...) for the bge-m3 comparison,
because there is no reliable GGUF for them and sentence-transformers applies each
model's correct pooling/normalization automatically (the fair-comparison requirement).

Run with uv (the inline deps above are resolved into a venv automatically):
    uv run serve_st.py --model Lajavaness/sentence-camembert-large --port 8098
    uv run serve_st.py --model OrdalieTech/Solon-embeddings-large-0.1 --port 8099
    # (optional: serve bge-m3 here too for a serving-stack-controlled run)
    uv run serve_st.py --model BAAI/bge-m3 --port 8197

Then point endpoints.json at http://127.0.0.1:<port>/v1 and run eval.py.
"""
import argparse, json
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

argp = argparse.ArgumentParser()
argp.add_argument("--model", required=True)
argp.add_argument("--port", type=int, default=8098)
argp.add_argument("--host", default="127.0.0.1")
argp.add_argument("--device", default=None, help="cpu / cuda (default: auto)")
argp.add_argument("--no-normalize", action="store_true", help="disable L2 normalization (default: on)")
ARGS = argp.parse_args()

print(f"loading sentence-transformers model: {ARGS.model} ...")
from sentence_transformers import SentenceTransformer
MODEL = SentenceTransformer(ARGS.model, device=ARGS.device)
DIM = MODEL.get_sentence_embedding_dimension()
print(f"loaded. dim={DIM}  normalize={not ARGS.no_normalize}  pooling=model-default")

class H(BaseHTTPRequestHandler):
    def log_message(self, *a):  # quiet
        pass

    def _send(self, code, obj):
        payload = json.dumps(obj).encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)

    def do_GET(self):
        if self.path.rstrip("/") in ("/health", "/v1/models"):
            self._send(200, {"object": "list", "data": [{"id": ARGS.model, "dim": DIM}]})
        else:
            self._send(404, {"error": "not found"})

    def do_POST(self):
        if self.path.rstrip("/") not in ("/v1/embeddings", "/embeddings"):
            return self._send(404, {"error": "not found"})
        n = int(self.headers.get("Content-Length", 0))
        req = json.loads(self.rfile.read(n) or b"{}")
        inp = req.get("input", [])
        texts = [inp] if isinstance(inp, str) else list(inp)
        vecs = MODEL.encode(texts, normalize_embeddings=not ARGS.no_normalize,
                            convert_to_numpy=True, batch_size=64)
        data = [{"object": "embedding", "index": i, "embedding": v.tolist()}
                for i, v in enumerate(vecs)]
        self._send(200, {"object": "list", "data": data, "model": ARGS.model,
                         "usage": {"prompt_tokens": 0, "total_tokens": 0}})

if __name__ == "__main__":
    print(f"serving OpenAI-compatible /v1/embeddings on http://{ARGS.host}:{ARGS.port}/v1")
    ThreadingHTTPServer((ARGS.host, ARGS.port), H).serve_forever()
