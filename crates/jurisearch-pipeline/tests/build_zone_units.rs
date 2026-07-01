//! PG-gated integration test for [`jurisearch_pipeline::build_zone_units`]: a seeded `ok`
//! `decision_zones` row derives its `zone_units` and emits exactly one document-scoped `replace_set`
//! outbox row, and re-running is a no-op (the derived decision drops out of the derivable set).

mod common;

use common::discover_pg_config;
use jurisearch_pipeline::{BuildZoneUnitsRequest, build_zone_units};
use jurisearch_storage::runtime::{ManagedPostgres, StorageError};

/// Seed a Cassation decision (source `cass`, parser-valid pourvoi) plus its `ok` `decision_zones`
/// overlay row carrying `zones_json` with two motivations + one moyen (three non-empty fragments).
fn seed_ok_decision(
    postgres: &ManagedPostgres,
    document_id: &str,
    pourvoi: &str,
) -> Result<(), StorageError> {
    let zones = r#"{"motivations":[{"start":0,"end":8,"text":"motif un"},{"start":9,"end":19,"text":"motif deux"}],"moyens":[{"start":0,"end":5,"text":"moyen"}],"dispositif":[]}"#;
    postgres
        .execute_sql(&format!(
            "INSERT INTO documents \
               (document_id, source, kind, source_uid, citation, title, body, \
                valid_from, source_payload_hash, canonical_json) \
             VALUES \
               ('{document_id}', 'cass', 'decision', '{document_id}', 'Cass. civ. {pourvoi}', \
                'Arret', 'corps de la decision', '2024-01-01', 'sha256:{document_id}', \
                '{{\"case_numbers\":[\"{pourvoi}\"]}}'); \
             INSERT INTO decision_zones \
               (document_id, provider, provider_decision_id, source_uid, status, \
                fetched_at, expires_at, text_hash, offset_unit, zones_json, raw_json) \
             VALUES \
               ('{document_id}', 'judilibre', 'jdl:{document_id}', '{document_id}', 'ok', \
                now(), now() + interval '30 days', 'hash-1', 'char', \
                '{zones}'::jsonb, '{{}}'::jsonb);",
        ))
        .map(|_| ())
}

#[test]
fn build_zone_units_derives_and_emits_document_scoped_outbox_row() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("build_zone_units derive")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-build-zone-units.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;

    seed_ok_decision(&postgres, "cass:JURITEXT0001", "12-34567")?;

    // Derive: the ok row yields 3 units (2 motivations + 1 moyen), 1 decision.
    let outcome = build_zone_units(
        &postgres,
        BuildZoneUnitsRequest {
            limit: None,
            rebuild: false,
        },
    )
    .map_err(|error| StorageError::Projection {
        message: format!("build_zone_units failed: {error}"),
    })?;
    assert_eq!(outcome.decisions_derived, 1);
    assert_eq!(outcome.zone_units_written, 3);

    let unit_count = postgres.execute_sql("SELECT count(*)::text FROM zone_units;")?;
    assert_eq!(unit_count.trim(), "3");

    // Exactly one document-scoped `replace_set` outbox row for the zone-unit set (INV-2) — never per-row.
    let replace_rows = postgres.execute_sql(
        "SELECT count(*)::text FROM package_change_log \
         WHERE table_name='zone_units' AND op='replace_set' AND scope_key='cass:JURITEXT0001';",
    )?;
    assert_eq!(replace_rows.trim(), "1", "one replace_set per document");
    let deletes =
        postgres.execute_sql("SELECT count(*)::text FROM package_change_log WHERE op='delete';")?;
    assert_eq!(
        deletes.trim(),
        "0",
        "never per-row deletes for a derived rebuild"
    );

    // Coverage reflects the derived units.
    assert_eq!(outcome.coverage["zone_units"]["total"].as_i64(), Some(3));

    // Idempotency: the ok row derived to >= 1 unit, so a second run derives NOTHING (drops out of the
    // derivable set) and emits no further outbox row.
    let again = build_zone_units(
        &postgres,
        BuildZoneUnitsRequest {
            limit: None,
            rebuild: false,
        },
    )
    .map_err(|error| StorageError::Projection {
        message: format!("build_zone_units re-run failed: {error}"),
    })?;
    assert_eq!(
        again.decisions_derived, 0,
        "no re-derive of a current decision"
    );
    assert_eq!(again.zone_units_written, 0);
    let replace_rows_after = postgres.execute_sql(
        "SELECT count(*)::text FROM package_change_log \
         WHERE table_name='zone_units' AND op='replace_set' AND scope_key='cass:JURITEXT0001';",
    )?;
    assert_eq!(
        replace_rows_after.trim(),
        "1",
        "re-run emits no additional replace_set (no timer churn)"
    );
    Ok(())
}
