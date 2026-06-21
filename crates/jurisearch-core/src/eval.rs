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
    pub as_of: Option<String>,
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
    pub release_candidates: usize,
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

    /// True for source-checked labels intended for the release gate but still
    /// waiting for named human review.
    pub fn is_release_candidate(&self) -> bool {
        self.tier == FixtureTier::ReleaseGating
            && self.is_source_verified()
            && !self.is_release_gating()
    }

    pub fn requires_hierarchy_context(&self) -> bool {
        self.hierarchy.is_some()
    }
}

pub const HIERARCHY_SENSITIVE_STATUTORY_CATEGORY: &str = "hierarchy_sensitive_statutory";
pub const KNOWN_ARTICLE_STATUTORY_CATEGORY: &str = "known_article_statutory";
pub const CONCEPTUAL_STATUTORY_CATEGORY: &str = "conceptual_statutory";
pub const TEMPORAL_STATUTORY_CATEGORY: &str = "temporal_statutory";
pub const CITATION_STATE_STATUTORY_CATEGORY: &str = "citation_state_statutory";

const PHASE1_SOURCE_ARCHIVE: &str = "DILA LEGI Freemium_legi_global_20250713-140000.tar.gz";

pub fn phase1_hierarchy_dev_fixtures() -> Vec<LegalRetrievalFixture> {
    vec![
        same_section_hierarchy_fixture(),
        temporal_sibling_hierarchy_fixture(),
    ]
}

pub fn phase1_release_candidate_fixtures() -> Vec<LegalRetrievalFixture> {
    vec![
        code_rural_r242_40_historical_lookup_fixture(),
        veterinarian_deontology_concept_fixture(),
        reserve_natural_temporal_lookup_fixture(),
        long_stay_social_security_citation_fixture(),
    ]
}

pub fn phase1_eval_fixtures() -> Vec<LegalRetrievalFixture> {
    let mut fixtures = phase1_hierarchy_dev_fixtures();
    fixtures.extend(phase1_release_candidate_fixtures());
    fixtures
}

