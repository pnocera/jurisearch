//! No-infra acceptance gates: config parse/validate, embedding storage-fingerprint parity with the
//! site config, and the OpenRouter request-model / storage-fingerprint separation.

use std::os::unix::fs::PermissionsExt;

use jurisearch_producer::PRODUCER_CONFIG_EXAMPLE;
use jurisearch_producer::config::ProducerConfig;

/// Write the example config into a temp dir with all referenced secret files created at 0600, so
/// `validate()` (which rejects world/group-readable secrets) passes.
fn write_example(dir: &std::path::Path) -> std::path::PathBuf {
    let secrets = dir.join("secrets");
    std::fs::create_dir_all(&secrets).unwrap();
    for name in [
        "postgres-admin-password",
        "jurisearch-write-password",
        "producer-signing.seed",
    ] {
        let path = secrets.join(name);
        let contents = if name == "producer-signing.seed" {
            "00".repeat(32) // 32-byte ed25519 seed as 64 hex chars
        } else {
            "secret".to_owned()
        };
        std::fs::write(&path, contents).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    // Rewrite the example's secret paths to the temp secrets dir.
    let toml =
        PRODUCER_CONFIG_EXAMPLE.replace("/etc/jurisearch/secrets", secrets.to_str().unwrap());
    let config_path = dir.join("producer.toml");
    std::fs::write(&config_path, toml).unwrap();
    config_path
}

#[test]
fn example_config_round_trips_parse_and_validate() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_example(dir.path());
    let config = ProducerConfig::load(&path).expect("example loads + validates");
    assert_eq!(config.package.corpus, "core");
    assert_eq!(config.fetch_groups.len(), 2);
    // The signer loads from the 0600 seed file.
    config.signer().expect("signer loads from seed file");
}

#[test]
fn provision_config_loads_the_writer_password_file_into_the_role_spec() {
    // WARN fix: `provision-db` must set the writer role's password from `writer_password_file`, else on a
    // password-auth external PG it never runs `ALTER ROLE ... PASSWORD` and its writer probe fails.
    let dir = tempfile::tempdir().unwrap();
    let path = write_example(dir.path());
    let config = ProducerConfig::load(&path).expect("example loads + validates");
    let provision = config.provision_config().expect("provision config builds");
    // `write_example` wrote the 0600 writer-password file with the contents "secret".
    assert_eq!(
        provision.roles.writer_password.as_deref(),
        Some("secret"),
        "writer_password_file must flow into RoleSpec.writer_password"
    );
    // The example sets no read_password_file, so the read password stays unset (no spurious ALTER).
    assert_eq!(provision.roles.read_password, None);
}

#[test]
fn provision_config_loads_the_read_password_file_when_configured() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_example(dir.path());
    // Add a 0600 read-password file and point the config at it.
    let read_pw = dir.path().join("secrets").join("jurisearch-read-password");
    std::fs::write(&read_pw, "read-secret").unwrap();
    std::fs::set_permissions(&read_pw, std::fs::Permissions::from_mode(0o600)).unwrap();
    let toml = std::fs::read_to_string(&path).unwrap().replace(
        "read_user = \"jurisearch_read\"",
        &format!(
            "read_user = \"jurisearch_read\"\nread_password_file = \"{}\"",
            read_pw.display()
        ),
    );
    std::fs::write(&path, toml).unwrap();
    let config = ProducerConfig::load(&path).expect("loads + validates with read_password_file");
    let provision = config.provision_config().expect("provision config builds");
    assert_eq!(
        provision.roles.read_password.as_deref(),
        Some("read-secret")
    );
    assert_eq!(provision.roles.writer_password.as_deref(), Some("secret"));
}

#[test]
fn example_config_defaults_the_judilibre_min_decision_date() {
    // The example sets the cutoff explicitly; it must parse to Some("2016-01-01").
    let dir = tempfile::tempdir().unwrap();
    let path = write_example(dir.path());
    let config = ProducerConfig::load(&path).expect("example loads + validates");
    assert_eq!(
        config.enrichment.min_decision_date.as_deref(),
        Some("2016-01-01")
    );
}

