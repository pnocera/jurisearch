# Q&A — 20260701-050656

## Question

# Design validation (pre-implementation) — bound the producer's incremental delta-build memory

Repo `/home/pierre/Work/jurisearch`. **Read the actual source; do not trust this prose.** This is a DESIGN gate
before I implement — I want you to confirm the design is correct/sufficient or push back, against the real code.

## Problem
`jurisearch-producer update --group legislation` (the INCREMENTAL path) OOM-killed a 96 GiB box. It left an 8.3 GB
partial `documents.upsert.jsonl`. We must bound peak memory to a few GB regardless of delta size, WITHOUT changing
the produced payload bytes / per-file digests / signed manifest (the consumer independently verifies the ed25519
signature over `canonical_digest(&manifest)`).

## Proposed design (verify each claim against source)
Target: `crates/jurisearch-package-build/src/incremental.rs`, `build_incremental_inner` (~:129). Reference pattern:
the already-committed baseline fix in `crates/jurisearch-package-build/src/baseline.rs:309-350` (`tee_digest` +
`BufWriter<File>`), and `tee_digest`/`format_sha256` in `crates/jurisearch-package/src/canonical.rs`.

1. **LOAD-BEARING claim — is the OOM fully explained by Rust-side accumulation, with NO unbounded DB-side buffer?**
   The design asserts every row read is already **per-scope** (one `document_id`/`scope_key` per `tx.query`):
   `read_scope_rows` predicate `= {key}` (`incremental.rs` ~:564-586, `ORDER BY` PK), graph_edges
   `from_document_id = {doc}` (~:293-303, `ORDER BY edge_id`), `replace_set_rows` per-document
   (`crates/jurisearch-storage/src/incremental.rs` ~:84-103). The three unbounded accumulators are Rust-side:
   `upserts: Vec<Value>` (~:255-267, the 8.3 GB file), `edges: Vec<Value>` (~:295-302),
   `replace_sets: Vec<ReplaceSet>` (~:317-379, chunk bodies + 1024-dim embedding vectors as JSON), each then
   `join("\n")` into one `String` (~:375-379 and the `write_jsonl_op` helper ~:697-704) which DOUBLES peak at write.
   **Verify against source:** is there ANY query in this build path that returns the whole delta (all scopes) in one
   `client.query(...) -> Vec<Row>` (postgres client materializing the full result set)? Specifically check the
   change-scope fetch (`scopes`, `crates/jurisearch-storage/src/outbox.rs` ~:243-291) and the coalescing sets
   (`incremental.rs` ~:193-242). If any such full-delta buffer exists, streaming the FILE writes alone would NOT
   prevent the re-OOM — flag it and say what else must be streamed/cursored. (The design defers `scopes`/BTreeSet as
   "strings-only, O(#scopes), a few GB at worst" — is that acceptable, or is it also a primary OOM term?)

2. **Byte-identity of the streamed output.** Design replaces "collect Vec → `join("\n")` + trailing `\n` → write"
   with a per-row streaming pass: for each row in the SAME order, `serde_json::to_string(row)` then write
   `line` + `"\n"` to a `HashingWriter<BufWriter<File>>`. Claim: current bytes are `l1\n...lN\n`, streamed bytes are
   identical. **Verify:** does `write_jsonl_bytes`/`write_jsonl_op` currently produce exactly `join("\n") + "\n"`
   (i.e. a trailing newline)? Any per-file framing (leading bytes, BOM, sorted-within-file) that a naive stream would
   miss? Are the row `Value`s serialized with `serde_json::to_string` today (so streamed serialization is byte-equal),
   or via some canonical/sorted-key serializer that must be matched?

