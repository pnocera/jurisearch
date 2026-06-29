use jurisearch_ingest::juri::CanonicalDecision;
use jurisearch_ingest::legi::{
    CanonicalDocument, CanonicalGraphEdge, ParsedSectionTa, ParsedTextStruct, ParsedTextVersion,
};
use postgres::GenericClient;

use crate::runtime::{ManagedPostgres, StorageError};

mod decisions;
mod embeddings;
mod graph_edges;
mod hierarchy_backfill;
mod legi;
mod metadata;

use self::graph_edges::*;
use self::metadata::*;

pub use decisions::{
    DocumentProjectionStatements, insert_decision_documents, insert_decision_documents_with_client,
    insert_decision_documents_with_statements, prepare_document_projection_statements,
};
pub use embeddings::{
    ChunkEmbeddingInsert, insert_chunk_embeddings, insert_chunk_embeddings_with_client,
};
pub use hierarchy_backfill::{
    LegiHierarchyBackfillReport, LegiHierarchyBackfillScope,
    backfill_legi_article_hierarchy_from_metadata,
    backfill_legi_article_hierarchy_from_metadata_scoped,
    backfill_legi_article_hierarchy_from_metadata_scoped_with_client,
};
pub use legi::{
    CanonicalInsertReport, LegiProjectionStatements, insert_legi_documents,
    insert_legi_documents_with_client, insert_legi_documents_with_statements,
    prepare_legi_projection_statements,
};
pub use metadata::{
    LegiMetadataInsertReport, LegiMetadataRoot, insert_legi_metadata_roots,
    insert_legi_metadata_roots_with_client,
};

#[cfg(test)]
mod tests {
    use super::hierarchy_backfill::*;
    use super::*;
    use serde_json::json;

    #[test]
    fn text_struct_hierarchy_handles_niv_gaps_and_sibling_resets() -> Result<(), StorageError> {
        let section = json!({
            "title": "Titre fallback",
            "hierarchy_path": ["Code civil"],
        });
        let links_json = json!([
            {"text": "Livre I", "level": 1},
            {"text": "Section gap", "level": 3},
            {"text": "Livre II", "level": 1},
            {"text": "Titre II", "level": 2},
        ])
        .to_string();

        let hierarchy = text_struct_hierarchy_from_links(&section, &links_json)?
            .expect("TEXTELR hierarchy should be built");

        assert_eq!(
            hierarchy,
            string_vec(&["Code civil", "Livre II", "Titre II"])
        );
        Ok(())
    }

    #[test]
    fn merge_hierarchy_with_overlap_deduplicates_existing_prefix() {
        let hierarchy = merge_hierarchy_with_overlap(
            string_vec(&["Code civil", "Livre I"]),
            string_vec(&["Livre I", "Titre II"]),
        );

        assert_eq!(
            hierarchy,
            string_vec(&["Code civil", "Livre I", "Titre II"])
        );
    }

    #[test]
    fn enriched_article_hierarchy_keeps_section_when_text_struct_is_not_richer()
    -> Result<(), StorageError> {
        let document_json = json!({
            "title": "Article 1",
            "body": "Body",
            "hierarchy_path": ["Code civil"],
            "chunks": [{
                "body": "Body",
                "hierarchy_path": ["Code civil"],
                "contextualized_body": "Code civil > Article 1\n\nBody"
            }]
        })
        .to_string();
        let section_json = json!({
            "title": "Titre officiel",
            "hierarchy_path": ["Code civil", "Livre officiel"],
        })
        .to_string();
        let text_links_json = json!([
            {"text": "Livre officiel", "level": 1},
            {"text": "Titre texte", "level": 2},
        ])
        .to_string();

        let enriched =
            enriched_article_hierarchy_json(&document_json, &section_json, Some(&text_links_json))?
                .expect("section hierarchy should enrich the article");
        let enriched: serde_json::Value = serde_json::from_str(&enriched)?;

        assert_eq!(
            string_array_field(&enriched, "hierarchy_path"),
            string_vec(&["Code civil", "Livre officiel", "Titre officiel"])
        );
        assert_eq!(
            string_array_field(&enriched["chunks"][0], "hierarchy_path"),
            string_vec(&["Code civil", "Livre officiel", "Titre officiel"])
        );
        assert_eq!(
            enriched["chunks"][0]["contextualized_body"].as_str(),
            Some("Code civil > Livre officiel > Titre officiel > Article 1\n\nBody")
        );
        Ok(())
    }

    fn string_vec(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_owned()).collect()
    }
}
