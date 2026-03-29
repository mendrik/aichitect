use crate::document::AnchorId;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum TargetType {
    Section,
    Paragraph,
    CodeBlock,
    TextSpan,
    ListItem,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum RemarkStatus {
    Draft,
    #[serde(alias = "Queued")]
    Pending,
    Sent,
    Applied,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Remark {
    pub id: Uuid,
    #[serde(default)]
    pub source_review_id: Option<Uuid>,
    pub anchor: AnchorId,
    pub selected_text: String,
    pub target_type: TargetType,
    pub text: String,
    /// Full text of the contiguous list this item belongs to (set when target_type == ListItem).
    pub list_context: Option<String>,
    /// Other document locations that contain the same pattern and should receive the same update.
    /// Each entry is (anchor_id, text_snippet).
    pub occurrence_anchors: Vec<(AnchorId, String)>,
    pub created_at: DateTime<Utc>,
    pub status: RemarkStatus,
}

#[derive(Debug, Clone, Default)]
pub struct RemarkStore {
    pub remarks: Vec<Remark>,
}

impl RemarkStore {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn add(&mut self, r: Remark) {
        self.remarks.push(r);
    }
    pub fn get(&self, id: Uuid) -> Option<&Remark> {
        self.remarks.iter().find(|r| r.id == id)
    }
    pub fn get_mut(&mut self, id: Uuid) -> Option<&mut Remark> {
        self.remarks.iter_mut().find(|r| r.id == id)
    }
    #[allow(dead_code)]
    pub fn remove(&mut self, id: Uuid) {
        self.remarks.retain(|r| r.id != id);
    }
    pub fn pending(&self) -> Vec<&Remark> {
        self.remarks
            .iter()
            .filter(|r| r.status == RemarkStatus::Pending)
            .collect()
    }
    #[allow(dead_code)]
    pub fn mark_sent(&mut self, id: Uuid) {
        if let Some(r) = self.remarks.iter_mut().find(|r| r.id == id) {
            r.status = RemarkStatus::Sent;
        }
    }
    #[allow(dead_code)]
    pub fn mark_applied(&mut self, id: Uuid) {
        if let Some(r) = self.remarks.iter_mut().find(|r| r.id == id) {
            r.status = RemarkStatus::Applied;
        }
    }
    #[allow(dead_code)]
    pub fn mark_failed(&mut self, id: Uuid) {
        if let Some(r) = self.remarks.iter_mut().find(|r| r.id == id) {
            r.status = RemarkStatus::Failed;
        }
    }
}
