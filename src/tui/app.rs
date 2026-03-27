use crate::config::Config;
use crate::document::{AnchorId, Document, PatchOp, StyledLine, truncate_chars};
use crate::openai::client::{ChatRequest, OpenAiClient, StreamEvent};
use crate::remarks::{Remark, RemarkStatus, RemarkStore, TargetType};
use crate::review::{ReviewItem, ReviewStore};
use crate::openai::prompts;
use crate::revision_context::RevisionRequestMode;
use crate::tui::input::InputBuffer;
use anyhow::{Context, Result};
use arboard::Clipboard;
use chrono::Utc;
use ratatui::layout::Rect;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tokio::sync::mpsc;
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum AppMode {
    /// Normal document navigation.
    Normal,
    /// Searching within the document.
    Search,
    /// Editing the current block locally without AI.
    DirectEdit,
    /// Typing a remark on the selected node.
    RemarkEdit,
    /// Browsing AI review findings.
    ReviewMode,
    /// Typing an answer to a review item.
    ReviewAnswer,
    /// Prompt-to-create mode: file did not exist when the app started.
    CreationPrompt,
    /// Browsing the on-disk revision history for this document.
    HistoryBrowser,
    /// Help overlay.
    Help,
}

#[allow(dead_code)]
pub enum AppEvent {
    StreamToken(String),
    StreamDone,
    StreamError(String),
    PatchesReceived(Vec<PatchOp>, HashMap<AnchorId, crate::document::BlockFingerprint>),
    ReviewReceived(Vec<ReviewItem>),
    DocumentCreated(String),
    StatusMessage(String),
    Loading(bool),
    RequestProgress(usize),
}

pub struct App {
    pub config: Config,
    pub doc: Document,
    pub remarks: RemarkStore,
    pub review_store: ReviewStore,
    pub mode: AppMode,
    pub scroll_offset: usize,
    pub selected_node: Option<usize>,
    /// Which line within the currently selected code block is highlighted (0-based).
    /// `None` means the whole block is selected (no intra-block selection active).
    pub selected_line_in_node: Option<usize>,
    /// Shared input buffer used by RemarkEdit, ReviewAnswer, and CreationPrompt.
    pub input: InputBuffer,
    pub status_message: Option<String>,
    pub is_loading: bool,
    pub streaming_response: String,
    pub should_quit: bool,
    pub event_tx: mpsc::Sender<AppEvent>,
    pub display_lines: Vec<StyledLine>,
    pub selected_review: Option<usize>,
    pub show_remarks_panel: bool,
    pub selected_remark: Option<usize>,
    /// Set of heading anchors whose sections are currently collapsed.
    pub collapsed_sections: HashSet<String>,
    /// Scroll offset for the side (remarks) panel.
    pub side_scroll: usize,
    pub terminal_width: u16,
    pub terminal_height: u16,
    /// Populated during draw; used for mouse hit-testing.
    pub last_doc_area: Rect,
    pub last_side_area: Option<Rect>,
    /// History browser state.
    pub history_entries: Vec<crate::history::HistoryEntry>,
    pub selected_history: usize,
    pub history_preview: String,
    pub history_scroll: usize,
    /// Occurrence hits for the current find-occurrences query.
    /// Each entry is (anchor_id, text_snippet).  Cleared on navigation / Esc.
    pub occurrence_hits: Vec<(String, String)>,
    /// Search hits for the Ctrl-F search flow.
    pub search_hits: Vec<(String, String)>,
    /// Currently selected search hit index.
    pub selected_search_hit: Option<usize>,
    /// Incremented every event-loop tick; drives spinner + gradient animations.
    pub spinner_tick: u64,
    /// Label + chars received so far during a streaming request. `None` when
    /// no request-progress overlay should be shown.
    pub request_progress: Option<(String, usize)>,
    /// True when the user asked to see review results as soon as background
    /// analysis finishes.
    pub open_review_when_ready: bool,
    /// Anchor currently being edited in DirectEdit mode.
    pub direct_edit_anchor: Option<String>,
}

impl App {
    pub fn new(config: Config, doc: Document, event_tx: mpsc::Sender<AppEvent>) -> Self {
        let is_new = doc.is_new();
        let display_lines = doc.render_display(80, &HashSet::new());
        let mode = if is_new { AppMode::CreationPrompt } else { AppMode::Normal };
        let status = if is_new {
            Some("New file — describe what to create, then press Enter to generate.".to_string())
        } else {
            Some("Press ? for help  Space: collapse/expand heading  c: collapse/expand all".to_string())
        };
        App {
            config,
            doc,
            remarks: RemarkStore::new(),
            review_store: ReviewStore::new(),
            mode,
            scroll_offset: 0,
            selected_node: None,
            selected_line_in_node: None,
            input: InputBuffer::new(),
            status_message: status,
            is_loading: false,
            streaming_response: String::new(),
            should_quit: false,
            event_tx,
            display_lines,
            selected_review: None,
            show_remarks_panel: false,
            selected_remark: None,
            collapsed_sections: HashSet::new(),
            side_scroll: 0,
            terminal_width: 80,
            terminal_height: 24,
            last_doc_area: Rect::default(),
            last_side_area: None,
            history_entries: vec![],
            selected_history: 0,
            history_preview: String::new(),
            history_scroll: 0,
            occurrence_hits: vec![],
            search_hits: vec![],
            selected_search_hit: None,
            spinner_tick: 0,
            request_progress: None,
            open_review_when_ready: false,
            direct_edit_anchor: None,
        }
    }

    fn start_request_progress(&mut self, label: impl Into<String>) {
        self.request_progress = Some((label.into(), 0));
    }

    fn clear_request_progress(&mut self) {
        self.request_progress = None;
    }

    fn analysis_in_progress(&self) -> bool {
        matches!(
            self.request_progress.as_ref(),
            Some((label, _)) if label == "ANALYZING DOCUMENT"
        )
    }

