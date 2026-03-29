use crate::config::Config;
use crate::document::{truncate_chars, AnchorId, Document, PatchOp, StyledLine};
use crate::openai::client::{OpenAiClient, ResponsePayload, ResponseRequest};
use crate::openai::prompts;
use crate::openai::session::DocumentSessionStore;
use crate::remarks::{Remark, RemarkStatus, RemarkStore, TargetType};
use crate::review::{ReviewItem, ReviewStore};
use crate::tui::input::InputBuffer;
use anyhow::Result;
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
    PatchReceived {
        remark_id: Uuid,
        patches: Vec<PatchOp>,
        snapshot: HashMap<AnchorId, crate::document::BlockFingerprint>,
        response_id: String,
    },
    PatchFailed {
        remark_id: Uuid,
        message: String,
    },
    ReviewReceived {
        items: Vec<ReviewItem>,
        response_id: String,
    },
    AnalysisFailed(String),
    DocumentCreated {
        content: String,
        response_id: String,
    },
    CreationFailed(String),
    StatusMessage(String),
}

pub struct App {
    pub config: Config,
    pub doc: Document,
    pub remarks: RemarkStore,
    pub review_store: ReviewStore,
    pub mode: AppMode,
    session_store: DocumentSessionStore,
    pub scroll_offset: usize,
    pub selected_node: Option<usize>,
    /// Which line within the currently selected code block is highlighted (0-based).
    /// `None` means the whole block is selected (no intra-block selection active).
    pub selected_line_in_node: Option<usize>,
    /// Shared input buffer used by RemarkEdit, ReviewAnswer, and CreationPrompt.
    pub input: InputBuffer,
    pub status_message: Option<String>,
    pub is_loading: bool,
    active_request_count: usize,
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
    /// Search hits for the Ctrl-F search flow.
    pub search_hits: Vec<(String, String)>,
    /// Currently selected search hit index.
    pub selected_search_hit: Option<usize>,
    /// Incremented every event-loop tick; drives spinner + gradient animations.
    pub spinner_tick: u64,
    /// Label + chars received so far during a long-running request. `None` when
    /// no request-progress overlay should be shown.
    pub request_progress: Option<(String, usize)>,
    /// True when the user asked to see review results as soon as background
    /// analysis finishes.
    pub open_review_when_ready: bool,
    /// Anchor currently being edited in DirectEdit mode.
    pub direct_edit_anchor: Option<String>,
    /// Column highlighted in the currently selected Table node (None = whole row).
    pub selected_table_col: Option<usize>,
    /// (row, col) of the table cell being edited via DirectEdit.
    pub direct_edit_table_cell: Option<(usize, usize)>,
}

impl App {
    pub fn new(config: Config, doc: Document, event_tx: mpsc::Sender<AppEvent>) -> Result<Self> {
        let is_new = doc.is_new();
        let display_lines = doc.render_display(80, &HashSet::new());
        let mode = if is_new {
            AppMode::CreationPrompt
        } else {
            AppMode::Normal
        };
        let status = if is_new {
            Some("New file — describe what to create, then press Enter to generate.".to_string())
        } else {
            Some(
                "Press ? for help  ←/→ collapse/expand heading  c: collapse/expand all".to_string(),
            )
        };
        let session_store = DocumentSessionStore::for_doc(&doc.path)?;
        Ok(App {
            config,
            doc,
            remarks: RemarkStore::new(),
            review_store: ReviewStore::new(),
            mode,
            session_store,
            scroll_offset: 0,
            selected_node: None,
            selected_line_in_node: None,
            input: InputBuffer::new(),
            status_message: status,
            is_loading: false,
            active_request_count: 0,
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
            search_hits: vec![],
            selected_search_hit: None,
            spinner_tick: 0,
            request_progress: None,
            open_review_when_ready: false,
            direct_edit_anchor: None,
            selected_table_col: None,
            direct_edit_table_cell: None,
        })
    }

