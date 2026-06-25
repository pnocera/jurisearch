//! Embedding runtime: the query-time `PreparedQueryEmbedder`, the embedding-config loaders
//! (env + TOML overlay), model-cache/endpoint status probes, and the concurrent
//! embedding-endpoint pool that drives bulk `embed-chunks` / `embed-zone-units`.

use crate::*;

/// A query-embedding client built once and reused across many searches. Building an
/// `OpenAiCompatibleClient` loads a tokenizer and a fresh HTTP agent, and `ensure_embedding_runtime_ready`
/// is a network probe — paying both per query in a batch sweep (the France-LEGI runner issues up to
/// ~192 queries) is wasteful. The index is static during a run, so one prepared embedder serves all.
pub(crate) struct PreparedQueryEmbedder {
    pub(crate) client: OpenAiCompatibleClient,
    pub(crate) expected_fingerprint: EmbeddingFingerprint,
    pub(crate) storage_fingerprint: String,
}

impl PreparedQueryEmbedder {
    pub(crate) fn from_env() -> Result<Self, ErrorObject> {
        let embedding_config = embedding_config_from_env();
        ensure_embedding_runtime_ready(&embedding_config, false)?;
        let expected_fingerprint = embedding_config.fingerprint();
        let storage_fingerprint = embedding_config.storage_embedding_fingerprint();
        let client =
            OpenAiCompatibleClient::new(embedding_config).map_err(embedding_error_object)?;
        Ok(Self {
            client,
            expected_fingerprint,
            storage_fingerprint,
        })
    }

    /// Returns `(pgvector_literal, storage_fingerprint)` for the query.
    pub(crate) fn embed(&self, query: &str) -> Result<(String, String), ErrorObject> {
        let embedding = self
            .client
            .embed_query(query, &self.expected_fingerprint)
            .map_err(embedding_error_object)?;
        Ok((
            pgvector_literal(&embedding.values),
            self.storage_fingerprint.clone(),
        ))
    }
}

pub(crate) const MODEL_CACHE_REQUIRED_FILES: &[&str] = &["model.onnx", "tokenizer.json"];

pub(crate) const EMBEDDING_ENDPOINT_MAX_ATTEMPTS: usize = 3;

pub(crate) const LOOPBACK_ENDPOINT_CONNECT_TIMEOUT: Duration = Duration::from_millis(250);

#[derive(Debug, Clone)]
pub(crate) struct EmbeddingEndpointPoolConfig {
    pub(crate) base_url: String,
    pub(crate) request_model: Option<String>,
    pub(crate) config: EmbeddingConfig,
    pub(crate) expected_fingerprint: EmbeddingFingerprint,
}

