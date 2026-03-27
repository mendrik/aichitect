use super::AnchorId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum PatchOp {
    ReplaceSection { anchor: AnchorId, content: String, rationale: String },
    ReplaceTextSpan { anchor: AnchorId, content: String, rationale: String },
    ReplaceCodeBlock { anchor: AnchorId, content: String, lang: Option<String>, rationale: String },
    InsertAfter { anchor: AnchorId, content: String, rationale: String },
    InsertBefore { anchor: AnchorId, content: String, rationale: String },
    DeleteBlock { anchor: AnchorId, rationale: String },
    UpdateHeadingText { anchor: AnchorId, new_text: String, rationale: String },
    UpdateListItem { anchor: AnchorId, new_text: String, rationale: String },
}

impl PatchOp {
    pub fn anchor(&self) -> &AnchorId {
        match self {
            PatchOp::ReplaceSection { anchor, .. } => anchor,
            PatchOp::ReplaceTextSpan { anchor, .. } => anchor,
            PatchOp::ReplaceCodeBlock { anchor, .. } => anchor,
            PatchOp::InsertAfter { anchor, .. } => anchor,
            PatchOp::InsertBefore { anchor, .. } => anchor,
            PatchOp::DeleteBlock { anchor, .. } => anchor,
            PatchOp::UpdateHeadingText { anchor, .. } => anchor,
            PatchOp::UpdateListItem { anchor, .. } => anchor,
        }
    }

