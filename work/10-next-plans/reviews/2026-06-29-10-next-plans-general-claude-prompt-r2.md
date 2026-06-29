# Claude re-review prompt

Repo/workdir: `/home/pierre/Work/jurisearch`

Updated artifact to re-review:

- `/home/pierre/Work/jurisearch/work/10-next-plans/01-makeitsimpletodeploy.md`
- `/home/pierre/Work/jurisearch/work/10-next-plans/02-auto-update-server-crons.md`

Prior review(s):

- `/home/pierre/Work/jurisearch/work/10-next-plans/reviews/2026-06-29-10-next-plans-general-claude-review.md`

Please re-review the updated artifact independently. Use the prior review(s) to understand the
previously reported issues, but verify the current artifact and relevant source/context yourself
rather than trusting a summary.

Review focus:

- Verify that the producer DB topology contradiction is resolved in favor of CT 111 orchestrating against external PostgreSQL on CT 110.
- Verify that producer OpenRouter request_model is separated from the storage fingerprint model_name and that site query embedding is loopback-only.
- Verify that dist/update-server Phase 2 references and cross-plan sequencing are unambiguous.

Constraints:

- Treat this as an artifact review; do not call it a code review unless the artifact is code.
- Do not edit files.
- Every finding should include a concrete recommended fix.

Validation already run after the update:

- rg -n '[[:blank:]]$' work/10-next-plans/01-makeitsimpletodeploy.md work/10-next-plans/02-auto-update-server-crons.md returned no findings
- Targeted rg scan found no remaining recommendation for v1 local ManagedPostgres/index_dir; remaining ManagedPostgres mentions describe current code limitations or no-fallback tests.

Required output structure:

1. Findings first, ordered by severity. Use `BLOCKER`, `WARN`, or `NIT`, and include file/section
   references.
2. Open questions or residual risks.
3. Verification notes: what you inspected.
4. Final line exactly one of:
   - `VERDICT: GO`
   - `VERDICT: FIXES_REQUIRED`