    async fn collect_streaming_response(
        client: OpenAiClient,
        mut req: ChatRequest,
        tx: mpsc::Sender<AppEvent>,
    ) -> Result<String> {
        req.stream = true;
        let (stream_tx, mut stream_rx) = mpsc::channel(64);
        let stream_task = tokio::spawn(async move { client.chat_stream(req, stream_tx).await });
        let mut content = String::new();

        while let Some(event) = stream_rx.recv().await {
            match event {
                StreamEvent::Token(token) => {
                    content.push_str(&token);
                    let _ = tx.send(AppEvent::RequestProgress(content.len())).await;
                }
                StreamEvent::Done => break,
                StreamEvent::Error(error) => anyhow::bail!(error),
            }
        }

        stream_task
            .await
            .context("Streaming task failed to join")??;

        Ok(content)
    }

    pub fn refresh_display(&mut self) {
        self.display_lines = self
            .doc
            .render_display(self.terminal_width.saturating_sub(4) as usize, &self.collapsed_sections);
    }

    // ── Document scrolling ───────────────────────────────────────────────────

    pub fn scroll_up(&mut self) {
        self.scroll_offset = self.scroll_offset.saturating_sub(1);
    }

    pub fn scroll_down(&mut self) {
        let max = self.display_lines.len().saturating_sub(1);
        if self.scroll_offset < max {
            self.scroll_offset += 1;
        }
    }

    pub fn page_up(&mut self) {
        let step = (self.terminal_height as usize).saturating_sub(4) / 2;
        self.scroll_offset = self.scroll_offset.saturating_sub(step.max(1));
    }

    pub fn page_down(&mut self) {
        let max = self.display_lines.len().saturating_sub(1);
        let step = (self.terminal_height as usize).saturating_sub(4) / 2;
        self.scroll_offset = (self.scroll_offset + step.max(1)).min(max);
    }

    // ── Side-panel scrolling ─────────────────────────────────────────────────

    pub fn side_scroll_up(&mut self) {
        self.side_scroll = self.side_scroll.saturating_sub(1);
    }

    pub fn side_scroll_down(&mut self) {
        let max = self.remarks.remarks.len().saturating_sub(1);
        if self.side_scroll < max {
            self.side_scroll += 1;
        }
    }

    // ── Node selection ────────────────────────────────────────────────────────

    /// Returns the number of content lines in a CodeBlock node.
    fn code_block_line_count(&self, node_idx: usize) -> usize {
        if let Some(node) = self.doc.nodes.get(node_idx) {
            if let crate::document::NodeKind::CodeBlock { code, .. } = &node.kind {
                return code.lines().count();
            }
        }
        0
    }

    fn is_code_block(&self, node_idx: usize) -> bool {
        self.doc.nodes.get(node_idx)
            .map(|n| matches!(n.kind, crate::document::NodeKind::CodeBlock { .. }))
            .unwrap_or(false)
    }

    pub fn select_next_node(&mut self) {
        // If we're inside a code block, navigate line-by-line first.
        if let (Some(cur), Some(line)) = (self.selected_node, self.selected_line_in_node) {
            if self.is_code_block(cur) {
                let count = self.code_block_line_count(cur);
                if line + 1 < count {
                    self.selected_line_in_node = Some(line + 1);
                    self.scroll_to_code_line(cur, line + 1);
                    return;
                }
                // Fall through to move to next node, clearing intra-block selection.
                self.selected_line_in_node = None;
            }
        }

        let visible = self.doc.visible_node_indices(&self.collapsed_sections);
        if visible.is_empty() { return; }
        let next = match self.selected_node {
            None => visible[0],
            Some(cur) => {
                let pos = visible.iter().position(|&i| i == cur).unwrap_or(0);
                visible[(pos + 1).min(visible.len() - 1)]
            }
        };
        // Entering a code block: start at first line.
        if self.is_code_block(next) && self.code_block_line_count(next) > 0 {
            self.selected_line_in_node = Some(0);
        } else {
            self.selected_line_in_node = None;
        }
        self.selected_node = Some(next);
        if let Some(line) = self.selected_line_in_node {
            self.scroll_to_code_line(next, line);
        } else {
            self.scroll_to_node(next);
        }
    }

    pub fn select_prev_node(&mut self) {
        // If we're inside a code block, navigate line-by-line first.
        if let (Some(cur), Some(line)) = (self.selected_node, self.selected_line_in_node) {
            if self.is_code_block(cur) {
                if line > 0 {
                    self.selected_line_in_node = Some(line - 1);
                    self.scroll_to_code_line(cur, line - 1);
                    return;
                }
                // Fall through to move to previous node, clearing intra-block selection.
                self.selected_line_in_node = None;
            }
        }

        let visible = self.doc.visible_node_indices(&self.collapsed_sections);
        if visible.is_empty() { return; }
        let prev = match self.selected_node {
            None => visible[0],
            Some(cur) => {
                let pos = visible.iter().position(|&i| i == cur).unwrap_or(0);
                visible[pos.saturating_sub(1)]
            }
        };
        // Entering a code block from below: land on its last line.
        if self.is_code_block(prev) {
            let count = self.code_block_line_count(prev);
            self.selected_line_in_node = if count > 0 { Some(count - 1) } else { None };
        } else {
            self.selected_line_in_node = None;
        }
        self.selected_node = Some(prev);
        if let Some(line) = self.selected_line_in_node {
            self.scroll_to_code_line(prev, line);
        } else {
            self.scroll_to_node(prev);
        }
    }

    /// Toggle collapse on the currently selected node if it is a heading.
    pub fn toggle_collapse(&mut self) {
        if let Some(idx) = self.selected_node {
            if let Some(node) = self.doc.nodes.get(idx) {
                if matches!(node.kind, crate::document::NodeKind::Heading { .. }) {
                    if self.collapsed_sections.contains(&node.anchor) {
                        self.collapsed_sections.remove(&node.anchor);
                        self.status_message = Some("Section expanded.".to_string());
                    } else {
                        self.collapsed_sections.insert(node.anchor.clone());
                        self.status_message = Some("Section collapsed.".to_string());
                    }
                    self.refresh_display();
                    return;
                }
            }
        }
        self.status_message = Some("Select a heading first (↑↓ to navigate).".to_string());
    }

    /// `←` — collapse the selected heading (no-op if not a heading or already collapsed).
    pub fn collapse_heading(&mut self) {
        if let Some(idx) = self.selected_node {
            if let Some(node) = self.doc.nodes.get(idx) {
                if matches!(node.kind, crate::document::NodeKind::Heading { .. }) {
                    if !self.collapsed_sections.contains(&node.anchor) {
                        self.collapsed_sections.insert(node.anchor.clone());
                        self.refresh_display();
                        self.status_message = Some("Section collapsed. → to expand.".to_string());
                    }
                    return;
                }
            }
        }
        // Not on a heading — scroll left does nothing special.
    }

