//! ingest archive/embed/sync + legislation-enrichment contract tests.

mod support;
use support::*;

#[test]
fn ingest_embed_chunks_rejects_zero_limit_before_opening_index() {
    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env_remove("JURISEARCH_INDEX_DIR")
        .args(["ingest", "embed-chunks", "--limit", "0"])
        .assert()
        .code(2)
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["code"], "bad_input");
}

// NOTE: `--index-lists 0` is intentionally NOT rejected — it requests auto-scaling the ivfflat lists
// to the corpus size at finalize time (see `dense::recommended_ivfflat_lists`). Coverage for the
// auto/explicit lists resolution lives in the storage unit/integration tests.

#[test]
fn ingest_embed_chunks_rejects_zero_pool_knobs_before_opening_index() {
    for args in [
        ["ingest", "embed-chunks", "--batch-size", "0"],
        ["ingest", "embed-chunks", "--pool-concurrency", "0"],
    ] {
        let output = Command::cargo_bin("jurisearch")
            .unwrap()
            .env_remove("JURISEARCH_INDEX_DIR")
            .args(args)
            .assert()
            .code(2)
            .stderr(predicate::str::is_empty())
            .get_output()
            .stdout
            .clone();

        let json: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(json["ok"], false);
        assert_eq!(json["error"]["code"], "bad_input");
    }
}

