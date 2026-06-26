# Phase 2 jurisprudence benchmark design

## Recommendation

Build a new first-class command, `jurisearch eval france-juris` or `jurisearch eval phase2`, mirroring `eval france-legi`.

Do **not** use a lighter `eval run` wrapper over generated qrels as the release artifact path. `eval run` is useful for generic retrieval, but the Phase 2 gate requires a very specific artifact shape, identifier-family citation accuracy, production provenance, and category floors. The honest design is:

1. storage extractor: `france_juris_gold_json(...)` derives gold from the built official-source index;
2. runner: open the target index once, verify query readiness once, run qrels through the same production search/cite functions;
3. artifact assembler: emit a `phase2_france_juris_benchmark` JSON artifact matching `phase2_benchmark_artifact_errors`;
4. status gate: point `JURISEARCH_PHASE2_BENCHMARK` at that artifact and let the existing gate re-derive pass/fail.

This is the right precedent: `eval_france_legi_payload` already does exactly this for Phase 1, using `france_legi_gold_json` plus `search_with_postgres`, then assembling a gate artifact with provenance.

## Must-fix before the benchmark can be honest

Current HEAD still has a production search bug for decisions:

- `search_payload` rejects `--kind decision` with the old message “Phase 0.6 search currently supports ... LEGI subset”.
- `search_with_postgres` maps `LegalKind::Code` to `Some("article")`, but maps both `Decision` and `All` to `None`. So an internal eval using `LegalKind::Decision` would actually search the full mixed corpus unless decision filters happen to force `d.kind='decision'`.

Fix this before generating a passing artifact:

```rust
let kind_filter = match kind {
    LegalKind::Code => Some("article"),
    LegalKind::Decision => Some("decision"),
    LegalKind::All => None,
};
```

Remove the `search_payload` and `compare_payload` `LegalKind::Decision` rejection. Otherwise the benchmark can only be “production-ish” through a private path, while the actual CLI production path cannot search decisions.

## Gold methodology

The gold should be official-source-derived, deterministic, and disclosed as `human_in_gold=false`, `llm_in_gold=false`, `sampled=false`. “Not sampled” here should mean “deterministic bounded extraction with recorded caps/order”, as in the Phase 1 artifact.

### Retrieval categories

Do **not** use decision title/citation as the retrieval query for `judicial_retrieval` or `administrative_retrieval`. Titles often contain exact court/date/number strings and make the task closer to citation lookup than semantic retrieval. They also have duplicates.

Use source-authored summary/headnote text instead:

- Judicial (`cass/capp/inca`): use `chunks.chunk_kind='decision_summary'`, built from `TEXTE/SOMMAIRE`.
- Administrative (`jade`): same; the unified index has many JADE `decision_summary` chunks.
- Gold: the containing `documents.document_id`.
- Query: a cleaned excerpt of the summary chunk body, not contextualized text. Strip or avoid obvious identifiers (`ECLI:...`, `JURITEXT...`, `CETATEXT...`, normalized pourvoi-like numbers) if present. Cap around 300-600 chars.
- Search mode: production `search_with_postgres` using `CliSearchMode::Hybrid`, `LegalKind::Decision`, `group_by=Document`, `top_k=10`, query readiness already checked once, shared `PreparedQueryEmbedder`.
- Scoring: `recall_at_10 = hits / queries`, hit if the expected `document_id` is in the top 10 unique document results.

This is meaningful but achievable: it asks “given the official abstract/headnote of what the case is about, can the system retrieve the case?” It is not a pure identifier lookup, and it does not require human-labeled fact-pattern questions. The floor is only 0.5, which is appropriate for a first official-evidence semantic gate.

Use deterministic extraction, for example:

```sql
SELECT jsonb_agg(jsonb_build_object(
  'query', query,
  'gold_document_id', document_id,
  'source', source
) ORDER BY document_id)
FROM (
  SELECT d.document_id, d.source,
         left(regexp_replace(c.body, '\s+', ' ', 'g'), 500) AS query
  FROM documents d
  JOIN chunks c ON c.document_id = d.document_id
  WHERE d.kind = 'decision'
    AND d.source IN ('cass', 'capp', 'inca') -- or source='jade'
    AND c.chunk_kind = 'decision_summary'
    AND length(c.body) BETWEEN 120 AND 2000
    AND d.valid_from <= CURRENT_DATE
  ORDER BY d.document_id
  LIMIT $limit
) q;
```

