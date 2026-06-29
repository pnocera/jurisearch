# Claude re-review request — orchestrator instructions

Please re-review the updated artifact:

- `work/10-next-plans/04-claude-orchestrator-instructions.md`

Prior review:

- `work/10-next-plans/reviews/2026-06-29-04-claude-orchestrator-instructions-claude-review.md`

User intent:

- These are instructions for a fresh Claude Code session to act as orchestrator for executing
  `work/10-next-plans/00-macro-implementation-plan.md`.
- Claude must use agents for implementation/fix/validation work rather than doing those tasks itself.
- After each agent task, Claude must request a Codex review using Claude Code's `codex-review` skill.
- If Codex returns `FIXES_REQUIRED`, an agent fixes the task and Claude asks Codex again until Codex
  gives `GO`.
- Claude commits and pushes only after Codex gives `GO`.
- The instructions should make clear which tasks can run in parallel, which cannot, and how branch or
  worktree coordination works.

Validation already run:

- `git diff --check -- work/10-next-plans/04-claude-orchestrator-instructions.md` passed.
- Searched the artifact for the prior review's critical topics:
  `codex-review`, `integration branch`, `worktree`, `ArchiveTimestamp`, `empty outbox`,
  `partial publish`, `git add -A`, `3 consecutive`, `rebaseline --source`, `M1-B`, and
  `credentialed`.

Please verify the current artifact independently and check whether the prior BLOCKER/WARN/NIT
findings are resolved without introducing new operational ambiguity.

Focus especially on:

- whether the worktree/branch/commit protocol is mechanically executable by a fresh Claude session;
- whether the Codex review protocol correctly names Claude Code's `codex-review` skill and its
  file/`DONE` workflow;
- whether re-review, staging, push, and worktree synchronization rules prevent lost work and noisy
  commits;
- whether the producer vertical-slice and unattended-operation acceptance gates are concrete enough
  for agents and Codex reviewers;
- whether any instruction still conflicts with the macro plan or detailed plans.

Output findings first, ordered by severity. Include concrete recommended fixes for every finding.
End with exactly one line:

VERDICT: GO

or

VERDICT: FIXES_REQUIRED
