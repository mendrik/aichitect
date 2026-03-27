#![allow(dead_code)]
pub mod patch;
pub use patch::PatchOp;

use anyhow::Result;
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::fs;

pub type AnchorId = String;

#[derive(Debug, Clone)]
pub struct Document {
    pub path: PathBuf,
    pub raw: String,
    pub nodes: Vec<DocNode>,
    pub anchor_map: HashMap<AnchorId, usize>,
    pub undo_stack: Vec<String>,
    pub redo_stack: Vec<String>,
    pub generation: u64,
}

#[derive(Debug, Clone)]
pub struct DocNode {
    pub anchor: AnchorId,
    pub kind: NodeKind,
    pub source_start: usize,
    pub source_end: usize,
}

#[derive(Debug, Clone)]
pub enum NodeKind {
    Heading { level: u8, text: String },
    Paragraph { text: String },
    CodeBlock { lang: Option<String>, code: String },
    ListItem { ordered: bool, text: String },
    BlockQuote { text: String },
    HorizontalRule,
    Html { content: String },
}

#[derive(Debug, Clone)]
pub struct StyledLine {
    pub text: String,
    pub spans: Vec<StyledSpan>,
    pub anchor: Option<AnchorId>,
    pub node_index: Option<usize>,
    /// Index of this line within its parent code block (inner content lines only).
    pub line_in_block: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct StyledSpan {
    pub text: String,
    pub style: SpanStyle,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SpanStyle {
    Normal,
    Bold,
    Italic,
    Code,
    Heading(u8),
    CodeBlockLine,
    BlockQuote,
    Dimmed,
    Error,
}

impl Document {
    pub fn load(path: PathBuf) -> Result<Self> {
        let raw = fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("Failed to read {}: {}", path.display(), e))?;
        let nodes = Self::parse(&raw);
        let anchor_map = Self::build_anchor_map(&nodes);
        Ok(Document {
            path,
            raw,
            nodes,
            anchor_map,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            generation: 0,
        })
    }

    /// Create an empty document for a path that does not yet exist.
    pub fn empty(path: PathBuf) -> Self {
        Document {
            path,
            raw: String::new(),
            nodes: Vec::new(),
            anchor_map: HashMap::new(),
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
            generation: 0,
        }
    }

    /// True when the document has not been populated yet (new-file creation flow).
    pub fn is_new(&self) -> bool {
        self.raw.is_empty()
    }

    /// Replace the entire document content (used after AI doc creation).
    pub fn set_content(&mut self, raw: String) -> Result<()> {
        self.push_undo();
        self.raw = raw;
        self.nodes = Self::parse(&self.raw);
        self.anchor_map = Self::build_anchor_map(&self.nodes);
        Ok(())
    }

    pub fn parse(raw: &str) -> Vec<DocNode> {
        let options = Options::all();
        let parser = Parser::new_ext(raw, options);
        let iter = parser.into_offset_iter();

        let mut nodes = Vec::new();
        let mut counters: HashMap<String, usize> = HashMap::new();

        let events: Vec<(Event, std::ops::Range<usize>)> = iter.collect();

        let mut i = 0;
        while i < events.len() {
            let (ref event, ref range) = events[i];
            match event {
                Event::Start(Tag::Heading { level, .. }) => {
                    let lvl = heading_level_to_u8(*level);
                    let start = range.start;
                    let mut text = String::new();
                    let mut end = range.end;
                    i += 1;
                    while i < events.len() {
                        match &events[i].0 {
                            Event::End(TagEnd::Heading(_)) => {
                                end = events[i].1.end;
                                break;
                            }
                            Event::Text(t) | Event::Code(t) => text.push_str(t),
                            _ => {}
                        }
                        i += 1;
                    }
                    let slug = slugify(&text, 30);
                    let prefix = format!("h{}-{}", lvl, slug);
                    let anchor = unique_anchor(&prefix, &mut counters);
                    nodes.push(DocNode { anchor, kind: NodeKind::Heading { level: lvl, text }, source_start: start, source_end: end });
                }
                Event::Start(Tag::Paragraph) => {
                    let start = range.start;
                    let mut text = String::new();
                    let mut end = range.end;
                    i += 1;
                    while i < events.len() {
                        match &events[i].0 {
                            Event::End(TagEnd::Paragraph) => {
                                end = events[i].1.end;
                                break;
                            }
                            Event::Text(t) => text.push_str(t),
                            Event::Code(t) => { text.push('`'); text.push_str(t); text.push('`'); }
                            Event::SoftBreak => text.push(' '),
                            Event::HardBreak => text.push('\n'),
                            _ => {}
                        }
                        i += 1;
                    }
                    let anchor = unique_anchor("p", &mut counters);
                    nodes.push(DocNode { anchor, kind: NodeKind::Paragraph { text }, source_start: start, source_end: end });
                }
                Event::Start(Tag::CodeBlock(kind)) => {
                    let start = range.start;
                    let lang = match kind {
                        pulldown_cmark::CodeBlockKind::Fenced(l) => {
                            let s = l.to_string();
                            if s.is_empty() { None } else { Some(s) }
                        }
                        pulldown_cmark::CodeBlockKind::Indented => None,
                    };
                    let lang_slug = lang.as_deref().unwrap_or("").replace(' ', "-");
                    let prefix = if lang_slug.is_empty() { "cb".to_string() } else { format!("cb-{}", &lang_slug[..lang_slug.len().min(15)]) };
                    let mut code = String::new();
                    let mut end = range.end;
                    i += 1;
                    while i < events.len() {
                        match &events[i].0 {
                            Event::End(TagEnd::CodeBlock) => {
                                end = events[i].1.end;
                                break;
                            }
                            Event::Text(t) => code.push_str(t),
                            _ => {}
                        }
                        i += 1;
                    }
                    let anchor = unique_anchor(&prefix, &mut counters);
                    nodes.push(DocNode { anchor, kind: NodeKind::CodeBlock { lang, code }, source_start: start, source_end: end });
                }
                Event::Start(Tag::Item) => {
                    let start = range.start;
                    let mut text = String::new();
                    let mut end = range.end;
                    i += 1;
                    while i < events.len() {
                        match &events[i].0 {
                            Event::End(TagEnd::Item) => {
                                end = events[i].1.end;
                                break;
                            }
                            Event::Text(t) => text.push_str(t),
                            Event::Code(t) => { text.push('`'); text.push_str(t); text.push('`'); }
                            Event::SoftBreak => text.push(' '),
                            _ => {}
                        }
                        i += 1;
                    }
                    let anchor = unique_anchor("li", &mut counters);
                    nodes.push(DocNode { anchor, kind: NodeKind::ListItem { ordered: false, text }, source_start: start, source_end: end });
                }
                Event::Start(Tag::BlockQuote(_)) => {
                    let start = range.start;
                    let mut text = String::new();
                    let mut end = range.end;
                    i += 1;
                    while i < events.len() {
                        match &events[i].0 {
                            Event::End(TagEnd::BlockQuote(_)) => {
                                end = events[i].1.end;
                                break;
                            }
                            Event::Text(t) => text.push_str(t),
                            _ => {}
                        }
                        i += 1;
                    }
                    let anchor = unique_anchor("bq", &mut counters);
                    nodes.push(DocNode { anchor, kind: NodeKind::BlockQuote { text }, source_start: start, source_end: end });
                }
                Event::Rule => {
                    let anchor = unique_anchor("hr", &mut counters);
                    nodes.push(DocNode { anchor, kind: NodeKind::HorizontalRule, source_start: range.start, source_end: range.end });
                }
                Event::Html(content) => {
                    let anchor = unique_anchor("html", &mut counters);
                    nodes.push(DocNode { anchor, kind: NodeKind::Html { content: content.to_string() }, source_start: range.start, source_end: range.end });
                }
                _ => {}
            }
            i += 1;
        }
        nodes
    }

    fn build_anchor_map(nodes: &[DocNode]) -> HashMap<AnchorId, usize> {
        nodes.iter().enumerate().map(|(i, n)| (n.anchor.clone(), i)).collect()
    }

    pub fn save(&self) -> Result<()> {
        fs::write(&self.path, &self.raw)?;
        Ok(())
    }

    pub fn push_undo(&mut self) {
        self.undo_stack.push(self.raw.clone());
        self.redo_stack.clear();
        self.generation += 1;
    }

    pub fn undo(&mut self) -> bool {
        if let Some(prev) = self.undo_stack.pop() {
            self.redo_stack.push(self.raw.clone());
            self.raw = prev;
            self.nodes = Self::parse(&self.raw);
            self.anchor_map = Self::build_anchor_map(&self.nodes);
            self.generation += 1;
            true
        } else {
            false
        }
    }

    pub fn redo(&mut self) -> bool {
        if let Some(next) = self.redo_stack.pop() {
            self.undo_stack.push(self.raw.clone());
            self.raw = next;
            self.nodes = Self::parse(&self.raw);
            self.anchor_map = Self::build_anchor_map(&self.nodes);
            self.generation += 1;
            true
        } else {
            false
        }
    }

    pub fn content_snapshot(&self) -> HashMap<AnchorId, String> {
        self.nodes
            .iter()
            .map(|n| {
                (
                    n.anchor.clone(),
                    self.raw[n.source_start..n.source_end].to_string(),
                )
            })
            .collect()
    }

    pub fn apply_patches(
        &mut self,
        patches: Vec<PatchOp>,
        expected_content: Option<&HashMap<AnchorId, String>>,
    ) -> Result<(Vec<String>, Vec<String>)> {
        self.push_undo();

        let mut applied = Vec::new();
        let mut skipped = Vec::new();
        let mut ops_with_pos: Vec<(usize, usize, PatchOp)> = Vec::new();

        for patch in patches {
            let anchor = patch.anchor().to_string();
            if let Some(&idx) = self.anchor_map.get(&anchor) {
                let node = &self.nodes[idx];
                if let Some(snapshot) = expected_content {
                    let current = &self.raw[node.source_start..node.source_end];
                    if let Some(expected) = snapshot.get(&anchor) {
                        if current != expected {
                            skipped.push(anchor);
                            continue;
                        }
                    }
                }
                ops_with_pos.push((node.source_start, node.source_end, patch));
            } else {
                skipped.push(anchor);
            }
        }

        ops_with_pos.sort_by(|a, b| b.0.cmp(&a.0));

        let mut raw = self.raw.clone();
        for (start, end, op) in ops_with_pos {
            let replacement = match &op {
                PatchOp::ReplaceSection { content, .. } => content.clone(),
                PatchOp::ReplaceTextSpan { content, .. } => content.clone(),
                PatchOp::ReplaceCodeBlock { content, lang, .. } => {
                    let l = lang.as_deref().unwrap_or("");
                    format!("```{}\n{}\n```\n", l, content)
                }
                PatchOp::InsertAfter { content, .. } => {
                    let original = raw[start..end].to_string();
                    format!("{}\n{}", original, content)
                }
                PatchOp::InsertBefore { content, .. } => {
                    let original = raw[start..end].to_string();
                    format!("{}\n{}", content, original)
                }
                PatchOp::DeleteBlock { .. } => String::new(),
                PatchOp::UpdateHeadingText { new_text, .. } => {
                    if let Some(&idx) = self.anchor_map.get(op.anchor()) {
                        if let NodeKind::Heading { level, .. } = self.nodes[idx].kind {
                            format!("{} {}\n", "#".repeat(level as usize), new_text)
                        } else {
                            new_text.clone()
                        }
                    } else {
                        new_text.clone()
                    }
                }
                PatchOp::UpdateListItem { new_text, .. } => {
                    format!("- {}\n", new_text)
                }
            };
            raw.replace_range(start..end, &replacement);
            applied.push(op.anchor().to_string());
        }

        self.raw = raw;
        self.nodes = Self::parse(&self.raw);
        self.anchor_map = Self::build_anchor_map(&self.nodes);

        // Save a snapshot to ~/.aichitect/history/
        if !applied.is_empty() {
            if let Ok(store) = crate::history::HistoryStore::for_doc(&self.path) {
                let _ = store.save_snapshot(&self.raw);
            }
        }

        Ok((applied, skipped))
    }

    pub fn render_display(&self, width: usize, collapsed: &HashSet<AnchorId>) -> Vec<StyledLine> {
        let mut lines = Vec::new();
        // When Some(n), we are inside a collapsed heading of level n — skip
        // everything until the next heading at level ≤ n.
        let mut skip_until_level: Option<u8> = None;

        for (idx, node) in self.nodes.iter().enumerate() {
            // A heading at ≤ skip level ends the collapsed region.
            if let Some(skip_lvl) = skip_until_level {
                if let NodeKind::Heading { level, .. } = &node.kind {
                    if *level <= skip_lvl {
                        skip_until_level = None;
                    }
                }
                if skip_until_level.is_some() {
                    continue; // still inside a collapsed section
                }
            }

            match &node.kind {
                NodeKind::Heading { level, text } => {
                    let is_collapsed = collapsed.contains(&node.anchor);
                    let icon = if is_collapsed { "▶ " } else { "▼ " };
                    let icon_style = if is_collapsed { SpanStyle::Heading(*level) } else { SpanStyle::Dimmed };
                    let prefix = "#".repeat(*level as usize);
                    let heading_text = format!("{} {}", prefix, text);
                    lines.push(StyledLine {
                        text: format!("{}{}", icon, heading_text),
                        spans: vec![
                            StyledSpan { text: icon.to_string(), style: icon_style },
                            StyledSpan { text: heading_text, style: SpanStyle::Heading(*level) },
                        ],
                        anchor: Some(node.anchor.clone()),
                        node_index: Some(idx),
                        line_in_block: None,
                    });
                    lines.push(StyledLine { text: String::new(), spans: vec![], anchor: None, node_index: None, line_in_block: None });
                    if is_collapsed {
                        skip_until_level = Some(*level);
                    }
                }
                NodeKind::Paragraph { text } => {
                    let wrapped = word_wrap(text, width.saturating_sub(2));
                    for (li, line) in wrapped.into_iter().enumerate() {
                        lines.push(StyledLine {
                            text: line.clone(),
                            spans: vec![StyledSpan { text: line, style: SpanStyle::Normal }],
                            anchor: if li == 0 { Some(node.anchor.clone()) } else { None },
                            node_index: if li == 0 { Some(idx) } else { None },
                            line_in_block: None,
                        });
                    }
                    lines.push(StyledLine { text: String::new(), spans: vec![], anchor: None, node_index: None, line_in_block: None });
                }
                NodeKind::CodeBlock { lang, code } => {
                    let lang_str = lang.as_deref().unwrap_or("");
                    let header = format!("```{}", lang_str);
                    lines.push(StyledLine {
                        text: header.clone(),
                        spans: vec![StyledSpan { text: header, style: SpanStyle::Dimmed }],
                        anchor: Some(node.anchor.clone()),
                        node_index: Some(idx),
                        line_in_block: None,
                    });
                    for (li, cline) in code.lines().enumerate() {
                        lines.push(StyledLine {
                            text: cline.to_string(),
                            spans: vec![StyledSpan { text: cline.to_string(), style: SpanStyle::CodeBlockLine }],
                            anchor: None,
                            node_index: Some(idx),
                            line_in_block: Some(li),
                        });
                    }
                    lines.push(StyledLine {
                        text: "```".to_string(),
                        spans: vec![StyledSpan { text: "```".to_string(), style: SpanStyle::Dimmed }],
                        anchor: None,
                        node_index: Some(idx),
                        line_in_block: None,
                    });
                    lines.push(StyledLine { text: String::new(), spans: vec![], anchor: None, node_index: None, line_in_block: None });
                }
                NodeKind::ListItem { ordered, text } => {
                    let bullet = if *ordered { "1." } else { "•" };
                    let line_text = format!("  {} {}", bullet, text);
                    lines.push(StyledLine {
                        text: line_text.clone(),
                        spans: vec![StyledSpan { text: line_text, style: SpanStyle::Normal }],
                        anchor: Some(node.anchor.clone()),
                        node_index: Some(idx),
                        line_in_block: None,
                    });
                    // Blank line after the last item in a list.
                    let next_is_list = self.nodes.get(idx + 1)
                        .map(|n| matches!(n.kind, NodeKind::ListItem { .. }))
                        .unwrap_or(false);
                    if !next_is_list {
                        lines.push(StyledLine { text: String::new(), spans: vec![], anchor: None, node_index: None, line_in_block: None });
                    }
                }
                NodeKind::BlockQuote { text } => {
                    let line_text = format!("▌ {}", text);
                    lines.push(StyledLine {
                        text: line_text.clone(),
                        spans: vec![StyledSpan { text: line_text, style: SpanStyle::BlockQuote }],
                        anchor: Some(node.anchor.clone()),
                        node_index: Some(idx),
                        line_in_block: None,
                    });
                    lines.push(StyledLine { text: String::new(), spans: vec![], anchor: None, node_index: None, line_in_block: None });
                }
                NodeKind::HorizontalRule => {
                    let line_text = "─".repeat(width.min(60));
                    lines.push(StyledLine {
                        text: line_text.clone(),
                        spans: vec![StyledSpan { text: line_text, style: SpanStyle::Dimmed }],
                        anchor: Some(node.anchor.clone()),
                        node_index: Some(idx),
                        line_in_block: None,
                    });
                }
                NodeKind::Html { content } => {
                    lines.push(StyledLine {
                        text: content.clone(),
                        spans: vec![StyledSpan { text: content.clone(), style: SpanStyle::Dimmed }],
                        anchor: Some(node.anchor.clone()),
                        node_index: Some(idx),
                        line_in_block: None,
                    });
                }
            }
        }
        lines
    }

    /// Returns the indices of nodes that are currently visible (not hidden
    /// inside a collapsed section).  Used by J/K navigation so the cursor
    /// never lands on a hidden node.
    pub fn visible_node_indices(&self, collapsed: &HashSet<AnchorId>) -> Vec<usize> {
        let mut visible = Vec::new();
        let mut skip_until_level: Option<u8> = None;
        for (idx, node) in self.nodes.iter().enumerate() {
            if let Some(skip_lvl) = skip_until_level {
                if let NodeKind::Heading { level, .. } = &node.kind {
                    if *level <= skip_lvl {
                        skip_until_level = None;
                    }
                }
                if skip_until_level.is_some() {
                    continue;
                }
            }
            visible.push(idx);
            if let NodeKind::Heading { level, .. } = &node.kind {
                if collapsed.contains(&node.anchor) {
                    skip_until_level = Some(*level);
                }
            }
        }
        visible
    }

    pub fn anchor_map_display(&self) -> String {
        self.nodes.iter()
            .map(|n| format!("{}: {:?}", n.anchor, match &n.kind {
                NodeKind::Heading { text, .. } => format!("Heading({})", &text[..text.len().min(40)]),
                NodeKind::Paragraph { text } => format!("Paragraph({})", &text[..text.len().min(40)]),
                NodeKind::CodeBlock { lang, .. } => format!("CodeBlock({:?})", lang),
                NodeKind::ListItem { text, .. } => format!("ListItem({})", &text[..text.len().min(40)]),
                NodeKind::BlockQuote { text } => format!("BlockQuote({})", &text[..text.len().min(40)]),
                NodeKind::HorizontalRule => "HorizontalRule".to_string(),
                NodeKind::Html { .. } => "Html".to_string(),
            }))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Find all nodes (and code-block lines) whose text contains `query`
    /// (case-insensitive), excluding the node identified by `exclude_anchor`.
    /// Returns `(anchor_id, text_snippet)` pairs, capped at 50 results.
    pub fn find_occurrences(
        &self,
        query: &str,
        exclude_anchor: Option<&str>,
    ) -> Vec<(AnchorId, String)> {
        if query.trim().is_empty() {
            return vec![];
        }
        let q = query.to_lowercase();
        let mut hits = Vec::new();

        for node in &self.nodes {
            if exclude_anchor == Some(node.anchor.as_str()) {
                continue;
            }
            match &node.kind {
                NodeKind::Heading { text, .. }
                | NodeKind::Paragraph { text }
                | NodeKind::ListItem { text, .. }
                | NodeKind::BlockQuote { text } => {
                    if text.to_lowercase().contains(&q) {
                        let snippet = if text.len() > 80 {
                            format!("{}…", &text[..80])
                        } else {
                            text.clone()
                        };
                        hits.push((node.anchor.clone(), snippet));
                    }
                }
                NodeKind::CodeBlock { code, .. } => {
                    for (line_num, line) in code.lines().enumerate() {
                        if line.to_lowercase().contains(&q) {
                            let line_anchor = format!("{}:L{}", node.anchor, line_num);
                            // Skip if this specific line was the excluded anchor.
                            if exclude_anchor == Some(line_anchor.as_str()) {
                                continue;
                            }
                            hits.push((line_anchor, line.trim_end().to_string()));
                        }
                    }
                }
                _ => {}
            }
            if hits.len() >= 50 {
                break;
            }
        }
        hits
    }
}

fn heading_level_to_u8(level: pulldown_cmark::HeadingLevel) -> u8 {
    match level {
        pulldown_cmark::HeadingLevel::H1 => 1,
        pulldown_cmark::HeadingLevel::H2 => 2,
        pulldown_cmark::HeadingLevel::H3 => 3,
        pulldown_cmark::HeadingLevel::H4 => 4,
        pulldown_cmark::HeadingLevel::H5 => 5,
        pulldown_cmark::HeadingLevel::H6 => 6,
    }
}

fn slugify(text: &str, max_len: usize) -> String {
    let s: String = text.chars()
        .map(|c| if c.is_alphanumeric() { c.to_lowercase().next().unwrap_or(c) } else { '-' })
        .collect();
    let s = s.trim_matches('-').to_string();
    let mut result = String::new();
    let mut last_dash = false;
    for c in s.chars() {
        if c == '-' {
            if !last_dash { result.push(c); }
            last_dash = true;
        } else {
            result.push(c);
            last_dash = false;
        }
    }
    let trimmed = result[..result.len().min(max_len)].trim_end_matches('-').to_string();
    trimmed
}

fn unique_anchor(prefix: &str, counters: &mut HashMap<String, usize>) -> String {
    let count = counters.entry(prefix.to_string()).or_insert(0);
    let anchor = if *count == 0 && prefix.starts_with('h') {
        prefix.to_string()
    } else {
        format!("{}-{}", prefix, count)
    };
    *count += 1;
    anchor
}

fn word_wrap(text: &str, width: usize) -> Vec<String> {
    if width == 0 { return vec![text.to_string()]; }
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        if current.is_empty() {
            current.push_str(word);
        } else if current.len() + 1 + word.len() <= width {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
        }
    }
    if !current.is_empty() { lines.push(current); }
    if lines.is_empty() { lines.push(String::new()); }
    lines
}
