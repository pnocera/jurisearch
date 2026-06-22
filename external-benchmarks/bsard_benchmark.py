#!/usr/bin/env python3
# /// script
# dependencies = ["datasets", "huggingface-hub", "numpy", "requests"]
# ///
"""Run the BSARD external expert-annotated retrieval benchmark.

The output JSON is meant to be consumed by:

    JURISEARCH_PHASE1_EXTERNAL_BENCHMARK=<artifact.json> jurisearch status

The benchmark is intentionally external to the Rust CLI. It evaluates:
- a local Python BM25 implementation;
- dense retrieval through an OpenAI-compatible embeddings endpoint;
- RRF hybrid fusion matching jurisearch's BM25+dense rank-fusion shape.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import re
import sys
import time
from collections import Counter, defaultdict
from dataclasses import dataclass
from pathlib import Path
from typing import Any

os.environ.setdefault("USE_TORCH", "0")

import numpy as np
import requests
from datasets import load_dataset
from huggingface_hub import HfApi


DATASET_ID = "maastrichtlawtech/bsard"
DATASET_LICENSE = "cc-by-nc-sa-4.0"
DATASET_JURISDICTION = "belgium"
USAGE_SCOPE = "eval_only"
CLAIM_SCOPE = (
    "external expert-annotated French-language statutory retrieval benchmark, "
    "not France-LEGI human-reviewed gold"
)
APPLICABILITY = (
    "BSARD contains French-language statutory retrieval questions labeled by "
    "legal experts against Belgian statutory articles. It is used as a proxy "
    "for French-language statutory retrieval quality and does not prove "
    "France-LEGI human-reviewed gold quality."
)

TOKEN_RE = re.compile(r"[0-9A-Za-zÀ-ÖØ-öø-ÿ]+")
STOPWORDS = {
    "a",
    "au",
    "aux",
    "avec",
    "ce",
    "ces",
    "dans",
    "de",
    "des",
    "du",
    "elle",
    "en",
    "est",
    "et",
    "il",
    "la",
    "le",
    "les",
    "leur",
    "leurs",
    "lors",
    "ou",
    "par",
    "pas",
    "peut",
    "pour",
    "qu",
    "que",
    "qui",
    "se",
    "ses",
    "son",
    "sur",
    "un",
    "une",
}


@dataclass(frozen=True)
class Document:
    doc_id: int
    text: str
    reference: str


@dataclass(frozen=True)
class Query:
    query_id: int
    text: str
    relevant_ids: tuple[int, ...]
    category: str


class BM25Index:
    def __init__(self, documents: list[Document], k1: float = 1.5, b: float = 0.75) -> None:
        self.documents = documents
        self.k1 = k1
        self.b = b
        self.doc_lengths: list[int] = []
        self.postings: dict[str, list[tuple[int, int]]] = defaultdict(list)
        document_frequency: Counter[str] = Counter()
        for index, document in enumerate(documents):
            terms = tokenize(document.text)
            counts = Counter(terms)
            self.doc_lengths.append(len(terms))
            document_frequency.update(counts.keys())
            for term, frequency in counts.items():
                self.postings[term].append((index, frequency))
        self.average_doc_length = sum(self.doc_lengths) / max(1, len(self.doc_lengths))
        total = len(documents)
        self.idf = {
            term: math.log(1.0 + (total - frequency + 0.5) / (frequency + 0.5))
            for term, frequency in document_frequency.items()
        }

    def rank(self, query_text: str, limit: int) -> list[int]:
        scores: dict[int, float] = defaultdict(float)
        for term in tokenize(query_text):
            idf = self.idf.get(term)
            if idf is None:
                continue
            for document_index, term_frequency in self.postings[term]:
                doc_length = self.doc_lengths[document_index]
                denominator = term_frequency + self.k1 * (
                    1.0 - self.b + self.b * doc_length / self.average_doc_length
                )
                scores[document_index] += idf * (term_frequency * (self.k1 + 1.0)) / denominator
        ranked = sorted(scores.items(), key=lambda item: (-item[1], self.documents[item[0]].doc_id))
        return [self.documents[index].doc_id for index, _score in ranked[:limit]]


def tokenize(text: str) -> list[str]:
    return [
        token.lower()
        for token in TOKEN_RE.findall(text)
        if len(token) > 1 and token.lower() not in STOPWORDS
    ]


def corpus_text(row: dict[str, Any]) -> str:
    parts = [
        str(row.get("reference") or ""),
        str(row.get("description") or ""),
        str(row.get("article") or ""),
    ]
    return "\n".join(part for part in parts if part.strip())


def question_text(row: dict[str, Any]) -> str:
    extra = row.get("extra_description")
    if extra:
        return f"{row['question']}\n{extra}"
    return str(row["question"])


def parse_article_ids(value: Any) -> tuple[int, ...]:
    if value is None:
        return ()
    if isinstance(value, (list, tuple)):
        return tuple(int(item) for item in value)
    return tuple(int(item.strip()) for item in str(value).split(",") if item.strip())


def load_bsard(
    revision: str | None,
    question_split: str,
    limit_corpus: int | None,
    limit_questions: int | None,
) -> tuple[list[Document], list[Query], str]:
    resolved_revision = resolve_dataset_revision(revision)
    load_revision = revision or resolved_revision
    corpus_split = "corpus" if limit_corpus is None else f"corpus[:{limit_corpus}]"
    questions_split = question_split if limit_questions is None else f"{question_split}[:{limit_questions}]"
    corpus_rows = load_dataset(DATASET_ID, "corpus", split=corpus_split, revision=load_revision)
    question_rows = load_dataset(DATASET_ID, "questions", split=questions_split, revision=load_revision)
    documents = [
        Document(
            doc_id=int(row["id"]),
            text=corpus_text(row),
            reference=str(row.get("reference") or row["id"]),
        )
        for row in corpus_rows
    ]
    available_ids = {document.doc_id for document in documents}
    queries = []
    for row in question_rows:
        relevant_ids = tuple(doc_id for doc_id in parse_article_ids(row["article_ids"]) if doc_id in available_ids)
        if not relevant_ids:
            continue
        queries.append(
            Query(
                query_id=int(row["id"]),
                text=question_text(row),
                relevant_ids=relevant_ids,
                category=str(row.get("category") or "unknown"),
            )
        )
    return documents, queries, resolved_revision


def resolve_dataset_revision(revision: str | None) -> str:
    if revision:
        return revision
    try:
        info = HfApi().dataset_info(DATASET_ID)
        if info.sha:
            return info.sha
    except Exception as error:  # noqa: BLE001 - best-effort metadata only
        raise RuntimeError(f"could not resolve dataset revision for {DATASET_ID}: {error}") from error
    raise RuntimeError(f"could not resolve dataset revision for {DATASET_ID}")


def embed_texts(
    texts: list[str],
    base_url: str,
    model: str,
    api_key: str | None,
    batch_size: int,
    timeout: float,
) -> np.ndarray:
    url = base_url.rstrip("/") + "/embeddings"
    vectors: list[list[float]] = []
    headers = {"Content-Type": "application/json"}
    if api_key:
        headers["Authorization"] = f"Bearer {api_key}"
    for start in range(0, len(texts), batch_size):
        batch = texts[start : start + batch_size]
        for attempt in range(1, 5):
            response = requests.post(
                url,
                headers=headers,
                json={"model": model, "input": batch},
                timeout=timeout,
            )
            if response.status_code in {429, 500, 502, 503, 504} and attempt < 4:
                wait_seconds = min(30.0, 2.0**attempt)
                print(
                    f"embedding batch {start}-{start + len(batch)} got HTTP {response.status_code}; retrying in {wait_seconds:.1f}s",
                    file=sys.stderr,
                )
                time.sleep(wait_seconds)
                continue
            response.raise_for_status()
            break
        data = response.json()["data"]
        data = sorted(data, key=lambda item: item.get("index", 0))
        if len(data) != len(batch):
            raise RuntimeError(f"endpoint returned {len(data)} vectors for {len(batch)} inputs")
        vectors.extend(item["embedding"] for item in data)
        print(f"embedded {min(start + len(batch), len(texts))}/{len(texts)}", file=sys.stderr)
    array = np.asarray(vectors, dtype=np.float32)
    norms = np.linalg.norm(array, axis=1, keepdims=True)
    norms[norms == 0.0] = 1.0
    return array / norms


def cache_key(args: argparse.Namespace, documents: list[Document], queries: list[Query], revision: str) -> str:
    digest = hashlib.sha256()
    digest.update(DATASET_ID.encode())
    digest.update(revision.encode())
    digest.update(args.base_url.encode())
    digest.update(args.model.encode())
    digest.update(str(args.max_input_chars).encode())
    digest.update(str(args.limit_corpus).encode())
    digest.update(str(args.limit_questions).encode())
    for document in documents:
        digest.update(str(document.doc_id).encode())
    for query in queries:
        digest.update(str(query.query_id).encode())
    return digest.hexdigest()[:16]


def load_or_embed(
    args: argparse.Namespace,
    documents: list[Document],
    queries: list[Query],
    revision: str,
) -> tuple[np.ndarray, np.ndarray]:
    cache_dir = Path(args.cache_dir).expanduser()
    cache_dir.mkdir(parents=True, exist_ok=True)
    cache_path = cache_dir / f"bsard-{safe_slug(args.model)}-{cache_key(args, documents, queries, revision)}.npz"
    if cache_path.is_file() and not args.no_cache:
        cached = np.load(cache_path)
        print(f"using embedding cache {cache_path}", file=sys.stderr)
        return cached["document_embeddings"], cached["query_embeddings"]

    api_key = os.environ.get(args.api_key_env) if args.api_key_env else None
    if args.api_key_env and not api_key:
        raise RuntimeError(f"{args.api_key_env} is not set")
    document_embeddings = embed_texts(
        [bounded_input(document.text, args.max_input_chars) for document in documents],
        args.base_url,
        args.model,
        api_key,
        args.embed_batch_size,
        args.timeout,
    )
    query_embeddings = embed_texts(
        [bounded_input(query.text, args.max_input_chars) for query in queries],
        args.base_url,
        args.model,
        api_key,
        args.embed_batch_size,
        args.timeout,
    )
    np.savez_compressed(
        cache_path,
        document_ids=np.asarray([document.doc_id for document in documents], dtype=np.int64),
        query_ids=np.asarray([query.query_id for query in queries], dtype=np.int64),
        document_embeddings=document_embeddings,
        query_embeddings=query_embeddings,
    )
    print(f"wrote embedding cache {cache_path}", file=sys.stderr)
    return document_embeddings, query_embeddings


def dense_rankings(
    documents: list[Document],
    queries: list[Query],
    document_embeddings: np.ndarray,
    query_embeddings: np.ndarray,
    limit: int,
) -> dict[int, list[int]]:
    doc_ids = np.asarray([document.doc_id for document in documents])
    rankings: dict[int, list[int]] = {}
    for query, query_embedding in zip(queries, query_embeddings, strict=True):
        scores = document_embeddings @ query_embedding
        if limit < len(scores):
            top_indexes = np.argpartition(scores, -limit)[-limit:]
        else:
            top_indexes = np.arange(len(scores))
        top_indexes = sorted(top_indexes, key=lambda index: (-float(scores[index]), int(doc_ids[index])))
        rankings[query.query_id] = [int(doc_ids[index]) for index in top_indexes[:limit]]
    return rankings


def hybrid_rankings(
    lexical: dict[int, list[int]],
    dense: dict[int, list[int]],
    queries: list[Query],
    limit: int,
    rrf_k: float,
) -> dict[int, list[int]]:
    rankings = {}
    for query in queries:
        scores: defaultdict[int, float] = defaultdict(float)
        for rank, doc_id in enumerate(lexical.get(query.query_id, []), 1):
            scores[doc_id] += 1.0 / (rrf_k + rank)
        for rank, doc_id in enumerate(dense.get(query.query_id, []), 1):
            scores[doc_id] += 1.0 / (rrf_k + rank)
        ranked = sorted(scores.items(), key=lambda item: (-item[1], item[0]))
        rankings[query.query_id] = [doc_id for doc_id, _score in ranked[:limit]]
    return rankings


def evaluate_rankings(queries: list[Query], rankings: dict[int, list[int]], k: int) -> dict[str, Any]:
    per_query = []
    reciprocal_ranks = []
    recalls = []
    ndcgs = []
    for query in queries:
        ranking = rankings.get(query.query_id, [])[:k]
        relevant = set(query.relevant_ids)
        first_rank = next((rank for rank, doc_id in enumerate(ranking, 1) if doc_id in relevant), None)
        retrieved_relevant = len(relevant.intersection(ranking))
        recalls.append(retrieved_relevant / max(1, len(relevant)))
        reciprocal_ranks.append(1.0 / first_rank if first_rank else 0.0)
        relevance = [1 if doc_id in relevant else 0 for doc_id in ranking]
        ndcg = dcg(relevance) / ideal_dcg(min(len(relevant), k))
        ndcgs.append(ndcg)
        per_query.append(
            {
                "id": query.query_id,
                "category": query.category,
                "relevant_ids": list(query.relevant_ids),
                "best_rank": first_rank,
                "retrieved_relevant": retrieved_relevant,
                "top_ids": ranking[:10],
            }
        )
    count = max(1, len(queries))
    return {
        f"recall_at_{k}": sum(recalls) / count,
        f"success_at_{k}": sum(1.0 if item["best_rank"] else 0.0 for item in per_query) / count,
        f"mrr_at_{k}": sum(reciprocal_ranks) / count,
        f"ndcg_at_{k}": sum(ndcgs) / count,
        "query_count": len(queries),
        "misses": [item for item in per_query if item["best_rank"] is None][:25],
        "per_query_sample": per_query[:25],
    }


def dcg(relevance: list[int]) -> float:
    return sum(score / math.log2(index + 2) for index, score in enumerate(relevance))


def ideal_dcg(relevant_count: int) -> float:
    if relevant_count <= 0:
        return 1.0
    return dcg([1] * relevant_count)


def safe_slug(value: str) -> str:
    return re.sub(r"[^A-Za-z0-9_.-]+", "-", value).strip("-").lower() or "model"


def bounded_input(text: str, max_input_chars: int | None) -> str:
    if max_input_chars is None or max_input_chars <= 0 or len(text) <= max_input_chars:
        return text
    return text[:max_input_chars]


def build_artifact(
    args: argparse.Namespace,
    revision: str,
    documents: list[Document],
    queries: list[Query],
    metrics: dict[str, Any],
    embedding_dimension: int,
    elapsed_seconds: float,
) -> dict[str, Any]:
    k = args.k
    thresholds = {
        f"hybrid_recall_at_{k}_min": args.min_hybrid_recall_at_k,
        f"hybrid_ndcg_at_{k}_min": args.min_hybrid_ndcg_at_k,
        f"hybrid_mrr_at_{k}_min": args.min_hybrid_mrr_at_k,
    }
    hybrid = metrics["hybrid"]
    passed = (
        hybrid[f"recall_at_{k}"] >= args.min_hybrid_recall_at_k
        and hybrid[f"ndcg_at_{k}"] >= args.min_hybrid_ndcg_at_k
        and hybrid[f"mrr_at_{k}"] >= args.min_hybrid_mrr_at_k
    )
    state = "passed" if passed else "failed"
    return {
        "schema_version": 1,
        "kind": "phase1_external_expert_benchmark",
        "state": state,
        "generated_at_unix": int(time.time()),
        "dataset": {
            "id": DATASET_ID,
            "revision": revision,
            "question_split": args.question_split,
            "jurisdiction": DATASET_JURISDICTION,
            "usage_scope": USAGE_SCOPE,
            "license": DATASET_LICENSE,
            "corpus_documents": len(documents),
            "questions": len(queries),
            "limit_corpus": args.limit_corpus,
            "limit_questions": args.limit_questions,
        },
        "claim_scope": CLAIM_SCOPE,
        "applicability": APPLICABILITY,
        "embedding": {
            "base_url_class": classify_base_url(args.base_url),
            "fingerprint_model": "bge-m3",
            "request_model": args.model,
            "dimension": embedding_dimension,
            "normalize": True,
            "max_input_chars": args.max_input_chars,
        },
        "thresholds": thresholds,
        "metrics": metrics,
        "evidence": [
            str(Path(args.out)),
            "external-benchmarks/bsard_benchmark.py",
            "work/03-implementation/02-evidence/2026-06-22-external-expert-benchmark-gate.md",
        ],
        "elapsed_seconds": round(elapsed_seconds, 3),
    }


def classify_base_url(base_url: str) -> str:
    if "127.0.0.1" in base_url or "localhost" in base_url:
        return "local_loopback"
    return "hosted"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--dataset-revision", default=None)
    parser.add_argument("--question-split", default="test")
    parser.add_argument("--limit-corpus", type=int)
    parser.add_argument("--limit-questions", type=int)
    parser.add_argument("--base-url", required=True)
    parser.add_argument("--model", default="baai/bge-m3")
    parser.add_argument("--api-key-env", default="OPENROUTER_API_KEY")
    parser.add_argument("--embed-batch-size", type=int, default=32)
    parser.add_argument("--timeout", type=float, default=180.0)
    parser.add_argument("--max-input-chars", type=int, default=24000)
    parser.add_argument("--k", type=int, default=20)
    parser.add_argument("--lexical-limit", type=int, default=200)
    parser.add_argument("--dense-limit", type=int, default=200)
    parser.add_argument("--rrf-k", type=float, default=60.0)
    parser.add_argument("--min-hybrid-recall-at-k", type=float, default=0.75)
    parser.add_argument("--min-hybrid-ndcg-at-k", type=float, default=0.60)
    parser.add_argument("--min-hybrid-mrr-at-k", type=float, default=0.50)
    parser.add_argument(
        "--cache-dir",
        default=os.environ.get("XDG_CACHE_HOME", str(Path.home() / ".cache"))
        + "/jurisearch/benchmarks",
    )
    parser.add_argument("--no-cache", action="store_true")
    parser.add_argument("--out", required=True)
    args = parser.parse_args()

    started = time.time()
    documents, queries, revision = load_bsard(
        args.dataset_revision,
        args.question_split,
        args.limit_corpus,
        args.limit_questions,
    )
    if not documents:
        raise RuntimeError("BSARD corpus is empty")
    if not queries:
        raise RuntimeError("BSARD question set is empty after filtering qrels to the loaded corpus")

    print(
        f"loaded BSARD corpus={len(documents)} questions={len(queries)} revision={revision}",
        file=sys.stderr,
    )
    bm25 = BM25Index(documents)
    lexical = {query.query_id: bm25.rank(query.text, args.lexical_limit) for query in queries}
    document_embeddings, query_embeddings = load_or_embed(args, documents, queries, revision)
    dense = dense_rankings(documents, queries, document_embeddings, query_embeddings, args.dense_limit)
    hybrid = hybrid_rankings(lexical, dense, queries, args.k, args.rrf_k)

    metrics = {
        "bm25": evaluate_rankings(queries, lexical, args.k),
        "dense": evaluate_rankings(queries, dense, args.k),
        "hybrid": evaluate_rankings(queries, hybrid, args.k),
    }
    artifact = build_artifact(
        args,
        revision,
        documents,
        queries,
        metrics,
        int(document_embeddings.shape[1]),
        time.time() - started,
    )
    out = Path(args.out)
    out.parent.mkdir(parents=True, exist_ok=True)
    out.write_text(json.dumps(artifact, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(artifact, ensure_ascii=False, indent=2))
    print(f"wrote {out}", file=sys.stderr)
    return 0 if artifact["state"] == "passed" else 2


if __name__ == "__main__":
    raise SystemExit(main())
