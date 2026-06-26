# Design: authority-aware ranking + Judilibre zone enrichment

Scope checked: static source only. I did not open `/mnt/models/jurisearch-index/phase2-full-juridic`.

Relevant current code facts:

- Hybrid retrieval is in `crates/jurisearch-storage/src/retrieval.rs`: `RRF_K=60`, env-backed `rrf_weights()`, `RetrievalOptions`, `HybridCandidateQuery`, `ranked_candidate_ctes()`, and final ordering in `hybrid_candidates_json()`.
- CLI search already maps `LegalKind::Decision => Some("decision")` in `search_with_postgres()` and passes decision filters through `DecisionFilters`.
- Decision authority metadata already exists in `documents.canonical_json`: `publication`, `solution`, `formation`, `nature`, `jurisdiction`, `ecli`, `case_numbers` from `crates/jurisearch-ingest/src/juri/mod.rs`.
- `related` currently emits an `authority` object, but it is graph-edge authority only, not retrieval ranking authority.
- `fetch --part` currently calls `annotate_fetched_parts()` and is explicitly heuristic: summary from `decision_summary`, dispositif/visa best-effort, motivations/moyens unavailable.
- `cite --online` for decisions is still a no-op; `PisteClient` has Judilibre KeyId GET support but only exposes `/search` and `/transactionalhistory` helpers.
- Phase 2 gate still correctly requires bulk `corpus_sources.{cass,capp,inca,jade}.zone_accurate=false`; per-decision enrichment should not mutate that source-level claim.
- Judilibre docs say `/decision` returns full text plus `zones`; `/search` returns result IDs; zones include `introduction`, `expose`, `moyens`, `motivations`, `dispositif`, `annexes`, each as one or more `{start,end}` fragments in `text`. Sources: [Judilibre OpenAPI](https://raw.githubusercontent.com/Cour-de-cassation/judilibre-search/dev/public/JUDILIBRE-public.json), [Judilibre README](https://github.com/Cour-de-cassation/judilibre-search/).

## A. Authority-Aware Ranking

### Recommendation

- Add authority as a **post-RRF re-rank term over the already-generated candidate pool**, not as a lexical/dense candidate-generation filter.
- Default it to effectively disabled first: `JURISEARCH_DECISION_AUTHORITY_WEIGHT=0.0`.
- Turn it on only after advisory ordering eval shows gain and France-juris known-item recall non-regression. A realistic tuned value will be small because RRF scores are around `0.01-0.03`; start sweep `0.0000,0.0005,0.001,0.002,0.004`.
- Apply only when `d.kind='decision'`. Statutes/articles must get `authority_score=0` and rank exactly as today when mixed with decisions.

### Why Post-RRF

- Candidate recall stays controlled by existing BM25+dense arms.
- Authority cannot introduce non-matching documents; it can only reorder candidates already retrieved.
- It is deterministic, cheap, pagination-compatible, and reversible by setting the env weight to zero.
- It avoids polluting BM25/dense semantics with legal-policy preferences.

### Concrete API / Env

In `crates/jurisearch-storage/src/retrieval.rs`:

```rust
const DEFAULT_DECISION_AUTHORITY_WEIGHT: f64 = 0.0;

pub fn decision_authority_weight() -> f64 {
    env_weight("JURISEARCH_DECISION_AUTHORITY_WEIGHT", DEFAULT_DECISION_AUTHORITY_WEIGHT)
}

#[derive(Debug, Clone, Copy, Default)]
pub struct RetrievalOptions {
    pub rrf_lexical_weight: Option<f64>,
    pub rrf_dense_weight: Option<f64>,
    pub ivfflat_probes: Option<u32>,
    pub decision_authority_weight: Option<f64>,
}

impl HybridCandidateQuery<'_> {
    fn effective_decision_authority_weight(&self) -> f64 {
        self.options
            .decision_authority_weight
            .unwrap_or_else(decision_authority_weight)
            .max(0.0)
    }
}
```

In CLI `SearchArgs` / session args:

```rust
#[arg(long)]
authority_weight: Option<f64>,
```

Wire it through `SearchArgs::retrieval_options()`, and add sweep support in `eval tune`:

```text
--sweep authority-decision=0.0:0.004:0.0005
```

### Authority Score SQL

Keep the score simple, inspectable, and JSON-visible:

```sql
CASE
  WHEN d.kind <> 'decision' THEN 0.0
  WHEN d.source IN ('cass','capp','inca')
       AND lower(coalesce(d.canonical_json->>'publication','')) IN ('oui','b','r','l','c')
    THEN 1.0
  WHEN d.source = 'jade'
       AND nullif(btrim(coalesce(d.canonical_json->>'publication','')), '') IS NOT NULL
       AND lower(d.canonical_json->>'publication') NOT IN ('non','inédit','inedit')
    THEN 1.0
  ELSE 0.0
END AS authority_score
```

Notes:

- Judicial bulk stores `PUBLI_BULL@publie`; current parser likely produces `oui/non`, while Judilibre taxonomy uses keys such as `b/r/l/c`. Accept both shapes so the scorer is robust.
- For JADE, do not guess a detailed hierarchy until corpus values are inspected on the backup index; first slice should treat non-empty/non-negative `PUBLI_RECUEIL` as “higher authority”.
- Add `authority_reasons` later if needed; the first ranking term can be numeric only.

### Exact SQL Placement

In `hybrid_candidates_json()`:

1. Leave `ranked_candidate_ctes()` unchanged; it returns RRF `fused_score`.
2. In `GroupBy::Chunk`, change `limited` to compute:

```sql
round(r.fused_score::numeric, 8) AS rrf_score,
{authority_score_sql} AS authority_score,
round((
  r.fused_score
  + ({authority_weight} * {authority_score_sql})
)::numeric, 8) AS rank_score
```

Order and cursor by `rank_score`, then stable tie-breakers:

```sql
ORDER BY rank_score DESC, rrf_score DESC, r.chunk_id
```

3. In `GroupBy::Document`, compute authority in `scored`, pick the best chunk per document by original RRF, then page by final rank:

```sql
scored AS (
  SELECT
    r.chunk_id, c.document_id, d.source, d.kind, d.citation, d.title, d.source_url,
    ...,
    round(r.fused_score::numeric, 8) AS rrf_score,
    {authority_score_sql} AS authority_score,
    round((r.fused_score + ({authority_weight} * {authority_score_sql}))::numeric, 8) AS rank_score
  FROM ranked r
  JOIN chunks c ON c.chunk_id = r.chunk_id
  JOIN documents d ON d.document_id = c.document_id
),
best_document_chunks AS (
  SELECT DISTINCT ON (document_id) *
  FROM scored
  ORDER BY document_id, rrf_score DESC, chunk_id
),
limited AS (
  SELECT *
  FROM best_document_chunks
  {cursor_predicate_on_rank_score}
  ORDER BY rank_score DESC, rrf_score DESC, document_id
  LIMIT {limit}
)
```

4. Response scores should expose both values:

```sql
'scores', jsonb_build_object(
  'rrf', rrf_score,
  'authority', authority_score,
  'authority_weight', {authority_weight},
  'final', rank_score,
  'lexical_rank', lexical_rank,
  'dense_rank', dense_rank
)
```

5. Cursor should use the final score:

```text
doc:<rank_score>:<rrf_score>:<document_id>
```

If preserving the existing cursor format is preferred, keep `doc:<rank_score>:<document_id>` and use `document_id` as the only stable tie-breaker; this is simpler but less exact when many rows share rounded scores. Since this is a ranking-behavior change, I would accept a cursor version bump.

### Precision / Recall Guardrails

- Never apply a hard `WHERE publication...` filter.
- Never alter `lexical_limit` / `dense_limit` candidate construction.
- Clamp invalid env values to default as `rrf_weights()` does.
- Keep `authority_weight=0.0` in CI/unit tests unless a test explicitly exercises authority.
- Add tests proving:
  - `kind='article'` ranks are byte-for-byte unchanged at weight > 0.
  - decision with higher authority outranks an otherwise tied decision.
  - cursor pagination does not duplicate/skip rows with authority enabled.

### Honest Eval

Do **not** use `eval france-juris` recall@10 as the validation signal; it is known-item recall from `decision_summary` and may not move.

Add an advisory eval category/command, not a Phase 2 gate requirement:

```text
jurisearch eval authority-ranking \
  --index-dir /mnt/models/jurisearch-index/phase2-full-juridic.backup-20260624 \
  --top-k 10 \
  --sweep authority-decision=0.0:0.004:0.0005 \
  --out work/03-implementation/02-evidence/authority-ranking.json
```

Gold/query shape:

- Build qrels from existing official/inferred decision-to-statute `graph_edges`:
  - choose LEGI articles cited by at least N decisions and at least one published/high-authority decision;
  - query = statute citation/title, e.g. `"jurisprudence article 1240 code civil responsabilité"` using article citation/title/body excerpt;
  - labels:
    - `2` = decision cites the article and has high authority publication;
    - `1` = decision cites the article but is not high-authority;
    - `0` = pooled candidate not known to cite the article.
- Metrics:
  - `ndcg@10` over graded labels: primary;
  - `published_in_top3_rate`: fraction of queries with a high-authority cited decision in top 3;
  - `mean_best_published_rank`;
  - `france_juris_recall_at_10_delta` against the existing known-item benchmark as a non-regression check.

This is honest because it evaluates an ordering preference among plausibly relevant decisions, not a lookup pass/fail. It also makes a no-op visible: if authority weight does not improve nDCG / published-rank metrics, leave default at `0.0`.

### Smallest Reviewable First Slice

1. Add `decision_authority_weight` plumbing and score exposure with default `0.0`.
2. Add unit tests for SQL/output shape and article non-effect.
3. Add `eval tune` parameter support for `authority-decision`.
4. Add a lightweight `eval authority-ranking --gold-from-graph` extractor only after the ranking hook is reviewed.

Main pitfalls:

- Overweighting authority can bury the semantically best decision; keep candidate generation untouched and require recall non-regression.
- Publication value vocabularies differ between DILA bulk and Judilibre taxonomy; make the scorer tolerant and log/expose raw publication.
- Cursor semantics must use the final score, not old RRF, once authority is enabled.
- Mixed `kind=all` searches must not privilege decisions so much that statutes disappear; default `0.0` plus small tuned weights avoids this.

## B. Lazy Judilibre Zone Enrichment

### Recommendation

- Implement lazy, explicit-online enrichment for **judicial decisions only** (`cass/capp/inca`) first.
- Do not bulk-enrich 1.14M decisions.
- Do not mutate `documents.canonical_json`; add a v12 cache table.
- Use cached official zones automatically when present; perform network fetch only when the user opts in (`fetch --part ... --online` / later `context --online-zones`).
- Keep JADE administrative decisions on current heuristic/unavailable path unless a separate administrative official-zone source is added. Judilibre is a Cour de cassation / judicial-order API; treating it as covering JADE would be a correctness bug.

### Identifiers / Resolution Strategy

Local identifiers:

- `documents.source_uid`: `JURITEXT...` for judicial, `CETATEXT...` for administrative.
- `canonical_json.ecli`: best exact cross-system identifier when valid.
- `canonical_json.case_numbers`: useful for Cassation pourvoi, less reliable for CAPP/INCA.

Judilibre:

- `/search` returns Judilibre provider `id`.
- `/decision?id=<provider_id>` returns full `text` and `zones`.
- Search filters include `query`, `operator=exact`, `number`, `publication`, `jurisdiction`, date bounds, etc.; default sort is `scorepub`.

Resolution order:

1. If cache has `provider_decision_id`, call `/decision?id=...` only when refresh is needed.
2. Else if local decision has valid `ECLI:FR:...`, search `query=<ecli>&operator=exact&page_size=5&resolve_references=false`; accept exactly one result whose `ecli` equals local ECLI case-insensitively.
3. Else for `source='cass'`, search by pourvoi/case number:
   - `query=<pourvoi>&operator=exact&page_size=5`;
   - optionally constrain by local `decision_date` with `date_start=date_end`;
   - accept only if returned `numbers` contains normalized local case number and date matches.
4. For CAPP/INCA without ECLI, skip network enrichment in the first slice; do not fuzzy-match by title.
5. For JADE, return current heuristic/unavailable with `zone_provenance="bulk_heuristic"` and a note that Judilibre does not cover this source family.

### Piste Client Additions

In `crates/jurisearch-official-api/src/lib.rs`:

```rust
impl PisteClient {
    pub fn judilibre_search_params(
        &self,
        params: &[(&str, &str)],
    ) -> Result<Value, OfficialApiError>;

    pub fn judilibre_decision(
        &self,
        provider_id: &str,
        query: Option<&str>,
    ) -> Result<Value, OfficialApiError>;
}
```

Implementation:

- Build query string with `urlencoding` or equivalent; current `judilibre_get(path)` only accepts a raw path.
- Use existing KeyId header, retry/backoff, and `OfficialApiError::RateLimited`.
- Add tests asserting paths:
  - `/cassation/judilibre/v1.0/search?query=...&operator=exact&page_size=5`
  - `/cassation/judilibre/v1.0/decision?id=...&resolve_references=false`

### Migration v12 Cache Table

In `crates/jurisearch-storage/src/migrations.rs`:

```sql
CREATE TABLE IF NOT EXISTS decision_zones (
    document_id text PRIMARY KEY REFERENCES documents(document_id) ON DELETE CASCADE,
    provider text NOT NULL,
    provider_decision_id text,
    source_uid text NOT NULL,
    ecli text,
    status text NOT NULL CHECK (status IN ('ok','not_found','unsupported','invalid_offsets','upstream_error')),
    fetched_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz,
    upstream_update_date text,
    upstream_decision_date text,
    text_hash text,
    offset_unit text,
    zones_json jsonb NOT NULL DEFAULT '{}'::jsonb,
    raw_json jsonb NOT NULL DEFAULT '{}'::jsonb,
    error text,
    zone_schema_version text NOT NULL DEFAULT 'judilibre:v1'
);

CREATE INDEX IF NOT EXISTS decision_zones_provider_idx
ON decision_zones(provider, provider_decision_id);

CREATE INDEX IF NOT EXISTS decision_zones_ecli_idx
ON decision_zones(upper(ecli))
WHERE ecli IS NOT NULL;

INSERT INTO index_manifest(key, value, updated_at)
VALUES ('schema', jsonb_build_object('schema_version', 12), now())
ON CONFLICT (key) DO UPDATE
SET value = excluded.value,
    updated_at = excluded.updated_at;
```

Why separate table:

- Cache can refresh independently of immutable bulk ingest/projection.
- Source-level Phase 2 zone honesty remains intact.
- Upstream failures and misses can be cached without contaminating canonical records.
- Future admin-zone provider can reuse the table with `provider='arianeweb'` or similar.

### Storage Helpers

Add `crates/jurisearch-storage/src/decision_zones.rs`:

```rust
pub struct DecisionZoneCacheQuery<'a> {
    pub document_id: &'a str,
}

pub struct UpsertDecisionZones<'a> {
    pub document_id: &'a str,
    pub provider: &'a str,
    pub provider_decision_id: Option<&'a str>,
    pub source_uid: &'a str,
    pub ecli: Option<&'a str>,
    pub status: &'a str,
    pub upstream_update_date: Option<&'a str>,
    pub upstream_decision_date: Option<&'a str>,
    pub text_hash: Option<&'a str>,
    pub offset_unit: Option<&'a str>,
    pub zones_json: &'a serde_json::Value,
    pub raw_json: &'a serde_json::Value,
    pub error: Option<&'a str>,
    pub ttl_seconds: Option<i64>,
}

pub fn decision_zones_json(
    postgres: &ManagedPostgres,
    query: &DecisionZoneCacheQuery<'_>,
) -> Result<String, StorageError>;

pub fn upsert_decision_zones(
    client: &mut postgres::Client,
    row: &UpsertDecisionZones<'_>,
) -> Result<(), StorageError>;
```

For CLI one-shot commands, opening a `postgres::Client` from `postgres.connection_string()` is consistent with ingestion paths.

### Zone Normalization

Judilibre raw shape:

```json
{
  "id": "...",
  "ecli": "ECLI:FR:...",
  "number": "17-18.194",
  "numbers": ["17-18.194"],
  "publication": ["c", "b"],
  "decision_date": "2018-12-20",
  "text": "...",
  "zones": {
    "introduction": [{"start": 0, "end": 2309}],
    "moyens": [{"start": 2204, "end": 4753}],
    "motivations": [{"start": 4753, "end": 7543}],
    "dispositif": [{"start": 7543, "end": 7924}],
    "annexes": [{"start": 7924, "end": 45372}]
  }
}
```

Normalize to:

```json
{
  "provider": "judilibre",
  "provider_decision_id": "...",
  "text_hash": "sha256:...",
  "parts": {
    "motivations": [{"start": 4753, "end": 7543, "text": "..."}],
    "moyens": [{"start": 2204, "end": 4753, "text": "..."}],
    "dispositif": [{"start": 7543, "end": 7924, "text": "..."}],
    "moyens_annexes": [{"start": 7924, "end": 45372, "text": "..."}]
  }
}
```

Part mapping:

- `fetch --part motivations` -> Judilibre `zones.motivations`.
- `fetch --part moyens` -> Judilibre `zones.moyens`; include `annexes` only as a separate `moyens_annexes` field or with an explicit note, not silently mixed.
- `fetch --part dispositif` -> Judilibre `zones.dispositif`.
- `fetch --part visa` -> no primary Judilibre zone named `visa`; use upstream `visa` field if present, otherwise keep current heuristic and report `official_zones=false` for that part.
- `fetch --part summary` -> keep source `SOMMAIRE`/Judilibre `summary`; this is structural but not a zone offset.

Offset handling pitfall:

- Do not slice Rust strings blindly with upstream `start/end`.
- Validate offsets:
  - first try byte offsets only if both are UTF-8 boundaries;
  - else try character-index slicing;
  - if neither produces valid non-empty text, cache `status='invalid_offsets'` and fall back to heuristic/unavailable.
- Preserve multiple fragments. Do not assume zones are sequential; Judilibre docs explicitly warn they may not be.

### CLI Hook

Extend `FetchArgs`:

```rust
/// Also consult Judilibre for official decision zones when --part targets a decision.
#[arg(long)]
online: bool,
```

Then change:

```rust
fn annotate_fetched_parts(
    postgres: &ManagedPostgres,
    response: &mut Value,
    part: DecisionPart,
    online: bool,
) -> Result<(), ErrorObject>
```

Flow per document:

1. Non-decision: current `not_applicable`.
2. Decision with cached `decision_zones.status='ok'` and requested part present: return official part.
3. If no cache and `online=false`: current heuristic/unavailable response, plus `"official_zones_available": false`.
4. If no cache and `online=true`:
   - if `source='jade'`: cache/return `unsupported`;
   - resolve Judilibre provider ID;
   - fetch `/decision`;
   - validate ECLI/number/date against local metadata;
   - normalize zones and upsert cache;
   - return official part if present.
5. On 429/423/5xx/transport: do not fail the whole `fetch` unless the user requests strict mode later; return fallback part with:

```json
{
  "official_zones": false,
  "zone_provenance": "judilibre_unavailable",
  "online": {"checked": true, "provider": "judilibre", "state": "rate_limited"}
}
```

Official part response:

```json
{
  "requested": "motivations",
  "applicable": true,
  "available": true,
  "official_zones": true,
  "zone_accurate": true,
  "zone_provenance": "judilibre",
  "provider": "judilibre",
  "provider_decision_id": "...",
  "fetched_at": "...",
  "text_hash": "sha256:...",
  "fragments": [
    {"start": 4753, "end": 7543, "text": "..."}
  ],
  "text": "..."
}
```

For `context`, do not block the first slice. Later add:

```rust
#[arg(long)]
online_zones: bool,
#[arg(long)]
part: Option<String>,
```

and reuse the same enrichment helper for `target.part`.

### Per-Decision Provenance vs Phase 2 Gate

- Enriched document response can honestly say `zone_accurate=true` for that returned part/decision.
- `corpus_sources.cass.zone_accurate` must remain `false`, because the bulk corpus as a source is not fully zone-accurate.
- Optionally add `status` advisory fields:

```json
"zone_enrichment": {
  "provider": "judilibre",
  "cache_table": "decision_zones",
  "cached_ok": 1234,
  "unsupported": 545939,
  "source_level_bulk_zone_accurate": false
}
```

Do not make this a Phase 2 gate input unless the product later claims corpus-wide official zones.

### Rate Limits / Freshness

- Network only on explicit `--online`.
- Cache positive responses for long TTL, e.g. 30 days; cache `not_found`/`unsupported` for 7 days; do not cache transient `rate_limited` unless with very short TTL.
- Respect existing `RetryPolicy` and `OfficialApiError::RateLimited`.
- Add env:

```text
JURISEARCH_JUDILIBRE_ZONE_TTL_DAYS=30
JURISEARCH_JUDILIBRE_ZONE_NEGATIVE_TTL_DAYS=7
JURISEARCH_JUDILIBRE_ZONE_REFRESH=0
```

- Freshness check:
  - store `upstream_update_date` / `update_datetime` when present;
  - if a later fetch sees changed update date or text hash, replace zones and raw JSON.

### Pseudonymisation

- Judilibre and DILA bulk are already pseudonymised public sources.
- Never join to non-public or non-pseudonymised data.
- Do not attempt identity reconciliation across DILA/Judilibre; match only legal identifiers (ECLI, numbers, date).
- Return upstream pseudonymised text fragments exactly as received, with provider provenance.

### Smallest Reviewable First Slice

1. Add v12 `decision_zones` migration and storage helpers.
2. Add `PisteClient::judilibre_search_params()` and `judilibre_decision()`.
3. Add `fetch --part <motivations|moyens|dispositif> --online` for `cass` decisions with valid ECLI:
   - cache hit;
   - ECLI search;
   - `/decision`;
   - zone offset validation;
   - official response.
4. Keep `visa`, CAPP/INCA without ECLI, and JADE as fallback with clear provenance.
5. Add tests with mocked Judilibre JSON:
   - multi-fragment zones;
   - invalid offsets;
   - ECLI mismatch rejection;
   - cached response used without network;
   - `zone_accurate=true` only on official returned parts.

Main pitfalls:

- Judilibre provider `id` is not the same as DILA `JURITEXT`; search first, then cache provider ID.
- ECLI is the safest resolver; pourvoi/number needs date validation to avoid collisions.
- JADE is administrative and should not be silently sent to Judilibre.
- Upstream text may differ from local bulk text; official zone offsets apply to upstream `text`, not necessarily local `documents.body`.
- Offset units may not be Rust byte offsets; validate before slicing.
- Do not let a transient API failure turn a normal offline `fetch` into an error; fallback provenance is the correct behavior.

## Priority

1. **First implement authority score plumbing with default `0.0` and score exposure.** It is low-risk and creates the tuning surface without changing live behavior.
2. **Then implement lazy Judilibre zones for `fetch --part motivations --online` on ECLI-resolved Cassation decisions.** That delivers immediate user-visible value while keeping bulk provenance honest.
3. **Only after both are observable, add advisory evals/tuning and broaden zones to `moyens`/`dispositif`, then CAPP/INCA ECLI cases.**
