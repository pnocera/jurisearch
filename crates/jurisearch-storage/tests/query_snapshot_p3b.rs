//! work/09 P3B acceptance: one request = one read snapshot. A snapshot opened against the active
//! generation keeps observing THAT generation even after a concurrent activation swaps in a new one
//! (the operated switch never drops the old physical generation), and a fresh snapshot observes the new
//! topology. No sleeps: the swap happens on another connection between the two reads. Skips cleanly when
//! the managed PG harness is absent.

mod common;

use common::{discover_pg_config, vector_literal};
use jurisearch_storage::generations::{
    ActivationStamps, activate_generation, create_generation_from_public, generation_schema,
};
use jurisearch_storage::query::{QueryStore, ReadSnapshot};
use jurisearch_storage::runtime::{ManagedPostgres, StorageError};
use jurisearch_storage::zone_units::zone_retrieval_coverage_in_snapshot;

const FP: &str = "bge-m3:1024:cls:normalize=true";

fn stamps(sequence: i64, baseline_id: &'static str) -> ActivationStamps<'static> {
    ActivationStamps {
        sequence,
        baseline_id,
        schema_version: 24,
        embedding_fingerprint: FP,
        builder_versions: &serde_json::Value::Null,
        last_package_id: None,
        last_package_digest: None,
    }
}

/// Seed a tiny, fully query-ready `core` corpus in `public`: one decision carrying `title` as a
/// sentinel, plus its BM25/dense-indexed chunk + a matching embedding under [`FP`]. The chunk/embedding
/// never change, so each generation populated from `public` stays query-ready — only the sentinel
/// `title` (changed by [`retitle`]) differs between generations.
fn seed_core_with_title(postgres: &ManagedPostgres, title: &str) -> Result<(), StorageError> {
    postgres.execute_sql(&format!(
        "INSERT INTO documents (document_id, source, kind, source_uid, citation, title, body, \
           valid_from, source_payload_hash, canonical_json) \
         VALUES ('cass:SWAP','cass','decision','cass:SWAP','Cass','{title}', \
           'corps','2024-01-01','sha256:swap','{{}}'); \
         INSERT INTO chunks (chunk_id, document_id, chunk_index, body, contextualized_body, \
           source_payload_hash, chunk_builder_version, embedding_fingerprint) \
         VALUES ('cass:SWAP#0','cass:SWAP',0,'corps','ctx corps','sha256:cs','c1','{FP}');"
    ))?;
    let vector = vector_literal(3);
    postgres.execute_sql(&format!(
        "INSERT INTO chunk_embeddings (chunk_id, embedding_fingerprint, embedding, model, dimension) \
         VALUES ('cass:SWAP#0','{FP}','{vector}'::vector,'m',1024);"
    ))?;
    Ok(())
}

/// Re-title the sentinel decision in `public` (the only field that differs between the two generations).
fn retitle(postgres: &ManagedPostgres, title: &str) -> Result<(), StorageError> {
    postgres.execute_sql(&format!(
        "UPDATE documents SET title = '{title}' WHERE document_id = 'cass:SWAP';"
    ))?;
    Ok(())
}

/// Read the sentinel title through `snapshot` (unqualified `documents` resolves to the snapshot's pinned
/// active physical generation).
fn title_via(snapshot: &mut dyn ReadSnapshot) -> Result<String, StorageError> {
    snapshot.read_text("SELECT title FROM documents WHERE document_id = 'cass:SWAP';")
}

/// Insert one official zone unit for the sentinel decision in `public` (the replicated `zone_units`
/// table is carried into each generation, so the per-generation zone coverage count differs).
fn insert_zone_unit(
    postgres: &ManagedPostgres,
    zone_unit_id: &str,
    zone: &str,
) -> Result<(), StorageError> {
    postgres.execute_sql(&format!(
        "INSERT INTO zone_units (zone_unit_id, document_id, zone, fragment_index, body, search_body, \
           source, text_hash, zone_unit_builder_version) \
         VALUES ('{zone_unit_id}','cass:SWAP','{zone}',0,'corps','corps','cass','h-{zone_unit_id}','v1');"
    ))?;
    Ok(())
}

/// The active generation's `zone_units` count, read THROUGH `snapshot`'s coverage (the value that backs
/// both the `search --zone` gate and the response's `scope.indexed_decisions`).
fn zone_units_total(snapshot: &mut dyn ReadSnapshot) -> Result<u64, StorageError> {
    let coverage: serde_json::Value =
        serde_json::from_str(&zone_retrieval_coverage_in_snapshot(snapshot)?)
            .map_err(StorageError::Json)?;
    Ok(coverage["zone_units"]["total"].as_u64().unwrap_or(0))
}

#[test]
fn one_request_observes_one_generation_across_a_concurrent_swap() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("p3b concurrent swap")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-p3b-swap.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;

    // Generation A: sentinel "GEN-A", activated at sequence 1.
    seed_core_with_title(&postgres, "GEN-A")?;
    let gen_a = create_generation_from_public(&postgres, "core", 1, Some("core-swap-g0001"))?;
    activate_generation(
        &postgres,
        "core",
        &gen_a,
        &stamps(1, "core-swap-g0001"),
        None,
    )?;

    // Open ONE snapshot. begin_snapshot resolves the active corpora inside REPEATABLE READ, which
    // establishes the MVCC snapshot deterministically; the read below confirms it sees GEN-A.
    let mut snapshot = postgres.begin_snapshot()?;
    assert_eq!(snapshot.active_corpora().len(), 1);
    assert_eq!(
        snapshot.active_corpora()[0].schema,
        generation_schema("core", 1)
    );
    assert_eq!(title_via(&mut *snapshot)?, "GEN-A");

    // Swap in Generation B on the live database (another connection), sentinel "GEN-B", sequence 2.
    retitle(&postgres, "GEN-B")?;
    let gen_b = create_generation_from_public(&postgres, "core", 2, Some("core-swap-g0002"))?;
    activate_generation(
        &postgres,
        "core",
        &gen_b,
        &stamps(2, "core-swap-g0002"),
        Some(1),
    )?;

    // The ALREADY-OPEN snapshot still observes GEN-A: it is pinned to the old physical generation (which
    // the operated switch never drops) and its REPEATABLE READ snapshot predates the swap.
    assert_eq!(
        title_via(&mut *snapshot)?,
        "GEN-A",
        "a swap mid-request is invisible to the open snapshot"
    );
    assert_eq!(
        snapshot.active_corpora()[0].schema,
        generation_schema("core", 1)
    );
    drop(snapshot);

    // A FRESH snapshot observes the new active topology (GEN-B at generation 2).
    let mut next = postgres.begin_snapshot()?;
    assert_eq!(
        next.active_corpora()[0].schema,
        generation_schema("core", 2)
    );
    assert_eq!(
        title_via(&mut *next)?,
        "GEN-B",
        "the next request opens on the new active generation"
    );

    Ok(())
}

