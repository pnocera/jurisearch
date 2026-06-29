# Claude review prompt - Claude orchestrator instructions

Please review this instruction artifact:

- `/home/pierre/Work/jurisearch/work/10-next-plans/04-claude-orchestrator-instructions.md`

Context artifacts:

- `/home/pierre/Work/jurisearch/work/10-next-plans/00-macro-implementation-plan.md`
- `/home/pierre/Work/jurisearch/work/10-next-plans/01-makeitsimpletodeploy.md`
- `/home/pierre/Work/jurisearch/work/10-next-plans/02-auto-update-server-crons.md`

## User intent

The user plans to start a fresh Claude Code session in a separate terminal and give it instructions to
execute `00-macro-implementation-plan.md`. The instruction file should make Claude Code act as an
orchestrator, use agents for all implementation/fix work, request Codex review after every agent task,
iterate with agents until Codex gives `VERDICT: GO`, and commit/push after each Codex GO. Claude should
identify which tasks can run in parallel and which are dependency-ordered. The instruction file should
help by including a task dependency graph.

## Non-negotiable requirements to check

- Claude is explicitly the orchestrator.
- Claude must not execute implementation tasks directly; it must use agents.
- Agents do implementation, fixes, and validation.
- After each agent task, Claude asks Codex for review.
- If Codex returns `FIXES_REQUIRED`, Claude assigns an agent to apply Codex recommendations and asks
  Codex again until Codex returns `GO`.
- Claude commits and pushes after each Codex GO.
- The file helps Claude identify parallelizable vs non-parallelizable work.
- The file contains a task dependency graph.
- The file preserves the resolved implementation decisions from `00-macro-implementation-plan.md`.
- The instructions should be practically usable by a fresh-context Claude Code session.

## Validation already run

From `/home/pierre/Work/jurisearch`:

```sh
git diff --check -- work/10-next-plans/04-claude-orchestrator-instructions.md
```

This passed.

## Review request

Review the instruction file for orchestration correctness, completeness, ambiguity, and operational
risk. Focus on whether a fresh Claude Code session could follow it without accidentally:

- doing implementation work directly instead of using agents;
- committing without Codex GO;
- missing dependency gates;
- running unsafe tasks in parallel;
- losing review artifacts or committing noisy temporary artifacts;
- violating any resolved plan decisions;
- getting blocked by underspecified Codex review workflow.

Do not edit files. Write findings first, ordered by severity. For each finding include:

- severity: BLOCKER, WARN, or NIT;
- concrete file/line reference;
- why it matters;
- recommended fix.

Then include open questions/risks, verification notes, and finish with exactly one final verdict line:

`VERDICT: GO`

or

`VERDICT: FIXES_REQUIRED`
