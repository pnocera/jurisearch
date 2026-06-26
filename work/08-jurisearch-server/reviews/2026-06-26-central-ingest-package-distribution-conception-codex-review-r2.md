# Re-review - central ingest package distribution conception

Reviewed:
- `work/08-jurisearch-server/2026-06-26-central-ingest-package-distribution-conception.md`
- `work/08-jurisearch-server/reviews/2026-06-26-central-ingest-package-distribution-conception-codex-review.md`
- `work/08-jurisearch-server/2026-06-26-central-ingest-package-distribution-design.md`
- `work/08-jurisearch-server/2026-06-25-central-ingest-delta-sync-analysis.md`

R1 resolution check:
- WARN 1 is resolved for the `minimum_client_version` authority split: the conception now separates content compatibility from client-binary compatibility and correctly states that `minimum_client_version` is not carried in the outbox or cursor.
- WARN 2 is resolved: catch-up is now qualified to the retained compatible incremental chain, with a fresh signed baseline fallback when the chain is unavailable or too expensive.
- WARN 3 is resolved in substance: the new-table OCP row now limits genericity to existing identities, dependency order, and event kinds, and the `replace_set` row is now an abstract scoped derived-set replacement with concrete examples.
- NIT 1 is resolved: ordering is now described as one per-corpus package-sequence coordinate space.
- NIT 2 is resolved: index materialisation now distinguishes baseline/rebaseline finalisation, ordinary incremental row-level maintenance, and rare additive index DDL.
- NIT 3 is resolved: the one-directional replication wording now distinguishes local service materialisation from client-application semantic mutation.

## BLOCKER

None.

## WARN

### 1. The compatibility rewrite now over-propagates the schema/extension bundle digest

The r1 `minimum_client_version` issue is fixed, but the new content-compatibility row now says the stamp set is `schema_version`, `embedding_fingerprint`, `builder_versions` "plus the schema/extension-bundle digest" and that the whole set is propagated identically through "outbox -> package -> both manifests -> control cursor -> precondition check" (`conception.md:93`). That does not match the design contract. The outbox fields are `builder_versions`, `embedding_fingerprint`, and `schema_version`, with no schema/extension digest (`design.md:213-230`); the embedded manifest carries `schema_version` plus a schema-migration-bundle digest and extension requirements (`design.md:438-443`) and separately carries `schema_ops_digest` in the apply contract (`design.md:450-456`); the client cursor stores only `schema_version`, `embedding_fingerprint`, and `builder_versions` as compatibility state (`design.md:497-508`). The analysis's known manifest field set likewise names corpus, sequence, minimum client version, schema version, embedding fingerprint, builder versions, and signature, not a cursor-carried schema bundle digest (`analysis.md:386-388`).

Why this matters: the conception now tells an implementer to persist and compare a bundle digest as if it were the same kind of cursor state as schema/fingerprint/builder versions. That either creates a client cursor/outbox field the design does not specify, or blurs artifact integrity/apply-contract digests with durable content-state stamps.

Recommended fix: keep the row's r1 split, but separate the digest path. Say the cursor-carried content compatibility stamps are `schema_version`, `embedding_fingerprint`, and `builder_versions`. Then say schema/extension bundle digests are artifact/apply-contract integrity and precondition material carried by the package/embedded manifest, and optionally by the remote manifest for planning if the design is amended, but not by the outbox or `corpus_state` unless the design explicitly adds those fields.

## NIT

None.

VERDICT: FIXES_REQUIRED
