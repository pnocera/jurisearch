use std::{
    fs::{self, File},
    io::Cursor,
    path::Path,
};

use assert_cmd::Command;
use flate2::{Compression, write::GzEncoder};
use jurisearch_embed::{EmbeddingConfig, OpenAiCompatibleClient};
use jurisearch_storage::{
    ingest_accounting::{
        IngestCompatibility, IngestMemberInput, IngestMemberStatus, IngestRunInput,
        finish_ingest_run, record_ingest_member, start_ingest_run,
    },
    runtime::{ManagedPostgres, PgConfig, StorageError},
};
use predicates::prelude::*;
use serde_json::Value;
use tar::{Builder, Header};

#[test]
fn help_agent_works_without_index() {
    let mut command = Command::cargo_bin("jurisearch").unwrap();
    command
        .args(["help", "agent"])
        .assert()
        .success()
        .stdout(predicate::str::contains("jurisearch agent contract"))
        .stdout(predicate::str::contains("search"))
        .stdout(predicate::str::contains("help schema --json"));
}

#[test]
fn help_schema_json_is_valid_and_lists_commands() {
    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .args(["help", "schema", "--json"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["schema_version"], "1");
    assert!(json["commands"].as_array().unwrap().iter().any(|command| {
        command["name"] == "search"
            && command["status"] == "implemented"
            && command["request_schema"] == "SearchRequest"
    }));
    assert_eq!(json["common_enums"]["kind"]["values"][0], "code");
}

#[test]
fn status_returns_json_without_index() {
    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env_remove("JURISEARCH_INDEX_DIR")
        .arg("status")
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["schema_version"], "1");
    assert_eq!(json["index"]["state"], "not_configured");
    assert_eq!(json["index"]["query_ready"], false);
    assert_eq!(json["ingest_health"]["state"], "pending");
    assert_eq!(json["embedding"]["provider"], "openai_compatible");
    assert_eq!(json["embedding"]["base_url_class"], "local_loopback");
    assert_eq!(json["embedding"]["model"], "bge-m3");
    assert_eq!(json["embedding"]["dimension"], 1024);
    assert_eq!(json["embedding"]["pooling"], "cls");
    assert_eq!(json["embedding"]["max_input_chars"], 24_000);
    assert_eq!(json["embedding"]["max_estimated_tokens"], 8_192);
    assert_eq!(json["embedding"]["estimated_chars_per_token"], 4);
    assert_eq!(json["embedding"]["provisional"], true);
    assert_eq!(json["embedding"]["reembeddable"], true);
}

#[test]
fn status_reports_embedding_budget_env_overrides() {
    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env_remove("JURISEARCH_INDEX_DIR")
        .env("JURISEARCH_EMBED_MAX_INPUT_CHARS", "0")
        .env("JURISEARCH_EMBED_MAX_ESTIMATED_TOKENS", "none")
        .env("JURISEARCH_EMBED_ESTIMATED_CHARS_PER_TOKEN", "3")
        .arg("status")
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert!(json["embedding"]["max_input_chars"].is_null());
    assert!(json["embedding"]["max_estimated_tokens"].is_null());
    assert_eq!(json["embedding"]["estimated_chars_per_token"], 3);
}

#[test]
fn status_reports_not_initialized_index_without_opening_postgres() -> Result<(), StorageError> {
    let root = tempfile::Builder::new()
        .prefix("jurisearch-cli-status-empty.")
        .tempdir()
        .map_err(StorageError::Io)?;

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .arg("status")
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["index"]["state"], "not_initialized");
    assert_eq!(json["index"]["query_ready"], false);
    assert_eq!(json["ingest_health"]["state"], "pending");
    Ok(())
}

