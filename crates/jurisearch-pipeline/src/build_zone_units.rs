//! Zone-unit derivation: derive `zone_units` rows from the cached official zones in `decision_zones`.
//!
//! Extracted from `jurisearch-cli` (work/10) so the producer can run derivation IN-PROCESS against its
//! external [`DbClientSource`] between Judilibre enrichment (Phase 4, writes `decision_zones`) and
//! zone-unit embedding (Phase 5). The CLI `ingest build-zone-units` command now delegates here, and the
//! producer `update` cycle calls [`build_zone_units`] directly — the only correct home, since only the
//! producer reaches the external DB.
//!
//! ## Idempotency invariant (why no "derived-empty" marker is needed)
//!
//! The derivable selector ([`load_derivable_decision_zones_json_with_client`]) treats an
//! `ok`/non-expired `decision_zones` row with absent/stale `zone_units` as derivable, and
//! [`derive_zone_unit_rows`] skips blank fragments. A row that derives to ZERO non-empty fragments would
//! therefore stay selected on every run (repeated `replace_set` outbox churn). That edge CANNOT occur:
//! a `decision_zones` row only reaches `status='ok'` when `normalize_judilibre_zones` set `any_valid =
//! true`, and it sets that flag ONLY after pushing a fragment whose trimmed text is non-empty
//! (`crates/jurisearch-pipeline/src/enrichment/judilibre_zones.rs`, `normalize_judilibre_zones` +
//! `status = if valid_zones { "ok" } else { "invalid_offsets" }`). The `zones_json` persisted on the row
//! is exactly that normalized object, and `derive_zone_unit_rows` walks the same
//! motivations/moyens/dispositif fragment text with the same non-empty-trim rule. Hence an `ok` row
//! ALWAYS yields >= 1 derived unit, so it drops out of the derivable set after one derive. The single
//! writer of `ok` rows is `enrich_decision_from_judilibre_with_client`; no other path fabricates `ok`.

use crate::*;

/// Derivation-logic version stamped on `zone_units.zone_unit_builder_version`; bump to force a full
/// re-derive on a logic change (part of the staleness predicate in
/// [`load_derivable_decision_zones_json_with_client`]).
pub const ZONE_UNIT_BUILDER_VERSION: &str = "zone-units:v1";
/// Candidate page size for the zone-unit derivation keyset scan.
pub const BUILD_ZONE_UNITS_PAGE_SIZE: u32 = 500;

/// Derive a decision's `zone_units` rows from its cached `zones_json` object (motivations/moyens/
/// dispositif fragment text). One row per non-empty fragment with a contiguous per-zone `fragment_index`.
/// Borrows the fragment text from `zones`, so the returned rows must be used before `zones` is dropped.
///
/// For a `decision_zones` row with `status='ok'` this ALWAYS returns >= 1 row (see the module-level
/// idempotency invariant); an empty return can only arise from a non-`ok`/`invalid_offsets` shape the
/// derivable selector never surfaces.
pub fn derive_zone_unit_rows<'a>(
    document_id: &'a str,
    source: &'a str,
    text_hash: &'a str,
    zones: &'a Value,
) -> Vec<ZoneUnitRow<'a>> {
    let mut rows = Vec::new();
    for zone in ["motivations", "moyens", "dispositif"] {
        let Some(fragments) = zones[zone].as_array() else {
            continue;
        };
        let mut fragment_index = 0i32;
        for fragment in fragments {
            let Some(text) = fragment["text"].as_str() else {
                continue;
            };
            if text.trim().is_empty() {
                continue;
            }
            rows.push(ZoneUnitRow {
                document_id,
                zone,
                fragment_index,
                body: text,
                search_body: text,
                source,
                text_hash,
                builder_version: ZONE_UNIT_BUILDER_VERSION,
            });
            fragment_index += 1;
        }
    }
    rows
}

/// Inputs for one [`build_zone_units`] pass. `limit` caps the number of decisions derived (paged);
/// `rebuild` re-derives every eligible `ok` row regardless of unit staleness.
#[derive(Debug, Clone)]
pub struct BuildZoneUnitsRequest {
    pub limit: Option<u32>,
    pub rebuild: bool,
}

/// What one [`build_zone_units`] pass produced. `coverage` is the zone-retrieval coverage object read
/// AFTER derivation (over the producer `public` working set — see
/// [`zone_retrieval_coverage_with_client`]).
#[derive(Debug, Clone)]
pub struct BuildZoneUnitsOutcome {
    pub decisions_derived: u64,
    pub zone_units_written: u64,
    pub coverage: Value,
}

