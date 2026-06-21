use std::collections::BTreeSet;

use serde::Serialize;

pub const LEGAL_VOCABULARY_SEED_VERSION: &str = "legal-vocabulary-seed:v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExpansionResponse {
    pub query: String,
    pub seed_version: &'static str,
    pub expanded_terms: Vec<ExpandedTerm>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExpandedTerm {
    pub term: &'static str,
    pub matched_terms: Vec<&'static str>,
    pub source_seed_id: &'static str,
    pub source_label: &'static str,
    pub source_citation: &'static str,
    pub review_status: &'static str,
    pub reviewer: &'static str,
    pub rationale: &'static str,
}

#[derive(Debug, Clone, Copy)]
struct ExpansionSeed {
    id: &'static str,
    label: &'static str,
    match_terms: &'static [&'static str],
    expanded_terms: &'static [&'static str],
    source_citation: &'static str,
    review_status: &'static str,
    reviewer: &'static str,
    rationale: &'static str,
}

const EXPANSION_SEEDS: &[ExpansionSeed] = &[
    ExpansionSeed {
        id: "civil-liability-fault-damage",
        label: "Responsabilite civile delictuelle",
        match_terms: &[
            "responsabilite",
            "faute",
            "dommage",
            "reparation",
            "prejudice",
        ],
        expanded_terms: &[
            "responsabilite civile",
            "fait quelconque",
            "faute",
            "dommage",
            "prejudice",
            "reparation du dommage",
            "article 1240",
            "article 1241",
        ],
        source_citation: "Code civil, articles 1240 et 1241",
        review_status: "dev_seed_pending_legal_review",
        reviewer: "pending_legal_domain_review",
        rationale: "Common statutory vocabulary for fault, damage, and civil-liability queries.",
    },
    ExpansionSeed {
        id: "contract-non-performance-damages",
        label: "Inexecution contractuelle",
        match_terms: &[
            "contrat",
            "contractuel",
            "obligation",
            "inexecution",
            "dommages interets",
        ],
        expanded_terms: &[
            "force obligatoire",
            "obligation contractuelle",
            "inexecution du contrat",
            "dommages et interets",
            "article 1103",
            "article 1231-1",
        ],
        source_citation: "Code civil, articles 1103 et 1231-1",
        review_status: "dev_seed_pending_legal_review",
        reviewer: "pending_legal_domain_review",
        rationale: "Core contract-law vocabulary for obligation and non-performance queries.",
    },
    ExpansionSeed {
        id: "civil-prescription",
        label: "Prescription extinctive",
        match_terms: &["prescription", "delai", "action", "forclusion"],
        expanded_terms: &[
            "prescription extinctive",
            "delai de prescription",
            "action personnelle",
            "action mobiliere",
            "article 2219",
            "article 2224",
        ],
        source_citation: "Code civil, articles 2219 et 2224",
        review_status: "dev_seed_pending_legal_review",
        reviewer: "pending_legal_domain_review",
        rationale: "Baseline limitation-period vocabulary for civil-action queries.",
    },
];

// Matching is deliberately phrase-contiguous and conservative: this seed lexicon
// is an audited hint source, not a generative thesaurus. `review_status` and
// `reviewer` remain explicit placeholders until legal-domain review promotes a
// seed.
pub fn expand_query(query: &str) -> ExpansionResponse {
    let normalized_query = normalize_for_match(query);
    let mut seen = BTreeSet::new();
    let mut expanded_terms = Vec::new();

    for seed in EXPANSION_SEEDS {
        let matched_terms = seed
            .match_terms
            .iter()
            .copied()
            .filter(|term| contains_normalized_term(&normalized_query, term))
            .collect::<Vec<_>>();
        if matched_terms.is_empty() {
            continue;
        }

        for term in seed.expanded_terms {
            let normalized_term = normalize_for_match(term);
            if normalized_term.is_empty()
                || contains_normalized_phrase(&normalized_query, &normalized_term)
                || !seen.insert(normalized_term)
            {
                continue;
            }
            expanded_terms.push(ExpandedTerm {
                term,
                matched_terms: matched_terms.clone(),
                source_seed_id: seed.id,
                source_label: seed.label,
                source_citation: seed.source_citation,
                review_status: seed.review_status,
                reviewer: seed.reviewer,
                rationale: seed.rationale,
            });
        }
    }

    ExpansionResponse {
        query: query.trim().to_owned(),
        seed_version: LEGAL_VOCABULARY_SEED_VERSION,
        expanded_terms,
    }
}

fn contains_normalized_term(normalized_query: &str, term: &str) -> bool {
    let normalized_term = normalize_for_match(term);
    !normalized_term.is_empty() && contains_normalized_phrase(normalized_query, &normalized_term)
}

fn contains_normalized_phrase(normalized_query: &str, normalized_term: &str) -> bool {
    let query = format!(" {normalized_query} ");
    let term = format!(" {normalized_term} ");
    query.contains(&term)
}

fn normalize_for_match(value: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::expand_query;

    #[test]
    fn expands_matching_legal_seed_with_review_metadata() {
        let response = expand_query("faute et dommage");

        assert_eq!(response.seed_version, "legal-vocabulary-seed:v1");
        assert!(
            response
                .expanded_terms
                .iter()
                .any(|term| term.term == "responsabilite civile"
                    && term.source_seed_id == "civil-liability-fault-damage"
                    && term.source_citation == "Code civil, articles 1240 et 1241"
                    && term.review_status == "dev_seed_pending_legal_review")
        );
        assert!(
            response
                .expanded_terms
                .iter()
                .any(|term| term.term == "article 1240"
                    && term.matched_terms == vec!["faute", "dommage"])
        );
    }

    #[test]
    fn expansion_matching_is_accent_and_punctuation_insensitive() {
        let response = expand_query("Inexecution d'une obligation contractuelle");

        assert!(
            response
                .expanded_terms
                .iter()
                .any(|term| term.term == "article 1231-1")
        );
    }

    #[test]
    fn non_matching_query_returns_empty_expansions() {
        let response = expand_query("recette de tarte aux pommes");

        assert!(response.expanded_terms.is_empty());
    }
}
