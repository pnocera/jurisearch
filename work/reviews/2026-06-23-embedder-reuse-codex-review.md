# Review: embedder reuse in France-LEGI eval

## Findings

- P3 `crates/jurisearch-cli/src/main.rs:1131` - The existing `#[allow(clippy::too_many_arguments)]` attribute was left above the newly inserted `PreparedQueryEmbedder` item, so it no longer suppresses `search_with_postgres`'s 10-argument signature. The preceding `/// Run one search...` rustdoc also now attaches to `PreparedQueryEmbedder`, making generated docs misleading. This is not a runtime regression, but a clippy gate using `-D warnings` would likely fail on `search_with_postgres`.

## Behavioral Review

- One-shot behavior is preserved. `search_payload`, including the session path that calls it, now passes `None`; the dense branch then runs `PreparedQueryEmbedder::from_env()?.embed(args.query.as_str())?`, which performs the same `embedding_config_from_env()`, `ensure_embedding_runtime_ready(&embedding_config, false)`, `fingerprint()`, `storage_embedding_fingerprint()`, `OpenAiCompatibleClient::new(...)`, `embed_query(...)`, and `pgvector_literal(&embedding.values)` sequence as the previous inline dense path.
- Lexical/non-dense behavior is preserved in `search_with_postgres`: the `embedder` option is only matched inside `if retrieval_mode.uses_dense()`, so `RetrievalMode::Bm25` still builds no embedder and produces `(None, None)` for dense query inputs.
- The France-LEGI runner is intentionally hybrid/dense. It builds one `PreparedQueryEmbedder` after gold extraction and passes it through all three `france_legi_search_documents` loops. Each individual query still calls `embed_query`, so endpoint/runtime failures during the sweep are still surfaced at the query that fails.
- Fingerprint reuse is sound for the intended batch run. The cached `expected_fingerprint` and `storage_fingerprint` come from a single `embedding_config_from_env()` call, and there is no code in the eval loop that mutates embedding environment/config state.
- Query-readiness semantics are unchanged for the batch path relative to the existing optimization: `eval_france_legi_payload` still calls `ensure_query_readiness(&postgres, QueryReadinessGate::Search)` once before the loops, and per-query calls still pass `verify_readiness = false`.

VERDICT: GO
