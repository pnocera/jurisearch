//! Search-family command schemas (search/compare/fetch/cite/context/related/expand).
//!
//! Returns this domain's entries for the flat `#/schemas/*` map assembled by `compiled_schema()`.

use serde_json::{Map, Value, json};

pub(crate) fn schemas() -> Map<String, Value> {
    let Value::Object(map) = json!({
        "SearchRequest": {
            "required": ["query"],
            "properties": {
                "query": { "type": "string" },
                "kind": { "enum": ["code", "decision", "all"], "default": "all" },
                "mode": { "enum": ["hybrid", "bm25", "dense"], "default": "hybrid" },
                "group_by": { "enum": ["chunk", "document"], "default": "chunk", "description": "Result granularity: one row per passage, or one row per article." },
                "format": { "enum": ["concise", "detailed"], "default": "concise" },
                "top_k": { "type": "integer", "minimum": 1, "default": 10 },
                "cursor": { "type": "string", "description": "Chunk cursor <score>:<chunk_id> or document cursor doc:<score>:<document_id>; must match --group-by." },
                "as_of": { "type": "string", "format": "date" },
                "rrf_lexical_weight": { "type": "number", "minimum": 0, "description": "Per-request hybrid RRF lexical weight (default from env, else 1.0)." },
                "rrf_dense_weight": { "type": "number", "minimum": 0, "description": "Per-request hybrid RRF dense weight (default from env, else 0.3)." },
                "probes": { "type": "integer", "minimum": 1, "maximum": 4096, "description": "Per-request ivfflat.probes for dense ANN (default 4)." },
                "zone": { "enum": ["motivations", "moyens", "dispositif"], "description": "Official Cour de cassation zone scope (case-law only): restrict retrieval to a decision part — `motivations` (reasoning), `moyens` (grounds raised), or `dispositif` (holding). Routes to the coverage-bounded official-zone subsystem (cass+inca only); incompatible with kind=code." }
            }
        },
        "SearchResponse": {
            "properties": {
                "query": { "type": "string" },
                "retrieval_mode": { "enum": ["hybrid", "bm25", "dense"] },
                "group_by": { "enum": ["chunk", "document"] },
                "format": { "enum": ["concise", "detailed"] },
                "expansion_seed_version": { "type": "string" },
                "expanded_terms": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "term": { "type": "string" },
                            "matched_terms": {
                                "type": "array",
                                "items": { "type": "string" }
                            },
                            "source_seed_id": { "type": "string" },
                            "source_label": { "type": "string" },
                            "source_citation": { "type": "string" },
                            "review_status": { "type": "string" },
                            "reviewer": { "type": "string" },
                            "rationale": { "type": "string" }
                        }
                    }
                },
                "as_of": { "type": "string", "format": "date" },
                "limit": { "type": "integer" },
                "scope": {
                    "type": "object",
                    "description": "Present only for `zone` searches: the coverage-bounded official-zone scope (Cour de cassation only).",
                    "properties": {
                        "mode": { "const": "official_zone_retrieval" },
                        "zone": { "enum": ["motivations", "moyens", "dispositif"] },
                        "coverage": { "type": "string" },
                        "zone_accurate": { "type": "boolean" },
                        "indexed_decisions": { "type": ["integer", "null"] },
                        "note": { "type": "string" }
                    }
                },
                "routing": {
                    "type": "object",
                    "description": "Intent-routing audit: how the query was classified and which backend served it.",
                    "properties": {
                        "query_type": { "enum": ["citation", "semantic", "zone"] },
                        "chosen_backend": { "enum": ["hybrid", "bm25", "dense", "structured_citation", "official_zone_retrieval"] },
                        "candidate_count": { "type": "integer" },
                        "fallback_path": { "enum": ["none", "hybrid_fallback"] },
                        "zone": { "enum": ["motivations", "moyens", "dispositif"], "description": "Present only on zone routes." }
                    }
                },
                "pagination": {
                    "type": "object",
                    "properties": {
                        "requested_top_k": { "type": "integer" },
                        "after_cursor": { "type": ["string", "null"] },
                        "returned": { "type": "integer" },
                        "possibly_truncated": { "type": "boolean" },
                        "cursor_supported": { "type": "boolean" },
                        "next_cursor": { "type": ["string", "null"] },
                        "cursor_note": { "type": "string" },
                        "guidance": { "type": ["string", "null"] }
                    }
                },
                "diagnostics": {
                    "type": "object",
                    "properties": {
                        "query_input": { "type": "string" },
                        "lexical_query_text": { "type": ["string", "null"] },
                        "retrieval": {
                            "type": "object",
                            "properties": {
                                "mode": { "enum": ["hybrid", "bm25", "dense"] },
                                "uses_lexical": { "type": "boolean" },
                                "uses_dense": { "type": "boolean" },
                                "lexical_limit": { "type": "integer" },
                                "dense_limit": { "type": "integer" },
                                "query_limit": { "type": "integer" },
                                "embedding_fingerprint": { "type": ["string", "null"] },
                                "kind_filter": { "type": ["string", "null"] },
                                "after_cursor": { "type": ["string", "null"] }
                            }
                        }
                    }
                },
                "candidates": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "chunk_id": { "type": "string" },
                            "best_chunk_id": { "type": "string", "description": "Document grouping: the best-ranked chunk of the article." },
                            "document_id": { "type": "string" },
                            "source": { "type": "string" },
                            "kind": { "type": "string" },
                            "citation": { "type": ["string", "null"] },
                            "title": { "type": ["string", "null"] },
                            "source_url": { "type": ["string", "null"] },
                            "snippet": { "type": "string" },
                            "validity": { "type": "object" },
                            "scores": { "type": "object" },
                            "cursor": { "type": "string" }
                        }
                    }
                }
            }
        },
        "CompareRequest": {
            "required": ["query"],
            "properties": {
                "query": { "type": "string" },
                "kind": { "enum": ["code", "all"], "default": "code" },
                "top_k": { "type": "integer", "minimum": 1, "default": 10 },
                "as_of": { "type": "string", "format": "date" }
            }
        },
        "CompareResponse": {
            "properties": {
                "query": { "type": "string" },
                "as_of": { "type": "string", "format": "date" },
                "kind": { "enum": ["code", "all"] },
                "group_by": { "const": "document" },
                "top_k": { "type": "integer" },
                "modes": {
                    "type": "object",
                    "description": "Per-retriever results keyed by mode (bm25/dense/hybrid).",
                    "additionalProperties": {
                        "type": "object",
                        "properties": { "candidates": { "type": "array" } }
                    }
                },
                "pool": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "document_id": { "type": "string" },
                            "best_chunk_id": { "type": ["string", "null"] },
                            "citation": { "type": ["string", "null"] },
                            "title": { "type": ["string", "null"] },
                            "by_mode": {
                                "type": "object",
                                "description": "Per-mode rank+score for this document; null if a mode did not return it."
                            }
                        }
                    }
                },
                "overlap": {
                    "type": "object",
                    "properties": {
                        "bm25_dense": { "type": "integer" },
                        "bm25_hybrid": { "type": "integer" },
                        "dense_hybrid": { "type": "integer" }
                    }
                },
                "pagination": { "type": "object" }
            }
        },
        "FetchRequest": {
            "required": ["ids"],
            "properties": {
                "ids": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Exact, version-pinned stable IDs (e.g. legi:LEGIARTI...@YYYY-MM-DD)."
                }
            }
        },
        "FetchResponse": {
            "properties": {
                "documents": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "document_id": { "type": "string" },
                            "source": { "type": "string" },
                            "kind": { "type": "string" },
                            "source_uid": { "type": "string" },
                            "version_group": { "type": ["string", "null"] },
                            "citation": { "type": ["string", "null"] },
                            "title": { "type": ["string", "null"] },
                            "body": { "type": "string" },
                            "validity": { "type": "object" },
                            "source_url": { "type": ["string", "null"] },
                            "source_payload_hash": { "type": "string" },
                            "chunks": { "type": "array" }
                        }
                    }
                }
            }
        },
        "CiteRequest": {
            "required": ["cite"],
            "properties": {
                "cite": { "type": "string" },
                "strict": { "type": "boolean", "default": false },
                "online": { "type": "boolean", "default": false },
                "as_of": { "type": "string", "format": "date" }
            }
        },
        "CiteResponse": {
            "properties": {
                "query": { "type": "string" },
                "input_class": {
                    "enum": ["document_id", "legiarti", "legitext", "legiscta", "nor", "free_text_article", "malformed"]
                },
                "normalized": { "type": ["string", "null"] },
                "as_of": { "type": "string", "format": "date" },
                "requested_as_of": { "type": ["string", "null"], "format": "date" },
                "state": {
                    "enum": ["exact", "normalized", "ambiguous", "stale_version", "not_found", "source_unavailable"]
                },
                "local_state": {
                    "enum": ["exact", "normalized", "ambiguous", "stale_version", "not_found", "source_unavailable"]
                },
                "strict": { "type": "boolean" },
                "online": { "type": "object" },
                "match_count": { "type": "integer" },
                "valid_match_count": { "type": "integer" },
                "matches": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "target_type": { "enum": ["document", "metadata_root"] },
                            "document_id": { "type": ["string", "null"] },
                            "metadata_key": { "type": ["string", "null"] },
                            "source": { "type": "string" },
                            "kind": { "type": "string" },
                            "source_uid": { "type": ["string", "null"] },
                            "version_group": { "type": ["string", "null"] },
                            "root_kind": { "type": ["string", "null"] },
                            "parent_source_uid": { "type": ["string", "null"] },
                            "citation": { "type": ["string", "null"] },
                            "title": { "type": ["string", "null"] },
                            "nor": { "type": ["string", "null"] },
                            "validity": { "type": "object" },
                            "valid_on_as_of": { "type": "boolean" },
                            "source_url": { "type": ["string", "null"] },
                            "source_payload_hash": { "type": "string" },
                            "exact_identifier_match": { "type": "boolean" }
                        }
                    }
                }
            }
        },
        "ContextRequest": {
            "required": ["id"],
            "properties": {
                "id": { "type": "string" },
                "siblings": { "type": "boolean", "default": false },
                "as_of": { "type": "string", "format": "date" }
            }
        },
        "ContextResponse": {
            "properties": {
                "id": { "type": "string" },
                "as_of": { "type": ["string", "null"], "format": "date" },
                "requested_as_of": { "type": ["string", "null"], "format": "date" },
                "target": { "type": ["object", "null"] },
                "ancestry": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "depth": { "type": "integer" },
                            "title": { "type": "string" }
                        }
                    }
                },
                "siblings": { "type": "array" },
                "sibling_count": { "type": "integer" },
                "sibling_limit": { "type": "integer" },
                "sibling_truncated": { "type": "boolean" }
            }
        },
        "ExpandRequest": {
            "required": ["query"],
            "properties": {
                "query": { "type": "string" }
            }
        },
        "ExpandResponse": {
            "properties": {
                "query": { "type": "string" },
                "seed_version": { "type": "string" },
                "expanded_terms": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "term": { "type": "string" },
                            "matched_terms": {
                                "type": "array",
                                "items": { "type": "string" }
                            },
                            "source_seed_id": { "type": "string" },
                            "source_label": { "type": "string" },
                            "source_citation": { "type": "string" },
                            "review_status": { "type": "string" },
                            "reviewer": { "type": "string" },
                            "rationale": { "type": "string" }
                        }
                    }
                }
            }
        },
        "RelatedRequest": {
            "required": ["id"],
            "properties": {
                "id": { "type": "string", "description": "Exact, version-pinned document ID." },
                "rel": {
                    "enum": ["cites", "cited_by", "temporal"],
                    "default": "cites",
                    "description": "cites=outgoing citations, cited_by=incoming citations, temporal=version family."
                },
                "limit": { "type": "integer", "minimum": 1, "default": 50 },
                "depth": { "type": "integer", "enum": [1], "default": 1, "description": "Only depth 1 is supported." }
            }
        },
        "RelatedResponse": {
            "properties": {
                "id": { "type": "string" },
                "rel": { "enum": ["cites", "cited_by", "temporal"] },
                "depth": { "type": "integer" },
                "returned": { "type": "integer" },
                "neighbours": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "rel": { "type": "string" },
                            "direction": { "enum": ["outgoing", "incoming"] },
                            "depth": { "type": "integer" },
                            "document": {
                                "type": "object",
                                "properties": {
                                    "document_id": { "type": "string" },
                                    "source_uid": { "type": "string" },
                                    "citation": { "type": ["string", "null"] },
                                    "title": { "type": ["string", "null"] },
                                    "validity": { "type": "object" },
                                    "source_url": { "type": ["string", "null"] }
                                }
                            },
                            "edge": {
                                "type": "object",
                                "properties": {
                                    "edge_id": { "type": "string" },
                                    "edge_kind": { "type": "string" },
                                    "edge_source": { "type": "string" },
                                    "source_tag": { "type": ["string", "null"] },
                                    "attributes": { "type": ["array", "null"] }
                                }
                            },
                            "authority": {
                                "type": "object",
                                "properties": {
                                    "score": { "type": "number" },
                                    "label": { "type": "string" },
                                    "confidence": { "type": "string" },
                                    "reasons": { "type": "array", "items": { "type": "string" } }
                                }
                            }
                        }
                    }
                },
                "pagination": {
                    "type": "object",
                    "properties": {
                        "limit": { "type": "integer" },
                        "possibly_truncated": { "type": "boolean" }
                    }
                }
            }
        },
    }) else {
        unreachable!()
    };
    map
}
