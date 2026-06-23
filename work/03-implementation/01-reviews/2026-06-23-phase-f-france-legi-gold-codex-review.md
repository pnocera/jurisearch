BLOCKER: None found.

WARN: crates/jurisearch-storage/src/france_legi.rs:147-150
The temporal extractor identifies version-family evidence as any publisher LIEN_ART edge whose
attributes contain debut, fin, num, and etat. That matches the intended VERSIONS rows, and TEXTELR
structural LIEN_ART links with the same attributes are not stored in graph_edges, so the known
structural fallback path should not leak here. The remaining corpus-scale risk is that a non-VERSIONS
article publisher LIEN_ART with the same four attributes would be indistinguishable after projection
and could become a temporal family if it resolves to at least two article versions. Fix: if this
exists in official data, persist a source path/container marker during ingest and filter on it here;
otherwise document the invariant and add a synthetic negative test with a CITATION/structural
LIEN_ART carrying debut/fin/num/etat.

WARN: crates/jurisearch-storage/src/france_legi.rs:160-163 and 189-193
The temporal query relies on the LEGI article parser invariant that valid_from is present and
valid_to is either NULL or after valid_from. The schema allows null/invalid windows, and this SQL
would emit null as_of/query values or an as_of outside a bad finite window if malformed rows ever
land in documents. Also, for open-ended and very short finite windows, as_of is valid_from, which is
in-force but not strictly inside the interval if the gate interprets "strictly inside" as greater
than valid_from. Fix: add d.valid_from IS NOT NULL and valid_to ordering filters in resolved/cases;
if strict interior dates are required, use an interior date when one exists and exclude windows with
no interior date.

NIT: crates/jurisearch-storage/src/france_legi.rs:118-122 and 199-203
known_item has an ORDER BY inside jsonb_agg, but temporal and cross_reference depend on CTE ordering
before the final aggregate. PostgreSQL often preserves this in simple plans, but it is not a strong
determinism guarantee once the final SELECT joins/aggregates. Fix: add ORDER BY to both jsonb_agg
calls, e.g. cross_reference by g.from_document_id and temporal by c.valid_from, c.gold_document_id.

Checked against the requested points:
- known_item is deterministic, limited after ORDER BY document_id, and excludes blank citations and
  null valid_from.
- temporal resolves LIEN_ART targets through payload->>'to_source_uid' to documents.source_uid,
  requires the four VERSIONS-like attributes, keeps only families with at least two distinct
  resolved versions, deduplicates repeated per-member VERSIONS lists by family_key, and sets each
  gold_document_id to the version whose own validity window supplied as_of.
- cross_reference requires both typelien=CITATION and sens=cible, excludes self-citations, resolves
  target source_uid to a LEGI article document, groups targets per citing article, and uses the first
  chunk contextualized_body truncated to 2000 characters.
- SQL interpolation is limited to u32 constants/limits. Each SELECT coalesces to []::jsonb, so the
  psql -qAt output should be a raw JSON array and the format!-combined wrapper remains valid JSON
  for empty result sets.

Verification run:
- cargo test -p jurisearch-storage --test france_legi_gold --no-run

VERDICT: GO
