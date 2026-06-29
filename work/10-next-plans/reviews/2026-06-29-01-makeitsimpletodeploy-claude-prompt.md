# Claude review prompt: make JuriSearch simple to deploy

Repo/workdir: `/home/pierre/Work/jurisearch`

Artifact to review:

- `/home/pierre/Work/jurisearch/work/10-next-plans/01-makeitsimpletodeploy.md`

User intent:

- Review the new implementation-plan document for making JuriSearch simple to deploy.
- The plan should build on the existing work/09 site-server + thin-client runtime rather than
  redesigning it.
- The intended result is an implementable deployment roadmap: one config file, generated systemd/env
  files, host doctor, DB provisioning, service install, trust/catch-up/readiness bootstrap, embedder
  checks, thin-client configuration, smoke tests, and release/upgrade handling.

Important existing context to verify against source/artifacts where relevant:

- `deploy/systemd/jurisearch-site.service`
- `deploy/systemd/jurisearch-syncd.service`
- `deploy/systemd/jurisearch-bge-m3.service`
- `work/09-jurisearch-cli/01-target-deployment-analysis.md`
- `work/09-jurisearch-cli/03-deployment-design.md`
- `work/09-jurisearch-cli/04-implementation-plan.md`
- `work/09-jurisearch-cli/05-two-host-acceptance.md`
- `work/09-jurisearch-cli/scripts/single-host-acceptance.sh`
- Current CLI surfaces in `crates/jurisearch-cli/src/args.rs`,
  `crates/jurisearch-cli/src/site/serve.rs`, `crates/jurisearch-syncd/src/main.rs`, and
  `crates/jurisearch-client/src/main.rs`.

Non-negotiable constraints:

- Do not ask for Kubernetes, Helm, Docker Compose, HTTP/gRPC, internet exposure, or new client auth in
  this phase unless you are flagging a contradiction in the document.
- Preserve the work/09 trusted-LAN/Tailscale boundary and versioned JSONL site protocol.
- Treat this as a document/plan review, not a code review.
- Do not edit files.

Validation already run:

- The document was created and read back.
- ASCII scan passed: `LC_ALL=C rg -n '[^ -~]' work/10-next-plans/01-makeitsimpletodeploy.md` returned no matches.
- No code/tests were run because this is a plan-only artifact.

Review focus:

- Find blocker-level gaps that would make the plan misleading, unsafe, unimplementable, or inconsistent
  with the current repo.
- Find warn-level sequencing, acceptance, operator UX, security, privilege, or packaging issues.
- Look for false-green acceptance holes, especially where a command could report success without a real
  ready corpus/site service/thin-client query.
- Check whether the proposed `jurisearchctl` surface is coherent with the existing binaries and flags.
- Every finding should include a concrete recommended fix to the document.

Required output structure:

1. Findings first, ordered by severity. Use `BLOCKER`, `WARN`, or `NIT`, and include file/section
   references.
2. Open questions or residual risks.
3. Verification notes: what you inspected.
4. Final line exactly one of:
   - `VERDICT: GO`
   - `VERDICT: FIXES_REQUIRED`