    #[allow(dead_code)]
    pub fn rationale(&self) -> &str {
        match self {
            PatchOp::ReplaceSection { rationale, .. } => rationale,
            PatchOp::ReplaceTextSpan { rationale, .. } => rationale,
            PatchOp::ReplaceCodeBlock { rationale, .. } => rationale,
            PatchOp::InsertAfter { rationale, .. } => rationale,
            PatchOp::InsertBefore { rationale, .. } => rationale,
            PatchOp::DeleteBlock { rationale, .. } => rationale,
            PatchOp::UpdateHeadingText { rationale, .. } => rationale,
            PatchOp::UpdateListItem { rationale, .. } => rationale,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::Document;

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
    fn test_replace_section_patch() {
        let raw = "# Introduction\n\nThis is a paragraph.\n\n## Details\n\nMore text.\n";
        let mut doc = make_doc(raw);

        let para_anchor = doc.nodes.iter()
            .find(|n| matches!(&n.kind, crate::document::NodeKind::Paragraph { text } if text.contains("paragraph")))
            .map(|n| n.anchor.clone())
            .expect("paragraph node");

        let patches = vec![PatchOp::ReplaceSection {
            anchor: para_anchor.clone(),
            content: "This is a replaced paragraph.\n".to_string(),
            rationale: "test".to_string(),
        }];

        let (applied, _skipped) = doc.apply_patches(patches, None).unwrap();
        assert!(applied.contains(&para_anchor));
        assert!(doc.raw.contains("replaced paragraph"));
        assert!(!doc.raw.contains("This is a paragraph."));
    }

    #[test]
    fn test_insert_after_patch() {
        let raw = "# Title\n\nFirst paragraph.\n\nSecond paragraph.\n";
        let mut doc = make_doc(raw);

        let first_para = doc.nodes.iter()
            .find(|n| matches!(&n.kind, crate::document::NodeKind::Paragraph { text } if text.contains("First")))
            .map(|n| n.anchor.clone())
            .expect("first paragraph");

        let patches = vec![PatchOp::InsertAfter {
            anchor: first_para.clone(),
            content: "Inserted paragraph.\n".to_string(),
            rationale: "test".to_string(),
        }];

        let (applied, _skipped) = doc.apply_patches(patches, None).unwrap();
        assert!(applied.contains(&first_para));
        assert!(doc.raw.contains("Inserted paragraph."));
        let first_pos = doc.raw.find("First paragraph").unwrap();
        let inserted_pos = doc.raw.find("Inserted paragraph").unwrap();
        assert!(inserted_pos > first_pos);
    }

    #[test]
    fn test_anchor_stability() {
        let raw = "# Title\n\nParagraph one.\n\n## Section\n\nParagraph two.\n";
        let mut doc = make_doc(raw);

        let section_anchor = doc.nodes.iter()
            .find(|n| matches!(&n.kind, crate::document::NodeKind::Heading { text, .. } if text.contains("Section")))
            .map(|n| n.anchor.clone())
            .expect("section heading");

        let para1_anchor = doc.nodes.iter()
            .find(|n| matches!(&n.kind, crate::document::NodeKind::Paragraph { text } if text.contains("one")))
            .map(|n| n.anchor.clone())
            .expect("para one");

        let patches = vec![PatchOp::ReplaceSection {
            anchor: para1_anchor.clone(),
            content: "Paragraph one - updated.\n".to_string(),
            rationale: "test".to_string(),
        }];

        doc.apply_patches(patches, None).unwrap();
        assert!(doc.anchor_map.contains_key(&section_anchor));
    }

    #[test]
    fn test_patch_applied_even_when_content_changed() {
        let raw = "# Title\n\nOriginal paragraph.\n\n## Section\n\nMore text.\n";
        let mut doc = make_doc(raw);

        let para_anchor = doc.nodes.iter()
            .find(|n| matches!(&n.kind, crate::document::NodeKind::Paragraph { text } if text.contains("Original")))
            .map(|n| n.anchor.clone())
            .expect("paragraph node");

        let snapshot = doc.content_snapshot();

        doc.set_content("# Title\n\nChanged paragraph.\n\n## Section\n\nMore text.\n".to_string()).unwrap();

        let patches = vec![PatchOp::ReplaceSection {
            anchor: para_anchor.clone(),
            content: "AI replacement.\n".to_string(),
            rationale: "test".to_string(),
        }];

        let (applied, skipped) = doc.apply_patches(patches, Some(&snapshot)).unwrap();
        assert!(applied.contains(&para_anchor), "patch should apply despite content change");
        assert!(skipped.is_empty());
        assert!(doc.raw.contains("AI replacement."));
        assert!(!doc.raw.contains("Changed paragraph."));
    }

    #[test]
    fn test_patch_applied_when_content_unchanged() {
        let raw = "# Title\n\nOriginal paragraph.\n\n## Section\n\nMore text.\n";
        let mut doc = make_doc(raw);

        let para_anchor = doc.nodes.iter()
            .find(|n| matches!(&n.kind, crate::document::NodeKind::Paragraph { text } if text.contains("Original")))
            .map(|n| n.anchor.clone())
            .expect("paragraph node");

        let snapshot = doc.content_snapshot();

        let patches = vec![PatchOp::ReplaceSection {
            anchor: para_anchor.clone(),
            content: "AI replacement.\n".to_string(),
            rationale: "test".to_string(),
        }];

        let (applied, skipped) = doc.apply_patches(patches, Some(&snapshot)).unwrap();
        assert!(applied.contains(&para_anchor));
        assert!(skipped.is_empty());
        assert!(doc.raw.contains("AI replacement."));
    }

    #[test]
    fn test_partial_apply_mixed_stale_and_fresh() {
        let raw = "# Title\n\nParagraph one.\n\n## Section\n\nParagraph two.\n";
        let mut doc = make_doc(raw);

        let para1_anchor = doc.nodes.iter()
            .find(|n| matches!(&n.kind, crate::document::NodeKind::Paragraph { text } if text.contains("one")))
            .map(|n| n.anchor.clone())
            .expect("para one");

        let para2_anchor = doc.nodes.iter()
            .find(|n| matches!(&n.kind, crate::document::NodeKind::Paragraph { text } if text.contains("two")))
            .map(|n| n.anchor.clone())
            .expect("para two");

        let snapshot = doc.content_snapshot();

        doc.set_content("# Title\n\nChanged paragraph one.\n\n## Section\n\nParagraph two.\n".to_string()).unwrap();

        let patches = vec![
            PatchOp::ReplaceSection {
                anchor: para1_anchor.clone(),
                content: "AI para one.\n".to_string(),
                rationale: "test".to_string(),
            },
            PatchOp::ReplaceSection {
                anchor: para2_anchor.clone(),
                content: "AI para two.\n".to_string(),
                rationale: "test".to_string(),
            },
        ];

        let (applied, skipped) = doc.apply_patches(patches, Some(&snapshot)).unwrap();
        assert!(applied.contains(&para1_anchor), "anchor still exists so patch applies");
        assert!(applied.contains(&para2_anchor));
        assert!(skipped.is_empty());
        assert!(doc.raw.contains("AI para one."));
        assert!(doc.raw.contains("AI para two."));
    }

    #[test]
    fn test_missing_anchor_skipped() {
        let raw = "# Title\n\nParagraph one.\n";
        let mut doc = make_doc(raw);

        let snapshot = doc.content_snapshot();

        let patches = vec![PatchOp::ReplaceSection {
            anchor: "nonexistent-anchor".to_string(),
            content: "Ghost.\n".to_string(),
            rationale: "test".to_string(),
        }];

        let (applied, skipped) = doc.apply_patches(patches, Some(&snapshot)).unwrap();
        assert!(applied.is_empty());
        assert!(skipped.contains(&"nonexistent-anchor".to_string()));
    }
}
