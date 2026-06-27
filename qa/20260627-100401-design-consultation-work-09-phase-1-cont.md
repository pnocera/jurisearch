# Phase 1 Design Consultation - Contract / Codec / Render Base

## Verdict

**GO-with-adjustments.**

The crate-boundary direction is sound, but I would tighten the Phase 1 slice:

- Treat `jurisearch-core` as the implementation of the planned contract base, and make the docs/tests explicit that the "contract crate" in P1 is `jurisearch-core`.
- Move only the pure retrieval request wire types (`RetrievalMode`, `GroupBy`, `RetrievalOptions`) into `jurisearch-core`, and re-export them from `jurisearch-storage`.
- Build `Operation`, `parse_command`, `as_command`, the site allowlist, `ProtocolEnvelope`, and the transport/render shells in P1.
- Defer full typed per-operation `RequestDto` + `parse_args` until P4, unless you also migrate the current CLI request DTO consumers in the same slice. Building a full unused parallel DTO set in P1 is the bigger DRY risk.

Source basis: the plan explicitly builds on `jurisearch-core::session` and says local bare `SessionRequest` parsing is preserved (`work/09-jurisearch-cli/04-implementation-plan.md:66-91`); the design calls `jurisearch-contract` "today: jurisearch-core::session" (`work/09-jurisearch-cli/03-deployment-design.md:79-132`); `jurisearch-core` currently depends only on `serde`, `serde_json`, and `thiserror` (`crates/jurisearch-core/Cargo.toml`); `SessionRequest`/`SessionResponse` and `ErrorObject` already live there (`crates/jurisearch-core/src/session.rs:6-48`, `crates/jurisearch-core/src/error.rs:4-89`); and the local JSONL paths currently parse bare `SessionRequest` directly (`crates/jurisearch-cli/src/session.rs:88-159`, `crates/jurisearch-cli/src/serve.rs:75-123`).

## 1. Q1 - Crate identity

Reusing `jurisearch-core` as the contract crate is sound for Phase 1.

The design's crate name is serving an architectural role: the dependency-light wire base. The current `jurisearch-core` already satisfies that role: it owns the session wire structs, error object, command inventory/schema, and has no storage/embed/ingest/postgres dependency. `cargo tree -e normal -p jurisearch-core` confirms the cone is only `serde`, `serde_json`, and `thiserror`.

What I would adjust:

1. In the P1 code/tests, treat the three base crates as `jurisearch-core`, `jurisearch-transport`, and `jurisearch-render`, or add an explicit note that `jurisearch-core` is the concrete `jurisearch-contract` for this implementation.
2. Do not create a new thin `jurisearch-contract` crate that only re-exports `jurisearch-core`; it adds churn without improving the cone.
3. Add the P1 dependency-cone assertion against `jurisearch-core` as the contract base, so future accidental imports of storage/embed/ingest/postgres are caught immediately.

Leaving `eval.rs`, `expand.rs`, and `schema/` in `jurisearch-core` does not compromise the dependency-cone invariant today. They are dependency-light source/schema helpers. It is an SRP smell only if future code starts pushing storage, embedding, or CLI behavior into core. For P6 thin-client cone purposes, depending on core will not pull heavy crates.

The main naming hazard is human, not mechanical: future code might assume `jurisearch-core::contract::COMMANDS` is the site API. It is not. That table includes local/admin/session surfaces, while the site `Operation` set is only `search`, `fetch`, `cite`, `related`, `context`, `compare`, and `status`.

## 2. Q2 - Wire-enum move

Move the three pure-data request types down into `jurisearch-core`, and re-export them from `jurisearch-storage::retrieval`.

This is the cleaner direction for the current graph. There is no current `core -> storage` edge; adding `storage -> core` does not create a cycle. Existing callers can keep importing `jurisearch_storage::retrieval::{RetrievalMode, GroupBy, RetrievalOptions}` if storage re-exports the core types.

The split point in `crates/jurisearch-storage/src/retrieval/types.rs` is clear:

- Move: `GroupBy`, `RetrievalMode`, `RetrievalOptions`, plus pure methods like `as_str`, `uses_lexical`, and `uses_dense`.
- Keep in storage: `HybridCandidateQuery`, `DecisionFilters`, `RetrievalCursor`, `RelatedRelation`, SQL-adjacent query structs, `rrf_weights`, `effective_rrf_weights`, `effective_probes`, and default constants.

`RetrievalOptions` is safe to relocate. It is immutable request state: optional per-request overrides for RRF weights, probes, and authority weight. The storage-coupled behavior is how those overrides are interpreted against environment defaults and index-manifest defaults; that interpretation should stay in storage.

Implementation details to avoid drift:

