//! Integration tests for `jurisearch-fetch`, fixture-only (NO network).
//!
//! Covers the M2-A acceptance gates:
//! * fetching the same source twice is a no-op after the first complete download;
//! * a corrupt/truncated download is quarantined and does NOT advance the cursor;
//! * archive selection is by DILA `ArchiveTimestamp`/name + per-archive state,
//!   NOT by package `change_seq`;
//! * the listing parser rejects/ignores cross-source or malformed entries.

mod support;

use jurisearch_fetch::{
    ArchiveKind, ArchiveSource, FetchConfig, FetchCursor, Fetcher, IntegrityError,
    parse_apache_index, parse_source_listing, verify_targz,
};
use support::{
    FixtureClient, Layout, corrupt_archive, footer_corrupt_archive, make_targz, mirror_files,
    quarantine_files, truncated_archive, valid_archive,
};

/// Modern Apache `HTMLTable` listing for LEGI. Deliberately contaminated with a
/// cross-source CASS delta, a parent-dir link, sort links, and a stray text file
/// — all of which must be ignored.
fn legi_table_html() -> &'static str {
    r#"<!DOCTYPE html>
<html><head><title>Index of /OPENDATA/LEGI</title></head><body>
<h1>Index of /OPENDATA/LEGI</h1>
<table>
 <tr><th><a href="?C=N;O=D">Name</a></th><th><a href="?C=M;O=A">Last modified</a></th><th><a href="?C=S;O=A">Size</a></th></tr>
 <tr><th colspan="3"><hr></th></tr>
 <tr><td><a href="/OPENDATA/">Parent Directory</a></td><td>&nbsp;</td><td align="right">  - </td></tr>
 <tr><td><a href="Freemium_legi_global_20250713-140000.tar.gz">Freemium_legi_global_20250713-140000.tar.gz</a></td><td align="right">2025-07-13 14:05  </td><td align="right">1.1G</td></tr>
 <tr><td><a href="LEGI_20250715-060000.tar.gz">LEGI_20250715-060000.tar.gz</a></td><td align="right">2025-07-15 06:12  </td><td align="right">2.0M</td></tr>
 <tr><td><a href="LEGI_20250714-000000.tar.gz">LEGI_20250714-000000.tar.gz</a></td><td align="right">2025-07-14 00:10  </td><td align="right"> 42M</td></tr>
 <tr><td><a href="CASS_20250714-000000.tar.gz">CASS_20250714-000000.tar.gz</a></td><td align="right">2025-07-14 00:10  </td><td align="right">400K</td></tr>
 <tr><td><a href="notes.txt">notes.txt</a></td><td align="right">2025-07-14 00:10  </td><td align="right">12</td></tr>
 <tr><th colspan="3"><hr></th></tr>
</table></body></html>"#
}

/// Classic `<pre>`-formatted listing for CASS.
fn cass_pre_html() -> &'static str {
    "<html><head><title>Index of /OPENDATA/CASS</title></head><body>\n\
<h1>Index of /OPENDATA/CASS</h1>\n\
<pre><img src=\"/icons/blank.gif\" alt=\"Icon \"> <a href=\"?C=N;O=D\">Name</a> <a href=\"?C=M;O=A\">Last modified</a> <a href=\"?C=S;O=A\">Size</a>\n\
<hr><img src=\"/icons/back.gif\" alt=\"[PARENTDIR]\"> <a href=\"/OPENDATA/\">Parent Directory</a>                             -   \n\
<img src=\"/icons/compressed.gif\" alt=\"[   ]\"> <a href=\"Freemium_cass_global_20250713-140000.tar.gz\">Freemium_cass_global_20250713-140000.tar.gz</a> 2025-07-13 14:05  248M  \n\
<img src=\"/icons/compressed.gif\" alt=\"[   ]\"> <a href=\"CASS_20250721-212334.tar.gz\">CASS_20250721-212334.tar.gz</a>      2025-07-21 21:23  484K  \n\
<hr></pre></body></html>"
}

// ---------------------------------------------------------------------------
// Listing parser
// ---------------------------------------------------------------------------

