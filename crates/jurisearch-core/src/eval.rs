use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
    Draft,
    OfficialSourceChecked,
    HumanReviewed,
    HeldOut,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegalRetrievalFixture {
    pub id: String,
    pub category: String,
    pub query: String,
    pub expected_ids: Vec<String>,
    #[serde(default)]
    pub allowed_alternates: Vec<String>,
    #[serde(default)]
    pub temporal_expectation: Option<String>,
    pub drafted_by: String,
    pub verified_against: String,
    pub reviewer: Option<String>,
    pub review_status: ReviewStatus,
    pub rationale: String,
}

impl LegalRetrievalFixture {
    pub fn is_release_gating(&self) -> bool {
        matches!(
            self.review_status,
            ReviewStatus::HumanReviewed | ReviewStatus::HeldOut
        ) && self
            .reviewer
            .as_deref()
            .is_some_and(|reviewer| !reviewer.trim().is_empty())
    }
}
