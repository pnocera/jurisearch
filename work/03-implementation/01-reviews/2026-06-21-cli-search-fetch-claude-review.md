## Review: commit 7e49fe4 — "Wire CLI search and fetch to storage" (Phase 0.6 CLI search/fetch wiring)

Verdict: GO

The core wiring is correct, safe, and well-tested. Build is clean; all 7 non-ignored contract tests pass, and `fetch_returns_documents_from_existing_index` actually executed against a real durable Postgres on this machine (extensions present) — so the start → query → drop lifecycle is verified end-to-end, not just on paper. Findings below are non-blocking suggestions.

### What I verified against the focus areas

- **No silent empty-index creation.** `require_existing_index_dir` gates on `pg/data/PG_VERSION` *before* `open_index` calls `start_durable` (which would `initdb`). Missing/uninitialized → `index_unavailable`. ✓
- **index-dir handling, both paths.** Direct CLI uses the `global`, `env = "JURISEARCH_INDEX_DIR"` clap arg; JSONL session takes `args.index_dir` from JSON with an env fallback inside `require_existing_index_dir`. Consistent. ✓
- **JSON error/exit codes.** Direct path: `emit_error` writes JSON then `process::exit` with the mapped code (bad_input→2, index_unavailable→3, dependency→4, upstream→5). Session path returns `SessionResponse::err` as a line and keeps the session alive — no `process::exit`. Clean separation. ✓
- **Storage lifecycle safety.** `ManagedPostgres::Drop` drops the advisory lock then `stop()`s the server; `StartupLock`/`reclaim_data_dir` handle concurrent/stale starts. No leaked server per command. ✓
- **Embeddings only for search.** `search_payload` builds the OpenAI-compatible client and embeds the *original* query; `fetch_payload` never touches embeddings. ✓
- **ParadeDB sanitization.** `parade_query_text` keeps only `is_alphanumeric` tokens (Unicode-aware — French accents survive) and joins with spaces; `sql_string_literal` escapes quotes. Punctuation-only query → `bad_input`. The full punctuated query still goes to the embedder (correct). ✓
- **Temporal default.** `today_utc()` uses a correct Hinnant `civil_from_days`; search defaults `as_of` to today. ✓

### Suggestions (non-blocking)

1. **`SearchResponse` schema drift (highest priority).** `schema.rs` still declares `SearchResponse = { query, results: array }`, but the live output is `{ query, as_of, limit, candidates: [...] }`. Now that this commit flips `search` to `status: Implemented`, an agent reading `help schema --json` is told to expect `results`, which doesn't exist. The `candidates` shape predates this slice (GO'd at the storage layer), so it's not a regression — but the published contract should be brought in line with the real candidate/fetch shapes soon, ideally before agents consume it.
2. **`no_results` is documented but never emitted.** `agent_help` says exit `2` covers "no-results", and `ErrorCode::NoResults` maps to exit 2 — yet `search` with no hits returns `{candidates: []}` exit 0, and `fetch` of all-missing IDs returns `{documents: []}` exit 0. Consider emitting `NoResults` so the documented contract holds.
3. **`fetch --as-of` / `--part` are echoed, not applied.** They're copied onto the response JSON but don't filter anything (`fetch_documents_json` selects purely by ID). This reads as filtering to an agent. Either wire them into selection or document them as passthrough metadata.
4. **`--kind code` is accepted but not applied.** `kind` is validated (rejects `decision`) but there's no `WHERE d.kind = …` in `hybrid_candidates_json`, so `--kind code` and `--kind all` return identical results. Fine for the LEGI-only subset, but worth a comment so it's not mistaken for a real filter once decisions land.
5. **Fingerprint convention is duplicated with no source of truth.** `storage_embedding_fingerprint` hand-builds `"{model}:{dimension}:normalize:{normalize}"`; the storage tests hardcode the same string as a const. When the real index-build/re-embed path lands, any divergence in this format silently yields zero dense rows (degrades to lexical-only with no error). Recommend centralizing this string in one helper (on `EmbeddingConfig` or in `jurisearch-storage`) shared by insert and query paths.
6. **Minor:** per-command Postgres start/stop is correct but slow (acceptable for 0.6); `top_k.saturating_mul(4).max(top_k)` — the `.max` is redundant since `4·k ≥ k`; in `search_payload` the index-existence check runs before the searchable-token check, so a punctuation-only query against a missing index reports `index_unavailable` rather than `bad_input` (cosmetic ordering).

Acceptable as the next 0.6 slice. The remaining items in the updated `IMPLEMENTATION_PLAN.md` (live-embedding index-build path, ANN/rebuild mechanics, chunk-provenance decision) are correctly carried forward as Remaining.