For JADE, do the same with `d.source='jade'`. Do not fallback to opening “Vu l’ordonnance...” body text for the release gate unless summary coverage is too small; it is noisier and can create duplicate boilerplate queries. Current checked index has enough summaries for both families.

### Citation categories

These should exercise `cite_payload` / `citation_lookup_json`, not search. Correctness is: the local citation result state is acceptable and at least one returned match resolves to the expected decision document.

Use:

- `ecli`: `canonical_json->>'ecli'`, but only values matching a real ECLI shape, e.g. `upper(ecli) ~ '^ECLI:FR:'`. Some rows currently have bad `ecli`-field captures such as chamber/person strings; do not include those in gold.
- `pourvoi`: `canonical_json->'case_numbers'`, but only values accepted by the production parser `parse_pourvoi`: normalized form `^[0-9]{2}-[0-9]{4,6}$` after removing dots/spaces. Prefer `source='cass'` for the “pourvoi” category; CAPP/INCA `NUMERO_AFFAIRE` values can look parseable but are not necessarily pourvois in the Cassation sense.
- `cetatext`: `source_uid`, for `source='jade' AND source_uid ~ '^CETATEXT[0-9]{12}$'`.

Gold shape:

```json
{ "query": "ECLI:FR:CCASS:2025:AP00683", "gold_document_id": "cass:JURITEXT000051824029" }
```

For each qrel, call the same local citation path as CLI `cite`:

- `cite_payload(CiteArgs { cite: query, strict: false, online: false, as_of: None }, index_dir)`
- `ecli` expected state: `exact`
- `cetatext` expected state: `exact`
- `pourvoi` expected state: `normalized` or `exact`; current code classifies unique pourvoi matches as `normalized`
- success: `matches[*].document_id` contains `gold_document_id`
- failure: no match, wrong document, or ambiguous where the expected document is not returned

The v11 indexes directly support these paths:

- ECLI: partial expression index `documents_decision_ecli_idx`
- pourvoi/NUMERO_AFFAIRE: `jurisearch_normalized_case_numbers(canonical_json)` plus `documents_decision_case_numbers_idx`
- CETATEXT/source UID: existing document/source UID lookup path

## Storage extractor shape

Add `crates/jurisearch-storage/src/france_juris.rs` with:

```rust
pub struct FranceJurisGoldLimits {
    pub judicial_retrieval: u32,
    pub administrative_retrieval: u32,
    pub ecli: u32,
    pub pourvoi: u32,
    pub cetatext: u32,
}

impl Default for FranceJurisGoldLimits {
    fn default() -> Self {
        Self {
            judicial_retrieval: 60,
            administrative_retrieval: 60,
            ecli: 30,
            pourvoi: 30,
            cetatext: 30,
        }
    }
}

pub fn france_juris_gold_json(
    postgres: &ManagedPostgres,
    limits: FranceJurisGoldLimits,
) -> Result<String, StorageError>
```

Return:

```json
{
  "judicial_retrieval": [
    {"query": "...", "gold_document_id": "cass:JURITEXT...", "source": "cass"}
  ],
  "administrative_retrieval": [
    {"query": "...", "gold_document_id": "jade:CETATEXT...", "source": "jade"}
  ],
  "decision_citation": {
    "ecli": [
      {"query": "ECLI:FR:...", "gold_document_id": "cass:JURITEXT..."}
    ],
    "pourvoi": [
      {"query": "22-21.812", "gold_document_id": "cass:JURITEXT..."}
    ],
    "cetatext": [
      {"query": "CETATEXT000...", "gold_document_id": "jade:CETATEXT..."}
    ]
  }
}
```

For pourvoi display, preserve the stored string if it already includes a dot; otherwise the normalized `22-21812` is fine because `parse_pourvoi` accepts both dotted and plain forms. If you want to exercise normalization specifically, synthesize the dotted form only when the right-hand side has 5 digits: `22-21812 -> 22-21.812`.

## Runner and artifact

Add an eval subcommand next to `FranceLegi`:

