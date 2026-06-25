//! Retrieval-command contract tests (search/fetch/cite/context/expand + readiness).
#![allow(unused_imports)]

use std::{
    fs::{self, File},
    io::{Cursor, Read, Write},
    net::TcpListener,
    path::Path,
    thread,
    time::Duration,
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

mod support;
use support::*;

#[test]
fn expand_returns_curated_terms_with_review_metadata_without_index() {
    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env_remove("JURISEARCH_INDEX_DIR")
        .args(["expand", "faute dommage"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["query"], "faute dommage");
    assert_eq!(json["seed_version"], "legal-vocabulary-seed:v1");
    let article_1240 = json["expanded_terms"]
        .as_array()
        .unwrap()
        .iter()
        .find(|term| {
            term["term"] == "article 1240"
                && term["source_seed_id"] == "civil-liability-fault-damage"
                && term["source_citation"] == "Code civil, articles 1240 et 1241"
        });
    let article_1240 = article_1240.expect("article 1240 expansion is present");
    assert_eq!(
        article_1240["review_status"],
        "dev_seed_pending_legal_review"
    );
    assert_eq!(article_1240["reviewer"], "pending_legal_domain_review");
    assert_eq!(
        article_1240["matched_terms"],
        serde_json::json!(["faute", "dommage"])
    );
}

#[test]
fn expand_rejects_empty_query_in_cli_and_session() {
    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env_remove("JURISEARCH_INDEX_DIR")
        .args(["expand", "   "])
        .assert()
        .code(2)
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["code"], "bad_input");
    assert_eq!(json["error"]["message"], "expand query must not be empty");

    let input = concat!(
        "{\"id\":\"bad-expand\",\"command\":\"expand\",\"args\":{\"query\":\"\"}}\n",
        "{\"id\":\"done\",\"command\":\"exit\"}\n",
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
    assert_eq!(values.len(), 2);
    assert_eq!(values[0]["id"], "bad-expand");
    assert_eq!(values[0]["ok"], false);
    assert_eq!(values[0]["error"]["code"], "bad_input");
    assert_eq!(
        values[0]["error"]["message"],
        "expand query must not be empty"
    );
    assert_eq!(values[1]["result"]["bye"], true);
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
                (chunk_id, document_id, chunk_index, body, contextualized_body, source_payload_hash, \
                 chunk_builder_version, embedding_fingerprint) \
             VALUES \
                ('chunk:1240:0', 'legi:LEGIARTI000006419320@1804-02-21', 0, \
                 'responsabilite civile faute reparation dommage article 1240', \
                 'Code civil > Article 1240\nresponsabilite civile faute reparation dommage article 1240', \
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

    // `fetch --as-of` was removed (T0.4): fetch is exact, version-pinned retrieval; date-resolved
    // fetch is deferred (alongside `versions`/`diff`). A now-unknown `--as-of` is a clap usage error.
    // `--part` was later re-introduced as a real decision feature (see
    // `fetch_part_extracts_decision_parts_with_honest_provenance`).
    Ok(())
}

#[test]
fn cite_resolves_local_statutory_citations_and_strict_states() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("CLI cite existing index")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-cli-cite.")
        .tempdir()
        .map_err(StorageError::Io)?;

    {
        let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;
        postgres.execute_sql(
            r#"INSERT INTO documents
                (document_id, source, kind, source_uid, citation, title, body,
                 valid_from, valid_to, valid_to_raw, source_payload_hash, canonical_json)
             VALUES
                ('legi:LEGIARTI000006419320@1804-02-21', 'legi', 'article',
                 'LEGIARTI000006419320', 'Code civil article 1240',
                 'Article 1240', 'Responsabilite civile courante.',
                 '1804-02-21', NULL, '2999-01-01', 'sha256:civil-1240',
                 '{"official":true}'),
                ('legi:LEGIARTI000000001240@2020-01-01', 'legi', 'article',
                 'LEGIARTI000000001240', 'Code des assurances article 1240',
                 'Article 1240', 'Autre article courant avec le meme numero.',
                 '2020-01-01', NULL, '2999-01-01', 'sha256:assurance-1240',
                 '{"official":true}'),
                ('legi:LEGIARTI000000121001@2020-01-01', 'legi', 'article',
                 'LEGIARTI000000121001', 'Code de la consommation article L121-1',
                 'Article L121-1', 'Article prefixe courant.',
                 '2020-01-01', NULL, '2999-01-01', 'sha256:conso-l121-1',
                 '{"official":true}'),
                ('legi:LEGIARTI000000000888@1900-01-01', 'legi', 'article',
                 'LEGIARTI000000000888', 'Code civil article 88',
                 'Article 88', 'Ancienne version historique.',
                 '1900-01-01', '2000-01-01', '2000-01-01', 'sha256:article-88-old',
                 '{"official":true}'),
                ('legi:LEGIARTI000000000888@2000-01-01', 'legi', 'article',
                 'LEGIARTI000000000888', 'Code civil article 88',
                 'Article 88', 'Version courante.',
                 '2000-01-01', NULL, '2999-01-01', 'sha256:article-88-current',
                 '{"official":true}'),
                ('legi:LEGIARTI000000000777@1900-01-01', 'legi', 'article',
                 'LEGIARTI000000000777', 'Code civil article 777',
                 'Article 777', 'Version abrogee.',
                 '1900-01-01', '2000-01-01', '2000-01-01', 'sha256:article-777-old',
                 '{"official":true}');
             INSERT INTO chunks
                (chunk_id, document_id, chunk_index, body, contextualized_body, source_payload_hash,
                 chunk_builder_version, embedding_fingerprint)
             VALUES
                ('chunk:civil-1240:0', 'legi:LEGIARTI000006419320@1804-02-21', 0,
                 'Responsabilite civile courante.', 'Code civil > Article 1240',
                 'sha256:civil-1240', 'chunker:v0', NULL),
                ('chunk:assurance-1240:0', 'legi:LEGIARTI000000001240@2020-01-01', 0,
                 'Autre article courant avec le meme numero.', 'Code des assurances > Article 1240',
                 'sha256:assurance-1240', 'chunker:v0', NULL),
                ('chunk:conso-l121-1:0', 'legi:LEGIARTI000000121001@2020-01-01', 0,
                 'Article prefixe courant.', 'Code de la consommation > Article L121-1',
                 'sha256:conso-l121-1', 'chunker:v0', NULL),
                ('chunk:article-88-old:0', 'legi:LEGIARTI000000000888@1900-01-01', 0,
                 'Ancienne version historique.', 'Code civil > Article 88',
                 'sha256:article-88-old', 'chunker:v0', NULL),
                ('chunk:article-88-current:0', 'legi:LEGIARTI000000000888@2000-01-01', 0,
                 'Version courante.', 'Code civil > Article 88',
                 'sha256:article-88-current', 'chunker:v0', NULL),
                ('chunk:article-777-old:0', 'legi:LEGIARTI000000000777@1900-01-01', 0,
                 'Version abrogee.', 'Code civil > Article 777',
                 'sha256:article-777-old', 'chunker:v0', NULL);
             INSERT INTO legi_metadata_roots
                (metadata_key, root_kind, source_uid, parent_source_uid, title,
                 valid_from, valid_to, valid_to_raw, source_payload_hash, canonical_version, canonical_json)
             VALUES
                ('legi:SECTION_TA:LEGISCTA000006089696@1804-03-21', 'SECTION_TA',
                 'LEGISCTA000006089696', 'LEGITEXT000006070721', 'Titre preliminaire',
                 '1804-03-21', NULL, '2999-01-01', 'sha256:section',
                 'legi_section_ta:v1', '{"title":"Titre preliminaire"}'),
                ('legi:TEXTELR:LEGITEXT000006070721@1804-03-21:nor', 'TEXTELR',
                 'LEGITEXT000006070721', NULL, NULL,
                 '1804-03-21', NULL, NULL, 'sha256:textelr',
                 'legi_textelr:v1', '{"text_id":"LEGITEXT000006070721","nor":"JUSC2301234L"}');"#,
        )?;
    }

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .args(["cite", "LEGIARTI000006419320", "--as-of", "2024-01-01"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["state"], "exact");
    assert_eq!(json["input_class"], "legiarti");
    assert_eq!(json["valid_match_count"], 1);
    assert_eq!(
        json["matches"][0]["document_id"],
        "legi:LEGIARTI000006419320@1804-02-21"
    );

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .args(["cite", "Code civil article 1240", "--as-of", "2024-01-01"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["state"], "normalized");
    assert_eq!(json["input_class"], "free_text_article");
    assert_eq!(json["match_count"], 1);

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .args([
            "cite",
            "Code de la consommation article L. 121-1",
            "--as-of",
            "2024-01-01",
        ])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["state"], "normalized");
    assert_eq!(json["normalized"], "l121-1");
    assert_eq!(
        json["matches"][0]["document_id"],
        "legi:LEGIARTI000000121001@2020-01-01"
    );

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .args([
            "cite",
            "Code des assurances article 1240",
            "--as-of",
            "2024-01-01",
        ])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["state"], "normalized");
    assert_eq!(
        json["matches"][0]["document_id"],
        "legi:LEGIARTI000000001240@2020-01-01"
    );

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .args(["cite", "article 1240", "--as-of", "2024-01-01"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["state"], "ambiguous");
    assert_eq!(json["valid_match_count"], 2);

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .args(["cite", "LEGIARTI000000000888", "--as-of", "1999-01-01"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["state"], "exact");
    assert!(json["matches"].as_array().unwrap().iter().any(|candidate| {
        candidate["document_id"] == "legi:LEGIARTI000000000888@1900-01-01"
            && candidate["valid_on_as_of"] == true
    }));

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .args(["cite", "LEGIARTI000000000777", "--as-of", "2024-01-01"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["state"], "stale_version");
    assert_eq!(json["valid_match_count"], 0);

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .args(["cite", "JUSC2301234L", "--as-of", "2024-01-01"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["state"], "exact");
    assert_eq!(json["input_class"], "nor");
    assert_eq!(
        json["matches"][0]["metadata_key"],
        "legi:TEXTELR:LEGITEXT000006070721@1804-03-21:nor"
    );

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env_remove("JURISEARCH_INDEX_DIR")
        .args(["cite", "not a citation"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["state"], "not_found");
    assert_eq!(json["input_class"], "malformed");

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env_remove("JURISEARCH_INDEX_DIR")
        .env_remove("JURISEARCH_PISTE_LEGIFRANCE_CLIENT_ID")
        .env_remove("JURISEARCH_PISTE_LEGIFRANCE_CLIENT_SECRET")
        .args(["cite", "not a citation", "--online"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["state"], "not_found");
    assert_eq!(json["online"]["requested"], true);
    assert_eq!(json["online"]["checked"], false);
    assert!(
        json["online"]["note"]
            .as_str()
            .is_some_and(|note| note.contains("not sent"))
    );

    let online_base_url = spawn_server(2, |request| {
        if request.starts_with("POST /api/oauth/token ") {
            assert!(request.contains("grant_type=client_credentials"));
            assert!(request.contains("scope=openid"));
            ok_json(r#"{"access_token":"token-123","expires_in":3600}"#)
        } else {
            assert!(request.starts_with("POST /dila/legifrance/lf-engine-app/search "));
            assert!(request.contains("\r\nAuthorization: Bearer token-123\r\n"));
            // cite --online now shares the real-contract body builder: the query rides in the
            // recherche champ `valeur` with `fond=CODE_DATE`, NOT the old HTTP-500 `{"query":…}` shape.
            assert!(request.contains(r#""valeur":"LEGIARTI999999999999""#));
            assert!(request.contains(r#""fond":"CODE_DATE""#));
            assert!(
                !request.contains(r#""query":"LEGIARTI999999999999""#),
                "the old top-level {{query,pageSize}} body must not reappear"
            );
            ok_json(r#"{"results":[]}"#)
        }
    });
    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .env("JURISEARCH_PISTE_ENV", "sandbox")
        .env("JURISEARCH_PISTE_API_BASE_URL", &online_base_url)
        .env("JURISEARCH_PISTE_OAUTH_BASE_URL", &online_base_url)
        .env("JURISEARCH_PISTE_LEGIFRANCE_CLIENT_ID", "client-id")
        .env("JURISEARCH_PISTE_LEGIFRANCE_CLIENT_SECRET", "client-secret")
        .args(["cite", "LEGIARTI999999999999", "--online"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["state"], "source_unavailable");
    assert_eq!(json["local_state"], "not_found");
    assert_eq!(json["online"]["requested"], true);
    assert_eq!(json["online"]["checked"], true);
    assert_eq!(json["online"]["provider"], "legifrance");

    let failing_online_base_url = spawn_server(2, |request| {
        if request.starts_with("POST /api/oauth/token ") {
            ok_json(r#"{"access_token":"token-500","expires_in":3600}"#)
        } else {
            "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 11\r\n\r\nserver down".to_owned()
        }
    });
    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .env("JURISEARCH_PISTE_ENV", "sandbox")
        .env("JURISEARCH_PISTE_API_BASE_URL", &failing_online_base_url)
        .env("JURISEARCH_PISTE_OAUTH_BASE_URL", &failing_online_base_url)
        .env("JURISEARCH_PISTE_LEGIFRANCE_CLIENT_ID", "client-id")
        .env("JURISEARCH_PISTE_LEGIFRANCE_CLIENT_SECRET", "client-secret")
        // Probe failure must map fast: disable upstream retries so the 500 mock needs one request.
        .env("JURISEARCH_PISTE_MAX_RETRIES", "0")
        .args([
            "cite",
            "Code civil article 1240",
            "--as-of",
            "2024-01-01",
            "--online",
        ])
        .assert()
        .code(5)
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    assert_json_error_contains(&output, "upstream", "official API returned HTTP status 500");

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .args(["cite", "article 1240", "--as-of", "2024-01-01", "--strict"])
        .assert()
        .code(2)
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    assert_json_error_contains(&output, "no_results", "ambiguous");

    let input = format!(
        "{{\"id\":\"cite-one\",\"command\":\"cite\",\"args\":{{\"cite\":\"Code civil article 1240\",\"as_of\":\"2024-01-01\",\"index_dir\":\"{}\"}}}}\n",
        root.path().display()
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
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["id"], "cite-one");
    assert_eq!(json["ok"], true);
    assert_eq!(json["result"]["state"], "normalized");

    Ok(())
}

#[test]
fn cite_free_text_matches_ingested_legi_article_citation() -> Result<(), Box<dyn std::error::Error>>
{
    let Some(_pg_config) = discover_pg_config("CLI cite ingested LEGI article")? else {
        return Ok(());
    };
    let index = tempfile::Builder::new()
        .prefix("jurisearch-cli-cite-ingested.")
        .tempdir()?;
    let archives = tempfile::Builder::new()
        .prefix("jurisearch-cli-cite-archives.")
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
        .args(["--run-id", "run-cite-ingested"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["run_status"], "completed");
    assert_eq!(json["inserted_documents"], 1);

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .arg("--index-dir")
        .arg(index.path())
        .args(["cite", "Code civil article 1240", "--as-of", "2024-01-01"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["state"], "normalized");
    assert_eq!(json["match_count"], 1);
    assert_eq!(
        json["matches"][0]["document_id"],
        "legi:LEGIARTI000006419320@1804-02-21"
    );

    Ok(())
}

#[test]
fn cite_resolves_decision_identifiers() -> Result<(), Box<dyn std::error::Error>> {
    let Some(_pg_config) = discover_pg_config("CLI cite decision")? else {
        return Ok(());
    };
    let index = tempfile::Builder::new()
        .prefix("jurisearch-cli-cite-decision.")
        .tempdir()?;
    let archives = tempfile::Builder::new()
        .prefix("jurisearch-cli-cite-decision-archives.")
        .tempdir()?;
    let archive_path = archives
        .path()
        .join("Freemium_cass_global_20250101-000000.tar.gz");
    write_tar_gz(
        archive_path.as_path(),
        &[(
            "juri/cass/JURITEXT000051824029.xml",
            cass_decision_fixture("JURITEXT000051824029", "23-14999").as_slice(),
        )],
    )?;

    let ingest = Command::cargo_bin("jurisearch")
        .unwrap()
        .arg("--index-dir")
        .arg(index.path())
        .args(["ingest", "juri-archives", "--source", "cass", "--archives-dir"])
        .arg(archives.path())
        .args(["--run-id", "run-cite-decision"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let ingest: Value = serde_json::from_slice(&ingest)?;
    assert_eq!(ingest["run_status"], "completed");
    assert_eq!(ingest["inserted_documents"], 1);

    let cite = |target: &str| -> Value {
        let output = Command::cargo_bin("jurisearch")
            .unwrap()
            .arg("--index-dir")
            .arg(index.path())
            .args(["cite", target])
            .assert()
            .success()
            .stderr(predicate::str::is_empty())
            .get_output()
            .stdout
            .clone();
        serde_json::from_slice(&output).unwrap()
    };

    // Source-native UID.
    let by_uid = cite("JURITEXT000051824029");
    assert_eq!(by_uid["input_class"], "decision_id");
    assert_eq!(by_uid["state"], "exact");
    assert_eq!(by_uid["match_count"], 1);
    assert_eq!(by_uid["matches"][0]["document_id"], "cass:JURITEXT000051824029");
    assert_eq!(by_uid["matches"][0]["kind"], "decision");

    // ECLI (case-insensitive).
    let by_ecli = cite("ecli:fr:ccass:2025:so00111");
    assert_eq!(by_ecli["input_class"], "ecli");
    assert_eq!(by_ecli["state"], "exact");
    assert_eq!(by_ecli["matches"][0]["document_id"], "cass:JURITEXT000051824029");

    // Pourvoi / numéro d'affaire (dotted input normalizes to the stored 23-14999).
    let by_pourvoi = cite("23-14.999");
    assert_eq!(by_pourvoi["input_class"], "pourvoi");
    assert_eq!(by_pourvoi["state"], "normalized");
    assert_eq!(by_pourvoi["matches"][0]["document_id"], "cass:JURITEXT000051824029");

    // Decision document_id.
    let by_doc = cite("cass:JURITEXT000051824029");
    assert_eq!(by_doc["input_class"], "decision_document_id");
    assert_eq!(by_doc["state"], "exact");

    // Unknown decision -> not_found.
    let missing = cite("JURITEXT000000000000");
    assert_eq!(missing["input_class"], "decision_id");
    assert_eq!(missing["state"], "not_found");
    assert_eq!(missing["match_count"], 0);

    // --as-of BEFORE the decision date must NOT make an existing decision "stale_version":
    // decisions are dated, not versioned. Existence-based -> exact, and --strict succeeds.
    let as_of = Command::cargo_bin("jurisearch")
        .unwrap()
        .arg("--index-dir")
        .arg(index.path())
        .args(["cite", "cass:JURITEXT000051824029", "--as-of", "2024-01-01", "--strict"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let as_of: Value = serde_json::from_slice(&as_of)?;
    assert_eq!(as_of["state"], "exact");

    // --online for a decision must NOT probe the Légifrance statutory endpoint; it is an honest
    // no-op note (Judilibre verification not wired) and the local state is preserved.
    let online = Command::cargo_bin("jurisearch")
        .unwrap()
        .arg("--index-dir")
        .arg(index.path())
        .args(["cite", "JURITEXT000051824029", "--online"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let online: Value = serde_json::from_slice(&online)?;
    assert_eq!(online["state"], "exact");
    assert_eq!(online["online"]["checked"], false);
    assert_eq!(online["online"]["provider"], "judilibre");

    // --online for a MISSING decision stays not_found (not source_unavailable).
    let online_missing = Command::cargo_bin("jurisearch")
        .unwrap()
        .arg("--index-dir")
        .arg(index.path())
        .args(["cite", "JURITEXT000000000000", "--online"])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let online_missing: Value = serde_json::from_slice(&online_missing)?;
    assert_eq!(online_missing["state"], "not_found");

    Ok(())
}

#[test]
fn fetch_part_extracts_decision_parts_with_honest_provenance()
-> Result<(), Box<dyn std::error::Error>> {
    let Some(_pg_config) = discover_pg_config("CLI fetch --part")? else {
        return Ok(());
    };
    let index = tempfile::Builder::new()
        .prefix("jurisearch-cli-fetch-part.")
        .tempdir()?;
    let archives = tempfile::Builder::new()
        .prefix("jurisearch-cli-fetch-part-archives.")
        .tempdir()?;
    let archive_path = archives
        .path()
        .join("Freemium_cass_global_20250101-000000.tar.gz");
    let decision_xml = br#"<?xml version="1.0" encoding="UTF-8"?>
<TEXTE_JURI_JUDI>
<META><META_COMMUN><ID>JURITEXT000051824099</ID><ANCIEN_ID/><ORIGINE>JURI</ORIGINE>
<URL>texte/juri/judi/JURI/TEXT/.../JURITEXT000051824099.xml</URL><NATURE>ARRET</NATURE>
</META_COMMUN><META_SPEC><META_JURI>
<TITRE>Cour de cassation, chambre civile 1, 4 juin 2025</TITRE>
<DATE_DEC>2025-06-04</DATE_DEC><JURIDICTION>Cour de cassation</JURIDICTION>
<NUMERO>P2500222</NUMERO><SOLUTION>Rejet</SOLUTION>
</META_JURI><META_JURI_JUDI>
<NUMEROS_AFFAIRES><NUMERO_AFFAIRE>23-15000</NUMERO_AFFAIRE></NUMEROS_AFFAIRES>
<PUBLI_BULL publie="oui"/><FORMATION>CHAMBRE_CIVILE_1</FORMATION>
<ECLI>ECLI:FR:CCASS:2025:C100222</ECLI>
</META_JURI_JUDI></META_SPEC></META>
<TEXTE><BLOC_TEXTUEL><CONTENU>Vu les articles 1240 et 1241 du code civil ;<br/>
Faits et procedure<br/>
1. Selon l'arret attaque, le demandeur a saisi la juridiction.<br/>
PAR CES MOTIFS, la Cour REJETTE le pourvoi.</CONTENU></BLOC_TEXTUEL>
<SOMMAIRE><SCT ID="1" TYPE="PRINCIPAL">RESPONSABILITE - faute</SCT><ANA ID="1">La faute engage la responsabilite de son auteur.</ANA></SOMMAIRE>
<CITATION_JP/></TEXTE>
<LIENS/>
</TEXTE_JURI_JUDI>"#;
    write_tar_gz(
        archive_path.as_path(),
        &[("juri/cass/JURITEXT000051824099.xml", decision_xml)],
    )?;

    Command::cargo_bin("jurisearch")
        .unwrap()
        .arg("--index-dir")
        .arg(index.path())
        .args(["ingest", "juri-archives", "--source", "cass", "--archives-dir"])
        .arg(archives.path())
        .args(["--run-id", "run-fetch-part"])
        .assert()
        .success();

    let fetch_part = |part: &str| -> Value {
        let output = Command::cargo_bin("jurisearch")
            .unwrap()
            .arg("--index-dir")
            .arg(index.path())
            .args(["fetch", "cass:JURITEXT000051824099", "--part", part])
            .assert()
            .success()
            .stderr(predicate::str::is_empty())
            .get_output()
            .stdout
            .clone();
        serde_json::from_slice(&output).unwrap()
    };

    // Summary comes from the SOMMAIRE (not an official zone).
    let summary = fetch_part("summary");
    let part = &summary["documents"][0]["part"];
    assert_eq!(part["applicable"], true);
    assert_eq!(part["official_zones"], false);
    assert_eq!(part["zone_provenance"], "sommaire");
    assert_eq!(part["available"], true);
    assert!(part["text"].as_str().unwrap().contains("responsabilite"));

    // Dispositif is a heuristic extraction from the "PAR CES MOTIFS" marker.
    let dispositif = fetch_part("dispositif");
    let part = &dispositif["documents"][0]["part"];
    assert_eq!(part["zone_provenance"], "heuristic");
    assert_eq!(part["available"], true);
    assert!(part["text"].as_str().unwrap().contains("REJETTE"));

    // Motivations have no bulk marker -> honestly unavailable (needs Judilibre zones).
    let motivations = fetch_part("motivations");
    let part = &motivations["documents"][0]["part"];
    assert_eq!(part["zone_provenance"], "unavailable");
    assert_eq!(part["available"], false);

    // Visa is heuristic from leading "Vu …" lines.
    let visa = fetch_part("visa");
    let part = &visa["documents"][0]["part"];
    assert_eq!(part["zone_provenance"], "heuristic");
    assert_eq!(part["available"], true);
    assert!(part["text"].as_str().unwrap().contains("1240"));

    // Moyens has no bulk marker -> unavailable.
    let moyens = fetch_part("moyens");
    assert_eq!(moyens["documents"][0]["part"]["zone_provenance"], "unavailable");

    // Unknown part -> bad_input.
    Command::cargo_bin("jurisearch")
        .unwrap()
        .arg("--index-dir")
        .arg(index.path())
        .args(["fetch", "cass:JURITEXT000051824099", "--part", "bogus"])
        .assert()
        .code(2);

    // JSONL session forwards `part` (the per-request index_dir carries the index path).
    let request = serde_json::json!({
        "command": "fetch",
        "args": {
            "ids": ["cass:JURITEXT000051824099"],
            "part": "summary",
            "index_dir": index.path().to_string_lossy(),
        }
    });
    let session_out = Command::cargo_bin("jurisearch")
        .unwrap()
        .arg("session")
        .arg("--jsonl")
        .write_stdin(format!("{request}\n"))
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let line = String::from_utf8(session_out)?;
    let session: Value = serde_json::from_str(line.lines().next().unwrap())?;
    assert_eq!(
        session["result"]["documents"][0]["part"]["zone_provenance"],
        "sommaire"
    );

    Ok(())
}

#[test]
fn context_returns_hierarchy_and_siblings_from_existing_index() -> Result<(), StorageError> {
    let Some(pg_config) = discover_pg_config("CLI context existing index")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-cli-context.")
        .tempdir()
        .map_err(StorageError::Io)?;
    let document_id = "legi:LEGIARTI000006419320@1804-02-21";

    {
        let postgres =
            jurisearch_storage::runtime::ManagedPostgres::start_durable(pg_config, root.path())?;
        postgres.execute_sql(
            "INSERT INTO documents \
                (document_id, source, kind, source_uid, citation, title, body, \
                 valid_from, valid_to, source_payload_hash, hierarchy_path, canonical_json) \
             VALUES \
                ('legi:LEGIARTI000006419320@1804-02-21', 'legi', 'article', \
                 'LEGIARTI000006419320', 'Code civil article 1240', \
                 'Article 1240', 'Responsabilite civile.', '1804-02-21', NULL, \
                 'sha256:article-1240', \
                 '[\"Code civil\",\"Livre III\",\"Titre IV\"]'::jsonb, \
                 '{\"hierarchy_path\":[\"Code civil\",\"Livre III\",\"Titre IV\"]}'), \
                ('legi:LEGIARTI000006419321@1804-02-21', 'legi', 'article', \
                 'LEGIARTI000006419321', 'Code civil article 1241', \
                 'Article 1241', 'Responsabilite voisine.', '1804-02-21', NULL, \
                 'sha256:article-1241', \
                 '[\"Code civil\",\"Livre III\",\"Titre IV\"]'::jsonb, \
                 '{\"hierarchy_path\":[\"Code civil\",\"Livre III\",\"Titre IV\"]}'), \
                ('legi:LEGIARTI000006419322@2025-01-01', 'legi', 'article', \
                 'LEGIARTI000006419322', 'Code civil article futur', \
                 'Article futur', 'Future section article.', '2025-01-01', NULL, \
                 'sha256:article-future', \
                 '[\"Code civil\",\"Livre III\",\"Titre IV\"]'::jsonb, \
                 '{\"hierarchy_path\":[\"Code civil\",\"Livre III\",\"Titre IV\"]}'); \
             INSERT INTO chunks \
                (chunk_id, document_id, chunk_index, body, contextualized_body, chunking, \
                 boundary, hierarchy_path, source_payload_hash, chunk_builder_version) \
             VALUES \
                ('chunk:1240:0', 'legi:LEGIARTI000006419320@1804-02-21', 0, \
                 'Responsabilite civile.', 'Code civil > Livre III > Titre IV > Article 1240', \
                 'structural', 'article', '[\"Code civil\",\"Livre III\",\"Titre IV\"]', \
                 'sha256:article-1240', 'chunker:v1'), \
                ('chunk:1241:0', 'legi:LEGIARTI000006419321@1804-02-21', 0, \
                 'Responsabilite voisine.', 'Code civil > Livre III > Titre IV > Article 1241', \
                 'structural', 'article', '[\"Code civil\",\"Livre III\",\"Titre IV\"]', \
                 'sha256:article-1241', 'chunker:v1'), \
                ('chunk:future:0', 'legi:LEGIARTI000006419322@2025-01-01', 0, \
                 'Future section article.', 'Code civil > Livre III > Titre IV > Article futur', \
                 'structural', 'article', '[\"Code civil\",\"Livre III\",\"Titre IV\"]', \
                 'sha256:article-future', 'chunker:v1');",
        )?;
    }

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .args([
            "context",
            document_id,
            "--siblings",
            "--as-of",
            "2024-01-01",
        ])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["target"]["document_id"], document_id);
    assert_eq!(json["ancestry"][0]["title"], "Code civil");
    assert_eq!(json["sibling_count"], 1);
    assert_eq!(json["sibling_limit"], 50);
    assert_eq!(json["sibling_truncated"], false);
    assert_eq!(
        json["siblings"][0]["document_id"],
        "legi:LEGIARTI000006419321@1804-02-21"
    );

    let input = format!(
        "{{\"id\":\"context-one\",\"command\":\"context\",\"args\":{{\"id\":\"{document_id}\",\"siblings\":true,\"as_of\":\"2024-01-01\"}}}}\n"
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
    assert_eq!(json["id"], "context-one");
    assert_eq!(json["ok"], true);
    assert_eq!(json["result"]["target"]["document_id"], document_id);

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env_remove("JURISEARCH_INDEX_DIR")
        .args(["context", document_id, "--as-of", "20240101"])
        .assert()
        .code(2)
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();
    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["code"], "bad_input");

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env_remove("JURISEARCH_INDEX_DIR")
        .args(["context", document_id, "--as-of", "2024-13-01"])
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
                (chunk_id, document_id, chunk_index, body, contextualized_body, source_payload_hash, \
                 chunk_builder_version, embedding_fingerprint) \
             VALUES \
                ('chunk:1240:0', '{document_id}', 0, \
                 'responsabilite civile faute reparation dommage article 1240', \
                 'Code civil > Article 1240\nresponsabilite civile faute reparation dommage article 1240', \
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
        .env("JURISEARCH_EMBED_BASE_URL", &embedding_base_url)
        .args(["search", query, "--as-of", "2024-01-01", "--top-k", "3"])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["retrieval_mode"], "hybrid");
    assert_eq!(json["candidates"][0]["document_id"], document_id);
    assert_eq!(json["candidates"][0]["scores"]["dense_rank"], 1);

    let output = Command::cargo_bin("jurisearch")
        .unwrap()
        .env("JURISEARCH_INDEX_DIR", root.path())
        .env("JURISEARCH_EMBED_BASE_URL", embedding_base_url)
        .args([
            "search",
            query,
            "--as-of",
            "2024-01-01",
            "--top-k",
            "3",
            "--mode",
            "dense",
        ])
        .assert()
        .success()
        .stderr(predicate::str::is_empty())
        .get_output()
        .stdout
        .clone();

    let json: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(json["retrieval_mode"], "dense");
    assert_eq!(json["candidates"][0]["document_id"], document_id);
    assert!(json["candidates"][0]["scores"]["lexical_rank"].is_null());
    assert_eq!(json["candidates"][0]["scores"]["dense_rank"], 1);
    Ok(())
}
