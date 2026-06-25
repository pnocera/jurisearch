//! Tokenizer loading.

use super::*;

pub(crate) fn load_tokenizer(path: Option<&Path>) -> Result<Option<Tokenizer>, EmbeddingError> {
    let Some(path) = path else {
        return Ok(None);
    };
    let mut tokenizer =
        Tokenizer::from_file(path).map_err(|error| EmbeddingError::TokenizerLoad {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
    tokenizer
        .with_truncation(None)
        .map_err(|error| EmbeddingError::TokenizerLoad {
            path: path.to_path_buf(),
            message: error.to_string(),
        })?;
    tokenizer.with_padding(None);
    Ok(Some(tokenizer))
}