#[test]
fn zone_coverage_is_read_through_the_request_snapshot_across_a_swap() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("p3b zone coverage swap")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-p3b-zonecov.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;

    // Generation A: the sentinel decision carries ONE official zone unit.
    seed_core_with_title(&postgres, "GEN-A")?;
    insert_zone_unit(&postgres, "z1", "motivations")?;
    let gen_a = create_generation_from_public(&postgres, "core", 1, Some("core-zone-g0001"))?;
    activate_generation(
        &postgres,
        "core",
        &gen_a,
        &stamps(1, "core-zone-g0001"),
        None,
    )?;

    // A request snapshot reads zone coverage = 1 (the gate + the response `scope` both use this value).
    let mut snapshot = postgres.begin_snapshot()?;
    assert_eq!(zone_units_total(&mut *snapshot)?, 1);

    // Generation B adds a SECOND zone unit, swapped in on another connection at sequence 2.
    insert_zone_unit(&postgres, "z2", "moyens")?;
    let gen_b = create_generation_from_public(&postgres, "core", 2, Some("core-zone-g0002"))?;
    activate_generation(
        &postgres,
        "core",
        &gen_b,
        &stamps(2, "core-zone-g0002"),
        Some(1),
    )?;

    // The open request's zone coverage is wholly-old (1) — the candidates and the `scope` coverage can
    // never disagree across a swap because both come from this one snapshot.
    assert_eq!(
        zone_units_total(&mut *snapshot)?,
        1,
        "zone coverage is wholly-old for the open request"
    );
    drop(snapshot);

    // The next request observes the new zone coverage (2).
    let mut next = postgres.begin_snapshot()?;
    assert_eq!(
        zone_units_total(&mut *next)?,
        2,
        "the next request sees the new generation's zone coverage"
    );
    Ok(())
}

#[test]
fn a_query_snapshot_refuses_a_multi_corpus_topology_until_3c() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("p3b multi-corpus refusal")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-p3b-multi.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;

    // Activate `core`, then install a SECOND corpus row directly into the cursor so the resolver returns
    // two active corpora (a real 2-corpus install is 3C; this is the minimal multi-corpus topology).
    seed_core_with_title(&postgres, "GEN-A")?;
    let gen_a = create_generation_from_public(&postgres, "core", 1, Some("core-swap-g0001"))?;
    activate_generation(
        &postgres,
        "core",
        &gen_a,
        &stamps(1, "core-swap-g0001"),
        None,
    )?;
    postgres.execute_sql(
        "INSERT INTO jurisearch_control.corpus_state \
           (corpus, active_generation, sequence, baseline_id, schema_version, embedding_fingerprint) \
         VALUES ('inpi','inpi_g0001',1,'inpi-g0001',24,'fp');",
    )?;

    let error = postgres
        .begin_snapshot()
        .map(|_| ())
        .expect_err("a query snapshot must refuse a multi-corpus topology until 3C");
    assert!(
        error.to_string().to_lowercase().contains("multi-corpus"),
        "the refusal names the 3C deferral: {error}"
    );
    Ok(())
}
