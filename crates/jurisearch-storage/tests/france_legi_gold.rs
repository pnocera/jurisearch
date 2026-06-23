//! Managed-PG test for France-LEGI gold qrel extraction over synthetic LEGI rows.
//!
//! Run with:
//! `cargo test -p jurisearch-storage --test france_legi_gold -- --ignored --nocapture`.

mod common;

use std::error::Error;

use common::discover_pg_config;
use jurisearch_storage::{
    france_legi::{FranceLegiGoldLimits, france_legi_gold_json},
    runtime::ManagedPostgres,
};
use serde_json::Value;

const SEED_SQL: &str = r#"
INSERT INTO documents (document_id, source, kind, source_uid, citation, body, valid_from, valid_to, source_payload_hash) VALUES
 ('legi:LEGIARTI000000000A01@2020-01-01','legi','article','LEGIARTI000000000A01','Code test Article A','Body A','2020-01-01',NULL,'sha256:a'),
 ('legi:LEGIARTI000000000B01@2020-01-01','legi','article','LEGIARTI000000000B01','Code test Article B','Body B','2020-01-01',NULL,'sha256:b'),
 ('legi:LEGIARTI000000000V01@1990-01-01','legi','article','LEGIARTI000000000V01','Code rural Article R242-40','Body V1','1990-01-01','2003-01-01','sha256:v1'),
 ('legi:LEGIARTI000000000V02@2003-01-01','legi','article','LEGIARTI000000000V02','Code rural Article R242-40','Body V2','2003-01-01',NULL,'sha256:v2'),
 ('legi:LEGIARTI000000000W01@2020-01-01','legi','article','LEGIARTI000000000W01','Code test Article W','Body W','2020-01-01',NULL,'sha256:w');

INSERT INTO chunks (chunk_id, document_id, chunk_index, body, source_payload_hash, chunk_builder_version, contextualized_body) VALUES
 ('chunk:A','legi:LEGIARTI000000000A01@2020-01-01',0,'Body A','sha256:a','test:v1','Code test > Article A. This article A body refers to article B.'),
 ('chunk:B','legi:LEGIARTI000000000B01@2020-01-01',0,'Body B','sha256:b','test:v1','Code test > Article B body.'),
 ('chunk:V1','legi:LEGIARTI000000000V01@1990-01-01',0,'Body V1','sha256:v1','test:v1','Code rural > Article R242-40 (1990) body.'),
 ('chunk:V2','legi:LEGIARTI000000000V02@2003-01-01',0,'Body V2','sha256:v2','test:v1','Code rural > Article R242-40 (2003) body.'),
 ('chunk:W','legi:LEGIARTI000000000W01@2020-01-01',0,'Body W','sha256:w','test:v1','Code test > Article W body.');

INSERT INTO graph_edges (edge_id, from_document_id, to_document_id, edge_kind, edge_source, payload) VALUES
 ('edge:A-cites-B','legi:LEGIARTI000000000A01@2020-01-01',NULL,'refers_to','publisher',
  '{"source_tag":"LIEN","to_source_uid":"LEGIARTI000000000B01","attributes":[{"key":"typelien","value":"CITATION"},{"key":"sens","value":"cible"}]}'::jsonb),
 ('edge:V1-ver-V1','legi:LEGIARTI000000000V01@1990-01-01',NULL,'refers_to','publisher',
  '{"source_tag":"LIEN_ART","to_source_uid":"LEGIARTI000000000V01","attributes":[{"key":"debut","value":"1990-01-01"},{"key":"fin","value":"2003-01-01"},{"key":"num","value":"R242-40"},{"key":"etat","value":"ABROGE"}]}'::jsonb),
 ('edge:V1-ver-V2','legi:LEGIARTI000000000V01@1990-01-01',NULL,'refers_to','publisher',
  '{"source_tag":"LIEN_ART","to_source_uid":"LEGIARTI000000000V02","attributes":[{"key":"debut","value":"2003-01-01"},{"key":"fin","value":"2999-01-01"},{"key":"num","value":"R242-40"},{"key":"etat","value":"VIGUEUR"}]}'::jsonb),
 -- W has two version-attribute LIEN_ART edges but points only at OTHER articles (A, B), never
 -- itself: this must NOT be treated as a version family (self-inclusion guard).
 ('edge:W-ver-A','legi:LEGIARTI000000000W01@2020-01-01',NULL,'refers_to','publisher',
  '{"source_tag":"LIEN_ART","to_source_uid":"LEGIARTI000000000A01","attributes":[{"key":"debut","value":"2020-01-01"},{"key":"fin","value":"2999-01-01"},{"key":"num","value":"A"},{"key":"etat","value":"VIGUEUR"}]}'::jsonb),
 ('edge:W-ver-B','legi:LEGIARTI000000000W01@2020-01-01',NULL,'refers_to','publisher',
  '{"source_tag":"LIEN_ART","to_source_uid":"LEGIARTI000000000B01","attributes":[{"key":"debut","value":"2020-01-01"},{"key":"fin","value":"2999-01-01"},{"key":"num","value":"B"},{"key":"etat","value":"VIGUEUR"}]}'::jsonb);
