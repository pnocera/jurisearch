# Corpus-wide authority-aware ranking for French jurisprudence search — DESIGN

- **Date:** 2026-06-24
- **Status:** DESIGN (concrete + buildable; no implementation diffs). Builds on the codex-GO'd
  analysis `05-ranking/2026-06-24-authority-aware-ranking-analysis.md` (reviews r1 FIXES_REQUIRED →
  r2 GO, `05-ranking/reviews/2026-06-24-ranking-analysis-codex-review-r1.md` / `-r2.md`). House style
  mirrors `04-zones/2026-06-24-zone-precise-retrieval-implementation-plan.md`.
- **Scope:** jurisprudence (`kind='decision'`) across `cass` / `inca` / `capp` / `jade`, in BOTH the
  main chunk/document path (`hybrid_candidates_json`) AND the zone overlay (`zone_candidates_json`).
- **Hard invariant (non-negotiable):** the default ranking is **byte-identical** unless an explicit
  knob is set. The Phase 2 gate is proven; this design must not move it.
- **Codebase audit:** the analysis's `file:line` citations were re-verified against current source
  while writing this design (the zone path `04-zones` plan has since shipped — commits
  `2ef7b42`..`c4a753b` — so `zone_retrieval.rs`, `--zone`, `ensure_zone_retrieval_readiness`, the
  session `zone` field, and the zone benchmark all exist and are cited live below). Schema head is
  **v17** (`migrations.rs:3`).

---

## 1. Decision summary (read this first)

| # | Decision | Choice |
|---|---|---|
| D1 | **Mechanism** | **(b) Post-SQL deterministic re-rank over a widened, knob-gated window**, default **OFF / inert** (byte-identical when unset). NOT (c) RRF-blend (edits proven score), NOT (e) LTR (no graded data). |
| D2 | **Authority scale** | Two **separate** per-order scales (judicial / administrative), each a small ordered integer tier from `source` (reliable) refined by `publication` (coarse). **No cross-order numeric mapping.** Authority is a **bounded tie-/near-tie re-rank within a relevance band**, never a global key. |
| D3 | **PBRI ladder** | **Deferred to a later phase, behind its own sub-knob.** v1 uses `source` + `canonical_json.publication` only. Backfill of Judilibre PBRI from `official_api_responses.response_json` is specified (§5, Phase R4) but OFF by default and Cassation-only. |
| D4 | **Where data lives** | v1: **expose `publication` in the candidate JSON** (a SELECT/projection change, no migration) and re-rank in Rust. A **projected column + expression index** (`documents.authority_publication`, migration **v18**) is specified but only needed if/when authority moves into SQL or is filtered at scale; v1 does not require it because the re-rank reads only the already-fetched window. |
| D5 | **Relevance dominance** | Authority may only reorder candidates whose relevance scores fall within a **relative band** `ε` of each other (default band chosen so the rule is *off* at `authority_weight=0`); a clearly-more-relevant result is never buried. |
| D6 | **Pagination** | The widened window + re-rank applies **only when effectively ON** (`rerank_on`, weight `> 0.0`), and is **FIRST-PAGE-ONLY**: ON ⇒ return the re-ranked `top_k` with `next_cursor = null` (deep paging unsupported for the experimental re-rank; no valid keyset cursor exists over a post-SQL re-ranked window — codex r1 BLOCKER). An inbound `--cursor` is rejected **only when `rerank_on`** (so `--authority-weight 0.0` + `--cursor` pages normally). When OFF (unset or `0.0`), today's `top_k+1` limit + `fused_score` cursor + full `next_cursor` are byte-identical. |
| D7 | **New eval metric** | **Pairwise authority-lift** (no human/LLM, measured-only): among **within-order** decision pairs that are BOTH in the same pre-rerank relevance band for a query AND both in the OFF widened window (a conservative "comparably relevant" rule — not mere term co-occurrence), the fraction where the higher-tier one ranks above the lower — emitted as a **separate measured-only** `phase2_authority_benchmark` artifact (with coverage + score-gap distribution), NOT a gate input. Plus the mandatory regression guard (Phase 2 + zone recall@10 ON vs OFF). |
| D8 | **Config** | One new field on `RetrievalOptions`: `authority_weight: Option<f64>` (default `None` ⇒ inert). Exposed as `--authority-weight` on `search` + `session search` + future `serve`, validated `[0.0, 1.0]`. **`authority_weight ≤ 0.0` is normalized to inert (treated as `None`)** so `--authority-weight 0.0` takes the OFF path exactly (legacy `top_k+1` limit, legacy `next_cursor`, no authority block) — i.e. `rerank_on ≡ effective_authority_weight.is_some_and(|w| w > 0.0)`; this keeps "weight 0 = byte-identical" honest and makes `0.0` a safe OFF baseline for eval/tune. **No env fallback in v1** (an env fallback would make `RetrievalOptions::default()` — used by eval/internal call sites — silently re-rank, breaking the unset-is-byte-identical invariant). **Requires `kind=decision`** (rejected for `code`/`all`; zone implies decision). A second optional `authority_band` knob is specified but defaults to a fixed constant in v1. |

**One-sentence justification (D1):** a default-OFF post-SQL re-rank is the only option that leaves the
gate-locked `fused_score` SQL expression byte-identical, is trivially eval-gated ON vs OFF, keeps main
and zone consistent through one shared Rust helper, and confines the cursor/window rework entirely to
the ON path — so the proven default is provably untouched.

---

## 2. Mechanism: chosen, justified, and rejected alternatives

### 2.1 Why (b) post-SQL re-rank

