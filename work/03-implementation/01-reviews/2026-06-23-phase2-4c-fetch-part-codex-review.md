# Phase 2.4-C `fetch --part` Review

Reviewed commit: `41b2e6527c4d2c6356de95148ee48d470316cf47` on `main`, diff vs `f59899abc78fe41ab6299a05add9c2500c3af842`.

Review basis: source inspection of `crates/jurisearch-cli/src/main.rs` and `crates/jurisearch-cli/tests/cli_contract.rs`, plus a read-only `cargo fmt --check`. I did not rerun the Postgres-backed contract tests listed in the brief.

## BLOCKER

None.

## WARN

1. `heuristic_visa` does not implement the "leading block" behavior its comment and feature intent describe.

   In `crates/jurisearch-cli/src/main.rs:3099-3110`, the function scans every line in the body and returns every line whose trimmed text starts with `Vu`, regardless of where it appears. That means a later quoted line, factual passage, or reasoning paragraph beginning with `Vu` can be returned as the requested `visa` even after the decision has already moved past the opening visa section. The response is still marked `zone_provenance: "heuristic"` and `official_zones: false`, so this is not an official-zone overclaim, but it can make the extracted `text` materially misleading for `fetch --part visa`.

   Concrete fix: change `heuristic_visa` to scan from the top of the body, collect the initial consecutive `Vu` lines, and stop at the first non-empty non-`Vu` substantive line such as `Faits`, a numbered paragraph, or `Considérant`. Add a regression test where a later body line starts with `Vu` and assert it is not included.

2. The dispositif marker search is only ASCII-case-insensitive, but one caller marker is non-ASCII.

   `rfind_ascii_ci` is documented at `crates/jurisearch-cli/src/main.rs:2496-2512` as requiring an ASCII needle. `heuristic_dispositif` passes `"DÉCIDE"` at `crates/jurisearch-cli/src/main.rs:3089-3094`. The current implementation is UTF-8 safe for slicing because any successful match still starts on an ASCII `D` byte, but the matching is not actually case-insensitive for the accented `É`: `DÉCIDE` can match, while `Décide` or `décide` will not. This weakens the "case-insensitively against the ORIGINAL body" guarantee for accented French markers.

   Concrete fix: either document that accented `DÉCIDE` is matched only in its uppercase source form, or replace this path with a char-boundary-aware matcher that compares the candidate slice and marker using Unicode-aware case folding without deriving offsets from a transformed body. Add a test for `Décide, la Cour...` before relying on the case-insensitive claim.

## NIT

1. The new contract test does not cover several scoped acceptance points.

   `fetch_part_extracts_decision_parts_with_honest_provenance` covers summary, dispositif, motivations, and invalid `--part`, but it does not exercise `visa`, `moyens`, session JSONL forwarding of `part`, or the non-decision `applicable:false` path. The implementation for those paths looks wired correctly (`SessionFetchArgs.part` is defaulted and forwarded through `session_fetch_payload`), but adding focused assertions would make the phase contract harder to regress.

   Concrete fix: extend the test or add a smaller unit/contract test for:
   - `session`/JSONL `{"command":"fetch","args":{"ids":[...],"part":"summary"}}`
   - `fetch` on an article with `--part summary` returning `part.applicable == false`
   - `visa` returning `zone_provenance == "heuristic"`
   - `moyens` returning `zone_provenance == "unavailable"`

2. Formatting is not currently clean in this checkout.

   `cargo fmt --check` fails and reports broad diffs across the repository, including files outside this phase. Because the failure is not isolated to the reviewed hunk, I am not treating it as a phase blocker. The new test signature at `crates/jurisearch-cli/tests/cli_contract.rs:3461-3462` is one visible example that rustfmt would rewrite.

   Concrete fix: run rustfmt in a separate formatting pass or at least format the touched files before the merge gate if the project expects `cargo fmt --check` to pass.

## Verified

- Honest provenance is mostly preserved: decision `part` responses set `official_zones: false`; `summary` uses `zone_provenance: "sommaire"` from `decision_summary` chunks only; `dispositif` and `visa` use `heuristic`; `motivations` and `moyens` use `unavailable`.
- `fetch --part` is gated behind `args.part`: when omitted, `fetch_payload` parses the same storage JSON and does not call `annotate_fetched_parts`, so no `part` key is added by this code path.
- Non-decision documents take the explicit not-applicable path with `applicable:false`.
- Unknown `--part` maps to `bad_input` before index opening.
- `SessionFetchArgs` has `#[serde(default)] part: Option<String>` and `session_fetch_payload` forwards it into `FetchArgs`.
- The previous UTF-8 slicing hazard from uppercasing the whole body is fixed: `heuristic_dispositif` slices the original body using a byte offset returned by `rfind_ascii_ci`, and the unit test `heuristic_dispositif_is_utf8_safe_with_accents_before_marker` covers accented text before the marker.

VERDICT: GO
