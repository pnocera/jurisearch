# Global work/09 review

## Findings

### BLOCKER 1 - The site service still rejects every multi-corpus topology, so the shipped thin-client path cannot deliver the "all corpora" endpoint.

The target architecture makes multi-corpus service exposure load-bearing: hot search must resolve the active corpus set once per request and fan out over every physical generation (`work/09-jurisearch-cli/02-target-architecture.md:136-143`), `syncd` owns all subscribed corpora and the single query-service endpoint exposes all of them (`work/09-jurisearch-cli/02-target-architecture.md:201-203`), and the readiness decision says the writer-stamped readiness signature is aggregate over all active corpora (`work/09-jurisearch-cli/02-target-architecture.md:379-384`).

The implementation does have a storage-level P3C fan-out: `hybrid_candidates_in_snapshot` dispatches to `hybrid_candidates_fanout` when `snapshot.active_corpora().len() > 1` (`crates/jurisearch-storage/src/retrieval/hybrid.rs:49-56`), and the P3C tests exercise that storage primitive directly (`crates/jurisearch-storage/tests/query_fanout_p3c.rs:1-7`, `crates/jurisearch-storage/tests/query_fanout_p3c.rs:296-303`). But the site service never reaches it. Every site query handler calls `ensure_site_readiness` before its builder (`crates/jurisearch-cli/src/site/handlers.rs:28-36`, `crates/jurisearch-cli/src/site/handlers.rs:61-90`, `crates/jurisearch-cli/src/site/handlers.rs:114-182`), and `load_query_readiness_in_snapshot` hard-errors as soon as the snapshot has more than one active corpus (`crates/jurisearch-storage/src/ingest_accounting/readiness.rs:511-517`). Health reports this as `"multi_corpus_readiness": "deferred"` rather than ready (`crates/jurisearch-cli/src/site/handlers.rs:211-227`).

That means a site that subscribes to, for example, `core` plus a second corpus becomes unusable through the actual `serve-site`/`jurisearch-client` path: `fetch`, `search`, `cite`, `related`, `context`, and `compare` all fail before any physical-generation fan-out or union-view by-id read can run. The current site tests are false-green for this invariant because `ready_site` activates only one `core` corpus (`crates/jurisearch-cli/src/site/tests.rs:47-82`), while the multi-corpus coverage is isolated in storage tests that bypass the site readiness gate.

Fix direction: make writer-owned readiness truly aggregate for the active topology. On every activation/incremental topology change, compute and stamp readiness for all active corpora or a per-corpus stamp set with an aggregate signature. Then allow `load_query_readiness_in_snapshot` to validate `>1` active corpora, and add a site/thin-client e2e with two active corpora proving that `search` reaches physical fan-out and by-id/status paths still work.

### BLOCKER 2 - The required operated two-host acceptance evidence is still a placeholder.

P6 requires operational evidence, not just unit/integration tests: the plan calls for systemd/config plus "a two-host acceptance run" (`work/09-jurisearch-cli/04-implementation-plan.md:276-285`) and says done means "a thin client on a second machine queries the site service by URL" (`work/09-jurisearch-cli/04-implementation-plan.md:288`). The checked-in runbook explicitly says the real two-physical-host evidence must be filled in when operated (`work/09-jurisearch-cli/05-two-host-acceptance.md:18-19`), but the `OBSERVED` block is still empty placeholders for hosts, sequence, package digest, health output, fetch hashes, and negative checks (`work/09-jurisearch-cli/05-two-host-acceptance.md:74-91`).

The single-host tests are useful, but they do not prove the deployment is complete and operable across the producer -> site server -> thin client boundary. In particular, they do not prove the published package root, system PostgreSQL, systemd units, local bge-m3 service, LAN bind, and thin client on a separate host work together. Until that evidence is recorded, the work/09 deployment claim is incomplete.

Fix direction: perform the operated run and replace the placeholder block with concrete host identifiers, applied package id/digest, syncd cursor/head evidence, health output, thin-client fetch/search output or checksums, and the unreachable/skew/bad-URL negative results. If the intended acceptance is only single-host CI, update the plan and target docs; as written, they require real operated evidence.

### WARN 1 - The contract-owned typed request DTO seam promised by the design/plan is not actually implemented.

The design says the shared wire contract owns both `Operation::parse_args` and typed per-operation request DTOs, with both the thin client and server handlers using those DTOs so defaults and validation have one authority (`work/09-jurisearch-cli/03-deployment-design.md:91-108`, `work/09-jurisearch-cli/03-deployment-design.md:376-382`). The implementation plan repeats that P1 deliverable as `Operation` plus typed `RequestDto` and `parse_args` (`work/09-jurisearch-cli/04-implementation-plan.md:71-78`).

The live contract only owns operation strings: `Operation` has `as_command` and `parse_command`, but no `parse_args` or `RequestDto` (`crates/jurisearch-core/src/operation.rs:42-72`). The request DTOs that carry serde defaults still live in the heavy CLI crate and include `index_dir` fields for the local/session surface (`crates/jurisearch-cli/src/request.rs:1-6`, `crates/jurisearch-cli/src/request.rs:15-56`, `crates/jurisearch-cli/src/request.rs:108-118`, `crates/jurisearch-cli/src/request.rs:131-143`). Site handlers import those CLI request types directly for several operations and define a separate strict `SiteFetchArgs` locally (`crates/jurisearch-cli/src/site/handlers.rs:20-24`, `crates/jurisearch-cli/src/site/handlers.rs:39-55`). The thin client is a raw `command` plus JSON `args` sender (`crates/jurisearch-client/src/main.rs:28-32`), not a DTO-producing client.

This does not break the current raw-JSON thin client by itself, and the dependency-cone test is green, but it leaves an important DRY/SOLID seam incomplete. Site validation/defaults are tied to `jurisearch-cli` internals rather than a dependency-light contract authority, and future friendly thin-client commands can drift from server-side validation unless they reintroduce another mapping layer.

Fix direction: move the site request DTOs/defaults/validation into the dependency-light contract/core layer (or explicit conversion types that keep the core crate independent of storage), make `Operation::parse_args` the shared server/client boundary, and keep server-owned fields such as `index_dir` out of the site DTOs entirely.

## Validation

- `git status --short --branch`
- `cargo test -p jurisearch-cli site::tests -- --nocapture`
- `cargo test -p jurisearch-storage --test query_fanout_p3c -- --nocapture`
- `cargo test -p jurisearch-client --test dependency_cone -- --nocapture`
- `cargo test -p jurisearch-package-build --test daemon_loop -- --nocapture`

VERDICT: FIXES_REQUIRED
