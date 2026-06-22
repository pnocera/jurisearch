# Claude Review Prompt: OpenRouter Dense Projection Evidence

Repo: `/home/pierre/Work/jurisearch`

Review scope:
- Completed OpenRouter-based dense embedding projection evidence for the Phase 1 freemium LEGI index.
- Do not edit files. Review only.

Relevant implementation commits already pushed:
- `f76d0ec Add OpenRouter embedding pool support`
- `e3cbaa8 Harden OpenRouter embedding projection`

Relevant review gates already completed:
- `/home/pierre/Work/jurisearch/work/03-implementation/01-reviews/2026-06-22-openrouter-embedding-pool-claude-review.md` -> `VERDICT: GO`
- `/home/pierre/Work/jurisearch/work/03-implementation/01-reviews/2026-06-22-openrouter-embedding-retry-truncation-claude-review.md` -> `VERDICT: GO`

Artifact to review:
- `/home/pierre/Work/jurisearch/work/03-implementation/02-evidence/2026-06-22-openrouter-dense-projection-run.log`

Final run command:

```bash
JURISEARCH_CONFIG=none \
JURISEARCH_EMBED_BASE_URL=http://127.0.0.1:8097/v1 \
JURISEARCH_EMBED_POOL='https://openrouter.ai/api/v1|baai/bge-m3|OPENROUTER_API_KEY' \
/home/pierre/Work/jurisearch/target/debug/jurisearch \
  --index-dir /home/pierre/Work/jurisearch/index/phase1-freemium-20250713 \
  ingest embed-chunks \
  --batch-size 32 \
  --pool-concurrency 16 \
  --index-lists 32
```

Observed completion:

```json
{
  "chunks_considered": 1778025,
  "command": "ingest embed-chunks",
  "dense_rebuild": {
    "chunks": 1852745,
    "embedding_fingerprint": "bge-m3:1024:normalize:true",
    "embeddings": 1852745,
    "index_lists": 32,
    "index_name": "chunk_embeddings_embedding_ivfflat_idx"
  },
  "embedding": {
    "base_urls": [
      "http://127.0.0.1:8097/v1"
    ],
    "dimension": 1024,
    "estimated_chars_per_token": 4,
    "fingerprint": "bge-m3:1024:normalize:true",
    "max_estimated_tokens": 8192,
    "max_input_chars": 20000,
    "model": "bge-m3",
    "normalize": true,
    "pool": [
      {
        "api_key_configured": true,
        "api_key_env": "OPENROUTER_API_KEY",
        "base_url": "https://openrouter.ai/api/v1",
        "request_model": "baai/bge-m3"
      }
    ],
    "pool_overrides_base_urls": true,
    "pooling": "cls",
    "provisional": true,
    "reembeddable": true,
    "token_count_method": "estimated_chars",
    "tokenizer_path": null
  },
  "embedding_inputs_truncated": 32,
  "embeddings_inserted": 1778025,
  "endpoint_pool": {
    "batch_size": 32,
    "endpoints": [
      {
        "base_url": "https://openrouter.ai/api/v1",
        "chunks": 1778025,
        "failures": 0,
        "request_model": "baai/bge-m3",
        "requests": 55564,
        "truncated_inputs": 32
      }
    ],
    "pool_concurrency": 16,
    "strategy": "least_outstanding_requests"
  },
  "index_dir": "/home/pierre/Work/jurisearch/index/phase1-freemium-20250713",
  "limit": null,
  "schema_version": "1"
}
```

Known context:
- This run resumed prior partial work. `embeddings_inserted=1778025`, while the final dense rebuild reports full coverage: `1852745` chunks and `1852745` embeddings for fingerprint `bge-m3:1024:normalize:true`.
- Endpoint pool overrode `JURISEARCH_EMBED_BASE_URL`, so all new embedding requests went to OpenRouter, while canonical stored model/fingerprint stayed `bge-m3`.
- `embedding_inputs_truncated=32` means 32 outbound embedding request texts were capped to fit the configured 20k-char / estimated 8192-token budget. Stored chunk text was not truncated.
- PostgreSQL shut down after command completion, so a post-exit count query by the monitor saw connection refused. Before exit, the monitor observed `1852745/1852745`.
- The `.pid` file is a runtime artifact and may be removed or ignored after review.

Please review whether:
1. The evidence is sufficient to consider dense projection complete for this index.
2. The run output exposes any correctness risks before moving to the next implementation-plan step.
3. Any evidence artifact should be committed, amended, or cleaned up before proceeding.
4. Any follow-up validation should happen before the next step.

Output format:
1. Findings first, ordered by severity, with file/path references where applicable.
2. Open questions or residual risks.
3. Verification notes.
4. Final line exactly one of:
   - `VERDICT: GO`
   - `VERDICT: FIXES_REQUIRED`
