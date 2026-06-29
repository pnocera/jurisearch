# Claude re-review prompt: make JuriSearch simple to deploy, r2

Repo/workdir: `/home/pierre/Work/jurisearch`

Updated artifact to re-review:

- `/home/pierre/Work/jurisearch/work/10-next-plans/01-makeitsimpletodeploy.md`

Prior review:

- `/home/pierre/Work/jurisearch/work/10-next-plans/reviews/2026-06-29-01-makeitsimpletodeploy-claude-review.md`

Please re-review the updated plan independently. Use the prior review to understand the previously
reported issues, but verify the current document and source context yourself rather than trusting a
summary.

Review focus:

- Confirm whether the prior BLOCKER/WARN findings are resolved in the current document.
- Look for any new contradictions introduced by the revision.
- Check the same deployment-plan risks as before: sequencing, trust/license bootstrap, readiness and
  false-green acceptance, embedder/env rendering, DB provisioning/migration ownership, thin-client
  setup, demo socket behavior, smoke tests, and release packaging.

Constraints:

- Treat this as a document/plan review, not a code review.
- Preserve the work/09 trusted-LAN/Tailscale boundary and versioned JSONL site protocol.
- Do not ask for Kubernetes, Helm, Docker Compose, HTTP/gRPC, internet exposure, or new client auth
  unless you are flagging a contradiction.
- Do not edit files.

Validation already run after the update:

- ASCII scan passed: `LC_ALL=C rg -n '[^ -~]' work/10-next-plans/01-makeitsimpletodeploy.md` returned no
  matches.
- No code/tests were run because this remains a plan-only artifact.

Required output structure:

1. Findings first, ordered by severity. Use `BLOCKER`, `WARN`, or `NIT`, and include file/section
   references.
2. Open questions or residual risks.
3. Verification notes: what you inspected.
4. Final line exactly one of:
   - `VERDICT: GO`
   - `VERDICT: FIXES_REQUIRED`
