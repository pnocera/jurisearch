# TEXTELR Hierarchy Backfill EXPLAIN Evidence

Date: 2026-06-21

## Objective

Capture `EXPLAIN (ANALYZE, BUFFERS)` evidence for the production-shaped LEGI hierarchy backfill candidate query before relying on corpus-scale TEXTELR fallback backfills.

## Data Sources

Official LEGI data was read from:

- Expanded data: `/home/pierre/Apps/juridocs/opendata/LEGI/Freemium_legi_global_20250713-140000`
- Source archive version: `Freemium_legi_global_20250713-140000.tar.gz`

Two temporary mini-baselines were built from official members only:

1. Current Code civil baseline:
   - Source prefix: `legi/global/code_et_TNC_en_vigueur/code_en_vigueur/LEGI/TEXT/00/00/06/07/07/LEGITEXT000006070721`
   - Temp root: `/tmp/jurisearch-code-civil.28xWFJ`
   - Members: 7,370 XML members
   - Mini archive size: 2.4 MB
   - Ingest elapsed: 3:27.44
   - Ingest result: completed; 6,357 documents/chunks, 60,573 publisher edges, 1,013 metadata members
   - Finding: current Code civil `TEXTELR` has 8 structure links (`LIEN_TXT` 1, `LIEN_SECTION_TA` 7, `LIEN_ART` 0), so it is useful as a broad current-code baseline but not as a TEXTELR fallback stress case.

2. Fallback-heavy official text:
   - Source prefix: `legi/global/code_et_TNC_en_vigueur/TNC_en_vigueur/JORF/TEXT/00/00/00/82/99/JORFTEXT000000829916`
   - Text struct: `LEGITEXT000006075080`
   - Temp root: `/tmp/jurisearch-textelr-heavy.130i8j`
   - Members: 226 XML members
   - Mini archive size: 572 KB
   - Ingest elapsed: 0:09.35
   - Ingest result: completed; 223 documents/chunks, 4,307 publisher edges, 3 metadata members
   - TEXTELR structure links: 223 total (`LIEN_ART` 220, `LIEN_SECTION_TA` 1, `LIEN_TXT` 2)
   - Scoped ingest backfill updated 33 documents and invalidated 0 embeddings.

## Query Shape

The EXPLAIN used the candidate CTE from `backfill_legi_article_hierarchy_from_metadata_scoped` in `crates/jurisearch-storage/src/projection.rs`, with the same direct publisher branch, TEXTELR fallback branch, nearest-preceding-section lateral, full preceding-section `jsonb_agg`, direct-edge anti-join, and final candidate ordering.

Plans captured:

- Full-scope candidate selection (`full_scope = true`)
- Text-source scoped replay for `LEGITEXT000006075080`

Raw plan files preserved with this evidence:

- `work/03-implementation/02-evidence/2026-06-21-textelr-backfill-explain-full.txt`
- `work/03-implementation/02-evidence/2026-06-21-textelr-backfill-explain-text-scope.txt`

## Results

The following excerpts are abridged and annotated from the raw EXPLAIN files above.

Full-scope plan on the fallback-heavy official text:

```text
Sort  (actual time=91.562..91.570 rows=33.00 loops=1)
  Sort Method: quicksort  Memory: 849kB
  ->  Append  (actual time=65.509..90.857 rows=33.00 loops=1)
        ->  Nested Loop direct branch (actual time=2.557..2.560 rows=0.00 loops=1)
              Seq Scan on graph_edges edge
              Rows Removed by Filter: 4307
        ->  Nested Loop Anti Join TEXTELR branch (actual time=62.951..88.291 rows=33.00 loops=1)
              Function Scan on jsonb_array_elements article_link
                actual rows=220.00 loops=1
                Rows Removed by Filter: 3
              Index Scan using documents_source_uid_idx on documents
                Index Searches: 220
              nearest-section lateral jsonb_array_elements
                actual rows=0.15 loops=220
                Rows Removed by Filter: 223
              preceding-sections aggregate jsonb_array_elements
                actual rows=1.00 loops=33
                Rows Removed by Filter: 222
              Index Scan using graph_edges_from_idx direct_edge
                Index Searches: 33
Planning Time: 5.550 ms
Execution Time: 91.819 ms
```