```rust
#[derive(Debug, Clone, Args)]
struct EvalFranceJurisArgs {
    #[arg(long, default_value_t = 60)]
    judicial_retrieval: u32,
    #[arg(long, default_value_t = 60)]
    administrative_retrieval: u32,
    #[arg(long, default_value_t = 30)]
    ecli: u32,
    #[arg(long, default_value_t = 30)]
    pourvoi: u32,
    #[arg(long, default_value_t = 30)]
    cetatext: u32,
    #[arg(long)]
    source_revision: Option<String>,
    #[arg(long)]
    out: Option<PathBuf>,
}
```

Runner steps:

1. `require_existing_index_dir`
2. `open_index`
3. `ensure_query_readiness(&postgres, QueryReadinessGate::Search)`
4. `france_juris_gold_json(&postgres, limits)`
5. build one `PreparedQueryEmbedder`
6. run retrieval qrels through `search_with_postgres` with:
   - `RetrievalMode::Hybrid`
   - `LegalKind::Decision`
   - `CliKind::Decision`
   - `CliGroupBy::Document`
   - `top_k=10`
   - `verify_readiness=false`
   - shared embedder
7. run citation qrels through a helper that calls the same citation lookup path as `cite_payload`
8. compute metrics
9. assemble artifact
10. write `--out` and print JSON like `eval france-legi`

Artifact must contain the exact category names and metric names consumed by `phase2_benchmark_artifact_errors`:

```json
{
  "schema_version": 1,
  "kind": "phase2_france_juris_benchmark",
  "state": "passed",
  "jurisdiction": "france",
  "fingerprint": "bge-m3:1024:normalize:true",
  "categories": {
    "judicial_retrieval": {
      "metric": "recall_at_10",
      "value": 0.0,
      "queries": 60,
      "routing_backends": {"hybrid": 60}
    },
    "administrative_retrieval": {
      "metric": "recall_at_10",
      "value": 0.0,
      "queries": 60,
      "routing_backends": {"hybrid": 60}
    },
    "decision_citation": {
      "metric": "decision_citation_accuracy",
      "by_identifier": {
        "ecli": {"metric": "decision_citation_accuracy", "value": 1.0, "queries": 30},
        "pourvoi": {"metric": "decision_citation_accuracy", "value": 1.0, "queries": 30},
        "cetatext": {"metric": "decision_citation_accuracy", "value": 1.0, "queries": 30}
      }
    }
  },
  "provenance": {
    "official_source": "DILA LEGI + CASS/CAPP/INCA/JADE bulk XML (Licence Ouverte), extracted from the built index",
    "pipeline": "production",
    "code_version": "ecf3830c0f56",
    "index_revision": "...",
    "source_revision": "...",
    "qrel_selection": "deterministic_bounded_by_document_id_from_official_index_fields",
    "qrel_limits": {
      "judicial_retrieval": 60,
      "administrative_retrieval": 60,
      "ecli": 30,
      "pourvoi": 30,
      "cetatext": 30
    },
    "sampled": false,
    "human_in_gold": false,
    "llm_in_gold": false
  },
  "evidence": [
    "France jurisprudence runner over /mnt/models/jurisearch-index/phase2-full-juridic: ... qrels through production search/cite"
  ]
}
```

The `state` can be emitted for humans, but the existing gate correctly ignores it and re-derives pass/fail from fields and floors.

## Index revision

Do not use only the directory basename as `index_revision` for this Phase 2 artifact. The index was merged, and the gate is about the exact combined corpus.

Use a deterministic lightweight revision string derived from the built index state, for example:

- `phase2-full-juridic:schema11:docs2880961:chunks4701354:embbge-m3:1024:normalize:true`
- plus source versions from `corpus_sources`: `legi=20250713-140000,cass=20251201-212627,capp=20251117-210231,inca=20251201-212627,jade=20251205-211816`

Better: add a storage helper that hashes stable manifest facts:

```sql
SELECT md5(jsonb_build_object(
  'schema', (SELECT value FROM index_manifest WHERE key='schema'),
  'embedding', (SELECT value FROM index_manifest WHERE key='embedding'),
  'sources', (SELECT jsonb_object_agg(source, manifest->>'source_version') FROM ingest_run WHERE status='completed'),
  'counts', jsonb_build_object(
    'documents', (SELECT count(*) FROM documents),
    'chunks', (SELECT count(*) FROM chunks),
    'embeddings', (SELECT count(*) FROM chunk_embeddings)
  )
)::text);
```

