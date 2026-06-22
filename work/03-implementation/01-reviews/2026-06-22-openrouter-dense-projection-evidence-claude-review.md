# Review: OpenRouter Dense Projection Evidence

Scope: review-only assessment of the completed dense embedding projection run for the
Phase 1 freemium LEGI index. No source files edited.

Artifact reviewed: `work/03-implementation/02-evidence/2026-06-22-openrouter-dense-projection-run.log`
Build under test: `commit=e3cbaa8` (matches current HEAD).

## 1. Findings (ordered by severity)

No correctness blockers. The run is internally consistent and the dense index is fully
covered. All findings below are observations / non-blocking.

### [Info] Coverage is self-certified by the finalize transaction — evidence is sufficient
`finalize_dense_rebuild` (`crates/jurisearch-storage/src/dense.rs:93-201`) runs a single
transaction that (a) counts `chunks`, (b) counts `embeddings` for the exact
`(embedding_fingerprint, model, dimension)` triple, and (c) computes `missing` via a
`LEFT JOIN` of chunks lacking a matching embedding (`dense.rs:122-134`). If `missing != 0`
it returns `StorageError::DenseRebuild` and **does not** create the IVFFlat index or write
the manifest (`dense.rs:135-192`). Because the run returned a `dense_rebuild` block, the
`missing = 0` gate necessarily passed *inside the same transaction* that built
`chunk_embeddings_embedding_ivfflat_idx` and committed the manifest. The reported
`chunks = 1852745` and `embeddings = 1852745` (equal) are consistent with that gate. This
is stronger evidence than the external monitor count, so the post-exit "connection refused"
is irrelevant to the coverage claim.

### [Info] The `1778025` vs `1852745` discrepancy is fully explained by resume + the load filter
`embeddings_inserted = 1778025` is *this run's* upserts; `dense_rebuild.embeddings = 1852745`
is the total in the index. The 74,720-row gap equals chunks embedded by prior partial runs.
`load_chunk_embedding_inputs` (`dense.rs:34-91`) selects only chunks where the embedding is
`NULL` or its `fingerprint/model/dimension` differs, so resume correctly re-embeds only the
remaining work. `chunks_considered == embeddings_inserted == 1778025` confirms every
considered chunk was embedded with no silent drops.

### [Info] Request accounting reconciles exactly — no hidden retries or dropped batches
`ceil(1778025 / 32) = 55564`, which equals the reported `requests = 55564` for the single
OpenRouter endpoint, with `failures = 0`. (55563 full batches of 32 + one batch of 9 =
1,778,025.) Since failures were zero, no retry inflated the request count; the figure is the
clean lower bound, corroborating that every chunk was sent exactly once.

### [Low] 32 chunks are embedded from truncated text (vector ≠ full body for those chunks)
`embedding_inputs_truncated = 32` means 32 outbound texts were capped to the
`max_input_chars = 20000` budget before embedding; stored chunk bodies were not truncated
(confirmed by the truncation review gate). For those 32 chunks (0.0017% of the corpus) the
dense vector represents only the leading ~20k chars, so semantic recall over the tail of
those specific chunks is mildly degraded. This is the accepted, documented behavior of the
hardening change, not a regression. Worth recording as a known limitation; not a blocker.

