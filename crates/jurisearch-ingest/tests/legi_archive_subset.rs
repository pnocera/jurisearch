use std::{
    collections::BTreeSet,
    env,
    fmt::Write as _,
    path::{Path, PathBuf},
};

use jurisearch_ingest::{
    archive::{ArchiveVisit, DEFAULT_MEMBER_BYTE_LIMIT, for_each_xml_member_until},
    legi::{ParsedLegiXml, parse_legi_member},
};
use sha2::{Digest, Sha256};

const DEFAULT_LEGI_ARCHIVE: &str =
    "/home/pierre/Apps/juridocs/opendata/LEGI/Freemium_legi_global_20250713-140000.tar.gz";
const ARTICLE_SAMPLE_TARGET: usize = 25;
const MAX_VISITED_MEMBERS: usize = 5_000;

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
                    assert_eq!(document.source_payload_hash, sha256_hex(&member.bytes));
                    document.validate().unwrap_or_else(|error| {
                        panic!("{member_path} produced an invalid canonical document: {error}")
                    });
                }
                Ok(ParsedLegiXml::UnsupportedRoot { root }) => {
                    unsupported_roots.insert(root);
                }
                Err(error) => {
                    if member_path.contains("/article/") {
                        article_attempts += 1;
                    }
                    parse_errors.push(format!("{member_path}: {error}"));
                }
            }

            Ok(
                if article_attempts >= ARTICLE_SAMPLE_TARGET
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
    assert_eq!(parsed_articles, ARTICLE_SAMPLE_TARGET);
    assert!(
        unsupported_roots.contains("TEXTELR"),
        "expected the sample to classify at least one TEXTELR root before articles; visited {visited_xml} XML members, unsupported roots: {unsupported_roots:?}"
    );
    eprintln!(
        "parsed {parsed_articles} ARTICLE members from `{}` after visiting {visited_xml} XML members; unsupported roots: {unsupported_roots:?}",
        archive_path.display()
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

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut encoded = String::with_capacity("sha256:".len() + digest.len() * 2);
    encoded.push_str("sha256:");
    for byte in digest {
        write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
    }
    encoded
}