#[test]
fn ingest_legi_archives_rejects_zero_limit_before_opening_index() {
    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env_remove("JURISEARCH_INDEX_DIR")
        .args([
            "ingest",
            "legi-archives",
            "--archives-dir",
            "/tmp",
            "--limit-members",
            "0",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["code"], "bad_input");
}

#[test]
fn ingest_legi_archives_records_accounting_and_quarantines_failures()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(pg_config) = discover_pg_config("CLI LEGI archive ingest")? else {
        return Ok(());
    };
    let index = tempfile::Builder::new()
        .prefix("jurisearch-cli-legi-ingest.")
        .tempdir()?;
    let archives = tempfile::Builder::new()
        .prefix("jurisearch-cli-legi-archives.")
        .tempdir()?;
    let quarantine = tempfile::Builder::new()
        .prefix("jurisearch-cli-legi-quarantine.")
        .tempdir()?;
    let archive_path = archives
        .path()
        .join("Freemium_legi_global_20250101-000000.tar.gz");
    let article = article_fixture().replace(
        "  </LIENS>",
        r#"    <LIEN_SECTION_TA id="LEGISCTA000006089696" debut="1804-03-21" fin="2999-01-01"/>
  </LIENS>"#,
    );
    write_tar_gz(
        archive_path.as_path(),
        &[
            ("legi/articles/LEGIARTI000006419320.xml", article.as_bytes()),
            (
                "legi/sections/SECTION.xml",
                br#"<SECTION_TA>
  <ID>LEGISCTA000006089696</ID>
  <TITRE_TA>Titre preliminaire</TITRE_TA>
  <CONTEXTE>
    <TEXTE cid="LEGITEXT000006070721">
      <TITRE_TXT debut="1804-03-21" fin="2999-01-01">Code civil</TITRE_TXT>
      <TM>
        <TITRE_TM>Livre III : Des differentes manieres dont on acquiert la propriete</TITRE_TM>
        <TM>
          <TITRE_TM>Titre IV : Des engagements qui se forment sans convention</TITRE_TM>
        </TM>
      </TM>
    </TEXTE>
  </CONTEXTE>
</SECTION_TA>"#,
            ),
            (
                "legi/textes/LEGITEXT000049371154.xml",
                br#"<TEXTE_VERSION>
  <META>
    <META_COMMUN>
      <ID>LEGITEXT000049371154</ID>
      <URL>/id/LEGITEXT000049371154</URL>
      <NATURE/>
    </META_COMMUN>
    <META_SPEC>
      <META_TEXTE_VERSION>
        <TITRE>Arrete du 12 avril 1956</TITRE>
        <ETAT>VIGUEUR</ETAT>
        <DATE_DEBUT>1956-04-12</DATE_DEBUT>
        <DATE_FIN>2999-01-01</DATE_FIN>
      </META_TEXTE_VERSION>
    </META_SPEC>
  </META>
</TEXTE_VERSION>"#,
            ),
            (
                "legi/textelr/LEGITEXT000006070721.xml",
                br#"<TEXTELR>
  <META>
    <META_COMMUN>
      <ID>LEGITEXT000006070721</ID>
      <URL>/id/LEGITEXT000006070721</URL>
      <NATURE>CODE</NATURE>
    </META_COMMUN>
    <META_SPEC>
      <META_TEXTE_CHRONICLE>
        <CID>LEGITEXT000006070721</CID>
        <NUM>civil</NUM>
        <DATE_PUBLI>1804-03-21</DATE_PUBLI>
        <DATE_TEXTE>1804-03-21</DATE_TEXTE>
      </META_TEXTE_CHRONICLE>
    </META_SPEC>
  </META>
  <STRUCT>
    <LIEN_TXT id="LEGITEXT000006070721" debut="1804-03-21"/>
  </STRUCT>
</TEXTELR>"#,
            ),
            ("legi/articles/BROKEN.xml", b"<ARTICLE><META/></ARTICLE>"),
        ],
    )?;

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .arg("--index-dir")
        .arg(index.path())
        .args(["ingest", "legi-archives", "--archives-dir"])
        .arg(archives.path())
        .args([
            "--run-id",
            "run-cli",
            "--limit-members",
            "5",
            "--quarantine-dir",
        ])
        .arg(quarantine.path())
        .arg("--safe-mode")
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["command"], "ingest legi-archives");
    assert_eq!(json["run_id"], "run-cli");
    assert_eq!(json["run_status"], "failed");
    assert_eq!(json["safe_mode"], true);
    assert_eq!(json["visited_members"], 5);
    assert_eq!(json["inserted_documents"], 1);
    assert_eq!(json["parsed_metadata_members"], 3);
    assert_eq!(json["persisted_metadata_members"], 3);
    assert_eq!(json["hierarchy_backfill_scoped_documents"], 1);
    assert_eq!(json["hierarchy_backfill_scoped_sections"], 1);
    assert_eq!(json["hierarchy_backfill_scoped_texts"], 1);
    assert_eq!(json["hierarchy_backfilled_documents"], 1);
    assert_eq!(json["hierarchy_backfill_invalidated_embeddings"], 0);
    assert_eq!(json["skipped_members"], 3);
    assert_eq!(json["failed_members"], 1);
    assert_eq!(json["quarantined_payloads"], 1);
    assert_eq!(json["parsed_metadata_roots"]["SECTION_TA"], 1);
    assert_eq!(json["parsed_metadata_roots"]["TEXTELR"], 1);
    assert_eq!(json["parsed_metadata_roots"]["TEXTE_VERSION"], 1);
    assert_eq!(json["manifest"]["source"], "legi");
    assert_eq!(json["manifest"]["dataset"], "LEGI");
    assert_eq!(json["manifest"]["run_status"], "failed");
    assert_eq!(json["manifest"]["complete"], false);
    assert_eq!(json["manifest"]["source_version"], "20250101-000000");
    assert_eq!(
        json["manifest"]["freshness"]["latest_archive"],
        "Freemium_legi_global_20250101-000000.tar.gz"
    );
    assert_eq!(json["manifest"]["coverage"]["visited_members"], 5);
    assert_eq!(
        json["manifest"]["coverage"]["hierarchy_backfill_scoped_documents"],
        1
    );
    assert_eq!(
        json["manifest"]["coverage"]["hierarchy_backfill_scoped_sections"],
        1
    );
    assert_eq!(
        json["manifest"]["coverage"]["hierarchy_backfill_scoped_texts"],
        1
    );
    assert_eq!(
        json["manifest"]["coverage"]["hierarchy_backfilled_documents"],
        1
    );
    assert!(json["unsupported_roots"].as_object().unwrap().is_empty());

    let quarantine_entries =
        fs::read_dir(quarantine.path().join("run-cli"))?.collect::<Result<Vec<_>, _>>()?;
    assert_eq!(quarantine_entries.len(), 1);

    let postgres = ManagedPostgres::start_durable(pg_config.clone(), index.path())?;
    assert_eq!(
        postgres.execute_sql("SELECT count(*)::text FROM documents;")?,
        "1"
    );

    // P1 outbox (§5.1): the ingest captured its mutations in package_change_log — the document
    // upsert, the three metadata-root upserts, and the hierarchy-backfill chunk_embeddings
    // replace_set — every row attributed to corpus 'core', schema 24, and this ingest run.
    assert_eq!(
        postgres.execute_sql(
            "SELECT string_agg(op || ':' || scope_kind || ':' || table_name, ',' \
                 ORDER BY op, scope_kind, table_name) \
             FROM (SELECT DISTINCT op, scope_kind, table_name FROM package_change_log) s;",
        )?,
        "replace_set:document:chunk_embeddings,upsert:document:documents,\
         upsert:legi_metadata_root:legi_metadata_roots"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT count(*)::text FROM package_change_log \
             WHERE corpus <> 'core' OR schema_version <> 24 OR ingest_run_id <> 'run-cli';",
        )?,
        "0",
        "every outbox row is corpus core, schema 24, attributed to the ingest run"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT count(*)::text FROM package_change_log \
             WHERE table_name = 'legi_metadata_roots';",
        )?,
        "3"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT string_agg(status || ':' || member_count::text, ',' ORDER BY status) \
             FROM (SELECT status, count(*) AS member_count FROM ingest_member GROUP BY status) s;",
        )?,
        "failed:1,inserted:1,skipped:3"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT source_entity FROM ingest_member \
             WHERE member_path = 'legi/sections/SECTION.xml';",
        )?,
        "LEGISCTA000006089696"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT source_entity FROM ingest_member \
             WHERE member_path = 'legi/textes/LEGITEXT000049371154.xml';",
        )?,
        "LEGITEXT000049371154"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT string_agg(root_kind || ':' || source_uid, ',' ORDER BY root_kind, source_uid) \
             FROM legi_metadata_roots;",
        )?,
        "SECTION_TA:LEGISCTA000006089696,TEXTELR:LEGITEXT000006070721,TEXTE_VERSION:LEGITEXT000049371154"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT coalesce(canonical_json->>'nature', 'absent') \
             FROM legi_metadata_roots \
             WHERE root_kind = 'TEXTE_VERSION';",
        )?,
        "absent"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT canonical_json->>'num' \
             FROM legi_metadata_roots \
             WHERE root_kind = 'TEXTELR';",
        )?,
        "civil"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT canonical_json->'hierarchy_path'->>3 \
             FROM documents \
             WHERE document_id = 'legi:LEGIARTI000006419320@1804-02-21';",
        )?,
        "Titre preliminaire"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT (canonical_json->'chunks'->0->>'contextualized_body') \
                    LIKE '%Titre preliminaire%Article 1240%' \
             FROM documents \
             WHERE document_id = 'legi:LEGIARTI000006419320@1804-02-21';",
        )?,
        "t"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT contextualized_body LIKE '%Titre preliminaire%Article 1240%', \
                    hierarchy_path->>3, \
                    chunking || ':' || boundary \
             FROM chunks \
             WHERE document_id = 'legi:LEGIARTI000006419320@1804-02-21';",
        )?,
        "t|Titre preliminaire|structural:article"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT string_agg(error_code, ',' ORDER BY error_code) FROM ingest_error;"
        )?,
        "validation_missing_required_field"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT count(*)::text FROM pg_class \
             WHERE relkind = 'i' AND relname = 'chunks_bm25_idx';"
        )?,
        "1"
    );
    drop(postgres);

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .arg("--index-dir")
        .arg(index.path())
        .arg("status")
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["ingest_health"]["latest_manifest"]["source"], "legi");
    assert_eq!(
        json["ingest_health"]["latest_manifest"]["run_status"],
        "failed"
    );
    assert_eq!(json["ingest_health"]["latest_manifest"]["complete"], false);
    assert_eq!(
        json["ingest_health"]["latest_manifest"]["freshness"]["latest_archive_timestamp"],
        "20250101-000000"
    );
    assert_eq!(
        json["ingest_health"]["latest_manifest"]["coverage"]["persisted_metadata_members"],
        3
    );

    {
        let postgres = ManagedPostgres::start_durable(pg_config.clone(), index.path())?;
        postgres.execute_sql(
            "UPDATE documents \
             SET canonical_json = jsonb_set(canonical_json, '{hierarchy_path}', '[\"Code civil\"]'::jsonb), \
                 hierarchy_path = '[\"Code civil\"]'::jsonb \
             WHERE document_id = 'legi:LEGIARTI000006419320@1804-02-21'; \
             UPDATE chunks \
             SET contextualized_body = body, hierarchy_path = '[]'::jsonb \
             WHERE document_id = 'legi:LEGIARTI000006419320@1804-02-21';",
        )?;
    }

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .arg("--index-dir")
        .arg(index.path())
        .args(["ingest", "legi-archives", "--archives-dir"])
        .arg(archives.path())
        .args(["--run-id", "run-cli-resume", "--limit-members", "1"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["run_status"], "completed");
    assert_eq!(json["visited_members"], 1);
    assert_eq!(json["inserted_documents"], 0);
    assert_eq!(json["skipped_members"], 1);
    assert_eq!(json["skipped_compatible_members"], 1);
    assert_eq!(json["hierarchy_backfill_scoped_documents"], 0);
    assert_eq!(json["hierarchy_backfilled_documents"], 1);

    {
        let postgres = ManagedPostgres::start_durable(pg_config.clone(), index.path())?;
        assert_eq!(
            postgres.execute_sql(
                "SELECT canonical_json->'hierarchy_path'->>3 \
                 FROM documents \
                 WHERE document_id = 'legi:LEGIARTI000006419320@1804-02-21';",
            )?,
            "Titre preliminaire"
        );
        assert_eq!(
            postgres.execute_sql(
                "SELECT contextualized_body LIKE '%Titre preliminaire%Article 1240%', \
                        hierarchy_path->>3 \
                 FROM chunks \
                 WHERE document_id = 'legi:LEGIARTI000006419320@1804-02-21';",
            )?,
            "t|Titre preliminaire"
        );
        assert_eq!(
            postgres.execute_sql(
                "SELECT count(*)::text FROM pg_class \
                 WHERE relkind = 'i' AND relname = 'chunks_bm25_idx';"
            )?,
            "1"
        );
    }

    write_tar_gz(
        archive_path.as_path(),
        &[("legi/articles/BROKEN.xml", b"<ARTICLE><META/></ARTICLE>")],
    )?;
    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .arg("--index-dir")
        .arg(index.path())
        .args(["ingest", "legi-archives", "--archives-dir"])
        .arg(archives.path())
        .args(["--run-id", "run-cli-retry", "--limit-members", "1"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["run_status"], "failed");
    assert_eq!(json["visited_members"], 1);
    assert_eq!(json["failed_members"], 1);

    let postgres = ManagedPostgres::start_durable(pg_config.clone(), index.path())?;
    assert_eq!(
        postgres.execute_sql(
            "SELECT status FROM ingest_member \
             WHERE run_id = 'run-cli-retry' AND member_path = 'legi/articles/BROKEN.xml';",
        )?,
        "failed"
    );
    assert_eq!(
        postgres
            .execute_sql("SELECT error_code FROM ingest_error WHERE run_id = 'run-cli-retry';")?,
        "validation_missing_required_field"
    );
    drop(postgres);

    let mutated_article =
        article_fixture().replace("Tout fait quelconque", "Tout autre fait quelconque");
    write_tar_gz(
        archive_path.as_path(),
        &[(
            "legi/articles/LEGIARTI000006419320.xml",
            mutated_article.as_bytes(),
        )],
    )?;
    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .arg("--index-dir")
        .arg(index.path())
        .args(["ingest", "legi-archives", "--archives-dir"])
        .arg(archives.path())
        .args([
            "--run-id",
            "run-cli-incompatible",
            "--limit-members",
            "1",
            "--quarantine-dir",
        ])
        .arg(quarantine.path())
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["run_status"], "failed");
    assert_eq!(json["visited_members"], 1);
    assert_eq!(json["failed_members"], 1);
    assert_eq!(json["quarantined_payloads"], 1);

    let postgres = ManagedPostgres::start_durable(pg_config, index.path())?;
    assert_eq!(
        postgres.execute_sql(
            "SELECT status FROM ingest_member \
             WHERE run_id = 'run-cli-incompatible' \
               AND member_path = 'legi/articles/LEGIARTI000006419320.xml';",
        )?,
        "failed"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT error_class || ':' || error_code \
             FROM ingest_error \
             WHERE run_id = 'run-cli-incompatible';",
        )?,
        "validation_error:compatibility_mismatch"
    );

    Ok(())
}

