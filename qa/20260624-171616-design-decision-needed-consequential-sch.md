# Q&A — 20260624-171616

## Question

Design decision needed (consequential: schema + new enrichment code + a from-scratch re-run). The user
redirected the zone rollout mid-run. Repo: /home/pierre/Work/jurisearch.

USER REQUIREMENT ("Both"):
1. DURABLY ARCHIVE all raw PISTE/Judilibre API responses for every decision touched (incl.
   not_found/unsupported/upstream_error), in a store that survives the decision_zones cache TTL/invalidation
   ("useful info we can use later"). Today the enrichment (`enrich_decision_from_judilibre_with_client`,
   crates/jurisearch-cli/src/main.rs ~4272) keeps the full Judilibre /decision JSON in
   `decision_zones.raw_json` ONLY for ok/invalid_offsets rows; it DISCARDS the /search response and stores
   empty raw_json for not_found/unsupported/upstream_error (via cache_zone_status_with_client ~4453).
2. ADD a Legifrance legislation-enrichment step: for each decision, resolve its cited legislation and
   persist the Legifrance API results too.

KEY CONTEXT (verify against source):
- Judilibre /decision response fields include: text, visa, themes, summary, rapprochements, solution,
  titlesAndSummaries, zones (recent only), id, numbers, etc.
- `visa` is an ARRAY of {title} where title is HTML embedding the cited article + code + a Legifrance
  search URL, e.g.:
  {"title":"Article <a href=\"https://www.legifrance.gouv.fr/search/code?...&query=609+code+de+proc%C3%A9dure+civile&...\">609</a> du code de procédure civile."}
  So a decision cites (article_number, code_name) pairs, MANY decisions cite the SAME article (e.g. art.
  609 CPC appears in thousands) -> naive per-decision Legifrance calls would be massively redundant.
- Official API client (crates/jurisearch-official-api/src/lib.rs): Judilibre via KeyId header
  (judilibre_search_params, judilibre_decision); Legifrance via OAuth2 client-credentials
  (legifrance_search(&mut self, body: &Value) -> Value; legifrance_bearer_token). Only `legifrance_search`
  exists (no consult/getArticle helper yet).
- The LEGI corpus is ALREADY ingested locally (documents/chunks have LEGI articles) — so the cited
  articles' TEXT may already be in the index; the Legifrance API would add the official upstream record.
- Storage: decision_zones is the TTL'd cache (migration v15 is current; CURRENT_SCHEMA_VERSION=15).
  Migrations append-only with index_manifest version upsert. Enrichment runs on a clone
  (phase2-full-juridic.zone-rollout-20260624); I'll wipe decision_zones/zone_units and re-run from scratch.
- Scale: ~494,701 resolver-reachable cass+inca decisions; recent ones ~96% zoned. Enrichment ~9 dec/s.

DESIGN I NEED FROM YOU (concrete, ordered):
1. Schema: shape of the durable raw-response archive. One table or two? Columns/PK/indexes. Should it FK
   documents (CASCADE) or be independent for durability? How to key /search vs /decision vs legifrance
   responses, and handle re-fetch (upsert vs append history)? Give the v16 (and v17 if needed) DDL,
   matching the repo's migration style (decision_zones at migrations.rs ~450 is the model).
2. Write path: how to wire archive writes into enrich_decision_from_judilibre_with_client +
   cache_zone_status_with_client so EVERY API response (search + decision, success or not) is persisted,
   without changing the existing decision_zones cache semantics.