#[derive(Debug, Clone)]
pub(crate) struct EmbeddingEndpointState {
    pub(crate) base_url: String,
    pub(crate) request_model: Option<String>,
    pub(crate) outstanding: usize,
    pub(crate) requests: usize,
    pub(crate) chunks: usize,
    pub(crate) truncated_inputs: usize,
    pub(crate) failures: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct EmbeddingBatchWork {
    pub(crate) inputs: Vec<ChunkEmbeddingInput>,
}

#[derive(Debug, Clone)]
pub(crate) struct OwnedChunkEmbedding {
    pub(crate) chunk_id: String,
    pub(crate) embedding_literal: String,
}

#[derive(Debug, Clone)]
pub(crate) struct EmbeddingBatchSuccess {
    pub(crate) embeddings: Vec<OwnedChunkEmbedding>,
    pub(crate) truncated_inputs: usize,
}

#[derive(Debug, Clone)]
pub(crate) struct EmbeddingBatchFailure {
    pub(crate) error: ErrorObject,
}

#[derive(Debug, Clone)]
pub(crate) struct EmbeddingPoolRun {
    pub(crate) chunks_considered: usize,
    pub(crate) embeddings_inserted: usize,
    pub(crate) embedding_inputs_truncated: usize,
    pub(crate) endpoint_stats: Vec<Value>,
}

pub(crate) fn embedding_endpoint_pool_configs(
    config: &EmbeddingConfig,
    pool_endpoints: &[EmbeddingPoolEndpoint],
    expected_fingerprint: &EmbeddingFingerprint,
    storage_embedding_fingerprint: &str,
) -> Result<Vec<EmbeddingEndpointPoolConfig>, ErrorObject> {
    if !matches!(config.provider, EmbeddingProvider::OpenAiCompatible) {
        return Err(embedding_error_object(
            jurisearch_embed::EmbeddingError::UnsupportedProvider {
                provider: config.provider,
            },
        ));
    }

    let endpoint_specs = if pool_endpoints.is_empty() {
        legacy_embedding_pool_endpoints(config)
    } else {
        dedupe_embedding_pool_endpoints(pool_endpoints.to_vec())
    };
    if endpoint_specs.is_empty() {
        return Err(embedding_error_object(
            jurisearch_embed::EmbeddingError::MissingBaseUrl,
        ));
    }

    endpoint_specs
        .into_iter()
        .map(|endpoint| {
            let mut endpoint_config = config.clone();
            endpoint_config.base_url = Some(endpoint.base_url.clone());
            endpoint_config.base_urls = vec![endpoint.base_url.clone()];
            endpoint_config.request_model = endpoint.request_model.clone();
            if pool_endpoints.is_empty() {
                endpoint_config.api_key = config.api_key.clone();
            } else if endpoint.api_key_env.is_some() && endpoint.api_key.is_none() {
                let api_key_env = endpoint.api_key_env.as_deref().unwrap_or_default();
                return Err(dependency_unavailable(format!(
                    "embedding pool endpoint `{}` requires non-empty environment variable `{api_key_env}`",
                    endpoint.base_url
                )));
            } else {
                endpoint_config.api_key = endpoint.api_key.clone();
            }
            let endpoint_fingerprint = endpoint_config.fingerprint();
            if endpoint_fingerprint.provider != expected_fingerprint.provider
                || endpoint_fingerprint.model != expected_fingerprint.model
                || endpoint_fingerprint.dimension != expected_fingerprint.dimension
                || endpoint_fingerprint.normalize != expected_fingerprint.normalize
                || endpoint_fingerprint.pooling != expected_fingerprint.pooling
                || endpoint_fingerprint.storage_embedding_fingerprint()
                    != storage_embedding_fingerprint
            {
                return Err(dependency_unavailable(format!(
                    "embedding endpoint `{}` does not match the selected model fingerprint",
                    endpoint.base_url
                )));
            }
            Ok(EmbeddingEndpointPoolConfig {
                base_url: endpoint.base_url,
                request_model: endpoint.request_model,
                config: endpoint_config,
                expected_fingerprint: endpoint_fingerprint,
            })
        })
        .collect()
}

pub(crate) fn legacy_embedding_pool_endpoints(config: &EmbeddingConfig) -> Vec<EmbeddingPoolEndpoint> {
    let mut endpoints = config
        .base_urls
        .iter()
        .filter_map(|base_url| nonempty_string(Some(base_url.clone())))
        .map(|base_url| EmbeddingPoolEndpoint {
            base_url,
            request_model: None,
            api_key_env: None,
            api_key: config.api_key.clone(),
        })
        .collect::<Vec<_>>();
    if endpoints.is_empty()
        && let Some(base_url) = config
            .base_url
            .clone()
            .and_then(|base_url| nonempty_string(Some(base_url)))
    {
        endpoints.push(EmbeddingPoolEndpoint {
            base_url,
            request_model: None,
            api_key_env: None,
            api_key: config.api_key.clone(),
        });
    }
    dedupe_embedding_pool_endpoints(endpoints)
}

pub(crate) fn dedupe_embedding_pool_endpoints(
    endpoints: Vec<EmbeddingPoolEndpoint>,
) -> Vec<EmbeddingPoolEndpoint> {
    let mut deduped = Vec::new();
    for endpoint in endpoints {
        if !deduped.iter().any(|existing: &EmbeddingPoolEndpoint| {
            existing.base_url.trim_end_matches('/') == endpoint.base_url.trim_end_matches('/')
                && existing.request_model == endpoint.request_model
                && existing.api_key_env == endpoint.api_key_env
        }) {
            deduped.push(endpoint);
        }
    }
    deduped
}

/// Accumulate per-endpoint embedding stats across streamed pages, summing counters per `base_url`.
pub(crate) fn merge_embedding_endpoint_stats(accumulator: &mut Vec<Value>, page: Vec<Value>) {
    for stat in page {
        let base_url = stat
            .get("base_url")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let existing = accumulator.iter_mut().find(|entry| {
            entry.get("base_url").and_then(Value::as_str).map(str::to_owned) == base_url
        });
        match existing {
            Some(entry) => {
                for field in ["requests", "chunks", "truncated_inputs", "failures"] {
                    let sum = entry.get(field).and_then(Value::as_u64).unwrap_or(0)
                        + stat.get(field).and_then(Value::as_u64).unwrap_or(0);
                    entry[field] = json!(sum);
                }
            }
            None => accumulator.push(stat),
        }
    }
}

/// Generic embedding-pool driver: embeds `inputs` across the endpoint pool and applies `insert_batch`
/// to each completed batch's `(id, literal)` results. Identical for chunks and zone units (the workers
/// are id/text-agnostic); only the storage insert differs, so it is injected by the caller.
pub(crate) fn embed_and_insert_with_pool<F>(
    inputs: Vec<ChunkEmbeddingInput>,
    endpoint_configs: &[EmbeddingEndpointPoolConfig],
    batch_size: usize,
    pool_concurrency: usize,
    insert_batch: F,
) -> Result<EmbeddingPoolRun, ErrorObject>
where
    F: Fn(&[OwnedChunkEmbedding]) -> Result<usize, ErrorObject>,
{
    let chunks_considered = inputs.len();
    let work_queue = inputs
        .chunks(batch_size)
        .map(|inputs| EmbeddingBatchWork {
            inputs: inputs.to_vec(),
        })
        .collect::<VecDeque<_>>();
    let worker_count = pool_concurrency.min(work_queue.len().max(1));
    let work_queue = Arc::new(Mutex::new(work_queue));
    let endpoint_configs = Arc::new(endpoint_configs.to_vec());
    let endpoint_states = Arc::new(Mutex::new(
        endpoint_configs
            .iter()
            .map(|config| EmbeddingEndpointState {
                base_url: config.base_url.clone(),
                request_model: config.request_model.clone(),
                outstanding: 0,
                requests: 0,
                chunks: 0,
                truncated_inputs: 0,
                failures: 0,
            })
            .collect::<Vec<_>>(),
    ));
    let stop_requested = Arc::new(AtomicBool::new(false));
    let (sender, receiver) =
        mpsc::channel::<Result<EmbeddingBatchSuccess, EmbeddingBatchFailure>>();
    let mut handles = Vec::with_capacity(worker_count);

    for _ in 0..worker_count {
        let work_queue = Arc::clone(&work_queue);
        let endpoint_configs = Arc::clone(&endpoint_configs);
        let endpoint_states = Arc::clone(&endpoint_states);
        let stop_requested = Arc::clone(&stop_requested);
        let sender = sender.clone();
        handles.push(thread::spawn(move || {
            embedding_pool_worker(
                work_queue,
                endpoint_configs,
                endpoint_states,
                stop_requested,
                sender,
            );
        }));
    }
    drop(sender);

    let mut embeddings_inserted = 0usize;
    let mut embedding_inputs_truncated = 0usize;
    let mut first_error = None::<ErrorObject>;
    for message in receiver {
        match message {
            Ok(success) => {
                if first_error.is_some() {
                    continue;
                }
                embedding_inputs_truncated += success.truncated_inputs;
                match insert_batch(&success.embeddings) {
                    Ok(inserted) => {
                        embeddings_inserted += inserted;
                    }
                    Err(error) => {
                        stop_requested.store(true, Ordering::SeqCst);
                        first_error.get_or_insert(error);
                    }
                }
            }
            Err(failure) => {
                stop_requested.store(true, Ordering::SeqCst);
                first_error.get_or_insert(failure.error);
            }
        }
    }

    for handle in handles {
        if handle.join().is_err() && first_error.is_none() {
            first_error = Some(dependency_unavailable(
                "embedding endpoint pool worker panicked".to_owned(),
            ));
        }
    }

    if let Some(error) = first_error {
        return Err(error);
    }

    let endpoint_stats = endpoint_states
        .lock()
        .expect("embedding endpoint state lock")
        .iter()
        .map(|state| {
            json!({
                "base_url": state.base_url.as_str(),
                "request_model": state.request_model.as_deref(),
                "requests": state.requests,
                "chunks": state.chunks,
                "truncated_inputs": state.truncated_inputs,
                "failures": state.failures
            })
        })
        .collect();

    Ok(EmbeddingPoolRun {
        chunks_considered,
        embeddings_inserted,
        embedding_inputs_truncated,
        endpoint_stats,
    })
}

/// Embed chunk inputs across the pool and upsert into `chunk_embeddings` (thin wrapper over the generic
/// driver; behaviour unchanged).
pub(crate) fn embed_and_insert_chunks_with_pool(
    postgres: &ManagedPostgres,
    inputs: Vec<ChunkEmbeddingInput>,
    endpoint_configs: &[EmbeddingEndpointPoolConfig],
    embedding_fingerprint: &str,
    embedding_config: &EmbeddingConfig,
    batch_size: usize,
    pool_concurrency: usize,
) -> Result<EmbeddingPoolRun, ErrorObject> {
    embed_and_insert_with_pool(
        inputs,
        endpoint_configs,
        batch_size,
        pool_concurrency,
        |embeddings| {
            let inserts = embeddings
                .iter()
                .map(|embedding| ChunkEmbeddingInsert {
                    chunk_id: embedding.chunk_id.as_str(),
                    embedding_fingerprint,
                    embedding_literal: embedding.embedding_literal.as_str(),
                    model: embedding_config.model.as_str(),
                    dimension: embedding_config.dimension,
                })
                .collect::<Vec<_>>();
            insert_chunk_embeddings(postgres, &inserts).map_err(storage_error_object)
        },
    )
}

/// Embed zone-unit inputs across the SAME pool and upsert into `zone_unit_embeddings` (parallel to the
/// chunk wrapper; the only difference is the storage target). `OwnedChunkEmbedding.chunk_id` carries the
/// `zone_unit_id` here.
pub(crate) fn embed_and_insert_zone_units_with_pool(
    postgres: &ManagedPostgres,
    inputs: Vec<ChunkEmbeddingInput>,
    endpoint_configs: &[EmbeddingEndpointPoolConfig],
    embedding_fingerprint: &str,
    embedding_config: &EmbeddingConfig,
    batch_size: usize,
    pool_concurrency: usize,
) -> Result<EmbeddingPoolRun, ErrorObject> {
    embed_and_insert_with_pool(
        inputs,
        endpoint_configs,
        batch_size,
        pool_concurrency,
        |embeddings| {
            let inserts = embeddings
                .iter()
                .map(|embedding| ZoneUnitEmbeddingInsert {
                    zone_unit_id: embedding.chunk_id.as_str(),
                    embedding_fingerprint,
                    embedding_literal: embedding.embedding_literal.as_str(),
                    model: embedding_config.model.as_str(),
                    dimension: embedding_config.dimension,
                })
                .collect::<Vec<_>>();
            insert_zone_unit_embeddings(postgres, &inserts).map_err(storage_error_object)
        },
    )
}

pub(crate) fn embedding_pool_worker(
    work_queue: Arc<Mutex<VecDeque<EmbeddingBatchWork>>>,
    endpoint_configs: Arc<Vec<EmbeddingEndpointPoolConfig>>,
    endpoint_states: Arc<Mutex<Vec<EmbeddingEndpointState>>>,
    stop_requested: Arc<AtomicBool>,
    sender: mpsc::Sender<Result<EmbeddingBatchSuccess, EmbeddingBatchFailure>>,
) {
    let clients = match endpoint_configs
        .iter()
        .map(|config| OpenAiCompatibleClient::new(config.config.clone()))
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(clients) => clients,
        Err(error) => {
            stop_requested.store(true, Ordering::SeqCst);
            let _ = sender.send(Err(EmbeddingBatchFailure {
                error: embedding_error_object(error),
            }));
            return;
        }
    };