#[test]
fn status_reports_ingest_health_from_existing_index() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("CLI status ingest health")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-cli-status-health.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let compatibility = IngestCompatibility {
        parser_version: "legi-parser:v1",
        schema_version: "canonical:v1",
        code_version: "test-code-sha",
        source_payload_hash: "sha256:article-1240",
    };

    {
        let postgres = jurisearch_storage::runtime::ManagedPostgres::start_durable(
            pg_config.clone(),
            root.path(),
        )?;
        start_ingest_run(
            &postgres,
            &IngestRunInput {
                run_id: "run-status",
                source: "legi",
                parser_version: compatibility.parser_version,
                schema_version: compatibility.schema_version,
                code_version: compatibility.code_version,
                safe_mode: false,
                archive_plan_json: None,
                manifest_json: None,
            },
        )?;
        record_ingest_member(
            &postgres,
            &IngestMemberInput {
                run_id: "run-status",
                archive_name: "Freemium_legi_global_20240101-000000.tar.gz",
                member_path: "legi/articles/LEGIARTI000006419320.xml",
                source: "legi",
                source_entity: Some("LEGIARTI000006419320"),
                date_anchor: Some("1804-02-21"),
                status: IngestMemberStatus::Inserted,
                compatibility,
            },
        )?;
        postgres.execute_sql(&format!(
            "INSERT INTO documents \
                (document_id, source, kind, source_uid, citation, title, body, \
                 valid_from, source_payload_hash, canonical_json) \
             VALUES \
                ('legi:LEGIARTI000006419320@1804-02-21', 'legi', 'article', \
                 'LEGIARTI000006419320', 'Code civil article 1240', \
                 'Article 1240', 'Tout fait quelconque de l''homme oblige a reparer le dommage.', \
                 '1804-02-21', 'sha256:article-1240', '{{\"official\":true}}'); \
             INSERT INTO chunks \
                (chunk_id, document_id, chunk_index, body, source_payload_hash, \
                 chunk_builder_version, embedding_fingerprint) \
             VALUES \
                ('chunk:1240:0', 'legi:LEGIARTI000006419320@1804-02-21', 0, \
                 'responsabilite civile faute reparation dommage article 1240', \
                 'sha256:article-1240', 'chunker:v0', 'bge-m3:1024:normalize:true'); \
             INSERT INTO chunk_embeddings \
                (chunk_id, embedding_fingerprint, embedding, model, dimension) \
             VALUES \
                ('chunk:1240:0', 'bge-m3:1024:normalize:true', '{}', 'bge-m3', 1024);",
            unit_vector_literal(0)
        ))?;
        finish_ingest_run(
            &postgres,
            "run-status",
            jurisearch_storage::ingest_accounting::IngestRunStatus::Completed,
            None,
        )?;
    }

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .arg("status")
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["index"]["state"], "ready");
    assert_eq!(json["index"]["query_ready"], true);
    assert_eq!(json["ingest_health"]["state"], "available");
    assert_eq!(json["ingest_health"]["latest_run_id"], "run-status");
    assert_eq!(json["ingest_health"]["latest_completed_run"], "run-status");
    assert_eq!(json["ingest_health"]["total_members"], 1);
    assert_eq!(json["ingest_health"]["inserted_members"], 1);
    assert_eq!(json["ingest_health"]["projection_coverage"]["covered"], 1);
    assert_eq!(json["ingest_health"]["projection_coverage"]["total"], 1);
    assert_eq!(json["ingest_health"]["embedding_coverage"]["covered"], 1);
    assert_eq!(json["ingest_health"]["embedding_coverage"]["total"], 1);
    assert_eq!(json["ingest_health"]["replay_snapshot_status"], "available");
    assert_eq!(
        json["ingest_health"]["replay_snapshot"]["documents"]["count"],
        1
    );
    assert_eq!(
        json["ingest_health"]["replay_snapshot"]["chunks"]["count"],
        1
    );
    assert_eq!(
        json["ingest_health"]["replay_snapshot"]["embeddings"]["count"],
        1
    );
    assert_eq!(
        json["ingest_health"]["replay_snapshot"]["signature"]
            .as_str()
            .unwrap()
            .len(),
        32
    );
    assert!(
        json["ingest_health"]["recovery_warnings"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    let input = format!(
        "{{\"id\":\"status-index\",\"command\":\"status\",\"args\":{{\"index_dir\":\"{}\"}}}}\n",
        root.path().to_string_lossy()
    );
    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env_remove("JURISEARCH_INDEX_DIR")
        .args(["session", "--jsonl"])
        .write_stdin(input)
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["id"], "status-index");
    assert_eq!(json["ok"], true);
    assert_eq!(json["result"]["index"]["query_ready"], true);
    assert_eq!(json["result"]["ingest_health"]["state"], "available");

    {
        let postgres = jurisearch_storage::runtime::ManagedPostgres::start_durable(
            pg_config.clone(),
            root.path(),
        )?;
        postgres.execute_sql(
            "UPDATE chunk_embeddings \
             SET embedding_fingerprint = 'stale-fingerprint' \
             WHERE chunk_id = 'chunk:1240:0';",
        )?;
    }
    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .arg("status")
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["index"]["query_ready"], false);
    assert_eq!(json["ingest_health"]["embedding_coverage"]["covered"], 0);
    assert_eq!(json["ingest_health"]["embedding_coverage"]["total"], 1);

    {
        let postgres =
            jurisearch_storage::runtime::ManagedPostgres::start_durable(pg_config, root.path())?;
        postgres.execute_sql(
            "UPDATE chunk_embeddings \
             SET embedding_fingerprint = 'bge-m3:1024:normalize:true' \
             WHERE chunk_id = 'chunk:1240:0';",
        )?;
    }
    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .arg("status")
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["index"]["query_ready"], true);
    assert_eq!(json["ingest_health"]["embedding_coverage"]["covered"], 1);
    assert_eq!(json["ingest_health"]["embedding_coverage"]["total"], 1);
    Ok(())
}

