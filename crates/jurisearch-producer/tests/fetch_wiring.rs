//! No-infra acceptance gates for the producer's fetch wiring (driven through the generic seam with
//! fixture listings + archive bytes — NO network):
//! - fetching the same source twice is a no-op after the first complete download;
//! - a corrupt/truncated download is quarantined and does NOT advance the fetch cursor;
//! - dry-run reports what WOULD be downloaded without writing the mirror.

use std::collections::HashMap;
use std::path::Path;

use flate2::{Compression, write::GzEncoder};
use jurisearch_fetch::{ArchiveDownloader, ArchiveSource, DirectoryLister, FetchError};
use jurisearch_producer::PRODUCER_CONFIG_EXAMPLE;
use jurisearch_producer::config::ProducerConfig;
use jurisearch_producer::fetch::fetch_source_with;

#[derive(Default)]
struct FixtureClient {
    listing: String,
    archives: HashMap<String, Vec<u8>>,
}

impl DirectoryLister for FixtureClient {
    fn fetch_index(&self, _source: ArchiveSource) -> Result<String, FetchError> {
        Ok(self.listing.clone())
    }
}

impl ArchiveDownloader for FixtureClient {
    fn download_to(
        &self,
        _source: ArchiveSource,
        file_name: &str,
        dest: &Path,
    ) -> Result<(), FetchError> {
        let bytes = self
            .archives
            .get(file_name)
            .cloned()
            .unwrap_or_else(|| panic!("fixture missing archive `{file_name}`"));
        std::fs::write(dest, bytes).map_err(|source| FetchError::Io {
            path: dest.to_path_buf(),
            source,
        })
    }
}

fn make_targz(member: &str, data: &[u8]) -> Vec<u8> {
    let encoder = GzEncoder::new(Vec::new(), Compression::default());
    let mut builder = tar::Builder::new(encoder);
    let mut header = tar::Header::new_gnu();
    header.set_size(data.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder.append_data(&mut header, member, data).unwrap();
    builder.into_inner().unwrap().finish().unwrap()
}

fn apache_index(files: &[&str]) -> String {
    let mut html = String::from("<html><body><pre>\n");
    for file in files {
        html.push_str(&format!(
            "<a href=\"{file}\">{file}</a> 2026-06-28 12:00 1.0M\n"
        ));
    }
    html.push_str("</pre></body></html>\n");
    html
}

/// A producer config pointing every path under `root` (no secrets needed for the fetch path).
fn config_under(root: &Path) -> ProducerConfig {
    let secrets = root.join("secrets");
    std::fs::create_dir_all(&secrets).unwrap();
    use std::os::unix::fs::PermissionsExt;
    for name in ["postgres-admin-password", "jurisearch-write-password"] {
        let p = secrets.join(name);
        std::fs::write(&p, "x").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let seed = secrets.join("producer-signing.seed");
    std::fs::write(&seed, "00".repeat(32)).unwrap();
    std::fs::set_permissions(&seed, std::fs::Permissions::from_mode(0o600)).unwrap();

    let toml = PRODUCER_CONFIG_EXAMPLE
        .replace("/etc/jurisearch/secrets", secrets.to_str().unwrap())
        .replace(
            "/srv/jurisearch/storebox/archives",
            root.join("archives").to_str().unwrap(),
        )
        .replace(
            "/srv/jurisearch/storebox/packages",
            root.join("packages").to_str().unwrap(),
        )
        .replace(
            "/var/lib/jurisearch-producer",
            root.join("state").to_str().unwrap(),
        );
    ProducerConfig::parse_str(&toml, Path::new("producer.toml")).unwrap()
}

const BASELINE: &str = "Freemium_legi_global_20250713-140000.tar.gz";
const DELTA: &str = "LEGI_20260628-200000.tar.gz";

#[test]
fn fetching_the_same_source_twice_is_a_no_op_after_first_download() {
    let root = tempfile::tempdir().unwrap();
    let config = config_under(root.path());
    let mut archives = HashMap::new();
    archives.insert(BASELINE.to_owned(), make_targz("legi/a.xml", b"<X/>"));
    archives.insert(DELTA.to_owned(), make_targz("legi/b.xml", b"<X/>"));
    let client = FixtureClient {
        listing: apache_index(&[BASELINE, DELTA]),
        archives,
    };

    // First run downloads both new archives and advances the cursor to the delta timestamp.
    let first = fetch_source_with(&config, ArchiveSource::Legi, false, &client, &client).unwrap();
    assert_eq!(first.planned_or_downloaded.len(), 2, "{first:?}");
    assert!(first.quarantined.is_empty());
    assert_eq!(
        first.cursor.latest_compact_timestamp.as_deref(),
        Some("20260628200000")
    );

    // Second run is a complete no-op: nothing downloaded, both already present.
    let second = fetch_source_with(&config, ArchiveSource::Legi, false, &client, &client).unwrap();
    assert!(second.planned_or_downloaded.is_empty(), "{second:?}");
    assert_eq!(second.already_present.len(), 2);
    assert_eq!(
        second.cursor, first.cursor,
        "cursor unchanged on a no-op re-run"
    );
}

#[test]
fn corrupt_download_is_quarantined_and_does_not_advance_the_cursor() {
    let root = tempfile::tempdir().unwrap();
    let config = config_under(root.path());
    let mut archives = HashMap::new();
    // The baseline is valid; the delta is NOT a gzip stream at all.
    archives.insert(BASELINE.to_owned(), make_targz("legi/a.xml", b"<X/>"));
    archives.insert(DELTA.to_owned(), b"not a gzip stream".to_vec());
    let client = FixtureClient {
        listing: apache_index(&[BASELINE, DELTA]),
        archives,
    };

    let report = fetch_source_with(&config, ArchiveSource::Legi, false, &client, &client).unwrap();
    assert_eq!(report.planned_or_downloaded, vec![BASELINE.to_owned()]);
    assert_eq!(report.quarantined, vec![DELTA.to_owned()]);
    // The cursor advanced ONLY to the baseline — the corrupt delta did not move it.
    assert_eq!(
        report.cursor.latest_file_name.as_deref(),
        Some(BASELINE),
        "the quarantined delta must NOT advance the cursor"
    );
}

#[test]
fn dry_run_reports_plan_without_writing_the_mirror() {
    let root = tempfile::tempdir().unwrap();
    let config = config_under(root.path());
    let client = FixtureClient {
        listing: apache_index(&[BASELINE, DELTA]),
        archives: HashMap::new(), // a dry run never downloads, so no bytes are needed.
    };

    let report = fetch_source_with(&config, ArchiveSource::Legi, true, &client, &client).unwrap();
    assert!(report.dry_run);
    assert_eq!(report.planned_or_downloaded.len(), 2);
    assert!(
        report.cursor.latest_file_name.is_none(),
        "dry-run advances nothing"
    );
    // No mirror directory was written.
    assert!(
        !root
            .path()
            .join("archives")
            .join("legi")
            .join(BASELINE)
            .exists()
    );
}
