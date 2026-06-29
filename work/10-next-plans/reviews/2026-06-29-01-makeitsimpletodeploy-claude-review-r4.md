# Re-review (r4): `work/10-next-plans/01-makeitsimpletodeploy.md`

Reviewer: Claude (Opus 4.8)
Date: 2026-06-29
Artifact: implementation-plan document "Make JuriSearch simple to deploy" (revision r4)
Scope: document/plan review (not a code review). Re-verified independently against the work/09 runtime,
the checked-in systemd units, and the embed/cli/client source, rather than trusting the r3 summary.

## Summary

The r4 revision resolves every item the prompt named as a focus area, and it does so accurately. I
re-checked the load-bearing claim (storage fingerprint vs pooling) against source and the plan's prose
now matches the code. The remaining two items I found are precision nits, neither of which causes a
false-green or a contradiction. This is a GO.

Focus-item disposition:

- **r3 WARN-1 (storage fingerprint vs pooling): resolved, verified against source.** The "Generated
  runtime files" prose (lines 232-234) now reads "The effective storage fingerprint is derived by the
  embedder from model, dimension, and normalization only; pooling configures the endpoint but is not
  part of the storage-fingerprint comparison." That matches `EmbeddingFingerprint::storage_embedding_
  fingerprint()` exactly (`crates/jurisearch-embed/src/fingerprint.rs:17-22` →
  `"{model}:{dimension}:normalize:{normalize}"`, pooling absent). The three sub-defects of r3 WARN-1
  are all addressed:
  1. *Prose corrected* — pooling is no longer attributed to the storage fingerprint (lines 232-234;
     Phase 6 lines 433-435).
  2. *False-green closed* — pooling is now an explicit deploy-time validation rule
     (`embedder.pooling` must be `cls`, Phase 1 lines 266-267; Phase 6 lines 433-435: "pooling is a
     deploy-time validation rule, not a fingerprint guard"), so the plan no longer relies on a
     fingerprint comparison that structurally cannot detect a pooling mismatch.
  3. *Rendering gap closed* — the plan now threads pooling into the generated unit's CLI flag, not a
     phantom env var: "the generated bge-m3 unit renders the same value into the `llama-server
     --pooling` flag" (lines 230-231) and the golden invariant pins "the generated `llama-server
     --pooling` flag" (lines 274-275). This matches the checked-in
     `deploy/systemd/jurisearch-bge-m3.service`, where `--pooling cls` is an `ExecStart` flag and the
     env file carries only `JURISEARCH_BGE_M3_MODEL`/`_PORT`.

- **r3 NIT-1 (pre-bootstrap doctor behavior): resolved.** Phase 2 now has an explicit invariant
  (lines 310-312): "Pre-bootstrap doctor can exit zero with advisory 'not yet bootstrapped' statuses
  for absent trust rows and active corpora when config/package inputs are valid; post-bootstrap
  readiness and smoke remain the hard serving gates." The "Done when" (lines 315-317) now distinguishes
  fresh / DB-provisioned-but-pre-bootstrap / fully-bootstrapped, which removes the r3 ambiguity about
  the happy-path step-3 placement.

- **r3 NIT-2 (temp embedder endpoints colliding with the managed unit's port): resolved.** Phase 2
  (lines 313-314) and Phase 6 (line 440) now state that any doctor-started endpoint is stopped before
  exit, or skipped when the managed unit is already active, "so the subsequent systemd service can bind
  the configured port." This covers both `site doctor` (step 3) and `embed doctor` (step 8).

- **r3 NIT-3 (serving gate named only `site readiness`): resolved at the governing rule.** The headline
  serving rule (lines 42-43) now reads "until `site readiness` exits zero against an active,
  readiness-stamped corpus and, for embedder-configured sites, `embed doctor` exits zero." The happy
  path honors this (embed doctor at step 8, line 33, before `systemctl enable --now jurisearch-site` at
  step 9, line 35). See NIT-1 below for a residual Phase 4 wording gap.

Open-question disposition (carried from r3):

- **Admin/bootstrap auth mechanism: resolved.** Lines 203-204 now state the privileged connection uses
  "peer/ident, `.pgpass`, a systemd credential, or the optional `database.admin_password_file`; the
  password itself is never stored inline in `site.toml`," with the schema field present (line 161) and
  the Phase 1 permission rule on `*_password_file` (lines 259-260).
- **Demo coverage: resolved.** The demo example now runs `demo smoke ... --fetch-id` (line 66), and the
  prose requires that "`demo up` must apply a small fixture corpus or a configured package root so
  `demo smoke` can run real status/fetch/search legs; a status-only demo is not sufficient product
  proof" (lines 74-76).
- **Pooling has no fingerprint guard on either side (work/09 property): acknowledged.** Phase 6
  (lines 433-435) states plainly that "because the storage fingerprint does not encode pooling, pooling
  is a deploy-time validation rule, not a fingerprint guard." The plan no longer over-promises
  fingerprint-based embedder/corpus equivalence for pooling.

---

## 1. Findings (ordered by severity)

No BLOCKER. No WARN. Two NITs.

### NIT-1 — Phase 4's install-start gate names only `site readiness`; the headline rule also gates on `embed doctor` for embedder-configured sites

Sections: headline serving rule lines 42-43 ("until `site readiness` exits zero ... and, for
embedder-configured sites, `embed doctor` exits zero") vs Phase 4 install behavior lines 370-373
("starting `jurisearch-site` must be refused until `site readiness` exits zero unless the operator
passes an explicit force flag").

The r4 revision added `embed doctor` to the governing serving rule (resolving r3 NIT-3) but did not
restate it in Phase 4's description of what `site install` (run without `--no-start`) refuses. As
written, on an embedder-configured site an operator who runs `site install` without `--no-start` could
have `jurisearch-site` started once `site readiness` passes, even if `embed doctor` would fail —
contradicting lines 42-43. The happy path avoids this because it installs with `--no-start` (line 29),
then runs `embed doctor` (line 33) before enabling the site (line 35), and Phase 8 smoke has a hard
hybrid leg as a backstop. Impact is therefore a possible bad-embedder *false start* on a non-happy
path, not a deployment false-green. Fix is one clause in Phase 4: for embedder-configured sites, the
install-start refusal also requires `embed doctor` zero (mirroring lines 42-43), or an explicit force
flag.

### NIT-2 — "non-`cls` packages are rejected by explicit deploy validation" overstates what deploy validation can see

Section: "Generated runtime files" lines 233-234 ("In this phase `pooling` is fixed to `cls`; non-`cls`
packages are rejected by explicit deploy validation rather than inferred from the storage
fingerprint.").

By the plan's own (now-correct) statement, pooling is not encoded in the storage fingerprint, and
`corpus_state.embedding_fingerprint` is the storage form — so deploy validation has no package-side
field from which to detect that a *package* was embedded with non-`cls` pooling. What deploy validation
actually rejects is a non-`cls` `embedder.pooling` *configuration* (Phase 1 lines 266-267; Phase 6
lines 433-435 both phrase this correctly). The "rather than inferred from the storage fingerprint"
clause already concedes the fingerprint cannot carry pooling, which makes "non-`cls` packages are
rejected" internally loose. Recommend rewording to "non-`cls` *configurations* are rejected" (or
"deployments"), so this sentence matches the precise wording in Phase 1 and Phase 6. Purely a prose
precision item; no behavioral consequence.

---

## 2. Open questions / residual risks

- **`demo smoke --fetch-id <demo-id>` provenance.** Since `demo up` applies a fixture corpus or a
  configured package root (lines 74-76), the `<demo-id>` the demo smoke fetches must come from that
  fixture. The plan would be tighter if it noted that the bundled fixture exposes a documented known id
  for `demo smoke`. This is the same operator-supplies-a-known-id pattern as the production smoke
  (line 36, `--fetch-id '<known-id>'`), so it is a documentation nicety, not a gap.
- **Stale code comment, not a plan defect (out of scope).** `crates/jurisearch-query/src/embedder.rs:13`
  comments the query fingerprint as "model:dim:pooling:normalize," but the concrete embedder returns
  the storage fingerprint (`crates/jurisearch-cli/src/embedding_runtime/mod.rs:48`,
  `storage_fingerprint`), i.e. model:dimension:normalize with no pooling — so the plan's r4 prose is now
  *more* accurate than that comment. Flagging only so a future implementer who reads the comment is not
  misled into thinking the query path already guards pooling; the plan itself is correct. (No edit made;
  this is a code observation, outside the plan-review scope.)
- **Endpoint-vs-config pooling check is not a corpus guard.** `crates/jurisearch-cli/src/embedding_
  runtime/pool.rs:106` compares `endpoint_fingerprint.pooling != expected_fingerprint.pooling`, but both
  sides derive from the same embedder configuration, so it enforces pool self-consistency, not
  agreement with what the corpus was embedded under. This corroborates the plan's choice to treat
  pooling as a deploy-time config rule; no action needed.

## 3. Verification notes (what I inspected)

- The artifact in full: `work/10-next-plans/01-makeitsimpletodeploy.md` (r4).
- Prior review for issue context: `.../2026-06-29-01-makeitsimpletodeploy-claude-review-r3.md` (and the
  r3 review's references to r1/r2). Verified the current document text directly rather than trusting the
  summaries.
- Storage fingerprint (r3 WARN-1): `crates/jurisearch-embed/src/fingerprint.rs:15-22`
  (`storage_embedding_fingerprint()` = `"{model}:{dimension}:normalize:{normalize}"`, pooling absent);
  `crates/jurisearch-embed/src/config.rs:131-150` (full `fingerprint()` includes provider, base_url
  class, model, dimension, normalize, pooling; `storage_embedding_fingerprint()` delegates to the
  storage form); `crates/jurisearch-cli/src/embedding_runtime/mod.rs:41-49` (`PreparedQueryEmbedder::
  embed` returns `self.storage_fingerprint`); `crates/jurisearch-query/src/builders.rs:147-160` and
  `crates/jurisearch-query/src/search.rs:181-197` (the query-time dense-compatibility key passed to
  storage is `QueryEmbedding.fingerprint`, i.e. the storage form). Together these confirm pooling is not
  part of the query-time/storage comparison, so the r4 prose is accurate.
- Pooling rendering target (r3 WARN-1 sub-point 3): `deploy/systemd/jurisearch-bge-m3.service`
  (`--pooling cls` is an `ExecStart` flag; `EnvironmentFile` carries `JURISEARCH_BGE_M3_MODEL`/`_PORT`
  only) and `crates/jurisearch-cli/src/embedding_runtime/config.rs:284-285`
  (`JURISEARCH_EMBED_POOLING` sets the serve-site-side `embedding_config.pooling`). Confirms the plan's
  split: env var for serve-site, generated `--pooling` flag for the bge-m3 unit.
- Thin client / demo `--local` claim: `crates/jurisearch-client/src/lib.rs:22-23`
  (`LOCAL_SOCKET_NAME = "jurisearch-site.sock"`), `:117-141` (`resolve_endpoint` order: `--local` under
  `$XDG_RUNTIME_DIR` → `--server` → `JURISEARCH_SITE_URL`), `crates/jurisearch-client/src/main.rs:26`
  (`--local` doc = `unix://$XDG_RUNTIME_DIR/jurisearch-site.sock`). Confirms the demo prose (lines
  73-74) and the Phase 7 resolution-precedence claim (client config strictly below the env var,
  lines 456-457) are accurate.
