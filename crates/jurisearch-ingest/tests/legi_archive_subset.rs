//! Ignored smoke over a local official LEGI archive.
//!
//! Run with `JURISEARCH_LEGI_ARCHIVE=/path/to/Freemium_legi_global_*.tar.gz cargo test -p jurisearch-ingest --test legi_archive_subset -- --ignored --nocapture`.
//! When the env var is absent, the test tries Pierre's local juridocs baseline and skips if it is not present.

use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    path::{Path, PathBuf},
};

use jurisearch_ingest::{
    archive::{ArchiveVisit, DEFAULT_MEMBER_BYTE_LIMIT, for_each_xml_member_until},
    legi::{ParsedLegiXml, parse_legi_member, source_payload_hash},
};

const DEFAULT_LEGI_ARCHIVE: &str =
    "/home/pierre/Apps/juridocs/opendata/LEGI/Freemium_legi_global_20250713-140000.tar.gz";
const ARTICLE_SAMPLE_TARGET: usize = 25;
const MAX_VISITED_MEMBERS: usize = 5_000;
const TEMPORAL_COVERAGE_MAX_VISITED_MEMBERS: usize = 250_000;
// ABROGE_DIFF remains covered by the focused unit fixture; it is not present in the current
// default local 2025-07-13 LEGI baseline within this capped real-data scan.
const TEMPORAL_STATUS_TARGETS: &[&str] = &[
    "VIGUEUR",
    "MODIFIE",
    "ABROGE",
    "ANNULE",
    "MODIFIE_MORT_NE",
    "PERIME",
    "TRANSFERE",
];
const FINITE_VALID_TO_STATUS_TARGETS: &[&str] =
    &["MODIFIE", "ABROGE", "ANNULE", "PERIME", "TRANSFERE"];