    /// `→` — expand the selected heading (no-op if not a heading or already expanded).
    pub fn expand_heading(&mut self) {
        if let Some(idx) = self.selected_node {
            if let Some(node) = self.doc.nodes.get(idx) {
                if matches!(node.kind, crate::document::NodeKind::Heading { .. }) {
                    if self.collapsed_sections.contains(&node.anchor) {
                        self.collapsed_sections.remove(&node.anchor);
                        self.refresh_display();
                        self.status_message = Some("Section expanded.".to_string());
                    }
                    return;
                }
            }
        }
        // Not on a heading — scroll right does nothing special.
    }

    /// Collapse all headings from the selected heading downward, including the
    /// current heading and leaving headings above untouched.
    pub fn collapse_headings_below(&mut self) {
        let Some(idx) = self.selected_node else { return; };
        let Some(node) = self.doc.nodes.get(idx) else { return; };
        let crate::document::NodeKind::Heading { .. } = node.kind else { return; };

        let mut collapsed = 0usize;
        for node in self.doc.nodes.iter().skip(idx) {
            if let crate::document::NodeKind::Heading { .. } = node.kind {
                if self.collapsed_sections.insert(node.anchor.clone()) {
                    collapsed += 1;
                }
            }
        }

        self.refresh_display();
        self.status_message = Some(if collapsed == 0 {
            "No headings below to collapse.".to_string()
        } else {
            format!(
                "Collapsed {} heading{} from here downward.",
                collapsed,
                if collapsed == 1 { "" } else { "s" }
            )
        });
    }

    /// Expand all headings from the selected heading downward, including the
    /// current heading and leaving headings above untouched.
    pub fn expand_headings_below(&mut self) {
        let Some(idx) = self.selected_node else { return; };
        let Some(node) = self.doc.nodes.get(idx) else { return; };
        let crate::document::NodeKind::Heading { .. } = node.kind else { return; };

        let mut expanded = 0usize;
        for node in self.doc.nodes.iter().skip(idx) {
            if let crate::document::NodeKind::Heading { .. } = node.kind {
                if self.collapsed_sections.remove(&node.anchor) {
                    expanded += 1;
                }
            }
        }

        self.refresh_display();
        self.status_message = Some(if expanded == 0 {
            "No headings below to expand.".to_string()
        } else {
            format!(
                "Expanded {} heading{} from here downward.",
                expanded,
                if expanded == 1 { "" } else { "s" }
            )
        });
    }

