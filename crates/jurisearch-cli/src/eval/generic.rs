//! Generic retrieval-eval harness: eval run/tune/phase1 + the metric/qrel/bootstrap machinery.

use crate::*;

#[derive(Debug, Deserialize)]
pub(crate) struct EvalQuestion {
    pub(crate) id: String,
    pub(crate) query: String,
    #[serde(default)]
    pub(crate) as_of: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct EvalQrel {
    pub(crate) query_id: String,
    pub(crate) document_id: String,
    pub(crate) label: i64,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum MetricKind {
    Precision,
    Recall,
    Ndcg,
    Mrr,
}

#[derive(Debug, Clone)]
pub(crate) struct MetricSpec {
    pub(crate) kind: MetricKind,
    pub(crate) k: usize,
    pub(crate) name: String,
}

pub(crate) struct PoolCandidate {
    pub(crate) uid: String,
    pub(crate) title: Value,
    pub(crate) snippet: Value,
}

pub(crate) struct EvalQuestionResult {
    pub(crate) id: String,
    pub(crate) query: String,
    pub(crate) per_mode: HashMap<&'static str, Vec<String>>,
    pub(crate) pool: Vec<PoolCandidate>,
    pub(crate) labels: HashMap<String, i64>,
}

/// Deterministic xorshift64 RNG so bootstrap CIs are reproducible (no rand dependency, and
/// `Math.random`-style nondeterminism would make eval artifacts unstable).
pub(crate) struct XorShift64(u64);

impl XorShift64 {
    pub(crate) fn new(seed: u64) -> Self {
        Self(seed | 1)
    }
    pub(crate) fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
}

/// FNV-1a fold of a question id → a stable bootstrap/shuffle seed (reproducible across runs).
pub(crate) fn eval_question_seed(id: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in id.bytes() {
        hash ^= byte as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

pub(crate) fn load_eval_json<T: serde::de::DeserializeOwned>(
    path: &Path,
    what: &str,
) -> Result<T, ErrorObject> {
    let bytes = fs::read(path).map_err(|error| {
        ErrorObject::bad_input(format!(
            "failed to read {what} file {}: {error}",
            path.display()
        ))
    })?;
    serde_json::from_slice(&bytes).map_err(|error| {
        ErrorObject::bad_input(format!(
            "invalid {what} JSON in {}: {error}",
            path.display()
        ))
    })
}

pub(crate) fn parse_eval_modes(value: &str) -> Result<Vec<RetrievalMode>, ErrorObject> {
    let mut modes = Vec::new();
    for token in value
        .split(',')
        .map(str::trim)
        .filter(|token| !token.is_empty())
    {
        let mode = match token {
            "bm25" => RetrievalMode::Bm25,
            "dense" => RetrievalMode::Dense,
            "hybrid" => RetrievalMode::Hybrid,
            other => {
                return Err(ErrorObject::bad_input(format!(
                    "unknown mode `{other}`; expected bm25, dense, or hybrid"
                )));
            }
        };
        if !modes.contains(&mode) {
            modes.push(mode);
        }
    }
    if modes.is_empty() {
        return Err(ErrorObject::bad_input(
            "--modes must list at least one of bm25, dense, hybrid",
        ));
    }
    Ok(modes)
}

pub(crate) fn parse_eval_metric(value: &str) -> Result<MetricSpec, ErrorObject> {
    let value = value.trim();
    let (name, k_str) = value.split_once('@').unwrap_or((value, "10"));
    let k: usize = k_str
        .parse()
        .map_err(|_| ErrorObject::bad_input(format!("metric `{value}` has a non-numeric @k")))?;
    if k == 0 {
        return Err(ErrorObject::bad_input(format!(
            "metric `{value}` @k must be >= 1"
        )));
    }
    let kind = match name {
        "p" | "precision" => MetricKind::Precision,
        "recall" => MetricKind::Recall,
        "ndcg" => MetricKind::Ndcg,
        "mrr" => MetricKind::Mrr,
        other => {
            return Err(ErrorObject::bad_input(format!(
                "unknown metric `{other}`; expected p, recall, ndcg, or mrr"
            )));
        }
    };
    Ok(MetricSpec {
        kind,
        k,
        name: format!("{name}@{k}"),
    })
}

/// Per-question metric value over a mode's ranked doc list. `recall` returns `None` when the pool
/// has no relevant document (so it is excluded from the mean, not counted as 0).
pub(crate) fn compute_eval_metric(
    spec: &MetricSpec,
    top: &[String],
    labels: &HashMap<String, i64>,
    pool: &[String],
    rel_min: i64,
) -> Option<f64> {
    let label_of = |uid: &String| *labels.get(uid).unwrap_or(&0);
    let topk: Vec<&String> = top.iter().take(spec.k).collect();
    let relevant: HashSet<&String> = pool.iter().filter(|uid| label_of(uid) >= rel_min).collect();
    match spec.kind {
        MetricKind::Precision => {
            // Standard P@k: divide by k (missing ranks count as non-relevant), so a short page does
            // not inflate precision (document grouping can exhaust the pool before k).
            let hits = topk.iter().filter(|uid| label_of(uid) >= rel_min).count();
            Some(hits as f64 / spec.k as f64)
        }
        MetricKind::Recall => {
            if relevant.is_empty() {
                None
            } else {
                let hits = topk.iter().filter(|uid| relevant.contains(*uid)).count();
                Some(hits as f64 / relevant.len() as f64)
            }
        }
        MetricKind::Ndcg => {
            let gain = |label: i64| (2f64.powi(label.max(0) as i32)) - 1.0;
            let dcg: f64 = topk
                .iter()
                .enumerate()
                .map(|(i, uid)| gain(label_of(uid)) / ((i as f64) + 2.0).log2())
                .sum();
            let mut ideal: Vec<i64> = pool.iter().map(|uid| label_of(uid)).collect();
            ideal.sort_unstable_by(|a, b| b.cmp(a));
            let idcg: f64 = ideal
                .iter()
                .take(spec.k)
                .enumerate()
                .map(|(i, label)| gain(*label) / ((i as f64) + 2.0).log2())
                .sum();
            Some(if idcg > 0.0 { dcg / idcg } else { 0.0 })
        }
        MetricKind::Mrr => {
            for (i, uid) in topk.iter().enumerate() {
                if label_of(uid) >= rel_min {
                    return Some(1.0 / ((i as f64) + 1.0));
                }
            }
            Some(0.0)
        }
    }
}

pub(crate) fn mean_of(values: &[Option<f64>]) -> Option<f64> {
    let present: Vec<f64> = values.iter().filter_map(|value| *value).collect();
    if present.is_empty() {
        None
    } else {
        Some(present.iter().sum::<f64>() / present.len() as f64)
    }
}

/// Bootstrap a 95% CI for the mean difference (a - b), resampling QUESTIONS with replacement.
pub(crate) fn bootstrap_delta_ci(
    a: &[Option<f64>],
    b: &[Option<f64>],
    resamples: u32,
) -> (f64, f64, f64) {
    let n = a.len();
    let resample_mean = |idx: &[usize], values: &[Option<f64>]| -> Option<f64> {
        let present: Vec<f64> = idx.iter().filter_map(|&i| values[i]).collect();
        if present.is_empty() {
            None
        } else {
            Some(present.iter().sum::<f64>() / present.len() as f64)
        }
    };
    let all: Vec<usize> = (0..n).collect();
    let point = match (resample_mean(&all, a), resample_mean(&all, b)) {
        (Some(x), Some(y)) => x - y,
        _ => f64::NAN,
    };
    let mut rng = XorShift64::new(0x6a75_7269_7365_6172 ^ n as u64);
    let mut deltas: Vec<f64> = Vec::with_capacity(resamples as usize);
    for _ in 0..resamples {
        let sample: Vec<usize> = (0..n)
            .map(|_| (rng.next_u64() % n.max(1) as u64) as usize)
            .collect();
        if let (Some(x), Some(y)) = (resample_mean(&sample, a), resample_mean(&sample, b)) {
            deltas.push(x - y);
        }
    }
    if deltas.is_empty() {
        return (point, f64::NAN, f64::NAN);
    }
    deltas.sort_by(|x, y| x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal));
    let lo = deltas[((0.025 * deltas.len() as f64) as usize).min(deltas.len() - 1)];
    let hi = deltas[((0.975 * deltas.len() as f64) as usize).min(deltas.len() - 1)];
    (point, lo, hi)
}

pub(crate) fn run_external_judge(command: &str, input: &Value) -> Result<Value, ErrorObject> {
    use std::process::{Command, Stdio};
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| dependency_unavailable(format!("failed to spawn judge: {error}")))?;
    let payload = serde_json::to_vec(input).map_err(|error| {
        dependency_unavailable(format!("failed to encode judge input: {error}"))
    })?;
    child
        .stdin
        .take()
        .ok_or_else(|| dependency_unavailable("judge stdin unavailable"))?
        .write_all(&payload)
        .map_err(|error| dependency_unavailable(format!("failed to write judge stdin: {error}")))?;
    let output = child
        .wait_with_output()
        .map_err(|error| dependency_unavailable(format!("judge did not complete: {error}")))?;
    if !output.status.success() {
        return Err(dependency_unavailable(format!(
            "judge command failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    serde_json::from_slice(&output.stdout).map_err(|error| {
        ErrorObject::bad_input(format!("judge stdout was not a JSON label map: {error}"))
    })
}

/// Custom retrieval eval: retrieve each question through the chosen modes (document grouping), pool
/// candidates, get relevance labels from qrels or an external judge, score per mode, and optionally
/// bootstrap between-mode delta CIs. Opens the index once.
pub(crate) fn eval_run_payload(
    args: EvalRunArgs,
    options: RetrievalOptions,
    index_dir: Option<&Path>,
) -> Result<Value, ErrorObject> {
    if args.qrels.is_none() && args.judge_cmd.is_none() {
        return Err(ErrorObject::bad_input(
            "eval run needs relevance labels: provide --qrels or --judge-cmd",
        ));
    }
    if args.qrels.is_some() && args.judge_cmd.is_some() {
        return Err(ErrorObject::bad_input(
            "provide --qrels OR --judge-cmd, not both",
        ));
    }
    if args.top_k == 0 {
        return Err(ErrorObject::bad_input("--top-k must be at least 1"));
    }
    validate_retrieval_options(&options)?;
    let modes = parse_eval_modes(&args.modes)?;
    let metrics: Vec<MetricSpec> = args
        .metrics
        .split(',')
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(parse_eval_metric)
        .collect::<Result<Vec<_>, _>>()?;
    if metrics.is_empty() {
        return Err(ErrorObject::bad_input(
            "--metrics must list at least one metric",
        ));
    }
    let questions: Vec<EvalQuestion> = load_eval_json(&args.questions, "questions")?;
    if questions.is_empty() {
        return Err(ErrorObject::bad_input("questions file is empty"));
    }

    // A BM25-only eval must not require the embedding runtime: only build the embedder and embed
    // when a dense/hybrid mode is requested, and use the lexical readiness gate otherwise.
    let needs_dense = modes.iter().any(|mode| mode.uses_dense());
    let index_dir = require_existing_index_dir(index_dir)?;
    let postgres = open_index(index_dir.as_path())?;
    ensure_query_readiness(
        &postgres,
        if needs_dense {
            QueryReadinessGate::Search
        } else {
            QueryReadinessGate::SearchLexical
        },
    )?;
    let embedder = if needs_dense {
        Some(PreparedQueryEmbedder::from_env()?)
    } else {
        None
    };
    let pool_limit = args.top_k.saturating_mul(20);

    // 1. Retrieval: per question, each mode's top docs + the pooled candidate set.
    let mut results: Vec<EvalQuestionResult> = Vec::with_capacity(questions.len());
    for question in &questions {
        let normalized = parade_query_text(&question.query).ok_or_else(|| {
            ErrorObject::bad_input(format!(
                "question `{}` has no searchable token: {:?}",
                question.id, question.query
            ))
        })?;
        let as_of = question.as_of.clone().unwrap_or_else(today_utc);
        let embedded = match &embedder {
            Some(embedder) => Some(embedder.embed(question.query.as_str())?),
            None => None,
        };
        let mut per_mode: HashMap<&'static str, Vec<String>> = HashMap::new();
        let mut pool: Vec<PoolCandidate> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for mode in &modes {
            let (embedding, fingerprint) = match (&embedded, mode.uses_dense()) {
                (Some((literal, fingerprint)), true) => {
                    (Some(literal.as_str()), Some(fingerprint.as_str()))
                }
                _ => (None, None),
            };
            let response = hybrid_candidates_json(
                &postgres,
                &HybridCandidateQuery {
                    query_text: &normalized,
                    query_embedding: embedding,
                    embedding_fingerprint: fingerprint,
                    retrieval_mode: *mode,
                    group_by: GroupBy::Document,
                    options,
                    after_cursor: None,
                    as_of: as_of.as_str(),
                    kind_filter: Some("article"),
                    project_authority: false,
                    decision_filters: DecisionFilters::default(),
                    lexical_limit: pool_limit,
                    dense_limit: pool_limit,
                    limit: args.top_k,
                },
            )
            .map_err(storage_error_object)?;
            let response: Value = serde_json::from_str(&response)
                .map_err(|error| dependency_unavailable(error.to_string()))?;
            let candidates = response["candidates"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let mut top = Vec::new();
            for candidate in &candidates {
                let Some(uid) = candidate["document_id"].as_str() else {
                    continue;
                };
                top.push(uid.to_owned());
                if seen.insert(uid.to_owned()) {
                    pool.push(PoolCandidate {
                        uid: uid.to_owned(),
                        title: candidate.get("title").cloned().unwrap_or(Value::Null),
                        snippet: candidate.get("snippet").cloned().unwrap_or(Value::Null),
                    });
                }
            }
            per_mode.insert(mode.as_str(), top);
        }
        results.push(EvalQuestionResult {
            id: question.id.clone(),
            query: question.query.clone(),
            per_mode,
            pool,
            labels: HashMap::new(),
        });
    }

    // 2. Relevance labels: qrels lookup, or a single blind external-judge invocation.
    let judge_source;
    if let Some(qrels_path) = &args.qrels {
        let qrels: Vec<EvalQrel> = load_eval_json(qrels_path, "qrels")?;
        let mut by_query: HashMap<String, HashMap<String, i64>> = HashMap::new();
        for qrel in qrels {
            by_query
                .entry(qrel.query_id)
                .or_default()
                .insert(qrel.document_id, qrel.label);
        }
        for result in &mut results {
            if let Some(labels) = by_query.get(&result.id) {
                result.labels = labels.clone();
            }
        }
        judge_source = "qrels".to_owned();
    } else {
        let command = args.judge_cmd.as_deref().unwrap_or_default();
        let mut judge_questions = Vec::new();
        let mut keymaps: HashMap<String, HashMap<String, String>> = HashMap::new();
        for result in &results {
            let mut candidates = Vec::new();
            let mut keymap = HashMap::new();
            // Deterministic per-question shuffle: the pool is built mode-by-mode (bm25 first), so
            // unshuffled keys would leak provenance and bias a position-sensitive judge. Seeded by
            // the question id for reproducibility.
            let mut order: Vec<usize> = (0..result.pool.len()).collect();
            let mut rng = XorShift64::new(eval_question_seed(&result.id));
            for i in (1..order.len()).rev() {
                let j = (rng.next_u64() % (i as u64 + 1)) as usize;
                order.swap(i, j);
            }
            for (slot, &pool_index) in order.iter().enumerate() {
                let candidate = &result.pool[pool_index];
                let key = format!("c{:02}", slot + 1);
                keymap.insert(key.clone(), candidate.uid.clone());
                candidates.push(json!({
                    "key": key,
                    "title": candidate.title,
                    "snippet": candidate.snippet,
                }));
            }
            judge_questions.push(json!({
                "question_id": result.id,
                "question": result.query,
                "candidates": candidates,
            }));
            keymaps.insert(result.id.clone(), keymap);
        }
        let judge_output = run_external_judge(command, &json!({ "questions": judge_questions }))?;
        for result in &mut results {
            let Some(per_key) = judge_output.get(&result.id).and_then(Value::as_object) else {
                continue;
            };
            let keymap = &keymaps[&result.id];
            for (key, label) in per_key {
                if let (Some(uid), Some(label)) = (keymap.get(key), label.as_i64()) {
                    result.labels.insert(uid.clone(), label);
                }
            }
        }
        judge_source = format!("external:{command}");
    }

    // 3. Score per metric per mode (per-question values, then mean).
    let mut per_question: HashMap<(String, &'static str), Vec<Option<f64>>> = HashMap::new();
    for spec in &metrics {
        for mode in &modes {
            let values: Vec<Option<f64>> = results
                .iter()
                .map(|result| {
                    // Relevance universe for recall/IDCG = pooled candidates UNION every labeled
                    // doc. For qrels this includes judged-relevant docs no retriever returned (so
                    // recall/nDCG can't look perfect when a retriever missed gold); for an external
                    // judge it equals the pool (the judge only labels pooled candidates).
                    let mut universe: HashSet<String> = result
                        .pool
                        .iter()
                        .map(|candidate| candidate.uid.clone())
                        .collect();
                    universe.extend(result.labels.keys().cloned());
                    let universe: Vec<String> = universe.into_iter().collect();
                    let empty = Vec::new();
                    let top = result.per_mode.get(mode.as_str()).unwrap_or(&empty);
                    compute_eval_metric(spec, top, &result.labels, &universe, args.rel_min)
                })
                .collect();
            per_question.insert((spec.name.clone(), mode.as_str()), values);
        }
    }

    let mut metrics_out = serde_json::Map::new();
    for mode in &modes {
        let mut mode_metrics = serde_json::Map::new();
        for spec in &metrics {
            let values = &per_question[&(spec.name.clone(), mode.as_str())];
            let value = mean_of(values).map(|v| (v * 1000.0).round() / 1000.0);
            mode_metrics.insert(
                spec.name.clone(),
                value.map(Value::from).unwrap_or(Value::Null),
            );
        }
        metrics_out.insert(mode.as_str().to_owned(), Value::Object(mode_metrics));
    }

    // 4. Optional bootstrap CIs for between-mode deltas on each metric.
    let bootstrap_out = if args.bootstrap > 0 && modes.len() >= 2 {
        let mut entries = Vec::new();
        for spec in &metrics {
            for i in 0..modes.len() {
                for j in (i + 1)..modes.len() {
                    let a = modes[i].as_str();
                    let b = modes[j].as_str();
                    let (point, lo, hi) = bootstrap_delta_ci(
                        &per_question[&(spec.name.clone(), a)],
                        &per_question[&(spec.name.clone(), b)],
                        args.bootstrap,
                    );
                    let round = |x: f64| (x * 1000.0).round() / 1000.0;
                    entries.push(json!({
                        "metric": spec.name,
                        "a": a,
                        "b": b,
                        "delta": round(point),
                        "ci_lo": round(lo),
                        "ci_hi": round(hi),
                        "significant": !(lo <= 0.0 && 0.0 <= hi),
                    }));
                }
            }
        }
        json!({ "resamples": args.bootstrap, "method": "question-resampled percentile", "deltas": entries })
    } else {
        Value::Null
    };

    let total_pool: usize = results.iter().map(|result| result.pool.len()).sum();
    let (env_lexical, env_dense) = rrf_weights();
    Ok(json!({
        "schema_version": SCHEMA_VERSION,
        "kind": "eval_run_benchmark",
        "questions": results.len(),
        "modes": modes.iter().map(|mode| mode.as_str()).collect::<Vec<_>>(),
        "group_by": "document",
        "top_k": args.top_k,
        "rel_min": args.rel_min,
        "judge": judge_source,
        "retrieval_options": {
            "rrf_lexical_weight": options.rrf_lexical_weight.unwrap_or(env_lexical),
            "rrf_dense_weight": options.rrf_dense_weight.unwrap_or(env_dense),
            // Report only what the eval requested: `null` means no per-request override, so retrieval
            // used the index manifest's recommended probes (else the fixed fallback). Echoing a literal
            // `4` here would misreport runs against a corpus-sized index whose default is much higher.
            "ivfflat_probes": options.ivfflat_probes,
        },
        "pool": { "total_pairs": total_pool },
        "metrics": Value::Object(metrics_out),
        "bootstrap": bootstrap_out,
    }))
}

/// Sweep one hybrid retrieval parameter against a fixture and report the metric-maximizing value.
/// Re-runs `eval_run_payload` (hybrid only) per sweep point with request-scoped options.
pub(crate) fn eval_tune_payload(
    args: EvalTuneArgs,
    index_dir: Option<&Path>,
) -> Result<Value, ErrorObject> {
    let (param, range) = args.sweep.split_once('=').ok_or_else(|| {
        ErrorObject::bad_input("--sweep must be PARAM=start:stop:step (e.g. rrf-dense=0.1:1.5:0.1)")
    })?;
    let bounds: Vec<&str> = range.split(':').collect();
    if bounds.len() != 3 {
        return Err(ErrorObject::bad_input(
            "--sweep range must be start:stop:step",
        ));
    }
    let parse = |s: &str| -> Result<f64, ErrorObject> {
        s.trim()
            .parse::<f64>()
            .map_err(|_| ErrorObject::bad_input(format!("--sweep value `{s}` is not a number")))
    };
    let (start, stop, step) = (parse(bounds[0])?, parse(bounds[1])?, parse(bounds[2])?);
    if !start.is_finite() || !stop.is_finite() || !step.is_finite() {
        return Err(ErrorObject::bad_input(
            "--sweep start/stop/step must be finite",
        ));
    }
    if step <= 0.0 || stop < start {
        return Err(ErrorObject::bad_input(
            "--sweep requires step > 0 and stop >= start",
        ));
    }
    if !matches!(param, "rrf-dense" | "rrf-lexical" | "probes") {
        return Err(ErrorObject::bad_input(format!(
            "unknown sweep param `{param}`; expected rrf-dense, rrf-lexical, or probes"
        )));
    }
    if param == "probes" && [start, stop, step].iter().any(|value| value.fract() != 0.0) {
        return Err(ErrorObject::bad_input(
            "--sweep probes=start:stop:step requires integer start/stop/step",
        ));
    }
    if param == "probes" && start < 1.0 {
        return Err(ErrorObject::bad_input("--sweep probes start must be >= 1"));
    }

    let mut values = Vec::new();
    let mut value = start;
    while value <= stop + 1e-9 {
        values.push((value * 1e6).round() / 1e6);
        value += step;
    }
    if values.is_empty() {
        return Err(ErrorObject::bad_input("--sweep produced no values"));
    }

    let mut points = Vec::new();
    for value in &values {
        let options = match param {
            "rrf-dense" => RetrievalOptions {
                rrf_dense_weight: Some(*value),
                ..Default::default()
            },
            "rrf-lexical" => RetrievalOptions {
                rrf_lexical_weight: Some(*value),
                ..Default::default()
            },
            // probes
            _ => RetrievalOptions {
                ivfflat_probes: Some(value.max(1.0) as u32),
                ..Default::default()
            },
        };
        let run_args = EvalRunArgs {
            questions: args.questions.clone(),
            qrels: args.qrels.clone(),
            judge_cmd: args.judge_cmd.clone(),
            modes: "hybrid".to_owned(),
            metrics: args.metric.clone(),
            top_k: args.top_k,
            rel_min: args.rel_min,
            bootstrap: 0,
            out: None,
        };
        let result = eval_run_payload(run_args, options, index_dir)?;
        let metric_value = result["metrics"]["hybrid"][&args.metric].as_f64();
        points.push(json!({ "value": value, "metric": metric_value }));
    }

    let best = points
        .iter()
        .filter(|point| point["metric"].is_f64())
        .max_by(|a, b| {
            a["metric"]
                .as_f64()
                .partial_cmp(&b["metric"].as_f64())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .cloned()
        .unwrap_or(Value::Null);

    Ok(json!({
        "schema_version": SCHEMA_VERSION,
        "kind": "eval_tune",
        "mode": "hybrid",
        "sweep": { "param": param, "start": start, "stop": stop, "step": step },
        "metric": args.metric,
        "points": points,
        "best": best,
        "note": "Re-opens the index per sweep point; query-readiness is cached after the first."
    }))
}

pub(crate) fn eval_phase1_payload(req: EvalPhase1Request) -> Result<Value, ErrorObject> {
    if !req.list && req.top_k == 0 {
        return Err(ErrorObject::bad_input(
            "eval phase1 --top-k must be at least 1 when executing fixtures",
        ));
    }

    let fixtures = selected_phase1_eval_fixtures(req.include_dev);
    let fixture_summary = phase1_eval_fixture_summary();
    if req.list {
        return Ok(json!({
            "schema_version": SCHEMA_VERSION,
            "command": "eval phase1",
            "action": "list",
            "include_dev": req.include_dev,
            "fixture_count": fixtures.len(),
            "eval_fixtures": fixture_summary,
            "fixtures": fixtures,
        }));
    }

    let index_dir = req.index_dir.as_deref();
    let mut results = Vec::with_capacity(fixtures.len());
    for fixture in &fixtures {
        results.push(eval_phase1_fixture_result(
            fixture, req.mode, req.top_k, index_dir,
        )?);
    }
    let passed = results
        .iter()
        .filter(|result| result["passed"].as_bool() == Some(true))
        .count();
    let failed = results.len().saturating_sub(passed);
    let retrieval_mode: RetrievalMode = req.mode.into();

    Ok(json!({
        "schema_version": SCHEMA_VERSION,
        "command": "eval phase1",
        "action": "run",
        "include_dev": req.include_dev,
        "retrieval_mode": retrieval_mode.as_str(),
        "top_k": req.top_k,
        "eval_fixtures": fixture_summary,
        "summary": {
            "fixture_count": results.len(),
            "passed": passed,
            "failed": failed,
            "all_passed": failed == 0,
        },
        "results": results,
    }))
}

pub(crate) fn selected_phase1_eval_fixtures(include_dev: bool) -> Vec<LegalRetrievalFixture> {
    if include_dev {
        phase1_eval_fixtures()
    } else {
        phase1_release_candidate_fixtures()
    }
}

pub(crate) fn eval_phase1_fixture_result(
    fixture: &LegalRetrievalFixture,
    mode: CliSearchMode,
    top_k: u32,
    index_dir: Option<&Path>,
) -> Result<Value, ErrorObject> {
    let search_result = search_payload(SearchRequest {
        query: fixture.query.clone(),
        kind: CliKind::Code,
        mode,
        format: CliOutputFormat::Detailed,
        group_by: CliGroupBy::Chunk,
        top_k,
        cursor: None,
        as_of: fixture.as_of.clone(),
        rrf_lexical_weight: None,
        rrf_dense_weight: None,
        probes: None,
        court: None,
        formation: None,
        publication: None,
        decided_from: None,
        decided_to: None,
        zone: None,
        authority_weight: None,
        index_dir: index_dir.map(Path::to_path_buf),
    });

    match search_result {
        Ok(search) => Ok(eval_phase1_fixture_search_result(fixture, search)),
        Err(error) if error.code == ErrorCode::NoResults => Ok(json!({
            "id": fixture.id.as_str(),
            "tier": &fixture.tier,
            "category": fixture.category.as_str(),
            "query": fixture.query.as_str(),
            "as_of": fixture.as_of.as_deref(),
            "expected_ids": &fixture.expected_ids,
            "allowed_alternates": &fixture.allowed_alternates,
            "status": "fail",
            "passed": false,
            "best_expected_rank": null,
            "best_allowed_alternate_rank": null,
            "matched_document_id": null,
            "candidate_count": 0,
            "top_document_ids": [],
            "error": error,
        })),
        Err(error) => Err(error),
    }
}

pub(crate) fn eval_phase1_fixture_search_result(
    fixture: &LegalRetrievalFixture,
    search: Value,
) -> Value {
    let expected_ids = fixture
        .expected_ids
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let allowed_alternates = fixture
        .allowed_alternates
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    let candidates = search["candidates"].as_array().cloned().unwrap_or_default();
    let mut top_document_ids = Vec::with_capacity(candidates.len());
    let mut best_expected_rank = None::<usize>;
    let mut best_allowed_alternate_rank = None::<usize>;
    let mut matched_document_id = None::<String>;

    for candidate in &candidates {
        let Some(document_id) = candidate["document_id"].as_str() else {
            continue;
        };
        top_document_ids.push(document_id.to_owned());
        let rank = top_document_ids.len();
        if best_expected_rank.is_none() && expected_ids.contains(document_id) {
            best_expected_rank = Some(rank);
            matched_document_id = Some(document_id.to_owned());
        }
        if best_allowed_alternate_rank.is_none() && allowed_alternates.contains(document_id) {
            best_allowed_alternate_rank = Some(rank);
            matched_document_id.get_or_insert_with(|| document_id.to_owned());
        }
    }

    let status = if best_expected_rank.is_some() {
        "pass"
    } else if best_allowed_alternate_rank.is_some() {
        "pass_allowed_alternate"
    } else {
        "fail"
    };

    json!({
        "id": fixture.id.as_str(),
        "tier": &fixture.tier,
        "category": fixture.category.as_str(),
        "query": fixture.query.as_str(),
        "as_of": fixture.as_of.as_deref(),
        "expected_ids": &fixture.expected_ids,
        "allowed_alternates": &fixture.allowed_alternates,
        "status": status,
        "passed": status != "fail",
        "best_expected_rank": best_expected_rank,
        "best_allowed_alternate_rank": best_allowed_alternate_rank,
        "matched_document_id": matched_document_id,
        "candidate_count": candidates.len(),
        "top_document_ids": top_document_ids,
        "search": {
            "retrieval_mode": search["retrieval_mode"].clone(),
            "pagination": search["pagination"].clone(),
            "diagnostics": search["diagnostics"]["retrieval"].clone(),
        }
    })
}