#[test]
fn status_marks_initialized_index_not_ready_when_embedding_coverage_is_incomplete()
-> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("CLI status incomplete coverage")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-cli-status-incomplete.")
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
                 'Article 1240', 'Tout fait quelconque de l''homme oblige a reparer le dommage.', \
                 '1804-02-21', 'sha256:article-1240', '{\"official\":true}'); \
             INSERT INTO chunks \
                (chunk_id, document_id, chunk_index, body, source_payload_hash, \
                 chunk_builder_version, embedding_fingerprint) \
             VALUES \
                ('chunk:1240:0', 'legi:LEGIARTI000006419320@1804-02-21', 0, \
                 'responsabilite civile faute reparation dommage article 1240', \
                 'sha256:article-1240', 'chunker:v0', 'bge-m3:1024:normalize:true');",
        )?;
    }

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .arg("status")
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["index"]["state"], "ready");
    assert_eq!(json["index"]["query_ready"], false);
    assert_eq!(json["ingest_health"]["projection_coverage"]["covered"], 1);
    assert_eq!(json["ingest_health"]["projection_coverage"]["total"], 1);
    assert_eq!(json["ingest_health"]["embedding_coverage"]["covered"], 0);
    assert_eq!(json["ingest_health"]["embedding_coverage"]["total"], 1);

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .env("JURISEARCH_EMBED_BASE_URL", "http://127.0.0.1:9/v1")
        .args(["search", "responsabilite civile"])
        .assert()
        .code(3)
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["code"], "index_unavailable");
    assert!(
        json["error"]["message"]
            .as_str()
            .unwrap()
            .contains("embedding coverage gate is incomplete")
    );
    Ok(())
}

#[test]
fn retrieval_commands_reject_incomplete_projection_coverage() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("CLI retrieval projection gate")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-cli-retrieval-projection-gate.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let document_id = "legi:LEGIARTI000006419320@1804-02-21";

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
                 'Article 1240', 'Tout fait quelconque de l''homme oblige a reparer le dommage.', \
                 '1804-02-21', 'sha256:article-1240', '{\"official\":true}');",
        )?;
    }

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .env("JURISEARCH_EMBED_BASE_URL", "http://127.0.0.1:9/v1")
        .args(["search", "responsabilite civile"])
        .assert()
        .code(3)
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    assert_json_error_contains(
        &output,
        "index_unavailable",
        "projection coverage gate is incomplete",
    );

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .args(["fetch", document_id])
        .assert()
        .code(3)
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    assert_json_error_contains(
        &output,
        "index_unavailable",
        "projection coverage gate is incomplete",
    );
    Ok(())
}

