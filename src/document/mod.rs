#![allow(dead_code)]
pub mod patch;
pub mod highlight;
pub use patch::PatchOp;

use anyhow::Result;
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::collections::hash_map::DefaultHasher;
use std::path::PathBuf;
use std::fs;

pub type AnchorId = String;

pub fn truncate_chars(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

#[derive(Debug, Clone)]
pub struct BlockFingerprint {
    pub hash: u64,
    pub raw: String,
}

impl BlockFingerprint {
    pub fn from_raw(raw: &str) -> Self {
        let mut hasher = DefaultHasher::new();
        raw.hash(&mut hasher);
        BlockFingerprint { hash: hasher.finish(), raw: raw.to_string() }
    }
}

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
    ListItem { ordered: bool, index: usize, checked: Option<bool>, text: String },
    BlockQuote { text: String },
    HorizontalRule,
    Html { content: String },
    Table { headers: Vec<String>, rows: Vec<Vec<String>> },
}

#[derive(Debug, Clone)]
pub struct StyledLine {
    pub text: String,
    pub spans: Vec<StyledSpan>,
    pub anchor: Option<AnchorId>,
    pub node_index: Option<usize>,
    /// Index of this line within its parent code block (inner content lines only).
    pub line_in_block: Option<usize>,
    /// URL of the first hyperlink in this line, if any.
    pub first_link_url: Option<String>,
}

