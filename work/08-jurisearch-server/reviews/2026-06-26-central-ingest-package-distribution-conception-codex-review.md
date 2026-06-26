# Code review - central ingest package distribution conception

Reviewed:
- `work/08-jurisearch-server/2026-06-26-central-ingest-package-distribution-conception.md`
- `work/08-jurisearch-server/2026-06-26-central-ingest-package-distribution-design.md`
- `work/08-jurisearch-server/2026-06-25-central-ingest-delta-sync-analysis.md`
- `work/08-jurisearch-server/00-idea.md`
- Source truth paths for deterministic identities, current migrations, derived-set rebuilds, chunk upserts, dense finalization, and the existing `serve` daemon.

## BLOCKER

None.

## WARN

### 1. The compatibility DRY row assigns `minimum_client_version` to the wrong authority path

The conception defines the compatibility stamp set as `minimum_client_version`, `schema_version`, `embedding_fingerprint`, and `builder_versions`, then says that set is "stamped once at build" and propagated identically through "outbox -> package -> both manifests -> control cursor -> precondition check" (`conception.md:88-89`). That contradicts the design contract. The outbox carries data/build compatibility facts such as `schema_version`, `embedding_fingerprint`, and `builder_versions` (`design.md:213-230`), and the client cursor stores `schema_version`, `embedding_fingerprint`, and `builder_versions` (`design.md:497-508`). `minimum_client_version` is a package/manifest gate checked against the running binary (`design.md:692-710`), not a value that belongs in the ingest outbox or in `corpus_state`.

Recommended fix: split this row into two concepts. Make "content compatibility" the carried stamp set (`schema_version`, `embedding_fingerprint`, `builder_versions`, extension/schema bundle digest as applicable) that can flow into the cursor. Make "client binary compatibility" a package/remote-manifest selection and apply precondition (`minimum_client_version`) that the service compares to its own version, without claiming it is propagated through the outbox or persisted in the cursor.

### 2. The catch-up invariant silently drops the design's fresh-baseline fallback

The conception says catching up means "replaying the missing segments in order" (`conception.md:47-48`) and later states that "a missed package is caught up by replaying in order, never skipped" (`conception.md:283-285`). That is true only inside a retained, compatible incremental range. The design resolved catch-up as size/cost driven: the client prefers incremental catch-up while there is no gap and the cumulative diff is below thresholds, but it should prefer a fresh baseline when `sequence < min_available_sequence`, cumulative diff bytes are too large, the range crosses a full reissue, or apply/index time exceeds media load time (`design.md:669-688`). The conception's bottom line repeats the absolute replay framing for offline clients (`conception.md:331-332`).

Recommended fix: qualify the invariant as "within an incremental chain, missed packages are applied in order and never skipped." Add the complementary rule that when the remote manifest says the retained chain is unavailable or too expensive, catch-up becomes applying a signed baseline/rebaseline chain root, not replaying every missing incremental.

### 3. The OCP/table-growth language overstates how generic the apply engine can be

The conception says a new replicated table joins by "role membership" and does not reopen the engine or event vocabulary (`conception.md:165-172`). It also describes `replace_set` as reusing the existing `replace_zone_units_for_document` invariant (`conception.md:91`). That is too broad against the design and source. The design has table-group-specific contracts: `official_api_responses` must preserve producer-assigned `response_id` and apply before citation tables (`design.md:274-288`, `design.md:458-462`), `zone_units` and `zone_unit_embeddings` use document-scoped replacement (`design.md:290-324`), and `chunks_with_embeddings` must also be a document-scoped replacement when chunk membership, partitioning, or body changes (`design.md:325-339`). The source supports why that special case matters: LEGI chunk projection upserts current chunks but does not delete dropped chunks (`crates/jurisearch-storage/src/projection/legi.rs:71-92`), while the zone-unit writer really is a delete-scope-then-insert operation (`crates/jurisearch-storage/src/zone_units.rs:120-169`).

Recommended fix: narrow the OCP claim. Say the outer apply loop is closed for tables that fit existing declared identities, dependency ordering, payload layout, and event kinds. If a new table needs a new identity exception, FK ordering, or set-replacement scope, the manifest/apply contract must be extended deliberately, while still preserving the closed outer lifecycle. Also rewrite the `replace_set` DRY row as an abstract "scoped derived-set replacement" and use `zone_units` and `chunks_with_embeddings` only as examples, not as the whole invariant.

## NIT

### 1. "All the same number" should be "the same sequence coordinate system"

The ordering row says `from`/`to`, `head`/`min_available`, and `corpus_state.sequence` are "all the same number" (`conception.md:88`). The design's point is that they all use the same per-corpus package-sequence domain, not that they are equal values: `head_sequence`, `min_available_sequence`, package `from_sequence`/`to_sequence`, and the client's last-applied `sequence` are related positions in one coordinate system (`design.md:246-257`, `design.md:389-428`, `design.md:497-508`).

Recommended fix: replace "all the same number" with "all coordinates in the same per-corpus package-sequence space" and keep the parenthetical distinction from global `change_seq`.

### 2. The index-builder wording sounds like every apply finalizes indexes

The conception says indexes are "built once (on the client)" (`conception.md:107-109`), lists an index builder that "finalises IVFFlat / BM25 as part of activation" (`conception.md:141-142`), and states that activation including index build precedes cursor advance (`conception.md:286-288`). The design is more precise: baseline/rebaseline does full index finalization before the view switch, ordinary incrementals rely on PostgreSQL row-level index maintenance inside the apply transaction, and only an incremental carrying new index DDL runs an explicit build before cursor advance (`design.md:532-540`, `design.md:646-660`). The current dense paths also show finalization is a drop/recreate operation for IVFFlat indexes (`crates/jurisearch-storage/src/dense.rs:93-192`, `crates/jurisearch-storage/src/zone_units.rs:431-524`), not something every ordinary incremental should imply.

Recommended fix: change the client-service bullet to "Index materializer - ensures the manifest-declared index state before activation/cursor advance." Add one sentence that baselines/rebaselines finalize indexes, ordinary incrementals use engine-maintained indexes, and additive index DDL is the rare explicit incremental build.

### 3. "The client never mutates replicated state" is too absolute

The conception says the client "never mutates replicated state" (`conception.md:47-49`) and repeats "Clients never mutate replicated state" as an invariant (`conception.md:281-282`). The intended boundary is correct, but the wording can be misread because the local service necessarily writes the local replicated generation while applying signed packages. The design says the service owns package apply and cursor advancement (`design.md:475-490`) and ordinary incrementals run a transaction into the active generation (`design.md:516-530`).

Recommended fix: say "the client application/CLI never originates semantic mutations to server-managed data; the local service only materializes signed producer packages into the local replica." That preserves the one-directional replication principle without denying the apply writes.

VERDICT: FIXES_REQUIRED