### [Low] Token budget is estimated, not exact — but empirically validated by this run
`token_count_method = estimated_chars`, `tokenizer_path = null`. The F1 residual risk from
`2026-06-22-openrouter-embedding-retry-truncation-claude-review.md` ("does a 20k-char
truncated chunk actually clear OpenRouter bge-m3's 8192-token limit?") is now answered by
data: the configured budget is internally consistent (20000 ÷ 4 = 5000 estimated tokens, a
~39% margin under 8192), and all 32 truncated inputs — plus the other 1.78M — returned with
`failures = 0`. No `InputTooLong` / error-shaped-200 aborts occurred. F1 is resolved for this
corpus and configuration.

### [Info] Canonical identity preserved across the provider override
`pool_overrides_base_urls = true`: all new requests went to `https://openrouter.ai/api/v1`
with wire alias `request_model = baai/bge-m3`, while stored `model = bge-m3` and
`fingerprint = bge-m3:1024:normalize:true`. This matches the confirmed-correct behavior of
both prior GO gates (`...-openrouter-embedding-pool...` and `...-retry-truncation...`):
the alias is wire-only and does not enter the storage fingerprint. The `dense_rebuild`
fingerprint matches the embedding fingerprint exactly, so the IVFFlat index and manifest
describe the same canonical vector space.

## 2. Open questions / residual risks

- **Homogeneity of the 74,720 resumed embeddings.** The log does not record whether the
  pre-existing 74,720 rows were produced by OpenRouter or by the local `127.0.0.1:8097`
  bge-m3 endpoint. The pool-override design treats bge-m3 outputs as interchangeable across
  hosts (same model/dim/normalize → same `bge-m3:1024:normalize:true` fingerprint), and the
  prior live probe (norm ≈ 1.0, dim 1024, `BAAI/bge-m3`) supports that. If the two hosts ever
  produced subtly different vectors (pooling/quantization drift) for identical text, IVFFlat
  neighbor quality could differ at the margins for that 4% slice. Low risk and inherent to
  the accepted design; flagged only so a future canonical re-embed (the `reembeddable = true`
  / `provisional = true` path) is the clean resolution if drift is ever observed.
- **`provisional = true` is recorded in the manifest.** The index is queryable now, but the
  manifest marks these embeddings provisional and re-embeddable. This is expected for the
  phase0/phase1 bge-m3 config (`phase0_bge_m3` sets `provisional: true`) and signals that a
  pinned canonical re-embed is anticipated later. Not a defect; ensure downstream
  query/eval steps don't treat `provisional` as a blocking gate.
- **`embedding_inputs_truncated` is per-run, not cumulative.** As noted in the prior gate,
  this counts only this run's truncations. The corpus-wide truncated total across all resume
  passes is not surfaced by a single log; acceptable, but operators should not read `32` as
  the lifetime figure.

## 3. Verification notes

- Re-read `finalize_dense_rebuild` and `load_chunk_embedding_inputs` end-to-end
  (`dense.rs:34-201`) and the `embed_chunks_payload` JSON assembly (`main.rs:2076-2194`) to
  confirm field provenance: `chunks_considered`/`embeddings_inserted`/
  `embedding_inputs_truncated` come from the run accumulator, `dense_rebuild.*` from the
  finalize report, and the coverage gate is transactional.
- Arithmetic reconciled: resume gap = 1852745 − 1778025 = 74720; `ceil(1778025/32) = 55564`
  == reported requests; `chunks_considered == embeddings_inserted`;
  `dense_rebuild.chunks == dense_rebuild.embeddings`; 20000/4 = 5000 < 8192.
- Build provenance: log header `commit=e3cbaa8` equals `git rev-parse HEAD`, so the binary
  matches the reviewed/approved source.
- Relied on the two prior GO gates for the code-level correctness of the pool, key isolation,
  alias-on-the-wire, and truncation safety; this review checks only that the *run output* is
  consistent with that code and sufficient as completion evidence. Did not re-run the
  ingest or re-query Postgres (instance shut down post-run).

### Artifact hygiene (answers prompt item 3)

- `2026-06-22-openrouter-dense-projection-run.log` — **commit it.** It is the completion
  evidence. The evidence dir already tracks `.md`/`.txt` artifacts; this is the first `.log`
  but belongs with them. Nothing is `.gitignore`d, so it will not be excluded automatically.
- `2026-06-22-openrouter-dense-projection-run.pid` (`2166455`) — **do not commit; remove.**
  Runtime artifact only; no value after the run. Not currently ignored, so it would be swept
  in by `git add -A` — delete it (or add a `*.pid` ignore) before staging.
- `2026-06-22-openrouter-dense-projection-evidence-claude-review.md.tmp` (0 bytes) —
  transient placeholder for this review; **remove** once this `.md` is finalized.
- `2026-06-22-openrouter-dense-projection-evidence-claude-prompt.md` — the review prompt;
  keep or commit alongside per existing convention; harmless either way.

## 4. Assessment against the prompt's four questions

1. **Sufficient to consider dense projection complete?** Yes. The transactional `missing = 0`
   gate plus equal chunk/embedding totals (1,852,745) self-certify full coverage for
   `bge-m3:1024:normalize:true`.
2. **Correctness risks before the next step?** None blocking. The only substantive items are
   the 32 truncated-text vectors (0.0017%, accepted) and the cross-host homogeneity question
   (low, inherent to the design).
3. **Artifact actions?** Commit the `.log`; remove the `.pid` and the empty `.md.tmp`; keep
   the prompt/review `.md`.
4. **Follow-up validation?** Optional, not gating: a one-shot dense `search` against the new
   `chunk_embeddings_embedding_ivfflat_idx` to confirm the IVFFlat index returns sane
   neighbors at `lists = 32`, and (if drift is a concern) spot-check that the 74,720 resumed
   rows share the same provider lineage as this run.

VERDICT: GO