"#;

#[test]
#[ignore = "requires pg_search/pgvector assets for managed Postgres"]
fn france_legi_gold_extracts_three_categories() -> Result<(), Box<dyn Error>> {
    let Some(pg_config) = discover_pg_config("france-legi gold")? else {
        return Ok(());
    };
    let root = tempfile::Builder::new()
        .prefix("jurisearch-france-legi-gold.")
        .tempdir()?;
    let postgres = ManagedPostgres::start_durable(pg_config, root.path())?;

    postgres.execute_sql(SEED_SQL)?;

    let gold = france_legi_gold_json(
        &postgres,
        FranceLegiGoldLimits {
            known_item: 10,
            temporal: 10,
            cross_reference: 10,
        },
    )?;
    let gold: Value = serde_json::from_str(&gold)?;

    // known-item: every article has a citation, so all four are picked.
    let known = gold["known_item"].as_array().expect("known_item array");
    assert!(
        known.len() >= 4,
        "expected >=4 known-item qrels, got {}",
        known.len()
    );
    assert!(known.iter().any(|q| q["gold_document_id"]
        == "legi:LEGIARTI000000000A01@2020-01-01"
        && q["query"] == "Code test Article A"
        && q["as_of"] == "2020-01-01"));

    // temporal: the R242-40 family has two versions resolved from its VERSIONS (LIEN_ART) edges.
    let temporal = gold["temporal"].as_array().expect("temporal array");
    assert!(
        temporal.len() >= 2,
        "expected >=2 temporal qrels, got {}",
        temporal.len()
    );
    let temporal_golds: Vec<&str> = temporal
        .iter()
        .filter_map(|q| q["gold_document_id"].as_str())
        .collect();
    assert!(temporal_golds.contains(&"legi:LEGIARTI000000000V01@1990-01-01"));
    assert!(temporal_golds.contains(&"legi:LEGIARTI000000000V02@2003-01-01"));
    for q in temporal {
        assert!(q["as_of"].is_string(), "temporal as_of must be a date string");
        assert!(
            q["query"].as_str().unwrap().contains("en vigueur au"),
            "temporal query phrasing: {}",
            q["query"]
        );
    }
    // self-inclusion guard: W's version-attribute edges point only at A/B (never W), so they must
    // NOT form a temporal family — A/B must not appear as temporal golds.
    assert!(
        !temporal_golds.contains(&"legi:LEGIARTI000000000A01@2020-01-01")
            && !temporal_golds.contains(&"legi:LEGIARTI000000000B01@2020-01-01"),
        "non-self VERSIONS edges leaked into temporal: {temporal_golds:?}"
    );

    // cross-reference: A cites B via a CITATION/cible edge; B is in the corpus.
    let xref = gold["cross_reference"].as_array().expect("cross_reference array");
    assert_eq!(xref.len(), 1, "expected exactly one citing article (A)");
    assert_eq!(
        xref[0]["query_document_id"],
        "legi:LEGIARTI000000000A01@2020-01-01"
    );
    let xref_golds = xref[0]["gold_document_ids"].as_array().unwrap();
    assert_eq!(xref_golds.len(), 1);
    assert_eq!(xref_golds[0], "legi:LEGIARTI000000000B01@2020-01-01");
    assert!(
        xref[0]["query"].as_str().unwrap().contains("article A body"),
        "cross-ref query should be the citing article text: {}",
        xref[0]["query"]
    );

    postgres.stop()?;
    Ok(())
}
