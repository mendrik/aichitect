use serde::{Deserialize, Serialize};
use uuid::Uuid;
use crate::document::AnchorId;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewCategory {
    Ambiguity,
    Contradiction,
    MissingAcceptanceCriteria,
    UndefinedTerm,
    HiddenAssumption,
    MissingEdgeCase,
    MissingOperationalConstraint,
    UnclearOwnership,
    VagueSuccessMetric,
    MissingFailureBehavior,
    MisleadingWording,
    IncompleteCodeExample,
    UnspecifiedInputOutput,
}

impl std::fmt::Display for ReviewCategory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            ReviewCategory::Ambiguity => "Ambiguity",
            ReviewCategory::Contradiction => "Contradiction",
            ReviewCategory::MissingAcceptanceCriteria => "Missing Acceptance Criteria",
            ReviewCategory::UndefinedTerm => "Undefined Term",
            ReviewCategory::HiddenAssumption => "Hidden Assumption",
            ReviewCategory::MissingEdgeCase => "Missing Edge Case",
            ReviewCategory::MissingOperationalConstraint => "Missing Operational Constraint",
            ReviewCategory::UnclearOwnership => "Unclear Ownership",
            ReviewCategory::VagueSuccessMetric => "Vague Success Metric",
            ReviewCategory::MissingFailureBehavior => "Missing Failure Behavior",
            ReviewCategory::MisleadingWording => "Misleading Wording",
            ReviewCategory::IncompleteCodeExample => "Incomplete Code Example",
            ReviewCategory::UnspecifiedInputOutput => "Unspecified Input/Output",
        };
        write!(f, "{}", s)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
    New,
    Answered,
    Queued,
    Sent,
    Applied,
    Dismissed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewItem {
    pub id: Uuid,
    pub category: ReviewCategory,
    pub anchor: AnchorId,
    pub evidence: String,
    pub why_it_matters: String,
    pub suggested_resolution: String,
    pub status: ReviewStatus,
    pub user_answer: Option<String>,
}

impl ReviewItem {
    /// Short title for use in remark text.
    pub fn suggested_question_or_title(&self) -> String {
        format!("{} @ {}", self.category, self.anchor)
    }
}

#[derive(Debug, Clone, Default)]
pub struct ReviewStore {
    pub items: Vec<ReviewItem>,
}

impl ReviewStore {
    pub fn new() -> Self { Self::default() }
    pub fn add(&mut self, item: ReviewItem) { self.items.push(item); }
    pub fn pending(&self) -> Vec<&ReviewItem> {
        self.items.iter().filter(|i| i.status == ReviewStatus::New || i.status == ReviewStatus::Answered).collect()
    }
    #[allow(dead_code)]
    pub fn answer(&mut self, id: Uuid, answer: String) {
        if let Some(item) = self.items.iter_mut().find(|i| i.id == id) {
            item.user_answer = Some(answer);
            item.status = ReviewStatus::Answered;
        }
    }
    pub fn dismiss(&mut self, id: Uuid) {
        if let Some(item) = self.items.iter_mut().find(|i| i.id == id) {
            item.status = ReviewStatus::Dismissed;
        }
    }
    pub fn mark_sent(&mut self, ids: &[Uuid]) {
        for item in self.items.iter_mut() {
            if ids.contains(&item.id) { item.status = ReviewStatus::Sent; }
        }
    }
}