#[test]
fn ingest_legi_archives_same_run_resume_keeps_inserted_members_inserted()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(pg_config) = discover_pg_config("CLI LEGI same-run resume")? else {
        return Ok(());
    };
    let index = tempfile::Builder::new()
        .prefix("jurisearch-cli-legi-same-run-resume.")
        .tempdir()?;
    let archives = tempfile::Builder::new()
        .prefix("jurisearch-cli-legi-same-run-resume-archives.")
        .tempdir()?;
    let archive_path = archives
        .path()
        .join("Freemium_legi_global_20250101-000000.tar.gz");
    let article = article_fixture();
    write_tar_gz(
        archive_path.as_path(),
        &[("legi/articles/LEGIARTI000006419320.xml", article.as_bytes())],
    )?;

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .arg("--index-dir")
        .arg(index.path())
        .args(["ingest", "legi-archives", "--archives-dir"])
        .arg(archives.path())
        .args(["--run-id", "run-cli-same-resume"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["run_status"], "completed");
    assert_eq!(json["visited_members"], 1);
    assert_eq!(json["inserted_documents"], 1);

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .arg("--index-dir")
        .arg(index.path())
        .args(["ingest", "legi-archives", "--archives-dir"])
        .arg(archives.path())
        .args(["--run-id", "run-cli-same-resume"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["run_status"], "completed");
    assert_eq!(json["visited_members"], 1);
    assert_eq!(json["inserted_documents"], 0);
    assert_eq!(json["skipped_members"], 1);
    assert_eq!(json["skipped_compatible_members"], 1);

    let postgres = ManagedPostgres::start_durable(pg_config, index.path())?;
    assert_eq!(
        postgres.execute_sql(
            "SELECT status || ':' || attempt_count::text || ':' || coalesce(source_entity, 'none') \
             FROM ingest_member \
             WHERE run_id = 'run-cli-same-resume' \
               AND member_path = 'legi/articles/LEGIARTI000006419320.xml';",
        )?,
        "inserted:1:LEGIARTI000006419320"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT count(*)::text \
             FROM ingest_member \
             WHERE run_id = 'run-cli-same-resume';",
        )?,
        "1"
    );

    Ok(())
}

#[test]
fn ingest_legi_archives_skips_no_text_articles_without_failing_run()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(pg_config) = discover_pg_config("CLI LEGI no-text article skip")? else {
        return Ok(());
    };
    let index = tempfile::Builder::new()
        .prefix("jurisearch-cli-legi-no-text.")
        .tempdir()?;
    let archives = tempfile::Builder::new()
        .prefix("jurisearch-cli-legi-no-text-archives.")
        .tempdir()?;
    let quarantine = tempfile::Builder::new()
        .prefix("jurisearch-cli-legi-no-text-quarantine.")
        .tempdir()?;
    let archive_path = archives
        .path()
        .join("Freemium_legi_global_20250101-000000.tar.gz");
    let no_text_article = article_fixture_without_body();
    write_tar_gz(
        archive_path.as_path(),
        &[(
            "legi/articles/LEGIARTI000006419320.xml",
            no_text_article.as_bytes(),
        )],
    )?;

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .arg("--index-dir")
        .arg(index.path())
        .args(["ingest", "legi-archives", "--archives-dir"])
        .arg(archives.path())
        .args(["--run-id", "run-no-text", "--quarantine-dir"])
        .arg(quarantine.path())
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["command"], "ingest legi-archives");
    assert_eq!(json["run_status"], "completed");
    assert_eq!(json["visited_members"], 1);
    assert_eq!(json["inserted_documents"], 0);
    assert_eq!(json["skipped_members"], 1);
    assert_eq!(json["skipped_no_text_articles"], 1);
    assert_eq!(json["failed_members"], 0);
    assert_eq!(json["quarantined_payloads"], 0);
    assert_eq!(json["manifest"]["coverage"]["skipped_no_text_articles"], 1);
    assert!(
        json["parsed_metadata_roots"]
            .as_object()
            .unwrap()
            .is_empty()
    );
    assert!(json["unsupported_roots"].as_object().unwrap().is_empty());
    assert!(!quarantine.path().join("run-no-text").exists());

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .arg("--index-dir")
        .arg(index.path())
        .args(["ingest", "legi-archives", "--archives-dir"])
        .arg(archives.path())
        .args(["--run-id", "run-no-text-resume"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["run_status"], "completed");
    assert_eq!(json["visited_members"], 1);
    assert_eq!(json["skipped_members"], 1);
    assert_eq!(json["skipped_compatible_members"], 1);
    assert_eq!(json["skipped_no_text_articles"], 0);
    assert_eq!(json["failed_members"], 0);

    let postgres = ManagedPostgres::start_durable(pg_config, index.path())?;
    assert_eq!(
        postgres.execute_sql("SELECT count(*)::text FROM documents;")?,
        "0"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT status || ':' || coalesce(source_entity, 'none') \
             FROM ingest_member \
             WHERE run_id = 'run-no-text';",
        )?,
        "skipped:LEGIARTI000006419320"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT count(*)::text \
             FROM ingest_error \
             WHERE run_id = 'run-no-text';",
        )?,
        "0"
    );

    Ok(())
}

