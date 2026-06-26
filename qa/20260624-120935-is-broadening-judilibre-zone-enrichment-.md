# Decision: broaden Judilibre zone enrichment?

## Recommendation

Broaden lazy Judilibre zone enrichment to **INCA only**. Do **not** broaden it to CAPP.

Concretely:

- **INCA: yes.** Same resolver as CASS: parser-valid pourvoi + decision date, then `/decision?id=...`, then cache in `decision_zones`.
- **CAPP: no.** Not reliably resolvable on Judilibre without fuzzy matching, and the live API probes do not show useful appeal-court coverage from the identifiers present in this corpus.

I used the real index `/mnt/models/jurisearch-index/phase2-full-juridic` after you explicitly asked for it, started it on its configured port `39763`, ran read-only sampling/count queries, then stopped it.

## Evidence

### Current code shape

Current enrichment is Cassation-only:

- `annotate_fetched_parts()` computes official-zone availability only when `document["source"] == "cass"`.
- `zone_cache_action(..., source)` enriches only when `online && source == "cass"`.
- `enrich_decision_from_judilibre()` resolves by parser-valid pourvoi plus `decision_date`.
- `decision_resolution_metadata_json()` already extracts:
  - `source_uid`
  - `ecli`
  - `decision_date`
  - first parser-valid pourvoi from `canonical_json.case_numbers`

So INCA support is mostly changing the source predicate from `cass` to `cass || inca`, not inventing a new resolver.

### Real index data

Read-only counts from `/mnt/models/jurisearch-index/phase2-full-juridic`:

```text
inca_docs|384312
inca_valid_pourvoi|377027
capp_docs|72929
capp_valid_ecli|4
capp_ecli_prefixes: TC|4
```

INCA sample records:

```text
inca:JURITEXT000050043900  2024-07-04  ECLI:FR:CCASS:2024:C300373  number=32400373  case_numbers=23-12670
inca:JURITEXT000050044025  2024-07-10  ECLI:FR:CCASS:2024:CO00539  number=42400539  case_numbers=22-16843
inca:JURITEXT000050044026  2024-07-10  ECLI:FR:CCASS:2024:SO00766  number=52400766  case_numbers=23-15666
```

CAPP sample records:

```text
capp:JURITEXT000044069092  2021-07-08  ecli=  number=10  case_numbers=21/03018
capp:JURITEXT000044069094  2021-07-08  ecli=  number=7   case_numbers=20/03951
capp:JURITEXT000035963440  2017-07-03  ECLI:FR:TC:2017:04092  number=T1704092  case_numbers=17-04092
```

The CAPP corpus mostly has RG-style numbers (`21/03018`, `20/03951`) rather than Cassation pourvois. Its only valid ECLI rows in this index are `ECLI:FR:TC:*` Tribunal des conflits rows, not ordinary appeal-court ECLIs.

### Live Judilibre probes

Judilibre search with `operator=exact&page_size=5`:

```text
query=23-12670
  total=1
  id=66863b7ab1dbbe3bae5fff2c
  jurisdiction=cc
  number=23-12.670
  numbers=["23-12.670"]
  ecli=ECLI:FR:CCASS:2024:C300373
  decision_date=2024-07-04
  publication=[]

query=22-16843
  total=2
  id=668e2497fcf93851fdd6456d
  number=22-16.843
  ecli=ECLI:FR:CCASS:2024:CO00539
  decision_date=2024-07-10
  publication=[]
  plus another same-pourvoi result on 2024-06-19

query=23-15666
  total=1
  id=668e24a8fcf93851fdd64589
  number=23-15.666
  ecli=ECLI:FR:CCASS:2024:SO00766
  decision_date=2024-07-10
  publication=[]
```

The duplicate `22-16843` result proves the current date guard is necessary and sufficient.

Judilibre `/decision?id=...` for sampled INCA hits returned official zones:

```text
id=66863b7ab1dbbe3bae5fff2c
  zones: dispositif, expose, introduction, motivations, moyens
  moyens=2, motivations=2, dispositif=1

id=668e2497fcf93851fdd6456d
  zones: dispositif, introduction, motivations
  moyens=0, motivations=1, dispositif=1

id=668e24a8fcf93851fdd64589
  zones: dispositif, expose, introduction, motivations, moyens
  moyens=2, motivations=2, dispositif=1
```

