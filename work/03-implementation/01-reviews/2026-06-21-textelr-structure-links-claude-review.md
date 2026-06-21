# Claude Review — TEXTELR Structure Links

Verdict: GO

- Commit reviewed: `785e044` "Preserve TEXTELR structure links"
- Baseline: `426dcb7`
- Reviewer: Claude (Opus 4.8), 2026-06-21

## Findings

No correctness, data-loss, or regression blockers. The parser faithfully captures the real DILA
STRUCT shape; findings below are robustness/forward-compat concerns, all non-blocking.

### Verified sound

- **Flat list + `niv` matches real DILA.** I extracted real TEXTELR members from the local
  baseline. `<STRUCT>` is a *flat* sequence of `<LIEN_SECTION_TA>` / `<LIEN_ART>` siblings with a
  `niv` attribute encoding depth (not XML-nested links), so the ordered flat
  `Vec<ParsedTextStructLink>` with `order` + `level` is a faithful, lossless-enough representation
  for later hierarchy assembly (`crates/jurisearch-ingest/src/legi/mod.rs:127-146`).
- **Link-text attribution is correct,** including the realistic case where `LIEN_SECTION_TA`
  carries its title as direct text and `LIEN_ART` is self-closing. The `link_stack` push/pop
  (`mod.rs:671,687-709`) attributes text to the innermost open link and would even handle nesting
  if it occurred; `assign_text_struct_link_text` (`mod.rs:1259-1273`) whitespace-normalizes via the
  shared `append_xml_content` and stores `None` when empty — consistent with the article path.
- **Full DILA attribute fidelity:** every raw attribute is preserved as `attributes: Vec<GraphEdgeAttribute>`
  (key/value, document order), so any field not promoted to a typed column (`etat`, `num`,
  `origine`, `url`, `cid`, …) is still recoverable downstream.
- **Backward-compatible reads:** `#[serde(default)]` on `structure_links` (`mod.rs:127`) means
  pre-existing TEXTELR `canonical_json` deserializes to an empty vec rather than erroring.
- **No regression surface:** `RawTextStruct`/`ParsedTextStruct` gain a `Vec` field constructed only
  in `into_text_struct`; `Default` covers `RawTextStruct`, and the storage layer stores
  `canonical_json` as opaque jsonb (no migration). Both targeted tests pass and workspace clippy is
  clean.

### Low–Medium — `target_source_uid` uses value-scan-first-match, not key priority
`push_text_struct_link` derives the target by scanning every attribute *value* for the first one
containing a known prefix (`mod.rs:1231-1233` → `extract_known_source_uid`, `mod.rs:1330-1346`).
Real DILA emits attributes **alphabetically**, so `cid` is scanned *before* `id`. In observed real
data this happens to be safe — `LIEN_SECTION_TA` has `cid == id` (both `LEGISCTA…`) and `LIEN_ART`
has no `cid` (only `id`) — so the extracted UID is correct. But it is a latent fragility: a
`LIEN_TXT` (or any link) whose `cid` is a *parent* chronicle (e.g. a `JORFTEXT…`/`LEGITEXT…`)
distinct from its `id` would yield the parent's UID, not the link target's. The article path
deliberately avoids this by using explicit key priority `["id","cid","cidtexte","href"]`
(`RawPublisherLink::target_source_uid`). Because the raw `attributes` are preserved, a consumer can
re-derive the correct target, so this is not a blocker — but it should be hardened before the
hierarchy-assembly slice depends on `target_source_uid`. Note both new tests mask this: the ingest
fixture (`mod.rs:~2073`) lists `id` *first* with an unrealistic `cid="LEGITEXT…"` on a section, and
the storage fixture (`legi_metadata_projection.rs:109`) has only `id` — neither uses DILA's real
alphabetical (`cid`-before-`id`) ordering.

