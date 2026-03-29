use crate::document::AnchorId;
use anyhow::Result;
use chrono::{DateTime, Local, NaiveDateTime, TimeZone};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use uuid::Uuid;

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
    #[serde(alias = "queued")]
    Pending,
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
    pub fn new() -> Self {
        Self::default()
    }
    pub fn add(&mut self, item: ReviewItem) {
        self.items.push(item);
    }
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
    pub fn pending(&self) -> Vec<&ReviewItem> {
        self.items
            .iter()
            .filter(|i| i.status == ReviewStatus::New || i.status == ReviewStatus::Answered)
            .collect()
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
    pub fn mark_pending(&mut self, id: Uuid, answer: String) {
        if let Some(item) = self.items.iter_mut().find(|i| i.id == id) {
            item.user_answer = Some(answer);
            item.status = ReviewStatus::Pending;
        }
    }
    pub fn mark_sent(&mut self, ids: &[Uuid]) {
        for item in self.items.iter_mut() {
            if ids.contains(&item.id) {
                item.status = ReviewStatus::Sent;
            }
        }
    }
    pub fn mark_answered(&mut self, id: Uuid) {
        if let Some(item) = self.items.iter_mut().find(|i| i.id == id) {
            item.status = ReviewStatus::Answered;
        }
    }
    pub fn mark_applied(&mut self, id: Uuid) {
        if let Some(item) = self.items.iter_mut().find(|i| i.id == id) {
            item.status = ReviewStatus::Applied;
        }
    }
    pub fn clear(&mut self) {
        self.items.clear();
    }
}

// ── Persistent analysis store ────────────────────────────────────────────────

/// One entry in the analysis history list.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct AnalysisEntry {
    pub path: PathBuf,
    pub timestamp: DateTime<Local>,
    pub label: String,
    pub item_count: usize,
}

/// Manages on-disk snapshots of analysis results for a single document.
///
/// Snapshots live at: `~/.aichitect/analysis/<stem>/<YYYY-MM-DDTHH-MM-SS>.json`
pub struct AnalysisStore {
    dir: PathBuf,
}

impl AnalysisStore {
    /// Create (or reuse) the analysis directory for `doc_path`.
    pub fn for_doc(doc_path: &Path) -> Result<Self> {
        let stem = doc_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("document");

        let dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".aichitect")
            .join("analysis")
            .join(stem);

        fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    /// Write `items` as a new snapshot. Returns the path written.
    pub fn save(&self, items: &[ReviewItem]) -> Result<PathBuf> {
        let ts = Local::now().format("%Y-%m-%dT%H-%M-%S").to_string();
        let filename = format!("{}.json", ts);
        let path = self.dir.join(&filename);
        let json = serde_json::to_string_pretty(items)?;
        fs::write(&path, json)?;
        Ok(path)
    }

    /// List all snapshots newest-first.
    pub fn list(&self) -> Vec<AnalysisEntry> {
        let Ok(rd) = fs::read_dir(&self.dir) else {
            return vec![];
        };
        let mut entries: Vec<AnalysisEntry> = rd
            .flatten()
            .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("json"))
            .filter_map(|e| {
                let path = e.path();
                let stem = path.file_stem()?.to_str()?.to_string();
                let normalized = stem
                    .replace('T', " ")
                    .replacen('-', "/", 2)
                    .replace('-', ":");
                let dt =
                    NaiveDateTime::parse_from_str(&normalized, "%Y/%m/%d %H:%M:%S").ok()?;
                let timestamp = Local.from_local_datetime(&dt).single()?;
                let label = timestamp.format("%Y-%m-%d  %H:%M:%S").to_string();
                let item_count = Self::load(&path).map(|v| v.len()).unwrap_or(0);
                Some(AnalysisEntry {
                    path,
                    timestamp,
                    label,
                    item_count,
                })
            })
            .collect();
        entries.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
        entries
    }

    /// Load review items from a snapshot file.
    pub fn load(path: &Path) -> Result<Vec<ReviewItem>> {
        let content = fs::read_to_string(path)?;
        let items: Vec<ReviewItem> = serde_json::from_str(&content)?;
        Ok(items)
    }

    /// Load the most recent snapshot, if any.
    pub fn load_latest(&self) -> Option<Vec<ReviewItem>> {
        self.list()
            .first()
            .and_then(|entry| Self::load(&entry.path).ok())
    }
}
