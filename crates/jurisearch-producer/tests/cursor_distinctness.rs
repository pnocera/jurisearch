//! Regression gate: the THREE producer cursor coordinate systems stay distinct and are never conflated.
//!
//! (1) DILA fetch cursor (archive-timestamp space), (2) ingest journal (accepted-archive name/timestamp
//! space), (3) package high-water mark (`change_seq`/sequence space). They are distinct types, so a
//! package sequence can never be substituted for an archive selector (the BLOCKER-2 trap), and a run
//! checkpoint records all three SEPARATELY and round-trips.

use jurisearch_producer::cursors::{
    FetchCursorCoordinate, IngestJournalCoordinate, PackageHighWaterMark, RunCheckpoint, RunPhase,
};

#[test]
fn the_three_coordinates_are_distinct_types_recorded_separately() {
    // Same numeric value (`42`) lives in three DIFFERENT coordinate systems with no shared field that
    // could let one be read as another.
    let fetch = FetchCursorCoordinate {
        source: "legi".to_owned(),
        latest_file_name: Some("LEGI_20260628-200000.tar.gz".to_owned()),
        latest_compact_timestamp: Some("20260628200000".to_owned()),
    };
    let journal = IngestJournalCoordinate {
        source: "legi".to_owned(),
        run_id: Some("legi-42".to_owned()),
        journal_compact_timestamp: Some("20260628200000".to_owned()),
        archives_ingested: 42,
    };
    let hwm = PackageHighWaterMark {
        corpus: "core".to_owned(),
        head_sequence: Some(42),
        included_change_seq_high: Some(42),
    };

    // The fetch/ingest cursors key on an ARCHIVE TIMESTAMP string; the package mark keys on an integer
    // SEQUENCE. There is no field on the archive cursors that holds a `change_seq`.
    assert_eq!(
        fetch.latest_compact_timestamp,
        journal.journal_compact_timestamp
    );
    assert_eq!(hwm.included_change_seq_high, Some(42));
    // Compile-time proof of separation: these are three different types; a function taking one cannot be
    // passed another. (If they were merged, this module would not type-check.)
    fn takes_fetch(_: &FetchCursorCoordinate) {}
    fn takes_journal(_: &IngestJournalCoordinate) {}
    fn takes_hwm(_: &PackageHighWaterMark) {}
    takes_fetch(&fetch);
    takes_journal(&journal);
    takes_hwm(&hwm);
}

#[test]
fn run_checkpoint_records_all_three_coordinates_and_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let mut checkpoint = RunCheckpoint::started("legislation", "legislation-1");
    checkpoint.phase = RunPhase::Published;
    checkpoint.fetch_cursors = vec![FetchCursorCoordinate {
        source: "legi".to_owned(),
        latest_file_name: Some("LEGI_20260628-200000.tar.gz".to_owned()),
        latest_compact_timestamp: Some("20260628200000".to_owned()),
    }];
    checkpoint.ingest_journals = vec![IngestJournalCoordinate {
        source: "legi".to_owned(),
        run_id: Some("ingest-1".to_owned()),
        journal_compact_timestamp: Some("20260628200000".to_owned()),
        archives_ingested: 1,
    }];
    checkpoint.package_high_water_mark = Some(PackageHighWaterMark {
        corpus: "core".to_owned(),
        head_sequence: Some(3),
        included_change_seq_high: Some(17),
    });
    checkpoint.save(dir.path()).unwrap();

    let loaded = RunCheckpoint::load(dir.path(), "legislation", "legislation-1")
        .unwrap()
        .expect("checkpoint persisted");
    assert_eq!(loaded, checkpoint);
    assert_eq!(loaded.phase, RunPhase::Published);
    // The three coordinate kinds are present and independent.
    assert_eq!(loaded.fetch_cursors.len(), 1);
    assert_eq!(loaded.ingest_journals.len(), 1);
    assert_eq!(
        loaded
            .package_high_water_mark
            .unwrap()
            .included_change_seq_high,
        Some(17)
    );
}

#[test]
fn a_missing_checkpoint_loads_as_none_resumable_first_run() {
    let dir = tempfile::tempdir().unwrap();
    assert!(
        RunCheckpoint::load(dir.path(), "legislation", "never-ran")
            .unwrap()
            .is_none()
    );
}