#[test]
fn parses_table_listing_and_filters_to_source() {
    let archives = parse_source_listing(ArchiveSource::Legi, legi_table_html());
    let names: Vec<&str> = archives.iter().map(|a| a.file_name()).collect();

    // Cross-source CASS delta, parent dir, sort links, and notes.txt are gone;
    // LEGI archives are ordered by ascending ArchiveTimestamp.
    assert_eq!(
        names,
        vec![
            "Freemium_legi_global_20250713-140000.tar.gz",
            "LEGI_20250714-000000.tar.gz",
            "LEGI_20250715-060000.tar.gz",
        ]
    );

    let baseline = &archives[0];
    assert_eq!(baseline.parsed.kind, ArchiveKind::Baseline);
    assert_eq!(baseline.parsed.timestamp.compact(), "20250713140000");
    // Size column is captured (approximate, informational only).
    assert_eq!(baseline.size.as_deref(), Some("1.1G"));
    assert_eq!(baseline.last_modified.as_deref(), Some("2025-07-13 14:05"));
}

#[test]
fn parses_pre_formatted_listing() {
    let archives = parse_source_listing(ArchiveSource::Cass, cass_pre_html());
    let names: Vec<&str> = archives.iter().map(|a| a.file_name()).collect();
    assert_eq!(
        names,
        vec![
            "Freemium_cass_global_20250713-140000.tar.gz",
            "CASS_20250721-212334.tar.gz",
        ]
    );
    assert_eq!(archives[1].size.as_deref(), Some("484K"));
}

#[test]
fn cross_source_listing_yields_nothing_for_wrong_source() {
    // Asking for JADE against a LEGI/CASS-only listing returns zero archives.
    assert!(parse_source_listing(ArchiveSource::Jade, legi_table_html()).is_empty());
    // The CASS delta present in the LEGI listing is not selected for LEGI.
    let legi = parse_source_listing(ArchiveSource::Legi, legi_table_html());
    assert!(!legi.iter().any(|a| a.file_name().starts_with("CASS_")));
}

#[test]
fn raw_index_parser_drops_navigation_and_directories() {
    let entries = parse_apache_index(legi_table_html());
    let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
    // No parent-dir, no `?C=...` sort links.
    assert!(!names.iter().any(|n| n.contains("OPENDATA")));
    assert!(!names.iter().any(|n| n.starts_with('?')));
    // Real file rows survive (including the not-our-source ones, pre-filter).
    assert!(names.contains(&"CASS_20250714-000000.tar.gz"));
    assert!(names.contains(&"notes.txt"));
}

#[test]
fn empty_listing_yields_no_archives() {
    let html = "<html><body><h1>Index of /OPENDATA/LEGI</h1><pre></pre></body></html>";
    assert!(parse_source_listing(ArchiveSource::Legi, html).is_empty());
}

// ---------------------------------------------------------------------------
// Integrity gate
// ---------------------------------------------------------------------------

#[test]
fn valid_targz_passes_integrity() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ok.tar.gz");
    std::fs::write(&path, valid_archive()).unwrap();

    let report = verify_targz(&path, None).expect("valid archive accepted");
    assert_eq!(report.members, 1);
    assert!(report.sha256.starts_with("sha256:"));
    assert!(report.size_bytes > 0);
}

#[test]
fn truncated_targz_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("trunc.tar.gz");
    std::fs::write(&path, truncated_archive()).unwrap();

    let err = verify_targz(&path, None).expect_err("truncated archive rejected");
    assert!(matches!(err, IntegrityError::Corrupt { .. }));
}

#[test]
fn footer_corrupt_targz_is_rejected() {
    // All tar members are readable, but the gzip trailer (CRC-32 + ISIZE) is
    // corrupted. The tar iterator stops at the end-of-archive marker before the
    // trailer is read, so only a full drain of the gzip decoder to EOF catches
    // this end-truncation/footer-corruption case.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("footer.tar.gz");
    std::fs::write(&path, footer_corrupt_archive()).unwrap();

    let err = verify_targz(&path, None).expect_err("footer-corrupt archive rejected");
    assert!(matches!(err, IntegrityError::Corrupt { .. }));
}

