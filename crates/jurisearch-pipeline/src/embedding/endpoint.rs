//! Embedding endpoint identity shared by the runtime config loader (in the CLI) and the bulk pool.

/// One resolved embedding endpoint: a base URL plus its optional per-endpoint request model and API
/// key. Constructed by the runtime config loader; consumed by the bulk embedding pool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingPoolEndpoint {
    pub base_url: String,
    pub request_model: Option<String>,
    pub api_key_env: Option<String>,
    pub api_key: Option<String>,
}

/// Trim a value, returning `None` when it is empty after trimming.
#[must_use]
pub fn nonempty_string(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim().to_owned();
        if value.is_empty() { None } else { Some(value) }
    })
}
