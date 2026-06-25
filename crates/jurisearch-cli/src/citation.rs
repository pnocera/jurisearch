//! Pure citation-target parser shared by `cite` (retrieval) and the France-juris eval path:
//! classify a free-text/identifier citation into a `ParsedCitationTarget` and map it to a
//! storage `CitationLookup`.

use jurisearch_storage::citation::CitationLookup;

#[derive(Debug)]
pub(crate) enum ParsedCitationTarget {
    DocumentId {
        document_id: String,
        source_uid: Option<String>,
    },
    ArticleSourceUid(String),
    TextSourceUid(String),
    SectionSourceUid(String),
    Nor(String),
    FreeTextArticle {
        article_number: String,
        code_hint: Option<String>,
    },
    /// A decision's internal document id (`cass:JURITEXT…` / `jade:CETATEXT…`). Existence-based,
    /// NOT version-validity-based: decisions are dated, not versioned.
    DecisionDocumentId {
        document_id: String,
        source_uid: Option<String>,
    },
    DecisionSourceUid(String),
    DecisionEcli(String),
    DecisionPourvoi(String),
    Malformed {
        normalized: String,
    },
}

impl ParsedCitationTarget {
    pub(crate) fn lookup(&self) -> Option<CitationLookup<'_>> {
        match self {
            Self::DocumentId {
                document_id,
                source_uid,
            } => Some(CitationLookup::DocumentId {
                document_id,
                source_uid: source_uid.as_deref(),
            }),
            Self::ArticleSourceUid(source_uid) => {
                Some(CitationLookup::ArticleSourceUid(source_uid))
            }
            Self::TextSourceUid(source_uid) => Some(CitationLookup::TextSourceUid(source_uid)),
            Self::SectionSourceUid(source_uid) => {
                Some(CitationLookup::SectionSourceUid(source_uid))
            }
            Self::Nor(nor) => Some(CitationLookup::Nor(nor)),
            Self::FreeTextArticle {
                article_number,
                code_hint,
            } => Some(CitationLookup::FreeTextArticle {
                article_number,
                code_hint: code_hint.as_deref(),
            }),
            Self::DecisionDocumentId {
                document_id,
                source_uid,
            } => Some(CitationLookup::DocumentId {
                document_id,
                source_uid: source_uid.as_deref(),
            }),
            Self::DecisionSourceUid(source_uid) => {
                Some(CitationLookup::DecisionSourceUid(source_uid))
            }
            Self::DecisionEcli(ecli) => Some(CitationLookup::DecisionEcli(ecli)),
            Self::DecisionPourvoi(pourvoi) => Some(CitationLookup::DecisionPourvoi(pourvoi)),
            Self::Malformed { .. } => None,
        }
    }

    pub(crate) fn input_class(&self) -> &'static str {
        match self {
            Self::DocumentId { .. } => "document_id",
            Self::ArticleSourceUid(_) => "legiarti",
            Self::TextSourceUid(_) => "legitext",
            Self::SectionSourceUid(_) => "legiscta",
            Self::Nor(_) => "nor",
            Self::FreeTextArticle { .. } => "free_text_article",
            Self::DecisionDocumentId { .. } => "decision_document_id",
            Self::DecisionSourceUid(_) => "decision_id",
            Self::DecisionEcli(_) => "ecli",
            Self::DecisionPourvoi(_) => "pourvoi",
            Self::Malformed { .. } => "malformed",
        }
    }

    /// Whether this target is a jurisprudence decision (existence-based, never version-validity).
    pub(crate) fn is_decision(&self) -> bool {
        matches!(
            self,
            Self::DecisionDocumentId { .. }
                | Self::DecisionSourceUid(_)
                | Self::DecisionEcli(_)
                | Self::DecisionPourvoi(_)
        )
    }

    pub(crate) fn normalized_value(&self) -> Option<&str> {
        match self {
            Self::DocumentId { document_id, .. }
            | Self::DecisionDocumentId { document_id, .. } => Some(document_id),
            Self::ArticleSourceUid(source_uid)
            | Self::TextSourceUid(source_uid)
            | Self::SectionSourceUid(source_uid)
            | Self::Nor(source_uid)
            | Self::DecisionSourceUid(source_uid)
            | Self::DecisionEcli(source_uid)
            | Self::DecisionPourvoi(source_uid) => Some(source_uid),
            Self::FreeTextArticle { article_number, .. } => Some(article_number),
            Self::Malformed { normalized } if !normalized.is_empty() => Some(normalized),
            Self::Malformed { .. } => None,
        }
        .map(String::as_str)
    }
}

