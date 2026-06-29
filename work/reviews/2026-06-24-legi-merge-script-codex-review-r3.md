# Code Review: `/tmp/juri-merge.sh` r3

## Findings

No findings.

## Verification Notes

- I did not execute `/tmp/juri-merge.sh` or mutate either database. This was a static review of the script and the repository source paths named in the brief.
- The r2 blocker is fixed. Step 7 no longer accepts a non-empty `candidates` array as proof of both-corpus retrieval; it parses the two CLI search responses and requires at least one LEGI hit from the code smoke and at least one jurisprudence/decision hit from the decision smoke (`/tmp/juri-merge.sh:179-198`).
- The code smoke invocation matches the real CLI semantics. `search --kind code --mode bm25` is accepted by `search_payload`, and `search_with_postgres` translates `LegalKind::Code` into `kind_filter = Some("article")` (`crates/jurisearch-cli/src/main.rs:2451-2491`, `:2652-2657`). The retrieval SQL then applies that as `AND d.kind = 'article'` and emits candidate fields including `document_id`, `source`, and `kind` (`crates/jurisearch-storage/src/retrieval.rs:276-281`, `:297-324`, `:647-674`). The script's `source == "legi"` / `document_id` prefix check therefore proves at least one LEGI article was retrievable through the merged index.
- The decision smoke invocation also matches the real CLI constraints. The CLI still rejects `--kind decision`, so using `search --kind all --mode dense` is the supported broad decision smoke path (`crates/jurisearch-cli/src/main.rs:2473-2478`). For `LegalKind::All`, `search_with_postgres` applies no kind predicate (`crates/jurisearch-cli/src/main.rs:2652-2657`), and dense retrieval emits the same candidate fields (`crates/jurisearch-storage/src/retrieval.rs:675-723`, `:297-324`). The script now requires a returned row with either `kind == "decision"` or a `cass`/`capp`/`inca`/`jade` source/document-id prefix, so an all-LEGI dense result can no longer pass.
- The source/document-id assumptions are grounded in the ingest model: jurisprudence decisions serialize `source` as one of `cass`, `capp`, `inca`, or `jade`, and validate `document_id == "<source>:<source_uid>"` (`crates/jurisearch-ingest/src/juri/mod.rs:123-132`, `:172-212`, `:672-695`; `crates/jurisearch-ingest/src/archive/parser.rs:40-50`). LEGI documents likewise use the `legi:` document-id prefix. The script checks both explicit `source` and document-id prefix, so it is tolerant of either field carrying the corpus discriminator.
- Explicit `--mode bm25` and `--mode dense` avoid structured citation routing; that routing is only used for citation-shaped queries in hybrid mode (`crates/jurisearch-cli/src/main.rs:2669-2738`). The smoke queries therefore exercise the hybrid candidate retrieval path directly.
- The earlier r2-verified destructive merge transaction remains unchanged in this revision (`/tmp/juri-merge.sh:46-143`), and the post-merge status gate still hard-fails on missing query readiness, missing corpus sources, or incomplete embedding coverage before the smoke runs (`/tmp/juri-merge.sh:153-177`).

VERDICT: GO
