use jurisearch_embed::{
    EmbeddingConfig, OpenAiCompatibleClient, PHASE0_EMBEDDING_DIMENSION, PHASE0_EMBEDDING_MODEL,
};

#[test]
#[ignore = "requires a running OpenAI-compatible bge-m3 embeddings endpoint"]
fn live_bge_m3_endpoint_returns_expected_dimension_and_norm() {
    let base_url = std::env::var("JURISEARCH_EMBED_BASE_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8097/v1".to_owned());
    let api_key = std::env::var("JURISEARCH_EMBED_API_KEY").ok();
    let config = EmbeddingConfig::phase0_bge_m3(base_url, api_key);
    let expected = config.fingerprint();
    let client = OpenAiCompatibleClient::new(config).unwrap();

    let embedding = client
        .embed_query(
            "responsabilite civile faute dommage article 1240",
            &expected,
        )
        .unwrap();

    assert_eq!(embedding.values.len(), PHASE0_EMBEDDING_DIMENSION);
    assert_eq!(embedding.fingerprint.model, PHASE0_EMBEDDING_MODEL);
    assert!((embedding.l2_norm() - 1.0).abs() < 0.01);
}