The analysis (§5) and its r1→r2 review establish the load-bearing constraint: there are **three pools**
— arm limits `top_k*4` / `top_k*20` (`main.rs:3691-3696`, zone `:3423-3424`), the final SQL output
`LIMIT = top_k+1` (`main.rs:3697`, zone `:3425`), and the returned `top_k` after Rust truncation
(`main.rs:3777-3785`, zone `:3468-3475`). A Rust re-rank as the code stands today sees only the page +
one sentinel; to be useful it must widen pool #2 and re-derive the cursor. Given that constraint, the
options compare:

| Option | Edits proven `fused_score` SQL? | Default byte-identical? | Cursor rework | Eval-ability | Verdict |
|---|---|---|---|---|---|
| (a) filter-only | no | yes | none | n/a | insufficient — relevance order still ignores authority; hard filter can exclude a perfect inédit |
| **(b) post-SQL re-rank (widened window)** | **no** | **yes (OFF restores `top_k+1`+cursor)** | **ON-path only** | **high (ON vs OFF)** | **CHOSEN** |
| (c) RRF-blend in `ranked` | **yes** (or weight=0 ⇒ effectively (b)-in-SQL) | only if weight=0 | none (still orders by score) | 3-way tune | rejected: highest isolation risk; entangles authority with calibrated RRF weights (`retrieval.rs:14-19`) |
| (d) strict tie-break | edits final ORDER BY + cursor in both paths | near-yes (pure ties rare) | both paths always | near-inert on recall | rejected as primary: pure ties are vanishingly rare under RRF; an ε-band tie-break is just (b) implemented in SQL with permanent ORDER-BY/cursor edits to the proven path |
| (e) LTR / cross-encoder | no (new stage) | yes if off | new | **blocked** (no graded data; risks human/LLM-in-gold) | rejected: premature |

(b) wins on the isolation invariant specifically: (c) and (d) must edit the exact SQL the gate
validated (the `fused_score` expression `retrieval.rs:636-646` or the final `ORDER BY`
`retrieval.rs:318`/`:370`, zone `:249`) in *both* paths — a permanent edit to proven code even when the
feature is off. (b) adds the re-rank as a **separable Rust layer** reached only when the knob is set;
when unset, `search_with_postgres` / `zone_search_payload` call exactly today's code path.

### 2.2 The mechanic, end to end (per request)

```
knob OFF (authority_weight = None)                 knob ON (authority_weight = w > 0)
──────────────────────────────────                ──────────────────────────────────
query_limit = top_k + 1                            query_limit = top_k * W            (W = rerank window factor, §4.4)
hybrid/zone_candidates_json                        hybrid/zone_candidates_json
   ORDER BY fused_score, id  (unchanged SQL)          ORDER BY fused_score, id  (UNCHANGED SQL — same expr)
candidates: [...] (≤ top_k+1)                       candidates: [...] (≤ top_k*W_eff)
truncate(top_k)                                     authority_rerank(candidates, w)   ← shared Rust helper
cursor = last row's fused_score cursor                 stable-sort by (banded authority_adjusted_score, id)
next_cursor = normal (deep paging)                  truncate(top_k)
                                                    next_cursor = null (FIRST-PAGE-ONLY, §4.5)
                                                    (inbound --cursor + authority ⇒ rejected)
```

The **SQL score expression is identical in both branches** — the only SQL difference on the ON path is
the value of `LIMIT` (a parameter, `query.limit`) and the inclusion of `source`+`publication` in the
projected JSON (which is a pure addition; see §4.2 for why adding fields is byte-safe for the OFF path).
All authority math lives in one Rust function.

---

## 3. Authority score model (precise)

### 3.1 Signals and their reliability (from analysis §2–§3.3, re-verified)

| Signal | Storage | Reliability | Coverage |
|---|---|---|---|
| `source` (`cass`/`inca`/`capp`/`jade`) | first-class column `documents.source` (`migrations.rs:29`), `zone_units.source` (`migrations.rs:505`); already in candidate JSON (`retrieval.rs:331`/`:384`, zone `:264`) | **reliable** | corpus-wide |
| `publication` | `canonical_json->>'publication'` (unindexed; `retrieval.rs:137-139`) | coarse, **source-dependent semantics** | judicial binary `oui`/`non`; administrative Lebon class letter (`C`); `capp` usually absent |
| PBRI `["b","r"]` | ONLY `official_api_responses.response_json` (v16, `migrations.rs:582-637`); not projected | rich but **Cassation-only**, unprojected | `cass`/`inca` Judilibre-reachable subset only |
| recency `valid_from` | real date column (`projection.rs:357`) | reliable | corpus-wide (decisions) |

### 3.2 Two separate ordered scales (D2)

**Honesty rule (analysis §2.3, §6):** never invent a tier a source lacks; `capp`/`jade` carry no
Bulletin/Rapport markers, so absence of `publie="oui"` must NOT be read as "low authority". The score
is therefore defined **per order**, and the two orders are **never compared on one number**.

**Judicial scale** `auth_j ∈ {0,1,2,3}` (ordre judiciaire — `cass`/`inca`/`capp`):

| Tier | Condition | Meaning |
|---|---|---|
| 3 | `source='cass'` AND `lower(publication)='oui'` | published Cour de cassation ("publié au Bulletin") |
| 2 | `source='cass'` AND NOT published | Cassation, publication flag absent/`non` |
| 1 | `source='inca'` | inédit Cassation (apex court, not a reference statement) |
| 0 | `source='capp'` | Cour d'appel (persuasive only) — **flagged `marker_absent`**, never penalized below by a missing Bulletin flag |

