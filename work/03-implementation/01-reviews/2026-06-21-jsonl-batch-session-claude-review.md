# Review — Phase 1.5 JSONL Session/Batch acceptance coverage

Date: 2026-06-21
Reviewer: Claude (Opus 4.8)
Scope: uncommitted diff only
- `crates/jurisearch-cli/tests/cli_contract.rs` (+68): new test `batch_jsonl_is_finite_ordered_and_honors_fatal_malformed_input`
- `work/03-implementation/IMPLEMENTATION_PLAN.md` (+6): §1.5 status block

Runtime under test (pre-existing): `run_jsonl` / `dispatch_session_request` (`crates/jurisearch-cli/src/main.rs:1828-1888`), `emit_error` (`:2706`), `write_session_response` (`:2720`), `ProcessExit` mapping (`crates/jurisearch-core/src/error.rs:64`).

Verification: `cargo test -p jurisearch-cli --test cli_contract jsonl` → 3 passed (new test + 2 sibling session tests). Test is hermetic — `expand`/`help schema` are local, no network or env required; per-response `flush` makes ordering deterministic.

## Findings

### Positives
- **Assertions are meaningful, not just smoke.** The non-fatal case proves order is preserved *across* a malformed line (`one` → error → `two`), and the `--fatal` case proves short-circuit by placing the bad line in the middle and asserting `len == 2` (the trailing `two` is never emitted). This genuinely distinguishes "skip" from "stop."
- **Exit/stream contract checked correctly.** Required-`--jsonl` asserts `code(2)` and parses stdout via `assert_json_error_contains`; `BadInput → ProcessExit::User(2)` confirmed at `error.rs:67`. `stderr` emptiness is asserted on all three sub-cases, locking in "stdout-only stream, errors as structured JSON."
- **Plan status block is accurate to the code**, including the narrow `--fatal` semantic ("stops after emitting the malformed-line error") which matches `main.rs:1846-1849`.

### F1 — `batch` ≡ `session`; the "finite batch EOF" claim has no batch-specific code (low, informational)
`run` dispatches both to `run_jsonl(args, warm)` (`main.rs:402-403`) and `_warm` is unused — `batch` and `session` are byte-for-byte identical. The shared loop iterates `stdin.lines()` to EOF, so `session --jsonl` is *also* finite over EOF; "finite batch" is not a distinct behavior. The new test exercises the shared path via the `batch` alias, which is fine for recording acceptance, but it does not test anything `batch`-only (there is nothing batch-only). Not a blocker; flagging that the doc framing implies a distinction the runtime does not yet make.

### F2 — `--fatal` only stops on malformed JSON, not on command-level errors (low, coverage gap)
The `if args.fatal { … break }` lives solely in the `serde_json::from_str` `Err` arm (`main.rs:1846`). A *well-formed* request that errors (unknown command, `bad_input` arg, storage error) always returns `(response, false)` from `dispatch_session_request` and never stops, even under `--fatal`. Code and doc agree, but no test pins this: a future change that makes `--fatal` abort on any error response would pass silently. Consider one case (`--fatal` + a valid-but-erroring command followed by a valid command) to lock the "fatal == malformed-only" semantic — or clarify in the plan whether that narrow scope is intended.

### F3 — Required-`--jsonl` is only tested for `batch`; plan claims both (low)
The status block states "both `session` and `batch` reject missing `--jsonl` with exit code `2`," but only `batch` (no flag) is asserted. The guard is shared code so the claim holds, yet there is no direct `session`-without-`--jsonl` assertion. Low risk; a one-line parametrization would make the test match the wording.

### F4 — §1.5 Tasks "diagnostics on stderr" now contradicts the implementation (low, doc consistency)
`IMPLEMENTATION_PLAN.md:652` still lists task "Keep stdout JSONL-only and **diagnostics on stderr**," while the implementation (and the new status block) emit errors as structured JSON on **stdout** and the tests assert `stderr` is empty. Routing structured errors in-stream on stdout is the correct choice for an agent consuming one correlated JSONL stream — but the status note rewords ("emit diagnostics/errors as structured JSON objects") without flagging that it supersedes the original task bullet, leaving §1.5 internally contradictory. Recommend explicitly noting the superseded contract.

### F5 — Pre-stream error is pretty-printed, not strict JSONL (very low, style)
The missing-`--jsonl` error goes through `emit_error` → `write_json` (multi-line pretty), not the single-line `write_session_response` framing. Harmless here (single object emitted before any stream; the test parses the whole buffer), but a strict JSONL consumer of that path would see multi-line output. Worth a mental note only.

## Recommendations
1. (Optional) Add a `--fatal` + valid-but-erroring-command case to pin the malformed-only stop semantic (F2).
2. (Optional) Assert `session` without `--jsonl` too, to match the "both reject" wording (F3).
3. (Optional) Reconcile the §1.5 Tasks "diagnostics on stderr" bullet with the implemented stdout-structured-error contract (F4).
4. (Optional, out of scope) Remove the unused `_warm` param or record why `batch`/`session` are intended to diverge (F1).

None of the above are blocking. The change is test-and-doc hardening over pre-existing runtime; the new test is correct, deterministic, passes, and its assertions genuinely exercise order preservation, non-fatal continuation, fatal short-circuit, finite EOF, and the required-flag exit code. The plan status accurately describes the shipped behavior.

Verdict: GO
