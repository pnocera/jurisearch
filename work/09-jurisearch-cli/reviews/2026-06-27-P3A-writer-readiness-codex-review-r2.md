# P3A Writer Readiness Re-review

## Findings

### WARN: idempotent no-op applies still bypass stamp verification/repair

The fresh apply paths now stamp readiness in the activation transaction and the normal incremental
path restamps after cursor advance. The idempotent branches still return success before proving the
writer-owned stamp exists for the already-active topology:

- `crates/jurisearch-syncd/src/apply.rs:187-195` returns the media package idempotency outcome before
  reaching activation, and `crates/jurisearch-syncd/src/apply.rs:666-673` decides `AlreadyApplied`
  using only sequence/package id/digest.
- `crates/jurisearch-syncd/src/apply.rs:918-924` commits and returns `IncrementalApplyOutcome::AlreadyApplied`
  before the restamp at `crates/jurisearch-syncd/src/apply.rs:1061-1064`.
- The read path now correctly fails closed on a missing or stale stamp at
  `crates/jurisearch-storage/src/ingest_accounting/readiness.rs:431-459`.

That leaves an operational false-green: a client already at the target package identity but missing
`public.index_manifest['query_readiness']` (for example after upgrading from a pre-P3A applied
database, or after a corrupted/manual stamp deletion) can receive a successful `AlreadyApplied`
result while subsequent reads still fail as "never stamped" or stale. The current tests cover fresh
activation stamping, normal successful incremental restamping, missing/stale read failures, and a
coverage-breaking incremental rollback, but they do not cover idempotent baseline/rebaseline or
incremental apply with a missing stamp.

Actionable fix: on `AlreadyApplied`, either verify the matching stamp before returning success or
restamp the current `active_generation` through the writer connection/transaction. Add regression
coverage that deletes `query_readiness`, reapplies the same baseline/rebaseline and the same
incremental package, and asserts either a repaired readable stamp or an explicit apply error rather
than a successful no-op with the site still unreadable.

## Notes

The prior blocker about dense coverage using the wrong fingerprint is fixed: the generation coverage
helper now requires both `chunks.embedding_fingerprint` and `chunk_embeddings.embedding_fingerprint`
to match `corpus_state.embedding_fingerprint`, and the new mismatch test exercises that failure path.

The prior multi-corpus authorization blocker is fixed on the read side: installed-topology readiness
lookup now fails closed when more than one active corpus exists, so a single-corpus stamp cannot
authorize fetch/BM25 over aggregate server views in 3A.

The prior incremental-test gap is addressed by the shared-writer loopback negative test: a dense-incomplete
incremental is built with valid package postconditions, refused by the restamp gate after cursor advance,
and asserted to leave the cursor and applied rows unchanged.

I reviewed the working-tree diff, the prior review, the new tests, and the relevant source paths. I did
not rerun the validation suite listed in the instructions.

VERDICT: GO