#[test]
fn retrieval_commands_reject_empty_initialized_index() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("CLI retrieval empty index gate")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-cli-retrieval-empty-gate.")
        .tempdir()
        .map_err(StorageError::Io)?;

    {
        let _postgres =
            jurisearch_storage::runtime::ManagedPostgres::start_durable(pg_config, root.path())?;
    }

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .env("JURISEARCH_EMBED_BASE_URL", "http://127.0.0.1:9/v1")
        .args(["search", "responsabilite civile"])
        .assert()
        .code(3)
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    assert_json_error_contains(
        &output,
        "index_unavailable",
        "projection coverage gate is incomplete",
    );

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .args(["fetch", "legi:LEGIARTI000006419320@1804-02-21"])
        .assert()
        .code(3)
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    assert_json_error_contains(
        &output,
        "index_unavailable",
        "projection coverage gate is incomplete",
    );
    Ok(())
}

#[test]
fn bad_input_is_json_and_uses_exit_code_2() {
    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env_remove("JURISEARCH_INDEX_DIR")
        .args(["search", ""])
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
fn retrieval_command_without_index_is_json_and_uses_exit_code_3() {
    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env_remove("JURISEARCH_INDEX_DIR")
        .args(["fetch", "legi:LEGIARTI000000000000@2020-01-01"])
        .assert()
        .code(3)
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["code"], "index_unavailable");
}

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

#[test]
fn ingest_embed_chunks_rejects_zero_index_lists_before_opening_index() {
    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env_remove("JURISEARCH_INDEX_DIR")
        .args(["ingest", "embed-chunks", "--index-lists", "0"])
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
            "SELECT string_agg(error_code, ',' ORDER BY error_code) FROM ingest_error;"
        )?,
        "validation_missing_required_field"
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
                (chunk_id, document_id, chunk_index, body, source_payload_hash,
                 chunk_builder_version, embedding_fingerprint)
             VALUES
                ('chunk:1240:0', 'legi:LEGIARTI000006419320@1804-02-21', 0,
                 'Texte initial pour le test.', 'sha256:article-1240',
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
fn fetch_returns_documents_from_existing_index() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("CLI fetch existing index")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-cli-fetch.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let document_id = "legi:LEGIARTI000006419320@1804-02-21";

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
                 'Article 1240', 'Tout fait quelconque de l''homme oblige a reparer le dommage.', \
                 '1804-02-21', 'sha256:article-1240', '{\"official\":true}'); \
             INSERT INTO chunks \
                (chunk_id, document_id, chunk_index, body, source_payload_hash, \
                 chunk_builder_version, embedding_fingerprint) \
             VALUES \
                ('chunk:1240:0', 'legi:LEGIARTI000006419320@1804-02-21', 0, \
                 'responsabilite civile faute reparation dommage article 1240', \
                 'sha256:article-1240', 'chunker:v0', 'bge-m3:1024:normalize:true');",
        )?;
    }

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .args(["fetch", document_id])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["documents"][0]["document_id"], document_id);
    assert_eq!(
        json["documents"][0]["chunks"][0]["chunk_id"],
        "chunk:1240:0"
    );

    let input = format!(
        "{{\"id\":\"fetch-one\",\"command\":\"fetch\",\"args\":{{\"ids\":[\"{document_id}\"]}}}}\n"
    );
    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .args(["session", "--jsonl"])
        .write_stdin(input)
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["id"], "fetch-one");
    assert_eq!(json["ok"], true);
    assert_eq!(json["result"]["documents"][0]["document_id"], document_id);

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .args(["fetch", "legi:LEGIARTI999999999999@2024-01-01"])
        .assert()
        .code(2)
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["code"], "no_results");

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .args(["fetch", document_id, "--as-of", "2024-01-01"])
        .assert()
        .code(2)
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["code"], "bad_input");
    Ok(())
}