#[test]
fn corrupt_gzip_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("corrupt.tar.gz");
    std::fs::write(&path, corrupt_archive()).unwrap();

    let err = verify_targz(&path, None).expect_err("corrupt gzip rejected");
    assert!(matches!(err, IntegrityError::Corrupt { .. }));
}

#[test]
fn empty_download_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("empty.tar.gz");
    std::fs::write(&path, Vec::<u8>::new()).unwrap();

    let err = verify_targz(&path, None).expect_err("empty file rejected");
    assert!(matches!(err, IntegrityError::Empty { .. }));
}

#[test]
fn size_mismatch_is_rejected_before_decompression() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ok.tar.gz");
    let bytes = valid_archive();
    let actual = bytes.len() as u64;
    std::fs::write(&path, bytes).unwrap();

    let err = verify_targz(&path, Some(actual + 10)).expect_err("size mismatch rejected");
    assert!(matches!(err, IntegrityError::SizeMismatch { .. }));
    // The exact-matching size still passes.
    assert!(verify_targz(&path, Some(actual)).is_ok());
}

// ---------------------------------------------------------------------------
// Cursor persistence
// ---------------------------------------------------------------------------

#[test]
fn cursor_roundtrips_and_records_only_via_record() {
    let dir = tempfile::tempdir().unwrap();
    let state = dir.path();

    // A brand-new cursor is empty.
    let loaded = FetchCursor::load(state, ArchiveSource::Legi).unwrap();
    assert!(loaded.fetched.is_empty());
    assert!(loaded.baseline_file_name.is_none());

    let mut cursor = FetchCursor::new(ArchiveSource::Legi);
    let baseline_path = dir
        .path()
        .join("Freemium_legi_global_20250713-140000.tar.gz");
    std::fs::write(&baseline_path, valid_archive()).unwrap();
    let parsed = jurisearch_fetch::ParsedArchive::parse_file_name(
        ArchiveSource::Legi,
        "Freemium_legi_global_20250713-140000.tar.gz",
    )
    .unwrap();
    let report = verify_targz(&baseline_path, None).unwrap();
    cursor.record(&parsed, &report);
    cursor.save(state).unwrap();

    let reloaded = FetchCursor::load(state, ArchiveSource::Legi).unwrap();
    assert!(reloaded.is_fetched("Freemium_legi_global_20250713-140000.tar.gz"));
    assert_eq!(
        reloaded.baseline_file_name.as_deref(),
        Some("Freemium_legi_global_20250713-140000.tar.gz")
    );
    assert_eq!(
        reloaded.highest_timestamp().map(|t| t.compact().to_owned()),
        Some("20250713140000".to_owned())
    );
}

// ---------------------------------------------------------------------------
// Engine: end-to-end acceptance gates
// ---------------------------------------------------------------------------

fn legi_client() -> FixtureClient {
    FixtureClient::new()
        .with_listing(ArchiveSource::Legi, legi_table_html())
        .with_archive(
            "Freemium_legi_global_20250713-140000.tar.gz",
            valid_archive(),
        )
        .with_archive("LEGI_20250714-000000.tar.gz", valid_archive())
        .with_archive("LEGI_20250715-060000.tar.gz", valid_archive())
}

#[test]
fn dry_run_plan_lists_without_downloading() {
    let root = tempfile::tempdir().unwrap();
    let layout = Layout::under(root.path());
    let client = legi_client();
    let fetcher = Fetcher::new(&client, &client);
    let cfg = FetchConfig {
        source: ArchiveSource::Legi,
        archives_dir: &layout.archives_dir,
        quarantine_dir: &layout.quarantine_dir,
        state_dir: &layout.state_dir,
    };

    let plan = fetcher.plan(&cfg).unwrap();
    let names: Vec<&str> = plan.to_fetch.iter().map(|a| a.file_name()).collect();
    assert_eq!(
        names,
        vec![
            "Freemium_legi_global_20250713-140000.tar.gz",
            "LEGI_20250714-000000.tar.gz",
            "LEGI_20250715-060000.tar.gz",
        ]
    );
    assert_eq!(plan.listing_total, 3);
    assert!(plan.already_fetched.is_empty());
    // Dry-run wrote nothing.
    assert!(mirror_files(&layout.archives_dir, ArchiveSource::Legi).is_empty());
    assert!(!layout.state_dir.join("fetch-cursor-legi.json").exists());
}