**Administrative scale** `auth_a ∈ {0,1,2}` (ordre administratif — `jade`), from the Lebon class letter
(`canonical_json.publication`, e.g. `"C"`; `tests.rs:194`):

| Tier | Condition | Meaning |
|---|---|---|
| 2 | Lebon class `A` (published "au Recueil") | apex administrative authority |
| 1 | Lebon class `B` ("mentionné aux tables") | intermediate |
| 0 | other class / absent (incl. CAA/TA) | lower; `marker_absent` when the letter is missing |

(`jade` is NOT exclusively Conseil d'État — the corpus sample is a `CAA de PARIS` decision,
`tests.rs:190` — so a class letter is the authority signal, not the `source` value alone.)

### 3.3 From tier to a bounded re-rank bonus

The re-rank does **not** add `auth` to the score. It computes a **normalized per-order authority
fraction** `a ∈ [0,1]` (judicial `auth_j/3`, administrative `auth_a/2`), then applies a **bounded,
relevance-dominant** adjustment to the fused score within a relevance band (§3.4). Cross-order
candidates are never ranked *against each other by authority* — a `jade` and a `cass` result are
ordered purely by relevance (their `a` values are not comparable), and authority only reorders
**within-order** neighbours that are already near-tied on relevance. This is the concrete realization of
"keep the orders on separate scales" without needing a cross-order mapping.

Honesty surfacing: each candidate gains an `authority` block in its JSON (§4.3) carrying `order`
(`judicial`/`administrative`), `tier`, `tier_max`, `signal` (`source+publication` in v1), and
`marker_absent: bool`. A client (or the eval) can therefore see *why* a result was/ wasn't boosted and
that `capp`/missing-letter rows were not penalized.

### 3.4 Relevance-dominance rule (D5) — the bound that prevents burying the best answer

The fused score `s` (rounded, as the cursor uses it) is the primary key. Authority may only reorder a
candidate `j` ahead of a more-relevant candidate `i` (`s_i > s_j`, same order) when they are in the same
**relevance band**:

```
in_band(i, j)  ≡  (s_i - s_j) ≤ band * s_i           band = authority_band (default 0.05, i.e. 5%)
adjusted(c)    =  s_c * (1 + authority_weight * a_c)  computed only for tie-break ordering within a band
```

Operationally the helper performs a **stable sort** on `(round(s,8), then within an ε=band cluster by
adjusted desc, then id)`. Because the band is a fraction of the leader's score, a clearly-more-relevant
result (outside the band) is never overtaken — satisfying analysis §2.3 ("a highly on-point inédit can
and should beat an off-topic published decision"). The helper is a defensive no-op at `weight = 0`
(`adjusted = s`), but more importantly **`authority_weight ≤ 0.0` is normalized to inert at the threading
layer (§4.4/§4.6), so `0.0` never even enters the ON path** — it runs today's exact code (the inertness
proof, §6.1). Recency (`valid_from`) is **deferred** to a sub-knob (D-open-1) and not folded into `a` in v1.

---

## 4. Mechanics in code (where it plugs in)

All anchors verified against current source.

### 4.1 Shared re-rank helper (keeps main + zone consistent — analysis §3.4, §5(b))

New free function in `crates/jurisearch-storage/src/` (a small `authority.rs`, or `pub(crate)` in
`retrieval_common.rs` alongside the helpers the zone work already extracted):

```rust
/// Per-order authority tier from the reliable `source` axis refined by the coarse `publication`
/// marker. Returns None for non-decisions / unknown source (caller leaves order untouched).
pub fn authority_tier(source: &str, publication: Option<&str>) -> Option<AuthorityTier>;

pub struct AuthorityTier {
    pub order: AuthorityOrder,   // Judicial | Administrative
    pub tier: u8,                // 0..=tier_max
    pub tier_max: u8,            // 3 (judicial) | 2 (administrative)
    pub marker_absent: bool,     // honesty flag (e.g. capp / missing Lebon letter)
}

/// Stable, deterministic re-rank of an ALREADY-RELEVANCE-SORTED candidate window.
/// - `weight == 0.0` (or None upstream) ⇒ no-op (returns input order unchanged): inertness.
/// - reorders only within a relative `band` of the local relevance leader, within the same order.
/// - reads each candidate's `scores.rrf`, `source`, `publication`; pure function of those fields.
pub fn authority_rerank(candidates: &mut [Value], weight: f64, band: f64);
```

`authority_rerank` is called from **both** payload builders so the layer is provably identical across
paths. It operates on `serde_json::Value` candidate objects (the shape both `hybrid_candidates_json` and
`zone_candidates_json` already emit), so it needs no SQL knowledge.

### 4.2 Candidate-projection change (expose `publication` in BOTH paths)

`source` is already projected; `publication` is not. Add it to the candidate JSON in three SQL builders:

- **chunk grouping** `retrieval.rs:328-336` (the `jsonb_build_object` after `'source', source`),
- **document grouping** `retrieval.rs:380-389`,
- **zone** `zone_retrieval.rs:260-269`.