    pub fn toggle_collapse_all(&mut self) {
        let heading_anchors: Vec<String> = self
            .doc
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, crate::document::NodeKind::Heading { .. }))
            .map(|n| n.anchor.clone())
            .collect();

        if heading_anchors.is_empty() {
            return;
        }

        if self.collapsed_sections.is_empty() {
            for anchor in heading_anchors {
                self.collapsed_sections.insert(anchor);
            }
            self.status_message = Some("All sections collapsed. c to expand all.".to_string());
        } else {
            self.collapsed_sections.clear();
            self.status_message = Some("All sections expanded.".to_string());
        }
        self.refresh_display();
    }

    // ── History browser ──────────────────────────────────────────────────────

    pub fn open_history(&mut self) {
        if let Ok(store) = crate::history::HistoryStore::for_doc(&self.doc.path) {
            self.history_entries = store.list();
        } else {
            self.history_entries = vec![];
        }
        self.selected_history = 0;
        self.history_scroll = 0;
        self.history_preview = self.load_history_preview(0);
        self.mode = AppMode::HistoryBrowser;
        let n = self.history_entries.len();
        self.status_message = Some(if n == 0 {
            "No history yet — patches create snapshots automatically.".to_string()
        } else {
            format!("{} snapshot(s). j/k navigate  Enter restore  q close", n)
        });
    }

    pub fn history_next(&mut self) {
        if self.history_entries.is_empty() { return; }
        let max = self.history_entries.len() - 1;
        if self.selected_history < max {
            self.selected_history += 1;
            self.history_scroll = 0;
            self.history_preview = self.load_history_preview(self.selected_history);
        }
    }

    pub fn history_prev(&mut self) {
        if self.selected_history > 0 {
            self.selected_history -= 1;
            self.history_scroll = 0;
            self.history_preview = self.load_history_preview(self.selected_history);
        }
    }

    fn load_history_preview(&self, idx: usize) -> String {
        self.history_entries
            .get(idx)
            .and_then(|e| crate::history::HistoryStore::load(&e.path).ok())
            .unwrap_or_default()
    }

    pub fn restore_history(&mut self) {
        if self.history_entries.is_empty() { return; }
        let content = self.history_preview.clone();
        if content.is_empty() { return; }
        // Snapshot the current state before overwriting.
        match self.doc.set_content(content) {
            Ok(()) => {
                self.refresh_display();
                self.mode = AppMode::Normal;
                let label = self.history_entries
                    .get(self.selected_history)
                    .map(|e| e.label.clone())
                    .unwrap_or_default();
                self.status_message = Some(format!(
                    "Restored snapshot from {}. Press W to save.",
                    label
                ));
            }
            Err(e) => {
                self.status_message = Some(format!("Restore failed: {}", e));
            }
        }
    }

    /// Lines of context kept above/below the selected item ("scrolloff").
    const SCROLL_MARGIN: usize = 5;

    /// Adjust `scroll_offset` so that display line `pos` sits within the
    /// scrolloff band:  at least SCROLL_MARGIN lines from the top edge and
    /// at least SCROLL_MARGIN lines from the bottom edge of the viewport.
    fn scroll_into_view(&mut self, pos: usize) {
        // terminal_height includes the top-bar (1) and status (1) rows, so the
        // usable document pane is roughly height - 4.  Use a conservative
        // estimate; the exact value doesn't need to be pixel-perfect.
        let visible = (self.terminal_height as usize).saturating_sub(4).max(1);
        let margin = Self::SCROLL_MARGIN.min(visible / 2);

        // Scroll up: pos is above the top margin.
        if pos < self.scroll_offset + margin {
            self.scroll_offset = pos.saturating_sub(margin);
        }
        // Scroll down: pos is below the bottom margin.
        let bottom_threshold = self.scroll_offset + visible.saturating_sub(margin);
        if pos >= bottom_threshold {
            self.scroll_offset = pos + margin + 1 - visible;
        }
        // Clamp to valid range.
        let max = self.display_lines.len().saturating_sub(1);
        self.scroll_offset = self.scroll_offset.min(max);
    }

    fn scroll_to_node(&mut self, node_idx: usize) {
        if let Some(pos) = self
            .display_lines
            .iter()
            .position(|l| l.node_index == Some(node_idx))
        {
            self.scroll_into_view(pos);
        }
    }

    fn scroll_to_code_line(&mut self, node_idx: usize, line_in_block: usize) {
        if let Some(pos) = self.display_lines.iter().position(|l| {
            l.node_index == Some(node_idx) && l.line_in_block == Some(line_in_block)
        }) {
            self.scroll_into_view(pos);
        }
    }

    fn select_anchor(&mut self, anchor: &str) {
        if let Some((node_anchor, line_str)) = anchor.split_once(":L") {
            if let Some(&node_idx) = self.doc.anchor_map.get(node_anchor) {
                let line_in_block = line_str.parse::<usize>().unwrap_or(0);
                self.selected_node = Some(node_idx);
                self.selected_line_in_node = Some(line_in_block);
                self.scroll_to_code_line(node_idx, line_in_block);
            }
        } else if let Some(&node_idx) = self.doc.anchor_map.get(anchor) {
            self.selected_node = Some(node_idx);
            if self.is_code_block(node_idx) && self.code_block_line_count(node_idx) > 0 {
                self.selected_line_in_node = Some(0);
                self.scroll_to_code_line(node_idx, 0);
            } else {
                self.selected_line_in_node = None;
                self.scroll_to_node(node_idx);
            }
        }
    }

    fn current_selection_text(&self) -> Option<String> {
        let node_idx = self.selected_node?;
        let node = self.doc.nodes.get(node_idx)?;

        if let crate::document::NodeKind::CodeBlock { code, .. } = &node.kind {
            if let Some(line_idx) = self.selected_line_in_node {
                return Some(code.lines().nth(line_idx).unwrap_or("").to_string());
            }
        }

        self.doc
            .raw
            .get(node.source_start..node.source_end)
            .map(|text| text.to_string())
    }

    pub fn copy_current_selection(&mut self) {
        let Some(selection) = self.current_selection_text() else {
            self.status_message = Some("Select something first, then press Ctrl+C to copy.".to_string());
            return;
        };

        match Clipboard::new().and_then(|mut clipboard| clipboard.set_text(selection)) {
            Ok(()) => {
                self.status_message = Some("Copied current selection.".to_string());
            }
            Err(e) => {
                self.status_message = Some(format!("Clipboard error: {}", e));
            }
        }
    }

    // ── Remark flow ──────────────────────────────────────────────────────────

    pub fn start_search(&mut self) {
        self.mode = AppMode::Search;
        self.input.clear();
        self.search_hits.clear();
        self.selected_search_hit = None;
        self.status_message = Some("Search: type to find matches, Enter next, Shift+Enter previous, Esc close.".to_string());
    }

    pub fn update_search(&mut self) {
        let query = self.input.text().trim().to_string();
        if query.is_empty() {
            self.search_hits.clear();
            self.selected_search_hit = None;
            self.status_message = Some("Search: type to find matches, Enter next, Shift+Enter previous, Esc close.".to_string());
            return;
        }

        self.search_hits = self.doc.find_occurrences(&query, None);
        if self.search_hits.is_empty() {
            self.selected_search_hit = None;
            self.status_message = Some(format!("No matches for \"{}\".", query));
            return;
        }

        self.selected_search_hit = Some(0);
        if let Some((anchor, _)) = self.search_hits.first().cloned() {
            self.select_anchor(&anchor);
        }
        self.status_message = Some(format!(
            "{} match{} for \"{}\". Enter next, Shift+Enter previous, Esc close.",
            self.search_hits.len(),
            if self.search_hits.len() == 1 { "" } else { "es" },
            query
        ));
    }

    pub fn advance_search(&mut self, forward: bool) {
        if self.search_hits.is_empty() {
            self.update_search();
            return;
        }

        let len = self.search_hits.len();
        let next_idx = match self.selected_search_hit {
            Some(idx) if forward => (idx + 1) % len,
            Some(idx) => (idx + len - 1) % len,
            None => 0,
        };
        self.selected_search_hit = Some(next_idx);
        if let Some((anchor, _)) = self.search_hits.get(next_idx).cloned() {
            self.select_anchor(&anchor);
        }
        let query = self.input.text().trim();
        self.status_message = Some(format!(
            "Match {}/{} for \"{}\". Enter next, Shift+Enter previous, Esc close.",
            next_idx + 1,
            len,
            query
        ));
    }

    pub fn cancel_search(&mut self) {
        self.mode = AppMode::Normal;
        self.input.clear();
        self.search_hits.clear();
        self.selected_search_hit = None;
        self.status_message = Some("Search closed.".to_string());
    }

    pub fn start_remark(&mut self) {
        if self.selected_node.is_some() {
            self.mode = AppMode::RemarkEdit;
            self.input.clear();
        } else {
            self.status_message = Some("Select a node first (↑/↓ to navigate)".to_string());
        }
    }

    pub fn start_direct_edit(&mut self) {
        let Some(node_idx) = self.selected_node else {
            self.status_message = Some("Select a block first, then press Ctrl+E.".to_string());
            return;
        };

        let Some(node) = self.doc.nodes.get(node_idx) else {
            self.status_message = Some("Selected block is unavailable.".to_string());
            return;
        };

        let (anchor, initial_text) = match &node.kind {
            crate::document::NodeKind::CodeBlock { code, .. } => {
                if let Some(line_idx) = self.selected_line_in_node {
                    (
                        format!("{}:L{}", node.anchor, line_idx),
                        code.lines().nth(line_idx).unwrap_or("").to_string(),
                    )
                } else {
                    (node.anchor.clone(), code.clone())
                }
            }
            crate::document::NodeKind::Heading { text, .. } => (node.anchor.clone(), text.clone()),
            crate::document::NodeKind::Paragraph { text } => (node.anchor.clone(), text.clone()),
            crate::document::NodeKind::ListItem { text, .. } => (node.anchor.clone(), text.clone()),
            crate::document::NodeKind::BlockQuote { text } => (node.anchor.clone(), text.clone()),
            _ => {
                self.status_message = Some("This block type is not editable yet.".to_string());
                return;
            }
        };

        self.direct_edit_anchor = Some(anchor);
        self.input.set_text(initial_text);
        self.mode = AppMode::DirectEdit;
        self.status_message = Some("Editing block locally. Enter save  Alt+Enter newline  Esc cancel".to_string());
    }

    pub fn submit_direct_edit(&mut self) {
        let Some(anchor) = self.direct_edit_anchor.clone() else {
            self.status_message = Some("No block is being edited.".to_string());
            self.mode = AppMode::Normal;
            return;
        };

        let replacement = self.input.text().to_string();
        let patch = if let Some((node_anchor, line_str)) = anchor.split_once(":L") {
            let Some(&node_idx) = self.doc.anchor_map.get(node_anchor) else {
                self.status_message = Some("Edited block no longer exists.".to_string());
                self.mode = AppMode::Normal;
                self.input.clear();
                self.direct_edit_anchor = None;
                return;
            };

            let line_idx = line_str.parse::<usize>().unwrap_or(0);
            let Some(node) = self.doc.nodes.get(node_idx) else {
                self.status_message = Some("Edited block no longer exists.".to_string());
                self.mode = AppMode::Normal;
                self.input.clear();
                self.direct_edit_anchor = None;
                return;
            };

            match &node.kind {
                crate::document::NodeKind::CodeBlock { code, lang } => {
                    let mut lines: Vec<String> = code.lines().map(|line| line.to_string()).collect();
                    if line_idx < lines.len() {
                        lines[line_idx] = replacement;
                    } else {
                        lines.push(replacement);
                    }
                    PatchOp::ReplaceCodeBlock {
                        anchor: node_anchor.to_string(),
                        content: lines.join("\n"),
                        lang: lang.clone(),
                        rationale: "local direct edit".to_string(),
                    }
                }
                _ => {
                    self.status_message = Some("Only code lines support line-level direct edit.".to_string());
                    return;
                }
            }
        } else {
            let Some(&node_idx) = self.doc.anchor_map.get(&anchor) else {
                self.status_message = Some("Edited block no longer exists.".to_string());
                self.mode = AppMode::Normal;
                self.input.clear();
                self.direct_edit_anchor = None;
                return;
            };

            let Some(node) = self.doc.nodes.get(node_idx) else {
                self.status_message = Some("Edited block no longer exists.".to_string());
                self.mode = AppMode::Normal;
                self.input.clear();
                self.direct_edit_anchor = None;
                return;
            };

            match &node.kind {
                crate::document::NodeKind::Heading { .. } => PatchOp::UpdateHeadingText {
                    anchor: anchor.clone(),
                    new_text: replacement,
                    rationale: "local direct edit".to_string(),
                },
                crate::document::NodeKind::Paragraph { .. }
                | crate::document::NodeKind::BlockQuote { .. } => PatchOp::ReplaceSection {
                    anchor: anchor.clone(),
                    content: format!("{}\n", replacement),
                    rationale: "local direct edit".to_string(),
                },
                crate::document::NodeKind::ListItem { .. } => PatchOp::UpdateListItem {
                    anchor: anchor.clone(),
                    new_text: replacement,
                    rationale: "local direct edit".to_string(),
                },
                crate::document::NodeKind::CodeBlock { lang, .. } => PatchOp::ReplaceCodeBlock {
                    anchor: anchor.clone(),
                    content: replacement,
                    lang: lang.clone(),
                    rationale: "local direct edit".to_string(),
                },
                _ => {
                    self.status_message = Some("This block type is not editable yet.".to_string());
                    return;
                }
            }
        };

        match self.doc.apply_patches(vec![patch], None) {
            Ok((applied, skipped)) => {
                self.refresh_display();
                self.input.clear();
                self.direct_edit_anchor = None;
                self.mode = AppMode::Normal;
                if let Err(e) = self.doc.save() {
                    self.status_message = Some(format!("Saved edit in memory, but write failed: {}", e));
                } else if skipped.is_empty() {
                    self.status_message = Some(format!("Updated {} block(s) locally.", applied.len()));
                } else {
                    self.status_message = Some(format!(
                        "Updated {} block(s), skipped {}.",
                        applied.len(),
                        skipped.len()
                    ));
                }
            }
            Err(e) => {
                self.status_message = Some(format!("Local edit failed: {}", e));
            }
        }
    }

    /// Find all occurrences of the selected node's text in the rest of the
    /// document, highlight them, then open the remark input.
    /// If no selection exists this is a no-op.
    pub fn find_and_show_occurrences(&mut self) {
        let query = match self.selected_node {
            None => {
                self.status_message = Some("Select a node first (↑/↓ to navigate)".to_string());
                return;
            }
            Some(idx) => {
                let node = &self.doc.nodes[idx];
                // For code-block line selection use the specific line text.
                if let crate::document::NodeKind::CodeBlock { code, .. } = &node.kind {
                    if let Some(li) = self.selected_line_in_node {
                        code.lines().nth(li).unwrap_or("").trim().to_string()
                    } else {
                        // whole block — use the first non-empty line as query
                        code.lines()
                            .find(|l| !l.trim().is_empty())
                            .unwrap_or("")
                            .trim()
                            .to_string()
                    }
                } else {
                    match &node.kind {
                        crate::document::NodeKind::Heading { text, .. } => text.clone(),
                        crate::document::NodeKind::Paragraph { text } => {
                            // Use first sentence / 60 chars as query to avoid over-matching.
                            let end = text
                                .find(|c| c == '.' || c == '!' || c == '?')
                                .map(|i| i + 1)
                                .unwrap_or(text.len());
                            let sentence = &text[..end];
                            truncate_chars(sentence, 60).trim().to_string()
                        }
                        crate::document::NodeKind::ListItem { text, .. } => text.clone(),
                        crate::document::NodeKind::BlockQuote { text } => text.clone(),
                        _ => String::new(),
                    }
                }
            }
        };

        if query.trim().is_empty() {
            self.status_message = Some("Nothing to search for in this node.".to_string());
            return;
        }

        let exclude = self.selected_node.map(|i| self.doc.nodes[i].anchor.clone());
        // Also exclude the specific code-block-line anchor if set.
        let exclude_str = match self.selected_line_in_node {
            Some(li) => exclude
                .as_ref()
                .map(|a| format!("{}:L{}", a, li)),
            None => exclude,
        };
        let hits = self
            .doc
            .find_occurrences(&query, exclude_str.as_deref());

        if hits.is_empty() {
            self.status_message = Some(format!("No other occurrences of \"{}\".", query));
            return;
        }

        let count = hits.len();
        self.occurrence_hits = hits;
        self.mode = AppMode::RemarkEdit;
        self.input.clear();
        self.status_message = Some(format!(
            "{} occurrence{} found — write your change instruction and press Enter",
            count,
            if count == 1 { "" } else { "s" }
        ));
    }

    /// Clear occurrence highlights (called on navigation, Esc, submit).
    pub fn clear_occurrences(&mut self) {
        self.occurrence_hits.clear();
    }

    pub async fn submit_remark(&mut self) {
        if let Some(node_idx) = self.selected_node {
            let text = self.input.text().trim().to_string();
            if text.is_empty() {
                self.mode = AppMode::Normal;
                return;
            }
            let node = &self.doc.nodes[node_idx];
            let (selected_text, anchor, target_type, list_context) = match &node.kind {
                crate::document::NodeKind::CodeBlock { lang, code } => {
                    if let Some(li) = self.selected_line_in_node {
                        let line_text = code.lines().nth(li).unwrap_or("").to_string();
                        let line_anchor = format!("{}:L{}", node.anchor, li);
                        (line_text, line_anchor, TargetType::CodeBlock, None)
                    } else {
                        (
                            format!("```{}", lang.as_deref().unwrap_or("")),
                            node.anchor.clone(),
                            TargetType::CodeBlock,
                            None,
                        )
                    }
                }
                crate::document::NodeKind::Heading { text, .. } => {
                    (text.clone(), node.anchor.clone(), TargetType::Section, None)
                }
                crate::document::NodeKind::Paragraph { text } => {
                    let snippet = if text.chars().count() > 80 {
                        format!("{}…", truncate_chars(text, 80))
                    } else {
                        text.clone()
                    };
                    (snippet, node.anchor.clone(), TargetType::Paragraph, None)
                }
                crate::document::NodeKind::ListItem { text, .. } => {
                    let ctx = self.collect_list_context(node_idx);
                    (text.clone(), node.anchor.clone(), TargetType::ListItem, Some(ctx))
                }
                crate::document::NodeKind::BlockQuote { text } => {
                    (text.clone(), node.anchor.clone(), TargetType::TextSpan, None)
                }
                _ => (String::new(), node.anchor.clone(), TargetType::TextSpan, None),
            };
            let occurrence_anchors = std::mem::take(&mut self.occurrence_hits);
            let remark = Remark {
                id: Uuid::new_v4(),
                anchor,
                selected_text,
                target_type,
                text,
                list_context,
                occurrence_anchors,
                created_at: Utc::now(),
                status: RemarkStatus::Queued,
            };
            self.remarks.add(remark);
            self.input.clear();
            self.mode = AppMode::Normal;
            self.send_remarks().await;
        }
    }

    /// Collect all contiguous ListItem nodes around `node_idx` and return
    /// them formatted as a markdown list with anchor annotations.
    fn collect_list_context(&self, node_idx: usize) -> String {
        let nodes = &self.doc.nodes;

        // Walk backward to the first item in this contiguous list.
        let mut start = node_idx;
        while start > 0 {
            if matches!(nodes[start - 1].kind, crate::document::NodeKind::ListItem { .. }) {
                start -= 1;
            } else {
                break;
            }
        }

        // Walk forward to the last item.
        let mut end = node_idx;
        while end + 1 < nodes.len() {
            if matches!(nodes[end + 1].kind, crate::document::NodeKind::ListItem { .. }) {
                end += 1;
            } else {
                break;
            }
        }

        nodes[start..=end]
            .iter()
            .map(|n| {
                if let crate::document::NodeKind::ListItem { text, .. } = &n.kind {
                    format!("- {} <!-- anchor: {} -->", text, n.anchor)
                } else {
                    String::new()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn cancel_input(&mut self) {
        self.input.clear();
        self.direct_edit_anchor = None;
        self.mode = AppMode::Normal;
    }

    // ── AI submission flows ──────────────────────────────────────────────────

    pub async fn send_remarks(&mut self) {
        let queued: Vec<_> = self.remarks.queued().into_iter().cloned().collect();
        if queued.is_empty() {
            self.status_message = Some("No queued remarks to send.".to_string());
            return;
        }
        self.is_loading = true;
        let refs: Vec<&Remark> = queued.iter().collect();
        let prepared = prompts::build_revision_request(&self.config, &self.doc, &refs);
        self.status_message = Some(match prepared.mode {
            RevisionRequestMode::Targeted => {
                format!("Sending {} remark(s) with targeted context…", queued.len())
            }
            RevisionRequestMode::FullDocument => {
                format!("Sending {} remark(s) with full-document fallback…", queued.len())
            }
        });

        let config = Arc::new(self.config.clone());
        let client = OpenAiClient::new(config.clone());
        let req = prepared.request;
        let snapshot = self.doc.content_snapshot();
        let tx = self.event_tx.clone();

        tokio::spawn(async move {
            match Self::collect_streaming_response(client, req, tx.clone()).await {
                Ok(response) => {
                    match serde_json::from_str::<serde_json::Value>(&response) {
                        Ok(json) => {
                            if let Some(arr) = json["patches"].as_array() {
                                let patches: Vec<PatchOp> = arr
                                    .iter()
                                    .filter_map(|p| serde_json::from_value(p.clone()).ok())
                                    .collect();
                                let _ = tx.send(AppEvent::PatchesReceived(patches, snapshot)).await;
                            } else {
                                let _ = tx
                                    .send(AppEvent::StreamError(
                                        "No patches in response".to_string(),
                                    ))
                                    .await;
                            }
                        }
                        Err(e) => {
                            let _ = tx
                                .send(AppEvent::StreamError(format!("JSON parse error: {}", e)))
                                .await;
                        }
                    }
                }
                Err(e) => {
                    let _ = tx.send(AppEvent::StreamError(e.to_string())).await;
                }
            }
        });
    }

    pub async fn open_review_panel(&mut self) {
        let pending = self.review_store.pending().len();
        if !self.review_store.is_empty() {
            self.selected_review = if pending > 0 {
                Some(
                    self.selected_review
                        .map(|i| i.min(pending - 1))
                        .unwrap_or(0),
                )
            } else {
                None
            };
            self.mode = AppMode::ReviewMode;
            self.status_message = if pending > 0 {
                Some(format!(
                    "{} pending review item(s). j/k navigate  a answer  y accept  d dismiss  x clear results  q close",
                    pending
                ))
            } else {
                Some("No pending review items. x clear cached results  q close".to_string())
            };
        } else if self.analysis_in_progress() {
            self.open_review_when_ready = true;
            self.status_message = Some(
                "Analysis is already running in the background. Keep editing; review will open when ready."
                    .to_string(),
            );
        } else {
            self.open_review_when_ready = true;
            self.run_review_fetch().await;
        }
    }

    pub async fn run_review_fetch(&mut self) {
        if self.analysis_in_progress() {
            self.open_review_when_ready = true;
            self.status_message = Some(
                "Analysis is already running in the background. Keep editing; review will open when ready."
                    .to_string(),
            );
            return;
        }

        self.is_loading = true;
        self.start_request_progress("ANALYZING DOCUMENT");
        self.status_message = Some(
            "Analyzing document in the background… keep editing; review will open when ready."
                .to_string(),
        );

        let config = Arc::new(self.config.clone());
        let client = OpenAiClient::new(config.clone());
        let req = prompts::build_ambiguity_request(&self.config, &self.doc);
        let tx = self.event_tx.clone();

        tokio::spawn(async move {
            match Self::collect_streaming_response(client, req, tx.clone()).await {
                Ok(content) => match serde_json::from_str::<serde_json::Value>(&content) {
                Ok(json) => {
                    if let Some(arr) = json["issues"].as_array() {
                        let items: Vec<ReviewItem> = arr
                            .iter()
                            .filter_map(|item| {
                                Some(ReviewItem {
                                    id: Uuid::new_v4(),
                                    category: serde_json::from_value(item["category"].clone()).ok()?,
                                    anchor: item["anchor"].as_str()?.to_string(),
                                    evidence: item["evidence"].as_str().unwrap_or("").to_string(),
                                    why_it_matters: item["why_it_matters"].as_str().unwrap_or("").to_string(),
                                    suggested_resolution: item["suggested_resolution"].as_str().unwrap_or("").to_string(),
                                    status: crate::review::ReviewStatus::New,
                                    user_answer: None,
                                })
                            })
                            .collect();
                        let _ = tx.send(AppEvent::ReviewReceived(items)).await;
                    } else {
                        let _ = tx.send(AppEvent::StreamError("No issues in response".to_string())).await;
                    }
                }
                Err(e) => {
                    let _ = tx.send(AppEvent::StreamError(format!("JSON parse error: {}", e))).await;
                }
            },
                Err(e) => {
                    let _ = tx.send(AppEvent::StreamError(e.to_string())).await;
                }
            };
        });
    }

    /// Submit the creation prompt (CreationPrompt mode) to OpenAI.
    pub async fn submit_creation_prompt(&mut self) {
        let prompt = self.input.text().trim().to_string();
        if prompt.is_empty() {
            self.status_message =
                Some("Please describe what to create before pressing Enter.".to_string());
            return;
        }
        self.is_loading = true;
        self.start_request_progress("CREATING DOCUMENT");
        self.status_message = Some("Creating document…".to_string());

        let config = Arc::new(self.config.clone());
        let client = OpenAiClient::new(config.clone());
        let req = prompts::build_creation_request(&self.config, &prompt);
        let tx = self.event_tx.clone();

        tokio::spawn(async move {
            match Self::collect_streaming_response(client, req, tx.clone()).await {
                Ok(content) => {
                    let _ = tx.send(AppEvent::DocumentCreated(content)).await;
                }
                Err(e) => {
                    let _ = tx.send(AppEvent::StreamError(e.to_string())).await;
                }
            }
        });
    }

    // ── Review flow ──────────────────────────────────────────────────────────

    pub fn start_review_answer(&mut self) {
        if self.selected_review.is_some() {
            self.mode = AppMode::ReviewAnswer;
            self.input.clear();
        }
    }

    pub async fn submit_review_answer(&mut self) {
        if let Some(idx) = self.selected_review {
            let answer = self.input.text().trim().to_string();
            if answer.is_empty() {
                self.mode = AppMode::ReviewMode;
                return;
            }
            self.input.clear();
            self.mode = AppMode::ReviewMode;
            self.submit_review_item(idx, answer).await;
        }
    }

    /// Accept the suggested resolution immediately — no input box needed.
    pub async fn accept_resolution(&mut self) {
        if let Some(idx) = self.selected_review {
            let pending: Vec<_> = self.review_store.pending().into_iter().cloned().collect();
            if let Some(item) = pending.get(idx) {
                let resolution = item.suggested_resolution.clone();
                self.submit_review_item(idx, resolution).await;
            }
        }
    }

    /// Convert review item at `pending[idx]` into a remark with `answer`,
    /// mark it Sent, advance the selection, then fire send_remarks.
    async fn submit_review_item(&mut self, idx: usize, answer: String) {
        let pending: Vec<_> = self.review_store.pending().into_iter().cloned().collect();
        let item = match pending.get(idx) {
            Some(i) => i.clone(),
            None => return,
        };

        // Find other document locations that mention the same evidence text.
        let occurrence_anchors = self
            .doc
            .find_occurrences(&item.evidence, Some(item.anchor.as_str()));

        // Build a remark from the review item + user answer.
        let remark = Remark {
            id: Uuid::new_v4(),
            anchor: item.anchor.clone(),
            selected_text: item.evidence.clone(),
            target_type: TargetType::Paragraph,
            text: format!(
                "Review issue ({}): {}\nSuggested resolution: {}\nUser answer: {}",
                item.category,
                item.suggested_question_or_title(),
                item.suggested_resolution,
                answer
            ),
            list_context: None,
            occurrence_anchors,
            created_at: Utc::now(),
            status: RemarkStatus::Queued,
        };
        self.remarks.add(remark);

        // Mark item as Sent immediately so it leaves the pending list.
        self.review_store.mark_sent(&[item.id]);

        // Advance selection to the next remaining item.
        let remaining = self.review_store.pending().len();
        self.selected_review = if remaining == 0 {
            None
        } else {
            Some(idx.min(remaining - 1))
        };

        if remaining == 0 {
            self.mode = AppMode::Normal;
            self.status_message = Some("All review items resolved — sending patches…".to_string());
        }

        self.send_remarks().await;
    }

    pub fn dismiss_review(&mut self) {
        if let Some(idx) = self.selected_review {
            let pending = self.review_store.pending();
            if let Some(item) = pending.get(idx) {
                let id = item.id;
                self.review_store.dismiss(id);
                let new_len = self.review_store.pending().len();
                self.selected_review = if new_len == 0 {
                    None
                } else {
                    Some(idx.min(new_len - 1))
                };
            }
        }
    }

    pub fn clear_review_results(&mut self) {
        self.review_store.clear();
        self.selected_review = None;
        self.input.clear();
        self.mode = AppMode::ReviewMode;
        self.status_message = Some("Cleared cached review results. Press q to close, then A to analyze again.".to_string());
    }

    // ── Misc ─────────────────────────────────────────────────────────────────

    pub fn save_doc(&mut self) {
        match self.doc.save() {
            Ok(()) => self.status_message = Some("Document saved.".to_string()),
            Err(e) => self.status_message = Some(format!("Save error: {}", e)),
        }
    }

    pub fn undo(&mut self) {
        if self.doc.undo() {
            self.refresh_display();
            self.status_message = Some("Undone.".to_string());
        } else {
            self.status_message = Some("Nothing to undo.".to_string());
        }
    }

    pub fn redo(&mut self) {
        if self.doc.redo() {
            self.refresh_display();
            self.status_message = Some("Redone.".to_string());
        } else {
            self.status_message = Some("Nothing to redo.".to_string());
        }
    }

    // ── Event handler (called from the async event channel) ──────────────────

    pub async fn handle_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::StreamToken(t) => {
                self.streaming_response.push_str(&t);
            }
            AppEvent::StreamDone => {
                self.is_loading = false;
                self.streaming_response.clear();
            }
            AppEvent::StreamError(e) => {
                self.is_loading = false;
                if self.analysis_in_progress() {
                    self.open_review_when_ready = false;
                }
                self.clear_request_progress();
                self.status_message = Some(format!("Error: {}", e));
                self.streaming_response.clear();
            }
            AppEvent::PatchesReceived(patches, snapshot) => {
                self.is_loading = false;
                self.clear_request_progress();
                self.search_hits.clear();
                self.selected_search_hit = None;
                match self.doc.apply_patches(patches, Some(&snapshot)) {
                    Ok((applied, skipped)) => {
                        let anchors: std::collections::HashSet<String> =
                            applied.iter().cloned().collect();
                        for r in self.remarks.remarks.iter_mut() {
                            if anchors.contains(&r.anchor) {
                                r.status = RemarkStatus::Applied;
                            }
                        }
                        for r in self.remarks.remarks.iter_mut() {
                            if skipped.contains(&r.anchor) {
                                r.status = RemarkStatus::Failed;
                            }
                        }
                        for item in self.review_store.items.iter_mut() {
                            if item.status == crate::review::ReviewStatus::Sent {
                                item.status = crate::review::ReviewStatus::Applied;
                            }
                        }
                        let remaining = self.review_store.pending().len();
                        self.selected_review = if remaining == 0 {
                            None
                        } else {
                            Some(self.selected_review.unwrap_or(0).min(remaining - 1))
                        };
                        self.refresh_display();
                        if let Err(e) = self.doc.save() {
                            self.status_message = Some(format!("Autosave failed: {}", e));
                        } else if skipped.is_empty() {
                            self.status_message = Some(format!(
                                "Applied {} patch(es) and saved.",
                                applied.len()
                            ));
                        } else {
                            self.status_message = Some(format!(
                                "Applied {} patch(es), skipped {} (document changed since request), and saved.",
                                applied.len(),
                                skipped.len()
                            ));
                        }
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Patch error: {}", e));
                    }
                }
            }
            AppEvent::ReviewReceived(items) => {
                self.is_loading = false;
                self.clear_request_progress();
                let should_open_review = self.open_review_when_ready && self.mode == AppMode::Normal;
                self.open_review_when_ready = false;
                let n = items.len();
                self.review_store.clear();
                for item in items {
                    self.review_store.add(item);
                }
                if n == 0 {
                    self.status_message = Some("Analysis complete — no issues found.".to_string());
                } else if should_open_review {
                    self.mode = AppMode::ReviewMode;
                    self.selected_review = Some(0);
                    self.status_message = Some(format!(
                        "Review found {} issue(s). j/k navigate  a answer  y accept  d dismiss  x clear results",
                        n
                    ));
                } else {
                    if self.selected_review.is_none() {
                        self.selected_review = Some(0);
                    }
                    self.status_message = Some(format!(
                        "Analysis found {} issue(s). Press A to open review.",
                        n
                    ));
                }
            }
            AppEvent::DocumentCreated(content) => {
                self.is_loading = false;
                self.clear_request_progress();
                self.search_hits.clear();
                self.selected_search_hit = None;
                match self.doc.set_content(content) {
                    Ok(()) => match self.doc.save() {
                        Ok(()) => {
                            self.input.clear();
                            self.refresh_display();
                            self.mode = AppMode::Normal;
                            self.status_message = Some(
                                "Document created and saved! J/K to navigate, r to add remarks."
                                    .to_string(),
                            );
                        }
                        Err(e) => {
                            self.status_message =
                                Some(format!("Created but save failed: {}", e));
                        }
                    },
                    Err(e) => {
                        self.status_message =
                            Some(format!("Failed to process document: {}", e));
                    }
                }
            }
            AppEvent::StatusMessage(msg) => {
                self.status_message = Some(msg);
            }
            AppEvent::Loading(v) => {
                self.is_loading = v;
            }
            AppEvent::RequestProgress(chars) => {
                if let Some((_, current_chars)) = self.request_progress.as_mut() {
                    *current_chars = chars;
                }
            }
        }
    }
}