    while !stop_requested.load(Ordering::SeqCst) {
        let Some(work) = work_queue
            .lock()
            .expect("embedding work queue lock")
            .pop_front()
        else {
            return;
        };
        let endpoint_index = acquire_least_outstanding_endpoint(&endpoint_states);
        let result = embed_batch_on_endpoint(
            &clients[endpoint_index],
            &endpoint_configs[endpoint_index],
            &work,
        );
        let truncated_inputs = match &result {
            Ok(success) => success.truncated_inputs,
            Err(_) => 0,
        };
        release_embedding_endpoint(
            &endpoint_states,
            endpoint_index,
            work.inputs.len(),
            truncated_inputs,
            &result,
        );
        if sender.send(result).is_err() {
            return;
        }
    }
}

pub(crate) fn acquire_least_outstanding_endpoint(
    endpoint_states: &Arc<Mutex<Vec<EmbeddingEndpointState>>>,
) -> usize {
    let mut states = endpoint_states
        .lock()
        .expect("embedding endpoint state lock");
    let endpoint_index = states
        .iter()
        .enumerate()
        .min_by_key(|(_, state)| (state.outstanding, state.requests))
        .map(|(index, _)| index)
        .expect("at least one embedding endpoint");
    states[endpoint_index].outstanding += 1;
    states[endpoint_index].requests += 1;
    endpoint_index
}