3. **Ordering is load-bearing for the signature.** `payload.files` and `apply.operations` are ARRAYS (canonical form
   preserves array order — `canonical.rs` ~:112-114). Design preserves the exact push order: per base table
   upsert-then-delete over a sorted `BTreeMap`, then graph_edges, then the four replace-set groups in fixed order
   `[ChunksWithEmbeddings, ChunkEmbeddings, ZoneUnits, DecisionZones]`. **Verify** the current code's push order and
   that the proposed streaming passes reproduce it exactly (including the empty-rows→delete branch ~:260-266 and the
   `document_exists` skip ~:320).

4. **Empty-file semantics.** Today `write_jsonl_op` early-returns on empty (~:697-699) → no file, no `PayloadFile`,
   no `OperationCount`. Design preserves this via LAZY open (open the writer on first row; if never opened, emit
   nothing). **Verify** this matches current behavior and that `row_count`/`OperationCount.count` still equal the
   streamed row count.

5. **HashingWriter digest == digest_bytes.** Design adds `HashingWriter<W>` next to `tee_digest`, reusing
   `format_sha256`, so a streamed digest is byte-identical to `digest_bytes` over the same bytes. **Verify** the
   per-file digest today is SHA-256 over the exact file bytes (`write_jsonl_bytes` ~:735-736) feeding
   `integrity.per_file_digests` → `aggregate_payload_digest` (`artifact.rs` ~:67-74) → signed manifest, so digest
   identity is sufficient to keep the signature stable.

6. **Correctness/safety:** partial-file-on-error is fine because the build stages under a pending dir and
   discards/rebuilds on crash (`cycle.rs` ~:13-33) — confirm that's real and the change doesn't weaken it. Flush the
   `BufWriter` and surface the flush error before trusting the digest. All reads stay inside the one RepeatableRead
   `tx` (committed ~:403); no new connection/cursor crossing the tx. The outbox fence is already released before
   materialisation (~:181) so streaming doesn't extend the fenced critical section — confirm.

