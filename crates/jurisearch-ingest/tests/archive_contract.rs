use std::{
    fs::File,
    path::{Path, PathBuf},
};

use flate2::{Compression, write::GzEncoder};
use jurisearch_ingest::archive::{
    ArchiveReadError, ArchiveSource, for_each_xml_member, plan_from_paths,
};
use tar::{Builder, Header};
use tempfile::tempdir;

fn path(name: &str) -> PathBuf {
    PathBuf::from(name)
}

#[test]
fn archive_plan_selects_latest_baseline_and_deltas_after_it() {
    let plan = plan_from_paths(
        ArchiveSource::Legi,
        [
            path("LEGI_20250103-000000.tar.gz"),
            path("Freemium_legi_global_20250101-000000.tar.gz"),
            path("LEGI_20250102-000000.tar.gz"),
            path("Freemium_legi_global_20250102-120000.tar.gz"),
            path("LEGI_20250104-000000.tar.gz"),
        ],
    )
    .unwrap();

    assert_eq!(
        plan.baseline.file_name,
        "Freemium_legi_global_20250102-120000.tar.gz"
    );
    assert_eq!(
        plan.deltas
            .iter()
            .map(|archive| archive.file_name.as_str())
            .collect::<Vec<_>>(),
        vec!["LEGI_20250103-000000.tar.gz", "LEGI_20250104-000000.tar.gz"]
    );
}

#[test]
fn streaming_reader_visits_xml_members_and_enforces_limits() {
    let dir = tempdir().unwrap();
    let archive_path = dir
        .path()
        .join("Freemium_legi_global_20250102-120000.tar.gz");
    write_tar_gz(
        &archive_path,
        &[
            ("legi/a.xml", b"<A/>".as_slice()),
            ("legi/readme.txt", b"skip".as_slice()),
            ("legi/b.xml", b"<B/>".as_slice()),
        ],
    );

    let mut seen = Vec::new();
    let count = for_each_xml_member(&archive_path, 64, |member| {
        seen.push((member.member_path, member.bytes));
        Ok(())
    })
    .unwrap();

    assert_eq!(count, 2);
    assert_eq!(seen[0].0, "legi/a.xml");
    assert_eq!(seen[0].1, b"<A/>");
    assert_eq!(seen[1].0, "legi/b.xml");
    assert_eq!(seen[1].1, b"<B/>");

    let error = for_each_xml_member(&archive_path, 3, |_| Ok(())).unwrap_err();
    assert!(matches!(error, ArchiveReadError::MemberTooLarge { .. }));
}

fn write_tar_gz(path: &Path, members: &[(&str, &[u8])]) {
    let file = File::create(path).unwrap();
    let encoder = GzEncoder::new(file, Compression::default());
    let mut tar = Builder::new(encoder);

    for (name, bytes) in members {
        let mut header = Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append_data(&mut header, *name, &mut &bytes[..])
            .unwrap();
    }

    let encoder = tar.into_inner().unwrap();
    encoder.finish().unwrap();
}