pub(crate) fn release_embedding_endpoint(
    endpoint_states: &Arc<Mutex<Vec<EmbeddingEndpointState>>>,
    endpoint_index: usize,
    chunk_count: usize,
    truncated_inputs: usize,
    result: &Result<EmbeddingBatchSuccess, EmbeddingBatchFailure>,
) {
    let mut states = endpoint_states
        .lock()
        .expect("embedding endpoint state lock");
    let state = &mut states[endpoint_index];
    state.outstanding = state.outstanding.saturating_sub(1);
    match result {
        Ok(_) => {
            state.chunks += chunk_count;
            state.truncated_inputs += truncated_inputs;
        }
        Err(_) => state.failures += 1,
    }
}

pub(crate) fn embed_batch_on_endpoint(
    client: &OpenAiCompatibleClient,
    endpoint_config: &EmbeddingEndpointPoolConfig,
    work: &EmbeddingBatchWork,
) -> Result<EmbeddingBatchSuccess, EmbeddingBatchFailure> {
    let mut truncated_inputs = 0usize;
    let input_texts = work
        .inputs
        .iter()
        .map(|input| {
            let (text, truncated) =
                embedding_request_text(input.embedding_text.as_str(), &endpoint_config.config);
            if truncated {
                truncated_inputs += 1;
            }
            text
        })
        .collect::<Vec<_>>();
    let input_text_refs = input_texts
        .iter()
        .map(|input| input.as_ref())
        .collect::<Vec<_>>();
    let embeddings = embed_batch_with_retries(
        client,
        &input_text_refs,
        &endpoint_config.expected_fingerprint,
    )
    .map_err(|error| {
        let chunk_id = work
            .inputs
            .first()
            .map(|input| input.chunk_id.as_str())
            .unwrap_or("<empty-batch>");
        let mut object = embedding_error_object_with_context(error, chunk_id);
        object.message = format!(
            "embedding endpoint `{}` failed: {}",
            endpoint_config.base_url, object.message
        );
        EmbeddingBatchFailure { error: object }
    })?;
    let embeddings = work
        .inputs
        .iter()
        .zip(embeddings)
        .map(|(input, embedding)| OwnedChunkEmbedding {
            chunk_id: input.chunk_id.clone(),
            embedding_literal: pgvector_literal(&embedding.values),
        })
        .collect();
    Ok(EmbeddingBatchSuccess {
        embeddings,
        truncated_inputs,
    })
}

pub(crate) fn embed_batch_with_retries(
    client: &OpenAiCompatibleClient,
    input_texts: &[&str],
    expected_fingerprint: &EmbeddingFingerprint,
) -> Result<Vec<jurisearch_embed::EmbeddingVector>, jurisearch_embed::EmbeddingError> {
    let attempts = EMBEDDING_ENDPOINT_MAX_ATTEMPTS.max(1);
    let mut attempt = 1usize;
    loop {
        match client.embed_batch(input_texts, expected_fingerprint) {
            Ok(embeddings) => return Ok(embeddings),
            Err(error) if attempt < attempts && retryable_embedding_error(&error) => {
                thread::sleep(Duration::from_millis(250 * attempt as u64));
                attempt += 1;
            }
            Err(error) => return Err(error),
        }
    }
}

pub(crate) fn retryable_embedding_error(error: &jurisearch_embed::EmbeddingError) -> bool {
    matches!(
        error,
        jurisearch_embed::EmbeddingError::Endpoint(_)
            | jurisearch_embed::EmbeddingError::InvalidResponse(_)
    )
}

pub(crate) fn embedding_request_text<'a>(input: &'a str, config: &EmbeddingConfig) -> (Cow<'a, str>, bool) {
    let Some(max_input_chars) = embedding_request_char_budget(config) else {
        return (Cow::Borrowed(input), false);
    };
    if max_input_chars == 0 {
        return (Cow::Borrowed(input), false);
    }
    for (chars, (index, _)) in input.char_indices().enumerate() {
        if chars == max_input_chars {
            return (Cow::Owned(input[..index].to_owned()), true);
        }
    }
    (Cow::Borrowed(input), false)
}

pub(crate) fn embedding_request_char_budget(config: &EmbeddingConfig) -> Option<usize> {
    let token_char_budget = config
        .max_estimated_tokens
        .map(|tokens| tokens.saturating_mul(config.estimated_chars_per_token.max(1)));
    match (config.max_input_chars, token_char_budget) {
        (Some(chars), Some(token_chars)) => Some(chars.min(token_chars)),
        (Some(chars), None) => Some(chars),
        (None, Some(token_chars)) => Some(token_chars),
        (None, None) => None,
    }
}