/// Derive `zone_units` from the cached official zones in `decision_zones` over `db`.
///
/// Pages the derivable set (fresh `ok` Cassation rows with stale/absent units), deriving each decision's
/// units in one idempotent `replace_zone_units_for_document_with_client` transaction that emits a single
/// document-scoped `replace_set` outbox row. Because every `ok` row yields >= 1 unit (module-level
/// invariant), a derived decision drops out of the derivable set and is never re-selected on the next
/// run — safe to call every producer cycle.
///
/// # Errors
/// [`BuildZoneUnitsError`] on a storage failure or a JSON parse failure of a page/coverage payload.
pub fn build_zone_units(
    db: &impl DbClientSource,
    req: BuildZoneUnitsRequest,
) -> Result<BuildZoneUnitsOutcome, BuildZoneUnitsError> {
    build_zone_units_inner(db, req).map_err(BuildZoneUnitsError::from)
}

fn build_zone_units_inner(
    db: &impl DbClientSource,
    req: BuildZoneUnitsRequest,
) -> Result<BuildZoneUnitsOutcome, ErrorObject> {
    let BuildZoneUnitsRequest { limit, rebuild } = req;
    let mut client = db.client().map_err(storage_error_object)?;
    let run_id = producer_run_id("build-zone-units");
    let outbox = jurisearch_storage::outbox::OutboxContext::new(
        &run_id,
        jurisearch_storage::migrations::CURRENT_SCHEMA_VERSION,
    );

    let mut decisions: u64 = 0;
    let mut units_written: u64 = 0;
    let mut cursor: Option<String> = None;
    loop {
        let page_limit = match limit {
            Some(limit) => {
                let done = u32::try_from(decisions).unwrap_or(u32::MAX);
                if done >= limit {
                    break;
                }
                (limit - done).min(BUILD_ZONE_UNITS_PAGE_SIZE)
            }
            None => BUILD_ZONE_UNITS_PAGE_SIZE,
        };
        let page_json = load_derivable_decision_zones_json_with_client(
            &mut client,
            ZONE_UNIT_BUILDER_VERSION,
            rebuild,
            cursor.as_deref(),
            page_limit,
        )
        .map_err(storage_error_object)?;
        let page: Value = serde_json::from_str(&page_json)
            .map_err(|error| dependency_unavailable(error.to_string()))?;
        let candidates = page["candidates"].as_array().cloned().unwrap_or_default();
        if candidates.is_empty() {
            break;
        }
        for candidate in &candidates {
            let document_id = candidate["document_id"].as_str().unwrap_or_default();
            if document_id.is_empty() {
                continue;
            }
            let source = candidate["source"].as_str().unwrap_or_default();
            let text_hash = candidate["text_hash"].as_str().unwrap_or_default();
            // `rows` borrows fragment text from THIS page's `candidate` value; it is consumed inside this
            // loop iteration (never stored across pages — the page `Value` is dropped next iteration).
            let rows = derive_zone_unit_rows(document_id, source, text_hash, &candidate["zones"]);
            replace_zone_units_for_document_with_client(
                &mut client,
                document_id,
                &rows,
                Some(&outbox),
            )
            .map_err(storage_error_object)?;
            decisions += 1;
            units_written += rows.len() as u64;
            if let Some(limit) = limit
                && decisions >= u64::from(limit)
            {
                break;
            }
        }
        cursor = page["next_cursor"].as_str().map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }

    let coverage: Value = serde_json::from_str(
        &zone_retrieval_coverage_with_client(&mut client).map_err(storage_error_object)?,
    )
    .map_err(|error| dependency_unavailable(error.to_string()))?;
    Ok(BuildZoneUnitsOutcome {
        decisions_derived: decisions,
        zone_units_written: units_written,
        coverage,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn derive_zone_unit_rows_handles_multi_fragment_and_skips_empty() {
        // T3.1: one row per non-empty fragment, contiguous per-zone fragment_index; empty zones/blank
        // fragments produce no rows.
        let zones = json!({
            "motivations": [{ "text": "premier motif" }, { "text": "  " }, { "text": "second motif" }],
            "moyens": [{ "text": "un moyen" }],
            "dispositif": []
        });
        let rows = derive_zone_unit_rows("cass:X", "cass", "h", &zones);
        // 2 motivations (the blank one skipped) + 1 moyens + 0 dispositif.
        assert_eq!(rows.len(), 3);
        let motivations: Vec<_> = rows.iter().filter(|r| r.zone == "motivations").collect();
        assert_eq!(motivations.len(), 2);
        assert_eq!(motivations[0].fragment_index, 0);
        assert_eq!(motivations[0].body, "premier motif");
        assert_eq!(motivations[1].fragment_index, 1); // contiguous despite the skipped blank
        assert_eq!(motivations[1].body, "second motif");
        assert!(
            rows.iter()
                .all(|r| r.builder_version == ZONE_UNIT_BUILDER_VERSION)
        );
        assert!(
            rows.iter()
                .all(|r| r.body == r.search_body && r.source == "cass" && r.text_hash == "h")
        );
    }
}
