## Recommendation

Implement `related` as a depth-1, publisher-edge resolver for `{cites,cited_by,temporal}` and do **not** include `sibling` in T1.1. Make it session-callable by adding `session_related_payload` / `SessionRelatedArgs` and removing `related` from `SESSION_EXCLUDED_COMMANDS`; otherwise the feature will work from the CLI but fail the agent/session contract.

## 1. Relation mapping

`cites` should be outgoing publisher citation edges from the requested exact `document_id`:

```sql
e.edge_source = 'publisher'
AND e.from_document_id = $id
AND e.payload->>'to_source_uid' LIKE 'LEGIARTI%'
AND e.payload->'attributes' @> '[{"key":"typelien","value":"CITATION"},{"key":"sens","value":"cible"}]'::jsonb
```

Resolve each target with `JOIN documents td ON td.source = 'legi' AND td.kind = 'article' AND td.source_uid = e.payload->>'to_source_uid'`.

`cited_by` is the same publisher citation filter in reverse, but keyed by the seed document's `source_uid`, not `to_document_id`:

```sql
seed.document_id = $id
AND e.edge_source = 'publisher'
AND e.payload->>'to_source_uid' = seed.source_uid
AND e.payload->'attributes' @> '[{"key":"typelien","value":"CITATION"},{"key":"sens","value":"cible"}]'::jsonb
```

The neighbour is `e.from_document_id`, joined back to `documents`.

`temporal` should be outgoing publisher `LIEN_ART` version-list edges from the requested exact `document_id`:

```sql
e.edge_source = 'publisher'
AND e.from_document_id = $id
AND e.payload->>'source_tag' = 'LIEN_ART'
AND e.payload->>'to_source_uid' LIKE 'LEGIARTI%'
AND e.payload->'attributes' @> '[{"key":"debut"},{"key":"fin"},{"key":"num"},{"key":"etat"}]'::jsonb
```

Resolve `to_source_uid` to `documents.source_uid`, order by `valid_from`, and either exclude the seed from `neighbours` or include it with `is_seed: true`; I would exclude it from neighbours and expose `family_count` separately.

`sibling` is not a `graph_edges` relation. The existing sibling behavior is structural context: `context_documents_json` finds same-hierarchy documents through `documents.hierarchy_path`, not publisher graph edges. Do not offer `sibling` in `related` for T1.1; keep it under `context --siblings`. If the CLI already advertises it, remove it or reject it with `bad_input` pointing to `context --siblings`.

## 2. Performance and indexing

`cites` and `temporal` are fine with the existing `graph_edges_from_idx`; they are driven by one exact `from_document_id` and then small local filters. `cited_by` is not fine without a new index: limiting the result still requires finding matching `payload->>'to_source_uid'` rows first, so it remains a full scan over the ~12.9M-edge table.

The lowest-risk performant choice is an additive partial expression index for incoming publisher citation targets:

```sql
CREATE INDEX IF NOT EXISTS graph_edges_publisher_citation_to_source_uid_idx
ON graph_edges ((payload->>'to_source_uid'))
WHERE edge_source = 'publisher'
  AND payload->'attributes' @> '[{"key":"typelien","value":"CITATION"},{"key":"sens","value":"cible"}]'::jsonb;
```

A broader reusable variant is also acceptable if you expect more reverse lookups soon:

```sql
CREATE INDEX IF NOT EXISTS graph_edges_publisher_to_source_uid_idx
ON graph_edges ((payload->>'to_source_uid'))
WHERE edge_source = 'publisher'
  AND payload->>'to_source_uid' IS NOT NULL;
```

For T1.1 I would use the citation-specific partial index because it directly supports the expensive relation and is smaller. A new migration is acceptable because this is exactly the kind of additive schema change needed to make the advertised command performant. Do not backfill `to_document_id` in this task: it is more invasive, updates a large table, changes the meaning of an intentionally unresolved column, and still needs careful rules for source UID/version ambiguity. Also do not ship `cited_by` as "scoped/limited" without the index; that only hides the scan.

The current migration runner wraps each migration in `BEGIN`/`COMMIT`, so do not recommend `CREATE INDEX CONCURRENTLY` inside the normal migration list. If there is a production-like existing index where lock time matters, handle that as an operator migration later; for the local index build path, a normal additive migration is the pragmatic choice.

## 3. Version/current/as-of semantics

Do not resolve targets to "current" documents implicitly. The input `document_id` is already version-pinned, and LEGI graph targets are version-level `LEGIARTI...` source UIDs stored in `payload->>'to_source_uid'`. Resolving that source UID back to its concrete `documents.document_id` is the honest graph operation.

I would not add `--as-of` to `related` in T1.1. If it is added later, make it explicit and use it only to filter or choose neighbour documents valid on that date; do not silently rewrite the seed or targets to today's/current version. Default behavior should be "publisher edges attached to this exact versioned document."

## 4. Response shape and depth

Use a stable JSON shape like:

```json
{
  "id": "legi:LEGIARTI...@YYYY-MM-DD",
  "rel": "cites",
  "depth": 1,
  "returned": 12,
  "neighbours": [
    {
      "rel": "cites",
      "direction": "outgoing",
      "depth": 1,
      "document": {
        "document_id": "legi:LEGIARTI...@YYYY-MM-DD",
        "source_uid": "LEGIARTI...",
        "citation": "...",
        "title": "...",
        "validity": {"from": "YYYY-MM-DD", "to": null, "to_exclusive": true},
        "source_url": "..."
      },
      "edge": {
        "edge_id": "...",
        "edge_kind": "refers_to",
        "edge_source": "publisher",
        "source_tag": "LIEN",
        "attributes": [{"key": "typelien", "value": "CITATION"}]
      },
      "authority": {
        "score": 1.0,
        "label": "publisher",
        "confidence": "high",
        "reasons": ["publisher_edge", "target_resolved_by_source_uid"]
      },
      "provenance": {
        "source_payload_hash": "...",
        "source_archive": "...",
        "source_member_path": "..."
      }
    }
  ],
  "pagination": {
    "limit": 50,
    "possibly_truncated": false
  }
}
```

Keep `authority.score` simple: `1.0` for publisher-authored edges with a resolved document target, lower only if you decide to return unresolved targets. Include raw edge attributes and source payload provenance so callers can audit why a relation exists.

Cap `--depth` at 1 for T1.1. Depth greater than 1 creates breadth explosion, repeats the incoming-index problem at every hop, and needs cycle detection, deduplication, scoring decay, and pagination semantics. Accept `--depth 1`; reject `--depth > 1` with `bad_input` saying multi-hop traversal is reserved for a later graph feature. That is the lowest-risk way to deliver a useful, performant graph command now.