pub fn phase1_eval_fixture_summary() -> EvalFixtureSummary {
    let fixtures = phase1_eval_fixtures();
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
        release_candidates: fixtures
            .iter()
            .filter(|fixture| fixture.is_release_candidate())
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
        as_of: Some("1996-01-01".to_owned()),
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
        as_of: Some("2000-01-01".to_owned()),
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

fn code_rural_r242_40_historical_lookup_fixture() -> LegalRetrievalFixture {
    LegalRetrievalFixture {
        id: "legi-release-candidate-code-rural-r242-40-1989".to_owned(),
        tier: FixtureTier::ReleaseGating,
        category: KNOWN_ARTICLE_STATUTORY_CATEGORY.to_owned(),
        query: "Retrouver l'article R*242-40 du code rural applicable avant le 7 aout 2003 sur les contraventions dans une reserve naturelle.".to_owned(),
        expected_ids: strings(&["legi:LEGIARTI000006590697@1989-11-04"]),
        allowed_alternates: Vec::new(),
        as_of: Some("1990-01-01".to_owned()),
        temporal_expectation: Some("1990-01-01 selects LEGIARTI000006590697 and excludes later R242-40 versions LEGIARTI000006590698/699/030361159.".to_owned()),
        hierarchy: None,
        drafted_by: "codex".to_owned(),
        verified_against: official_archive_evidence(
            "legi/global/code_et_TNC_en_vigueur/code_en_vigueur/LEGI/TEXT/00/00/06/07/13/LEGITEXT000006071367/article/LEGI/ARTI/00/00/06/59/06/LEGIARTI000006590697.xml",
            "META_ARTICLE NUM R*242-40, ETAT ABROGE, DATE_DEBUT 1989-11-04, DATE_FIN 2003-08-07; VERSIONS lists successor R*242-40/R242-40 article IDs; body sanctions contraventions in natural reserves.",
        ),
        reviewer: None,
        review_status: ReviewStatus::OfficialSourceChecked,
        rationale: "Known-article lookup with an explicit historical as-of constraint and successor-version exclusion.".to_owned(),
    }
}

fn veterinarian_deontology_concept_fixture() -> LegalRetrievalFixture {
    LegalRetrievalFixture {
        id: "legi-release-candidate-veterinaire-deontologie-2003".to_owned(),
        tier: FixtureTier::ReleaseGating,
        category: CONCEPTUAL_STATUTORY_CATEGORY.to_owned(),
        query: "Au 1er septembre 2003, quelle disposition interdisait a un veterinaire ayant une responsabilite administrative ou politique de s'en prevaloir a des fins personnelles ?".to_owned(),
        expected_ids: strings(&["legi:LEGIARTI000006590698@2003-08-07"]),
        allowed_alternates: Vec::new(),
        as_of: Some("2003-09-01".to_owned()),
        temporal_expectation: Some("2003-09-01 selects the short-lived 2003-08-07 version and excludes the 1989 reserve-nature predecessor plus later R242-40 versions.".to_owned()),
        hierarchy: None,
        drafted_by: "codex".to_owned(),
        verified_against: official_archive_evidence(
            "legi/global/code_et_TNC_en_vigueur/code_en_vigueur/LEGI/TEXT/00/00/06/07/13/LEGITEXT000006071367/article/LEGI/ARTI/00/00/06/59/06/LEGIARTI000006590698.xml",
            "META_ARTICLE NUM R*242-40, ETAT MODIFIE, DATE_DEBUT 2003-08-07, DATE_FIN 2003-10-11; CONTEXTE is Code rural, code de deontologie veterinaire; body contains the administrative/political responsibility prohibition.",
        ),
        reviewer: None,
        review_status: ReviewStatus::OfficialSourceChecked,
        rationale: "Conceptual statutory retrieval where the query names the legal duty, not the article identifier.".to_owned(),
    }
}

fn reserve_natural_temporal_lookup_fixture() -> LegalRetrievalFixture {
    LegalRetrievalFixture {
        id: "legi-release-candidate-reserve-naturelle-r242-41-1990".to_owned(),
        tier: FixtureTier::ReleaseGating,
        category: TEMPORAL_STATUTORY_CATEGORY.to_owned(),
        query: "Au 1er janvier 1990, quelle disposition sanctionnait la violation d'une decision de classement d'une reserve naturelle concernant les activites agricoles, pastorales, forestieres ou sportives ?".to_owned(),
        expected_ids: strings(&["legi:LEGIARTI000006590700@1989-11-04"]),
        allowed_alternates: Vec::new(),
        as_of: Some("1990-01-01".to_owned()),
        temporal_expectation: Some("1990-01-01 selects LEGIARTI000006590700; 2003-08-07 and later R242-41 versions are out of scope.".to_owned()),
        hierarchy: None,
        drafted_by: "codex".to_owned(),
        verified_against: official_archive_evidence(
            "legi/global/code_et_TNC_en_vigueur/code_en_vigueur/LEGI/TEXT/00/00/06/07/13/LEGITEXT000006071367/article/LEGI/ARTI/00/00/06/59/07/LEGIARTI000006590700.xml",
            "META_ARTICLE NUM R*242-41, ETAT ABROGE, DATE_DEBUT 1989-11-04, DATE_FIN 2003-08-07; body sanctions violations of reserve-natural classification decisions regulating agricultural, pastoral, forestry, games, or sports activities.",
        ),
        reviewer: None,
        review_status: ReviewStatus::OfficialSourceChecked,
        rationale: "Temporal statutory research task that should fail if the index ignores validity windows or successor versions.".to_owned(),
    }
}

fn long_stay_social_security_citation_fixture() -> LegalRetrievalFixture {
    LegalRetrievalFixture {
        id: "legi-release-candidate-loi-1990-long-sejour".to_owned(),
        tier: FixtureTier::ReleaseGating,
        category: CITATION_STATE_STATUTORY_CATEGORY.to_owned(),
        query: "Identifier la disposition de la loi du 23 janvier 1990 validant les forfaits journaliers de soins en unites de long sejour malgre l'absence des decrets d'application des lois de 1975 et 1978.".to_owned(),
        expected_ids: strings(&["legi:LEGIARTI000006756700@2000-12-23"]),
        allowed_alternates: Vec::new(),
        as_of: Some("2024-01-01".to_owned()),
        temporal_expectation: Some("DATE_FIN 2999-01-01 is an open-ended sentinel normalized to the current version for Phase 1 queries.".to_owned()),
        hierarchy: None,
        drafted_by: "codex".to_owned(),
        verified_against: official_archive_evidence(
            "legi/global/code_et_TNC_en_vigueur/TNC_en_vigueur/JORF/TEXT/00/00/00/70/72/JORFTEXT000000707200/article/LEGI/ARTI/00/00/06/75/67/LEGIARTI000006756700.xml",
            "META_ARTICLE NUM 27, ETAT VIGUEUR, DATE_DEBUT 2000-12-23, DATE_FIN 2999-01-01; body validates long-stay daily-care forfait and price decisions despite missing decrees; LIENS cite the 1970, 1978, and action-sociale-family code provisions.",
        ),
        reviewer: None,
        review_status: ReviewStatus::OfficialSourceChecked,
        rationale: "Citation-rich statutory retrieval task covering source links, sentinel end-date normalization, and a non-code law article.".to_owned(),
    }
}

fn official_decree_hierarchy(chapter: &str) -> Vec<String> {
    strings(&[
        "Décret n°94-46 du 5 janvier 1994 fixant les conditions de dissémination volontaire des organismes génétiquement modifiés destinés à l'alimentation humaine autres que les plantes, les semences, les plants et les animaux, ou entrant dans la composition des produits de nettoyage des matériaux et objets destinés à entrer en contact avec des denrées, produits ou boissons destinés à l'alimentation de l'homme ou des animaux",
        "TITRE Ier : Dispositions relatives aux organismes génétiquement modifiés autres que les plantes, les semences, les plants et les animaux destinés à l'alimentation humaine",
        chapter,
    ])
}

fn official_archive_evidence(member_path: &str, detail: &str) -> String {
    format!("{PHASE1_SOURCE_ARCHIVE}: {member_path}; {detail}")
}

fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::{
        CITATION_STATE_STATUTORY_CATEGORY, CONCEPTUAL_STATUTORY_CATEGORY, FixtureTier,
        HIERARCHY_SENSITIVE_STATUTORY_CATEGORY, HierarchyExpectation,
        KNOWN_ARTICLE_STATUTORY_CATEGORY, LegalRetrievalFixture, ReviewStatus,
        TEMPORAL_STATUTORY_CATEGORY, phase1_eval_fixture_summary, phase1_hierarchy_dev_fixtures,
        phase1_release_candidate_fixtures,
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
            as_of: None,
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
        assert!(!fixture.is_release_candidate());

        fixture.verified_against.clear();
        assert!(!fixture.is_source_verified());
        assert!(!fixture.is_release_gating());
        assert!(!fixture.is_release_candidate());

        fixture.verified_against = "official API".to_owned();
        fixture.reviewer = Some("   ".to_owned());
        assert!(!fixture.is_release_gating());
        assert!(fixture.is_release_candidate());
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
            as_of: Some("2000-01-01".to_owned()),
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
        assert!(fixture.as_of.is_none());
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

        assert_eq!(summary.total, 6);
        assert_eq!(summary.source_verified, 6);
        assert_eq!(summary.release_candidates, 4);
        assert_eq!(summary.release_gating, 0);
        assert_eq!(summary.hierarchy_sensitive, 2);
        assert_eq!(
            summary.categories[HIERARCHY_SENSITIVE_STATUTORY_CATEGORY],
            2
        );
        assert_eq!(summary.categories[KNOWN_ARTICLE_STATUTORY_CATEGORY], 1);
        assert_eq!(summary.categories[CONCEPTUAL_STATUTORY_CATEGORY], 1);
        assert_eq!(summary.categories[TEMPORAL_STATUTORY_CATEGORY], 1);
        assert_eq!(summary.categories[CITATION_STATE_STATUTORY_CATEGORY], 1);
    }

    #[test]
    fn phase1_release_candidate_fixtures_are_source_checked_not_gating() {
        let fixtures = phase1_release_candidate_fixtures();

        assert_eq!(fixtures.len(), 4);
        for fixture in fixtures {
            assert_eq!(fixture.tier, FixtureTier::ReleaseGating);
            assert!(fixture.verified_against.contains("DILA LEGI Freemium"));
            assert!(fixture.verified_against.contains(".xml"));
            assert!(fixture.reviewer.is_none());
            assert_eq!(fixture.review_status, ReviewStatus::OfficialSourceChecked);
            assert!(fixture.is_source_verified());
            assert!(fixture.is_release_candidate());
            assert!(!fixture.is_release_gating());
            assert_eq!(fixture.expected_ids.len(), 1);
            assert!(fixture.expected_ids[0].starts_with("legi:LEGIARTI"));
            assert!(fixture.expected_ids[0].contains('@'));
            assert!(fixture.as_of.is_some());
            assert!(fixture.temporal_expectation.is_some());
        }
    }
}
