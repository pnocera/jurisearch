No BLOCKER/WARN/NIT findings.

Reviewed the working-tree diff for:

- `crates/jurisearch-storage/src/projection/legi.rs`
- `crates/jurisearch-storage/src/dense.rs`
- `crates/jurisearch-storage/tests/chunk_fingerprint_preserve.rs`

Verification notes:

- `projection/legi.rs:98` uses the correct `ON CONFLICT DO UPDATE` value sources: `chunks.*` refers to the pre-update target row, while `EXCLUDED.*` refers to the proposed insert row. PostgreSQL evaluates the `SET` RHS expressions against the same pre-update row values, so the earlier `body = EXCLUDED.body` and `contextualized_body = EXCLUDED.contextualized_body` assignments do not change what the fingerprint `CASE` compares.
- The `CASE` branch order is correct at `projection/legi.rs:98`: text change invalidates to `NULL`; unchanged text plus non-null incoming fingerprint stamps that fingerprint; unchanged text plus null incoming fingerprint preserves the existing parent stamp. The other chunk conflict assignments are unchanged, and the continued SQL string is syntactically well-formed.
- `dense.rs:92` and `dense.rs:108` add `c.embedding_fingerprint IS NULL` to both pending-input selector branches, limited and unlimited. The clause is OR'd with the existing child-embedding drift checks, so a null parent stamp is selected even when `chunk_embeddings` still has an otherwise matching row.
- The finalize coverage query remains child-coverage-only at `dense.rs:183`; it was not changed to include parent `chunks.embedding_fingerprint IS NULL`, which preserves the intended finalize behavior.
- End-to-end behavior is sound: unchanged reproject with `None` preserves the active parent stamp and avoids re-embedding; changed text nulls the parent stamp, causes re-selection, reads the new body/context text, and the embedding writer restamps `chunks.embedding_fingerprint` before upserting `chunk_embeddings`; fresh inserts still start with the supplied/null parent fingerprint and are selected through the existing missing-child path.
- I found only one production `INSERT INTO chunks ... ON CONFLICT (chunk_id)` statement, the shared statement in `projection/legi.rs`. `projection/decisions.rs` aliases and reuses that same prepared statement, so the fix applies to both LEGI and jurisprudence ingestion without caller signature changes.
- `zone_units` is intentionally separate and unchanged. Its derivation/re-embedding model is distinct from the shared chunk projection path under review.
- The new PG-gated tests cover the critical regressions: unchanged reproject preserves and is not selected (`chunk_fingerprint_preserve.rs:96`), changed-body reproject invalidates and is selected despite a still-matching child row (`chunk_fingerprint_preserve.rs:144`), fresh chunks are selected (`chunk_fingerprint_preserve.rs:209`), and the direct-stamp branch is pinned (`chunk_fingerprint_preserve.rs:238`). The first two tests would fail against the old code for the original stuck-state bug.
- I did not rerun the validation commands; the brief records local green runs for build, storage tests, workspace no-run, clippy, and fmt.

VERDICT: GO
