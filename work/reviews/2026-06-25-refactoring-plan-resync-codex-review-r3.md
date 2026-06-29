# Review: refactoring-plan resync r3

## BLOCKER

None.

## WARN

- `work/06-refactoring/refactoring-plan.md:196` correctly resolves the r2 ownership issue by keeping `parse_citation_target` / `ParsedCitationTarget` out of `retrieval.rs` and placing them in shared `citation.rs`; CodeGraph shows the two current callers are `france_juris_cite_documents` (`crates/jurisearch-cli/src/main.rs:2709`) and `cite_payload` (`crates/jurisearch-cli/src/main.rs:5180`). However, the helper list for `citation.rs` is still incomplete. `parse_citation_target` directly calls `looks_like_nor` (`main.rs:11449`, defined at `main.rs:11526`), and the listed helpers have private dependencies that must move with them: `parse_article_number` calls `article_number_token` (`main.rs:11474`, defined at `main.rs:11490`), and `detect_code_hint` calls `contains_normalized_phrase` (`main.rs:11516`, defined at `main.rs:11520`). If the implementer follows the explicit list literally, the new `citation.rs` will not compile. Concrete fix: add `looks_like_nor`, `article_number_token`, and `contains_normalized_phrase` to the Phase 3a `citation.rs` move list, or change the sentence to say the listed parser helpers move with their private pure helper dependencies.

## NIT

- `work/06-refactoring/refactoring-plan.md:237` says `fetch_payload` calls `annotate_fetched_parts`, `official_decision_part`, `zone_cache_action`, and `part_block_from_cached_zones`. The dependency direction is right, but only `annotate_fetched_parts` is a direct callee of `fetch_payload`; the others are downstream through `annotate_fetched_parts` / `official_decision_part`. Concrete fix: reword this as "`fetch_payload` directly uses `annotate_fetched_parts`, which in turn uses `official_decision_part`, `zone_cache_action`, and `part_block_from_cached_zones`."

VERDICT: FIXES_REQUIRED
