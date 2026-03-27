use crate::document::{AnchorId, Document, NodeKind};
use crate::remarks::Remark;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

const MAX_TARGETED_REMARKS: usize = 6;
const MAX_TARGETED_OCCURRENCES: usize = 24;
const MAX_TARGETED_CONTEXT_CHARS: usize = 14_000;
const MAX_SECTION_NODES: usize = 10;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextTarget {
    pub anchor: AnchorId,
    pub selected_text: String,
    pub raw_markdown: String,
    pub role: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextNode {
    pub anchor: AnchorId,
    pub summary: String,
    pub raw_markdown: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RevisionContextPack {
    pub primary_anchor: AnchorId,
    pub generation: u64,
    pub instruction: String,
    pub primary_target: ContextTarget,
    pub occurrence_targets: Vec<ContextTarget>,
    pub local_context_nodes: Vec<ContextNode>,
    pub list_context: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevisionRequestMode {
    Targeted,
    FullDocument,
}

#[derive(Debug, Clone)]
pub struct TargetedRevisionPlan {
    pub packs: Vec<RevisionContextPack>,
}

pub fn build_targeted_revision_plan(
    doc: &Document,
    remarks: &[&Remark],
) -> Option<TargetedRevisionPlan> {
    if remarks.is_empty() || remarks.len() > MAX_TARGETED_REMARKS {
        return None;
    }

    let mut packs = Vec::with_capacity(remarks.len());
    let mut total_targets = 0usize;
    let mut total_context_chars = 0usize;

    for remark in remarks {
        let pack = build_context_pack(doc, remark)?;
        total_targets += 1 + pack.occurrence_targets.len();
        total_context_chars += pack.instruction.len()
            + pack.primary_target.raw_markdown.len()
            + pack.primary_target.selected_text.len()
            + pack
                .occurrence_targets
                .iter()
                .map(|t| t.raw_markdown.len() + t.selected_text.len())
                .sum::<usize>()
            + pack
                .local_context_nodes
                .iter()
                .map(|n| n.raw_markdown.len() + n.summary.len())
                .sum::<usize>()
            + pack.list_context.as_ref().map(|ctx| ctx.len()).unwrap_or(0);
        packs.push(pack);
    }

    if total_targets > MAX_TARGETED_OCCURRENCES || total_context_chars > MAX_TARGETED_CONTEXT_CHARS {
        return None;
    }

    Some(TargetedRevisionPlan {
        packs,
    })
}

fn build_context_pack(doc: &Document, remark: &Remark) -> Option<RevisionContextPack> {
    let primary = build_target(doc, &remark.anchor, &remark.selected_text, "primary target")?;
    let occurrence_targets: Vec<ContextTarget> = remark
        .occurrence_anchors
        .iter()
        .filter_map(|(anchor, snippet)| build_target(doc, anchor, snippet, "related occurrence"))
        .collect();

    let (node_idx, _) = resolve_anchor(doc, &remark.anchor)?;
    let local_context_nodes = collect_local_context_nodes(doc, node_idx, &remark.anchor);

    Some(RevisionContextPack {
        primary_anchor: remark.anchor.clone(),
        generation: doc.generation,
        instruction: remark.text.clone(),
        primary_target: primary,
        occurrence_targets,
        local_context_nodes,
        list_context: remark.list_context.clone(),
    })
}

fn build_target(
    doc: &Document,
    anchor: &str,
    selected_text: &str,
    role: &str,
) -> Option<ContextTarget> {
    let (node_idx, line_idx) = resolve_anchor(doc, anchor)?;
    let raw_markdown = match line_idx {
        Some(line_idx) => match &doc.nodes.get(node_idx)?.kind {
            NodeKind::CodeBlock { code, .. } => code.lines().nth(line_idx)?.to_string(),
            _ => return None,
        },
        None => doc.raw.get(doc.nodes[node_idx].source_start..doc.nodes[node_idx].source_end)?.to_string(),
    };

    Some(ContextTarget {
        anchor: anchor.to_string(),
        selected_text: selected_text.to_string(),
        raw_markdown,
        role: role.to_string(),
    })
}

fn resolve_anchor(doc: &Document, anchor: &str) -> Option<(usize, Option<usize>)> {
    if let Some((node_anchor, line_str)) = anchor.split_once(":L") {
        let node_idx = *doc.anchor_map.get(node_anchor)?;
        let line_idx = line_str.parse::<usize>().ok()?;
        Some((node_idx, Some(line_idx)))
    } else {
        let node_idx = *doc.anchor_map.get(anchor)?;
        Some((node_idx, None))
    }
}

fn collect_local_context_nodes(doc: &Document, node_idx: usize, selected_anchor: &str) -> Vec<ContextNode> {
    let section_indices = section_context_indices(doc, node_idx);
    let mut seen = HashSet::new();

    section_indices
        .into_iter()
        .filter_map(|idx| {
            let node = doc.nodes.get(idx)?;
            if !seen.insert(node.anchor.clone()) {
                return None;
            }
            Some(ContextNode {
                anchor: node.anchor.clone(),
                summary: summarize_node(node),
                raw_markdown: doc.raw.get(node.source_start..node.source_end)?.to_string(),
            })
        })
        .filter(|node| node.anchor != selected_anchor)
        .collect()
}

fn section_context_indices(doc: &Document, node_idx: usize) -> Vec<usize> {
    let mut heading_idx = None;
    let mut heading_level = None;

    for idx in (0..=node_idx).rev() {
        if let Some(node) = doc.nodes.get(idx) {
            if let NodeKind::Heading { level, .. } = node.kind {
                heading_idx = Some(idx);
                heading_level = Some(level);
                break;
            }
        }
    }

    if let (Some(start), Some(level)) = (heading_idx, heading_level) {
        let mut indices = Vec::new();
        for idx in start..doc.nodes.len() {
            let node = &doc.nodes[idx];
            if idx > start {
                if let NodeKind::Heading { level: next_level, .. } = node.kind {
                    if next_level <= level {
                        break;
                    }
                }
            }
            indices.push(idx);
            if indices.len() >= MAX_SECTION_NODES {
                break;
            }
        }
        return indices;
    }

    let start = node_idx.saturating_sub(2);
    let end = (node_idx + 3).min(doc.nodes.len());
    (start..end).collect()
}

fn summarize_node(node: &crate::document::DocNode) -> String {
    match &node.kind {
        NodeKind::Heading { text, .. } => format!("Heading({})", text),
        NodeKind::Paragraph { text } => format!("Paragraph({})", text),
        NodeKind::CodeBlock { lang, .. } => format!("CodeBlock({})", lang.as_deref().unwrap_or("plain")),
        NodeKind::ListItem { text, .. } => format!("ListItem({})", text),
        NodeKind::BlockQuote { text } => format!("BlockQuote({})", text),
        NodeKind::HorizontalRule => "HorizontalRule".to_string(),
        NodeKind::Html { .. } => "Html".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;
    use crate::remarks::{Remark, RemarkStatus, TargetType};
    use chrono::Utc;
    use uuid::Uuid;

    fn make_doc(raw: &str) -> Document {
        let mut doc = Document {
            path: std::path::PathBuf::from("test.md"),
            raw: raw.to_string(),
            nodes: Document::parse(raw),
            anchor_map: std::collections::HashMap::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            generation: 0,
        };
        doc.anchor_map = doc.nodes.iter().enumerate().map(|(i, n)| (n.anchor.clone(), i)).collect();
        doc
    }

    #[test]
    fn builds_targeted_plan_with_occurrences() {
        let raw = "# Title\n\nalpha beta\n\nalpha beta\n";
        let doc = make_doc(raw);
        let para_anchor = doc.nodes[1].anchor.clone();
        let second_anchor = doc.nodes[2].anchor.clone();
        let remark = Remark {
            id: Uuid::new_v4(),
            anchor: para_anchor,
            selected_text: "alpha beta".to_string(),
            target_type: TargetType::Paragraph,
            text: "Remove all occurrences".to_string(),
            list_context: None,
            occurrence_anchors: vec![(second_anchor, "alpha beta".to_string())],
            created_at: Utc::now(),
            status: RemarkStatus::Queued,
        };

        let plan = build_targeted_revision_plan(&doc, &[&remark]).expect("targeted plan");
        assert_eq!(plan.packs.len(), 1);
        assert_eq!(plan.packs[0].occurrence_targets.len(), 1);
    }

    #[test]
    fn falls_back_when_too_many_occurrences() {
        let raw = "# Title\n\nalpha\n\nalpha\n\nalpha\n\nalpha\n\nalpha\n\nalpha\n\nalpha\n\nalpha\n\nalpha\n\nalpha\n\nalpha\n\nalpha\n\nalpha\n\nalpha\n\nalpha\n\nalpha\n\nalpha\n\nalpha\n\nalpha\n\nalpha\n\nalpha\n\nalpha\n\nalpha\n\nalpha\n\nalpha\n\nalpha\n";
        let doc = make_doc(raw);
        let primary_anchor = doc.nodes[1].anchor.clone();
        let occurrence_anchors = doc
            .nodes
            .iter()
            .skip(2)
            .filter_map(|node| match node.kind {
                NodeKind::Paragraph { .. } => Some((node.anchor.clone(), "alpha".to_string())),
                _ => None,
            })
            .collect::<Vec<_>>();

        let remark = Remark {
            id: Uuid::new_v4(),
            anchor: primary_anchor,
            selected_text: "alpha".to_string(),
            target_type: TargetType::Paragraph,
            text: "Remove all occurrences".to_string(),
            list_context: None,
            occurrence_anchors,
            created_at: Utc::now(),
            status: RemarkStatus::Queued,
        };

        assert!(build_targeted_revision_plan(&doc, &[&remark]).is_none());
    }
}
