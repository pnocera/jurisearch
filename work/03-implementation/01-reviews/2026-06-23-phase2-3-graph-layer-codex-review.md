# Codex Review — Phase 2.3 Graph Layer

Reviewed HEAD `57b1014` against parent `12cd323` on `main`.

## BLOCKER

- `crates/jurisearch-ingest/src/juri/mod.rs:928` slices a UTF-8 `&str` with a fixed byte end offset:
  `&body[whole.end()..body.len().min(whole.end() + 80)]`. `whole.end()` from the regex match is a
  valid byte boundary, but `whole.end() + 80` is arbitrary and can land inside a multi-byte French
  character (`é`, `ê`, etc.). In that case parsing a decision body panics before projection or
  quarantine can handle the member. This directly violates the review checklist's char-safe slicing
  requirement and is realistic because decision bodies are accented UTF-8. Fix by computing the
  window end through `char_indices()` from `whole.end()` (or by taking the suffix and collecting up to
  80 bytes/characters only at valid boundaries), then add a regression test with an accented
  character crossing the window boundary.

## WARN

- None found. The extractor precision rules otherwise match the brief: bare `"article 8 de la
  convention européenne"` is skipped, statutory-prefix and `"du code"` references are retained, the
  next-article truncation prevents a following reference from supplying a code hint to an earlier bare
  reference, normalization is non-panicking, dedup is by `(article_number, code_hint)`, ordering is
  first appearance, and the 64-edge cap is enforced by construction.

- Projection keeps publisher and inferred trust separate. `CanonicalDecision::validate()` rejects
  mislabelled edge sources, LEGI reports `inferred_edges: 0`, decision projection inserts both edge
  sets through `insert_graph_edge`, and `insert_graph_edge` stores `edge.edge_source` verbatim.

- `interpreted_by` follows the intended publisher-only reverse citation path: it joins the seed
  article `source_uid` to `payload->>'to_source_uid'`, requires the same `CITATION`/`cible` attribute
  filter as the partial index, restricts neighbours to `fd.kind = 'decision'`, excludes the seed
  document, and leaves the shared `authority.label = 'publisher'` accurate for this relation.
  Existing `cites`, `cited_by`, and `temporal` arms remain structurally unchanged except for the new
  enum arm.

- The intended limitations are acceptable for this slice: inferred edges are stored as lower-trust
  unresolved evidence and are not traversed by `related`, while `interpreted_by` only covers publisher
  LIENs that resolve to an article `source_uid`.

## NIT

- `crates/jurisearch-ingest/src/juri/tests.rs` covers extraction precision, determinism/dedup, and
  mislabel rejection, but it does not cover the per-decision cap or the UTF-8 window boundary. The
  cap test is not required for correctness once the panic is fixed, but it would lock down a key
  operational bound from the brief.

VERDICT: FIXES_REQUIRED