#[test]
fn fetching_twice_is_a_no_op_after_first_complete_download() {
    let root = tempfile::tempdir().unwrap();
    let layout = Layout::under(root.path());
    let client = legi_client();
    let fetcher = Fetcher::new(&client, &client);
    let cfg = FetchConfig {
        source: ArchiveSource::Legi,
        archives_dir: &layout.archives_dir,
        quarantine_dir: &layout.quarantine_dir,
        state_dir: &layout.state_dir,
    };

    let first = fetcher.run(&cfg).unwrap();
    assert_eq!(first.downloaded.len(), 3);
    assert!(first.quarantined.is_empty());
    assert_eq!(
        mirror_files(&layout.archives_dir, ArchiveSource::Legi),
        vec![
            "Freemium_legi_global_20250713-140000.tar.gz".to_owned(),
            "LEGI_20250714-000000.tar.gz".to_owned(),
            "LEGI_20250715-060000.tar.gz".to_owned(),
        ]
    );

    // Second run: nothing new on the server, so nothing downloaded.
    let second = fetcher.run(&cfg).unwrap();
    assert!(second.downloaded.is_empty(), "re-run must download nothing");
    assert!(second.quarantined.is_empty());
    assert_eq!(second.already_present.len(), 3);
}

#[test]
fn corrupt_download_is_quarantined_and_does_not_advance_cursor() {
    let root = tempfile::tempdir().unwrap();
    let layout = Layout::under(root.path());
    // The middle delta is corrupt; baseline + later delta are valid.
    let client = FixtureClient::new()
        .with_listing(ArchiveSource::Legi, legi_table_html())
        .with_archive(
            "Freemium_legi_global_20250713-140000.tar.gz",
            valid_archive(),
        )
        .with_archive("LEGI_20250714-000000.tar.gz", truncated_archive())
        .with_archive("LEGI_20250715-060000.tar.gz", valid_archive());
    let fetcher = Fetcher::new(&client, &client);
    let cfg = FetchConfig {
        source: ArchiveSource::Legi,
        archives_dir: &layout.archives_dir,
        quarantine_dir: &layout.quarantine_dir,
        state_dir: &layout.state_dir,
    };

    let outcome = fetcher.run(&cfg).unwrap();
    assert_eq!(outcome.downloaded.len(), 2);
    assert_eq!(outcome.quarantined.len(), 1);
    assert_eq!(
        outcome.quarantined[0].file_name,
        "LEGI_20250714-000000.tar.gz"
    );

    // The corrupt file is in quarantine, not the mirror.
    assert_eq!(
        quarantine_files(&layout.quarantine_dir, ArchiveSource::Legi),
        vec!["LEGI_20250714-000000.tar.gz".to_owned()]
    );
    assert!(
        !mirror_files(&layout.archives_dir, ArchiveSource::Legi)
            .contains(&"LEGI_20250714-000000.tar.gz".to_owned())
    );

    // The cursor did NOT record the corrupt archive.
    let cursor = FetchCursor::load(&layout.state_dir, ArchiveSource::Legi).unwrap();
    assert!(!cursor.is_fetched("LEGI_20250714-000000.tar.gz"));
    assert!(cursor.is_fetched("Freemium_legi_global_20250713-140000.tar.gz"));
    assert!(cursor.is_fetched("LEGI_20250715-060000.tar.gz"));

    // A retry that now serves valid bytes for the previously-corrupt file picks
    // it up (it was never marked fetched) and completes the mirror.
    let repaired = FixtureClient::new()
        .with_listing(ArchiveSource::Legi, legi_table_html())
        .with_archive(
            "Freemium_legi_global_20250713-140000.tar.gz",
            valid_archive(),
        )
        .with_archive("LEGI_20250714-000000.tar.gz", valid_archive())
        .with_archive("LEGI_20250715-060000.tar.gz", valid_archive());
    let fetcher = Fetcher::new(&repaired, &repaired);
    let retry = fetcher.run(&cfg).unwrap();
    assert_eq!(retry.downloaded.len(), 1);
    assert_eq!(retry.downloaded[0].file_name, "LEGI_20250714-000000.tar.gz");
    let cursor = FetchCursor::load(&layout.state_dir, ArchiveSource::Legi).unwrap();
    assert!(cursor.is_fetched("LEGI_20250714-000000.tar.gz"));
}