#[derive(Debug)]
pub(crate) struct LoadedEmbeddingConfig {
    pub(crate) config: EmbeddingConfig,
    pub(crate) pool_endpoints: Vec<EmbeddingPoolEndpoint>,
    pub(crate) config_path: Option<PathBuf>,
    pub(crate) config_loaded: bool,
    pub(crate) config_error: Option<String>,
}

#[derive(Debug)]
pub(crate) struct RuntimeConfigLocation {
    pub(crate) path: PathBuf,
    pub(crate) explicit: bool,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RuntimeConfigFile {
    pub(crate) embedding: Option<EmbeddingConfigFile>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EmbeddingConfigFile {
    #[serde(default, deserialize_with = "deserialize_embedding_provider_option")]
    pub(crate) provider: Option<EmbeddingProvider>,
    pub(crate) base_url: Option<String>,
    pub(crate) base_urls: Option<Vec<String>>,
    pub(crate) pool: Option<Vec<EmbeddingPoolEndpointConfigFile>>,
    pub(crate) api_key: Option<String>,
    pub(crate) model: Option<String>,
    pub(crate) dimension: Option<usize>,
    pub(crate) normalize: Option<bool>,
    pub(crate) pooling: Option<String>,
    pub(crate) max_input_chars: Option<usize>,
    pub(crate) max_estimated_tokens: Option<usize>,
    pub(crate) estimated_chars_per_token: Option<usize>,
    pub(crate) tokenizer_json: Option<PathBuf>,
    pub(crate) tokenizer_path: Option<PathBuf>,
    pub(crate) provisional: Option<bool>,
    pub(crate) reembeddable: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct EmbeddingPoolEndpointConfigFile {
    pub(crate) base_url: String,
    pub(crate) request_model: Option<String>,
    pub(crate) api_key_env: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EmbeddingPoolEndpoint {
    pub(crate) base_url: String,
    pub(crate) request_model: Option<String>,
    pub(crate) api_key_env: Option<String>,
    pub(crate) api_key: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct ModelCacheStatus {
    pub(crate) required: bool,
    pub(crate) model_dir: PathBuf,
    pub(crate) model_cache_key: String,
    pub(crate) model_path: Option<PathBuf>,
    pub(crate) required_files: Vec<String>,
    pub(crate) missing_files: Vec<String>,
}

impl ModelCacheStatus {
    pub(crate) fn model_present(&self) -> bool {
        self.required && self.missing_files.is_empty()
    }

    pub(crate) fn state(&self) -> &'static str {
        if !self.required {
            "not_required"
        } else if self.model_present() {
            "ready"
        } else {
            "missing"
        }
    }
}

pub(crate) fn embedding_config_from_env() -> EmbeddingConfig {
    loaded_embedding_config().config
}

pub(crate) fn loaded_embedding_config() -> LoadedEmbeddingConfig {
    let mut embedding_config = EmbeddingConfig::phase0_bge_m3("http://127.0.0.1:8097/v1", None);
    let mut pool_endpoints = Vec::new();
    let mut config_path = None;
    let mut config_loaded = false;
    let mut config_error = None;

    if let Some(location) = runtime_config_location() {
        match fs::read_to_string(&location.path) {
            Ok(contents) => {
                config_path = Some(location.path.clone());
                match toml::from_str::<RuntimeConfigFile>(&contents) {
                    Ok(runtime_config) => {
                        if let Some(embedding) = runtime_config.embedding {
                            apply_embedding_file_config(
                                &mut embedding_config,
                                &mut pool_endpoints,
                                embedding,
                            );
                        }
                        config_loaded = true;
                    }
                    Err(error) => {
                        config_error =
                            Some(toml_parse_error_message(&location.path, &contents, &error));
                    }
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound && !location.explicit => {
                // The default config path is optional.
            }
            Err(error) => {
                config_path = Some(location.path.clone());
                config_error = Some(format!(
                    "failed to read `{}`: {error}",
                    location.path.display()
                ));
            }
        }
    }

    apply_embedding_env_overrides(&mut embedding_config, &mut pool_endpoints);

    LoadedEmbeddingConfig {
        config: embedding_config,
        pool_endpoints,
        config_path,
        config_loaded,
        config_error,
    }
}

pub(crate) fn runtime_config_location() -> Option<RuntimeConfigLocation> {
    if let Some(path) = std::env::var_os("JURISEARCH_CONFIG") {
        let text = path.to_string_lossy();
        let trimmed = text.trim();
        if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("none") || trimmed == "0" {
            return None;
        }
        return Some(RuntimeConfigLocation {
            path: PathBuf::from(trimmed),
            explicit: true,
        });
    }

    if let Some(config_home) = std::env::var_os("XDG_CONFIG_HOME")
        && !config_home.is_empty()
    {
        return Some(RuntimeConfigLocation {
            path: PathBuf::from(config_home)
                .join("jurisearch")
                .join("config.toml"),
            explicit: false,
        });
    }

    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(|home| RuntimeConfigLocation {
            path: PathBuf::from(home)
                .join(".config")
                .join("jurisearch")
                .join("config.toml"),
            explicit: false,
        })
}

pub(crate) fn apply_embedding_file_config(
    config: &mut EmbeddingConfig,
    pool_endpoints: &mut Vec<EmbeddingPoolEndpoint>,
    file_config: EmbeddingConfigFile,
) {
    if let Some(provider) = file_config.provider {
        config.provider = provider;
        if matches!(provider, EmbeddingProvider::InProcess) {
            config.base_url = None;
            config.base_urls.clear();
            config.api_key = None;
        }
    }
    if let Some(base_url) = nonempty_string(file_config.base_url) {
        config.provider = EmbeddingProvider::OpenAiCompatible;
        config.base_url = Some(base_url.clone());
        config.base_urls = vec![base_url];
    }
    if let Some(base_urls) = nonempty_string_list(file_config.base_urls) {
        config.provider = EmbeddingProvider::OpenAiCompatible;
        config.base_urls = base_urls;
        if config.base_url.is_none() {
            config.base_url = config.base_urls.first().cloned();
        }
    }
    if let Some(pool) = parse_embedding_pool_file_config(file_config.pool) {
        // A pool is an HTTP transport choice; it deliberately overrides local
        // in-process mode in the same config layer.
        config.provider = EmbeddingProvider::OpenAiCompatible;
        *pool_endpoints = pool;
    }
    if let Some(api_key) = nonempty_string(file_config.api_key) {
        config.api_key = Some(api_key);
    }
    if let Some(model) = nonempty_string(file_config.model) {
        config.model = model;
    }
    if let Some(dimension) = file_config.dimension {
        config.dimension = dimension;
    }
    if let Some(normalize) = file_config.normalize {
        config.normalize = normalize;
    }
    if let Some(pooling) = nonempty_string(file_config.pooling) {
        config.pooling = pooling;
    }
    if let Some(max_input_chars) = file_config.max_input_chars {
        config.max_input_chars = nonzero_usize(max_input_chars);
    }
    if let Some(max_estimated_tokens) = file_config.max_estimated_tokens {
        config.max_estimated_tokens = nonzero_usize(max_estimated_tokens);
    }
    if let Some(estimated_chars_per_token) = file_config.estimated_chars_per_token
        && estimated_chars_per_token != 0
    {
        config.estimated_chars_per_token = estimated_chars_per_token;
    }
    if file_config.tokenizer_json.is_some() {
        config.tokenizer_path = file_config.tokenizer_json;
    }
    if file_config.tokenizer_path.is_some() {
        config.tokenizer_path = file_config.tokenizer_path;
    }
    if let Some(provisional) = file_config.provisional {
        config.provisional = provisional;
    }
    if let Some(reembeddable) = file_config.reembeddable {
        config.reembeddable = reembeddable;
    }
    clear_unused_in_process_secret_fields(config);
    if matches!(config.provider, EmbeddingProvider::InProcess) {
        pool_endpoints.clear();
    }
}

pub(crate) fn apply_embedding_env_overrides(
    embedding_config: &mut EmbeddingConfig,
    pool_endpoints: &mut Vec<EmbeddingPoolEndpoint>,
) {
    if let Ok(provider) = std::env::var("JURISEARCH_EMBED_PROVIDER")
        && let Some(provider) = parse_embedding_provider(&provider)
    {
        embedding_config.provider = provider;
        if matches!(provider, EmbeddingProvider::InProcess) {
            embedding_config.base_url = None;
            embedding_config.base_urls.clear();
            embedding_config.api_key = None;
        }
    }
    if let Ok(base_url) = std::env::var("JURISEARCH_EMBED_BASE_URL")
        && let Some(base_url) = nonempty_string(Some(base_url))
    {
        embedding_config.provider = EmbeddingProvider::OpenAiCompatible;
        embedding_config.base_url = Some(base_url.clone());
        embedding_config.base_urls = vec![base_url];
    }
    if let Ok(base_urls) = std::env::var("JURISEARCH_EMBED_BASE_URLS")
        && let Some(base_urls) = parse_embedding_base_urls_env(&base_urls)
    {
        embedding_config.provider = EmbeddingProvider::OpenAiCompatible;
        embedding_config.base_urls = base_urls;
        if embedding_config.base_url.is_none() {
            embedding_config.base_url = embedding_config.base_urls.first().cloned();
        }
    }
    if let Ok(pool) = std::env::var("JURISEARCH_EMBED_POOL")
        && let Some(pool) = parse_embedding_pool_env(&pool)
    {
        // A pool is an HTTP transport choice; it deliberately overrides local
        // in-process mode in the same env layer.
        embedding_config.provider = EmbeddingProvider::OpenAiCompatible;
        *pool_endpoints = pool;
    }
    if let Ok(api_key) = std::env::var("JURISEARCH_EMBED_API_KEY")
        && let Some(api_key) = nonempty_string(Some(api_key))
    {
        embedding_config.api_key = Some(api_key);
    }
    if let Ok(model) = std::env::var("JURISEARCH_EMBED_MODEL") {
        embedding_config.model = model;
    }
    if let Ok(dimension) = std::env::var("JURISEARCH_EMBED_DIMENSION") {
        embedding_config.dimension = dimension.parse().unwrap_or(embedding_config.dimension);
    }
    if let Ok(normalize) = std::env::var("JURISEARCH_EMBED_NORMALIZE") {
        embedding_config.normalize = normalize.parse().unwrap_or(embedding_config.normalize);
    }
    if let Ok(pooling) = std::env::var("JURISEARCH_EMBED_POOLING") {
        embedding_config.pooling = pooling;
    }
    if let Ok(max_chars) = std::env::var("JURISEARCH_EMBED_MAX_INPUT_CHARS") {
        embedding_config.max_input_chars =
            parse_optional_usize(&max_chars).unwrap_or(embedding_config.max_input_chars);
    }
    if let Ok(max_tokens) = std::env::var("JURISEARCH_EMBED_MAX_ESTIMATED_TOKENS") {
        embedding_config.max_estimated_tokens =
            parse_optional_usize(&max_tokens).unwrap_or(embedding_config.max_estimated_tokens);
    }
    if let Ok(chars_per_token) = std::env::var("JURISEARCH_EMBED_ESTIMATED_CHARS_PER_TOKEN")
        && let Ok(parsed) = chars_per_token.parse::<usize>()
        && parsed != 0
    {
        embedding_config.estimated_chars_per_token = parsed;
    }
    if let Ok(tokenizer_path) = std::env::var("JURISEARCH_EMBED_TOKENIZER_JSON") {
        embedding_config.tokenizer_path = parse_optional_path_buf(&tokenizer_path);
    }
    clear_unused_in_process_secret_fields(embedding_config);
    if matches!(embedding_config.provider, EmbeddingProvider::InProcess) {
        pool_endpoints.clear();
    }
}

pub(crate) fn parse_embedding_provider(value: &str) -> Option<EmbeddingProvider> {
    match value.trim().to_ascii_lowercase().as_str() {
        "openai_compatible" | "openai-compatible" | "openai" | "remote" => {
            Some(EmbeddingProvider::OpenAiCompatible)
        }
        "in_process" | "in-process" | "local" => Some(EmbeddingProvider::InProcess),
        _ => None,
    }
}

pub(crate) fn nonempty_string(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let value = value.trim().to_owned();
        if value.is_empty() { None } else { Some(value) }
    })
}

pub(crate) fn nonempty_string_list(values: Option<Vec<String>>) -> Option<Vec<String>> {
    let values = values?
        .into_iter()
        .filter_map(|value| nonempty_string(Some(value)))
        .collect::<Vec<_>>();
    if values.is_empty() {
        None
    } else {
        Some(values)
    }
}

pub(crate) fn parse_embedding_base_urls_env(value: &str) -> Option<Vec<String>> {
    let values = value
        .split(|character: char| character == ',' || character == ';' || character.is_whitespace())
        .filter_map(|value| nonempty_string(Some(value.to_owned())))
        .collect::<Vec<_>>();
    if values.is_empty() {
        None
    } else {
        Some(values)
    }
}

pub(crate) fn parse_embedding_pool_file_config(
    endpoints: Option<Vec<EmbeddingPoolEndpointConfigFile>>,
) -> Option<Vec<EmbeddingPoolEndpoint>> {
    let endpoints = endpoints?
        .into_iter()
        .filter_map(|endpoint| {
            let base_url = nonempty_string(Some(endpoint.base_url))?;
            let request_model = nonempty_string(endpoint.request_model);
            let api_key_env = nonempty_string(endpoint.api_key_env);
            Some(embedding_pool_endpoint(
                base_url,
                request_model,
                api_key_env,
            ))
        })
        .collect::<Vec<_>>();
    if endpoints.is_empty() {
        None
    } else {
        Some(endpoints)
    }
}

pub(crate) fn parse_embedding_pool_env(value: &str) -> Option<Vec<EmbeddingPoolEndpoint>> {
    let endpoints = value
        .split([';', '\n'])
        .filter_map(|endpoint| {
            let mut parts = endpoint.split('|');
            let base_url = nonempty_string(parts.next().map(str::to_owned))?;
            let request_model = nonempty_string(parts.next().map(str::to_owned));
            let api_key_env = nonempty_string(parts.next().map(str::to_owned));
            Some(embedding_pool_endpoint(
                base_url,
                request_model,
                api_key_env,
            ))
        })
        .collect::<Vec<_>>();
    if endpoints.is_empty() {
        None
    } else {
        Some(endpoints)
    }
}

pub(crate) fn embedding_pool_endpoint(
    base_url: String,
    request_model: Option<String>,
    api_key_env: Option<String>,
) -> EmbeddingPoolEndpoint {
    let api_key = api_key_env
        .as_deref()
        .and_then(|env_name| std::env::var(env_name).ok())
        .and_then(|api_key| nonempty_string(Some(api_key)));
    EmbeddingPoolEndpoint {
        base_url,
        request_model,
        api_key_env,
        api_key,
    }
}

pub(crate) fn deserialize_embedding_provider_option<'de, D>(
    deserializer: D,
) -> Result<Option<EmbeddingProvider>, D::Error>
where
    D: Deserializer<'de>,
{
    let Some(value) = Option::<String>::deserialize(deserializer)? else {
        return Ok(None);
    };
    parse_embedding_provider(&value)
        .ok_or_else(|| {
            serde::de::Error::custom(format!("unsupported embedding provider `{value}`"))
        })
        .map(Some)
}

pub(crate) fn nonzero_usize(value: usize) -> Option<usize> {
    if value == 0 { None } else { Some(value) }
}

pub(crate) fn clear_unused_in_process_secret_fields(config: &mut EmbeddingConfig) {
    if matches!(config.provider, EmbeddingProvider::InProcess) {
        config.base_url = None;
        config.base_urls.clear();
        config.api_key = None;
        config.request_model = None;
    }
}

pub(crate) fn model_cache_status(config: &EmbeddingConfig) -> ModelCacheStatus {
    let model_dir = model_cache_dir();
    let required = matches!(config.provider, EmbeddingProvider::InProcess);
    let model_cache_key = model_cache_key(&config.model);
    let required_files = MODEL_CACHE_REQUIRED_FILES
        .iter()
        .map(|file| (*file).to_owned())
        .collect::<Vec<_>>();

    if !required {
        return ModelCacheStatus {
            required,
            model_dir,
            model_cache_key,
            model_path: None,
            required_files,
            missing_files: Vec::new(),
        };
    }

    let model_path = model_dir.join("embeddings").join(&model_cache_key);
    let missing_files = MODEL_CACHE_REQUIRED_FILES
        .iter()
        .filter(|file| !model_path.join(file).is_file())
        .map(|file| (*file).to_owned())
        .collect::<Vec<_>>();

    ModelCacheStatus {
        required,
        model_dir,
        model_cache_key,
        model_path: Some(model_path),
        required_files,
        missing_files,
    }
}

pub(crate) fn model_cache_status_json(status: &ModelCacheStatus) -> Value {
    json!({
        "required": status.required,
        "state": status.state(),
        "model_dir": status.model_dir.display().to_string(),
        "model_cache_key": status.model_cache_key,
        "model_path": status.model_path.as_ref().map(|path| path.display().to_string()),
        "model_present": if status.required { Some(status.model_present()) } else { None },
        "required_files": status.required_files,
        "missing_files": status.missing_files,
    })
}

pub(crate) fn embedding_pool_endpoints_status_json(endpoints: &[EmbeddingPoolEndpoint]) -> Vec<Value> {
    endpoints
        .iter()
        .map(|endpoint| {
            json!({
                "base_url": endpoint.base_url,
                "request_model": endpoint.request_model,
                "api_key_env": endpoint.api_key_env,
                "api_key_configured": endpoint.api_key.is_some()
            })
        })
        .collect()
}

pub(crate) fn model_cache_dir() -> PathBuf {
    if let Some(model_dir) = std::env::var_os("JURISEARCH_MODEL_DIR")
        && !model_dir.is_empty()
    {
        return PathBuf::from(model_dir);
    }

    if let Some(cache_home) = std::env::var_os("XDG_CACHE_HOME")
        && !cache_home.is_empty()
    {
        return PathBuf::from(cache_home).join("jurisearch").join("models");
    }

    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(|home| {
            PathBuf::from(home)
                .join(".cache")
                .join("jurisearch")
                .join("models")
        })
        .unwrap_or_else(|| PathBuf::from(".jurisearch").join("models"))
}

pub(crate) fn model_cache_key(model: &str) -> String {
    let mut key = String::with_capacity(model.len());
    for character in model.trim().chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-') {
            key.push(character);
        } else if character == '/' || character == '\\' {
            key.push_str("__");
        } else {
            key.push('_');
        }
    }
    if key.is_empty() {
        "model".to_owned()
    } else {
        key
    }
}

pub(crate) fn embedding_endpoint_status_json(config: &EmbeddingConfig) -> Value {
    if !matches!(config.provider, EmbeddingProvider::OpenAiCompatible) {
        return json!({
            "checked": false,
            "state": "not_applicable",
            "reachable": Value::Null,
            "message": "in-process embedding providers do not use an HTTP endpoint"
        });
    }

    let Some(base_url) = config.base_url.as_deref() else {
        return json!({
            "checked": true,
            "state": "invalid",
            "reachable": false,
            "message": "embedding base_url is not configured"
        });
    };

    let fingerprint = config.fingerprint();
    if !matches!(
        fingerprint.base_url_class,
        jurisearch_embed::BaseUrlClass::LocalLoopback
    ) {
        return json!({
            "checked": false,
            "state": "not_checked",
            "reachable": Value::Null,
            "message": "hosted endpoints are not probed by status to avoid unsolicited external network calls"
        });
    }

    match loopback_endpoint_reachable(base_url) {
        Ok(true) => json!({
            "checked": true,
            "state": "reachable",
            "reachable": true,
            "message": "loopback embedding endpoint accepted a TCP connection"
        }),
        Ok(false) => json!({
            "checked": true,
            "state": "unreachable",
            "reachable": false,
            "message": "loopback embedding endpoint did not accept a TCP connection"
        }),
        Err(message) => json!({
            "checked": true,
            "state": "invalid",
            "reachable": false,
            "message": message
        }),
    }
}

pub(crate) fn loopback_endpoint_reachable(base_url: &str) -> Result<bool, String> {
    let parsed =
        Url::parse(base_url).map_err(|error| format!("invalid embedding base_url: {error}"))?;
    let Some(host) = parsed.host_str() else {
        return Err("embedding base_url has no host".to_owned());
    };
    let port = parsed.port_or_known_default().ok_or_else(|| {
        format!(
            "embedding base_url scheme `{}` has no default port",
            parsed.scheme()
        )
    })?;
    let addresses = (host, port)
        .to_socket_addrs()
        .map_err(|error| format!("failed to resolve embedding endpoint `{host}:{port}`: {error}"))?
        .collect::<Vec<_>>();
    if addresses.is_empty() {
        return Err(format!(
            "embedding endpoint `{host}:{port}` resolved no addresses"
        ));
    }
    Ok(addresses.into_iter().any(|address| {
        TcpStream::connect_timeout(&address, LOOPBACK_ENDPOINT_CONNECT_TIMEOUT).is_ok()
    }))
}

pub(crate) fn toml_parse_error_message(path: &Path, contents: &str, error: &toml::de::Error) -> String {
    if let Some(span) = error.span() {
        let (line, column) = line_column_for_offset(contents, span.start);
        format!(
            "failed to parse `{}`: TOML syntax error at line {line}, column {column}",
            path.display()
        )
    } else {
        format!("failed to parse `{}`: TOML syntax error", path.display())
    }
}

pub(crate) fn line_column_for_offset(contents: &str, byte_offset: usize) -> (usize, usize) {
    let mut line = 1;
    let mut column = 1;
    for (index, character) in contents.char_indices() {
        if index >= byte_offset {
            break;
        }
        if character == '\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }
    (line, column)
}

pub(crate) fn ensure_embedding_runtime_ready(
    embedding_config: &EmbeddingConfig,
    allow_download: bool,
) -> Result<(), ErrorObject> {
    let model_cache = model_cache_status(embedding_config);
    embedding_config
        .ensure_in_process_ready(model_cache.model_present(), allow_download)
        .map_err(embedding_error_object)
}
