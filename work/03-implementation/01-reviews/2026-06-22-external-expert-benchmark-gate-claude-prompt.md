Please review the current uncommitted diff in /home/pierre/Work/jurisearch.

Scope:
- Replace the Phase 1 gate blocker that depended on unavailable named-human review of internal LEGI fixtures with a fail-closed external expert-annotated benchmark gate.
- Keep internal LEGI fixtures visible as source-checked smoke/release-candidate evidence, but not sufficient to open the Phase 1 claim.

Changed files:
- crates/jurisearch-cli/src/main.rs
- crates/jurisearch-core/src/schema.rs
- crates/jurisearch-cli/tests/cli_contract.rs
- work/03-implementation/IMPLEMENTATION_PLAN.md
- work/03-implementation/02-evidence/2026-06-22-phase1-fixture-strength-decision.md
- work/03-implementation/02-evidence/2026-06-22-external-expert-benchmark-gate.md

Separate research helper:
- A separate Claude research artifact may still be running at `work/03-implementation/01-reviews/2026-06-22-external-expert-benchmark-claude-research.md`.
- Do not depend on that file existing; review the current implementation diff directly.

User intent and constraints:
- No local human legal-domain reviewers are available.
- Do not pretend source-checked internal LEGI fixtures are enough for release gating.
- Phase 1 should require an external expert-annotated French legal retrieval benchmark before `claim_allowed=true`.
- The new gate must stay fail-closed until a real external benchmark run and metrics are recorded.
- Internal LEGI release candidates remain useful smoke/regression evidence.
- Do not edit files.

Validation already run:
- `cargo fmt --all`
- `cargo test -p jurisearch-cli phase1_gate_payload_maps_ready_inputs_and_failed_members`
- `cargo test -p jurisearch-cli --test cli_contract`

Please review for:
- Gate safety: can `claim_allowed` open incorrectly?
- Schema/API coherence for `phase1_gate.external_benchmark`.
- Whether the code/docs still imply named human review is required as the live Phase 1 blocker.
- Whether the selected datasets and limitations are represented honestly enough for this implementation slice.
- Any missing tests or compatibility concerns.

Output structure:
- Findings first, ordered by severity, with file/line references where applicable.
- Open questions/risks.
- Verification notes.
- Final verdict line exactly one of:
  - VERDICT: GO
  - VERDICT: FIXES_REQUIRED
