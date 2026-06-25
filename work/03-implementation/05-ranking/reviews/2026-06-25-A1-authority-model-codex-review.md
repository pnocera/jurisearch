# Codex Review: A1 Authority Model + Pure Rerank Helper

## Findings

No findings.

## Review Notes

- Scope matched the requested A1 code files: `crates/jurisearch-storage/src/authority.rs`, `crates/jurisearch-storage/src/lib.rs`, `crates/jurisearch-storage/src/retrieval/types.rs`, and `crates/jurisearch-cli/src/request.rs`. The working tree also contains unrelated untracked planning/review files, which I did not inspect as implementation scope.
- The referenced Q&A file `work/03-implementation/05-ranking/qa/20260625-115451-question-nail-down-two-ambiguous-details.md` is not present in this checkout. I checked the implementation against the two ambiguity resolutions restated in the review brief.
- `authority_tier` matches the requested per-source mapping at `crates/jurisearch-storage/src/authority.rs:78`: `cass` published `oui` maps to tier 3, present non-`oui` maps to tier 2 with `marker_absent=false`, absent/blank maps to tier 2 with `marker_absent=true`; `inca` maps to judicial tier 1; `capp` maps to judicial tier 0 with `marker_absent=true`; `jade` uses the trimmed first character case-insensitively for A/B/other/absent; unknown sources return `None`.
- Inertness is correct at both layers: `effective_authority_weight` returns `None` for unset, non-finite, and `<= 0.0` weights at `crates/jurisearch-storage/src/authority.rs:67`, and `authority_rerank` returns before annotation or reordering for the same non-ON weights at `crates/jurisearch-storage/src/authority.rs:207`.
- The rerank is a deterministic permutation. `authority_rerank` forms leader-relative clusters at `crates/jurisearch-storage/src/authority.rs:217`, not pairwise-adjacent band chains, and `rerank_cluster` only collects slots for one `AuthorityOrder` at a time at `crates/jurisearch-storage/src/authority.rs:175`, so mixed-order interleaving remains relevance-positioned while same-order subsequences can reorder within their own slots. Unknown-tier rows are excluded from all slot collections and therefore stay in place.
- Band math uses the rounded `scores.rrf` value through `rounded_rrf` at `crates/jurisearch-storage/src/authority.rs:131`, and adjusted scores are computed from that rounded score at `crates/jurisearch-storage/src/authority.rs:149`. The `leader > 0.0` guard is sound for the current RRF candidate producers, whose emitted candidates have positive RRF scores; missing/zero scores degrade to no clustering rather than over-broad movement.
- `AuthorityTier::fraction` divides only by the fixed judicial/admin `tier_max` values 3 or 2 at `crates/jurisearch-storage/src/authority.rs:59`; there is no zero denominator path from the public constructors.
- The implementation intentionally uses incoming relative order as the final adjusted-score tie-break at `crates/jurisearch-storage/src/authority.rs:193`, instead of re-deriving `chunk_id`/`document_id`. That can differ from an explicit-id tie-break if two same-order candidates with different relevance/tier combinations compute exactly equal adjusted scores, but preserving incoming relevance order on adjusted ties is acceptable here: the producer order is already deterministic, the operation is idempotent, and it avoids an unnecessary dependency on chunk/document/zone id shape in this pure helper.
- Candidate-shape assumptions are compatible with current producers. `hybrid_candidates_json` emits `source`, `scores.rrf`, `chunk_id`, and `document_id` for chunk/document grouping at `crates/jurisearch-storage/src/retrieval/hybrid.rs:52` and `crates/jurisearch-storage/src/retrieval/hybrid.rs:104`; `zone_candidates_json` emits `source`, `scores.rrf`, `document_id`, and `zone_unit_id` at `crates/jurisearch-storage/src/zone_retrieval.rs:260`. `publication` is not emitted yet, which is consistent with A1/A2 sequencing, and `candidate_tier` treats the absent field as `None`.
- Unit coverage locks the load-bearing acceptance cases in `crates/jurisearch-storage/src/authority.rs:263`: tier mapping, blank/missing markers, effective ON/OFF normalization, zero-weight no-op/no-annotation, out-of-band no movement, cross-order no movement, same-order mixed-slot swapping, and unknown-source immobility.

## Verification

- `cargo test -p jurisearch-storage authority` passed.
- `cargo check -p jurisearch-cli` passed.

VERDICT: GO
