# Claude re-review — `04-claude-orchestrator-instructions.md`

Reviewed artifact:
`work/10-next-plans/04-claude-orchestrator-instructions.md` (448 lines)

Prior review:
`work/10-next-plans/reviews/2026-06-29-04-claude-orchestrator-instructions-claude-review.md`
(verdict `FIXES_REQUIRED`: 2 BLOCKER, 6 WARN, 4 NIT).

Context re-read: `00-macro-implementation-plan.md` in full;
`01-makeitsimpletodeploy.md` and `02-auto-update-server-crons.md` cross-referenced where relevant.

## Summary

The revision resolves **every** prior finding, and resolves them correctly rather than cosmetically.
The two blockers — the contradictory worktree/commit model and the unspecified Codex mechanism — are
now genuinely fixed: there is a coherent "agents edit, orchestrator commits inside the worktree, then
merges into the integration branch, then pushes" protocol, and a concrete `codex-review` skill +
file + `DONE` + `VERDICT:` parse loop. The branch default (`main`) is now explicit with an escape
hatch, `git add -A` is forbidden, re-reviews re-supply prior findings, the M2/M3 behavioral acceptance
gates are named in the task contracts, M1-B/M1-C ownership is deferred to Task 0, the manual rebaseline
command has a home in Task 3, and the stop threshold is now "3 consecutive."

The artifact is mechanically executable by a fresh session. The residual findings below are
refinements, not contradictions or stranded-work conditions, and do not block. They are worth folding
in because this file drives an unattended autonomous run where a fresh, literal reader is the exact
audience.

---

## Prior-finding disposition

- **BLOCKER 1 (worktree + agents-never-commit + orchestrator-owns-commits contradiction) — RESOLVED.**
  Lines 63 (agents edit only, never commit/push), 71–72 (orchestrator commits *inside the task worktree*
  using exact pathspecs), 72–74 (merge task branch into integration, prefer fast-forward, re-rebase +
  re-review if integration advanced), 75 (push the integration branch). This is internally consistent:
  agents never commit, the orchestrator owns history, and the worktree changes are collected by the
  orchestrator committing in-worktree then merging. The `--autostash` rebase (lines 64–67, 76–80)
  correctly handles the agent's *uncommitted* working changes at sync points.
- **BLOCKER 2 (Codex mechanism unspecified) — RESOLVED.** Lines 87–109 name Claude Code's
  `codex-review` skill, require a markdown artifact under `work/10-next-plans/reviews/`, wait for the
  `DONE` reply, then read the artifact and parse `VERDICT: GO` / `VERDICT: FIXES_REQUIRED`, and treat
  the review as incomplete until *both* `DONE` and a non-empty artifact exist (lines 93–96). The
  "do not invent a replacement CLI; stop and ask if the skill is unavailable" guard (lines 97–98) is
  exactly right for a fresh session.
- **WARN 1 (re-review loses Codex context) — RESOLVED.** Lines 394–401 require re-sending the full
  template plus the prior artifact path, prior findings, the fix agent's per-finding resolution, and
  post-fix validation, and ask Codex to verify prior blockers independently.
- **WARN 2 (branch/push target unspecified) — RESOLVED.** Lines 56–60 make the integration branch the
  one active at session start, state that direct-to-`main` is intended if `main` is active, and give a
  stop-and-ask escape hatch. This converts an implicit risk into an explicit, acknowledged default.
- **WARN 3 (commit hygiene vs. untracked noise) — RESOLVED.** Line 410 forbids `git add -A`,
  `git add .`, and `git commit -a`; lines 411–412 require pathspec staging and `git diff --cached`
  verification. The review-artifact policy (lines 102–109) keeps `.done`/pane/meta noise untracked.
- **WARN 4 (acceptance gates not carried) — RESOLVED.** Task 2 (lines 282–286) and Task 3
  (lines 302–306) now name the tricky behavioral gates explicitly: ArchiveTimestamp-not-`change_seq`
  selection, empty-outbox→manifest-refresh→exit-zero, exactly-once/idempotent publish with no partial
  exposure, distinct cursor systems, single `core` lock across ingest→enrich→embed→publish, and
  explicit/recorded rebaseline.
- **WARN 5 (synchronize worktrees underspecified) — RESOLVED.** Lines 76–80 give the concrete
  operation (`git -C <worktree> fetch origin` + `rebase --autostash <integration-branch>`), tie it to
  the commit model, and route conflicts through an agent with re-validation/re-review.
- **WARN 6 (M1-B / M1-C parallel collision) — RESOLVED.** Lines 218–220 gate M1-B∥M1-C on Task 0
  assigning non-overlapping ownership and require sequencing on overlap; Task 0 (lines 250–251) makes
  that decision an explicit deliverable.
- **NIT 1 (manual rebaseline unassigned) — RESOLVED.** Task 3 locking agent (line 300) and acceptance
  gate (line 306) now own `jurisearch-producer rebaseline --source <src>` with shared integrity/order
  checks, matching `00` M7 (lines 304–305, 315).
- **NIT 2 (survey/doc-only vs commit gate) — RESOLVED.** Lines 108–109 state survey outputs stay
  untracked or commit via the `Document <…>` pattern after GO.
- **NIT 3 (>10 threshold too generous) — RESOLVED.** Line 444 is now "3 consecutive times for the same
  architectural issue."
- **NIT 4 (live/credentialed legs need a defer steer) — RESOLVED.** Lines 155–156 make CI/fixture the
  default and gate operated-bear/OpenRouter/PISTE/paid legs behind explicit user authorization,
  otherwise defer and record residual risk.

---

## New findings (introduced or surfaced by the revision)