CAPP probes:

```text
query=ECLI:FR:TC:2017:04092
  results=0

query=17-04092
  results=0

query=21/03018
  total=112072, first results are unrelated Cour de cassation hits

query=2103018
  results=0
```

That is not a usable resolver.

## INCA

### Feasible: yes

INCA is unpublished Cour de cassation. The live Judilibre API includes unpublished Cassation decisions: the sampled INCA results resolve with `publication=[]`.

The existing resolver works:

1. Extract parser-valid pourvoi from `canonical_json.case_numbers`.
2. Search Judilibre `query=<pourvoi>&operator=exact&page_size=5`.
3. Accept only a result where normalized `numbers` / `number` contains the local pourvoi and `decision_date` matches local `valid_from`.
4. Fetch `/decision?id=<judilibre_id>`.
5. Normalize `zones.moyens`, `zones.motivations`, `zones.dispositif`.
6. Cache in v12 `decision_zones`.

Although a sampled INCA ECLI query also resolved today, keep the production resolver as pourvoi+date. That is aligned with the shipped CASS implementation and avoids depending on ECLI behavior that has been observed as inconsistent.

### Valuable: yes

INCA is large and mostly resolvable:

- 384,312 INCA decisions.
- 377,027 have parser-valid pourvois.
- The sampled live API records return official zones.

This materially expands zone enrichment coverage for exactly the same court family and user need as CASS. It upgrades many unpublished Cassation decisions from heuristic/unavailable `fetch --part` behavior to official `motivations` / `moyens` / `dispositif`.

### Implementation scope

Small patch:

- Replace source checks:

```rust
document["source"].as_str() == Some("cass")
```

with:

```rust
is_judilibre_cassation_source(document["source"].as_str())
```

where:

```rust
fn is_judilibre_cassation_source(source: Option<&str>) -> bool {
    matches!(source, Some("cass" | "inca"))
}
```

- Likewise in `zone_cache_action()`:

```rust
_ if online && matches!(source, "cass" | "inca") => ZoneCacheAction::Enrich,
```

- Update user-facing text from “Cassation decision” only to “Cour de cassation decision”.
- Keep the same `unsupported` cache behavior for records without parser-valid pourvoi.
- Add tests:
  - `zone_cache_action(..., "inca")` enriches.
  - `zone_cache_action(..., "capp")` falls back.
  - official-zone hint is shown for INCA.

## CAPP

### Feasible: no, not reliably

CAPP is not a near-free extension of the current resolver.

Problems:

- Ordinary appeal decisions use RG numbers like `21/03018`, not Cassation pourvois.
- Querying an RG value on Judilibre returned a huge irrelevant Cassation result set, not a precise appeal decision.
- Plain-normalized RG (`2103018`) returned nothing.
- Valid ECLI coverage in this index is effectively absent for CAPP: only 4 valid ECLI rows, all `ECLI:FR:TC:*`.
- Those Tribunal des conflits ECLI/number probes returned zero Judilibre results.

So there is no reliable non-fuzzy identifier path from CAPP bulk records to Judilibre decisions using current fields.

### Valuable: not enough to justify the work

If Judilibre later gains broad appeal-court coverage with stable ECLI or RG lookup, CAPP zones would be valuable. But with the current corpus/API evidence, enabling CAPP now would mostly create:

- false positives;
- `not_found` cache rows;
- confusing user promises;
- a temptation to add fuzzy title matching, which should be avoided for legal decision identity.

Do not do fuzzy title matching. Do not search CAPP by title/court/date and “pick best”; collision and misidentification risk is too high for official-zone provenance.

## Final call

Enable **INCA**.

Do **not** enable **CAPP**.

The correct product statement after the change is:

> `fetch --part ... --online` uses Judilibre official zones for Cour de cassation decisions from `cass` and `inca` when resolvable by pourvoi + decision date; `capp` and `jade` remain bulk heuristic/unavailable unless a reliable official resolver is added.

This gives a large, low-risk coverage win without weakening provenance or identity correctness.