- Mechanical: relied on the prompt's stated ASCII/whitespace validation (ASCII scan and
  `git diff --check` reported clean for the r4 update); I did not re-run them. The plan file is
  untracked (`work/10-next-plans/` shows `??`), so no git diff between r3 and r4 was available; I
  compared the current text against the r3 review's cited line claims instead.

I did not run code or tests (plan-only artifact). I did not edit any files.

Constraint check: the revision still honors the non-negotiables — no Kubernetes/Helm/Compose/HTTP/gRPC,
no internet exposure, no new client auth; the work/09 trusted-LAN/Tailscale boundary and the versioned
JSONL site protocol are preserved (Non-goals lines 81-90; bind/`allow_lan` rules lines 252-254). Both
NITs are precision items within those constraints, not violations.

## 4. Disposition of prior (r3) findings

- r3 WARN-1 (storage fingerprint mis-attributes pooling; non-default pooling rendering unspecified):
  **resolved** — prose corrected to model/dimension/normalize only; pooling made an explicit deploy
  validation rule; rendering threads pooling into the generated `--pooling` flag (verified against
  source and the checked-in unit).
- r3 NIT-1 (pre-bootstrap doctor exit behavior ambiguous): **resolved** — Phase 2 advisory-status
  invariant + tri-state "Done when" (lines 310-312, 315-317).
- r3 NIT-2 (doctor temp endpoint vs managed unit port): **resolved** — Phase 2 lines 313-314, Phase 6
  line 440.
- r3 NIT-3 (serving gate omitted `embed doctor`): **resolved at the governing rule** (lines 42-43);
  residual Phase 4 wording gap raised as NIT-1 above.
- r3 open questions: admin/bootstrap auth mechanism **resolved** (lines 203-204); demo coverage
  **resolved** (lines 66, 74-76); pooling-has-no-fingerprint-guard **acknowledged** (Phase 6
  lines 433-435).

No new contradictions were introduced by the revision beyond the two precision NITs above, neither of
which blocks the plan.

VERDICT: GO
