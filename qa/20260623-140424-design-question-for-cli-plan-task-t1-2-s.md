## Recommendation

Implement `--group-by {chunk,document}` with `chunk` as the untouched default. For `document`, build a document-ranked rowset in SQL first, apply the document cursor to that rowset, then `LIMIT top_k + 1`. Do not dedupe after chunk pagination.

## 1. SQL shape

`DISTINCT ON (c.document_id)` is a good Postgres-native approach here, as long as it is wrapped in an outer ranking query. The safe shape is:

```sql
WITH ranked AS (...existing per-chunk ranked CTE...),
scored AS (
    SELECT
        r.chunk_id,
        c.document_id,
        d.source,
        d.kind,
        d.citation,
        d.title,
        d.source_url,
        d.valid_from::text AS valid_from,
        d.valid_to::text AS valid_to,
        left(regexp_replace(c.body, '\s+', ' ', 'g'), 280) AS snippet,
        r.lexical_rank,
        r.dense_rank,
        round(r.fused_score::numeric, 8) AS cursor_score,
        r.fused_score
    FROM ranked r
    JOIN chunks c ON c.chunk_id = r.chunk_id
    JOIN documents d ON d.document_id = c.document_id
),
best_document_chunks AS (
    SELECT DISTINCT ON (document_id) *
    FROM scored
    ORDER BY document_id, cursor_score DESC, chunk_id
),
limited AS (
    SELECT *
    FROM best_document_chunks
    WHERE ...document cursor predicate...
    ORDER BY cursor_score DESC, document_id
    LIMIT {limit}
)
SELECT ...
```

The correctness trap is relying on the `DISTINCT ON` ordering as the final ranking. `ORDER BY document_id, score DESC, chunk_id` is only the per-document winner selection order; it groups rows by document ID, so it is not the global ranking. Always use the outer `ORDER BY cursor_score DESC, document_id` for page order. A window function is also correct:

```sql
row_number() OVER (PARTITION BY c.document_id ORDER BY cursor_score DESC, r.chunk_id) AS document_rank
```

then filter `document_rank = 1`. I would use `DISTINCT ON` because it is shorter and idiomatic in Postgres, but either is fine if the final outer ordering is separate.

## 2. Document-level cursor

Use a cursor that is explicitly tagged with its grouping. Do not overload the current `(score, chunk_id)` cursor silently for document mode. The storage type can become something like:

```rust
pub enum RetrievalCursor<'a> {
    Chunk { score: &'a str, chunk_id: &'a str },
    Document { score: &'a str, document_id: &'a str },
}
```

or a struct with `group_by` plus `key`; the important part is rejecting a `chunk` cursor when `--group-by document` is requested, and rejecting a `document` cursor when `--group-by chunk` is requested. Preserve the current `score:chunk_id` parser for existing chunk cursors, but emit new opaque cursors with a version/group prefix, for example:

```text
v1:chunk:<score>:<chunk_id>
v1:document:<score>:<document_id>
```

Gap-free document paging comes from matching the cursor predicate exactly to the outer document order:

```sql
WHERE (
    cursor_score < {score}::numeric
    OR (cursor_score = {score}::numeric AND document_id > {document_id})
)
ORDER BY cursor_score DESC, document_id
LIMIT {top_k_plus_one}
```

Build `next_cursor` from the last displayed document, not from the hidden `top_k + 1` row. This gives a first page of `k` unique documents and resumes at the next document in the same ordered `best_document_chunks` rowset. Also compute/use the same rounded score column for ordering, emitted scores, and cursor comparison; mixing raw `fused_score` with rounded cursor values can duplicate or skip ties.

## 3. Compare capability

Make `compare` a separate command, not `search --modes bm25,dense,hybrid`. `search` should keep one ranking mode, one pagination model, and one cursor contract. `compare` is a diagnostic/evaluation surface whose job is to align multiple rankings.

Recommended shape:

```json
{
  "query": "...",
  "as_of": "YYYY-MM-DD",
  "kind": "code",
  "group_by": "document",
  "top_k": 10,
  "modes": {
    "bm25": {"candidates": [...]},
    "dense": {"candidates": [...]},
    "hybrid": {"candidates": [...]}
  },
  "pool": [
    {
      "document_id": "...",
      "best_chunk_id": "...",
      "by_mode": {
        "bm25": {"rank": 1, "score": 0.01639},
        "dense": null,
        "hybrid": {"rank": 3, "score": 0.02112}
      }
    }
  ],
  "overlap": {
    "bm25_dense": 3,
    "bm25_hybrid": 7,
    "dense_hybrid": 6
  },
  "pagination": {
    "cursor_supported": false
  }
}
```

Keep `compare` single-page only for now. Cursors across independently ranked modes are hard to define honestly: each mode has a different next page, and a pooled union cursor would not mean the same thing as any individual mode cursor. If users need more breadth, they can raise `--top-k`.

## 4. Pool and overfetch changes

Yes, document grouping needs larger candidate pools. If you leave `lexical_limit = top_k * 4` and `dense_limit = top_k * 4`, a long article with many high-scoring chunks can collapse the candidate set to fewer than `k` documents even though more relevant documents exist just below the chunk cutoff.

Use grouping-aware limits. For chunk mode, keep current behavior. For document mode, start with a larger multiplier, for example:

```rust
let pool_multiplier = match group_by {
    GroupBy::Chunk => 4,
    GroupBy::Document => 20,
};
let lexical_limit = top_k.saturating_mul(pool_multiplier);
let dense_limit = top_k.saturating_mul(pool_multiplier);
let query_limit = top_k.saturating_add(1);
```

The existing `dense_pool_limit(dense_limit)` then overfetches before validity/kind filtering, so increasing `dense_limit` also increases the dense temporal pool. That is acceptable for T1.2, but expose the effective limits in detailed diagnostics so performance regressions are visible.

This still cannot mathematically guarantee `k` unique documents if the ranked candidate pool itself contains fewer than `k + 1` unique documents after filtering. To guarantee that in all cases, you would need adaptive retry with larger pool limits or an exhaustive scan, which is higher risk. The low-risk correct implementation is: group before page, cursor over grouped rows, overfetch more for document mode, and report fewer than `k` only when the grouped candidate pool is exhausted.