    fn start_request_progress(&mut self, label: impl Into<String>) {
        self.request_progress = Some((label.into(), 0));
    }

    fn clear_request_progress(&mut self) {
        self.request_progress = None;
    }

    fn begin_request(&mut self) {
        self.active_request_count = self.active_request_count.saturating_add(1);
        self.is_loading = self.active_request_count > 0;
    }

    fn finish_request(&mut self) {
        self.active_request_count = self.active_request_count.saturating_sub(1);
        self.is_loading = self.active_request_count > 0;
    }

    fn analysis_in_progress(&self) -> bool {
        matches!(
            self.request_progress.as_ref(),
            Some((label, _)) if label == "ANALYZING DOCUMENT"
        )
    }

    async fn execute_response(
        client: OpenAiClient,
        req: ResponseRequest,
    ) -> Result<ResponsePayload> {
        client.respond(req).await
    }

    pub fn refresh_display(&mut self) {
        self.display_lines = self.doc.render_display(
            self.terminal_width.saturating_sub(4) as usize,
            &self.collapsed_sections,
        );
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
        self.doc
            .nodes
            .get(node_idx)
            .map(|n| matches!(n.kind, crate::document::NodeKind::CodeBlock { .. }))
            .unwrap_or(false)
    }

    fn is_table(&self, node_idx: usize) -> bool {
        self.doc
            .nodes
            .get(node_idx)
            .map(|n| matches!(n.kind, crate::document::NodeKind::Table { .. }))
            .unwrap_or(false)
    }

    fn table_row_count(&self, node_idx: usize) -> usize {
        if let Some(node) = self.doc.nodes.get(node_idx) {
            if let crate::document::NodeKind::Table { rows, .. } = &node.kind {
                return rows.len();
            }
        }
        0
    }

    fn table_col_count(&self, node_idx: usize) -> usize {
        if let Some(node) = self.doc.nodes.get(node_idx) {
            if let crate::document::NodeKind::Table { headers, .. } = &node.kind {
                return headers.len();
            }
        }
        0
    }

    pub fn is_on_table(&self) -> bool {
        self.selected_node
            .map(|ni| self.is_table(ni) && self.selected_line_in_node.is_some())
            .unwrap_or(false)
    }

    pub fn table_next_col(&mut self) {
        if let Some(ni) = self.selected_node {
            let col_count = self.table_col_count(ni);
            if col_count == 0 {
                return;
            }
            self.selected_table_col = Some(match self.selected_table_col {
                None => 0,
                Some(c) => (c + 1).min(col_count - 1),
            });
        }
    }