pub(crate) fn parse_citation_target(input: &str) -> ParsedCitationTarget {
    let trimmed = input.trim();
    if trimmed.starts_with("legi:") {
        return ParsedCitationTarget::DocumentId {
            document_id: trimmed.to_owned(),
            source_uid: extract_known_source_uid(trimmed, "LEGIARTI"),
        };
    }
    // Decision document_id (e.g. `cass:JURITEXT…`, `jade:CETATEXT…`).
    if let Some((prefix, _)) = trimmed.split_once(':')
        && matches!(prefix, "cass" | "capp" | "inca" | "jade")
    {
        let source_uid = extract_known_source_uid(trimmed, "JURITEXT")
            .or_else(|| extract_known_source_uid(trimmed, "CETATEXT"));
        return ParsedCitationTarget::DecisionDocumentId {
            document_id: trimmed.to_owned(),
            source_uid,
        };
    }
    if let Some(source_uid) = extract_known_source_uid(trimmed, "LEGIARTI") {
        return ParsedCitationTarget::ArticleSourceUid(source_uid);
    }
    if let Some(source_uid) = extract_known_source_uid(trimmed, "LEGITEXT") {
        return ParsedCitationTarget::TextSourceUid(source_uid);
    }
    if let Some(source_uid) = extract_known_source_uid(trimmed, "LEGISCTA") {
        return ParsedCitationTarget::SectionSourceUid(source_uid);
    }
    // Bare decision source-native UID.
    if let Some(source_uid) = extract_known_source_uid(trimmed, "JURITEXT")
        .or_else(|| extract_known_source_uid(trimmed, "CETATEXT"))
    {
        return ParsedCitationTarget::DecisionSourceUid(source_uid);
    }
    // ECLI (e.g. `ECLI:FR:CCASS:2025:AP00683`).
    if trimmed.to_ascii_uppercase().starts_with("ECLI:") {
        return ParsedCitationTarget::DecisionEcli(trimmed.to_ascii_uppercase());
    }
    let normalized = normalize_citation_text(trimmed);
    if let Some(article_number) = parse_article_number(&normalized) {
        return ParsedCitationTarget::FreeTextArticle {
            article_number,
            code_hint: detect_code_hint(&normalized),
        };
    }
    // Pourvoi / numéro d'affaire (e.g. `22-21.812` or `22-21812`).
    if let Some(pourvoi) = parse_pourvoi(trimmed) {
        return ParsedCitationTarget::DecisionPourvoi(pourvoi);
    }
    let compact_upper = trimmed
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(|character| character.to_uppercase())
        .collect::<String>();
    if looks_like_nor(&compact_upper) {
        return ParsedCitationTarget::Nor(compact_upper);
    }
    ParsedCitationTarget::Malformed { normalized }
}

pub(crate) fn extract_known_source_uid(value: &str, prefix: &str) -> Option<String> {
    let upper = value.to_ascii_uppercase();
    let start = upper.find(prefix)?;
    let suffix = upper[start + prefix.len()..]
        .chars()
        .take_while(|character| character.is_ascii_digit())
        .take(12)
        .collect::<String>();
    (suffix.len() == 12).then(|| format!("{prefix}{suffix}"))
}

