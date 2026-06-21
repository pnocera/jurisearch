use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
    Draft,
    OfficialSourceChecked,
    HumanReviewed,
    HeldOut,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixtureTier {
    #[default]
    Dev,
    ReleaseGating,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HierarchyExpectation {
    pub context_document_id: String,
    #[serde(default)]
    pub as_of: Option<String>,
    #[serde(default)]
    pub expected_ancestry_titles: Vec<String>,
    #[serde(default)]
    pub required_sibling_ids: Vec<String>,
    #[serde(default)]
    pub forbidden_sibling_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegalRetrievalFixture {
    pub id: String,
    #[serde(default)]
    pub tier: FixtureTier,
    pub category: String,
    pub query: String,
    pub expected_ids: Vec<String>,
    #[serde(default)]
    pub allowed_alternates: Vec<String>,
    #[serde(default)]
    pub temporal_expectation: Option<String>,
    #[serde(default)]
    pub hierarchy: Option<HierarchyExpectation>,
    pub drafted_by: String,
    pub verified_against: String,
    pub reviewer: Option<String>,
    pub review_status: ReviewStatus,
    pub rationale: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvalFixtureSummary {
    pub total: usize,
    pub source_verified: usize,
    pub release_gating: usize,
    pub hierarchy_sensitive: usize,
    pub categories: BTreeMap<String, usize>,
}

impl LegalRetrievalFixture {
    /// True once an expected result was checked against an official source, even
    /// if it is still a non-gating development fixture.
    pub fn is_source_verified(&self) -> bool {
        matches!(
            self.review_status,
            ReviewStatus::OfficialSourceChecked
                | ReviewStatus::HumanReviewed
                | ReviewStatus::HeldOut
        ) && !self.verified_against.trim().is_empty()
    }

    /// True only for source-verified fixtures promoted by a named human reviewer.
    pub fn is_release_gating(&self) -> bool {
        self.tier == FixtureTier::ReleaseGating
            && self.is_source_verified()
            && matches!(
                self.review_status,
                ReviewStatus::HumanReviewed | ReviewStatus::HeldOut
            )
            && self
                .reviewer
                .as_deref()
                .is_some_and(|reviewer| !reviewer.trim().is_empty())
    }

    pub fn requires_hierarchy_context(&self) -> bool {
        self.hierarchy.is_some()
    }
}

pub const HIERARCHY_SENSITIVE_STATUTORY_CATEGORY: &str = "hierarchy_sensitive_statutory";

pub fn phase1_hierarchy_dev_fixtures() -> Vec<LegalRetrievalFixture> {
    vec![
        same_section_hierarchy_fixture(),
        temporal_sibling_hierarchy_fixture(),
    ]
}

pub fn phase1_eval_fixture_summary() -> EvalFixtureSummary {
    let fixtures = phase1_hierarchy_dev_fixtures();
    let mut categories = BTreeMap::new();
    for fixture in &fixtures {
        *categories.entry(fixture.category.clone()).or_insert(0) += 1;
    }

    EvalFixtureSummary {
        total: fixtures.len(),
        source_verified: fixtures
            .iter()
            .filter(|fixture| fixture.is_source_verified())
            .count(),
        release_gating: fixtures
            .iter()
            .filter(|fixture| fixture.is_release_gating())
            .count(),
        hierarchy_sensitive: fixtures
            .iter()
            .filter(|fixture| fixture.requires_hierarchy_context())
            .count(),
        categories,
    }
}

fn same_section_hierarchy_fixture() -> LegalRetrievalFixture {
    LegalRetrievalFixture {
        id: "legi-hierarchy-same-section-1996".to_owned(),
        tier: FixtureTier::Dev,
        category: HIERARCHY_SENSITIVE_STATUTORY_CATEGORY.to_owned(),
        query: "Retrouver l'article applicable aux organismes genetiquement modifies destines a l'alimentation humaine et verifier son voisinage de section.".to_owned(),
        expected_ids: strings(&["legi:LEGIARTI000006850357@1994-01-19"]),
        allowed_alternates: Vec::new(),
        temporal_expectation: Some("1996-01-01 keeps the 1994-01-19 article versions and excludes later replacements.".to_owned()),
        hierarchy: Some(HierarchyExpectation {
            context_document_id: "legi:LEGIARTI000006850357@1994-01-19".to_owned(),
            as_of: Some("1996-01-01".to_owned()),
            expected_ancestry_titles: official_decree_hierarchy("Chapitre Ier : Dispositions applicables à la dissémination volontaire à toute fin autre que la mise sur le marché."),
            required_sibling_ids: strings(&[
                "legi:LEGIARTI000006850359@1994-01-19",
                "legi:LEGIARTI000006850360@1994-01-19",
            ]),
            forbidden_sibling_ids: strings(&[
                "legi:LEGIARTI000006850361@1999-03-28",
                "legi:LEGIARTI000006850372@1994-01-19",
            ]),
        }),
        drafted_by: "codex".to_owned(),
        verified_against: "DILA LEGI Freemium_legi_global_20250713-140000.tar.gz: LEGITEXT000005615128; section LEGISCTA000006129313 valid 1994-01-19..2007-03-20 contains LEGIARTI000006850357/359/360/361; neighbouring section LEGISCTA000006129314 contains forbidden LEGIARTI000006850372 valid 1994-01-19..1999-03-28".to_owned(),
        reviewer: None,
        review_status: ReviewStatus::OfficialSourceChecked,
        rationale: "Exercises CONTEXTE-derived ancestry, same-section siblings, neighbouring-section exclusion, and temporal exclusion of the 1999 replacement article.".to_owned(),
    }
}

fn temporal_sibling_hierarchy_fixture() -> LegalRetrievalFixture {
    LegalRetrievalFixture {
        id: "legi-hierarchy-temporal-sibling-2000".to_owned(),
        tier: FixtureTier::Dev,
        category: HIERARCHY_SENSITIVE_STATUTORY_CATEGORY.to_owned(),
        query: "Verifier la version de l'article 3 applicable apres la modification du 28 mars 1999 et son voisinage de chapitre.".to_owned(),
        expected_ids: strings(&["legi:LEGIARTI000006850361@1999-03-28"]),
        allowed_alternates: Vec::new(),
        temporal_expectation: Some("2000-01-01 selects LEGIARTI000006850361 and excludes the 1994-01-19 modified predecessor.".to_owned()),
        hierarchy: Some(HierarchyExpectation {
            context_document_id: "legi:LEGIARTI000006850361@1999-03-28".to_owned(),
            as_of: Some("2000-01-01".to_owned()),
            expected_ancestry_titles: official_decree_hierarchy("Chapitre Ier : Dispositions applicables à la dissémination volontaire à toute fin autre que la mise sur le marché."),
            required_sibling_ids: strings(&[
                "legi:LEGIARTI000006850357@1994-01-19",
                "legi:LEGIARTI000006850359@1994-01-19",
            ]),
            forbidden_sibling_ids: strings(&[
                "legi:LEGIARTI000006850360@1994-01-19",
                "legi:LEGIARTI000006850373@1999-03-28",
            ]),
        }),
        drafted_by: "codex".to_owned(),
        verified_against: "DILA LEGI Freemium_legi_global_20250713-140000.tar.gz: LEGITEXT000005615128; section LEGISCTA000006129313 valid 1994-01-19..2007-03-20 contains required LEGIARTI000006850357/359 and target LEGIARTI000006850361 valid 1999-03-28..2007-03-20; predecessor LEGIARTI000006850360 is valid 1994-01-19..1999-03-28; neighbouring section LEGISCTA000006129314 contains forbidden LEGIARTI000006850373 valid 1999-03-28..2007-03-20".to_owned(),
        reviewer: None,
        review_status: ReviewStatus::OfficialSourceChecked,
        rationale: "Exercises same article-number replacement, as-of-sensitive sibling filtering, and neighbouring chapter exclusion.".to_owned(),
    }
}

fn official_decree_hierarchy(chapter: &str) -> Vec<String> {
    strings(&[
        "Décret n°94-46 du 5 janvier 1994 fixant les conditions de dissémination volontaire des organismes génétiquement modifiés destinés à l'alimentation humaine autres que les plantes, les semences, les plants et les animaux, ou entrant dans la composition des produits de nettoyage des matériaux et objets destinés à entrer en contact avec des denrées, produits ou boissons destinés à l'alimentation de l'homme ou des animaux",
        "TITRE Ier : Dispositions relatives aux organismes génétiquement modifiés autres que les plantes, les semences, les plants et les animaux destinés à l'alimentation humaine",
        chapter,
    ])
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{
        FixtureTier, HIERARCHY_SENSITIVE_STATUTORY_CATEGORY, HierarchyExpectation,
        LegalRetrievalFixture, ReviewStatus, phase1_eval_fixture_summary,
        phase1_hierarchy_dev_fixtures,
    };

    #[test]
    fn release_gating_requires_tier_source_verification_and_named_reviewer() {
        let mut fixture = LegalRetrievalFixture {
            id: "fixture".to_owned(),
            tier: FixtureTier::Dev,
            category: "known_article".to_owned(),
            query: "article 1".to_owned(),
            expected_ids: vec!["legi:LEGIARTI@example".to_owned()],
            allowed_alternates: Vec::new(),
            temporal_expectation: None,
            hierarchy: None,
            drafted_by: "codex".to_owned(),
            verified_against: "official API".to_owned(),
            reviewer: Some("reviewer".to_owned()),
            review_status: ReviewStatus::HumanReviewed,
            rationale: "source checked".to_owned(),
        };

        assert!(!fixture.is_release_gating());

        fixture.tier = FixtureTier::ReleaseGating;
        assert!(fixture.is_release_gating());

        fixture.verified_against.clear();
        assert!(!fixture.is_source_verified());
        assert!(!fixture.is_release_gating());

        fixture.verified_against = "official API".to_owned();
        fixture.reviewer = Some("   ".to_owned());
        assert!(!fixture.is_release_gating());
    }

    #[test]
    fn hierarchy_expectation_round_trips_as_fixture_json() {
        let fixture = LegalRetrievalFixture {
            id: "hierarchy".to_owned(),
            tier: FixtureTier::Dev,
            category: HIERARCHY_SENSITIVE_STATUTORY_CATEGORY.to_owned(),
            query: "context".to_owned(),
            expected_ids: vec!["legi:target".to_owned()],
            allowed_alternates: Vec::new(),
            temporal_expectation: Some("as-of".to_owned()),
            hierarchy: Some(HierarchyExpectation {
                context_document_id: "legi:target".to_owned(),
                as_of: Some("2000-01-01".to_owned()),
                expected_ancestry_titles: vec!["Code".to_owned(), "Section".to_owned()],
                required_sibling_ids: vec!["legi:sibling".to_owned()],
                forbidden_sibling_ids: vec!["legi:other-section".to_owned()],
            }),
            drafted_by: "codex".to_owned(),
            verified_against: "official XML".to_owned(),
            reviewer: None,
            review_status: ReviewStatus::OfficialSourceChecked,
            rationale: "context-sensitive".to_owned(),
        };

        let json = serde_json::to_string(&fixture).expect("fixture serializes");
        let decoded: LegalRetrievalFixture =
            serde_json::from_str(&json).expect("fixture deserializes");

        assert_eq!(decoded, fixture);
        assert!(decoded.requires_hierarchy_context());
        assert!(!decoded.is_release_gating());
    }

    #[test]
    fn legacy_fixture_json_defaults_to_dev_without_hierarchy_expectations() {
        let json = r#"{
            "id": "legacy",
            "category": "known_article",
            "query": "article 1",
            "expected_ids": ["legi:LEGIARTI@example"],
            "drafted_by": "codex",
            "verified_against": "",
            "reviewer": null,
            "review_status": "draft",
            "rationale": "legacy fixture shape"
        }"#;

        let fixture: LegalRetrievalFixture =
            serde_json::from_str(json).expect("legacy fixture shape deserializes");

        assert_eq!(fixture.tier, FixtureTier::Dev);
        assert!(fixture.hierarchy.is_none());
        assert!(fixture.allowed_alternates.is_empty());
        assert!(fixture.temporal_expectation.is_none());
        assert!(!fixture.is_source_verified());
        assert!(!fixture.is_release_gating());
    }

    #[test]
    fn phase1_hierarchy_dev_fixtures_are_source_checked_but_not_gating() {
        let fixtures = phase1_hierarchy_dev_fixtures();

        assert_eq!(fixtures.len(), 2);
        for fixture in fixtures {
            assert_eq!(fixture.tier, FixtureTier::Dev);
            assert_eq!(fixture.category, HIERARCHY_SENSITIVE_STATUTORY_CATEGORY);
            assert!(fixture.is_source_verified());
            assert!(!fixture.is_release_gating());

            let hierarchy = fixture
                .hierarchy
                .as_ref()
                .expect("hierarchy fixture has context expectations");
            assert!(!hierarchy.expected_ancestry_titles.is_empty());
            assert!(!hierarchy.required_sibling_ids.is_empty());
            assert!(!hierarchy.forbidden_sibling_ids.is_empty());
            assert!(
                fixture
                    .expected_ids
                    .contains(&hierarchy.context_document_id)
            );

            let required: BTreeSet<_> = hierarchy.required_sibling_ids.iter().collect();
            let forbidden: BTreeSet<_> = hierarchy.forbidden_sibling_ids.iter().collect();
            assert!(required.is_disjoint(&forbidden));
            assert!(!required.contains(&hierarchy.context_document_id));
            assert!(!forbidden.contains(&hierarchy.context_document_id));
        }
    }

    #[test]
    fn phase1_eval_fixture_summary_counts_current_fixture_readiness() {
        let summary = phase1_eval_fixture_summary();

        assert_eq!(summary.total, 2);
        assert_eq!(summary.source_verified, 2);
        assert_eq!(summary.release_gating, 0);
        assert_eq!(summary.hierarchy_sensitive, 2);
        assert_eq!(
            summary.categories[HIERARCHY_SENSITIVE_STATUTORY_CATEGORY],
            2
        );
    }
}
