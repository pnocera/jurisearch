# Re-review: work/09-jurisearch-cli/03-deployment-design.md

The prior review's object-safety, snapshot-topology, codec-boundary, renderer-boundary, and OCP wording findings are resolved in this revision. The remaining issues below are independent design-contract gaps.

## Findings

### BLOCKER: The design names the existing CLI payload builders as the shared query-service authority

The revised design correctly introduces a server-owned `QueryStore`/`ReadSnapshot`/`Embedder` boundary for the site service (`03-deployment-design.md:117-128`, `:213-229`), but the crate map and DRY ledger then say `jurisearch-query` operation handlers delegate to the shared payload builders and that the authority is "the existing `*_payload` builders" (`03-deployment-design.md:307-308`, `:324-326`). Those functions are not currently pure payload builders. They are CLI entrypoints that resolve client/local `index_dir`, start the self-managed `ManagedPostgres`, run the current readiness path, and use the current local retrieval/embedder stack: `search_payload` requires an index dir and opens the index before calling `search_with_postgres` (`crates/jurisearch-cli/src/retrieval/search.rs:87-101`), `search_with_postgres` calls `ensure_query_readiness` on that concrete `ManagedPostgres` (`crates/jurisearch-cli/src/retrieval/search.rs:175-197`), and non-search payloads do the same through `open_query_index` (`crates/jurisearch-cli/src/retrieval/fetch.rs:30-33`, `crates/jurisearch-cli/src/retrieval/context.rs:20-29`).

As written, an implementer has two incompatible instructions: use the new server-owned, read-only snapshot boundary, or reuse CLI payload functions that reopen the local index and carry the side effects the design is trying to remove from the query service. Reusing them would reintroduce client/local data-source selection and read-path readiness writes; not reusing them would create a second response-building path and violate the DRY ledger.

Concrete fix: split the authority name and responsibility. Define a shared, dependency-light query response builder (or per-operation service functions) that accepts already validated request args plus `ReadSnapshot`/`Embedder`/retrieval-role inputs and returns the typed/value response body. Make the current CLI `*_payload` functions adapters that perform CLI-only validation/index opening and then call that shared builder. The site `OperationHandler`s should call the same shared builder only after opening `ctx.store.begin_snapshot()` and using server-owned dependencies. Update the DRY ledger to name that extracted builder, not the existing side-effecting CLI `*_payload` functions.

### WARN: Protocol versioning is specified as a type but not attached to the wire envelope

The architecture requires thin/client server skew to fail loudly through a protocol version on the session envelope, and this design says the shared codec owns protocol-version rejection (`03-deployment-design.md:236-243`, `:298-301`). But the actual wire-contract sketch still shows `SessionRequest` as only `{ id, command, args }` and defines `ProtocolVersion` as a standalone type with no field, wrapper, or handshake frame that carries it (`03-deployment-design.md:84-90`). That leaves the codec with no specified input from which to reject an incompatible peer. The current starting DTO has the same shape (`crates/jurisearch-core/src/session.rs:7-13`), so this is not just an implementation detail hidden by today's code.

Concrete fix: make the version placement explicit in the contract design. Either add a `proto: ProtocolVersion` field to the session request/envelope, or define an outer `ProtocolEnvelope { proto, request }` / first-frame handshake that both `JsonlClient` and the server accept loop must use. Then state where compatibility is checked, what error shape is returned on mismatch, and how old unversioned frames are rejected or treated during the transition.

VERDICT: FIXES_REQUIRED