Add `d.canonical_json->>'publication' AS publication` to the corresponding `limited`/`scored` SELECTs
(`retrieval.rs:310-313`/`:351-356`, zone `:230-235`) and a `'publication', publication` key in the
`jsonb_build_object`. **Byte-safety for the OFF path:** the analysis's isolation invariant is "default
*ranking* byte-identical" — adding a field to the candidate JSON changes the *payload shape*, not the
*order*. To keep even the payload byte-identical when OFF, gate the projection on the **effective** weight,
not raw flag presence: `project_authority = rerank_on` (i.e. `effective_weight.is_some()`, weight `> 0.0`)
— passed as a `bool project_authority` into the query struct; when false the SQL is exactly today's. In
particular `--authority-weight 0.0` has `rerank_on = false`, so it does NOT project `publication` and is
byte-identical (codex r3 WARN). This makes the OFF response byte-for-byte what it is today, which the gate's
recorded-fields re-derivation (`main.rs:10712-10797`) and any golden-output test will confirm. (If a
reviewer prefers always-on projection for simplicity, that is acceptable only if a golden test of the
default `search` JSON is updated in the same diff and the gate's recorded fields are unaffected — but
the gated projection is the safer default and is what this design specifies.)

### 4.3 The `authority` block added per candidate (ON path only)

When the re-rank runs, it annotates each returned candidate:

```json
"authority": {
  "order": "judicial",
  "tier": 3, "tier_max": 3,
  "signal": "source+publication",
  "marker_absent": false,
  "applied_weight": 0.25
}
```

This is additive and only present when the knob is ON, so the OFF response is unchanged.

### 4.4 Widened window + truncation (the pool-#2 fix)

In `search_with_postgres` (`main.rs:3691-3697`) and `zone_search_payload` (`main.rs:3422-3425`):

```
// `0.0` (and any ≤0) normalizes to inert, so it takes the OFF path exactly (not a degenerate ON path).
let effective_weight = options.authority_weight.filter(|w| *w > 0.0);   // both paths
let rerank_on = effective_weight.is_some();
let window_factor = if rerank_on { AUTHORITY_RERANK_WINDOW } else { 1 };   // W, e.g. 8
let query_limit = if rerank_on {
    args.top_k.saturating_mul(window_factor).saturating_add(1)
} else {
    args.top_k.saturating_add(1)                              // EXACTLY today (incl. for --authority-weight 0.0)
};
```

- `W = AUTHORITY_RERANK_WINDOW` (const, default **8**) bounds the re-rank window. It must satisfy
  `top_k * W ≤ top_k * pool_multiplier` (4 chunk / 20 doc / 20 zone) so the window never exceeds the arm
  pool feeding RRF (analysis §5(b) caveat: the re-rank can only resurface a doc already inside the fused
  candidate set). With `W=8` and chunk `pool_multiplier=4`, **chunk grouping caps `W` at 4**; document
  and zone grouping allow up to 20. The const is therefore clamped per-grouping:
  `W_eff = min(W, pool_multiplier)`.
- After `hybrid/zone_candidates_json` returns, ON path: `authority_rerank(candidates, w, band)` then
  `candidates.truncate(top_k)` (replacing the bare truncate at `main.rs:3777-3785` / `:3468-3475`). OFF
  path: the existing truncate runs unchanged.

### 4.5 Pagination contract (D6 — the pagination trap, analysis §6, §8.5)

Today the cursor encodes the rounded `fused_score` and pages **strictly in SQL order**: chunk resumes
`round(fused_score,8) < score OR (= AND chunk_id > id)`; document/zone resumes `cursor_score < score OR
(= AND document_id > id)` (`retrieval.rs:536-567`; cursor emit `:335`/`:388`, zone `:269`; parser
`main.rs:11694-11755`). A post-SQL re-rank reorders *displayed* rows away from SQL order, so **no single
`(score, id)` keyset cursor can represent a re-ranked window** — it cannot encode the set of
displayed-but-SQL-later rows nor the set of fetched-but-not-displayed rows, so it would skip/duplicate
(codex r1 BLOCKER, worked example: `top_k=2`, SQL `A,B,C,D`, displayed `C,A` ⇒ a fence on `A` re-fetches
and duplicates `C`). A "look-back of `band`" does not fix this: the existing predicate only fetches rows
*after* the cursor, and an in-window permutation needs the cursor to carry per-row state.

**v1 decision (D6): authority re-rank is FIRST-PAGE-ONLY. When authority is ON, the response returns the
re-ranked top_k and `pagination.next_cursor = null`, explicitly labeling deep paging unsupported for the
experimental re-rank.** Rationale: the whole value of authority ranking is "is the most authoritative
*relevant* result near the top" — that is a first-page question; the eval metric (§7) measures the top
window, not page 2. This is correct by construction (no invalid keyset is ever emitted) and minimal-risk.

- The response carries `pagination.cursor_note` explaining authority paging is first-page-only in v1
  (mirroring the existing pagination-note surface the zone path already uses), and a `routing` flag
  `authority_paging:"first_page_only"`.
- A request that combines an **effectively-ON** authority weight (`rerank_on`, i.e. `> 0.0`) with an
  INBOUND `--cursor` is rejected with a clear `bad_input` (you cannot page INTO an authority-re-ranked
  result set in v1). `--authority-weight 0.0` + `--cursor` is NOT rejected — it pages normally on the
  legacy path (codex r3 WARN).
- **Inertness:** when OFF, paging is byte-identical to today (existing `fused_score` cursor, full
  `next_cursor`); the first-page-only contract only applies on the explicit ON path. No `auth:` cursor
  tag is introduced, so the legacy parser (`main.rs:11694-11755`) is untouched (resolves the r1 NIT).

**Deferred (future phase, only if deep authority paging is demanded):** either (B) a stateful authority
cursor carrying a page origin + a bounded displayed-id set (with tests for promoted rows later in SQL
order), or (C) move the authority ordering into SQL so one total order drives both `ORDER BY` and the
keyset predicate (this is option (c)/migration-v18 territory, no longer the low-risk post-SQL design).
Neither is needed for v1; both are out of scope and explicitly flagged in §10.

