//! Shared fixtures and helpers for the CLI contract test suites. Each domain test binary
//! pulls these in via `mod support; use support::*;` — which also re-exports the common
//! test-facing imports (Command, Value, fs, predicate, …) so the suites need no import block
//! of their own. The allows cover the per-binary subset: each suite uses only some of the
//! re-exported imports and helpers.
#![allow(dead_code, unused_imports)]

pub use std::{
    fs::{self, File},
    io::{Cursor, Read, Write},
    net::TcpListener,
    path::Path,
    thread,
    time::Duration,
};

pub use assert_cmd::Command;
pub use flate2::{Compression, write::GzEncoder};
pub use jurisearch_embed::{EmbeddingConfig, OpenAiCompatibleClient};
pub use jurisearch_storage::{
    ingest_accounting::{
        IngestCompatibility, IngestMemberInput, IngestMemberStatus, IngestRunInput,
        finish_ingest_run, record_ingest_member, start_ingest_run,
    },
    runtime::{ManagedPostgres, PgConfig, StorageError},
};
pub use predicates::prelude::*;
pub use serde_json::Value;
pub use tar::{Builder, Header};

pub(crate) fn jurisearch_command_without_embedding_env() -> Command {
    let mut command = Command::cargo_bin("jurisearch").unwrap();
    for name in [
        "JURISEARCH_CONFIG",
        "XDG_CONFIG_HOME",
        "JURISEARCH_EMBED_PROVIDER",
        "JURISEARCH_EMBED_BASE_URL",
        "JURISEARCH_EMBED_BASE_URLS",
        "JURISEARCH_EMBED_POOL",
        "JURISEARCH_EMBED_API_KEY",
        "JURISEARCH_EMBED_MODEL",
        "JURISEARCH_EMBED_DIMENSION",
        "JURISEARCH_EMBED_NORMALIZE",
        "JURISEARCH_EMBED_POOLING",
        "JURISEARCH_EMBED_MAX_INPUT_CHARS",
        "JURISEARCH_EMBED_MAX_ESTIMATED_TOKENS",
        "JURISEARCH_EMBED_ESTIMATED_CHARS_PER_TOKEN",
        "JURISEARCH_EMBED_TOKENIZER_JSON",
        "JURISEARCH_PHASE1_EXTERNAL_BENCHMARK",
        "JURISEARCH_PHASE1_FRANCE_LEGI_BENCHMARK",
        "JURISEARCH_MODEL_DIR",
        "OPENROUTER_API_KEY",
        "XDG_CACHE_HOME",
    ] {
        command.env_remove(name);
    }
    command
}

pub(crate) fn cass_decision_fixture(uid: &str, num_affaire: &str) -> Vec<u8> {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<TEXTE_JURI_JUDI>
<META><META_COMMUN><ID>{uid}</ID><ANCIEN_ID/><ORIGINE>JURI</ORIGINE>
<URL>texte/juri/judi/JURI/TEXT/.../{uid}.xml</URL><NATURE>ARRET</NATURE>
</META_COMMUN><META_SPEC><META_JURI>
<TITRE>Cour de cassation, chambre sociale, 4 juin 2025, {num_affaire}</TITRE>
<DATE_DEC>2025-06-04</DATE_DEC><JURIDICTION>Cour de cassation</JURIDICTION>
<NUMERO>P2500111</NUMERO><SOLUTION>Cassation</SOLUTION>
</META_JURI><META_JURI_JUDI>
<NUMEROS_AFFAIRES><NUMERO_AFFAIRE>{num_affaire}</NUMERO_AFFAIRE></NUMEROS_AFFAIRES>
<PUBLI_BULL publie="oui"/><FORMATION>CHAMBRE_SOCIALE</FORMATION>
<ECLI>ECLI:FR:CCASS:2025:SO00111</ECLI>
</META_JURI_JUDI></META_SPEC></META>
<TEXTE><BLOC_TEXTUEL><CONTENU>La clause de non-concurrence est nulle faute de contrepartie financiere. La Cour casse l'arret attaque concernant M. [B].</CONTENU></BLOC_TEXTUEL>
<SOMMAIRE/><CITATION_JP/></TEXTE>
<LIENS><LIEN id="LEGIARTI000006900782" cidtexte="LEGITEXT000006072050" sens="cible" typelien="CITATION" num="L1121-1" naturetexte="" nortexte="" numtexte="" datesignatexte="">Article L1121-1 du code du travail</LIEN></LIENS>
</TEXTE_JURI_JUDI>"#
    )
    .into_bytes()
}

pub(crate) fn discover_pg_config(test_name: &str) -> Result<Option<PgConfig>, StorageError> {
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

pub(crate) fn assert_json_error_contains(output: &[u8], code: &str, message: &str) {
    let json: Value = serde_json::from_slice(output).unwrap();
    assert_eq!(json["ok"], false);
    assert_eq!(json["error"]["code"], code);
    assert!(json["error"]["message"].as_str().unwrap().contains(message));
}

pub(crate) fn write_tar_gz(path: &Path, members: &[(&str, &[u8])]) -> Result<(), Box<dyn std::error::Error>> {
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

pub(crate) fn article_fixture() -> String {
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

pub(crate) fn article_fixture_without_body() -> String {
    article_fixture().replace(
        r#"  <BLOC_TEXTUEL>
    <CONTENU>
      <p>Tout fait quelconque de l'homme, qui cause a autrui un dommage, oblige celui par la faute duquel il est arrive a le reparer.</p>
    </CONTENU>
  </BLOC_TEXTUEL>
"#,
        "",
    )
}

pub(crate) fn pgvector_literal(values: &[f32]) -> String {
    let values = values
        .iter()
        .map(|value| format!("{value:.8}"))
        .collect::<Vec<_>>()
        .join(",");
    format!("[{values}]")
}

pub(crate) fn unit_vector_literal(active_index: usize) -> String {
    let values = (0..1024)
        .map(|index| if index == active_index { 1.0 } else { 0.0 })
        .collect::<Vec<_>>();
    pgvector_literal(&values)
}

pub(crate) fn embedding_response_json(active_index: usize) -> String {
    let values = (0..1024)
        .map(|index| {
            if index == active_index {
                "1.0".to_owned()
            } else {
                "0.0".to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(r#"{{"data":[{{"embedding":[{values}]}}]}}"#)
}

pub(crate) fn spawn_server(
    request_count: usize,
    mut handler: impl FnMut(String) -> String + Send + 'static,
) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    thread::spawn(move || {
        for _ in 0..request_count {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_http_request(&mut stream);
            let response = handler(request);
            stream.write_all(response.as_bytes()).unwrap();
        }
    });
    format!("http://{address}")
}

pub(crate) fn read_http_request(stream: &mut std::net::TcpStream) -> String {
    let mut bytes = Vec::new();
    let mut buffer = [0; 4096];
    loop {
        let read = stream.read(&mut buffer).unwrap();
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..read]);
        if request_is_complete(&bytes) {
            break;
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

pub(crate) fn request_is_complete(bytes: &[u8]) -> bool {
    let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
        return false;
    };
    let headers = String::from_utf8_lossy(&bytes[..header_end]);
    let content_length = headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        if name.eq_ignore_ascii_case("Content-Length") {
            value.trim().parse::<usize>().ok()
        } else {
            None
        }
    });
    let Some(content_length) = content_length else {
        return true;
    };
    bytes.len() >= header_end + 4 + content_length
}

pub(crate) fn ok_json(body: &str) -> String {
    format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
        body.len(),
        body
    )
}
