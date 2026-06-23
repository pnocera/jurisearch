//! Ignored smoke over local official DILA bulk jurisprudence archives (CASS/CAPP/INCA/JADE).
//!
//! Run with:
//!   `cargo test -p jurisearch-ingest --test juri_archive_subset -- --ignored --nocapture`
//! Override the archive directory with `JURISEARCH_JURI_OPENDATA=/path/to/opendata`.
//! When the archives are absent the test skips.

use std::{collections::BTreeSet, env, path::PathBuf};

use jurisearch_ingest::{
    archive::{ArchiveSource, ArchiveVisit, DEFAULT_MEMBER_BYTE_LIMIT, for_each_xml_member_until},
    juri::{ParsedJuriXml, parse_juri_member},
    legi::source_payload_hash,
};

const DEFAULT_OPENDATA: &str = "/home/pierre/Apps/juridocs/opendata";
const DECISION_SAMPLE_TARGET: usize = 20;
const MAX_VISITED_MEMBERS: usize = 4_000;

fn opendata_dir() -> PathBuf {
    PathBuf::from(env::var("JURISEARCH_JURI_OPENDATA").unwrap_or_else(|_| DEFAULT_OPENDATA.into()))
}

/// A small representative archive per source (delta archives are smallest).
fn sample_archives() -> Vec<(ArchiveSource, PathBuf)> {
    let root = opendata_dir();
    vec![
        (
            ArchiveSource::Cass,
            root.join("CASS/CASS_20250707-210159.tar.gz"),
        ),
        (
            ArchiveSource::Capp,
            root.join("CAPP/Freemium_capp_global_20250713-140000.tar.gz"),
        ),
        (
            ArchiveSource::Inca,
            root.join("INCA/Freemium_inca_global_20250713-140000.tar.gz"),
        ),
        (
            ArchiveSource::Jade,
            root.join("JADE/Freemium_jade_global_20250713-140000.tar.gz"),
        ),
    ]
}

#[test]
#[ignore = "requires local official DILA jurisprudence tar.gz dumps; run with --ignored"]
fn parses_real_jurisprudence_subsets() {
    let mut any_archive_seen = false;
    for (source, archive_path) in sample_archives() {
        if !archive_path.exists() {
            eprintln!(
                "skipping {source} subset because `{}` does not exist",
                archive_path.display()
            );
            continue;
        }
        any_archive_seen = true;

        let mut visited = 0usize;
        let mut parsed_decisions = 0usize;
        let mut publisher_edges = 0usize;
        let mut decisions_with_summary = 0usize;
        let mut empty_body = 0usize;
        let mut natures = BTreeSet::new();
        let mut unsupported_roots = BTreeSet::new();

        let archive_name = archive_path
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned);

        for_each_xml_member_until(archive_path.as_path(), DEFAULT_MEMBER_BYTE_LIMIT, |member| {
            visited += 1;
            let member_path = member.member_path.clone();
            match parse_juri_member(source, &member) {
                Ok(ParsedJuriXml::Decision(decision)) => {
                    parsed_decisions += 1;
                    assert_eq!(decision.source, source.as_str());
                    assert_eq!(decision.source_archive, archive_name);
                    assert_eq!(decision.source_member_path.as_deref(), Some(member_path.as_str()));
                    assert_eq!(
                        decision.source_payload_hash,
                        source_payload_hash(&member.bytes)
                    );
                    decision.validate().unwrap_or_else(|error| {
                        panic!("{member_path} produced an invalid canonical decision: {error}")
                    });
                    assert!(!decision.chunks.is_empty());
                    for chunk in &decision.chunks {
                        assert_eq!(chunk.chunking, "heuristic");
                    }
                    if let Some(nature) = &decision.nature {
                        natures.insert(nature.clone());
                    }
                    if !decision.summaries.is_empty() {
                        decisions_with_summary += 1;
                    }
                    publisher_edges += decision.publisher_edges.len();
                }
                Ok(ParsedJuriXml::UnsupportedRoot { root }) => {
                    unsupported_roots.insert(root);
                }
                // Empty-body (metadata-only) decisions are a legitimate skip in real data.
                Err(jurisearch_ingest::juri::JuriParseError::EmptyBody { .. }) => {
                    empty_body += 1;
                }
                Err(error) => panic!("{member_path} failed to parse: {error}"),
            }

            if parsed_decisions >= DECISION_SAMPLE_TARGET || visited >= MAX_VISITED_MEMBERS {
                Ok(ArchiveVisit::Stop)
            } else {
                Ok(ArchiveVisit::Continue)
            }
        })
        .unwrap_or_else(|error| panic!("failed to stream {}: {error}", archive_path.display()));

        eprintln!(
            "{source}: visited {visited}, decisions {parsed_decisions}, summaries {decisions_with_summary}, empty_body {empty_body}, edges {publisher_edges}, natures {natures:?}, unsupported {unsupported_roots:?}"
        );
        assert!(
            parsed_decisions > 0,
            "expected at least one decision from {}",
            archive_path.display()
        );
    }

    if !any_archive_seen {
        eprintln!("skipping jurisprudence subset smoke: no local archives present");
    }
}