#[test]
#[ignore = "requires a local official LEGI tar.gz dump; run with --ignored"]
fn parses_real_archive_article_subset_with_raw_member_hashes() {
    let archive_path = archive_path();
    if !archive_path.exists() {
        eprintln!(
            "skipping LEGI archive subset smoke because `{}` does not exist",
            archive_path.display()
        );
        return;
    }

    let mut visited_members = 0usize;
    let mut article_attempts = 0usize;
    let mut parsed_articles = 0usize;
    let mut publisher_edges = 0usize;
    let mut articles_with_block_boundaries = 0usize;
    let mut saw_section_ta = false;
    let mut metadata_roots = BTreeSet::new();
    let mut unsupported_roots = BTreeSet::new();
    let mut parse_errors = Vec::new();

    let visited_xml = for_each_xml_member_until(
        archive_path.as_path(),
        DEFAULT_MEMBER_BYTE_LIMIT,
        |member| {
            visited_members += 1;
            let member_path = member.member_path.clone();
            match parse_legi_member(&member) {
                Ok(ParsedLegiXml::Article(document)) => {
                    article_attempts += 1;
                    parsed_articles += 1;
                    assert_eq!(
                        document.source_archive.as_deref(),
                        archive_file_name(&archive_path)
                    );
                    assert_eq!(
                        document.source_member_path.as_deref(),
                        Some(member_path.as_str())
                    );
                    assert_eq!(
                        document.source_payload_hash,
                        source_payload_hash(&member.bytes)
                    );
                    document.validate().unwrap_or_else(|error| {
                        panic!("{member_path} produced an invalid canonical document: {error}")
                    });
                    assert_eq!(document.chunks.len(), 1);
                    assert_eq!(document.chunks[0].body, document.body);
                    assert_eq!(document.chunks[0].chunking, "structural");
                    assert_eq!(document.chunks[0].boundary, "article");
                    if document.body.contains('\n') {
                        articles_with_block_boundaries += 1;
                    }
                    publisher_edges += document.publisher_edges.len();
                    for edge in &document.publisher_edges {
                        assert_eq!(edge.from_document_id, document.document_id);
                        assert_eq!(edge.edge_source, "publisher");
                        assert!(edge.edge_id.starts_with("publisher-edge:"));
                    }
                }
                Ok(ParsedLegiXml::UnsupportedRoot { root }) => {
                    unsupported_roots.insert(root);
                }
                Ok(ParsedLegiXml::TextVersion(_)) => {
                    metadata_roots.insert("TEXTE_VERSION");
                }
                Ok(ParsedLegiXml::SectionTa(_)) => {
                    saw_section_ta = true;
                    metadata_roots.insert("SECTION_TA");
                }
                Ok(ParsedLegiXml::TextStruct(_)) => {
                    metadata_roots.insert("TEXTELR");
                }
                Err(error) => {
                    if member_path.contains("/article/") {
                        article_attempts += 1;
                    }
                    parse_errors.push(format!("{member_path}: {error}"));
                }
            }

            Ok(
                if (article_attempts >= ARTICLE_SAMPLE_TARGET && saw_section_ta)
                    || visited_members >= MAX_VISITED_MEMBERS
                {
                    ArchiveVisit::Stop
                } else {
                    ArchiveVisit::Continue
                },
            )
        },
    )
    .unwrap();

    assert!(
        parse_errors.is_empty(),
        "unexpected parse errors in official LEGI sample:\n{}",
        parse_errors.join("\n")
    );
    assert!(
        parsed_articles >= ARTICLE_SAMPLE_TARGET,
        "expected at least {ARTICLE_SAMPLE_TARGET} ARTICLE members before stopping, got {parsed_articles}"
    );
    // The current 25-article default window is intentionally near the start of the baseline and
    // includes publisher links. If the default archive changes, adjust the window rather than
    // treating a link-free sample as proof that edge extraction is broken.
    assert!(
        publisher_edges > 0,
        "expected real LEGI article sample to emit publisher graph-edge candidates"
    );
    assert!(
        articles_with_block_boundaries > 0,
        "expected at least one sampled real article to preserve a structural body boundary"
    );
    assert!(
        saw_section_ta,
        "expected real LEGI sample to parse at least one SECTION_TA metadata root"
    );
    // The default baseline currently starts with text-version XML before articles. This keeps
    // metadata-root classification visible in the smoke, but the test remains ignored because
    // tar member ordering is source-archive-specific.
    assert!(
        metadata_roots.contains("TEXTELR"),
        "expected the sample to parse at least one TEXTELR metadata root before articles; visited {visited_xml} XML members, metadata roots: {metadata_roots:?}, unsupported roots: {unsupported_roots:?}"
    );
    eprintln!(
        "parsed {parsed_articles} ARTICLE members and {publisher_edges} publisher edges from `{}` after visiting {visited_xml} XML members; metadata roots: {metadata_roots:?}; unsupported roots: {unsupported_roots:?}",
        archive_path.display()
    );
}

