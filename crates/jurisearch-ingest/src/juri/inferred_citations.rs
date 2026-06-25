//! Heuristic inferred legislation-citation edges from decision body (regex-driven).

use super::*;

/// Max inferred citation edges kept per decision. Decisions citing more distinct articles are rare;
/// this bounds graph bloat while covering the common case.
pub(crate) const MAX_INFERRED_CITATION_EDGES: usize = 64;

// Matches "article(s) <num>" where <num> is an optional L/R/D prefix plus a dotted/hyphenated
// article number (e.g. "L. 1242-14", "R.1332-2", "1014", "1240"). Stops the number at the first
// separator. Case-insensitive.
pub(crate) static ARTICLE_CITATION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\barticles?\s+(?P<num>(?:[LRD]\.?\s?)?\d+(?:[-\u{2011}]\d+)*)")
        .expect("valid article citation regex")
});

// Within the short window after an article number (and before the next "article" keyword), detects
// "du [même] code <name>" so a reference can be tied to a statutory code (the signal that
// distinguishes a LEGI article citation from a treaty/convention article).
pub(crate) static CODE_HINT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\bdu\s+(?P<same>m[êe]me\s+)?code\b(?P<name>[^.;,\n)]{0,48})")
        .expect("valid code hint regex")
});

pub(crate) static NEXT_ARTICLE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\barticles?\b").expect("valid next-article regex"));

/// Parse lower-trust article-citation references from the decision body text into `inferred` graph
/// edges, distinct from official `publisher` `LIEN` edges. To stay precise (and avoid matching
/// treaty/convention articles), a reference is kept only when it carries an `L`/`R`/`D` statutory
/// prefix OR is followed by a "du [même] code …" hint. Targets are NOT resolved here (`to_source_uid`
/// stays `None`); the normalized article number + optional code hint are preserved as evidence.
pub(crate) fn build_inferred_citation_edges(decision: &CanonicalDecision) -> Vec<CanonicalGraphEdge> {
    let body = decision.body.as_str();
    let mut edges = Vec::new();
    let mut seen = BTreeSet::new();

    for capture in ARTICLE_CITATION_RE.captures_iter(body) {
        if edges.len() >= MAX_INFERRED_CITATION_EDGES {
            break;
        }
        let whole = capture.get(0).expect("group 0 always present");
        let raw_num = &capture["num"];
        let normalized = normalize_article_number(raw_num);
        if normalized.is_empty() {
            continue;
        }
        let has_statutory_prefix = matches!(normalized.as_bytes().first(), Some(b'L' | b'R' | b'D'));

        // Look just past the number for a "du [même] code …" hint, but stop at the next "article"
        // keyword so a following reference's code is never mis-attributed to this one. The window end
        // is floored to a UTF-8 char boundary so accented French bodies cannot panic the slice.
        let window = &body[whole.end()..char_safe_window_end(body, whole.end(), 80)];
        let tail = match NEXT_ARTICLE_RE.find(window) {
            Some(next) => &window[..next.start()],
            None => window,
        };
        let code_hint = CODE_HINT_RE.captures(tail).map(|code_capture| {
            if code_capture.name("same").is_some() {
                "même code".to_owned()
            } else {
                let name = code_capture["name"].split_whitespace().collect::<Vec<_>>().join(" ");
                format!("code {name}").trim().to_owned()
            }
        });

        if !has_statutory_prefix && code_hint.is_none() {
            continue; // ambiguous bare number (e.g. "article 8 de la convention") — skip.
        }

        let dedup_key = format!("{normalized}|{}", code_hint.as_deref().unwrap_or(""));
        if !seen.insert(dedup_key) {
            continue;
        }

        let source_text = collapse_ws(whole.as_str());
        let mut attributes = vec![GraphEdgeAttribute {
            key: "article_number".to_owned(),
            value: normalized.clone(),
        }];
        if let Some(code_hint) = &code_hint {
            attributes.push(GraphEdgeAttribute {
                key: "code_hint".to_owned(),
                value: code_hint.clone(),
            });
        }

        edges.push(CanonicalGraphEdge {
            edge_id: inferred_edge_id(&decision.document_id, &normalized, code_hint.as_deref()),
            from_document_id: decision.document_id.clone(),
            from_source_uid: decision.source_uid.clone(),
            to_source_uid: None,
            to_document_id: None,
            relation: "cites_article".to_owned(),
            edge_source: "inferred".to_owned(),
            source_tag: "body_citation".to_owned(),
            source_text: Some(source_text),
            source_payload_hash: decision.source_payload_hash.clone(),
            source_archive: decision.source_archive.clone(),
            source_member_path: decision.source_member_path.clone(),
            attributes,
        });
    }

    edges
}

/// Byte offset `max_bytes` after `start`, clamped to the string length and floored to the nearest
/// UTF-8 char boundary at or after `start`. `start` must already be a char boundary (regex match
/// ends are). This keeps body windowing panic-safe on accented French text.
pub(crate) fn char_safe_window_end(text: &str, start: usize, max_bytes: usize) -> usize {
    let mut end = start.saturating_add(max_bytes).min(text.len());
    while end > start && !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}

/// Normalize a raw matched article number ("L. 1242-14" → "L1242-14", "R.1332-2" → "R1332-2").
pub(crate) fn normalize_article_number(raw: &str) -> String {
    let mut normalized = String::new();
    for character in raw.chars() {
        match character {
            'l' | 'L' => normalized.push('L'),
            'r' | 'R' => normalized.push('R'),
            'd' | 'D' => normalized.push('D'),
            c if c.is_ascii_digit() => normalized.push(c),
            '-' | '\u{2011}' => normalized.push('-'),
            _ => {} // drop spaces, dots, etc.
        }
    }
    normalized
}

pub(crate) fn inferred_edge_id(from_document_id: &str, article_number: &str, code_hint: Option<&str>) -> String {
    let evidence = format!(
        "{from_document_id}|inferred|cites_article|{article_number}|{}",
        code_hint.unwrap_or_default()
    );
    let hash = source_payload_hash(evidence.as_bytes());
    let digest = hash.strip_prefix("sha256:").unwrap_or(hash.as_str());
    format!("inferred-edge:{digest}")
}