#[test]
fn ingest_embed_chunks_budget_error_names_offending_chunk() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("CLI embed chunk budget failure")? else {
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
                (chunk_id, document_id, chunk_index, body, source_payload_hash, \
                 chunk_builder_version, embedding_fingerprint) \
             VALUES \
                ('chunk:1240:0', 'legi:LEGIARTI000006419320@1804-02-21', 0, \
                 'abcde', \
                 'sha256:article-1240', 'chunker:v0', NULL);",
        )?;
    }

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .env("JURISEARCH_EMBED_MAX_INPUT_CHARS", "4")
        .args(["ingest", "embed-chunks", "--index-lists", "1"])
        .assert()
        .code(2)
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["code"], "bad_input");
    let message = json["error"]["message"].as_str().unwrap();
    assert!(message.contains("chunk:1240:0"));
    assert!(message.contains("embedding input is too long"));

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
                (chunk_id, document_id, chunk_index, body, source_payload_hash, \
                 chunk_builder_version, embedding_fingerprint) \
             VALUES \
                ('chunk:1240:0', 'legi:LEGIARTI000006419320@1804-02-21', 0, \
                 'plain fallback chunk text', \
                 'sha256:article-1240', 'chunker:v0', NULL);",
        )?;
    }

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .env("JURISEARCH_EMBED_BASE_URL", embedding_base_url)
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
#[ignore = "requires a running OpenAI-compatible bge-m3 embeddings endpoint"]
fn search_returns_results_from_existing_index_with_live_embeddings()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(pg_config) = discover_pg_config("CLI live search existing index")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-cli-search.")
        .tempdir()?;
    let query = "responsabilite civile faute dommage article 1240";
    let document_id = "legi:LEGIARTI000006419320@1804-02-21";
    let embedding_base_url = std::env::var("JURISEARCH_EMBED_BASE_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8097/v1".to_owned());
    let embedding_config = EmbeddingConfig::phase0_bge_m3(
        &embedding_base_url,
        std::env::var("JURISEARCH_EMBED_API_KEY").ok(),
    );
    let expected = embedding_config.fingerprint();
    let storage_fingerprint = expected.storage_embedding_fingerprint();
    let client = OpenAiCompatibleClient::new(embedding_config)?;
    let embedding = client.embed_query(query, &expected)?;
    let embedding = pgvector_literal(&embedding.values);

    {
        let postgres =
            jurisearch_storage::runtime::ManagedPostgres::start_durable(pg_config, root.path())?;
        postgres.execute_sql(&format!(
            "INSERT INTO documents \
                (document_id, source, kind, source_uid, citation, title, body, \
                 valid_from, source_payload_hash, canonical_json) \
             VALUES \
                ('{document_id}', 'legi', 'article', \
                 'LEGIARTI000006419320', 'Code civil article 1240', \
                 'Article 1240', 'Tout fait quelconque de l''homme oblige a reparer le dommage.', \
                 '1804-02-21', 'sha256:article-1240', '{{\"official\":true}}'); \
             INSERT INTO chunks \
                (chunk_id, document_id, chunk_index, body, source_payload_hash, \
                 chunk_builder_version, embedding_fingerprint) \
             VALUES \
                ('chunk:1240:0', '{document_id}', 0, \
                 'responsabilite civile faute reparation dommage article 1240', \
                 'sha256:article-1240', 'chunker:v0', '{storage_fingerprint}'); \
             INSERT INTO chunk_embeddings \
                (chunk_id, embedding_fingerprint, embedding, model, dimension) \
             VALUES \
                ('chunk:1240:0', '{storage_fingerprint}', '{}', 'bge-m3', 1024);",
            embedding
        ))?;
    }

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .env("JURISEARCH_EMBED_BASE_URL", embedding_base_url)
        .args(["search", query, "--as-of", "2024-01-01", "--top-k", "3"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["candidates"][0]["document_id"], document_id);
    assert_eq!(json["candidates"][0]["scores"]["dense_rank"], 1);
    Ok(())
}

#[test]
fn session_jsonl_preserves_order_handles_bad_json_and_exit() {
    let input = concat!(
        "{\"id\":\"one\",\"command\":\"status\"}\n",
        "not-json\n",
        "{\"id\":\"two\",\"command\":\"help schema\"}\n",
        "{\"id\":\"three\",\"command\":\"exit\"}\n",
    );

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .args(["session", "--jsonl"])
        .write_stdin(input)
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let lines = String::from_utf8(output).unwrap();
    let values = lines
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .collect::<Vec<_>>();

    assert_eq!(values.len(), 4);
    assert_eq!(values[0]["id"], "one");
    assert_eq!(values[0]["ok"], true);
    assert_eq!(values[1]["ok"], false);
    assert_eq!(values[1]["error"]["code"], "bad_input");
    assert_eq!(values[2]["id"], "two");
    assert_eq!(values[2]["ok"], true);
    assert_eq!(values[3]["id"], "three");
    assert_eq!(values[3]["result"]["bye"], true);
}

