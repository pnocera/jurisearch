# Code Review: Zone-Precise Retrieval Analysis r2

## Findings

No blocking findings.

The three r1 findings are resolved in the updated analysis:

- Coverage is no longer overstated as all `cass` plus parser-valid `inca`. The analysis now distinguishes total source counts from parser-valid-pourvoi counts and uses the current-resolver reachable set of 117,674 `cass` + 377,027 `inca` = 494,701 decisions, about 43% of 1,144,796 decisions (`work/03-implementation/04-zones/2026-06-24-zone-precise-retrieval-analysis.md:47-70`, `:96-99`, `:217-219`).
- The normalized-cache limitation is now explicit: `decision_zones.zones_json` contains only `motivations`/`moyens`/`dispositif`, while the full Judilibre response is in `raw_json`, so `introduction`/`expose`/`annexes` need either a normalizer extension or derivation from `raw_json` (`work/03-implementation/04-zones/2026-06-24-zone-precise-retrieval-analysis.md:81-84`, `:159-168`).
- Option A is now split into the two materially different variants: A1 replacing `decision_body` chunks, with snippet/`fetch` serialization mismatch, and A2 adding official zone chunks alongside heuristic chunks, with provenance/unit-type and default-query-exclusion requirements (`work/03-implementation/04-zones/2026-06-24-zone-precise-retrieval-analysis.md:111-131`).

## Validation Notes

- The live index matches the revised coverage table: `documents(kind='decision') = 1,144,796`; by source, `cass=141,616`, `inca=384,312`, `capp=72,929`, `jade=545,939`; parser-valid pourvoi counts are `cass=117,674`, `inca=377,027`, `capp=120`, `jade=0`. That confirms the revised 494,701 current-resolver reachable count.
- The lazy zone-cache claim matches the index: `decision_zones` has 2 rows, both `ok`.
- The sample two-texts/chunking claim matches the index for `cass:JURITEXT000051743650`: local `documents.body` is 9,883 chars; cached Judilibre `raw_json->>'text'` is 10,001 chars; chunks are one `decision_summary` of 877 chars plus two `decision_body` chunks of 5,490 and 4,392 chars.
- The sample normalized cache keys are exactly `dispositif,motivations,moyens`, matching the code path in `normalize_judilibre_zones` (`crates/jurisearch-cli/src/main.rs:3771-3801`).
- The resolver remains gated on the first parser-valid pourvoi extracted from `canonical_json.case_numbers` (`crates/jurisearch-storage/src/decision_zones.rs:48-76`), and no-pourvoi decisions cache `unsupported` in the online enrichment path (`crates/jurisearch-cli/src/main.rs:3652-3671`).
- The retrieval surface still has no zone dimension: `HybridCandidateQuery` has `kind_filter` and `DecisionFilters`, and candidate/snippet SQL joins retrieved `chunks` to `documents` while taking snippets from `chunks.body` (`crates/jurisearch-storage/src/retrieval.rs:70-86`, `:269-385`).
- The current chunking/provenance model remains heuristic for bulk decisions (`crates/jurisearch-ingest/src/juri/mod.rs:754-823`), and the Phase 2 gate still expects bulk jurisprudence sources to report `zone_accurate=false` (`crates/jurisearch-cli/src/main.rs:8790-8824`).

## Minor Note

One wording point is worth tightening later but does not block this analysis: Option A's shared cons say both A1 and A2 "re-chunk/re-embed the Cassation corpus" (`work/03-implementation/04-zones/2026-06-24-zone-precise-retrieval-analysis.md:128-131`). For A2, the more exact statement is that it adds and embeds new official zone units beside the existing heuristic body chunks; it should not require re-embedding the existing topical chunks if the design truly keeps them intact. The surrounding A2 text already conveys the important implementation distinction, so this is not a material correctness issue.

## Recommendation

The r2 analysis is accurate enough to use as the design basis. It now preserves the essential honesty constraints: current-resolver coverage is 494,701 Cassation decisions rather than all Cassation decisions, normalized cache coverage is only three zones unless `raw_json` or a new normalizer is used, and the main-index option has the correct A1/A2 split.

VERDICT: GO