### WARN 1 — The Codex review diff (`git diff <integration-branch>`) omits newly-created files

- Where: lines 68–70 — "Include `git -C <task-worktree> diff <integration-branch> --stat` and the full
  relevant diff or exact changed files." Because agents do not commit (line 63), at review time their
  new files are *untracked*, and `git diff <ref>` does not show untracked files. The producer vertical
  slice (`jurisearch-producer`, new crates, new commands) is almost entirely new files, so a session
  that builds the review payload purely from `git diff` would hand Codex an incomplete picture and risk
  a false `GO` on unreviewed new code.
- Mitigation already present: the loop requires the agent to report "files changed" (line 30) and the
  Codex template carries an explicit `Files changed: <list>` block (lines 367–368) plus "the full
  relevant diff or exact changed files," so a careful orchestrator would include new files anyway. This
  is why it is a WARN, not a blocker.
- Recommended fix: before producing the review diff, stage intent-to-add so untracked files appear:
  `git -C <task-worktree> add -A --intent-to-add` (or `git add -N <paths>`), then
  `git -C <task-worktree> diff <integration-branch>` and `--stat` include new files. Alternatively
  state that the review payload must reconcile `git diff` against `git -C <task-worktree> status
  --short` so untracked additions are never silently dropped.

### WARN 2 — The "Commit and push policy" section is not reconciled with the worktree commit/merge/push sequence

- Where: lines 405–420 (generic policy: step 1 `git status --short`, step 6 "Push immediately") vs.
  lines 71–75 (worktree path: commit *inside the worktree*, merge into integration, push the
  *integration* branch). The policy section reads as a main-checkout flow and never says, for a
  worktree task, to run the status/stage/diff/commit steps with `git -C <task-worktree>` or that
  "push" means the integration branch after merge — not the task branch. A fresh session that jumps to
  the policy section for a parallel task could run `git status --short` in the main checkout (which
  shows none of the worktree's changes) or push the task branch instead of integration.
- Note: the worktree-protocol section itself (lines 71–75) is correct and complete, so this is a
  cross-reference/consistency gap rather than a contradiction — it does not strand work the way the
  prior BLOCKER 1 did.
- Recommended fix: in the Commit and push policy, add one line: "For a worktree task, run steps 1–5
  with `git -C <task-worktree>`, then follow the merge-and-push-integration sequence in the Branch and
  worktree protocol; 'push' always means the integration branch." Or explicitly mark the policy section
  as the serial/main-checkout case and point to lines 71–75 for the worktree case.

### NIT 1 — No cleanup of merged task branches/worktrees

- Where: lines 71–80. After merge + push, the `agent/<task-slug>` branch and its
  `../jurisearch-worktrees/<task-slug>` directory linger. Over a full M0→M6 run this accumulates stale
  worktrees and branches, and a stale worktree left on an old base is a foot-gun if reused.
- Recommended fix: add "after a task is merged and pushed, remove its worktree (`git worktree remove`)
  and delete the merged task branch," or explicitly say worktrees are retained intentionally.

### NIT 2 — `git fetch origin` then rebase onto the *local* integration branch

- Where: lines 64–67 and 76–80 fetch `origin` but rebase onto `<integration-branch>` (the local ref),
  not `origin/<integration-branch>`. For the intended single-orchestrator, single-machine flow this is
  correct — local integration is authoritative and is what gets pushed — but the `fetch` is then mostly
  redundant and the local-vs-remote distinction may confuse a fresh reader.
- Recommended fix: one clarifying sentence that the local integration branch is the source of truth and
  `fetch` is only to detect external divergence; or drop the redundant `fetch` for the single-machine
  case.

---

## Cross-plan consistency check

- Dependency graph (lines 166–211) vs. `00` milestones: the C0→{A,B,C,D}, B→E, C→E, D→E, E→F,
  G→H→J, {F,I}→J, {F,G,H,I,J}→K, K→L structure remains consistent with `00`'s "Maps to"/"Builds on"
  edges. Splitting M1 into A/B/C, M2 into A/B, M4 into A/B, M5 into A/B is an elaboration of `00`'s
  milestone set, not a conflict. `00` M7's manual rebaseline now maps to Task 3; `00` M7's deferred
  freshness accelerator is carried as a constraint (line 148). No inverted or missing hard dependency
  found.
- Constraints block (lines 113–156) vs. `00` resolved decisions (lines 338–370) and invariants
  (lines 61–85): all ten resolved decisions plus the confidentiality split, external-PG, single-`core`-
  lock, and archive-cursor≠package-cursor invariants are present and faithful. No new constraint
  contradicts the plans.
- Producer/vertical-slice gates (lines 282–286) and unattended-operation gates (lines 302–306) are
  concrete and directly traceable to `00` M2 acceptance gates (lines 164–174) and M3 acceptance gates
  (lines 194–204) — concrete enough for both agents and Codex reviewers to test against.
- Verdict-string contract (`VERDICT: GO` / `VERDICT: FIXES_REQUIRED`) is consistent across the file and
  matches the `codex-review` skill convention.

## Verification notes

- `git diff --check -- work/10-next-plans/04-claude-orchestrator-instructions.md` → `CHECK_OK`
  (matches the prompt's claim); file is 448 lines.
- All ten prior-review topics named in the prompt are present and correctly used:
  `codex-review` (lines 89, 97), `integration branch` (56–60, 64–80), `worktree` (54–83),
  `ArchiveTimestamp` (282), `empty outbox` (284, plus constraint 143–144), `partial publish` (284,
  304), `git add -A` (410), `3 consecutive` (444), `rebaseline --source` (300), `M1-B` (218, 250),
  `credentialed` (155–156).

---

VERDICT: GO