#[test]
fn footer_corrupt_download_is_quarantined_and_does_not_advance_cursor() {
    // End-to-end mirror of the corrupt-download gate, but for an archive whose
    // tar members are all readable while only the gzip footer is corrupted.
    // Without draining the gzip stream to EOF this would slip through the gate
    // and wrongly advance the cursor.
    let root = tempfile::tempdir().unwrap();
    let layout = Layout::under(root.path());
    let client = FixtureClient::new()
        .with_listing(ArchiveSource::Legi, legi_table_html())
        .with_archive(
            "Freemium_legi_global_20250713-140000.tar.gz",
            valid_archive(),
        )
        .with_archive("LEGI_20250714-000000.tar.gz", footer_corrupt_archive())
        .with_archive("LEGI_20250715-060000.tar.gz", valid_archive());
    let fetcher = Fetcher::new(&client, &client);
    let cfg = FetchConfig {
        source: ArchiveSource::Legi,
        archives_dir: &layout.archives_dir,
        quarantine_dir: &layout.quarantine_dir,
        state_dir: &layout.state_dir,
    };

    let outcome = fetcher.run(&cfg).unwrap();
    assert_eq!(outcome.downloaded.len(), 2);
    assert_eq!(outcome.quarantined.len(), 1);
    assert_eq!(
        outcome.quarantined[0].file_name,
        "LEGI_20250714-000000.tar.gz"
    );

    // The footer-corrupt file is quarantined, not mirrored.
    assert_eq!(
        quarantine_files(&layout.quarantine_dir, ArchiveSource::Legi),
        vec!["LEGI_20250714-000000.tar.gz".to_owned()]
    );
    assert!(
        !mirror_files(&layout.archives_dir, ArchiveSource::Legi)
            .contains(&"LEGI_20250714-000000.tar.gz".to_owned())
    );

    // The cursor did NOT record the footer-corrupt archive.
    let cursor = FetchCursor::load(&layout.state_dir, ArchiveSource::Legi).unwrap();
    assert!(!cursor.is_fetched("LEGI_20250714-000000.tar.gz"));
    assert!(cursor.is_fetched("Freemium_legi_global_20250713-140000.tar.gz"));
    assert!(cursor.is_fetched("LEGI_20250715-060000.tar.gz"));
}