#[test]
fn old_enrichment_with_only_mode_still_parses_and_defaults_the_cutoff() {
    // Backward-compat: an existing `[enrichment]` block that only sets `mode` (no min_decision_date)
    // must still parse under `deny_unknown_fields` AND serde-default the cutoff to Some("2016-01-01").
    let dir = tempfile::tempdir().unwrap();
    let path = write_example(dir.path());
    let toml = std::fs::read_to_string(&path)
        .unwrap()
        .replace("min_decision_date = \"2016-01-01\"", "");
    std::fs::write(&path, toml).unwrap();
    let config = ProducerConfig::load(&path).expect("old [enrichment] mode-only still parses");
    assert_eq!(
        config.enrichment.min_decision_date.as_deref(),
        Some("2016-01-01"),
        "an [enrichment] block without min_decision_date must default to the 2016 cutoff"
    );
}

#[test]
fn an_earlier_min_decision_date_enriches_older_decisions() {
    // The docs promise that "enrich older decisions" is done by setting an EARLIER cutoff (TOML has no
    // null, so widening coverage is a date, not a disable). Prove the early-date path is real: an
    // explicit 1900-01-01 must parse + validate and load to Some("1900-01-01").
    let dir = tempfile::tempdir().unwrap();
    let path = write_example(dir.path());
    let toml = std::fs::read_to_string(&path).unwrap().replace(
        "min_decision_date = \"2016-01-01\"",
        "min_decision_date = \"1900-01-01\"",
    );
    std::fs::write(&path, toml).unwrap();
    let config =
        ProducerConfig::load(&path).expect("an earlier min_decision_date loads + validates");
    assert_eq!(
        config.enrichment.min_decision_date.as_deref(),
        Some("1900-01-01"),
        "an earlier cutoff is the documented way to enrich older decisions"
    );
}

#[test]
fn malformed_min_decision_date_is_rejected_by_validate() {
    for bad in [
        "2016-13-99",
        "2016/01/01",
        "2016-1-1",
        "not-a-date",
        "2016-02-30", // February never has 30 days
        "2019-02-29", // 2019 is not a leap year
        "0000-00-00", // year zero + zero month/day
        "2016-04-31", // April has only 30 days
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = write_example(dir.path());
        let toml = std::fs::read_to_string(&path).unwrap().replace(
            "min_decision_date = \"2016-01-01\"",
            &format!("min_decision_date = \"{bad}\""),
        );
        std::fs::write(&path, toml).unwrap();
        let err = ProducerConfig::load(&path).unwrap_err();
        assert!(
            err.to_string().contains("min_decision_date"),
            "malformed min_decision_date `{bad}` must be rejected by validate: {err}"
        );
    }

    // Valid leap-day: 2020 IS a leap year, so Feb 29 must be accepted.
    let good = "2020-02-29";
    let dir = tempfile::tempdir().unwrap();
    let path = write_example(dir.path());
    let toml = std::fs::read_to_string(&path).unwrap().replace(
        "min_decision_date = \"2016-01-01\"",
        &format!("min_decision_date = \"{good}\""),
    );
    std::fs::write(&path, toml).unwrap();
    let config =
        ProducerConfig::load(&path).expect("valid leap-day min_decision_date must be accepted");
    assert_eq!(config.enrichment.min_decision_date.as_deref(), Some(good));
}

#[test]
fn unknown_key_is_a_hard_parse_error() {
    let toml = format!("{PRODUCER_CONFIG_EXAMPLE}\n[unexpected]\nkey = 1\n");
    let err = ProducerConfig::parse_str(&toml, std::path::Path::new("x.toml")).unwrap_err();
    assert!(err.to_string().contains("parse"), "{err}");
}