#[derive(Debug, Clone)]
pub struct StyledSpan {
    pub text: String,
    pub style: SpanStyle,
    pub cell_col: Option<usize>,
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
    TableHeader,
    TableBorder,
    Keyword,
    StringLit,
    Comment,
    Number,
    Operator,
    Bracket,
    /// Inline hyperlink — the String payload is the URL.
    Link(String),
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
        let mut ordered_stack: Vec<(bool, usize)> = Vec::new();
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
                Event::Start(Tag::List(start_num)) => {
                    let start_idx = start_num.unwrap_or(1) as usize;
                    ordered_stack.push((start_num.is_some(), start_idx));
                }
                Event::End(TagEnd::List(_)) => {
                    ordered_stack.pop();
                }
                Event::Start(Tag::Item) => {
                    let start = range.start;
                    let mut text = String::new();
                    let mut checked: Option<bool> = None;
                    let mut end = range.end;
                    let (ordered, index) = match ordered_stack.last_mut() {
                        Some((is_ord, idx)) => {
                            let cur = *idx;
                            *idx += 1;
                            (*is_ord, cur)
                        }
                        None => (false, 1),
                    };
                    i += 1;
                    while i < events.len() {
                        match &events[i].0 {
                            Event::End(TagEnd::Item) => {
                                end = events[i].1.end;
                                break;
                            }
                            Event::TaskListMarker(c) => { checked = Some(*c); }
                            Event::Text(t) => text.push_str(t),
                            Event::Code(t) => { text.push('`'); text.push_str(t); text.push('`'); }
                            Event::SoftBreak => text.push(' '),
                            _ => {}
                        }
                        i += 1;
                    }
                    let anchor = unique_anchor("li", &mut counters);
                    nodes.push(DocNode { anchor, kind: NodeKind::ListItem { ordered, index, checked, text }, source_start: start, source_end: end });
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
                Event::Start(Tag::Table(_)) => {
                    let start = range.start;
                    let mut headers: Vec<String> = Vec::new();
                    let mut rows: Vec<Vec<String>> = Vec::new();
                    let mut end = range.end;
                    let mut in_head = false;
                    let mut current_row: Vec<String> = Vec::new();
                    let mut current_cell = String::new();
                    i += 1;
                    while i < events.len() {
                        match &events[i].0 {
                            Event::End(TagEnd::Table) => { end = events[i].1.end; break; }
                            Event::Start(Tag::TableHead) => { in_head = true; }
                            Event::End(TagEnd::TableHead) => {
                                if !current_row.is_empty() {
                                    headers = std::mem::take(&mut current_row);
                                }
                                in_head = false;
                            }
                            Event::Start(Tag::TableRow) => { current_row = Vec::new(); }
                            Event::End(TagEnd::TableRow) => {
                                if !in_head && !current_row.is_empty() {
                                    rows.push(std::mem::take(&mut current_row));
                                }
                            }
                            Event::Start(Tag::TableCell) => { current_cell = String::new(); }
                            Event::End(TagEnd::TableCell) => {
                                current_row.push(std::mem::take(&mut current_cell));
                            }
                            Event::Text(t) => { current_cell.push_str(t); }
                            Event::Code(t) => { current_cell.push('`'); current_cell.push_str(t); current_cell.push('`'); }
                            _ => {}
                        }
                        i += 1;
                    }
                    let anchor = unique_anchor("tbl", &mut counters);
                    nodes.push(DocNode { anchor, kind: NodeKind::Table { headers, rows }, source_start: start, source_end: end });
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

    pub fn content_snapshot(&self) -> HashMap<AnchorId, BlockFingerprint> {
        self.nodes
            .iter()
            .map(|n| {
                let raw = &self.raw[n.source_start..n.source_end];
                (n.anchor.clone(), BlockFingerprint::from_raw(raw))
            })
            .collect()
    }

    /// Build a map from fingerprint hash to node index for fast lookup.
    fn fingerprint_index(&self) -> HashMap<u64, usize> {
        self.nodes
            .iter()
            .enumerate()
            .map(|(i, n)| {
                let raw = &self.raw[n.source_start..n.source_end];
                let fp = BlockFingerprint::from_raw(raw);
                (fp.hash, i)
            })
            .collect()
    }

    pub fn apply_patches(
        &mut self,
        patches: Vec<PatchOp>,
        expected_content: Option<&HashMap<AnchorId, BlockFingerprint>>,
    ) -> Result<(Vec<String>, Vec<String>)> {
        self.push_undo();

        let mut applied = Vec::new();
        let mut skipped = Vec::new();
        let mut ops_with_pos: Vec<(usize, usize, PatchOp)> = Vec::new();

        let fp_index = expected_content.map(|_| self.fingerprint_index());

        for patch in patches {
            let anchor = patch.anchor().to_string();

            let node_pos: Option<(usize, usize)> = if let Some(&idx) = self.anchor_map.get(&anchor) {
                let node = &self.nodes[idx];
                Some((node.source_start, node.source_end))
            } else if let Some(snapshot) = expected_content {
                // Anchor gone — try to locate the original block by fingerprint
                snapshot.get(&anchor).and_then(|fp| {
                    fp_index.as_ref()
                        .and_then(|idx_map| idx_map.get(&fp.hash))
                        .map(|&ni| (self.nodes[ni].source_start, self.nodes[ni].source_end))
                })
            } else {
                None
            };

            if let Some((start, end)) = node_pos {
                ops_with_pos.push((start, end, patch));
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
                            StyledSpan { text: icon.to_string(), style: icon_style, cell_col: None },
                            StyledSpan { text: heading_text, style: SpanStyle::Heading(*level), cell_col: None },
                        ],
                        anchor: Some(node.anchor.clone()),
                        node_index: Some(idx),
                        line_in_block: None,
                        first_link_url: None,
                    });
                    lines.push(StyledLine { text: String::new(), spans: vec![], anchor: None, node_index: None, line_in_block: None, first_link_url: None });
                    if is_collapsed {
                        skip_until_level = Some(*level);
                    }
                }
                NodeKind::Paragraph { text } => {
                    let node_raw = &self.raw[node.source_start..node.source_end];
                    let links = extract_links(node_raw);
                    let first_url = links.first().map(|(_, url)| url.clone());
                    let wrapped = word_wrap(text, width.saturating_sub(2));
                    for (li, line) in wrapped.into_iter().enumerate() {
                        let mut spans = inline_spans(&line, SpanStyle::Normal);
                        for (link_text, url) in &links {
                            mark_link_in_spans(&mut spans, link_text, url);
                        }
                        lines.push(StyledLine {
                            text: line,
                            spans,
                            anchor: if li == 0 { Some(node.anchor.clone()) } else { None },
                            node_index: if li == 0 { Some(idx) } else { None },
                            line_in_block: None,
                            first_link_url: if li == 0 { first_url.clone() } else { None },
                        });
                    }
                    lines.push(StyledLine { text: String::new(), spans: vec![], anchor: None, node_index: None, line_in_block: None, first_link_url: None });
                }
                NodeKind::CodeBlock { lang, code } => {
                    let lang_str = lang.as_deref().unwrap_or("");
                    let header = if lang_str.is_empty() {
                        "  ▌".to_string()
                    } else {
                        format!("  ▌ {}", lang_str)
                    };
                    lines.push(StyledLine {
                        text: header.clone(),
                        spans: vec![StyledSpan { text: header, style: SpanStyle::Dimmed, cell_col: None }],
                        anchor: Some(node.anchor.clone()),
                        node_index: Some(idx),
                        line_in_block: None,
                        first_link_url: None,
                    });
                    for (li, cline) in code.lines().enumerate() {
                        let hl_spans: Vec<StyledSpan> = highlight::highlight_line(lang_str, cline)
                            .into_iter()
                            .map(|(style, text)| StyledSpan { text, style, cell_col: None })
                            .collect();
                        let spans = if hl_spans.is_empty() {
                            vec![StyledSpan { text: cline.to_string(), style: SpanStyle::CodeBlockLine, cell_col: None }]
                        } else {
                            hl_spans
                        };
                        lines.push(StyledLine {
                            text: cline.to_string(),
                            spans,
                            anchor: None,
                            node_index: Some(idx),
                            line_in_block: Some(li),
                            first_link_url: None,
                        });
                    }
                    lines.push(StyledLine { text: String::new(), spans: vec![], anchor: None, node_index: None, line_in_block: None, first_link_url: None });
                }
                NodeKind::ListItem { ordered, index, checked, text } => {
                    let node_raw = &self.raw[node.source_start..node.source_end];
                    let links = extract_links(node_raw);
                    let first_url = links.first().map(|(_, url)| url.clone());
                    let bullet = if *ordered { format!("{}.", index) } else { "•".to_string() };
                    let check = match checked {
                        Some(true) => "[✓] ",
                        Some(false) => "[ ] ",
                        None => "",
                    };
                    let base_style = if matches!(checked, Some(true)) { SpanStyle::Dimmed } else { SpanStyle::Normal };
                    let prefix = format!("  {} {}", bullet, check);
                    let line_text = format!("{}{}", prefix, text);
                    let mut spans = vec![StyledSpan { text: prefix, style: base_style.clone(), cell_col: None }];
                    let mut text_spans = inline_spans(text, base_style);
                    for (link_text, url) in &links {
                        mark_link_in_spans(&mut text_spans, link_text, url);
                    }
                    spans.append(&mut text_spans);
                    lines.push(StyledLine {
                        text: line_text,
                        spans,
                        anchor: Some(node.anchor.clone()),
                        node_index: Some(idx),
                        line_in_block: None,
                        first_link_url: first_url,
                    });
                    // Blank line after the last item in a list.
                    let next_is_list = self.nodes.get(idx + 1)
                        .map(|n| matches!(n.kind, NodeKind::ListItem { .. }))
                        .unwrap_or(false);
                    if !next_is_list {
                        lines.push(StyledLine { text: String::new(), spans: vec![], anchor: None, node_index: None, line_in_block: None, first_link_url: None });
                    }
                }
                NodeKind::BlockQuote { text } => {
                    let node_raw = &self.raw[node.source_start..node.source_end];
                    let links = extract_links(node_raw);
                    let first_url = links.first().map(|(_, url)| url.clone());
                    let line_text = format!("▌ {}", text);
                    let mut spans = vec![StyledSpan { text: line_text.clone(), style: SpanStyle::BlockQuote, cell_col: None }];
                    for (link_text, url) in &links {
                        mark_link_in_spans(&mut spans, link_text, url);
                    }
                    lines.push(StyledLine {
                        text: line_text,
                        spans,
                        anchor: Some(node.anchor.clone()),
                        node_index: Some(idx),
                        line_in_block: None,
                        first_link_url: first_url,
                    });
                    lines.push(StyledLine { text: String::new(), spans: vec![], anchor: None, node_index: None, line_in_block: None, first_link_url: None });
                }
                NodeKind::HorizontalRule => {
                    let line_text = "─".repeat(width.min(60));
                    lines.push(StyledLine {
                        text: line_text.clone(),
                        spans: vec![StyledSpan { text: line_text, style: SpanStyle::Dimmed, cell_col: None }],
                        anchor: Some(node.anchor.clone()),
                        node_index: Some(idx),
                        line_in_block: None,
                        first_link_url: None,
                    });
                }
                NodeKind::Html { content } => {
                    lines.push(StyledLine {
                        text: content.clone(),
                        spans: vec![StyledSpan { text: content.clone(), style: SpanStyle::Dimmed, cell_col: None }],
                        anchor: Some(node.anchor.clone()),
                        node_index: Some(idx),
                        line_in_block: None,
                        first_link_url: None,
                    });
                }
                NodeKind::Table { headers, rows } => {
                    let col_count = headers.len();
                    if col_count == 0 { continue; }
                    let mut col_widths: Vec<usize> = headers.iter()
                        .map(|h| visual_len(h).max(3))
                        .collect();
                    for row in rows.iter() {
                        for (j, cell) in row.iter().enumerate() {
                            if j < col_widths.len() {
                                col_widths[j] = col_widths[j].max(visual_len(cell));
                            }
                        }
                    }
                    let total = col_widths.iter().sum::<usize>() + col_count * 2 + col_count.saturating_sub(1);
                    if total > width && width > col_count * 4 {
                        let available = width.saturating_sub(col_count * 2 + col_count.saturating_sub(1));
                        let sum: usize = col_widths.iter().sum::<usize>().max(1);
                        for w in &mut col_widths {
                            *w = ((*w * available) / sum).max(3);
                        }
                    }
                    let make_row = |cells: &[String], is_header: bool| -> (String, Vec<StyledSpan>) {
                        let mut text = String::new();
                        let mut spans = Vec::new();
                        for (j, cell) in cells.iter().enumerate() {
                            let w = col_widths.get(j).copied().unwrap_or(3);
                            let base_style = if is_header { SpanStyle::TableHeader } else { SpanStyle::Normal };
                            let (cell_segs, cell_vis) = inline_segments(cell, w, base_style.clone());
                            let padding = " ".repeat(w.saturating_sub(cell_vis));
                            // leading space
                            text.push(' ');
                            spans.push(StyledSpan { text: " ".to_string(), style: base_style.clone(), cell_col: Some(j) });
                            // cell content
                            for (seg_text, seg_style) in cell_segs {
                                text.push_str(&seg_text);
                                spans.push(StyledSpan { text: seg_text, style: seg_style, cell_col: Some(j) });
                            }
                            // right-padding
                            if !padding.is_empty() {
                                text.push_str(&padding);
                                spans.push(StyledSpan { text: padding, style: base_style.clone(), cell_col: Some(j) });
                            }
                            // trailing space
                            text.push(' ');
                            spans.push(StyledSpan { text: " ".to_string(), style: base_style.clone(), cell_col: Some(j) });
                            if j + 1 < col_count {
                                text.push('│');
                                spans.push(StyledSpan { text: "│".to_string(), style: SpanStyle::TableBorder, cell_col: None });
                            }
                        }
                        (text, spans)
                    };
                    let header_cells: Vec<String> = headers.clone();
                    let (header_text, header_spans) = make_row(&header_cells, true);
                    lines.push(StyledLine {
                        text: header_text,
                        spans: header_spans,
                        anchor: Some(node.anchor.clone()),
                        node_index: Some(idx),
                        line_in_block: None,
                        first_link_url: None,
                    });
                    let sep: String = col_widths.iter().enumerate()
                        .map(|(j, &w)| {
                            let seg = "─".repeat(w + 2);
                            if j + 1 < col_count { format!("{}┼", seg) } else { seg }
                        })
                        .collect();
                    lines.push(StyledLine {
                        text: sep.clone(),
                        spans: vec![StyledSpan { text: sep, style: SpanStyle::TableBorder, cell_col: None }],
                        anchor: None,
                        node_index: Some(idx),
                        line_in_block: None,
                        first_link_url: None,
                    });
                    for (ri, row) in rows.iter().enumerate() {
                        let cells: Vec<String> = (0..col_count)
                            .map(|j| row.get(j).cloned().unwrap_or_default())
                            .collect();
                        let (row_text, row_spans) = make_row(&cells, false);
                        lines.push(StyledLine {
                            text: row_text,
                            spans: row_spans,
                            anchor: None,
                            node_index: Some(idx),
                            line_in_block: Some(ri),
                            first_link_url: None,
                        });
                    }
                    lines.push(StyledLine { text: String::new(), spans: vec![], anchor: None, node_index: None, line_in_block: None, first_link_url: None });
                }
            }
        }
        lines
    }

    /// Returns the indices of nodes that are currently visible (not hidden
    /// inside a collapsed section). Used by keyboard navigation so the cursor
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
                NodeKind::Heading { text, .. } => format!("Heading({})", truncate_chars(text, 40)),
                NodeKind::Paragraph { text } => format!("Paragraph({})", truncate_chars(text, 40)),
                NodeKind::CodeBlock { lang, .. } => format!("CodeBlock({:?})", lang),
                NodeKind::ListItem { text, checked, .. } => {
                    let check = match checked {
                        Some(true) => "[x] ", Some(false) => "[ ] ", None => "",
                    };
                    format!("ListItem({}{})", check, truncate_chars(text, 40))
                }
                NodeKind::BlockQuote { text } => format!("BlockQuote({})", truncate_chars(text, 40)),
                NodeKind::HorizontalRule => "HorizontalRule".to_string(),
                NodeKind::Html { .. } => "Html".to_string(),
                NodeKind::Table { headers, rows } => format!("Table({}col×{}row)", headers.len(), rows.len()),
            }))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Resolve a markdown fragment (`#section-name`) to a node index.
    /// Tries exact internal anchor match first, then GitHub-style heading slug.
    pub fn resolve_fragment(&self, fragment: &str) -> Option<usize> {
        let frag = fragment.trim_start_matches('#');
        if frag.is_empty() { return None; }
        // Exact match on internal anchor id (e.g. "h2-quick-start")
        if let Some(&idx) = self.anchor_map.get(frag) {
            return Some(idx);
        }
        // GitHub-style slug matching against heading text
        for (idx, node) in self.nodes.iter().enumerate() {
            if let NodeKind::Heading { text, .. } = &node.kind {
                if github_slug(text) == frag {
                    return Some(idx);
                }
            }
        }
        None
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
                        let snippet = if text.chars().count() > 80 {
                            format!("{}…", truncate_chars(text, 80))
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
                NodeKind::Table { headers, rows } => {
                    let mut all_cells: Vec<String> = headers.clone();
                    for row in rows { all_cells.extend_from_slice(row); }
                    let combined = all_cells.join(" ");
                    if combined.to_lowercase().contains(&q) {
                        let snippet = if combined.chars().count() > 80 {
                            format!("{}…", truncate_chars(&combined, 80))
                        } else {
                            combined
                        };
                        hits.push((node.anchor.clone(), snippet));
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

#[cfg(test)]
mod tests {
    use super::truncate_chars;

    #[test]
    fn truncate_chars_respects_unicode_boundaries() {
        let text = "signals — externally fed";
        assert_eq!(truncate_chars(text, 10), "signals — ");
    }
}

/// Rebuild the raw markdown for a table, escaping any pipe characters in cell
/// content.  Used when applying a direct-edit to a single table cell.
pub fn rebuild_table_raw(headers: &[String], rows: &[Vec<String>]) -> String {
    let col_count = headers.len();
    let mut result = String::new();
    result.push('|');
    for h in headers {
        result.push(' ');
        result.push_str(&h.replace('|', "\\|"));
        result.push_str(" |");
    }
    result.push('\n');
    result.push('|');
    for _ in 0..col_count {
        result.push_str(" --- |");
    }
    result.push('\n');
    for row in rows {
        result.push('|');
        for cell in row.iter() {
            result.push(' ');
            result.push_str(&cell.replace('|', "\\|"));
            result.push_str(" |");
        }
        result.push('\n');
    }
    result
}

/// Visual character count of `text`, not counting backtick delimiters in
/// matched `` `...` `` inline-code pairs.
fn visual_len(text: &str) -> usize {
    let mut len = 0;
    let mut remaining = text;
    loop {
        if let Some(open) = remaining.find('`') {
            len += remaining[..open].chars().count();
            let after = &remaining[open + 1..];
            if let Some(close) = after.find('`') {
                len += after[..close].chars().count();
                remaining = &after[close + 1..];
            } else {
                len += 1 + after.chars().count(); // unmatched backtick counts as-is
                break;
            }
        } else {
            len += remaining.chars().count();
            break;
        }
    }
    len
}

/// Split `text` into styled spans, applying `SpanStyle::Code` to backtick-delimited
/// inline-code fragments and `base_style` to surrounding text.  The backtick
/// delimiters themselves are consumed (not emitted).
fn inline_spans(text: &str, base_style: SpanStyle) -> Vec<StyledSpan> {
    let mut result = Vec::new();
    let mut remaining = text;
    loop {
        if let Some(open) = remaining.find('`') {
            let before = &remaining[..open];
            if !before.is_empty() {
                result.push(StyledSpan { text: before.to_string(), style: base_style.clone(), cell_col: None });
            }
            let after = &remaining[open + 1..];
            if let Some(close) = after.find('`') {
                let code = &after[..close];
                if !code.is_empty() {
                    result.push(StyledSpan { text: code.to_string(), style: SpanStyle::Code, cell_col: None });
                }
                remaining = &after[close + 1..];
            } else {
                // Unmatched backtick — emit the rest verbatim
                let rest = &remaining[open..];
                if !rest.is_empty() {
                    result.push(StyledSpan { text: rest.to_string(), style: base_style.clone(), cell_col: None });
                }
                break;
            }
        } else {
            if !remaining.is_empty() {
                result.push(StyledSpan { text: remaining.to_string(), style: base_style.clone(), cell_col: None });
            }
            break;
        }
    }
    if result.is_empty() {
        result.push(StyledSpan { text: String::new(), style: base_style, cell_col: None });
    }
    result
}

/// Like `inline_spans` but truncates to `max_width` visual characters and
/// returns the actual visual width used (for table-cell padding).
fn inline_segments(text: &str, max_width: usize, base_style: SpanStyle) -> (Vec<(String, SpanStyle)>, usize) {
    let mut segments: Vec<(String, SpanStyle)> = Vec::new();
    let mut remaining = text;
    let mut used = 0usize;
    loop {
        if used >= max_width { break; }
        if let Some(open) = remaining.find('`') {
            let before = &remaining[..open];
            if !before.is_empty() {
                let trunc: String = before.chars().take(max_width - used).collect();
                used += trunc.chars().count();
                segments.push((trunc, base_style.clone()));
            }
            if used >= max_width { break; }
            let after = &remaining[open + 1..];
            if let Some(close) = after.find('`') {
                let code = &after[..close];
                let trunc: String = code.chars().take(max_width - used).collect();
                used += trunc.chars().count();
                if !trunc.is_empty() {
                    segments.push((trunc, SpanStyle::Code));
                }
                remaining = &after[close + 1..];
            } else {
                // Unmatched backtick — treat the rest (including backtick) as plain text
                let rest = &remaining[open..];
                let trunc: String = rest.chars().take(max_width - used).collect();
                used += trunc.chars().count();
                segments.push((trunc, base_style.clone()));
                break;
            }
        } else {
            let trunc: String = remaining.chars().take(max_width - used).collect();
            used += trunc.chars().count();
            if !trunc.is_empty() {
                segments.push((trunc, base_style.clone()));
            }
            break;
        }
    }
    (segments, used)
}

/// Convert heading text to a GitHub-style anchor slug.
fn github_slug(text: &str) -> String {
    let mut result = String::new();
    let mut last_hyphen = false;
    for c in text.chars() {
        if c.is_alphanumeric() {
            result.push(c.to_lowercase().next().unwrap_or(c));
            last_hyphen = false;
        } else if (c == '-' || c == ' ') && !result.is_empty() {
            if !last_hyphen {
                result.push('-');
                last_hyphen = true;
            }
        }
        // all other punctuation is dropped
    }
    result.trim_end_matches('-').to_string()
}

/// Extract all `[text](url)` inline links from a raw markdown string.
/// Returns `(link_text, url)` pairs.
fn extract_links(raw: &str) -> Vec<(String, String)> {
    let mut links = Vec::new();
    let mut remaining = raw;
    loop {
        let Some(open) = remaining.find('[') else { break };
        let after_open = &remaining[open + 1..];
        if let Some(close_bracket) = after_open.find("](") {
            let link_text = &after_open[..close_bracket];
            if !link_text.contains('\n') && !link_text.contains('[') {
                let after_close = &after_open[close_bracket + 2..];
                if let Some(close_paren) = after_close.find(')') {
                    let url = after_close[..close_paren].trim();
                    if !url.is_empty() && !url.contains('\n') {
                        links.push((link_text.to_string(), url.to_string()));
                    }
                    remaining = &after_close[close_paren + 1..];
                    continue;
                }
            }
        }
        remaining = &remaining[open + 1..];
    }
    links
}

/// Find `link_text` within `spans` and replace its occurrence with a Link span.
/// Only the first matching Normal (or BlockQuote) span is processed.
fn mark_link_in_spans(spans: &mut Vec<StyledSpan>, link_text: &str, url: &str) {
    if link_text.is_empty() { return; }
    let mut i = 0;
    while i < spans.len() {
        if !matches!(spans[i].style, SpanStyle::Normal | SpanStyle::BlockQuote) {
            i += 1;
            continue;
        }
        if let Some(pos) = spans[i].text.find(link_text) {
            let before = spans[i].text[..pos].to_string();
            let after = spans[i].text[pos + link_text.len()..].to_string();
            let orig_style = spans[i].style.clone();
            let cell_col = spans[i].cell_col;
            let mut replacement = Vec::new();
            if !before.is_empty() {
                replacement.push(StyledSpan { text: before, style: orig_style.clone(), cell_col });
            }
            replacement.push(StyledSpan { text: link_text.to_string(), style: SpanStyle::Link(url.to_string()), cell_col });
            if !after.is_empty() {
                replacement.push(StyledSpan { text: after, style: orig_style, cell_col });
            }
            spans.splice(i..i + 1, replacement);
            return;
        }
        i += 1;
    }
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