## Deliver
Confirm the design is sufficient to prevent the re-OOM (especially claim #1) and preserves the signed-manifest
byte-identity (claims #2-#5), OR identify the specific gap + the concrete adjustment, against the real source.
End with a GO / GO-with-adjustments / NO-GO verdict on the design.

## Answer

# Design Validation: incremental delta-build memory bound

## Verdict Summary

The proposed payload streaming design is byte-correct for the JSONL payload files and should address the observed OOM class represented by an 8.3 GB `documents.upsert.jsonl`: the current implementation accumulates full payload bodies in Rust and then builds another contiguous `String`/`Vec<u8>` before writing.

However, the design is not literally sufficient to prove "peak memory to a few GB regardless of delta size" as written. There is a full-window outbox scope fetch and full Rust coalescing state before materialisation:

- `scopes_changed_for_corpus_with_client` uses `client.query(...) -> Vec<Row>` for the whole `(lo, hi]` window and then collects a `Vec<ChangedScope>` (`crates/jurisearch-storage/src/outbox.rs:243-276`).
- `build_incremental_inner` clones those scope keys into several `BTreeSet<String>`/`BTreeMap` structures (`crates/jurisearch-package-build/src/incremental.rs:193-242`).

That scope/coalescing memory is not payload-sized, so it is very likely not the primary cause of the 8.3 GB partial payload and the 96 GiB OOM. But it is still O(number of changed scopes), so it violates the strict "regardless of delta size" claim. Treat the design as **GO-with-adjustments**: stream payload files now, and either explicitly accept O(unique scopes) memory as a bounded operational assumption or add DB/cursor-backed scope coalescing before claiming a hard delta-size-independent bound.

## Claim 1: OOM Source And Full-Delta Queries

The load-bearing payload OOM diagnosis is substantially correct.

Current unbounded Rust-side payload accumulators:

- Base table upserts/deletes: `upserts: Vec<Value>` and `deletes: Vec<Value>` are accumulated across every key for a table (`incremental.rs:251-289`). For a large legislation delta, `documents.upsert.jsonl` directly maps to this path.
- Graph edges: `edges: Vec<Value>` accumulates all current edges for all document scopes before writing (`incremental.rs:292-313`).
- Replace sets: `replace_sets: Vec<ReplaceSet>` accumulates every materialized replace set, including nested chunk bodies and embedding vectors, before grouped serialization (`incremental.rs:316-390`).
- The write helpers double peak for each non-empty file: `write_jsonl_op` serializes every row to `Vec<String>`, joins into one `String`, appends a newline, converts to `Vec<u8>`, and only then writes (`incremental.rs:687-715`). Replace-set files do the same pattern inline at `incremental.rs:375-386`.

The per-payload-row fetches are scope-local:

- `read_scope_rows` builds a predicate on one scope key for each base table and orders by PK (`incremental.rs:554-588`).
- `graph_edges` is fetched per `from_document_id` and ordered by `edge_id` (`incremental.rs:296-302`).
- `replace_set_rows` queries the tables in one replace-set group for one `document_id` (`crates/jurisearch-storage/src/incremental.rs:84-103`), using per-table document predicates in `replace_set_table_select`.

But there is a full-window query before those per-scope reads:

- `scopes_changed_for_corpus_with_client` selects every `package_change_log` row for the corpus/window ordered by `change_seq`, materializes all rows with `client.query`, then collects them into `Vec<ChangedScope>` (`outbox.rs:243-276`).
- `build_incremental_inner` then clones keys into `base_keys`, `documents_scoped`, `chunks_touched`, `chunk_embeddings_touched`, `zone_touched`, and `decision_zones_touched` (`incremental.rs:193-242`).

So the statement "no query in this path returns the whole delta" is false for the change-scope fetch. The narrower statement "no query returns the whole payload body delta" is true from the source I checked. Streaming file writes alone removes the dominant payload-body accumulators, but not the O(scope count) memory.

Concrete adjustment if the hard bound is required:

- Move scope coalescing out of Rust memory. Use SQL to produce the final sorted key streams per output category, for example distinct base keys per table, `documents_scoped`, `chunks_wide`, `chunk_emb_only`, `zone_touched`, and `decision_zones_touched`, then consume each stream with a server-side cursor or bounded pagination inside the same repeatable-read transaction.
- Preserve the existing key ordering by making those SQL streams match the current `BTreeMap`/`BTreeSet` order. For replace-set groups, the output order must remain sorted document IDs within each group.
- If you intentionally keep Rust `BTreeSet` coalescing, document the memory contract as "bounded by unique changed scopes plus largest per-scope materialization", not "regardless of delta size."

Also note a smaller residual bound: `query_json_rows` still returns a `Vec<Value>` per scope/document (`incremental.rs:680-683`), and `materialize_replace_set` still holds one full replace set (`incremental.rs:621-659`). That is not full-delta memory, but the true bound is "largest scope/replace-set plus buffering", not strictly constant.

## Claim 2: Byte Identity Of Streamed JSONL

The byte-identity claim is correct if the stream uses the same row order and the same serde compact JSON serializer.

Current base/edge JSONL bytes:

- Empty rows return early with no file (`incremental.rs:697-699`).
- Non-empty rows are serialized with `serde_json::to_string`, collected, joined with `"\n"`, and passed as `(lines + "\n").into_bytes()` (`incremental.rs:700-711`).
- `write_jsonl_bytes` writes exactly those bytes with `std::fs::write` and adds no framing, BOM, sorting pass, compression, or prefix/suffix beyond the bytes it receives (`incremental.rs:719-752`).

Current replace-set JSONL bytes are the same shape:

- `group_sets.iter().map(serde_json::to_string).collect::<Vec<_>>()?.join("\n")`
- Then `(lines + "\n").into_bytes()` (`incremental.rs:375-386`).

Therefore the current file content is exactly:

```text
l1\nl2\n...\nlN\n
```

A streaming implementation that, for each logical row in the same order, writes `serde_json::to_string(row)?` followed by `b"\n"` will be byte-identical. There is no canonical/sorted-key serializer used for these JSONL payload rows; canonical JSON is used for manifest/signature/digest paths, not JSONL file serialization.

For very large replace sets, consider `serde_json::to_writer` directly into the hashing writer followed by `b"\n"` to avoid allocating one huge line string. That should be byte-equivalent to `to_string` for serde's default compact formatter, but if the implementation wants maximum conservatism against byte drift, add a focused test comparing `to_string` and `to_writer` for representative `Value` and `ReplaceSet` rows.

## Claim 3: Ordering And Manifest Signature

Ordering is load-bearing because canonicalization preserves array order (`crates/jurisearch-package/src/canonical.rs:112-114`), and the signed payload is the canonical bytes of the manifest payload (`crates/jurisearch-package/src/signed.rs:20-31`, `:44-50`).

Current push/write order:

- Base tables are processed in `base_keys: BTreeMap<&str, BTreeSet<String>>` order (`incremental.rs:194`, `:252`). Within each table, keys are sorted by `BTreeSet`.
- For each base table, current code writes the upsert operation first, then the delete operation (`incremental.rs:270-289`). Empty operations emit nothing.
- For a missing base row, the delete branch is emitted only when `table != "decision_legislation_citations"` (`incremental.rs:260-266`).
- `graph_edges` comes after all base-table files, only when `documents_scoped` is non-empty (`incremental.rs:292-314`), with documents sorted by `documents_scoped` and edges ordered by `edge_id`.
- Replace sets are materialized in four contiguous groups: `ChunksWithEmbeddings`, `ChunkEmbeddings`, `ZoneUnits`, `DecisionZones` (`incremental.rs:317-361`), then files are emitted in the fixed group order at `incremental.rs:362-390`.
- The deleted-document skip applies only in the `chunks_wide`/`ChunksWithEmbeddings` loop when the document came from a document scope and no longer exists (`incremental.rs:318-322`).

A streaming pass must preserve metadata push order as well as file byte order. In practice, finalize and push `PayloadFile`/`OperationCount` in the same sequence as the current `write_jsonl_op`/`write_jsonl_bytes` calls:

1. For each base table in the same sorted map order: upsert, then delete.
2. `graph_edges` upsert.
3. Replace-set files in `[ChunksWithEmbeddings, ChunkEmbeddings, ZoneUnits, DecisionZones]`.

If lazy writers are opened during the loop, do not push manifest metadata at open time in a way that changes this sequence. Push when finalizing each logical operation in the current order.

## Claim 4: Empty-File Semantics And Counts

The empty-file semantics claim is correct.

Today `write_jsonl_op` returns before file creation, digest insertion, operation insertion, or payload-file insertion when `rows.is_empty()` (`incremental.rs:697-699`). Replace-set group files are skipped when `group_sets.is_empty()` (`incremental.rs:368-374`).

Lazy open preserves this if "opened" means "at least one operation row was serialized." For base/edge files, the operation count must equal the number of streamed JSON object rows. For replace-set files, the count must equal the number of replace-set envelopes, not the number of nested rows. A replace-set envelope with empty nested `rows` is still one operation row today and must still produce one JSONL line.

## Claim 5: Streamed Digest And Signed Manifest Identity

Digest identity is sufficient for signed-manifest identity, provided all manifest arrays and row counts are also unchanged.

Current digest path:

- `write_jsonl_bytes` computes `digest_bytes(&bytes)` over the exact bytes written to the payload file (`incremental.rs:731-736`).
- That digest is inserted into both `integrity.per_file_digests` and the corresponding `PayloadFile.digest` (`incremental.rs:736-751`).
- `aggregate_payload_digest` hashes the sorted `name=digest` pairs from the `BTreeMap` (`crates/jurisearch-package/src/artifact.rs:60-74`).
- The aggregate becomes `integrity.artifact_sha256` and `integrity.uncompressed_payload_digest` (`incremental.rs:457-460`).
- The manifest digest is `canonical_digest(&manifest)` and `Signed::seal` signs the manifest payload (`incremental.rs:500-505`; `signed.rs:20-31`).

`digest_bytes` is SHA-256 over raw bytes and formats as `sha256:<lowercase hex>` (`canonical.rs:53-59`). `tee_digest` uses the same private `format_sha256` (`canonical.rs:61-97`). A `HashingWriter<W>` implemented in `canonical.rs` can reuse that formatter and be byte-identical to `digest_bytes` for the same stream.

Implementation details that matter:

- Flush the `BufWriter` and surface the flush error before inserting/trusting the digest, matching the baseline fix's explicit `writer.flush()?` (`baseline.rs:324-330`).
- If `HashingWriter` updates the hasher before `write_all`, never expose/use the digest after an error. A cleaner implementation updates only after a successful inner write of the accepted bytes.
- `format_sha256` is currently private. Put `HashingWriter` in `canonical.rs` next to `tee_digest`, or intentionally expose a formatting helper rather than duplicating digest formatting.

## Claim 6: Crash Safety, Transaction Scope, And Fence Duration

The staging/crash-safety argument is real.

- The producer cycle builds directly into `root/<corpus>/.staging/pending` (`crates/jurisearch-package-build/src/cycle.rs:151-168`; `publish.rs:30-38`).
- A prior staged artifact is resumed first if it has a catalog row; an uncataloged staged artifact is discarded (`cycle.rs:386-405`).
- Before building a fresh incremental, the pending slot is cleaned and recreated (`cycle.rs:157-162`).
- Publish copies the staged artifact to a temporary sibling and renames it into the immutable served package directory (`publish.rs:74-79`).

Partial payload files from an interrupted build are therefore not client-visible. If the crash happens before a manifest/catalog row, the next cycle cleans and rebuilds. If it happens after cataloging a complete staged artifact, the next cycle resumes/publishes the same package.

The transaction/fence claim is also correct:

- `build_incremental_inner` starts one read-only repeatable-read transaction (`incremental.rs:174-178`).
- It freezes `hi`, then releases the outbox fence before scope materialisation (`incremental.rs:179-181`).
- Payload materialisation, postcondition digests, and schema bundle digest all happen on the same `tx`, which commits at `incremental.rs:393-403`.

Streaming file writes does not extend the fenced critical section, because the fence is already released before materialisation. It does keep the repeatable-read transaction open for the duration of streaming, but that is already true for the existing materialisation/write path.

## Required Adjustment

For the observed OOM and the 8.3 GB partial `documents.upsert.jsonl`, implement the streaming payload writers as proposed.

For the stated hard guarantee, add one of these adjustments:

- **Preferred hard-bound design:** replace `scopes_changed_for_corpus_with_client` plus Rust `BTreeSet` coalescing with DB-produced final key streams per output category, consumed by cursor or bounded pages inside the same repeatable-read transaction. This avoids full-window `Vec<ChangedScope>` and full Rust coalescing memory.
- **Acceptable narrower design:** explicitly document the bound as "payload memory is bounded by the largest scope/replace-set and writer buffer; scope coalescing remains O(unique changed scopes)." This is probably acceptable for the current failure mode, but it is not the absolute guarantee requested.

## Verdict

**GO-with-adjustments.**

The streamed payload writer design preserves JSONL bytes, per-file digests, aggregate payload digest, manifest array order, and therefore signed-manifest identity if implemented in the current order with lazy empty-file handling and checked flushes. It should prevent the observed payload-body OOM. The adjustment is that the current full-window outbox scope fetch and Rust coalescing sets are still O(delta scopes); stream or DB-coalesce those before claiming peak memory is bounded to a few GB regardless of delta size.