    pub fn table_prev_col(&mut self) {
        if let Some(ni) = self.selected_node {
            let col_count = self.table_col_count(ni);
            if col_count == 0 {
                return;
            }
            self.selected_table_col = Some(match self.selected_table_col {
                None | Some(0) => 0,
                Some(c) => c - 1,
            });
        }
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
                self.selected_line_in_node = None;
            } else if self.is_table(cur) {
                let count = self.table_row_count(cur);
                if line + 1 < count {
                    self.selected_line_in_node = Some(line + 1);
                    self.scroll_to_code_line(cur, line + 1);
                    return;
                }
                self.selected_line_in_node = None;
                self.selected_table_col = None;
            }
        }

        let visible = self.doc.visible_node_indices(&self.collapsed_sections);
        if visible.is_empty() {
            return;
        }
        let next = match self.selected_node {
            None => visible[0],
            Some(cur) => {
                let pos = visible.iter().position(|&i| i == cur).unwrap_or(0);
                visible[(pos + 1).min(visible.len() - 1)]
            }
        };
        if self.is_code_block(next) && self.code_block_line_count(next) > 0 {
            self.selected_line_in_node = Some(0);
            self.selected_table_col = None;
        } else if self.is_table(next) && self.table_row_count(next) > 0 {
            self.selected_line_in_node = Some(0);
            self.selected_table_col = None;
        } else {
            self.selected_line_in_node = None;
            self.selected_table_col = None;
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
                self.selected_line_in_node = None;
            } else if self.is_table(cur) {
                if line > 0 {
                    self.selected_line_in_node = Some(line - 1);
                    self.scroll_to_code_line(cur, line - 1);
                    return;
                }
                self.selected_line_in_node = None;
                self.selected_table_col = None;
            }
        }

        let visible = self.doc.visible_node_indices(&self.collapsed_sections);
        if visible.is_empty() {
            return;
        }
        let prev = match self.selected_node {
            None => visible[0],
            Some(cur) => {
                let pos = visible.iter().position(|&i| i == cur).unwrap_or(0);
                visible[pos.saturating_sub(1)]
            }
        };
        if self.is_code_block(prev) {
            let count = self.code_block_line_count(prev);
            self.selected_line_in_node = if count > 0 { Some(count - 1) } else { None };
            self.selected_table_col = None;
        } else if self.is_table(prev) {
            let count = self.table_row_count(prev);
            self.selected_line_in_node = if count > 0 { Some(count - 1) } else { None };
            self.selected_table_col = None;
        } else {
            self.selected_line_in_node = None;
            self.selected_table_col = None;
        }
        self.selected_node = Some(prev);
        if let Some(line) = self.selected_line_in_node {
            self.scroll_to_code_line(prev, line);
        } else {
            self.scroll_to_node(prev);
        }
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
        let Some(idx) = self.selected_node else {
            return;
        };
        let Some(node) = self.doc.nodes.get(idx) else {
            return;
        };
        let crate::document::NodeKind::Heading { .. } = node.kind else {
            return;
        };

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
        let Some(idx) = self.selected_node else {
            return;
        };
        let Some(node) = self.doc.nodes.get(idx) else {
            return;
        };
        let crate::document::NodeKind::Heading { .. } = node.kind else {
            return;
        };

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
            format!("{} snapshot(s). ↑/↓ navigate  Enter restore  q close", n)
        });
    }

    pub fn history_next(&mut self) {
        if self.history_entries.is_empty() {
            return;
        }
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
        if self.history_entries.is_empty() {
            return;
        }
        let content = self.history_preview.clone();
        if content.is_empty() {
            return;
        }
        // Snapshot the current state before overwriting.
        match self.doc.set_content(content) {
            Ok(()) => {
                self.refresh_display();
                self.mode = AppMode::Normal;
                let label = self
                    .history_entries
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
        if let Some(pos) = self
            .display_lines
            .iter()
            .position(|l| l.node_index == Some(node_idx) && l.line_in_block == Some(line_in_block))
        {
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
                self.selected_table_col = None;
                self.scroll_to_code_line(node_idx, 0);
            } else if self.is_table(node_idx) && self.table_row_count(node_idx) > 0 {
                self.selected_line_in_node = Some(0);
                self.selected_table_col = None;
                self.scroll_to_code_line(node_idx, 0);
            } else {
                self.selected_line_in_node = None;
                self.selected_table_col = None;
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

        if let crate::document::NodeKind::Table { rows, .. } = &node.kind {
            if let (Some(row_idx), Some(col_idx)) =
                (self.selected_line_in_node, self.selected_table_col)
            {
                return rows.get(row_idx).and_then(|r| r.get(col_idx)).cloned();
            }
        }

        self.doc
            .raw
            .get(node.source_start..node.source_end)
            .map(|text| text.to_string())
    }

    pub fn copy_current_selection(&mut self) {
        let Some(selection) = self.current_selection_text() else {
            self.status_message =
                Some("Select something first, then press Ctrl+C to copy.".to_string());
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

    // ── Link activation ──────────────────────────────────────────────────────

    pub fn activate_link(&mut self) {
        let Some(node_idx) = self.selected_node else {
            self.status_message = Some("No node selected.".to_string());
            return;
        };
        let url = self
            .display_lines
            .iter()
            .filter(|l| l.node_index == Some(node_idx))
            .find_map(|l| l.first_link_url.clone());
        let Some(url) = url else {
            self.status_message = Some("No link on selected node.".to_string());
            return;
        };
        if url.starts_with('#') {
            match self.doc.resolve_fragment(&url) {
                Some(target) => {
                    self.selected_node = Some(target);
                    self.selected_line_in_node = None;
                    self.selected_table_col = None;
                    self.scroll_to_node(target);
                    self.status_message = Some(format!("Jumped to '{}'.", url));
                }
                None => {
                    self.status_message = Some(format!("Section '{}' not found.", url));
                }
            }
        } else {
            let opener = if cfg!(target_os = "macos") {
                "open"
            } else {
                "xdg-open"
            };
            match std::process::Command::new(opener).arg(&url).spawn() {
                Ok(_) => self.status_message = Some(format!("Opening: {}", url)),
                Err(e) => self.status_message = Some(format!("Cannot open '{}': {}", url, e)),
            }
        }
    }

    // ── Remark flow ──────────────────────────────────────────────────────────

    pub fn start_search(&mut self) {
        self.mode = AppMode::Search;
        self.input.clear();
        self.search_hits.clear();
        self.selected_search_hit = None;
        self.status_message = Some(
            "Search: type to find matches, Enter next, Shift+Enter previous, Esc close."
                .to_string(),
        );
    }

    pub fn update_search(&mut self) {
        let query = self.input.text().trim().to_string();
        if query.is_empty() {
            self.search_hits.clear();
            self.selected_search_hit = None;
            self.status_message = Some(
                "Search: type to find matches, Enter next, Shift+Enter previous, Esc close."
                    .to_string(),
            );
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
            if self.search_hits.len() == 1 {
                ""
            } else {
                "es"
            },
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

    pub fn start_direct_edit(&mut self) {
        let Some(node_idx) = self.selected_node else {
            self.status_message = Some("Select a block first, then press e.".to_string());
            return;
        };

        let Some(node) = self.doc.nodes.get(node_idx) else {
            self.status_message = Some("Selected block is unavailable.".to_string());
            return;
        };

        // Table cell editing: requires a row + column to be selected.
        if let crate::document::NodeKind::Table { rows, .. } = &node.kind {
            match (self.selected_line_in_node, self.selected_table_col) {
                (Some(row_idx), Some(col_idx)) => {
                    let cell = rows
                        .get(row_idx)
                        .and_then(|r| r.get(col_idx))
                        .cloned()
                        .unwrap_or_default();
                    self.direct_edit_anchor = Some(node.anchor.clone());
                    self.direct_edit_table_cell = Some((row_idx, col_idx));
                    self.input.set_text(cell);
                    self.mode = AppMode::DirectEdit;
                    self.status_message = Some(
                        "Editing cell. Enter save  Esc cancel  (pipes escaped automatically)"
                            .to_string(),
                    );
                    return;
                }
                _ => {
                    self.status_message = Some(
                        "Navigate to a cell first (←→ to select a column, then e).".to_string(),
                    );
                    return;
                }
            }
        }

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
        self.status_message =
            Some("Editing block locally. Enter save  Alt+Enter newline  Esc cancel".to_string());
    }

    pub fn submit_direct_edit(&mut self) {
        let Some(anchor) = self.direct_edit_anchor.clone() else {
            self.status_message = Some("No block is being edited.".to_string());
            self.mode = AppMode::Normal;
            return;
        };

        // Table cell edit path.
        if let Some((row_idx, col_idx)) = self.direct_edit_table_cell {
            let new_cell = self.input.text().to_string();
            let node_idx = match self.doc.anchor_map.get(&anchor) {
                Some(&i) => i,
                None => {
                    self.status_message = Some("Edited table no longer exists.".to_string());
                    self.mode = AppMode::Normal;
                    self.input.clear();
                    self.direct_edit_anchor = None;
                    self.direct_edit_table_cell = None;
                    return;
                }
            };
            if let Some(crate::document::NodeKind::Table { headers, rows }) =
                self.doc.nodes.get(node_idx).map(|n| n.kind.clone())
            {
                let mut new_rows = rows.clone();
                if row_idx < new_rows.len() && col_idx < new_rows[row_idx].len() {
                    new_rows[row_idx][col_idx] = new_cell;
                }
                let new_raw = crate::document::rebuild_table_raw(&headers, &new_rows);
                let patch = PatchOp::ReplaceSection {
                    anchor: anchor.clone(),
                    content: new_raw,
                    rationale: "local direct edit".to_string(),
                };
                self.direct_edit_table_cell = None;
                match self.doc.apply_patches(vec![patch], None) {
                    Ok((applied, skipped)) => {
                        self.refresh_display();
                        self.input.clear();
                        self.direct_edit_anchor = None;
                        self.mode = AppMode::Normal;
                        if let Err(e) = self.doc.save() {
                            self.status_message =
                                Some(format!("Saved in memory, write failed: {}", e));
                        } else if skipped.is_empty() {
                            self.status_message =
                                Some(format!("Updated {} block(s) locally.", applied.len()));
                        } else {
                            self.status_message = Some(format!(
                                "Updated {}, skipped {}.",
                                applied.len(),
                                skipped.len()
                            ));
                        }
                    }
                    Err(e) => {
                        self.status_message = Some(format!("Cell edit failed: {}", e));
                    }
                }
            } else {
                self.direct_edit_table_cell = None;
                self.status_message = Some("Table no longer available.".to_string());
                self.mode = AppMode::Normal;
                self.input.clear();
                self.direct_edit_anchor = None;
            }
            return;
        }

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
                    let mut lines: Vec<String> =
                        code.lines().map(|line| line.to_string()).collect();
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
                    self.status_message =
                        Some("Only code lines support line-level direct edit.".to_string());
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
                    self.status_message =
                        Some(format!("Saved edit in memory, but write failed: {}", e));
                } else if skipped.is_empty() {
                    self.status_message =
                        Some(format!("Updated {} block(s) locally.", applied.len()));
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

    pub fn start_remark(&mut self) {
        let Some(idx) = self.selected_node else {
            self.status_message = Some("Select a node first (↑/↓ to navigate)".to_string());
            return;
        };

        self.clear_occurrences();
        self.input.clear();
        self.mode = AppMode::RemarkEdit;
        self.status_message = Some(format!(
            "Remark on {} — describe the change and press Enter.",
            self.doc.nodes[idx].anchor
        ));
    }

    pub fn clear_occurrences(&mut self) {}

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
                    (
                        text.clone(),
                        node.anchor.clone(),
                        TargetType::ListItem,
                        Some(ctx),
                    )
                }
                crate::document::NodeKind::BlockQuote { text } => (
                    text.clone(),
                    node.anchor.clone(),
                    TargetType::TextSpan,
                    None,
                ),
                _ => (
                    String::new(),
                    node.anchor.clone(),
                    TargetType::TextSpan,
                    None,
                ),
            };
            let remark = Remark {
                id: Uuid::new_v4(),
                source_review_id: None,
                anchor,
                selected_text,
                target_type,
                text,
                list_context,
                occurrence_anchors: Vec::new(),
                created_at: Utc::now(),
                status: RemarkStatus::Pending,
            };
            self.remarks.add(remark);
            self.input.clear();
            self.mode = AppMode::Normal;
            if self.remark_request_in_flight() {
                self.status_message = Some(
                    "Queued remark — it will be sent after the current patch finishes.".to_string(),
                );
            } else {
                self.status_message = Some("Sending remark for patch generation…".to_string());
                self.send_next_remark().await;
            }
        }
    }

    /// Collect all contiguous ListItem nodes around `node_idx` and return
    /// them formatted as a markdown list with anchor annotations.
    fn collect_list_context(&self, node_idx: usize) -> String {
        let nodes = &self.doc.nodes;

        // Walk backward to the first item in this contiguous list.
        let mut start = node_idx;
        while start > 0 {
            if matches!(
                nodes[start - 1].kind,
                crate::document::NodeKind::ListItem { .. }
            ) {
                start -= 1;
            } else {
                break;
            }
        }

        // Walk forward to the last item.
        let mut end = node_idx;
        while end + 1 < nodes.len() {
            if matches!(
                nodes[end + 1].kind,
                crate::document::NodeKind::ListItem { .. }
            ) {
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
        self.direct_edit_table_cell = None;
        self.mode = AppMode::Normal;
    }

    // ── AI submission flows ──────────────────────────────────────────────────

    fn remark_request_in_flight(&self) -> bool {
        self.remarks
            .remarks
            .iter()
            .any(|remark| remark.status == RemarkStatus::Sent)
    }

    pub async fn send_next_remark(&mut self) {
        if self.remark_request_in_flight() {
            return;
        }

        let Some(remark_id) = self
            .remarks
            .remarks
            .iter()
            .find(|remark| remark.status == RemarkStatus::Pending)
            .map(|remark| remark.id)
        else {
            return;
        };

        let Some(remark) = self.remarks.get(remark_id).cloned() else {
            return;
        };

        if let Some(current) = self.remarks.get_mut(remark_id) {
            current.status = RemarkStatus::Sent;
        }
        if let Some(review_id) = remark.source_review_id {
            self.review_store.mark_sent(&[review_id]);
        }

        self.begin_request();

        let previous_response_id = self.session_store.patch_previous_response_id();
        let req = prompts::build_revision_request(
            &self.config,
            &self.doc,
            &[&remark],
            previous_response_id,
        );

        let config = Arc::new(self.config.clone());
        let client = OpenAiClient::new(config);
        let snapshot = self.doc.content_snapshot();
        let tx = self.event_tx.clone();

        tokio::spawn(async move {
            match Self::execute_response(client, req).await {
                Ok(payload) => match prompts::parse_revision_response(&payload.text) {
                    Ok(patches) if patches.is_empty() => {
                        let _ = tx
                            .send(AppEvent::PatchFailed {
                                remark_id,
                                message: "No patches were returned for this remark.".to_string(),
                            })
                            .await;
                    }
                    Ok(patches) => {
                        let _ = tx
                            .send(AppEvent::PatchReceived {
                                remark_id,
                                patches,
                                snapshot,
                                response_id: payload.id,
                            })
                            .await;
                    }
                    Err(e) => {
                        let _ = tx
                            .send(AppEvent::PatchFailed {
                                remark_id,
                                message: e.to_string(),
                            })
                            .await;
                    }
                },
                Err(e) => {
                    let _ = tx
                        .send(AppEvent::PatchFailed {
                            remark_id,
                            message: e.to_string(),
                        })
                        .await;
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
                    "{} pending review item(s). ↑/↓ navigate  a answer  y accept  d dismiss  x clear results  q close",
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

        self.begin_request();
        self.start_request_progress("ANALYZING DOCUMENT");
        self.status_message = Some(
            "Analyzing document in the background… keep editing; review will open when ready."
                .to_string(),
        );

        let config = Arc::new(self.config.clone());
        let client = OpenAiClient::new(config);
        let req = prompts::build_ambiguity_request(
            &self.config,
            &self.doc,
            self.session_store.analysis_previous_response_id(),
        );
        let tx = self.event_tx.clone();

        tokio::spawn(async move {
            match Self::execute_response(client, req).await {
                Ok(payload) => match prompts::parse_review_response(&payload.text) {
                    Ok(items) => {
                        let _ = tx
                            .send(AppEvent::ReviewReceived {
                                items,
                                response_id: payload.id,
                            })
                            .await;
                    }
                    Err(e) => {
                        let _ = tx.send(AppEvent::AnalysisFailed(e.to_string())).await;
                    }
                },
                Err(e) => {
                    let _ = tx.send(AppEvent::AnalysisFailed(e.to_string())).await;
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
        self.begin_request();
        self.start_request_progress("CREATING DOCUMENT");
        self.status_message = Some("Creating document…".to_string());

        let config = Arc::new(self.config.clone());
        let client = OpenAiClient::new(config);
        let req = prompts::build_creation_request(&self.config, &prompt, None);
        let tx = self.event_tx.clone();

        tokio::spawn(async move {
            match Self::execute_response(client, req).await {
                Ok(payload) => {
                    let _ = tx
                        .send(AppEvent::DocumentCreated {
                            content: payload.text,
                            response_id: payload.id,
                        })
                        .await;
                }
                Err(e) => {
                    let _ = tx.send(AppEvent::CreationFailed(e.to_string())).await;
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
    /// queue it for patch generation, advance the selection, then fire the
    /// next patch request when possible.
    async fn submit_review_item(&mut self, idx: usize, answer: String) {
        let pending: Vec<_> = self.review_store.pending().into_iter().cloned().collect();
        let item = match pending.get(idx) {
            Some(i) => i.clone(),
            None => return,
        };

        self.review_store.mark_pending(item.id, answer.clone());

        let remark = Remark {
            id: Uuid::new_v4(),
            source_review_id: Some(item.id),
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
            occurrence_anchors: Vec::new(),
            created_at: Utc::now(),
            status: RemarkStatus::Pending,
        };
        self.remarks.add(remark);

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

        if self.remark_request_in_flight() {
            self.status_message = Some(
                "Queued review fix — it will be sent after the current patch finishes.".to_string(),
            );
        } else {
            self.status_message = Some("Sending accepted review fix…".to_string());
            self.send_next_remark().await;
        }
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
        self.status_message = Some(
            "Cleared cached review results. Press q to close, then Shift+A to analyze again."
                .to_string(),
        );
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
            AppEvent::PatchReceived {
                remark_id,
                patches,
                snapshot,
                response_id,
            } => {
                self.finish_request();
                self.search_hits.clear();
                self.selected_search_hit = None;
                match self.doc.apply_patches(patches, Some(&snapshot)) {
                    Ok((applied, skipped)) => {
                        let review_id = self
                            .remarks
                            .get(remark_id)
                            .and_then(|remark| remark.source_review_id);
                        if let Some(remark) = self.remarks.get_mut(remark_id) {
                            remark.status = if applied.is_empty() {
                                RemarkStatus::Failed
                            } else {
                                RemarkStatus::Applied
                            };
                        }
                        if let Some(review_id) = review_id {
                            if applied.is_empty() {
                                self.review_store.mark_answered(review_id);
                            } else {
                                self.review_store.mark_applied(review_id);
                            }
                        }
                        let remaining = self.review_store.pending().len();
                        self.selected_review = if remaining == 0 {
                            None
                        } else {
                            Some(self.selected_review.unwrap_or(0).min(remaining - 1))
                        };
                        self.refresh_display();
                        let session_warning = self
                            .session_store
                            .set_patch_previous_response_id(Some(response_id))
                            .err()
                            .map(|e| format!(" Session state was not saved: {}", e))
                            .unwrap_or_default();
                        if let Err(e) = self.doc.save() {
                            self.status_message =
                                Some(format!("Autosave failed: {}{}", e, session_warning));
                        } else if applied.is_empty() {
                            self.status_message = Some(format!(
                                "No patches applied for this remark; {} target(s) were stale.{}",
                                skipped.len(),
                                session_warning
                            ));
                        } else if skipped.is_empty() {
                            self.status_message = Some(format!(
                                "Applied {} patch(es) and saved.{}",
                                applied.len(),
                                session_warning
                            ));
                        } else {
                            self.status_message = Some(format!(
                                "Applied {} patch(es), skipped {} (document changed since request), and saved.{}",
                                applied.len(),
                                skipped.len(),
                                session_warning
                            ));
                        }

                        if self.remarks.pending().len() > 0 {
                            self.status_message = Some(format!(
                                "{} Sending next queued remark…",
                                self.status_message.as_deref().unwrap_or("Done.")
                            ));
                            self.send_next_remark().await;
                        }
                    }
                    Err(e) => {
                        if let Some(remark) = self.remarks.get_mut(remark_id) {
                            remark.status = RemarkStatus::Failed;
                            if let Some(review_id) = remark.source_review_id {
                                self.review_store.mark_answered(review_id);
                            }
                        }
                        self.status_message = Some(format!("Patch error: {}", e));
                    }
                }
            }
            AppEvent::PatchFailed { remark_id, message } => {
                self.finish_request();
                if let Some(remark) = self.remarks.get_mut(remark_id) {
                    remark.status = RemarkStatus::Failed;
                    if let Some(review_id) = remark.source_review_id {
                        self.review_store.mark_answered(review_id);
                    }
                }
                self.status_message = Some(format!("Patch request failed: {}", message));
            }
            AppEvent::ReviewReceived { items, response_id } => {
                self.finish_request();
                self.clear_request_progress();
                let should_open_review =
                    self.open_review_when_ready && self.mode == AppMode::Normal;
                self.open_review_when_ready = false;
                let n = items.len();
                self.review_store.clear();
                for item in items {
                    self.review_store.add(item);
                }
                let session_warning = self
                    .session_store
                    .set_analysis_previous_response_id(Some(response_id))
                    .err()
                    .map(|e| format!(" Session state was not saved: {}", e))
                    .unwrap_or_default();
                if n == 0 {
                    self.status_message = Some(format!(
                        "Analysis complete — no issues found.{}",
                        session_warning
                    ));
                } else if should_open_review {
                    self.mode = AppMode::ReviewMode;
                    self.selected_review = Some(0);
                    self.status_message = Some(format!(
                        "Review found {} issue(s). ↑/↓ navigate  a answer  y accept  d dismiss  x clear results{}",
                        n,
                        session_warning
                    ));
                } else {
                    if self.selected_review.is_none() {
                        self.selected_review = Some(0);
                    }
                    self.status_message = Some(format!(
                        "Analysis found {} issue(s). Press Shift+A to open review.{}",
                        n, session_warning
                    ));
                }
            }
            AppEvent::AnalysisFailed(message) => {
                self.finish_request();
                self.open_review_when_ready = false;
                self.clear_request_progress();
                self.status_message = Some(format!("Analysis failed: {}", message));
            }
            AppEvent::DocumentCreated {
                content,
                response_id,
            } => {
                self.finish_request();
                self.clear_request_progress();
                self.search_hits.clear();
                self.selected_search_hit = None;
                match self.doc.set_content(content) {
                    Ok(()) => match self.doc.save() {
                        Ok(()) => {
                            let session_warning = self
                                .session_store
                                .set_patch_previous_response_id(Some(response_id))
                                .err()
                                .map(|e| format!(" Session state was not saved: {}", e))
                                .unwrap_or_default();
                            self.input.clear();
                            self.refresh_display();
                            self.mode = AppMode::Normal;
                            self.status_message = Some(
                                format!(
                                    "Document created and saved! Use ↑/↓ to navigate, r to add a remark.{}",
                                    session_warning
                                ),
                            );
                        }
                        Err(e) => {
                            self.status_message = Some(format!("Created but save failed: {}", e));
                        }
                    },
                    Err(e) => {
                        self.status_message = Some(format!("Failed to process document: {}", e));
                    }
                }
            }
            AppEvent::CreationFailed(message) => {
                self.finish_request();
                self.clear_request_progress();
                self.status_message = Some(format!("Creation failed: {}", message));
            }
            AppEvent::StatusMessage(msg) => {
                self.status_message = Some(msg);
            }
        }
    }
}
