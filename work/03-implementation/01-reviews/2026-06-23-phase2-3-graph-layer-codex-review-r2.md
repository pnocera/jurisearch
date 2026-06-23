# Codex Review r2 — Phase 2.3 Graph Layer

Reviewed HEAD `9cfa689c6e9374fa2f9b077a06c3f365a120c261` against `57b1014` on `main`, focusing on the r1 UTF-8 slice blocker and the requested cap/UTF-8 regression tests.

## BLOCKER

- None found.

## WARN

- None found. `crates/jurisearch-ingest/src/juri/mod.rs:929` now slices the window as `body[whole.end()..char_safe_window_end(body, whole.end(), 80)]`. `whole.end()` comes from the regex match and is a valid byte boundary within `body`; `char_safe_window_end` starts from `min(start + 80, text.len())`, walks backward only while the candidate is not a UTF-8 boundary, and must terminate at or before the original candidate because `start` itself is a boundary. Under the call-site precondition, the returned end is in `[start, text.len()]`, is a char boundary, and keeps the slice panic-safe for accented decision bodies.

- The non-boundary-only behavior change is limited to flooring the window end to the previous UTF-8 boundary. When `start + 80` (or `text.len()`) is already a char boundary, `char_safe_window_end` returns the same byte offset as the previous `body.len().min(whole.end() + 80)` expression, so extraction semantics are unchanged in the normal boundary case.

- The new UTF-8 regression test exercises the r1 failure mode. In `crates/jurisearch-ingest/src/juri/tests.rs:450`, `"é".repeat(60)` provides 120 bytes of two-byte characters; because the first window after `"article 5"` begins with one ASCII space, the raw `+80` end falls one byte into an accented character. The test also checks that the bare `article 5` is skipped while the later statutory `L1242-14` citation is retained.

- The new cap test in `crates/jurisearch-ingest/src/juri/tests.rs:470` builds 200 distinct statutory article citations and asserts exactly 64 inferred edges, matching the per-decision cap.

## NIT

- None found.

VERDICT: GO