#[test]
fn selection_keys_on_archive_timestamp_not_package_sequence() {
    // Gate: archive selection is by DILA ArchiveTimestamp/name + per-archive
    // cursor state, never by any package change_seq. We prove this two ways:
    //   (1) a NEW delta whose timestamp is *later* than everything fetched is
    //       selected purely because its name is absent from the cursor; and
    //   (2) ordering of the to-fetch set follows ArchiveTimestamp, regardless of
    //       the order the listing presents them.
    let root = tempfile::tempdir().unwrap();
    let layout = Layout::under(root.path());

    let client = legi_client();
    let fetcher = Fetcher::new(&client, &client);
    let cfg = FetchConfig {
        source: ArchiveSource::Legi,
        archives_dir: &layout.archives_dir,
        quarantine_dir: &layout.quarantine_dir,
        state_dir: &layout.state_dir,
    };
    // First run mirrors the baseline + the two known deltas.
    fetcher.run(&cfg).unwrap();

    // Now a brand-new delta lands on DILA, dated AFTER the last fetched one.
    // There is no package/change_seq input anywhere; selection is name+timestamp.
    let mut html = legi_table_html().replace(
        "</table>",
        "<tr><td><a href=\"LEGI_20250716-013000.tar.gz\">LEGI_20250716-013000.tar.gz</a></td><td align=\"right\">2025-07-16 01:30  </td><td align=\"right\">1.0M</td></tr>\n</table>",
    );
    // Sanity: the injected row is present.
    assert!(html.contains("LEGI_20250716-013000.tar.gz"));
    // Leak the string for a 'static fixture client.
    let html: &'static str = Box::leak(std::mem::take(&mut html).into_boxed_str());

    let client2 = FixtureClient::new()
        .with_listing(ArchiveSource::Legi, html)
        .with_archive(
            "Freemium_legi_global_20250713-140000.tar.gz",
            valid_archive(),
        )
        .with_archive("LEGI_20250714-000000.tar.gz", valid_archive())
        .with_archive("LEGI_20250715-060000.tar.gz", valid_archive())
        .with_archive("LEGI_20250716-013000.tar.gz", valid_archive());
    let fetcher2 = Fetcher::new(&client2, &client2);

    let plan = fetcher2.plan(&cfg).unwrap();
    let names: Vec<&str> = plan.to_fetch.iter().map(|a| a.file_name()).collect();
    // Only the genuinely-new, later-timestamped delta is selected.
    assert_eq!(names, vec!["LEGI_20250716-013000.tar.gz"]);
    assert_eq!(plan.already_fetched.len(), 3);

    let outcome = fetcher2.run(&cfg).unwrap();
    assert_eq!(outcome.downloaded.len(), 1);
    assert_eq!(
        outcome.downloaded[0].file_name,
        "LEGI_20250716-013000.tar.gz"
    );

    // The cursor's high-water mark is an ArchiveTimestamp, not a sequence.
    let cursor = FetchCursor::load(&layout.state_dir, ArchiveSource::Legi).unwrap();
    assert_eq!(
        cursor.highest_timestamp().map(|t| t.compact().to_owned()),
        Some("20250716013000".to_owned())
    );
}

#[test]
fn engine_never_downloads_cross_source_or_malformed_entries() {
    let root = tempfile::tempdir().unwrap();
    let layout = Layout::under(root.path());
    // Note: the client has NO bytes registered for CASS_/notes.txt. If the engine
    // tried to download them, the fixture downloader would panic. The run
    // succeeding proves they were filtered out of the plan entirely.
    let client = legi_client();
    let fetcher = Fetcher::new(&client, &client);
    let cfg = FetchConfig {
        source: ArchiveSource::Legi,
        archives_dir: &layout.archives_dir,
        quarantine_dir: &layout.quarantine_dir,
        state_dir: &layout.state_dir,
    };

    let outcome = fetcher.run(&cfg).unwrap();
    let names: Vec<&str> = outcome
        .downloaded
        .iter()
        .map(|d| d.file_name.as_str())
        .collect();
    assert!(!names.iter().any(|n| n.starts_with("CASS_")));
    assert!(!names.contains(&"notes.txt"));
    assert_eq!(names.len(), 3);
}

#[test]
fn jurisprudence_pre_listing_drives_a_full_fetch() {
    let root = tempfile::tempdir().unwrap();
    let layout = Layout::under(root.path());
    let client = FixtureClient::new()
        .with_listing(ArchiveSource::Cass, cass_pre_html())
        .with_archive(
            "Freemium_cass_global_20250713-140000.tar.gz",
            make_targz(&[("cass/decision.xml", b"<DEC>x</DEC>")]),
        )
        .with_archive(
            "CASS_20250721-212334.tar.gz",
            make_targz(&[("cass/delta.xml", b"<DEC>y</DEC>")]),
        );
    let fetcher = Fetcher::new(&client, &client);
    let cfg = FetchConfig {
        source: ArchiveSource::Cass,
        archives_dir: &layout.archives_dir,
        quarantine_dir: &layout.quarantine_dir,
        state_dir: &layout.state_dir,
    };

    let outcome = fetcher.run(&cfg).unwrap();
    assert_eq!(outcome.downloaded.len(), 2);
    assert_eq!(
        mirror_files(&layout.archives_dir, ArchiveSource::Cass),
        vec![
            "CASS_20250721-212334.tar.gz".to_owned(),
            "Freemium_cass_global_20250713-140000.tar.gz".to_owned(),
        ]
    );
}
