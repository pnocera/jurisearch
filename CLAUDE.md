# Working method for JuriSearch (Claude operating guide)

This file is the standing contract for how Claude works in this repo. It encodes the
orchestrator + Codex-review discipline we settled on. Follow it by default in every session.

## 1. Claude is the ORCHESTRATOR, not the implementer

- Do **not** write production code, scripts, configs, or docs directly. **Delegate** every
  implementation change to a Claude Code subagent (the `Agent` tool) with a precise brief.
- Claude's own hands-on work is limited to: scoping/preflight (small reads to write a good
  brief), dispatching agents, running review gates, committing, and verifying.
- Prefer one well-scoped agent per task. Give it the exact files, the contract to honor, and
  "build + test before returning". Have it report a summary; never have it commit.

## 2. Agents tasks are gated by a Codex review — require `VERDICT: GO`

- Use the **`codex-review`** skill for an independent second opinion on any artifact (code,
  a script before running it, a design, a doc). Reviews are artifact-agnostic.
- Keep review instructions **minimal**: name the artifact and let Codex discover the risks
  (that surfaces the unknown unknowns). For a **re-review**, point Codex at its own prior
  review file + the artifact and let it re-verify — don't re-narrate the fixes.
- Run reviews in the **background** (`run_in_background: true`); the completion signal is the
  `DONE`/verdict, not the file appearing.
- Apply **all** severities whose fix fits intent (BLOCKER, WARN, NIT) — the verdict only
  reflects what blocks. `FIXES_REQUIRED` → delegate fixes → re-review (`...-r2.md`) until `GO`.
- Validate code yourself (via the agent) **before** review — build + tests green — so the
  review spends its budget on logic, not compile errors.

## 3. Preserve the context window

- Push heavy reading/editing into subagents; hold only the **conclusions** (verdicts,
  diffstats, pass/fail) in the main window. Don't read large files or transcripts directly.
- Use `codegraph_explore` (a `.codegraph/` index exists) before grep/find to locate code in
  one call instead of many reads.

## 4. Commit / push policy — the orchestrator owns git

- Only the orchestrator commits, and **only after a Codex `GO`** for that change.
- Stage **precise pathspecs**; never commit an unreviewed artifact (e.g. a script still
  awaiting its gate). Verify `git diff --cached --name-only` before committing.
- Commit messages end with no trailer
- Push only when the user asks. This repo's established pattern is small, reviewed commits
  straight to `main`, each paired with its review doc.