1. Add `jurisearch-core = { path = "../jurisearch-core" }` to `jurisearch-storage/Cargo.toml`.
2. Add serde derives and `rename_all = "snake_case"` where these types become wire DTO fields. The current storage enums are pure Rust enums; the CLI-facing serde enums are separate `CliSearchMode` / `CliGroupBy`.
3. Do not duplicate storage enums and contract enums with conversions unless the move turns out much larger than expected. The plan explicitly prefers no duplication and no back-edge.
4. Keep validation of finite weights/probe ranges on the request boundary. Today that lives in `crates/jurisearch-cli/src/query_support.rs`; when `RequestDto::parse_args` lands, that validation needs to move or be reused from the contract side.

## 3. Q3 - Scope of typed `RequestDto` / `parse_args`

Defer the full typed `RequestDto` + per-operation `parse_args` set to P4, and make that an explicit plan adjustment.

For the minimal testable P1 slice, build:

1. `Operation` with exactly the site-exposed set: `Search`, `Fetch`, `Cite`, `Related`, `Context`, `Compare`, `Status`.
2. `Operation::parse_command` / `as_command`.
3. A table-driven allowlist test from the compatibility matrix, including every local-only and session-excluded command.
4. `ProtocolVersion` / `ProtocolEnvelope`.
5. Codec tests for versioned site frame round-trip and legacy/bare frame rejection on the site decoder.
6. Local byte-parity tests proving bare `session`, `batch`, and current `serve` behavior did not change.

Do not build a full parallel `RequestDto` set in P1 if the local dispatcher will continue to deserialize into the existing CLI-local request structs. The current CLI has a real DTO authority in `crates/jurisearch-cli/src/request.rs`; comments there explicitly say the one-shot and JSONL session paths share those request structs. A new full contract DTO set that no dispatcher consumes would immediately create two authorities for defaults, field names, validation, and server-owned `index_dir`.

The design's DRY goal is still right, but the first consumer is the P4 site dispatcher. That is the right moment to migrate or extract the DTOs because tests can prove they are actually authoritative. At P4, the DTOs should be contract-owned and should omit server-owned fields such as `index_dir`; the site dispatcher should validate every frame itself before invoking response builders.

If you want a compromise because the written plan literally lists typed DTOs in P1, implement only enough typed DTO surface for the P4A walking skeleton operation (`fetch` or `status`) and mark the full operation set as a P4 prerequisite. I would still prefer deferral over unused scaffolding.

## 4. Q4 - Compatibility matrix and version gating

Your version-gating read is correct.

The local `session`, `batch`, and current local `serve` surfaces must continue accepting bare `{id, command, args}` `SessionRequest` lines. Requiring `ProtocolEnvelope` locally would break the existing agent workflow that the plan explicitly preserves. The versioned `ProtocolEnvelope` applies to the new site protocol path, not to the legacy local JSONL path.

Make this hard to misuse in the transport crate by exposing separate APIs, for example:

- `decode_bare_request_line` / `encode_bare_response_line` for local `session`/`batch`/current `serve`.
- `decode_site_envelope_line` / `encode_site_envelope_line` for the future site service and thin client.

On the site path, an unversioned/bare legacy frame should be a transport/protocol rejection. An unknown but well-framed `command` should become a session-shaped `ErrorObject` / `SessionResponse::Err`, not a package `Reject`. Package `Reject` stays scoped to work/08 package verify/apply/fingerprint preflight.

## Additional Phase 1 risks

1. **Do not reuse the current command inventory as the site allowlist.** `COMMANDS` includes local-only commands (`expand`, `model fetch`, `eval phase1`, `setup`, `doctor`, `stats`, `inspect`, `versions`, `diff`, `help`, `schema`, `exit`) and one-shot-only commands. Site `Operation` must be its own explicit enum.

2. **Pin command spelling in tests.** Current dispatch uses `model fetch`, `help schema`, and `schema`; the matrix abbreviates "model (fetch)" and includes CLI names like `help schema --json`. The site allowlist test should test actual session command strings, not prose labels.

3. **Renderer scope can easily creep.** Current one-shot JSON output is pretty JSON plus newline, while session/serve JSONL is compact JSON plus newline. `jurisearch-render` P1 should preserve those bytes and unwrap `SessionResponse` errors consistently, but it should not absorb storage/query formatting or become a rich renderer.

4. **Keep listener policy out of the codec.** Moving `SERVE_MAX_REQUEST_BYTES` and newline framing into transport is right. Binding, timeouts, default `index_dir` injection, and local serve composition stay outside the codec.

5. **Test the dependency cone before P6.** The P1 assertion should fail if any base crate depends on `jurisearch-storage`, `jurisearch-embed`, `jurisearch-ingest`, `jurisearch-cli`, `postgres`, or model/runtime crates. This is the guard that makes reusing `jurisearch-core` safe.

6. **Avoid placeholder `parse_args`.** A `parse_args` that returns raw `Value`, skips validation, or has unimplemented variants would look like the contract authority while preserving the duplication. Either defer it or implement it for the first real site consumer with tests.