#[test]
fn ingest_legi_archives_keeps_non_body_article_errors_failed()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(pg_config) = discover_pg_config("CLI LEGI invalid article failure")? else {
        return Ok(());
    };
    let index = tempfile::Builder::new()
        .prefix("jurisearch-cli-legi-invalid-article.")
        .tempdir()?;
    let archives = tempfile::Builder::new()
        .prefix("jurisearch-cli-legi-invalid-article-archives.")
        .tempdir()?;
    let quarantine = tempfile::Builder::new()
        .prefix("jurisearch-cli-legi-invalid-article-quarantine.")
        .tempdir()?;
    let archive_path = archives
        .path()
        .join("Freemium_legi_global_20250101-000000.tar.gz");
    let invalid_article = article_fixture().replace(
        "<DATE_DEBUT>1804-02-21</DATE_DEBUT>",
        "<DATE_DEBUT>not-a-date</DATE_DEBUT>",
    );
    write_tar_gz(
        archive_path.as_path(),
        &[(
            "legi/articles/LEGIARTI000006419320.xml",
            invalid_article.as_bytes(),
        )],
    )?;

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .arg("--index-dir")
        .arg(index.path())
        .args(["ingest", "legi-archives", "--archives-dir"])
        .arg(archives.path())
        .args(["--run-id", "run-invalid-article", "--quarantine-dir"])
        .arg(quarantine.path())
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["run_status"], "failed");
    assert_eq!(json["visited_members"], 1);
    assert_eq!(json["skipped_members"], 0);
    assert_eq!(json["skipped_no_text_articles"], 0);
    assert_eq!(json["failed_members"], 1);
    assert_eq!(json["quarantined_payloads"], 1);

    let quarantine_entries = fs::read_dir(quarantine.path().join("run-invalid-article"))?
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(quarantine_entries.len(), 1);

    let postgres = ManagedPostgres::start_durable(pg_config, index.path())?;
    assert_eq!(
        postgres.execute_sql(
            "SELECT status \
             FROM ingest_member \
             WHERE run_id = 'run-invalid-article';",
        )?,
        "failed"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT error_code \
             FROM ingest_error \
             WHERE run_id = 'run-invalid-article';",
        )?,
        "validation_invalid_date"
    );

    Ok(())
}

#[test]
fn ingest_backfill_legi_hierarchy_updates_full_index() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("CLI LEGI hierarchy backfill")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-cli-legi-backfill.")
        .tempdir()
        .map_err(StorageError::Io)?;

    {
        let postgres = ManagedPostgres::start_durable(pg_config.clone(), root.path())?;
        postgres.execute_sql(
            r#"INSERT INTO legi_metadata_roots
                (metadata_key, root_kind, source_uid, parent_source_uid, title,
                 valid_from, valid_to, valid_to_raw, source_payload_hash,
                 canonical_version, canonical_json)
             VALUES
                ('legi:SECTION_TA:LEGISCTA000006089696@1804-03-21', 'SECTION_TA',
                 'LEGISCTA000006089696', 'LEGITEXT000006070721', 'Titre preliminaire',
                 '1804-03-21', NULL, '2999-01-01', 'sha256:section',
                 'legi_section_ta:v1',
                 '{"title":"Titre preliminaire","hierarchy_path":["Code civil","Livre III"]}');
             INSERT INTO documents
                (document_id, source, kind, source_uid, citation, title, body,
                 valid_from, source_payload_hash, canonical_json)
             VALUES
                ('legi:LEGIARTI000006419320@1804-02-21', 'legi', 'article',
                 'LEGIARTI000006419320', 'Code civil article 1240',
                 'Article 1240', 'Texte initial pour le test.',
                 '1804-02-21', 'sha256:article-1240',
                 '{"title":"Article 1240","hierarchy_path":["Code civil"],"chunks":[{"body":"Texte initial pour le test.","hierarchy_path":["Code civil"],"contextualized_body":"Code civil\nArticle 1240\nTexte initial pour le test."}]}');
             INSERT INTO chunks
                (chunk_id, document_id, chunk_index, body, contextualized_body, source_payload_hash,
                 chunk_builder_version, embedding_fingerprint)
             VALUES
                ('chunk:1240:0', 'legi:LEGIARTI000006419320@1804-02-21', 0,
                 'Texte initial pour le test.',
                 'Code civil\nArticle 1240\nTexte initial pour le test.',
                 'sha256:article-1240',
                 'chunker:v0', 'bge-m3:1024:normalize:true');
             INSERT INTO graph_edges
                (edge_id, from_document_id, edge_kind, edge_source, payload)
             VALUES
                ('edge:1240:section', 'legi:LEGIARTI000006419320@1804-02-21',
                 'structure', 'publisher',
                 '{"source_tag":"LIEN_SECTION_TA","to_source_uid":"LEGISCTA000006089696","attributes":[{"key":"debut","value":"1804-03-21"},{"key":"fin","value":"2999-01-01"}]}');"#,
        )?;
        postgres.execute_sql(&format!(
            "INSERT INTO chunk_embeddings \
                (chunk_id, embedding_fingerprint, embedding, model, dimension) \
             VALUES \
                ('chunk:1240:0', 'bge-m3:1024:normalize:true', '{}', 'bge-m3', 1024);",
            unit_vector_literal(0)
        ))?;
    }

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .arg("--index-dir")
        .arg(root.path())
        .args(["ingest", "backfill-legi-hierarchy"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["command"], "ingest backfill-legi-hierarchy");
    assert_eq!(json["scope"], "full");
    assert_eq!(json["hierarchy_backfilled_documents"], 1);
    assert_eq!(json["hierarchy_backfill_invalidated_embeddings"], 1);
    assert_eq!(json["embedding_rebuild_required"], true);
    assert_eq!(
        json["recommended_next_command"],
        "jurisearch ingest embed-chunks"
    );
    assert_eq!(json["replay_snapshot_cache"]["source"], "refreshed");
    assert_eq!(json["replay_snapshot_cache"]["status"], "available");
    assert_eq!(json["replay_snapshot_cache"]["documents"], 1);
    assert_eq!(json["replay_snapshot_cache"]["chunks"], 1);

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .arg("--index-dir")
        .arg(root.path())
        .args(["ingest", "backfill-legi-hierarchy"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["hierarchy_backfilled_documents"], 0);
    assert_eq!(json["hierarchy_backfill_invalidated_embeddings"], 0);
    assert_eq!(json["embedding_rebuild_required"], false);
    assert!(json["recommended_next_command"].is_null());

    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
    assert_eq!(
        postgres.execute_sql(
            "SELECT canonical_json->'hierarchy_path'->>2 \
             FROM documents \
             WHERE document_id = 'legi:LEGIARTI000006419320@1804-02-21';",
        )?,
        "Titre preliminaire"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT (canonical_json->'chunks'->0->>'contextualized_body') \
                    LIKE '%Titre preliminaire%Article 1240%Texte initial%' \
             FROM documents \
             WHERE document_id = 'legi:LEGIARTI000006419320@1804-02-21';",
        )?,
        "t"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT coalesce(embedding_fingerprint, 'cleared') \
             FROM chunks \
             WHERE chunk_id = 'chunk:1240:0';",
        )?,
        "cleared"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT count(*)::text \
             FROM chunk_embeddings \
             WHERE chunk_id = 'chunk:1240:0';",
        )?,
        "0"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT count(*)::text \
             FROM ingest_run \
             WHERE run_id = 'backfill-legi-hierarchy';",
        )?,
        "0"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT count(*)::text \
             FROM ingest_member;",
        )?,
        "0"
    );

    Ok(())
}

