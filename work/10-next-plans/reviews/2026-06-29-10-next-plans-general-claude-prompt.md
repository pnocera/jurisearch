# Claude Review Request: `work/10-next-plans` General Plan Review

## Workdir

`/home/pierre/Work/jurisearch`

## Primary artifacts to review

- `/home/pierre/Work/jurisearch/work/10-next-plans/01-makeitsimpletodeploy.md`
- `/home/pierre/Work/jurisearch/work/10-next-plans/02-auto-update-server-crons.md`

The directory also contains older review artifacts under
`/home/pierre/Work/jurisearch/work/10-next-plans/reviews/`. Treat those as context only if useful; do
not review them as primary plans.

## User intent

Perform a general cross-plan review. The user wants to know whether these plans are coherent,
implementable, and sufficiently precise for the next engineering work. Focus on contradictions,
missing implementation decisions, false assumptions, sequencing risks, and places where an implementer
could build the wrong thing.

## Non-negotiable constraints / current decisions

- Site/customer query embedding is local-only for confidentiality. The site service must use the local
  bge-m3 service; customer query text must not go to OpenRouter or any external embedding provider.
- Producer/update-ingest document embedding is different: it processes public legal-source text and may
  use a fast external OpenAI-compatible provider such as OpenRouter.
- `jurisearch-syncd` on site hosts applies already-embedded signed packages; it must not call
  OpenRouter or any other embedding API during catch-up.
- There must be a root-level release script, `./dist.sh`, that writes a local `./dist/` with distinct
  `update-server`, `site-server`, and `cli` bundles. Huge/runtime assets such as database contents,
  corpus packages, vector indexes, model weights, and tokenizer files must not be bundled.
- The current bear infra snapshot is deliberately a bootstrap state, not the hardened product
  contract. Current bootstrap credentials are intentionally recorded as `root / 20Sense20` and
  `postgres / postgres`; do not block solely because those are insecure, but do flag any place where
  the plan accidentally normalizes them as production design rather than bootstrap state.
- Update-server CT 111 should stay lightweight and download legal-source archives / publish artifacts
  to Storebox. Heavy PostgreSQL work runs in the JuriSearch DB guest CT 110.
- The implementation should remain compatible with the existing package/trust/query protocol unless a
  plan explicitly calls out a larger prerequisite.

## Validation already run

- `rg -n "[[:blank:]]$" work/10-next-plans/01-makeitsimpletodeploy.md work/10-next-plans/02-auto-update-server-crons.md`
  returned no trailing-whitespace findings.
- No code tests are applicable; these are plan documents.

## Review instructions

Review the two primary artifacts directly. Verify claims against the docs and, where necessary, against
the repository source. Do not rely on this prompt's summaries if the artifact text says something else.

Please prioritize findings that would materially affect implementation:

- contradictions between the two plans;
- unclear ownership between site-server, update-server/producer, syncd, and CLI;
- missing prerequisites or open decisions that block implementation;
- source-code mismatches where the plan assumes a capability that does not exist;
- sequencing errors that would cause a half-built or misleading deployment path;
- security/confidentiality boundary mistakes;
- release asset and Storebox/current-infra gaps.

Every finding should include:

- severity: `BLOCKER`, `WARN`, or `NIT`;
- exact file/line reference where possible;
- why it matters;
- a concrete recommended fix.

Preferred output structure:

1. Findings first, ordered by severity.
2. Open questions / residual risks.
3. Verification notes.
4. Final verdict line, exactly one of:
   - `VERDICT: GO`
   - `VERDICT: FIXES_REQUIRED`

Use `VERDICT: FIXES_REQUIRED` if the plans still contain a blocker or a serious ambiguity that should
be fixed before implementation starts. Use `VERDICT: GO` if the remaining issues are narrow
improvements or explicitly acceptable follow-up decisions.