### 4.6 Config threading and routing

- `RetrievalOptions` (`retrieval.rs:63-68`) gains `authority_weight: Option<f64>` (and, if the band is
  exposed, `authority_band: Option<f64>`). **No env fallback in v1** (unlike the RRF helpers): an
  `effective_authority_weight` that read `JURISEARCH_AUTHORITY_WEIGHT` would make
  `RetrievalOptions::default()` — used by eval helpers and many internal call sites (`main.rs:2628-2646`,
  `retrieval.rs:63-68`) — re-rank with NO request field set, contradicting the "unset ⇒ byte-identical"
  invariant. So v1 reads `authority_weight` **only** from the request field; the invariant is precisely
  "`effective_authority_weight == None` ⇒ inert", and the byte-identical golden tests (§6.2) run with
  `JURISEARCH_AUTHORITY_WEIGHT` absent. (A deployment env knob can be added later only if it is formally
  defined as an explicit ON switch and the golden tests are adjusted accordingly — codex r1 WARN.)
- `SearchArgs` (`main.rs:296-349`) gains `--authority-weight` (and the same on `SessionSearchArgs`
  `:474-507`, which already carries `zone`); `retrieval_options()` (`:399-405`) maps it;
  `session_search_payload` (`:5242-5270`) already rebuilds the field-by-field args and will carry it.
- **Kind gating (decision-only).** Authority is jurisprudence-only. The main `search` surface can run
  `kind=code`/`all` (`search_with_postgres` kind filter `main.rs:3682-3687`); an **effectively-ON**
  authority weight (`rerank_on`, `> 0.0`) with an effective kind other than `decision` is **rejected with
  `bad_input`** (mirroring how `--zone` rejects `--kind code`); `--authority-weight 0.0` is inert and not
  rejected. The zone path always implies decisions. As a defense-in-depth, `authority_tier`
  returns `None` for non-decisions, and when authority is rejected/inert the OFF `top_k+1` limit + legacy
  cursor path runs unchanged — a jurisprudence knob never alters statute/all paging.
- `validate_retrieval_options` (`:420-439`) adds a `[0.0, 1.0]` range check for `authority_weight` and a
  `(0.0, 0.5]` check for `authority_band` (a `bad_input`, not a clamp — matching the existing style).
  `0.0` is **valid but normalized to inert** (`effective_weight = authority_weight.filter(|w| *w > 0.0)`),
  so `--authority-weight 0.0` is a clean OFF baseline rather than a degenerate ON path (codex r2 WARN).
- Both `search_with_postgres` and `zone_search_payload` derive `rerank_on = effective_weight.is_some()`
  (i.e. `is_some_and(|w| w > 0.0)`); the re-rank applies corpus-wide and consistently (same helper) —
  satisfying the corpus-wide + main/zone-consistency requirement.
- **Diagnostics:** the response's `routing`/diagnostics records whether authority was enabled (and
  `applied_weight`), so an eval run can never accidentally compare an env-mutated "OFF" against ON
  (codex r1 WARN) — and with the env fallback dropped, "OFF" is unambiguous.

---

## 5. Where authority data lives (D3, D4) — schema decision

**v1 = no migration.** The re-rank reads `source` (column) + `publication` (`canonical_json->>`,
exposed in the candidate JSON per §4.2) from the **already-fetched window** (≤ `top_k*W` rows). No
index is needed because there is no scan: the rows are already materialized by the relevance query.
This is the cheapest correct option and keeps v1 reversible (drop the knob ⇒ nothing remains).

**Specified-but-deferred migration v18** (only required if authority later moves into SQL — option (c)
— or is filtered at corpus scale): a generated/projected column + expression index, in the repo's
Migration-struct style (`migrations.rs`, head v17 → bump `CURRENT_SCHEMA_VERSION` to 18,
`validate_migration_list` + `index_manifest` upsert mirroring v16 `:632-637` / v17 `:695-700`):

```sql
-- v18 authority_publication_projection (DEFERRED; not built in v1)
ALTER TABLE documents
  ADD COLUMN IF NOT EXISTS authority_publication text;        -- backfilled from canonical_json->>'publication'
CREATE INDEX IF NOT EXISTS documents_authority_publication_idx
  ON documents (source, authority_publication)
  WHERE kind = 'decision';
-- backfill: UPDATE documents SET authority_publication = lower(canonical_json->>'publication')
--           WHERE kind='decision';  (one-shot, idempotent)
-- INSERT INTO index_manifest ... schema_version 18 ...
```

(An expression index `((lower(canonical_json->>'publication')))` is an alternative that avoids the
column + backfill, but a real column is cheaper to read per-row at re-rank scale and survives a
`canonical_json` refresh deterministically; either is acceptable when this phase lands.)

**PBRI backfill (deferred, Phase R4, behind sub-knob):** project Judilibre `publication` markers from
`official_api_responses.response_json` (v16, `migrations.rs:609-610`) onto a queryable column
`documents.judilibre_publication text[]` (Cassation-only; NULL elsewhere — the honesty boundary). This
refines the **judicial** tier 3 into a `P/B/R/I`-aware sub-order *only for `cass`/`inca`*, never
inventing the ladder for `capp`/`jade`. It is a separate phase with its own backfill command (mirroring
the `04-zones` `enrich-zones` accounting style) and its own sub-knob; v1 does not depend on it.

---

## 6. Isolation invariant — how it is proven

### 6.1 Inertness when OFF (byte-identical default)