#[test]
fn sync_pulls_new_deltas_incrementally_with_since_filter() -> Result<(), Box<dyn std::error::Error>>
{
    let Some(_pg_config) = discover_pg_config("CLI sync")? else {
        return Ok(());
    };
    let index = tempfile::Builder::new()
        .prefix("jurisearch-cli-sync.")
        .tempdir()?;
    let archives = tempfile::Builder::new()
        .prefix("jurisearch-cli-sync-archives.")
        .tempdir()?;

    // Baseline with decision A.
    write_tar_gz(
        archives
            .path()
            .join("Freemium_cass_global_20250101-000000.tar.gz")
            .as_path(),
        &[(
            "juri/cass/JURITEXT000000000001.xml",
            cass_decision_fixture("JURITEXT000000000001", "23-10001").as_slice(),
        )],
    )?;

    // Full build of the baseline.
    Command::cargo_bin("jurisearch")
        .unwrap()
        .arg("--index-dir")
        .arg(index.path())
        .args([
            "ingest",
            "juri-archives",
            "--source",
            "cass",
            "--archives-dir",
        ])
        .arg(archives.path())
        .args(["--run-id", "run-base"])
        .assert()
        .success();

    // Two deltas: one BEFORE the --since cutoff (decision C), one AFTER (decision B).
    write_tar_gz(
        archives
            .path()
            .join("CASS_20250110-000000.tar.gz")
            .as_path(),
        &[(
            "juri/cass/JURITEXT000000000003.xml",
            cass_decision_fixture("JURITEXT000000000003", "23-10003").as_slice(),
        )],
    )?;
    write_tar_gz(
        archives
            .path()
            .join("CASS_20250201-000000.tar.gz")
            .as_path(),
        &[(
            "juri/cass/JURITEXT000000000002.xml",
            cass_decision_fixture("JURITEXT000000000002", "23-10002").as_slice(),
        )],
    )?;

    // Sync only deltas at/after 2025-01-15: B is pulled, C (2025-01-10) and the baseline are not.
    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .arg("--index-dir")
        .arg(index.path())
        .args([
            "sync",
            "--source",
            "cass",
            "--since",
            "2025-01-15",
            "--archives-dir",
        ])
        .arg(archives.path())
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["command"], "sync");
    assert_eq!(json["mode"], "incremental");
    assert_eq!(json["source"], "cass");
    assert_eq!(json["synced_since"], "2025-01-15");
    assert_eq!(json["run_status"], "completed");
    assert_eq!(json["inserted_documents"], 1); // only delta B (after cutoff)

    // A NEWER delta that a no-op sync will NOT process: status freshness must not jump to it.
    write_tar_gz(
        archives
            .path()
            .join("CASS_20250301-000000.tar.gz")
            .as_path(),
        &[(
            "juri/cass/JURITEXT000000000004.xml",
            cass_decision_fixture("JURITEXT000000000004", "23-10004").as_slice(),
        )],
    )?;
    let noop = Command::cargo_bin("jurisearch")
        .unwrap()
        .arg("--index-dir")
        .arg(index.path())
        .args([
            "sync",
            "--source",
            "cass",
            "--since",
            "2999-01-01",
            "--archives-dir",
        ])
        .arg(archives.path())
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let noop: Value = serde_json::from_slice(&noop).unwrap();
    // Two immediate same-source syncs must get distinct auto run IDs (no ON CONFLICT overwrite).
    assert_ne!(json["run_id"], noop["run_id"]);
    assert_eq!(noop["inserted_documents"], 0); // nothing processed
    // Honest: a run that processed nothing reports null freshness (BLOCKER fix), not the dir's newest.
    assert!(noop["manifest"]["source_version"].is_null());
    assert!(noop["manifest"]["freshness"].is_null());

    // status corpus_sources still reports the last ACTUALLY-processed delta (20250201), not 20250301.
    let status = Command::cargo_bin("jurisearch")
        .unwrap()
        .arg("--index-dir")
        .arg(index.path())
        .arg("status")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let status: Value = serde_json::from_slice(&status).unwrap();
    assert_eq!(
        status["corpus_sources"]["cass"]["source_version"],
        "20250201-000000"
    );

    // The index holds the baseline A + the synced B, but NOT the pre-cutoff C or the unprocessed D.
    let postgres = ManagedPostgres::start_durable(_pg_config, index.path())?;
    let count = postgres.execute_sql("SELECT count(*) FROM documents WHERE kind='decision';")?;
    assert_eq!(count.trim(), "2");
    for missing in ["cass:JURITEXT000000000003", "cass:JURITEXT000000000004"] {
        let present = postgres.execute_sql(&format!(
            "SELECT count(*) FROM documents WHERE document_id = '{missing}';"
        ))?;
        assert_eq!(present.trim(), "0", "{missing} should not be in the index");
    }
    postgres.stop()?;

    // Validation errors.
    Command::cargo_bin("jurisearch")
        .unwrap()
        .arg("--index-dir")
        .arg(index.path())
        .args(["sync", "--source", "bogus", "--archives-dir"])
        .arg(archives.path())
        .assert()
        .code(2);
    Command::cargo_bin("jurisearch")
        .unwrap()
        .arg("--index-dir")
        .arg(index.path())
        .args([
            "sync",
            "--source",
            "cass",
            "--since",
            "not-a-date",
            "--archives-dir",
        ])
        .arg(archives.path())
        .assert()
        .code(2);

    Ok(())
}

