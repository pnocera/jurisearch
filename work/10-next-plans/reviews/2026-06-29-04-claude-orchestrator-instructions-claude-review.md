# Claude review — `04-claude-orchestrator-instructions.md`

Reviewed artifact:
`work/10-next-plans/04-claude-orchestrator-instructions.md` (356 lines)

Context read in full:
`00-macro-implementation-plan.md`, `01-makeitsimpletodeploy.md`,
`02-auto-update-server-crons.md`.

## Summary

The instruction file is strong on intent and faithful to the macro plan. It explicitly makes
Claude the orchestrator, forbids direct implementation, mandates a Codex review gate after every
agent task, requires iteration until `VERDICT: GO`, and commits/pushes only after GO. The resolved
decisions from `00` are preserved well (all ten resolved decisions plus the external-PG, single-lock,
and archive-vs-package-cursor invariants are carried as non-negotiable constraints). A dependency
graph and parallelizable/non-parallelizable groups are present.

The problems are operational, not directional. The biggest is that the parallel-execution model
(isolated worktrees + agents-never-commit + orchestrator-owns-commits) is internally contradictory and
would strand agent work. The Codex review *mechanism* is also left implicit, which is risky for a
genuinely fresh-context session. Findings below are ordered by severity.

---

## Findings

### BLOCKER 1 — Worktree + "agents never commit" + "orchestrator owns commits" is mechanically contradictory for parallel work

- Where: lines 24 (`assign it to an agent in an isolated branch or worktree`), 43 (`commit only the
  reviewed logical change`), 45 (`synchronize the main workspace and any active agent worktrees`),
  47–49 (`the orchestrator owns final commits and pushes`), and 302–308 (fix agents likewise do not
  commit).
- Why it matters: A git worktree is a separate working directory on a separate branch. If a parallel
  task agent edits files inside worktree `Y` and — per the instruction — does **not** commit, then the
  orchestrator working in the main checkout sees nothing: `git status --short` (line 318) in the main
  tree does not show another worktree's uncommitted changes. There is no branch to merge (the agent
  didn't commit) and no staged change in the main tree to commit. The reviewed work is effectively
  stranded, or the orchestrator silently commits the wrong (empty/partial) set. This directly defeats
  the user's explicit requirement to run parallelizable work, and risks lost work.
  - Note the serial path is fine: Claude Code subagents run in the *same* working directory unless
    `isolation: worktree` is set, so non-parallel tasks land changes in the main tree and commit
    cleanly. The contradiction is specific to the parallel/worktree path the file recommends.
