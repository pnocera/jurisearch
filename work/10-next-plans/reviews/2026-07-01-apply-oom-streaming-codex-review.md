## Findings

No BLOCKER/WARN/NIT findings.

## Review Notes

The four OOM sites are now bounded in the current source:

- `verify_per_file_digests` streams each payload through `BufReader<File>` into `tee_digest(..., sink())`; it no longer materializes the file bytes. `tee_digest` and `digest_bytes` share `format_sha256`, so the digest string format and value are identical for the same byte sequence. The verified map, duplicate-file rejection, exact `integrity.per_file_digests` comparison, and aggregate recompute remain unchanged.
- `copy_payload_in` opens the local file before `db.copy_in(...)`, preserving the intended local-open error ordering, then uses `io::copy` into the COPY writer. That keeps COPY input byte-for-byte identical to the old `write_all(&bytes)` path without holding the payload in memory.
- Upsert/Delete JSONL files now flow through `stream_jsonl_batched`, which keeps only one `Vec<Value>` batch at a time and clears it after the callback. There is no retention of prior batches or all rows.
- ReplaceSet JSONL is streamed line by line and applies one replace-set record at a time.

The batching equivalence claim holds against `crates/jurisearch-storage/src/incremental.rs`: both `apply_upserts` and `apply_deletes` build one SQL statement, then iterate the supplied slice and execute once per JSON row/key. There is no all-rows-at-once statement, no cross-row dedup pass, and no whole-file semantic in those helpers. Splitting the same ordered row stream into 2,000-row batches inside the same incremental transaction preserves the cumulative effect and the `scopes` count.

Line handling remains equivalent for the reviewed paths: both old `str::lines()` and new `BufRead::lines()` split on newline, handle CRLF consistently, do not yield an extra trailing empty line, and the existing `trim().is_empty()` skip is preserved. One subtle difference is that streaming may apply earlier valid batches before a later read/UTF-8/JSON error, whereas the old `read_to_string` rejected invalid UTF-8 before parsing any row. That remains transaction-local in `apply_incremental`, and the transaction is not committed on error, so there is no partial-apply visibility.

Error ordering is correct in the current source. `apply_incremental` calls `verify_per_file_digests(artifact_dir, manifest)?` before opening the writer transaction and before `apply_incremental_files`, so digest mismatches, short reads, or corrupt payload bytes are rejected before any incremental row application. The baseline COPY path also opens the payload file before entering server COPY mode.

The added DB-free tests cover the intended non-DB invariants: the digest test crosses the `tee_digest` chunk boundary and compares directly against `digest_bytes`; the batching test proves full row coverage, ordering, blank-line skipping, and batch-size partitioning using a small explicit batch size. Remaining coverage limitation: the DB-backed apply/loopback behavior was not executed here because the local PG/pgvector/pg_search stack is unavailable, so end-to-end COPY and transactional incremental rollback remain covered only by source review plus the reported local validation.

VERDICT: GO