1. `authority_weight = None` **OR `≤ 0.0`** ⇒ `effective_weight = None` ⇒ `rerank_on = false` ⇒
   `query_limit = top_k+1` (identical to `main.rs:3697`/`:3425`), `project_authority = false` (SQL is
   today's exact string), the bare `truncate(top_k)` runs (`main.rs:3777-3785`/`:3468-3475`), the cursor
   is the existing `fused_score` cursor (full `next_cursor`, no `auth:` tag), and no `authority` block is
   added. **No code on the OFF path differs from today — including for an explicit `--authority-weight 0.0`.**
2. `authority_rerank(_, 0.0, _)` is a no-op even if reached (defensive): `adjusted = s * (1+0) = s`,
   stable sort preserves input order — but per (1) it is not reached for `0.0`, which is normalized to OFF.

### 6.2 Tests that lock the invariant (every review gate checks these)

- **Golden-output diff:** default `search` / `session search` / Phase 2 gate JSON is byte-identical
  pre/post the whole change set (the same golden-test discipline the zone plan used, `04-zones` T4.2
  acceptance). This is the single most important test.
- **Gate re-derivation unchanged:** `main.rs:10712-10797` re-derives pass/fail from recorded fields; the
  OFF response carries no new fields, so the gate is untouched.
- **Determinism:** `authority_rerank` is a pure function of stored fields with the existing `id`
  tie-break preserved (analysis §6 determinism requirement, `france_juris.rs:14-15`).

---

## 7. Eval plan (D7)

### 7.1 Regression guard (mandatory, exists today)

Run `eval france-juris` (`main.rs:2526`, floors `:160-172`, `PHASE2_MIN_RETRIEVAL_RECALL_AT_10=0.50`
`:165`) and `eval france-juris-zones` (`main.rs:2834`, measured-only) **with `--authority-weight`
unset (OFF) and set (e.g. 0.25, 0.5)**. Pass: judicial AND administrative recall@10 must NOT drop below
the current measured value and never below 0.50; zone recall@10 must not regress. This catches
"authority buried the gold doc" — the only failure mode recall@10 can see (analysis §4.3: it observes
whether the single known-item gold crosses the top-10 boundary; it can catch regressions and show
incidental gains but cannot reward authority ordering).

### 7.2 New ordering-quality metric: **pairwise authority-lift** (no human/LLM, measured-only)

Chosen because it is honest (publisher-authored labels only, like the existing known-item gold
`france_juris.rs:1-15`), needs no graded relevance set, and yields a single interpretable number.

- **Gold construction (no LLM, no human):** from official fields only. For a sample of decisions, take
  the identifier-stripped headnote/`decision_summary` excerpt as the query (the existing gold recipe).
  A candidate pair counts **only if ALL hold** (a conservative "comparably relevant" rule, so the metric
  measures ordering — not mere lexical co-occurrence; codex r1 WARN):
  1. both decisions are in the **same order** (judicial OR administrative — never cross-order),
  2. both are in the **OFF widened window** for that query (so the layer could actually reorder them),
  3. both fall inside the **same pre-rerank relevance band** (the §3.4 band) — i.e. they are near-tied on
     relevance, which is exactly the regime where authority is allowed to act,
  4. they form an **authority-ordered pair** of different tiers — e.g. (`cass`+`oui`, `inca`) or
     (`jade` Lebon `A`, Lebon `C`); `marker_absent` rows are excluded from pair formation.
  The pair is labeled purely by `source`+`publication` (structural, publisher-authored); no relevance
  judgement is invented. This is a **smoke/ordering signal, not graded-relevance gold** — the doc does
  not claim otherwise.
- **Metric:** `authority_lift = (# pairs where the higher-tier decision ranks above the lower-tier one) /
  (# such pairs)`, reported **ON minus OFF** (the lift the layer adds), **per source/order**, with
  **`coverage` (pair count)** and the **score-gap distribution** of the pairs (so a tiny or trivially-gapped
  pair-set cannot masquerade as a strong signal, and a Cassation gain cannot mask a `capp`/`jade`
  regression).
- **Artifact:** a SEPARATE `phase2_authority_benchmark` payload + `--out` under
  `work/03-implementation/02-evidence/`, modeled on the `phase2_zone_benchmark`
  (`main.rs:2828-2833`): **NOT a Phase 2 gate input**, records a PROPOSED advisory floor only
  (calibrate on first run; never asserted until trusted). Same "measured-only" discipline as the zone
  benchmark.
- **Honesty guardrails baked into the metric:** pairs are only formed *within an order* (no cross-order
  comparison); `marker_absent` rows are excluded from pair formation (so `capp` is never the "low" side
  of a fabricated pair). Report a `coverage` count so a tiny pair-set can't masquerade as a strong
  signal.

### 7.3 Authority weight sweep (extends `eval tune`)

Add `authority` to the `eval tune` sweep params (`main.rs:2085-2125`, currently `rrf-dense` /
`rrf-lexical` / `probes`) mapping to `RetrievalOptions { authority_weight: Some(value), .. }`. Optimize
`authority_lift` (the new metric) subject to **no recall@10 regression**. This finds a defensible
default weight before any non-zero default is ever considered (and v1 ships default-OFF regardless).

---

## 8. Phased implementation plan

Each phase is independently shippable and **codex-reviewed before any execution**; every review verifies
the **isolation invariant** (default ranking + gate byte-identical). The default path stays untouched
until the layer is proven (R1–R3 add only OFF-inert code; the knob does nothing observable until R3's
acceptance demonstrates ON behaviour behind the explicit flag).

```
R1 authority model + helper ──▶ R2 candidate projection (gated) ──▶ R3 window+rerank+first-page+config ──▶ R5 eval
   (authority.rs:                  (expose publication in           (widen window, authority_rerank,       (authority-lift
    authority_tier,                 both paths, OFF byte-safe)        FIRST-PAGE-ONLY contract, kind gate,   benchmark +
    authority_rerank,                                                 --authority-weight on search/session,  tune sweep +
    unit tests)                                                       NO env fallback)                       regression guard)
                                              R4 (OPTIONAL, deferred): PBRI backfill from v16 + v18 migration + sub-knob
```

### R1 — Authority model + re-rank helper (pure, no wiring)
- **Tasks:** `authority_tier(source, publication)` (the two ordered scales, §3.2, with `marker_absent`);
  `authority_rerank(candidates, weight, band)` (band-limited stable re-rank, §3.3–3.4); the
  `AUTHORITY_RERANK_WINDOW` const.
- **Acceptance:** unit tests for every tier (incl. `capp` ⇒ tier 0 + `marker_absent`; `jade` Lebon
  `A`/`B`/`C`/absent; `cass` published vs not; `inca`); `authority_rerank(_, 0.0, _)` is a proven no-op;
  band rule never moves an out-of-band row; determinism (stable on `id`). No call sites yet.
- **Review R1:** scope = the new module only; verify the legal model (separate orders, no cross-order
  number, honesty flag), the inertness of `weight=0`, and that nothing is wired into retrieval yet.

### R2 — Candidate projection (expose `publication`, OFF byte-safe)
- **Tasks:** add `publication` to the candidate SELECT/JSON in `retrieval.rs` (chunk `:310-336`, doc
  `:351-389`) and `zone_retrieval.rs` (`:230-269`), gated by a `project_authority: bool` on the query
  structs so the OFF SQL string is unchanged (§4.2).
- **Acceptance:** with `project_authority=false`, `hybrid_candidates_json` / `zone_candidates_json` emit
  **byte-identical** SQL and JSON to today (golden test, mirroring `04-zones` T1.3); with it true, the
  candidate carries `publication`. Default `search` output unchanged.
- **Review R2:** scope = the projection diff; verify the gated SQL is identical when OFF and that the
  zone twin mirrors the main change (corpus-wide consistency).

### R3 — Window widening + re-rank wiring + first-page contract + config (the feature, default OFF)
- **Tasks:** the window/`query_limit` logic (§4.4) in `search_with_postgres` (`main.rs:3691-3697`) and
  `zone_search_payload` (`:3422-3425`); call `authority_rerank` before truncate in both
  (`:3777-3785` / `:3468-3475`); the **first-page-only pagination contract** (§4.5: ON ⇒
  `next_cursor=null` + `cursor_note`/`routing.authority_paging`; reject inbound `--cursor`+authority) — NO
  new cursor tag, the legacy parser (`main.rs:11694-11755`) is untouched; the **kind=decision gate**
  (reject `--authority-weight` for `code`/`all`, §4.6); `authority_weight` (+optional `authority_band`) on
  `RetrievalOptions` (`retrieval.rs:63-68`, NO env fallback), `SearchArgs` / `SessionSearchArgs`
  (`:296-349` / `:474-507`), `retrieval_options()` (`:399-405`), `session_search_payload` (`:5242-5270`),
  `validate_retrieval_options` (`:420-439`); help/schema strings + the diagnostics enabled-flag.
- **Acceptance:** `--authority-weight` unset **OR `0.0`** ⇒ byte-identical default `search`/`session`/gate
  including `next_cursor` (full) and no `authority` block, run with `JURISEARCH_AUTHORITY_WEIGHT` absent
  (golden diff — THE isolation test; `0.0` is an explicit OFF baseline); set **`> 0.0`** ⇒ window widens
  (clamped `W_eff=min(W,pool_multiplier)` per grouping), candidates re-ordered within the band,
  `authority` block present, `next_cursor=null`, inbound `--cursor`+authority rejected; out-of-range
  `--authority-weight` rejected; `--authority-weight > 0` with `--kind code`/`all` rejected.
- **Review R3:** scope = the wiring + first-page contract + kind gate; verify OFF path is today's exact
  code (incl. cursor + env-absent), the window never exceeds the arm pool, ON emits no `next_cursor`, and
  main+zone use the same helper.

### R4 — (OPTIONAL, deferred) PBRI backfill + v18 + sub-knob
- **Tasks:** v18 migration (§5), a backfill command projecting `official_api_responses.response_json`
  PBRI onto `documents.judilibre_publication` (Cassation-only), and a sub-knob refining judicial tier 3.
- **Acceptance:** backfill is idempotent/resumable + accounted (mirror `enrich-zones`); refinement
  applies ONLY to `cass`/`inca`; `capp`/`jade` untouched; the v1 knob still works without it.
- **Review R4:** verify the honesty boundary (no PBRI invented for non-Cassation) and that v18 leaves the
  default path/gate untouched.

### R5 — Eval (regression guard + new metric + sweep)
- **Tasks:** the `phase2_authority_benchmark` artifact + pairwise authority-lift gold builder
  (`france_juris.rs` style, no LLM/human, within-order pairs only); the `eval tune` `authority` sweep
  param (`main.rs:2085-2125`); run the Phase 2 + zone regression guard ON vs OFF.
- **Acceptance:** Phase 2 + zone recall@10 do NOT regress at the chosen weight(s); authority-lift ON−OFF
  is reported with per-source breakdown + coverage; an advisory (never asserted) floor is proposed.
- **Review R5:** verify the gold is leak-free and publisher-authored, pairs are within-order, the
  benchmark is measured-only and never folds into the gated corpus claim.

---

## 9. Risks & rollback

| Risk | Mitigation |
|---|---|
| Authority buries the most-relevant decision (recall@10 regression) | band-limited (§3.4), relevance-dominant; R5 regression guard ON vs OFF blocks any drop |
| Default ranking accidentally changes | default-OFF + gated SQL projection + golden-output test in R2/R3; `weight=0` proven no-op (§6) |
| Pagination skip/dup once display order diverges from SQL order | v1 sidesteps it entirely: authority re-rank is FIRST-PAGE-ONLY (`next_cursor=null` when ON, inbound `--cursor`+authority rejected, §4.5); no invalid keyset is ever emitted; OFF path keeps today's cursor exactly. Deep authority paging is deferred (§10). |
| Over-claiming authority where the marker is absent (`capp`/`jade`) | `marker_absent` flag, per-order scales, no cross-order number, pairs excluded in eval (§3.2, §7.2) |
| Window exceeds the RRF arm pool (can't resurface what fell out of the arms) | `W_eff = min(W, pool_multiplier)` per grouping (§4.4) — honest about the ceiling |
| Non-determinism | pure function of stored fields, stable `id` tie-break (§6.2) |
| Migration risk (if v18 is built) | v18 is deferred and not required by v1; rollback = drop column/index; v1 rollback = remove the knob (no schema state) |

**Rollback:** v1 leaves **no schema state** — disabling the feature is "don't pass the knob" (and the
knob defaults OFF). Removing the code restores the byte-identical path by construction.

---

## 10. Open decisions (deferred — recommendation each)

1. **Recency / "good law" in the authority score.** Whether `valid_from` (or an overruled edge, which
   does not exist) folds into `a`. *Recommendation:* defer; keep authority = source+publication in v1; a
   separate `recency_weight` sub-knob later, measured the same way. (analysis §8.6)
2. **Default `authority_band` value (5%) and whether to expose it.** *Recommendation:* ship `band` as a
   fixed const in v1 (one knob is enough to prove the layer); expose `--authority-band` only if R5's
   sweep shows the band materially trades lift vs recall.
3. **`W` window factor default (8) and the chunk-grouping cap (4).** *Recommendation:* start `W=8`
   (clamped per grouping), tune in R5; document that chunk grouping is inherently capped at the chunk
   arm pool (4).
4. **Whether to ever ship a non-zero default `authority_weight`.** *Recommendation:* NO in v1 (preserves
   the byte-identical invariant); revisit only after R5's authority-lift is trusted and the gate shows no
   regression at the proposed default — and even then, only as a deliberate, separately-reviewed change to
   the default, not as part of this layer.
5. **Cross-order behaviour.** v1 never compares `jade` vs `cass` by authority (orders are separate
   scales). *Recommendation:* keep it that way; a defensible cross-order mapping is a research question,
   not a v1 requirement, and the stated scope (within-Cassation published-vs-inédit is the main zone
   split) is fully served by per-order scales.
6. **PBRI backfill (R4) priority.** *Recommendation:* defer until the `source`+`publication` layer is
   proven to help; the richer ladder refines only judicial tier 3 and is Cassation-only, so it is a
   refinement, not a prerequisite.
7. **Deep authority pagination.** v1 is first-page-only (D6/§4.5). *Recommendation:* defer; only build a
   stateful authority cursor (or move ordering into SQL) if there is a concrete demand to page INTO a
   re-ranked result set — the eval and the product value live in the first window, so this is unlikely to
   be needed.

---

## Appendix — verified anchors used by this design

- Score / RRF / final ORDER BY / candidate JSON: `crates/jurisearch-storage/src/retrieval.rs:13-19`,
  `:63-68`, `:120-156`, `:161-173`, chunk `:310-336`, document `:351-389`, `:636-646`; cursor
  `:535-567`.
- Zone path (shipped): `crates/jurisearch-storage/src/zone_retrieval.rs:160-189`, `:198-277` (projection
  `:260-269`); `crates/jurisearch-cli/src/main.rs:3373-3491` (`zone_search_payload`, window `:3422-3425`,
  truncate `:3468-3475`).
- Main path window/truncate: `crates/jurisearch-cli/src/main.rs:3659-3834` (arm/query limits
  `:3691-3697`, JSON-mutation/truncate `:3769-3785`).
- Config surface: `RetrievalOptions` `retrieval.rs:63-68`; `SearchArgs` `main.rs:296-349`,
  `retrieval_options()`/`decision_filters()` `:399-415`, `validate_retrieval_options` `:420-439`;
  `SessionSearchArgs` `:474-507`; `session_search_payload` `:5242-5270`.
- Cursor parser: `crates/jurisearch-cli/src/main.rs:11694-11755`.
- Schema / migrations (head v17; style): `crates/jurisearch-storage/src/migrations.rs:3`, documents
  `:27-44`, v16 `official_api_responses`(+`response_json`) `:582-637`, v17 `:639-701`,
  `validate_migration_list` `:773-776`.
- Authority data: `source` column `migrations.rs:29` / `zone_units.source` `:505`; publication
  extraction `crates/jurisearch-ingest/src/juri/mod.rs:680-683`, tests `tests.rs:89`/`:190`/`:194`;
  projection `crates/jurisearch-storage/src/projection.rs:281-366`.
- Eval: gate floors `main.rs:160-172`, `eval france-juris` `:2526`, gate re-derivation `:10712-10797`,
  zone benchmark `:2828-2834`, `eval tune` sweep `:2085-2125`; gold `france_juris.rs:1-80`.