#[test]
#[ignore = "requires a local official LEGI tar.gz dump; run with --ignored"]
fn real_archive_covers_article_status_and_temporal_variants() {
    let archive_path = archive_path();
    if !archive_path.exists() {
        eprintln!(
            "skipping LEGI temporal coverage smoke because `{}` does not exist",
            archive_path.display()
        );
        return;
    }

    let mut article_members = 0usize;
    let mut statuses = BTreeMap::<String, String>::new();
    let mut finite_valid_to_by_status = BTreeMap::<String, String>::new();
    let mut sentinel_2999_01_01 = None::<String>;
    let mut sentinel_2999_12_31 = None::<String>;
    let mut article_parse_errors = 0usize;
    let mut article_parse_error_samples = Vec::<String>::new();
    let mut visited_members = 0usize;

    let visited_xml = for_each_xml_member_until(
        archive_path.as_path(),
        DEFAULT_MEMBER_BYTE_LIMIT,
        |member| {
            visited_members += 1;
            let member_path = member.member_path.clone();
            match parse_legi_member(&member) {
                Ok(ParsedLegiXml::Article(document)) => {
                    let document = *document;
                    article_members += 1;
                    document.validate().unwrap_or_else(|error| {
                        panic!("{member_path} produced an invalid canonical document: {error}")
                    });

                    if let Some(status) = &document.source_status {
                        statuses
                            .entry(status.clone())
                            .or_insert_with(|| member_path.clone());
                        if document.valid_to.is_some() {
                            finite_valid_to_by_status
                                .entry(status.clone())
                                .or_insert_with(|| member_path.clone());
                        }
                    }
                    match (
                        document.valid_to.as_deref(),
                        document.valid_to_raw.as_deref(),
                    ) {
                        (None, Some("2999-01-01")) => {
                            sentinel_2999_01_01.get_or_insert_with(|| member_path.clone());
                        }
                        (None, Some("2999-12-31")) => {
                            sentinel_2999_12_31.get_or_insert_with(|| member_path.clone());
                        }
                        _ => {}
                    }
                }
                Ok(
                    ParsedLegiXml::UnsupportedRoot { .. }
                    | ParsedLegiXml::TextVersion(_)
                    | ParsedLegiXml::SectionTa(_)
                    | ParsedLegiXml::TextStruct(_),
                ) => {}
                Err(error) => {
                    if member_path.contains("/article/") {
                        article_parse_errors += 1;
                        if article_parse_error_samples.len() < 20 {
                            article_parse_error_samples.push(format!("{member_path}: {error}"));
                        }
                    }
                }
            }

            let coverage_complete = TEMPORAL_STATUS_TARGETS
                .iter()
                .all(|status| statuses.contains_key(*status))
                && FINITE_VALID_TO_STATUS_TARGETS
                    .iter()
                    .all(|status| finite_valid_to_by_status.contains_key(*status))
                && sentinel_2999_01_01.is_some();

            Ok(
                if coverage_complete || visited_members >= TEMPORAL_COVERAGE_MAX_VISITED_MEMBERS {
                    ArchiveVisit::Stop
                } else {
                    ArchiveVisit::Continue
                },
            )
        },
    )
    .unwrap();

    assert!(
        article_members > 0,
        "expected at least one ARTICLE member in `{}` after visiting {visited_xml} XML members",
        archive_path.display()
    );

    let missing_statuses = TEMPORAL_STATUS_TARGETS
        .iter()
        .filter(|status| !statuses.contains_key(**status))
        .copied()
        .collect::<Vec<_>>();
    assert!(
        missing_statuses.is_empty(),
        "missing expected LEGI statuses {missing_statuses:?}; observed {:?}",
        statuses.keys().collect::<Vec<_>>()
    );

    for status in FINITE_VALID_TO_STATUS_TARGETS {
        assert!(
            finite_valid_to_by_status.contains_key(*status),
            "expected a finite valid_to example for status `{status}`; finite statuses: {:?}",
            finite_valid_to_by_status.keys().collect::<Vec<_>>()
        );
    }
    assert!(
        sentinel_2999_01_01.is_some(),
        "expected at least one open-ended ARTICLE with DATE_FIN 2999-01-01"
    );
    let article_attempts = article_members + article_parse_errors;
    assert!(
        article_parse_errors * 100 <= article_attempts,
        "expected ARTICLE parse-error rate to stay at or below 1%, got {article_parse_errors}/{article_attempts}; samples: {article_parse_error_samples:?}"
    );

    eprintln!(
        "scanned {article_members} parsed ARTICLE members from `{}` after visiting {visited_xml} XML members; article parse errors: {article_parse_errors}; parse error samples: {:?}; statuses: {:?}; finite status examples: {:?}; 2999-01-01 sentinel: {:?}; 2999-12-31 sentinel: {:?}",
        archive_path.display(),
        article_parse_error_samples,
        statuses,
        finite_valid_to_by_status,
        sentinel_2999_01_01,
        sentinel_2999_12_31
    );
}

fn archive_path() -> PathBuf {
    env::var_os("JURISEARCH_LEGI_ARCHIVE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_LEGI_ARCHIVE))
}

fn archive_file_name(path: &Path) -> Option<&str> {
    path.file_name().and_then(|name| name.to_str())
}
