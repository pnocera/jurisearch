# Re-review: work/09 implementation plan

## Findings

No BLOCKER/WARN/NIT findings.

## Re-review notes

- The prior BLOCKER on the shared-server writer path is resolved. The plan now makes the `ManagedPostgres`-bound writer/update refactor an explicit 2B gate before the query service or daemon can rely on a standalone site PG (`work/09-jurisearch-cli/04-implementation-plan.md:36-38`, `:118-140`, `:219-223`, `:251-255`).
- The prior BLOCKER on readiness missing incremental apply is resolved. The plan now requires one writer-owned readiness-stamp helper for every topology-changing commit, explicitly including incremental apply, with same-transaction stamping and cursor-unchanged negative tests (`work/09-jurisearch-cli/04-implementation-plan.md:39-40`, `:144-169`, `:301-303`).
- The prior WARN on dependency-light base crates being enforced too late is resolved. P1 now owns the contract/transport/render dependency-cone assertion and the wire-enum ownership decision, with the risk also tracked cross-cutting (`work/09-jurisearch-cli/04-implementation-plan.md:71-91`, `:295-297`).
- The prior WARN on incomplete site allowlist coverage is resolved. The plan now has a compatibility matrix and requires table-driven P1/P4 denial of every non-exposed command, including local-only session commands and session-excluded one-shots (`work/09-jurisearch-cli/04-implementation-plan.md:45-62`, `:234-244`).
- I also checked the current plan against the live-code constraints it names: `dispatch_session_request` remains broader than the target site API, readiness is currently cached/written on the read path, read APIs still shell through `ManagedPostgres::execute_read_sql`, and syncd/apply entry points are still structurally bound to `&ManagedPostgres`. The revised plan now sequences each of those constraints as an explicit gate with a verification surface, so I do not see a remaining plan-level blocker.

VERDICT: GO
