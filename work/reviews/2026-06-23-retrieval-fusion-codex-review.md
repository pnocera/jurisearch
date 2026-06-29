# Code Review: Weighted RRF Fusion

LOW crates/jurisearch-storage/src/retrieval.rs:8: The new top-level comment still says "Defaults preserve equal-weight RRF", but the implementation below sets `DEFAULT_RRF_DENSE_WEIGHT` to `0.3`. This is documentation drift only; the code, tuning note, and artifact provenance all point at the intended BM25-favored default.

INFO crates/jurisearch-storage/src/retrieval.rs:23: `rrf_weights()` rejects missing, unparsable, negative, NaN, and infinite environment values before they reach SQL. The defensive clamp in `format_sql_f64()` gives a finite non-negative literal even if a future caller bypasses `rrf_weights()`.

INFO crates/jurisearch-storage/src/retrieval.rs:276: The weighted SQL expression is valid and preserves the existing NULL-arm behavior. A lexical-only or dense-only hybrid candidate still contributes only its present arm, and setting both environment weights to `1.0` recovers the old `1.0 / (60.0 + rank)` sum exactly, with no off-by-constant.

INFO crates/jurisearch-storage/src/retrieval.rs:276: I did not find an injection path for non-default weights. The interpolated values are parsed `f64`s, not raw strings, filtered to finite `>= 0`, and rendered through fixed Rust formatting before interpolation.

INFO crates/jurisearch-storage/src/retrieval.rs:294: The BM25-only and dense-only branches remain unchanged and still use their single-arm `1.0 / (60.0 + rank)` ordering.

INFO crates/jurisearch-cli/src/main.rs:823: The France-LEGI artifact now records the same default/env weight source used by hybrid retrieval. Because production search and eval both call the shared retrieval path, the default-change blast radius is consistent with the stated intent: the gate measures the production hybrid configuration instead of a special eval-only override.

INFO work/03-implementation/02-evidence/2026-06-23-retrieval-fusion-tuning.md:1: The tuning artifact is honest about the result: dense `0.3` is documented as a measured production improvement, while the Phase 1 gate remains fail-closed because no global weight passes the configured thresholds.

Validation not rerun for this review; I inspected the working-tree diff and relevant current source.

VERDICT: GO