- Recommended fix: Pick one coherent protocol and spell it out. Either (a) parallel agents commit to
  their own worktree branch, and after Codex `GO` the orchestrator reviews that branch's diff and
  fast-forwards/cherry-picks/merges it into the integration branch (this reconciles "agents in
  worktrees" with "orchestrator owns the integration history"); or (b) keep agents non-committing and
  run them serially in the shared tree, reserving worktrees only for the explicit case where two agents
  must edit disjoint files concurrently — and then define exactly how the orchestrator collects each
  worktree's changes (e.g. agent commits on its branch; orchestrator merges post-GO). As written, the
  three rules cannot all hold simultaneously for parallel work.

### BLOCKER 2 — Codex review mechanism and its file/DONE protocol are unspecified

- Where: line 33 (`Ask Codex for a review`), 189/204/215/226/238/249/262 (`Codex review gate`),
  267–310 (prompt templates), and the whole loop assumes Codex "returns" a verdict inline.
- Why it matters: A fresh-context Claude Code session is told to "ask Codex" but never told *how*.
  In this project Codex review is driven by the `codex-review` skill, which runs Codex in a separate
  interactive session that **writes its review to a file and replies `DONE`** — it does not hand the
  verdict back inline the way the loop implies. A session that does not know this will either invent a
  nonexistent `codex` CLI invocation, block, or misread the protocol (waiting for an inline verdict
  that never comes, or never reading the review file to extract `VERDICT:`). This is exactly the
  "getting blocked by underspecified Codex review workflow" risk called out in the review request.
- Recommended fix: State the concrete mechanism: invoke the `codex-review` skill (one review per
  completed task), wait for the `DONE` reply, then read the review file and parse the final
  `VERDICT: GO` / `VERDICT: FIXES_REQUIRED` line. Name where review artifacts land and how the
  orchestrator locates them. If the skill is not guaranteed present in the fresh session, say so and
  give the fallback.

### WARN 1 — Codex re-review does not account for Codex's lost context between sessions

- Where: lines 302–310 (fix-agent prompt, then "ask Codex for a re-review").
- Why it matters: Each `codex-review` run is a fresh Codex session with no memory of its prior
  `FIXES_REQUIRED` findings. "Ask Codex for a re-review" without re-supplying the full template plus an
  explicit "here are your previous findings and how each was addressed" block invites Codex to re-derive
  findings from scratch, miss whether its earlier blockers were actually fixed, or oscillate — which
  also makes the ">10 times" stop condition (line 351) more likely to trip on churn rather than real
  disagreement.
- Recommended fix: For re-reviews, re-send the full review template (lines 271–300) and append the
  prior review's findings list plus the fix agent's per-finding resolution, so Codex verifies specific
  prior blockers rather than starting blind.

### WARN 2 — Branch/push target is unspecified; default is `main`

- Where: lines 43–49 and 314–328 ("Push immediately", "pushed branch") — no branch strategy stated.
- Why it matters: The repo's current branch is `main` (the default branch). With no instruction to
  branch first, the orchestrator will commit and push directly to `main` after every Codex GO — many
  small commits straight to the default branch. That may be intended, but it is left to chance, and it
  conflicts with the common "branch before committing on the default branch" convention. A fresh
  session has no way to know which is wanted.
- Recommended fix: State the branch policy explicitly — e.g. "create and work on an integration branch
  `feat/deploy-update-automation`, push there after each GO, open a PR at the end," or, if direct-to-main
  is intended, say so and confirm pushing to `main` is acceptable.

### WARN 3 — Commit hygiene against a working tree full of untracked review noise is under-guarded

- Where: lines 51–56 (review-artifact policy), 318–319 (`git status --short`; "Do not sweep unrelated
  dirty files").
- Why it matters: The working tree already contains many untracked review artifacts
  (`work/10-next-plans/reviews/*.meta`, `*.pane.txt`, `*.driver-prompt.md`, `*.done`, prompt/review
  `.md` files — confirmed via `git status --short`). The policy correctly says not to commit these, but
  it never forbids the mechanism that would sweep them in: `git add -A` / `git add .` / `git commit -a`.
  A session that reaches for `git add -A` will pull this noise (and unrelated tracked edits like the
  already-modified `01-…md` and `02-…md`) into a commit despite the policy text.
- Recommended fix: Add an explicit rule: stage commits with exact pathspecs only; never `git add -A`,
  `git add .`, or `git commit -a`. Consider recommending the agent verify the staged set with
  `git diff --cached --stat` before committing.

### WARN 4 — Several critical acceptance gates are not in the "carry through every agent prompt" list

- Where: lines 60–96 (non-negotiable constraints carried into every agent prompt) vs. the M2/M3
  acceptance gates in `00`/`02` (no-partial-publish; empty-outbox still refreshes the signed manifest;
  archive selection by `ArchiveTimestamp` **not** `change_seq` — the explicit "BLOCKER-2 trap";
  publish-exactly-once idempotence; single `core` lock spans ingest→enrich→embed→publish).
- Why it matters: The carried-constraints list captures durable invariants but omits the trickiest
  *behavioral* gates that are easiest to get wrong and hardest for Codex to catch without being pointed
  at them. The loop does say to give each agent "the relevant plan sections" (line 26), which mitigates
  this — but only if the orchestrator reliably extracts the right gates. For the producer vertical slice
  these gates are the entire point of the milestone.
- Recommended fix: In the Task 2 and Task 3 contracts, name the specific acceptance gates the agent and
  Codex must satisfy (no partial publish; empty-outbox→manifest-refresh→exit-zero; archive-timestamp
  selection regression test; one-lock-across-all-DB-mutation), rather than relying solely on "relevant
  plan sections."

### WARN 5 — Step 7 "synchronize worktrees" is underspecified and compounds BLOCKER 1

- Where: line 45 ("synchronize the main workspace and any active agent worktrees before starting
  dependent tasks").
- Why it matters: After the orchestrator commits/pushes one task, sibling worktrees are now behind the
  integration branch. "Synchronize" has no defined mechanism (pull? rebase each worktree? recreate?).
  Combined with BLOCKER 1's unclear change-collection model, a fresh session has no deterministic way to
  keep parallel worktrees consistent, raising the risk of double-applying or dropping changes.
- Recommended fix: Specify the sync operation (e.g. "after each push, `git -C <worktree> rebase
  <integration-branch>` for every active worktree, resolving conflicts via an agent"), and tie it to the
  commit protocol chosen in BLOCKER 1.

### WARN 6 — M1-B / M1-C are declared parallel but may collide on storage/migration/embedding code

- Where: lines 157–160 ("`M1-B` … can run in parallel with `M1-A`, `M1-C`"; "`M1-C` … high-risk and
  must not collide with the producer update agent"), 192–204 (Task 1 splits three parallel agents).
- Why it matters: M1-C (library extraction of ingest/enrich/embed/package entrypoints) and M1-B
  (connection-based external-PG migration/provisioning) both plausibly touch the storage/migration and
  embedding surfaces. The file flags M1-C's collision risk only against the *producer update* agent
  (Task 2), not against M1-B running concurrently. The general guard at lines 170–171 forbids two agents
  editing the same crate API/migration path, which would contradict greenlighting M1-B∥M1-C if they
  overlap.
- Recommended fix: Make the Task 0 survey explicitly resolve M1-B/M1-C file/API ownership *before*
  launching them in parallel, or sequence M1-C ahead of M1-B if the survey finds overlap.

### NIT 1 — M7 manual `rebaseline` repair command has no task contract

- Where: task contracts cover Task 0–6 (lines 177–263); `00` M7 / `02` Phase 5 keep a manual
  `jurisearch-producer rebaseline --source <src>` as a v1 operator repair command.
- Why it matters: Automatic rebaseline is correctly placed in Task 3, but the manual repair affordance
  (still a v1 deliverable) is not assigned anywhere, so it could be silently dropped.
- Recommended fix: Add the manual repair command to the Task 3 contract (it shares the integrity/order/
  convergence checks with auto rebaseline), or note it as deferred-with-stub.

### NIT 2 — Survey/doc-only tasks vs. the commit gate

- Where: Task 0 (lines 177–190) has a Codex review gate but produces an execution map, not a code diff.
- Why it matters: The commit/push policy is framed around "reviewed logical change" (code). It's mildly
  unclear whether the survey output is committed (the "Document …" pattern at line 337 covers it) or
  kept ephemeral. Harmless but worth one sentence.
- Recommended fix: State that survey/analysis outputs are either kept untracked as working notes or
  committed via the `Document <…>` pattern after GO.

### NIT 3 — ">10 FIXES_REQUIRED" stop threshold is generous

- Where: line 351.
- Why it matters: Ten review/fix cycles on one architectural issue is a large amount of agent + Codex
  spend before surfacing to the user. Reasonable as a backstop, but worth lowering for a tighter
  human-in-the-loop on genuine disagreement.
- Recommended fix: Consider 3–4 iterations on the *same* issue before stopping to ask the user, while
  keeping a higher global cap.

### NIT 4 — Live-infra/credentialed acceptance tasks (bear, OpenRouter, PISTE) need an explicit "defer/fixture" steer

- Where: Final node `L` (operated acceptance + docs, line 125), Task 5 operated-bear acceptance
  (line 249), producer embedding via OpenRouter.
- Why it matters: Operated acceptance needs live bear SSH/credentials and paid embedding; the stop
  conditions (lines 350–353) cover this, but the orchestrator isn't told up front that these legs
  generally can't run from a fresh local session and should default to fixtures, deferring operated
  legs.
- Recommended fix: Add a short note that CI/fixture paths are the default and operated/credentialed legs
  are gated by the stop conditions and likely deferred unless the user provisions access.

---

## Open questions / risks

1. Is direct-to-`main` pushing actually intended, or should the run use an integration branch + PR?
   (WARN 2.)
2. Is the `codex-review` skill guaranteed available in the fresh session, and is its file/`DONE`
   protocol the intended mechanism? (BLOCKER 2.) The whole loop hinges on this.
3. For parallel tasks, what is the precise change-collection protocol — agent commits on a branch, or
   orchestrator stages from a worktree? (BLOCKER 1 / WARN 5.) Until resolved, parallel execution is the
   riskiest part of these instructions.
4. Does the orchestrator have the right to merge `jurisearch-producer` changes that depend on heavy
   library extraction without a human gate at the M1-C boundary, given the plans flag it as the
   highest-risk refactor?

## Verification notes

- `git diff --check -- work/10-next-plans/04-claude-orchestrator-instructions.md` → clean
  (`CHECK_OK`), matching the prompt's claim.
- Confirmed the working tree carries numerous untracked review artifacts under
  `work/10-next-plans/reviews/` (`.meta`, `.pane.txt`, `.driver-prompt.md`, prompt/review `.md`), plus
  pre-existing tracked edits to `01-…md` and `02-…md` — the noise that WARN 3 guards against.
- Cross-checked all ten "Resolved decisions" in `00` against the non-negotiable constraints block in
  `04` (lines 60–96): all present, plus external-PG, single-`core`-lock, archive-cursor≠package-cursor,
  and the confidentiality embedding split. Resolved-decision preservation is the file's strongest area.
- Verified the dependency graph edges against `00`'s milestone "Maps to"/"Builds on" statements: the
  C0→{A,B,C,D}, B→E, C→E, D→E, E→F, G→H→J, F→K→L structure is consistent with the plans; no inverted or
  missing hard dependency found beyond the M1-B/M1-C parallel-overlap caveat (WARN 6).
- Verdict-string contract (`VERDICT: GO` / `VERDICT: FIXES_REQUIRED`) is consistent across the file and
  matches the `codex-review` skill's GO/FIXES_REQUIRED convention.

---

VERDICT: FIXES_REQUIRED