fn discover_pg_config(test_name: &str) -> Result<Option<PgConfig>, StorageError> {
    let pg_config = match PgConfig::discover() {
        Ok(pg_config) => pg_config,
        Err(error @ StorageError::MissingPgConfig { .. }) => {
            if std::env::var_os("JURISEARCH_REQUIRE_PG_EXTENSIONS").is_some() {
                return Err(error);
            }
            eprintln!("skipping {test_name}: {error}");
            return Ok(None);
        }
        Err(error) => return Err(error),
    };

    for extension in ["pg_search", "vector"] {
        if let Err(error) = pg_config.require_extension_assets(extension) {
            if std::env::var_os("JURISEARCH_REQUIRE_PG_EXTENSIONS").is_some() {
                return Err(error);
            }
            eprintln!("skipping {test_name}: {error}");
            return Ok(None);
        }
    }

    Ok(Some(pg_config))
}

fn assert_json_error_contains(output: &[u8], code: &str, message: &str) {
    let json: Value = serde_json::from_slice(output).unwrap();
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["code"], code);
    assert!(json["error"]["message"].as_str().unwrap().contains(message));
}

fn write_tar_gz(path: &Path, members: &[(&str, &[u8])]) -> Result<(), Box<dyn std::error::Error>> {
    let file = File::create(path)?;
    let encoder = GzEncoder::new(file, Compression::default());
    let mut builder = Builder::new(encoder);
    for (member_path, bytes) in members {
        let mut header = Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, member_path, Cursor::new(bytes))?;
    }
    builder.finish()?;
    Ok(())
}

fn article_fixture() -> String {
    r#"
<ARTICLE>
  <META>
    <META_COMMUN>
      <ID>LEGIARTI000006419320</ID>
      <URL>/codes/article_lc/LEGIARTI000006419320</URL>
      <NATURE>Article</NATURE>
    </META_COMMUN>
    <META_ARTICLE>
      <NUM>1240</NUM>
      <ETAT>VIGUEUR</ETAT>
      <TYPE>AUTONOME</TYPE>
      <DATE_DEBUT>1804-02-21</DATE_DEBUT>
      <DATE_FIN>2999-01-01</DATE_FIN>
    </META_ARTICLE>
  </META>
  <CONTEXTE>
    <TEXTE>
      <TITRE_TXT>Code civil</TITRE_TXT>
      <TM>
        <TITRE_TM>Livre III : Des differentes manieres dont on acquiert la propriete</TITRE_TM>
        <TM>
          <TITRE_TM>Titre IV : Des engagements qui se forment sans convention</TITRE_TM>
        </TM>
      </TM>
    </TEXTE>
  </CONTEXTE>
  <BLOC_TEXTUEL>
    <CONTENU>
      <p>Tout fait quelconque de l'homme, qui cause a autrui un dommage, oblige celui par la faute duquel il est arrive a le reparer.</p>
    </CONTENU>
  </BLOC_TEXTUEL>
  <LIENS>
    <LIEN cidtexte="JORFTEXT000000696195" id="LEGIARTI000006554637" sens="cible" typelien="MODIFICATION">Decret no 73-138 - art. 11</LIEN>
  </LIENS>
</ARTICLE>
"#
    .to_owned()
}

fn pgvector_literal(values: &[f32]) -> String {
    let values = values
        .iter()
        .map(|value| format!("{value:.8}"))
        .collect::<Vec<_>>()
        .join(",");
    format!("[{values}]")
}

fn unit_vector_literal(active_index: usize) -> String {
    let values = (0..1024)
        .map(|index| if index == active_index { 1.0 } else { 0.0 })
        .collect::<Vec<_>>();
    pgvector_literal(&values)
}
