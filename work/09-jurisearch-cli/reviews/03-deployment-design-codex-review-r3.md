# Re-review: work/09-jurisearch-cli/03-deployment-design.md

The prior review's side-effecting CLI payload-builder and unattached protocol-version findings are resolved in this revision. The object-safety, snapshot-topology, codec-boundary, renderer-boundary, and OCP wording fixes also remain resolved. I found one remaining design-contract blocker and one warning below.

## Findings

### BLOCKER: Read-role visibility for newly activated generation schemas has no owner

The design correctly splits read and write identities through `StorageBackend`, `QueryStore`, and `CorpusWriter` (`03-deployment-design.md:117-146`), and the read path is explicitly supposed to fan out over the active physical generation schemas (`03-deployment-design.md:131-136`, `:274-288`). But the architecture this document claims to realize makes a stronger, load-bearing requirement: every activation must leave the read-only identity able to read `corpus_state`, `index_manifest`, the stable views, and the newly created generation schemas at commit time (`02-target-architecture.md:323-328`; also `:283-288`).

That responsibility is not assigned anywhere in the design. `CorpusWriter::apply` is described as `build→validate→activate→stamp` (`03-deployment-design.md:139-142`), and the DRY ledger names activation/readiness authorities, but there is no authority for propagating read-role grants or proving read-role visibility. This matters because the existing substrate creates generation schemas dynamically (`crates/jurisearch-storage/src/generations.rs:156-190`) and the current activation transaction updates state, dense metadata, and views without any privilege handoff (`crates/jurisearch-storage/src/generations.rs:1060-1132`). As written, an implementation can satisfy the type-level read/write split while still activating a generation that the query service's read-only role cannot read; the first post-activation query can then fail despite the design's "active generation is ready" invariant.

Concrete fix: add a first-class read-role visibility/grant authority to §3.2 and the DRY ledger, owned by `CorpusWriter`/the activation path or by `StorageBackend` as an activation helper. State that generation creation/activation must grant the query read role the required schema/table/control-view visibility for each newly activated physical generation before the cursor/view switch commits, or must otherwise prove the same postcondition inside the switch transaction. Make the activation postcondition explicit: after `activate_generation_with_guard` plus readiness stamping, the read identity can read the full active topology selected by `ActiveCorpusResolver`; if not, the apply fails with cursor unchanged.

### WARN: The `Operation` vocabulary is still not attached to request parsing

Section 3.1 says the operation vocabulary lives once in the contract, but the sketch keeps `SessionRequest` as `{ id, command: String, args }` and declares `Operation` beside it (`03-deployment-design.md:84-91`). Section 3.7 then registers handlers by `Operation` (`03-deployment-design.md:216-230`), and the DRY ledger treats the wire envelope plus operation vocabulary as one authority (`03-deployment-design.md:340-347`). The missing piece is the contract-owned conversion between the raw wire `command` string and the closed `Operation` enum.

Without that specified, implementers can easily recreate today's string-match shape in two places: the thin client emits strings, and the site service parses/allowlists strings locally. That weakens the claimed single operation authority and makes the narrow query API depend on ad hoc parsing rather than on the contract enum. Today's source starts from exactly that risk: `SessionRequest.command` is a `String`, and the existing dispatcher matches literal command strings including non-query/admin/model/eval commands (`crates/jurisearch-core/src/session.rs:7-13`, `crates/jurisearch-cli/src/session.rs:121-145`).

Concrete fix: either make the wire field typed (`command: Operation`, with stable serde names/aliases if the legacy field name must stay), or define contract-owned `Operation::parse_command` / `Operation::as_command` functions that both the thin CLI and `SiteDispatcher` must use. Specify that unknown, legacy, admin, model, ingest, and eval strings are rejected before handler lookup with the shared transport/session error shape, not routed to the old dispatcher.

VERDICT: FIXES_REQUIRED