pub(crate) fn parse_article_number(normalized: &str) -> Option<String> {
    let tokens = normalized.split_whitespace().collect::<Vec<_>>();
    let mut index = 0usize;
    const ARTICLE_PREFIXES: &[&str] = &["l", "lo", "r", "d"];
    while let Some(token) = tokens.get(index) {
        if *token == "article"
            && let Some(candidate) = tokens.get(index + 1)
        {
            if let Some(number) = article_number_token(candidate) {
                return Some(number);
            }
            if ARTICLE_PREFIXES.contains(candidate)
                && let Some(number) = tokens
                    .get(index + 2)
                    .and_then(|candidate| article_number_token(candidate))
            {
                return Some(format!("{candidate}{number}"));
            }
        }
        index += 1;
    }
    None
}

pub(crate) fn article_number_token(candidate: &str) -> Option<String> {
    (candidate
        .chars()
        .any(|character| character.is_ascii_digit())
        && candidate
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-'))
    .then(|| candidate.to_owned())
}

pub(crate) fn detect_code_hint(normalized: &str) -> Option<String> {
    const CODE_HINTS: &[&str] = &[
        "code civil",
        "code penal",
        "code de procedure civile",
        "code de procedure penale",
        "code du travail",
        "code de la consommation",
        "code des assurances",
        "code de commerce",
        "code de l environnement",
        "code de la sante publique",
        "code general des impots",
    ];
    CODE_HINTS
        .iter()
        .find(|hint| contains_normalized_phrase(normalized, hint))
        .map(|hint| (*hint).to_owned())
}

pub(crate) fn contains_normalized_phrase(normalized: &str, phrase: &str) -> bool {
    let normalized = format!(" {normalized} ");
    let phrase = format!(" {phrase} ");
    normalized.contains(&phrase)
}

pub(crate) fn looks_like_nor(value: &str) -> bool {
    let chars = value.chars().collect::<Vec<_>>();
    chars.len() == 12
        && chars[0..4]
            .iter()
            .all(|character| character.is_ascii_alphabetic())
        && chars[4..11]
            .iter()
            .all(|character| character.is_ascii_digit())
        && chars[11].is_ascii_alphabetic()
}

pub(crate) fn normalize_citation_text(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len());
    let mut previous_was_space = true;
    for character in value.chars().flat_map(|character| character.to_lowercase()) {
        let replacement = match character {
            'à' | 'â' | 'ä' => "a",
            'ç' => "c",
            'é' | 'è' | 'ê' | 'ë' => "e",
            'î' | 'ï' => "i",
            'ô' | 'ö' => "o",
            'ù' | 'û' | 'ü' => "u",
            'œ' => "oe",
            'æ' => "ae",
            '-' => {
                normalized.push('-');
                previous_was_space = false;
                continue;
            }
            ascii if ascii.is_ascii_alphanumeric() => {
                normalized.push(ascii);
                previous_was_space = false;
                continue;
            }
            _ => "",
        };
        if !replacement.is_empty() {
            normalized.push_str(replacement);
            previous_was_space = false;
        } else if !previous_was_space {
            normalized.push(' ');
            previous_was_space = true;
        }
    }
    normalized.trim().to_owned()
}

/// Detect a pourvoi / `NUMERO_AFFAIRE` shape — two digits, a dash, then 4–6 digits (optionally
/// dotted, e.g. `22-21.812`, `57-10.110`) — and return it normalized (dots/spaces removed).
/// Conservative to avoid false positives (dates like `2024-01-01` and short forms are rejected).
pub(crate) fn parse_pourvoi(input: &str) -> Option<String> {
    let compact: String = input
        .chars()
        .filter(|character| !matches!(character, '.' | ' '))
        .collect();
    let (left, right) = compact.split_once('-')?;
    if left.len() == 2
        && left.bytes().all(|byte| byte.is_ascii_digit())
        && (4..=6).contains(&right.len())
        && right.bytes().all(|byte| byte.is_ascii_digit())
    {
        Some(compact)
    } else {
        None
    }
}