### Low — no parser/canonical version bump accompanies the output change
The TEXTELR output format changed (added `structure_links`) but `canonical_version`
(`"legi_textelr:v1"`, `mod.rs:1079`) and `LEGI_PARSER_VERSION` (`jurisearch-cli/src/main.rs:54`) are
unchanged. On an existing index, resume treats TEXTELR members as `compatible_complete` (same
payload hash + versions) and skips them, so their persisted rows will *not* gain `structure_links`
without an out-of-band reprocess — and there is no automatic signal that one is needed. Combined
with `#[serde(default)]`, the consuming slice cannot distinguish "stale row, never parsed for links"
from "genuinely link-less TEXTELR." Fine for a not-yet-consumed prerequisite, but the version-bump
+ reprocess decision should be made deliberately when the consumer lands.

### Low — `debut`/`fin` stored raw (no normalization/validation)
Unlike article/section dates (which run through `validate_date` / `normalize_end_date` and collapse
the `2999-01-01`/`2999-12-31` sentinels to `None`), structure-link `debut`/`fin` are stored
verbatim (`mod.rs:1236-1237`), so real sentinels like `fin="2999-01-01"` persist as-is and malformed
dates are not rejected. This is a reasonable fidelity choice for a raw-preservation layer, but the
consumer must apply the same sentinel/validation rules used elsewhere to stay consistent.

### Nit — redundant tag check
In `parse_text_struct`, each Start/Empty event checks both `is_text_struct_link_tag(name)` and
`matches!(name, "LIEN_TXT"|"LIEN_SECTION_TA"|"LIEN_ART")` (`mod.rs:678-684,691-698`) — the same
three-tag set evaluated twice. Harmless; could be unified.

## Verification

- Inspected the full diff and surrounding code: `ParsedTextStructLink`/`structure_links`
  (`mod.rs:127-146,420`), the `parse_text_struct` event loop and `link_stack` handling
  (`mod.rs:664-722`), `push_text_struct_link` / `text_struct_link_attribute` /
  `assign_text_struct_link_text` / `is_text_struct_link_tag` (`mod.rs:1225-1277`),
  `extract_known_source_uid` (`mod.rs:1330-1346`), `into_text_struct` (`mod.rs:1065-1081`), and the
  article-path analogues (`push_publisher_link`, `assign_link_text`,
  `RawPublisherLink::target_source_uid`) for consistency.
- Checked version constants (`jurisearch-cli/src/main.rs:54-56`) and confirmed none were bumped.
- Extracted and read real TEXTELR members from
  `/home/pierre/Apps/juridocs/opendata/LEGI/Freemium_legi_global_20250713-140000.tar.gz`, confirming:
  flat `<STRUCT>` with `niv`; `LIEN_SECTION_TA` carrying title text with `cid == id` (both
  `LEGISCTA…`); `LIEN_ART` self-closing with only `id`; and **alphabetical** attribute emission
  (`cid` before `id`) — which the test fixtures do not mirror.
- Ran `cargo test -p jurisearch-ingest parses_textelr_metadata_root_with_date_hint` → **1 passed**.
- Ran `cargo test -p jurisearch-storage persists_legi_metadata_roots_with_stable_keys` → **1 passed**
  (live Postgres; asserts `structure_links[0]` is persisted in `canonical_json`).
- Ran `cargo clippy --workspace --all-targets -- -D warnings` → **clean**.
- Did not separately re-run the full `cargo test --workspace` / ignored real-archive subset
  (already reported green by the author); the ignored subset does exercise real TEXTELR members
  through `parse_legi_member` → `parse_text_struct`, confirming the new path is real-data-safe.

## Recommendations

1. Harden `target_source_uid`: select by key priority (mirroring
   `RawPublisherLink::target_source_uid`: `id` → `cid`/`cidtexte` → `href`) rather than first
   value-match, and add a test fixture using DILA's real alphabetical ordering with a divergent
   `cid` (e.g. a `LIEN_TXT` whose `cid` is a parent text) so the extraction is pinned against
   realistic input.
2. Decide the reprocessing story for the consuming slice: bump the parser/canonical version when
   `structure_links` are first consumed (and document the reprocess), since otherwise existing
   indexes silently retain empty `structure_links` indistinguishable from link-less texts.
3. Either normalize/validate `debut`/`fin` consistently with the rest of the parser or explicitly
   document that structure-link dates are raw and the consumer owns sentinel handling.
4. Optionally collapse the duplicated three-tag check in `parse_text_struct`.