This is much cheaper than a full replay snapshot and is enough for benchmark provenance. If a cached `replay_snapshot` is available and fresh, recording its signature as `index_revision` is also good, but do not force `status --deep` just to run the eval on a 4.7M chunk index.

## Pitfalls

- **Decision search is not actually wired yet in `search_payload`.** Fix this first.
- **Internal eval must not bypass the kind bug.** After fixing `kind_filter`, the eval should assert all returned retrieval candidates have `kind='decision'`.
- **Bad ECLI captures exist.** Filter ECLI qrels by shape (`^ECLI:FR:`), not merely non-empty `canonical_json.ecli`.
- **Case numbers are not all pourvois.** For the pourvoi category, prefer `source='cass'` and parser-compatible `^[0-9]{2}-[0-9]{4,6}$` normalized values.
- **Duplicate/empty titles.** Avoid titles for retrieval gold; use summary chunks.
- **Boilerplate administrative openings.** Use `decision_summary` chunks for JADE retrieval rather than first body chunks where possible.
- **Hybrid query embeddings.** `eval france-juris` needs the same embedding runtime setup as `eval france-legi`; build the embedder once.
- **No online Judilibre in the gate.** Current `cite --online` explicitly says online decision verification is not wired. The Phase 2 artifact should measure local production `cite` over the built official index.
- **Provenance booleans.** Set `sampled=false`, `human_in_gold=false`, `llm_in_gold=false` only if all qrels are extracted from official indexed fields with deterministic bounds.
- **Artifact path.** After writing the artifact, set `JURISEARCH_PHASE2_BENCHMARK=/path/to/artifact.json`; `status` will re-read and validate it.

## Confirmation sequence

After implementation:

```bash
cargo run -q -p jurisearch-cli -- eval france-juris \
  --index-dir /mnt/models/jurisearch-index/phase2-full-juridic \
  --judicial-retrieval 60 \
  --administrative-retrieval 60 \
  --ecli 30 \
  --pourvoi 30 \
  --cetatext 30 \
  --out work/03-implementation/02-evidence/2026-06-24-phase2-france-juris-benchmark.json
```

Then:

```bash
export JURISEARCH_PHASE2_BENCHMARK=$PWD/work/03-implementation/02-evidence/2026-06-24-phase2-france-juris-benchmark.json

cargo run -q -p jurisearch-cli -- status \
  --index-dir /mnt/models/jurisearch-index/phase2-full-juridic \
  | jq '.phase2_gate'
```

Expected:

- `phase2_gate.checks[].status` pass for `jurisprudence_eval_benchmark`;
- `phase2_gate.claim_allowed=true`;
- `phase2_gate.state="ready"`.

If retrieval recall misses the 0.5 floor with summary queries, do not weaken the gold by switching to citation/title queries. First inspect whether `--kind decision` filtering, BM25 summary indexing, RRF weights, and document grouping are behaving correctly. Title/citation known-item retrieval can be added as an advisory diagnostic, but it should not be the semantic retrieval category that opens the claim.

## Source-grounding notes

I checked:

- `crates/jurisearch-cli/src/main.rs`: Phase 2 gate/artifact validation, `eval_france_legi_payload`, `france_legi_artifact`, `search_payload`, `search_with_postgres`, `cite_payload`, citation parsing.
- `crates/jurisearch-storage/src/france_legi.rs`: Phase 1 official-evidence gold extraction pattern.
- `crates/jurisearch-storage/src/citation.rs`: decision source UID, ECLI, and pourvoi lookup semantics.
- `crates/jurisearch-storage/src/migrations.rs`: v11 decision citation indexes.
- `crates/jurisearch-ingest/src/juri/mod.rs`: `CanonicalDecision`, `case_numbers`, `ecli`, `summaries`, and heuristic decision chunking.
- `/mnt/models/jurisearch-index/phase2-full-juridic`: status shows query-ready unified index with `legi`, `cass`, `capp`, `inca`, `jade`, schema v11, 2,880,961 docs and 4,701,354 embedded chunks.
