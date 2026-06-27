# Re-review: work/09-jurisearch-cli/03-deployment-design.md

The two r4 findings are resolved in this revision. The prior blocker on per-operation `args`
ownership is addressed by making typed request DTOs, defaults, and validation contract-owned via
`RequestDto` / `Operation::parse_args`, with both the thin client and site dispatcher using that same
authority (`03-deployment-design.md:91-114`, `:378-382`). The prior warning on command parsing using
package `Reject` is addressed by returning the session error shape (`ErrorObject` /
`SessionResponse::Err`) for command allowlist and argument failures, while keeping the work/08
`Reject` vocabulary scoped to package verify / apply / fingerprint-preflight (`03-deployment-design.md:93`,
`:111-114`, `:380`, `:390`).

I did not find new BLOCKER, WARN, or NIT findings in the current design. The updated document keeps the
thin-client dependency cone structurally separate from the heavy CLI stack, moves side-effecting CLI
payload functions behind extracted response builders, and preserves the read-only query-service
boundary through snapshot-scoped `QueryStore` access and writer-owned activation/readiness
postconditions (`03-deployment-design.md:146-179`, `:276-302`, `:350-367`).

VERDICT: GO