3. Legifrance enrichment: concrete design. How to extract cited (article, code) from `visa` (parse the
   HTML/title, or the Legifrance URL query param?). Resolve UNIQUE citations once (dedup across decisions)
   — where to store the dedup keyset and the decision->citation links. Which Legifrance endpoint/body for
   `legifrance_search`. OAuth + rate-limit handling (separate from Judilibre's KeyId). Should we SKIP
   calling Legifrance when the article is already in the local LEGI index, or always fetch the upstream
   record? What exactly to persist.
4. Re-run strategy: given dedup, is the Legifrance pass a SEPARATE command/phase after enrich-zones
   (collect citations from archived /decision responses -> resolve unique -> persist), or inline per
   decision? Recommend the cleaner one.
5. Cost/scale + feasibility: rough extra API-call count and time for the Legifrance pass with dedup; any
   reason to push back on scope or stage it. Pitfalls (the standalone-index-mistake class).

Be concrete and ordered; I'll implement your design (with a follow-up review) then re-run.

## Answer

# Decision: Archive PISTE Responses + Deduped Legislation Enrichment

## Recommendation

Build **both**, but do it in two separated layers:

1. Add a durable, append-only official API response archive first, and wire it into Judilibre enrichment before re-running zones.
2. Add legislation enrichment as a **separate post-Judilibre phase** that extracts citations from archived Judilibre `/decision` responses, dedupes them, then calls Legifrance once per unique citation.

Do **not** inline Legifrance calls into `enrich-zones`. Inline would multiply calls by decision count, couple two upstreams with different auth/rate-limit behavior, and make the already-long zone pass harder to resume and reason about.

## 1. Schema

Use **one generic append-only table** for all raw official API traffic, plus **two legislation-specific tables** for deduped citation work. Keep the raw archive independent from `documents`: no FK, no cascade. `decision_zones` is a TTL cache; this archive is evidence/provenance and must survive cache invalidation, index repair, or document deletion.

### v16: durable official API archive

Add `CURRENT_SCHEMA_VERSION = 16`, append this migration after v15:

```sql
CREATE TABLE IF NOT EXISTS official_api_responses (
    response_id bigserial PRIMARY KEY,

    provider text NOT NULL CHECK (provider IN ('judilibre','legifrance','local')),
    api_environment text NOT NULL DEFAULT 'production',
    endpoint text NOT NULL,
    http_method text NOT NULL CHECK (http_method IN ('GET','POST','LOCAL')),

    -- Optional subject metadata. Deliberately no FK: this table is the durable archive.
    subject_document_id text,
    subject_source_uid text,
    provider_object_id text,
    citation_key text,

    request_fingerprint text NOT NULL,
    request_url text,
    request_json jsonb NOT NULL DEFAULT '{}'::jsonb,
    request_body text,

    outcome text NOT NULL CHECK (outcome IN (
        'ok',
        'not_found',
        'unsupported',
        'invalid_offsets',
        'upstream_error',
        'parse_error'
    )),
    http_status integer,

    -- Store exact body text for durability, plus parsed JSON for querying.
    response_body text NOT NULL DEFAULT '',
    response_json jsonb,
    response_body_sha256 text NOT NULL,

    error text,
    run_id text,
    code_version text,
    fetched_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS official_api_responses_subject_idx
ON official_api_responses (subject_document_id, fetched_at DESC);

CREATE INDEX IF NOT EXISTS official_api_responses_provider_request_idx
ON official_api_responses (provider, endpoint, request_fingerprint, fetched_at DESC);

CREATE INDEX IF NOT EXISTS official_api_responses_provider_object_idx
ON official_api_responses (provider, provider_object_id, fetched_at DESC)
WHERE provider_object_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS official_api_responses_citation_key_idx
ON official_api_responses (citation_key, fetched_at DESC)
WHERE citation_key IS NOT NULL;

INSERT INTO index_manifest(key, value, updated_at)
VALUES ('schema', jsonb_build_object('schema_version', 16), now())
ON CONFLICT (key) DO UPDATE
SET value = excluded.value,
    updated_at = excluded.updated_at;
```

Notes:

- This is append-only by default. Re-fetches produce new rows. That is the safest interpretation of "archive all raw responses".
- Do not put a GIN index on `response_json`; it will be huge and not needed for rollout.
- Store both `response_body` and `response_json`. `jsonb` is useful, but it is not byte-raw because it normalizes JSON object representation.
- For unsupported local decisions, insert a `provider='local'`, `endpoint='judilibre:unsupported-no-request'`, `http_method='LOCAL'` row so every touched decision has durable accounting even when no HTTP request was possible.

### v17: extracted legislation citations and deduped resolutions

Add v17 separately so the raw archive can be reviewed and landed first.

```sql
CREATE TABLE IF NOT EXISTS decision_legislation_citations (
    citation_occurrence_id text PRIMARY KEY,
    decision_document_id text NOT NULL REFERENCES documents(document_id) ON DELETE CASCADE,
    decision_source_uid text NOT NULL,
    source_response_id bigint NOT NULL REFERENCES official_api_responses(response_id),

    visa_index integer NOT NULL CHECK (visa_index >= 0),
    citation_key text NOT NULL,

    article_number_raw text,
    article_number_norm text NOT NULL,
    code_name_raw text,
    code_name_norm text NOT NULL,
    query_raw text NOT NULL,
    legifrance_url text,
    decision_date date,
    raw_title text NOT NULL,
    extraction_method text NOT NULL CHECK (extraction_method IN (
        'legifrance_url_query',
        'visa_title_regex'
    )),

    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (decision_document_id, visa_index, citation_key)
);

CREATE INDEX IF NOT EXISTS decision_legislation_citations_decision_idx
ON decision_legislation_citations (decision_document_id);

CREATE INDEX IF NOT EXISTS decision_legislation_citations_citation_key_idx
ON decision_legislation_citations (citation_key);

CREATE TABLE IF NOT EXISTS legislation_citation_resolutions (
    citation_key text PRIMARY KEY,
    article_number_norm text NOT NULL,
    code_name_norm text NOT NULL,
    canonical_query text NOT NULL,

    local_status text NOT NULL DEFAULT 'pending' CHECK (local_status IN (
        'pending',
        'resolved',
        'ambiguous',
        'not_found'
    )),
    local_document_id text REFERENCES documents(document_id) ON DELETE SET NULL,
    local_candidates_json jsonb NOT NULL DEFAULT '[]'::jsonb,

    legifrance_status text NOT NULL DEFAULT 'pending' CHECK (legifrance_status IN (
        'pending',
        'ok',
        'not_found',
        'upstream_error',
        'parse_error'
    )),
    legifrance_response_id bigint REFERENCES official_api_responses(response_id) ON DELETE SET NULL,
    legifrance_request_fingerprint text,

    fetched_at timestamptz,
    error text,
    resolution_schema_version text NOT NULL DEFAULT 'legislation-citation:v1',
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS legislation_citation_resolutions_status_idx
ON legislation_citation_resolutions (legifrance_status, local_status);

INSERT INTO index_manifest(key, value, updated_at)
VALUES ('schema', jsonb_build_object('schema_version', 17), now())
ON CONFLICT (key) DO UPDATE
SET value = excluded.value,
    updated_at = excluded.updated_at;
```

Do not store these as `graph_edges` initially. After the resolver is proven precise, exact local matches can optionally be materialized as derived `graph_edges` with a distinct `edge_source`, for example `judilibre_visa_legifrance`. Keep the first implementation separate and auditable.

## 2. Judilibre Write Path

Current source shape:

- `enrich_decision_from_judilibre_with_client` resolves by pourvoi/date, calls `judilibre_search_params`, then `judilibre_decision`, normalizes zones, and writes `decision_zones`.
- `cache_zone_status_with_client` writes negative TTL cache rows with empty `raw_json`.
- `decision_zones` currently stores full `/decision` JSON only for `ok` / `invalid_offsets`.

Change the official API client first. The current `PisteClient` methods return only `Value` on success and a mostly summarized `OfficialApiError` on error. That is not enough to archive raw upstream errors. Add lower-level exchange methods that return a response envelope:

```rust
pub struct OfficialApiExchange {
    pub provider: &'static str,
    pub endpoint: String,
    pub method: &'static str,
    pub request_url: String,
    pub request_json: serde_json::Value,
    pub request_body: Option<String>,
    pub request_fingerprint: String,
    pub http_status: Option<u16>,
    pub response_body: String,
    pub response_json: Option<serde_json::Value>,
    pub outcome: OfficialApiOutcome,
    pub error: Option<String>,
}
```

Add wrappers such as:

```rust
pub fn judilibre_search_params_exchange(
    &self,
    params: &[(&str, &str)],
) -> OfficialApiExchangeResult;

pub fn judilibre_decision_exchange(
    &self,
    provider_id: &str,
    query: Option<&str>,
) -> OfficialApiExchangeResult;

pub fn legifrance_search_exchange(
    &mut self,
    body: &Value,
) -> OfficialApiExchangeResult;
```

Then keep the existing `judilibre_search_params`, `judilibre_decision`, and `legifrance_search` as compatibility wrappers over the exchange methods.

Add `crates/jurisearch-storage/src/official_api_archive.rs` with:

```rust
pub struct InsertOfficialApiResponse<'a> {
    pub provider: &'a str,
    pub api_environment: &'a str,
    pub endpoint: &'a str,
    pub http_method: &'a str,
    pub subject_document_id: Option<&'a str>,
    pub subject_source_uid: Option<&'a str>,
    pub provider_object_id: Option<&'a str>,
    pub citation_key: Option<&'a str>,
    pub request_fingerprint: &'a str,
    pub request_url: Option<&'a str>,
    pub request_json: &'a Value,
    pub request_body: Option<&'a str>,
    pub outcome: &'a str,
    pub http_status: Option<i32>,
    pub response_body: &'a str,
    pub response_json: Option<&'a Value>,
    pub response_body_sha256: &'a str,
    pub error: Option<&'a str>,
    pub run_id: Option<&'a str>,
    pub code_version: Option<&'a str>,
}

pub fn insert_official_api_response_with_client<C: postgres::GenericClient>(
    client: &mut C,
    row: &InsertOfficialApiResponse<'_>,
) -> Result<i64, StorageError>;
```

Wire it into `enrich_decision_from_judilibre_with_client` like this:

1. If there is no parser-valid pourvoi:
   - Insert a local archive row with `outcome='unsupported'`.
   - Call `cache_zone_status_with_client` unchanged.
2. For Judilibre `/search`:
   - Call `judilibre_search_params_exchange`.
   - Insert the archive row immediately, with `subject_document_id` and `subject_source_uid`.
   - If the exchange failed, cache `upstream_error`.
   - If it succeeded but no matching provider id is found, cache `not_found`; the `/search` JSON is now durably stored.
3. For Judilibre `/decision`:
   - Call `judilibre_decision_exchange`.
   - Insert the archive row immediately with `provider_object_id=provider_id`.
   - If it failed, cache `upstream_error`.
   - If it succeeded, normalize zones and keep the existing `decision_zones` behavior.
   - `decision_zones.raw_json` can still store the latest successful `/decision` JSON for cache convenience, but it is no longer the durable archive.

Do not make the archive insert part of `cache_zone_status_with_client`. Keep `cache_zone_status_with_client` as the TTL-cache writer. Put archive writes at the API call sites so it can store `/search`, `/decision`, unsupported local attempts, and later Legifrance responses without overloading the cache helper.

For consistency, return archive insert failures as hard enrichment errors. If archive is required, a decision is not "touched successfully" unless the response was persisted.

## 3. Legifrance Enrichment

### Extract citations from archived Judilibre decisions

Source of truth for this phase should be `official_api_responses`, not `decision_zones.raw_json`, because the archive survives TTL and includes all `/decision` responses.

Select archived successful Judilibre decision responses:

```sql
SELECT response_id, subject_document_id, subject_source_uid, response_json
FROM official_api_responses
WHERE provider = 'judilibre'
  AND endpoint = '/cassation/judilibre/v1.0/decision'
  AND outcome IN ('ok','invalid_offsets')
  AND response_json ? 'visa';
```

For each `response_json->'visa'` array element, use `title`.

Extraction priority:

1. Parse the Legifrance URL from the anchor `href`.
   - The CLI already depends on `url::Url`.
   - Extract `query` from the URL query parameters.
   - Example decoded query: `609 code de procédure civile`.
   - This is the best primary key because it is exactly what Judilibre embedded for Legifrance search.
2. Fallback to stripped title text and a conservative regex only when no usable URL query exists.
   - Example target text: `Article 609 du code de procédure civile.`
   - Do not chase broad free text. If the parser cannot extract both article number and code name, skip with a counted parse failure.

Normalize:

```text
article_number_norm = uppercase(article).replace spaces around dots/hyphens, collapse whitespace
code_name_norm = lowercase(strip_accents? optional), collapse whitespace, strip trailing punctuation
canonical_query = "{article_number_norm} {code_name_norm}"
citation_key = sha256("legi-citation:v1\0" || article_number_norm || "\0" || code_name_norm)
```

Do not include `decision_document_id` in `citation_key`. The point is cross-decision dedup. Keep `decision_date` on each occurrence because local LEGI version resolution depends on date.

### Resolve locally first, but still fetch Legifrance once

Use the existing local resolver shape as the precision filter:

```rust
CitationResolutionQuery {
    query: canonical_query,
    article_number: article_number_norm,
    code_hint: Some(code_name_norm),
    as_of: decision_date,
    kind_filter: Some("article"),
    limit: 5,
}
```

For `legislation_citation_resolutions`:

- `local_status='resolved'` only when exactly one strong local candidate is found for the occurrence's `as_of`, or when all sampled occurrence dates for the same `citation_key` resolve to the same `version_group`/article family under expected version changes.
- `ambiguous` if multiple plausible articles remain.
- `not_found` if the local LEGI index cannot resolve it.

Still call Legifrance once per unique `citation_key` even when local LEGI resolves. The requirement is to persist the official upstream result too; local resolution is for dedupe, validation, and later graph materialization, not a reason to skip upstream archival.

### Legifrance API body

The existing official API client only has:

```rust
PisteClient::legifrance_search(&mut self, body: &Value)
```

and it posts to:

```text
/dila/legifrance/lf-engine-app/search
```

The only source-proven request shape in the repo is the current online citation confirmation:

```json
{
  "query": "<citation query>",
  "pageSize": 1
}
```

Use that as the first implementation, with a slightly larger page:

```json
{
  "query": "<article_number_norm> <code_name_norm>",
  "pageSize": 5
}
```

Persist the full response in `official_api_responses` with:

- `provider='legifrance'`
- `endpoint='/dila/legifrance/lf-engine-app/search'`
- `http_method='POST'`
- `citation_key=<citation_key>`
- `request_json=<body>`
- `outcome='ok' | 'not_found' | 'upstream_error' | 'parse_error'`

Then update `legislation_citation_resolutions.legifrance_response_id`.

Do not add a consult/getArticle helper in the same first slice unless the search response clearly exposes a stable article id and the API client is extended under tests. The first goal is durable upstream evidence and deduped resolution, not perfect article consultation.

### Commands

Prefer one command with sub-phases, or two explicit commands:

```text
jurisearch ingest collect-legislation-citations --index-dir <clone>
jurisearch ingest enrich-legislation-citations --index-dir <clone> --concurrency 2 --limit ...
```

If one command:

```text
jurisearch ingest enrich-legislation-citations --index-dir <clone> --collect --resolve --concurrency 2
```

Keep it separate from `ingest enrich-zones`.

Implementation split:

1. `collect-legislation-citations`
   - Reads archived Judilibre `/decision` responses.
   - Extracts visa citations.
   - Upserts `decision_legislation_citations`.
   - Upserts `legislation_citation_resolutions` with `pending` rows.
   - No network.
2. `enrich-legislation-citations`
   - Pages `legislation_citation_resolutions WHERE legifrance_status IN ('pending','upstream_error' if expired/retry flag)`.
   - Calls Legifrance once per `citation_key`.
   - Archives each response in `official_api_responses`.
   - Updates the resolution row.

This gives you a cheap cardinality count before any Legifrance traffic:

```sql
SELECT count(*) AS occurrences,
       count(DISTINCT citation_key) AS unique_citations
FROM decision_legislation_citations;
```

## 4. Re-run Strategy

Use this sequence on the clone:

1. Land v16 + archive write path.
2. Wipe current zone rollout scratch data:
   - `TRUNCATE zone_unit_embeddings, zone_units, decision_zones RESTART IDENTITY CASCADE;`
   - Also truncate `official_api_responses` only on the throwaway clone if you want a clean evidence run.
3. Re-run `ingest enrich-zones --source cass --order recent ...` and `--source inca ...`.
   - This now archives every Judilibre `/search`, `/decision`, negative response, and unsupported local attempt.
4. Land v17 + legislation commands.
5. Run `collect-legislation-citations`.
6. Inspect occurrence and unique counts.
7. Run `enrich-legislation-citations` with conservative Legifrance concurrency.
8. Only after that, build/embed zone units and evaluate zone retrieval.

Do not promote an index whose raw archive was only partly wired. If the user requirement is "all raw responses for every decision touched", this has to be true before the from-scratch re-run starts.

## 5. Cost, Scale, and Feasibility

Judilibre archive cost:

- No extra API calls.
- Extra storage is significant because `/decision` includes full text. Expect many GB, possibly tens of GB depending on coverage and text length.
- PostgreSQL TOAST will compress large `text`/`jsonb`; avoid JSON GIN indexes.

Legifrance enrichment cost with dedupe:

- Naive per-decision calls are unacceptable: hundreds of thousands to millions of repeated calls.
- Deduped calls should be closer to unique `(article, code)` citations. I would expect low tens of thousands, not 494k times citations, but measure with `collect-legislation-citations` before calling the API.
- At 2 req/s, 20k unique citations is about 3 hours; at 5 req/s it is about 1.1 hours. Start conservative because Legifrance OAuth/API quota is separate from Judilibre KeyId behavior.

Use separate env knobs:

```text
JURISEARCH_LEGIFRANCE_CONCURRENCY=2
JURISEARCH_LEGIFRANCE_MAX_RETRIES=...
JURISEARCH_LEGIFRANCE_RETRY_BASE_MS=...
```

The existing `PisteClient` token cache is mutable, so either run Legifrance sequentially with one client or give each worker its own `PisteClient`. Do not share one mutable client across threads.

## Main Pitfalls

- **Current official client loses raw error bodies.** Fix this before claiming archive completeness.
- **`decision_zones.raw_json` is not an archive.** It is latest-cache state and should remain so.
- **Unsupported decisions still need durable touched accounting.** Insert local archive rows even when no PISTE request is made.
- **Do not FK the raw archive to `documents`.** Durability beats relational cleanliness here.
- **Do not Legifrance-call per visa occurrence.** Always dedupe by normalized citation key first.
- **Do not trust HTML text over URL query when the URL is present.** The embedded Legifrance query is the most stable first extraction target.
- **Do not materialize graph edges until precision is measured.** Store occurrence/resolution tables first; graph promotion can be a later, explicit step.
- **Avoid the standalone-index mistake.** Every command must require the target `--index-dir`, report it in JSON output, and run only on the clone until the complete archive/enrichment/eval sequence is validated.

## Concrete First Slice

1. v16 `official_api_responses`.
2. `official_api_archive.rs` insert helper.
3. Official API exchange methods that preserve raw body text on success and failure.
4. Wire archive writes into Judilibre search/decision/unsupported paths.
5. Re-run a tiny clone smoke and verify:
   - one `/search` archive row per resolver attempt,
   - one `/decision` archive row when a provider id resolves,
   - durable rows for `not_found`, `unsupported`, and `upstream_error`,
   - `decision_zones` semantics unchanged.

Only then add v17 and the Legifrance pass. This keeps the consequential schema/code change reviewable and prevents another long run that still discards useful upstream evidence.