Text-source scoped replay for `LEGITEXT000006075080`:

```text
Sort  (actual time=89.997..90.003 rows=33.00 loops=1)
  Sort Method: quicksort  Memory: 849kB
  ->  Append  (actual time=64.520..89.319 rows=33.00 loops=1)
        ->  Nested Loop direct branch (actual time=2.541..2.543 rows=0.00 loops=1)
              Seq Scan on graph_edges edge
              Rows Removed by Filter: 4307
        ->  Nested Loop Anti Join TEXTELR branch (actual time=61.978..86.770 rows=33.00 loops=1)
              Join Filter includes:
                d.document_id = ANY ('{}')
                OR section target = ANY ('{}')
                OR text_struct.source_uid = ANY ('{LEGITEXT000006075080}')
              Function Scan on jsonb_array_elements article_link
                actual rows=220.00 loops=1
                Rows Removed by Filter: 3
              Index Scan using documents_source_uid_idx on documents
                Index Searches: 220
              nearest-section lateral jsonb_array_elements
                actual rows=0.15 loops=220
                Rows Removed by Filter: 223
              preceding-sections aggregate jsonb_array_elements
                actual rows=1.00 loops=33
                Rows Removed by Filter: 222
Planning Time: 6.234 ms
Execution Time: 90.257 ms
```

## Interpretation

- The `documents_source_uid_idx` added in migration 5 is used on the TEXTELR fallback article join (`Index Searches: 220`), so the article lookup side is indexable.
- The current JSONB implementation does repeat `jsonb_array_elements` work:
  - one pass over the structure links for `LIEN_ART`,
  - one nearest-section pass per matched `LIEN_ART`,
  - one preceding-section aggregate pass per candidate that resolves to a section.
- The observed fallback-heavy official text remains small enough for the current query shape: about 90 ms execution time for 223 structure links and 33 candidate rows.
- The scoped replay plan still shows the scope predicate as a join filter around the lateral section lookup. On a corpus with many `TEXTELR` rows, a future optimization should push text-source scoping earlier or materialize structure links instead of relying on repeated JSONB expansion.
- The measured plan is optimistic in one important dimension: the fallback-heavy temp index has exactly one `TEXTELR` row, so the planner starts from that single row and expands its links once. This does not prove the same join shape will hold when a full corpus index contains many thousands of `TEXTELR` rows.
- The fallback-heavy text stresses article-link fan-out but not section depth: it has one `LIEN_SECTION_TA` and 220 `LIEN_ART`, so the nearest-section and preceding-section laterals do not measure deep table-of-contents stacks with many section links.
- The direct publisher branch is also under-stressed here. It scans `graph_edges` and removes 4,307 rows by filtering `payload->>'source_tag' = 'LIEN_SECTION_TA'`, but it matches zero direct section edges in this temp index. A true full-corpus backfill should measure whether that un-indexed JSONB payload filter becomes the dominant cost.
- No immediate maintenance batching change is required for the current implementation slice. The evidence does not prove full-corpus backfill safety; corpus-scale runs should still re-check this path and add batching/materialization if execution time grows with large or numerous TEXTELR structures.

## Commands

Mini archive creation used the expanded official tree and preserved original member paths:

```bash
src=/home/pierre/Apps/juridocs/opendata/LEGI/Freemium_legi_global_20250713-140000
base='legi/global/code_et_TNC_en_vigueur/TNC_en_vigueur/JORF/TEXT/00/00/00/82/99/JORFTEXT000000829916'
root=$(mktemp -d /tmp/jurisearch-textelr-heavy.XXXXXX)
mkdir -p "$root/archives"
(cd "$src" && find "$base" \( -path '*/article/*' -o -path '*/section_ta/*' -o -path '*/texte/*' \) -name '*.xml' | sort > "$root/members.txt")
tar czf "$root/archives/Freemium_legi_global_20250713-140000.tar.gz" -C "$src" -T "$root/members.txt"
target/debug/jurisearch --index-dir "$root/index" ingest legi-archives --archives-dir "$root/archives" > "$root/ingest.json"
```

EXPLAIN was run with `/home/pierre/.pgrx/18.4/pgrx-install/bin/psql` against the temp durable index after starting it with the same pgrx PostgreSQL 18.4 `pg_ctl`.
