# Code Review: SOLID/DRY #19 - command-inventory session-exclusion unification

## Scope Reviewed

- Commit range: `HEAD~1..HEAD`
- Commit: `b4b8c04 cli: unify session-exclusion into the command inventory (SOLID/DRY #19, OCP)`
- Files reviewed:
  - `crates/jurisearch-core/src/contract.rs`
  - `crates/jurisearch-core/src/schema/mod.rs`
  - `crates/jurisearch-cli/src/session.rs`
  - `crates/jurisearch-cli/src/tests.rs`

## Findings

No findings.

## Behavior Preservation Checks

- The former `SESSION_EXCLUDED_COMMANDS` set is represented exactly by `CommandSpec::session_excluded == true` for the six intended one-shot-only commands: `ingest`, `eval france-legi`, `eval run`, `eval tune`, `serve`, and `sync`.
- `session --jsonl` and `batch --jsonl` are explicitly present in `COMMANDS` with `session_excluded: false`; because `dispatch_session_request` has no direct match arm for those names, they still fall through to `bad_input` rather than `not_implemented`.
- `command_session_excluded` returns `false` for unknown names through `.unwrap_or(false)`, preserving the dispatcher's unknown-command `bad_input` behavior.
- `agent_help` now derives the one-shot annotation from `command.session_excluded`, which is equivalent to the old `SESSION_EXCLUDED_COMMANDS.contains(&command.name)` check for the pinned six-command set.
- `CommandSpec::session_excluded` is annotated with `#[serde(skip)]`, so `compiled_schema()` serialization of the `commands` inventory remains shaped like the previous public agent schema. The existing `compiled_schema_matches_golden` test continues to guard byte-identical output.
- The new `session_excluded_set_is_exactly_the_one_shot_only_commands` test pins the six-command set directly against the unified inventory, which is stronger than the removed validity-only test over the parallel constant.

## Verification Notes

- Compared the changed implementation against `HEAD~1` for `contract.rs` and `session.rs`.
- Checked current references for `SESSION_EXCLUDED_COMMANDS`, `session_excluded`, `command_session_excluded`, and `command_session_available`.
- Used CodeGraph to inspect `CommandSpec`, `command_session_excluded`, `command_session_available`, `agent_help`, and `compiled_schema` call context.
- Did not rerun Cargo tests in this review because the instruction was to avoid modifying any files other than the requested review artifact, and Cargo may update build artifacts. The review instructions report that the relevant core and CLI test suites already passed.

VERDICT: GO