#[test]
fn non_core_corpus_is_rejected_v1_single_corpus_invariant() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_example(dir.path());
    let toml = std::fs::read_to_string(&path)
        .unwrap()
        .replace("corpus = \"core\"", "corpus = \"jurisprudence\"");
    std::fs::write(&path, toml).unwrap();
    let err = ProducerConfig::load(&path).unwrap_err();
    assert!(err.to_string().contains("core"), "{err}");
}

#[test]
fn a_relative_path_rendered_into_a_unit_is_rejected() {
    // Every path rendered into a systemd unit (and the producer data/state paths in `ExecStart`/
    // `ReadWritePaths`) MUST be absolute — systemd does not expand env in unit file paths. A relative
    // value must be rejected by `validate()` BEFORE any unit is rendered.
    for (needle, relative) in [
        (
            "unit_dir = \"/etc/systemd/system\"",
            "unit_dir = \"systemd\"",
        ),
        (
            "binary_path = \"/usr/local/bin/jurisearch-producer\"",
            "binary_path = \"bin/jurisearch-producer\"",
        ),
        (
            "corpora_dir = \"/srv/jurisearch/storebox/packages\"",
            "corpora_dir = \"packages\"",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let path = write_example(dir.path());
        let original = std::fs::read_to_string(&path).unwrap();
        let toml = original.replacen(needle, relative, 1);
        assert!(toml.contains(relative), "fixture replaced `{needle}`");
        std::fs::write(&path, &toml).unwrap();
        let err = ProducerConfig::load(&path).unwrap_err();
        assert!(
            err.to_string().contains("ABSOLUTE"),
            "a relative path must be rejected with an absolute-path diagnostic: {err}"
        );
    }
}

#[test]
fn world_readable_secret_file_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_example(dir.path());
    // Loosen the seed file's permissions; validate must reject it.
    let seed = dir.path().join("secrets").join("producer-signing.seed");
    std::fs::set_permissions(&seed, std::fs::Permissions::from_mode(0o644)).unwrap();
    let err = ProducerConfig::load(&path).unwrap_err();
    assert!(err.to_string().contains("accessible"), "{err}");
}

#[test]
fn producer_and_site_example_configs_have_matching_storage_fingerprints() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_example(dir.path());
    let producer = ProducerConfig::from_path(&path).unwrap();

    let site = jurisearch_deploy::SiteConfig::parse_str(
        jurisearch_deploy::SITE_CONFIG_EXAMPLE,
        std::path::Path::new("site.toml"),
    )
    .unwrap();
    let site_fp = site
        .embedder
        .to_embedding_config()
        .storage_embedding_fingerprint();

    assert_eq!(
        producer.storage_embedding_fingerprint(),
        site_fp,
        "producer + site example configs must compute the same storage fingerprint"
    );
    // The shared fingerprint is exactly the canonical bge-m3 contract.
    assert_eq!(site_fp, "bge-m3:1024:normalize:true");
}

#[test]
fn openrouter_request_model_does_not_change_the_storage_fingerprint() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_example(dir.path());
    let producer = ProducerConfig::from_path(&path).unwrap();
    let embedding = producer.embedding_config();

    // The provider request model is the OpenRouter id...
    assert_eq!(embedding.request_model.as_deref(), Some("baai/bge-m3"));
    // ...but the STORAGE fingerprint keys only on model_name/dimension/normalize.
    assert_eq!(embedding.model, "bge-m3");
    assert_eq!(
        embedding.storage_embedding_fingerprint(),
        "bge-m3:1024:normalize:true"
    );

    // Flipping request_model to anything else leaves the storage fingerprint byte-identical.
    let mut other = embedding.clone();
    other.request_model = Some("some/other-provider-id".to_owned());
    assert_eq!(
        other.storage_embedding_fingerprint(),
        embedding.storage_embedding_fingerprint(),
        "request_model must never leak into the storage fingerprint"
    );
}