#[test]
fn ingest_embed_chunks_truncates_over_budget_input_and_reports_count() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("CLI embed chunk truncation")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-cli-embed-budget.")
        .tempdir()
        .map_err(StorageError::Io)?;

    {
        let postgres =
            jurisearch_storage::runtime::ManagedPostgres::start_durable(pg_config, root.path())?;
        postgres.execute_sql(
            "INSERT INTO documents \
                (document_id, source, kind, source_uid, citation, title, body, \
                 valid_from, source_payload_hash, canonical_json) \
             VALUES \
                ('legi:LEGIARTI000006419320@1804-02-21', 'legi', 'article', \
                 'LEGIARTI000006419320', 'Code civil article 1240', \
                 'Article 1240', 'Texte long pour le test.', \
                 '1804-02-21', 'sha256:article-1240', \
                 '{\"chunks\":[{\"contextualized_body\":\"abcde\"}]}'); \
             INSERT INTO chunks \
                (chunk_id, document_id, chunk_index, body, contextualized_body, source_payload_hash, \
                 chunk_builder_version, embedding_fingerprint) \
             VALUES \
                ('chunk:1240:0', 'legi:LEGIARTI000006419320@1804-02-21', 0, \
                 'abcde', \
                 'abcde', \
                 'sha256:article-1240', 'chunker:v0', NULL);",
        )?;
    }

    let endpoint = spawn_server(1, |request| {
        assert!(request.starts_with("POST /v1/embeddings "));
        assert!(request.contains(r#""input":["abcd"]"#));
        assert!(!request.contains(r#""abcde""#));
        ok_json(&embedding_response_json(0))
    });

    let output = jurisearch_command_without_embedding_env()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .env("JURISEARCH_CONFIG", "none")
        .env("JURISEARCH_EMBED_BASE_URL", format!("{endpoint}/v1"))
        .env("JURISEARCH_EMBED_MAX_INPUT_CHARS", "4")
        .args(["ingest", "embed-chunks", "--index-lists", "1"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["command"], "ingest embed-chunks");
    assert_eq!(json["chunks_considered"], 1);
    assert_eq!(json["embeddings_inserted"], 1);
    assert_eq!(json["embedding_inputs_truncated"], 1);
    assert_eq!(json["endpoint_pool"]["endpoints"][0]["truncated_inputs"], 1);

    Ok(())
}

#[test]
fn ingest_embed_chunks_uses_endpoint_pool_and_finalizes_dense_index()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(pg_config) = discover_pg_config("CLI embed chunk endpoint pool")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-cli-embed-pool.")
        .tempdir()?;

    {
        let postgres = ManagedPostgres::start_durable(pg_config.clone(), root.path())?;
        postgres.execute_sql(
            "INSERT INTO documents \
                (document_id, source, kind, source_uid, citation, title, body, \
                 valid_from, source_payload_hash, canonical_json) \
             VALUES \
                ('legi:LEGIARTI000006419320@1804-02-21', 'legi', 'article', \
                 'LEGIARTI000006419320', 'Code civil article 1240', \
                 'Article 1240', 'Texte pour la projection dense.', \
                 '1804-02-21', 'sha256:article-1240', '{\"chunks\":[]}'); \
             INSERT INTO chunks \
                (chunk_id, document_id, chunk_index, body, contextualized_body, source_payload_hash, \
                 chunk_builder_version, embedding_fingerprint) \
             VALUES \
                ('chunk:pool:0', 'legi:LEGIARTI000006419320@1804-02-21', 0, \
                 'alpha', 'alpha', 'sha256:article-1240', 'chunker:v0', NULL), \
                ('chunk:pool:1', 'legi:LEGIARTI000006419320@1804-02-21', 1, \
                 'beta', 'beta', 'sha256:article-1240', 'chunker:v0', NULL);",
        )?;
    }

    let local_endpoint = spawn_server(1, |request| {
        assert!(request.starts_with("POST /v1/embeddings "));
        assert!(request.contains(r#""model":"bge-m3""#));
        assert!(request.contains(r#""input":["alph"]"#));
        assert!(!request.contains(r#""alpha""#));
        assert!(!request.to_ascii_lowercase().contains("authorization:"));
        thread::sleep(Duration::from_millis(150));
        ok_json(&embedding_response_json(0))
    });
    let mut openrouter_attempt = 0usize;
    let openrouter_endpoint = spawn_server(2, move |request| {
        openrouter_attempt += 1;
        assert!(request.starts_with("POST /api/v1/embeddings "));
        assert!(request.contains(r#""model":"baai/bge-m3""#));
        assert!(request.contains(r#""input":["beta"]"#));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("authorization: bearer openrouter-secret-token")
        );
        if openrouter_attempt == 1 {
            ok_json(r#"{"error":{"message":"transient provider error","code":529}}"#)
        } else {
            ok_json(&embedding_response_json(1))
        }
    });
    let pool =
        format!("{local_endpoint}/v1;{openrouter_endpoint}/api/v1|baai/bge-m3|OPENROUTER_API_KEY");
    let primary_base_url = "http://127.0.0.1:1/v1";

    let output = jurisearch_command_without_embedding_env()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .env("JURISEARCH_CONFIG", "none")
        .env("JURISEARCH_EMBED_BASE_URL", primary_base_url)
        .env("JURISEARCH_EMBED_POOL", pool)
        .env("JURISEARCH_EMBED_MAX_INPUT_CHARS", "4")
        .env("OPENROUTER_API_KEY", "openrouter-secret-token")
        .args([
            "ingest",
            "embed-chunks",
            "--batch-size",
            "1",
            "--pool-concurrency",
            "2",
            "--index-lists",
            "1",
        ])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["command"], "ingest embed-chunks");
    assert_eq!(json["chunks_considered"], 2);
    assert_eq!(json["embeddings_inserted"], 2);
    assert_eq!(json["embedding_inputs_truncated"], 1);
    assert_eq!(
        json["endpoint_pool"]["strategy"],
        "least_outstanding_requests"
    );
    assert_eq!(json["endpoint_pool"]["batch_size"], 1);
    assert_eq!(json["endpoint_pool"]["pool_concurrency"], 2);
    let endpoints = json["endpoint_pool"]["endpoints"].as_array().unwrap();
    assert_eq!(endpoints.len(), 2);
    assert!(
        endpoints
            .iter()
            .all(|endpoint| endpoint["base_url"].as_str().unwrap() != primary_base_url)
    );
    assert!(endpoints.iter().any(|endpoint| {
        endpoint["base_url"].as_str().unwrap().ends_with("/v1")
            && endpoint["request_model"].is_null()
    }));
    assert!(endpoints.iter().any(|endpoint| {
        endpoint["base_url"].as_str().unwrap().ends_with("/api/v1")
            && endpoint["request_model"] == "baai/bge-m3"
    }));
    assert!(endpoints.iter().all(|endpoint| {
        endpoint["requests"].as_u64().unwrap() == 1 && endpoint["chunks"].as_u64().unwrap() == 1
    }));
    assert_eq!(
        endpoints
            .iter()
            .map(|endpoint| endpoint["truncated_inputs"].as_u64().unwrap())
            .sum::<u64>(),
        1
    );
    assert_eq!(
        endpoints
            .iter()
            .map(|endpoint| endpoint["chunks"].as_u64().unwrap())
            .sum::<u64>(),
        2
    );
    assert_eq!(json["dense_rebuild"]["chunks"], 2);
    assert_eq!(json["dense_rebuild"]["embeddings"], 2);
    assert_eq!(json["replay_snapshot_cache"]["source"], "refreshed");
    assert_eq!(json["replay_snapshot_cache"]["status"], "available");
    assert_eq!(json["replay_snapshot_cache"]["chunks"], 2);
    assert_eq!(json["replay_snapshot_cache"]["embeddings"], 2);
    assert_eq!(
        json["replay_snapshot_cache"]["signature"]
            .as_str()
            .unwrap()
            .len(),
        32
    );

    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
    assert_eq!(
        postgres.execute_sql("SELECT count(*)::text FROM chunk_embeddings;")?,
        "2"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT count(*)::text \
             FROM chunks \
             WHERE embedding_fingerprint = 'bge-m3:1024:normalize:true';",
        )?,
        "2"
    );
    assert_eq!(
        postgres.execute_sql(
            "SELECT count(*)::text \
             FROM chunk_embeddings \
             WHERE model = 'bge-m3' AND embedding_fingerprint = 'bge-m3:1024:normalize:true';",
        )?,
        "2"
    );

    Ok(())
}

#[test]
#[ignore = "requires a running OpenAI-compatible bge-m3 embeddings endpoint"]
fn ingest_embed_chunks_uses_live_endpoint_and_finalizes_dense_index()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(pg_config) = discover_pg_config("CLI live embed chunks")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-cli-embed-chunks.")
        .tempdir()?;
    let embedding_base_url = std::env::var("JURISEARCH_EMBED_BASE_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8097/v1".to_owned());

    {
        let postgres = jurisearch_storage::runtime::ManagedPostgres::start_durable(
            pg_config.clone(),
            root.path(),
        )?;
        postgres.execute_sql(
            "INSERT INTO documents \
                (document_id, source, kind, source_uid, citation, title, body, \
                 valid_from, source_payload_hash, canonical_json) \
             VALUES \
                ('legi:LEGIARTI000006419320@1804-02-21', 'legi', 'article', \
                 'LEGIARTI000006419320', 'Code civil article 1240', \
                 'Article 1240', 'Tout fait quelconque de l''homme oblige a reparer le dommage.', \
                 '1804-02-21', 'sha256:article-1240', \
                 '{\"chunks\":[{\"contextualized_body\":\"Code civil > Article 1240\\nresponsabilite civile faute reparation dommage\"}]}'); \
             INSERT INTO chunks \
                (chunk_id, document_id, chunk_index, body, contextualized_body, source_payload_hash, \
                 chunk_builder_version, embedding_fingerprint) \
             VALUES \
                ('chunk:1240:0', 'legi:LEGIARTI000006419320@1804-02-21', 0, \
                 'plain fallback chunk text', \
                 'Code civil > Article 1240\nresponsabilite civile faute reparation dommage', \
                 'sha256:article-1240', 'chunker:v0', NULL);",
        )?;
    }

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .env("JURISEARCH_EMBED_BASE_URL", &embedding_base_url)
        .args(["ingest", "embed-chunks", "--index-lists", "1"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["command"], "ingest embed-chunks");
    assert_eq!(json["chunks_considered"], 1);
    assert_eq!(json["embeddings_inserted"], 1);
    assert_eq!(
        json["embedding"]["fingerprint"],
        "bge-m3:1024:normalize:true"
    );
    assert_eq!(json["dense_rebuild"]["chunks"], 1);
    assert_eq!(json["dense_rebuild"]["embeddings"], 1);
    assert_eq!(json["dense_rebuild"]["index_lists"], 1);

    let postgres =
        jurisearch_storage::runtime::ManagedPostgres::start_durable(pg_config, root.path())?;
    let stored = postgres.execute_sql(
        "SELECT concat(embedding_fingerprint, '|', model, '|', dimension::text) \
         FROM chunk_embeddings \
         WHERE chunk_id = 'chunk:1240:0';",
    )?;
    assert_eq!(stored, "bge-m3:1024:normalize:true|bge-m3|1024");
    let manifest = postgres.execute_sql(
        "SELECT value->>'embedding_fingerprint' \
         FROM index_manifest \
         WHERE key = 'embedding';",
    )?;
    assert_eq!(manifest, "bge-m3:1024:normalize:true");

    Ok(())
}

#[test]
fn ingest_juri_archives_rejects_zero_limit_before_opening_index() {
    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env_remove("JURISEARCH_INDEX_DIR")
        .args([
            "ingest",
            "juri-archives",
            "--source",
            "cass",
            "--archives-dir",
            "/tmp",
            "--limit-members",
            "0",
        ])
        .assert()
        .code(2)
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["code"], "bad_input");
}

#[test]
fn ingest_juri_archives_records_accounting_and_quarantines_failures()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(pg_config) = discover_pg_config("CLI juri archive ingest")? else {
        return Ok(());
    };
    let index = tempfile::Builder::new()
        .prefix("jurisearch-cli-juri-ingest.")
        .tempdir()?;
    let archives = tempfile::Builder::new()
        .prefix("jurisearch-cli-juri-archives.")
        .tempdir()?;
    let quarantine = tempfile::Builder::new()
        .prefix("jurisearch-cli-juri-quarantine.")
        .tempdir()?;
    let archive_path = archives
        .path()
        .join("Freemium_cass_global_20250101-000000.tar.gz");
    write_tar_gz(
        archive_path.as_path(),
        &[
            (
                "juri/cass/JURITEXT000051824029.xml",
                cass_decision_fixture("JURITEXT000051824029", "23-14999").as_slice(),
            ),
            (
                "juri/cass/JURITEXT000051824030.xml",
                cass_decision_fixture("JURITEXT000051824030", "23-15000").as_slice(),
            ),
            // Missing required ID/DATE_DEC -> parse failure -> quarantined.
            (
                "juri/cass/BROKEN.xml",
                b"<TEXTE_JURI_JUDI><META/></TEXTE_JURI_JUDI>",
            ),
        ],
    )?;

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .arg("--index-dir")
        .arg(index.path())
        .args([
            "ingest",
            "juri-archives",
            "--source",
            "cass",
            "--archives-dir",
        ])
        .arg(archives.path())
        .args(["--run-id", "run-juri", "--quarantine-dir"])
        .arg(quarantine.path())
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["command"], "ingest juri-archives");
    assert_eq!(json["source"], "cass");
    assert_eq!(json["run_id"], "run-juri");
    assert_eq!(json["run_status"], "failed"); // one broken member
    // Honest provenance surfaced at the command + manifest level.
    assert_eq!(json["zone_accurate"], false);
    assert_eq!(json["chunking_provenance"], "heuristic");
    assert_eq!(json["visited_members"], 3);
    assert_eq!(json["inserted_documents"], 2);
    assert_eq!(json["failed_members"], 1);
    assert_eq!(json["quarantined_payloads"], 1);
    assert!(json["inserted_publisher_edges"].as_u64().unwrap() >= 2);
    assert_eq!(json["manifest"]["source"], "cass");
    assert_eq!(json["manifest"]["dataset"], "CASS");
    assert_eq!(json["manifest"]["zone_accurate"], false);
    assert_eq!(json["manifest"]["chunking_provenance"], "heuristic");
    assert_eq!(json["manifest"]["source_version"], "20250101-000000");

    let quarantine_entries =
        fs::read_dir(quarantine.path().join("run-juri"))?.collect::<Result<Vec<_>, _>>()?;
    assert_eq!(quarantine_entries.len(), 1);

    // The two decisions are persisted as kind='decision' with the publisher edge resolved.
    let postgres = ManagedPostgres::start_durable(pg_config, index.path())?;
    let decision_count =
        postgres.execute_sql("SELECT count(*) FROM documents WHERE kind = 'decision';")?;
    assert_eq!(decision_count.trim(), "2");
    let resolved_edge = postgres.execute_sql(
        "SELECT count(*) FROM graph_edges \
         WHERE edge_source = 'publisher' AND payload->>'to_source_uid' = 'LEGIARTI000006900782';",
    )?;
    assert_eq!(resolved_edge.trim(), "2");
    postgres.stop()?;

    Ok(())
}

#[test]
fn ingest_juri_archives_skips_empty_body_decisions() -> Result<(), Box<dyn std::error::Error>> {
    let Some(_pg_config) = discover_pg_config("CLI juri empty body")? else {
        return Ok(());
    };
    let index = tempfile::Builder::new()
        .prefix("jurisearch-cli-juri-empty.")
        .tempdir()?;
    let archives = tempfile::Builder::new()
        .prefix("jurisearch-cli-juri-empty-archives.")
        .tempdir()?;
    let archive_path = archives
        .path()
        .join("Freemium_cass_global_20250101-000000.tar.gz");
    // An empty-CONTENU decision (metadata only) alongside a normal one: the run must COMPLETE,
    // skipping the empty record (not abort the whole ingest).
    let empty_body = br#"<?xml version="1.0" encoding="UTF-8"?>
<TEXTE_JURI_JUDI>
<META><META_COMMUN><ID>JURITEXT000000099999</ID><ANCIEN_ID/><ORIGINE>JURI</ORIGINE>
<URL>texte/juri/judi/JURI/TEXT/.../JURITEXT000000099999.xml</URL><NATURE>ARRET</NATURE>
</META_COMMUN><META_SPEC><META_JURI>
<TITRE>Cour de cassation, 1 janvier 2025</TITRE><DATE_DEC>2025-01-01</DATE_DEC>
<JURIDICTION>Cour de cassation</JURIDICTION><NUMERO>X1</NUMERO><SOLUTION/>
</META_JURI><META_JURI_JUDI><NUMEROS_AFFAIRES/><PUBLI_BULL publie="non"/><FORMATION/></META_JURI_JUDI>
</META_SPEC></META>
<TEXTE><BLOC_TEXTUEL><CONTENU> </CONTENU></BLOC_TEXTUEL><SOMMAIRE/></TEXTE><LIENS/>
</TEXTE_JURI_JUDI>"#;
    write_tar_gz(
        archive_path.as_path(),
        &[
            (
                "juri/cass/JURITEXT000051824029.xml",
                cass_decision_fixture("JURITEXT000051824029", "23-14999").as_slice(),
            ),
            ("juri/cass/JURITEXT000000099999.xml", empty_body),
        ],
    )?;

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .arg("--index-dir")
        .arg(index.path())
        .args([
            "ingest",
            "juri-archives",
            "--source",
            "cass",
            "--archives-dir",
        ])
        .arg(archives.path())
        .args(["--run-id", "run-empty"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["run_status"], "completed"); // empty body did NOT abort the run
    assert_eq!(json["inserted_documents"], 1); // the valid decision
    assert_eq!(json["skipped_empty_body_members"], 1);
    assert_eq!(
        json["manifest"]["coverage"]["skipped_empty_body_members"],
        1
    );
    assert_eq!(json["failed_members"], 0);

    Ok(())
}

#[test]
fn ingest_juri_archives_compatible_replay_skips_inserted_members()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(_pg_config) = discover_pg_config("CLI juri replay skip")? else {
        return Ok(());
    };
    let index = tempfile::Builder::new()
        .prefix("jurisearch-cli-juri-replay.")
        .tempdir()?;
    let archives = tempfile::Builder::new()
        .prefix("jurisearch-cli-juri-replay-archives.")
        .tempdir()?;
    let archive_path = archives
        .path()
        .join("Freemium_jade_global_20250101-000000.tar.gz");
    write_tar_gz(
        archive_path.as_path(),
        &[(
            "juri/jade/CETATEXT000051549953.xml",
            br#"<?xml version="1.0" encoding="UTF-8"?>
<TEXTE_JURI_ADMIN>
<META><META_COMMUN><ID>CETATEXT000051549953</ID><ANCIEN_ID/><ORIGINE>CETAT</ORIGINE>
<URL>texte/juri/admin/CETA/TEXT/.../CETATEXT000051549953.xml</URL><NATURE>Texte</NATURE>
</META_COMMUN><META_SPEC><META_JURI>
<TITRE>CAA de PARIS, 9eme chambre, 30/04/2025, 24PA03561</TITRE>
<DATE_DEC>2025-04-30</DATE_DEC><JURIDICTION>CAA de PARIS</JURIDICTION>
<NUMERO>24PA03561</NUMERO><SOLUTION/>
</META_JURI><META_JURI_ADMIN>
<FORMATION>9eme chambre</FORMATION><TYPE_REC>exces de pouvoir</TYPE_REC><PUBLI_RECUEIL>C</PUBLI_RECUEIL>
</META_JURI_ADMIN></META_SPEC></META>
<TEXTE><BLOC_TEXTUEL><CONTENU>Le refus de renouvellement du titre de sejour est legal.</CONTENU></BLOC_TEXTUEL><SOMMAIRE/></TEXTE>
<LIENS/>
</TEXTE_JURI_ADMIN>"#,
        )],
    )?;

    let run = |run_id: &str| {
        Command::cargo_bin("jurisearch")
            .unwrap()
            .arg("--index-dir")
            .arg(index.path())
            .args([
                "ingest",
                "juri-archives",
                "--source",
                "jade",
                "--archives-dir",
            ])
            .arg(archives.path())
            .args(["--run-id", run_id])
            .assert()
            .success()
            .get_output()
            .stdout
            .clone()
    };

    let first: Value = serde_json::from_slice(&run("run-1"))?;
    assert_eq!(first["inserted_documents"], 1);
    assert_eq!(first["run_status"], "completed");

    // A second run over the same unchanged archive must skip the already-inserted member.
    let second: Value = serde_json::from_slice(&run("run-2"))?;
    assert_eq!(second["inserted_documents"], 0);
    assert_eq!(second["skipped_compatible_members"], 1);
    assert_eq!(second["run_status"], "completed");

    // status reports per-source jurisprudence coverage + freshness with honest provenance.
    let status = Command::cargo_bin("jurisearch")
        .unwrap()
        .arg("--index-dir")
        .arg(index.path())
        .arg("status")
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let status: Value = serde_json::from_slice(&status)?;
    let jade = &status["corpus_sources"]["jade"];
    assert_eq!(jade["dataset"], "JADE");
    assert_eq!(jade["source_version"], "20250101-000000");
    assert_eq!(jade["zone_accurate"], false);
    assert_eq!(jade["chunking_provenance"], "heuristic");
    assert!(jade["latest_completed_run"].is_string());
    assert_eq!(
        jade["freshness"]["latest_archive"],
        "Freemium_jade_global_20250101-000000.tar.gz"
    );
    // The latest run here is a no-op replay, so its per-run inserts are 0 (cumulative corpus
    // counts live in `stats`); freshness + honest provenance are what status surfaces.
    assert_eq!(jade["last_run_coverage"]["inserted_documents"], 0);

    Ok(())
}

#[test]
fn enrich_legislation_citations_archives_missing_credential_attempt() -> Result<(), StorageError> {
    // Slice-2 review fix: with NO Legifrance OAuth credential, the command must NOT short-circuit — it
    // must still archive every attempt as an upstream_error in official_api_responses AND record the
    // resolution as upstream_error (uniform durable accounting, not a silent skip).
    let Some(pg_config) = discover_pg_config("CLI legislation missing cred")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-cli-legi-cred.")
        .tempdir()
        .map_err(StorageError::Io)?;
    {
        let postgres = jurisearch_storage::runtime::ManagedPostgres::start_durable(
            pg_config.clone(),
            root.path(),
        )?;
        // Seed a realistic corpus-attribution chain (P0): document -> archived /decision ->
        // occurrence -> resolution, so the citation 'legi-cite:test' resolves to corpus 'core' and
        // the Legifrance archive write during enrich can derive its corpus from the occurrence.
        postgres.execute_sql(
            "INSERT INTO documents (document_id, source, kind, source_uid, citation, title, body, \
               valid_from, source_payload_hash, canonical_json) \
             VALUES ('cass:DEC1','cass','decision','cass:DEC1','Cass','Arret','corps','2024-01-01', \
               'sha256:dec1','{}'); \
             INSERT INTO official_api_responses \
                 (provider, api_environment, endpoint, http_method, subject_document_id, \
                  request_fingerprint, outcome, response_body_sha256, corpus) \
             VALUES ('judilibre','production','/decision','GET','cass:DEC1','fp-dec1','ok','sha256:x','core'); \
             INSERT INTO decision_legislation_citations \
                 (citation_occurrence_id, decision_document_id, decision_source_uid, \
                  source_response_id, visa_index, citation_key, article_number_norm, \
                  code_name_norm, canonical_query, raw_title, extraction_method) \
             SELECT 'occ1','cass:DEC1','cass:DEC1', r.response_id, 0, 'legi-cite:test','609', \
                  'code de procédure civile','609 code de procédure civile', \
                  'Article 609 du code de procédure civile.','visa_title_regex' \
             FROM official_api_responses r WHERE r.subject_document_id='cass:DEC1' LIMIT 1; \
             INSERT INTO legislation_citation_resolutions \
                 (citation_key, article_number_norm, code_name_norm, canonical_query, corpus) \
             VALUES ('legi-cite:test','609','code de procédure civile', \
                 '609 code de procédure civile','core');",
        )?;
    }

    let output = jurisearch_command_without_embedding_env()
        .env_remove("JURISEARCH_INDEX_DIR")
        .env_remove("PISTE_ENV")
        .env_remove("JURISEARCH_PISTE_ENV")
        .env_remove("PISTE_OAUTH_CLIENT_ID")
        .env_remove("JURISEARCH_PISTE_LEGIFRANCE_CLIENT_ID")
        .arg("--index-dir")
        .arg(root.path())
        .arg("ingest")
        .arg("enrich-legislation-citations")
        .arg("--limit")
        .arg("1")
        .output()
        .expect("run enrich-legislation-citations");
    assert!(
        output.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).expect("report JSON");
    assert_eq!(report["considered"], 1);
    assert_eq!(report["errors"], 1);
    assert_eq!(report["resolved_ok"], 0);

    // The missing-credential attempt is durably archived, not skipped.
    let postgres =
        jurisearch_storage::runtime::ManagedPostgres::start_durable(pg_config, root.path())?;
    let archived = postgres.execute_sql(
        "SELECT count(*)::text FROM official_api_responses \
         WHERE provider='legifrance' AND outcome='upstream_error';",
    )?;
    assert_eq!(
        archived.trim(),
        "1",
        "the failed Legifrance attempt is archived"
    );
    let status = postgres.execute_sql(
        "SELECT legifrance_status FROM legislation_citation_resolutions \
         WHERE citation_key='legi-cite:test';",
    )?;
    assert_eq!(
        status.trim(),
        "upstream_error",
        "resolution recorded as upstream_error"
    );
    Ok(())
}
