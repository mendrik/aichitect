use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{
        Block, Borders, Clear, List, ListItem, ListState, Paragraph,
        Scrollbar, ScrollbarOrientation, ScrollbarState, Wrap,
    },
};

use super::app::{App, AppMode};
use super::input::InputSpan;
use crate::document::{SpanStyle, truncate_chars};

pub fn draw(f: &mut Frame, app: &mut App) {
    let size = f.area();
    app.terminal_width = size.width;
    app.terminal_height = size.height;
    let show_bottom_progress = shows_bottom_progress(app);

    // Full-screen creation prompt: skip the normal layout.
    if app.mode == AppMode::CreationPrompt {
        draw_creation_prompt(f, app, size);
        draw_status_bar(f, app, Rect { y: size.height.saturating_sub(1), height: 1, ..size });
        if app.request_progress.is_some() {
            draw_request_overlay(f, app, size);
        }
        return;
    }

    let main_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(if show_bottom_progress { 1 } else { 0 }),
            Constraint::Length(1),
        ])
        .split(size);

    let content_area = main_chunks[0];
    let progress_area = main_chunks[1];
    let status_area = main_chunks[2];

    let (doc_area, side_area_opt) = if app.show_remarks_panel {
        let chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
            .split(content_area);
        (chunks[0], Some(chunks[1]))
    } else {
        (content_area, None)
    };

    // Store rects for mouse hit-testing.
    app.last_doc_area = doc_area;
    app.last_side_area = side_area_opt;

    draw_document(f, app, doc_area);

    if let Some(ra) = side_area_opt {
        draw_remarks_panel(f, app, ra);
    }

    if show_bottom_progress {
        draw_request_progress_bar(f, app, progress_area);
    }

    draw_status_bar(f, app, status_area);

    match app.mode {
        AppMode::Search => draw_search_popup(f, app, size),
        AppMode::DirectEdit => draw_direct_edit_popup(f, app, size),
        AppMode::RemarkEdit => draw_remark_editor(f, app, size),
        AppMode::ReviewMode | AppMode::ReviewAnswer => draw_review_panel(f, app, size),
        AppMode::HistoryBrowser => draw_history_browser(f, app, size),
        AppMode::Help => draw_help(f, size),
        _ => {}
    }

    if app.request_progress.is_some() && !show_bottom_progress {
        draw_request_overlay(f, app, size);
    }
}

// ── Document pane ─────────────────────────────────────────────────────────────

fn span_style_to_ratatui(style: &SpanStyle) -> Style {
    match style {
        SpanStyle::Normal => Style::default().fg(Color::White),
        SpanStyle::Bold => Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        SpanStyle::Italic => Style::default().fg(Color::White).add_modifier(Modifier::ITALIC),
        SpanStyle::Code => Style::default().fg(Color::Yellow),
        SpanStyle::Heading(1) => Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        SpanStyle::Heading(2) => Style::default().fg(Color::LightCyan).add_modifier(Modifier::BOLD),
        SpanStyle::Heading(_) => Style::default().fg(Color::LightBlue).add_modifier(Modifier::BOLD),
        SpanStyle::CodeBlockLine => Style::default().fg(Color::Yellow),
        SpanStyle::BlockQuote => Style::default().fg(Color::LightMagenta).add_modifier(Modifier::ITALIC),
        SpanStyle::Dimmed => Style::default().fg(Color::DarkGray),
        SpanStyle::Error => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        SpanStyle::TableHeader => Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        SpanStyle::TableBorder => Style::default().fg(Color::DarkGray),
        SpanStyle::Keyword => Style::default().fg(Color::LightCyan).add_modifier(Modifier::BOLD),
        SpanStyle::StringLit => Style::default().fg(Color::LightGreen),
        SpanStyle::Comment => Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
        SpanStyle::Number => Style::default().fg(Color::LightYellow),
        SpanStyle::Operator => Style::default().fg(Color::Gray),
        SpanStyle::Bracket => Style::default().fg(Color::LightMagenta),
    }
}

fn spinner_frame(tick: u64) -> &'static str {
    const FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    FRAMES[(tick / 2) as usize % FRAMES.len()]
}

fn shows_bottom_progress(app: &App) -> bool {
    matches!(
        app.request_progress.as_ref(),
        Some((label, _)) if label == "ANALYZING DOCUMENT"
    )
}

fn draw_document(f: &mut Frame, app: &App, area: Rect) {
    let fname = app.doc.path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("document.md");
    let loading = if app.is_loading {
        format!(" {}", spinner_frame(app.spinner_tick))
    } else {
        String::new()
    };
    let collapsed_hint = if app.collapsed_sections.is_empty() {
        String::new()
    } else {
        format!(" ▶{} collapsed", app.collapsed_sections.len())
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {}{}{} ", fname, collapsed_hint, loading))
        .border_style(Style::default().fg(Color::DarkGray));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let visible_height = inner.height as usize;
    let scroll = app.scroll_offset;

    let occurrence_set: std::collections::HashSet<&str> = app
        .occurrence_hits
        .iter()
        .map(|(hit, _)| hit.as_str())
        .collect();
    let search_set: std::collections::HashSet<&str> = app
        .search_hits
        .iter()
        .map(|(hit, _)| hit.as_str())
        .collect();

    let lines: Vec<Line> = app.display_lines
        .iter()
        .skip(scroll)
        .take(visible_height)
        .map(|sl| {
            let is_selected = match (sl.node_index, app.selected_node) {
                (Some(ni), Some(sel)) if ni == sel => {
                    match app.selected_line_in_node {
                        Some(sel_line) => sl.line_in_block == Some(sel_line),
                        None => true,
                    }
                }
                _ => false,
            };
            let is_occurrence = sl.anchor.as_deref().map(|a| {
                occurrence_set.contains(a)
            }).unwrap_or(false)
            || sl.node_index.and_then(|ni| {
                sl.line_in_block.map(|li| {
                    let node_anchor = &app.doc.nodes[ni].anchor;
                    let line_anchor = format!("{}:L{}", node_anchor, li);
                    occurrence_set.contains(line_anchor.as_str())
                })
            }).unwrap_or(false);
            let is_search_hit = sl.anchor.as_deref().map(|a| {
                search_set.contains(a)
            }).unwrap_or(false)
            || sl.node_index.and_then(|ni| {
                sl.line_in_block.map(|li| {
                    let node_anchor = &app.doc.nodes[ni].anchor;
                    let line_anchor = format!("{}:L{}", node_anchor, li);
                    search_set.contains(line_anchor.as_str())
                })
            }).unwrap_or(false);

            if sl.spans.is_empty() {
                Line::from("")
            } else {
                let selected_col = app.selected_table_col;
                let spans: Vec<Span> = sl.spans.iter().map(|s| {
                    let mut style = span_style_to_ratatui(&s.style);
                    // For table rows, only highlight the selected cell span (or all if no column selected).
                    let apply_sel = is_selected && match (s.cell_col, selected_col) {
                        (Some(sc), Some(tc)) => sc == tc,
                        (None, Some(_)) => false,
                        _ => true,
                    };
                    if apply_sel {
                        style = style.bg(Color::DarkGray);
                    } else if is_occurrence {
                        style = style.bg(Color::Yellow).fg(Color::Black);
                    } else if is_search_hit {
                        style = style.bg(Color::Cyan).fg(Color::Black);
                    }
                    Span::styled(s.text.clone(), style)
                }).collect();
                Line::from(spans)
            }
        })
        .collect();

    f.render_widget(Paragraph::new(Text::from(lines)), inner);

    if app.display_lines.len() > visible_height {
        let mut ss = ScrollbarState::default()
            .content_length(app.display_lines.len())
            .viewport_content_length(visible_height)
            .position(scroll);
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight),
            area,
            &mut ss,
        );
    }
}

// ── Side (remarks) panel ──────────────────────────────────────────────────────

fn draw_remarks_panel(f: &mut Frame, app: &App, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Remarks ")
        .border_style(Style::default().fg(Color::DarkGray));
    let inner = block.inner(area);
    f.render_widget(block, area);

    if app.remarks.remarks.is_empty() {
        f.render_widget(
            Paragraph::new("No remarks yet.\nSelect a node and press r.")
                .style(Style::default().fg(Color::DarkGray)),
            inner,
        );
        return;
    }

    let items: Vec<ListItem> = app.remarks.remarks.iter().map(|r| {
        let (icon, color) = match r.status {
            crate::remarks::RemarkStatus::Draft => ("○", Color::Gray),
            crate::remarks::RemarkStatus::Queued => ("◉", Color::Yellow),
            crate::remarks::RemarkStatus::Sent => ("⟳", Color::Cyan),
            crate::remarks::RemarkStatus::Applied => ("✓", Color::Green),
            crate::remarks::RemarkStatus::Failed => ("✗", Color::Red),
        };
        let anchor = if r.anchor.len() > 15 { &r.anchor[..15] } else { &r.anchor };
        let text = if r.text.chars().count() > 28 {
            format!("{}…", truncate_chars(&r.text, 28))
        } else {
            r.text.clone()
        };
        ListItem::new(Line::from(vec![
            Span::styled(format!("{} ", icon), Style::default().fg(color)),
            Span::styled(format!("[{}] ", anchor), Style::default().fg(Color::DarkGray)),
            Span::styled(text, Style::default().fg(Color::White)),
        ]))
    }).collect();

    let total = items.len();
    let mut state = ListState::default();
    state.select(app.selected_remark);
    *state.offset_mut() = app.side_scroll;

    let list = List::new(items)
        .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD));
    f.render_stateful_widget(list, inner, &mut state);

    if total > inner.height as usize {
        let mut ss = ScrollbarState::default()
            .content_length(total)
            .viewport_content_length(inner.height as usize)
            .position(app.side_scroll);
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight),
            area,
            &mut ss,
        );
    }
}

// ── Status bar ────────────────────────────────────────────────────────────────

fn draw_status_bar(f: &mut Frame, app: &App, area: Rect) {
    let mode_str = match &app.mode {
        AppMode::Normal => "NORMAL",
        AppMode::Search => "SEARCH",
        AppMode::DirectEdit => "EDIT",
        AppMode::RemarkEdit => "REMARK",
        AppMode::ReviewMode => "REVIEW",
        AppMode::ReviewAnswer => "ANSWER",
        AppMode::CreationPrompt => "CREATE",
        AppMode::HistoryBrowser => "HISTORY",
        AppMode::Help => "HELP",
    };
    let msg = app.status_message.as_deref().unwrap_or("");
    let node_info = app.selected_node
        .and_then(|i| app.doc.nodes.get(i))
        .map(|n| format!(" [{}]", n.anchor))
        .unwrap_or_default();
    let qcount = app.remarks.queued().len();
    let total = app.remarks.remarks.len();

    let left = Span::styled(
        format!(" {} ", mode_str),
        Style::default().fg(Color::Black).bg(Color::Cyan).add_modifier(Modifier::BOLD),
    );
    let mid = Span::styled(format!(" {}{} ", msg, node_info), Style::default().fg(Color::White));
    let right = Span::styled(
        format!(" {}r {}q ", total, qcount),
        Style::default().fg(Color::DarkGray),
    );
    let bar = Paragraph::new(Line::from(vec![left, mid, right]))
        .style(Style::default().bg(Color::DarkGray));
    f.render_widget(bar, area);
}

// ── Creation-prompt screen ────────────────────────────────────────────────────

fn draw_creation_prompt(f: &mut Frame, app: &App, area: Rect) {
    // Leave room for the status bar at the bottom.
    let content_area = Rect { height: area.height.saturating_sub(1), ..area };

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(4), Constraint::Min(3), Constraint::Length(2)])
        .split(content_area);

    // Header block
    let fname = app.doc.path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("new-document.md");
    let header = Paragraph::new(vec![
        Line::from(vec![
            Span::styled("New document: ", Style::default().fg(Color::DarkGray)),
            Span::styled(fname, Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        ]),
        Line::from(""),
        Line::from(vec![Span::styled(
            "Describe what you want Aichitect to create.",
            Style::default().fg(Color::DarkGray),
        )]),
    ])
    .block(Block::default().borders(Borders::ALL).title(" Aichitect — Create ")
        .border_style(Style::default().fg(Color::Cyan)));
    f.render_widget(header, chunks[0]);

    // Input area
    let loading = app.is_loading;
    let border_color = if loading { Color::Yellow } else { Color::White };
    let title = if loading { " Generating… " } else { " Your prompt (Enter to create  Alt+Enter for newline) " };

    let input_block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(border_color));
    let inner = input_block.inner(chunks[1]);
    f.render_widget(input_block, chunks[1]);

    let lines = input_spans_to_lines(&app.input.render_spans());
    f.render_widget(
        Paragraph::new(Text::from(lines))
            .style(Style::default().fg(Color::White))
            .wrap(Wrap { trim: false }),
        inner,
    );

    // Hint bar
    let hint = if loading {
        "Waiting for OpenAI response…"
    } else {
        "Enter  generate      Alt+Enter  newline      Esc  quit"
    };
    f.render_widget(
        Paragraph::new(hint).style(Style::default().fg(Color::DarkGray)),
        chunks[2],
    );
}

// ── Remark editor popup ───────────────────────────────────────────────────────

fn draw_remark_editor(f: &mut Frame, app: &App, size: Rect) {
    let area = centered_rect(64, 35, size);
    f.render_widget(Clear, area);

    let node_info = app.selected_node
        .and_then(|i| app.doc.nodes.get(i))
        .map(|n| n.anchor.clone())
        .unwrap_or_default();

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Add Remark → {} ", node_info))
        .border_style(Style::default().fg(Color::Yellow));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(2), Constraint::Length(1)])
        .split(inner);

    let lines = input_spans_to_lines(&app.input.render_spans());
    f.render_widget(
        Paragraph::new(Text::from(lines))
            .style(Style::default().fg(Color::White))
            .wrap(Wrap { trim: false }),
        chunks[0],
    );

    f.render_widget(
        Paragraph::new("Enter submit  Alt+Enter newline  ←→ move  Esc cancel")
            .style(Style::default().fg(Color::DarkGray)),
        chunks[1],
    );
}

fn draw_direct_edit_popup(f: &mut Frame, app: &App, size: Rect) {
    let area = centered_rect(72, 40, size);
    f.render_widget(Clear, area);

    let title_anchor = app.direct_edit_anchor.as_deref().unwrap_or("selection");
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" Edit Block → {} ", title_anchor))
        .border_style(Style::default().fg(Color::Green));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(2), Constraint::Length(1)])
        .split(inner);

    let lines = input_spans_to_lines(&app.input.render_spans());
    f.render_widget(
        Paragraph::new(Text::from(lines))
            .style(Style::default().fg(Color::White))
            .wrap(Wrap { trim: false }),
        chunks[0],
    );

    f.render_widget(
        Paragraph::new("Enter save locally  Alt+Enter newline  Esc cancel")
            .style(Style::default().fg(Color::DarkGray)),
        chunks[1],
    );
}

fn draw_search_popup(f: &mut Frame, app: &App, size: Rect) {
    let area = centered_rect(72, 20, size);
    f.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Search ")
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Length(1), Constraint::Length(1)])
        .split(inner);

    let input_block = Block::default()
        .borders(Borders::ALL)
        .title(" Ctrl-F ")
        .border_style(Style::default().fg(Color::White));
    let input_inner = input_block.inner(chunks[0]);
    f.render_widget(input_block, chunks[0]);

    let lines = input_spans_to_lines(&app.input.render_spans());
    f.render_widget(
        Paragraph::new(Text::from(lines))
            .style(Style::default().fg(Color::White))
            .wrap(Wrap { trim: false }),
        input_inner,
    );

    let summary = if app.input.text().trim().is_empty() {
        Line::from(vec![Span::styled(
            "Type to search the document.",
            Style::default().fg(Color::DarkGray),
        )])
    } else if app.search_hits.is_empty() {
        Line::from(vec![Span::styled(
            "No matches.",
            Style::default().fg(Color::DarkGray),
        )])
    } else {
        let selected = app.selected_search_hit.unwrap_or(0).min(app.search_hits.len() - 1);
        let snippet = &app.search_hits[selected].1;
        let snippet = if snippet.chars().count() > 60 {
            format!("{}…", truncate_chars(snippet, 60))
        } else {
            snippet.clone()
        };
        Line::from(vec![
            Span::styled(
                format!("Match {}/{} ", selected + 1, app.search_hits.len()),
                Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
            ),
            Span::styled(snippet, Style::default().fg(Color::DarkGray)),
        ])
    };

    f.render_widget(Paragraph::new(summary), chunks[1]);
    f.render_widget(
        Paragraph::new("Enter next  Shift+Enter previous  Esc close")
            .style(Style::default().fg(Color::DarkGray)),
        chunks[2],
    );
}

// ── Review panel popup ────────────────────────────────────────────────────────

fn draw_review_panel(f: &mut Frame, app: &App, size: Rect) {
    let area = centered_rect(80, 80, size);
    f.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Document Review ")
        .border_style(Style::default().fg(Color::Magenta));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let pending: Vec<_> = app.review_store.pending().into_iter().cloned().collect();

    if pending.is_empty() {
        let msg = if app.review_store.items.is_empty() {
            "No review items. Press A to analyze."
        } else {
            "All items addressed. Press x to clear cached results or q to exit."
        };
        f.render_widget(
            Paragraph::new(msg).style(Style::default().fg(Color::DarkGray)),
            inner,
        );
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
        .split(inner);

    // Left: item list
    let items: Vec<ListItem> = pending.iter().map(|item| {
        let color = match item.status {
            crate::review::ReviewStatus::Answered => Color::Green,
            _ => Color::Yellow,
        };
        let anchor = if item.anchor.len() > 12 { &item.anchor[..12] } else { &item.anchor };
        ListItem::new(Line::from(vec![
            Span::styled(format!("[{}] ", anchor), Style::default().fg(Color::DarkGray)),
            Span::styled(format!("{}", item.category), Style::default().fg(color)),
        ]))
    }).collect();

    let mut list_state = ListState::default();
    list_state.select(app.selected_review);
    let list = List::new(items)
        .block(Block::default().borders(Borders::RIGHT).border_style(Style::default().fg(Color::DarkGray)))
        .highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD));
    f.render_stateful_widget(list, chunks[0], &mut list_state);

    // Right: detail + answer input
    if let Some(idx) = app.selected_review {
        if let Some(item) = pending.get(idx) {
            let right_chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(5), Constraint::Length(7)])
                .split(chunks[1]);

            let detail = format!(
                "Category: {}\nAnchor: {}\n\nEvidence:\n{}\n\nWhy it matters:\n{}\n\nSuggested resolution:\n{}{}",
                item.category, item.anchor,
                item.evidence, item.why_it_matters, item.suggested_resolution,
                item.user_answer.as_ref().map(|a| format!("\n\nAnswer: {}", a)).unwrap_or_default(),
            );
            f.render_widget(
                Paragraph::new(detail)
                    .style(Style::default().fg(Color::White))
                    .wrap(Wrap { trim: true }),
                right_chunks[0],
            );

            if matches!(app.mode, AppMode::ReviewAnswer) {
                let ab = Block::default()
                    .borders(Borders::ALL)
                    .title(" Your Answer (Enter submit  Alt+Enter newline  ←→ move) ")
                    .border_style(Style::default().fg(Color::Yellow));
                let answer_inner = ab.inner(right_chunks[1]);
                f.render_widget(ab, right_chunks[1]);
                let lines = input_spans_to_lines(&app.input.render_spans());
                f.render_widget(
                    Paragraph::new(Text::from(lines))
                        .style(Style::default().fg(Color::White))
                        .wrap(Wrap { trim: false }),
                    answer_inner,
                );
            } else {
                f.render_widget(
                    Paragraph::new("a answer  y accept resolution  d dismiss  x clear results  q exit")
                        .style(Style::default().fg(Color::DarkGray)),
                    right_chunks[1],
                );
            }
        }
    }
}

// ── Request progress ──────────────────────────────────────────────────────────

fn draw_request_progress_bar(f: &mut Frame, app: &App, area: Rect) {
    let Some((label, chars)) = &app.request_progress else {
        return;
    };

    let spinner = spinner_frame(app.spinner_tick);
    let approx_tokens = chars / 4;
    let prefix = format!(" {} {} ~{} tok ", spinner, label, approx_tokens);
    let bar_width = area.width.saturating_sub(prefix.chars().count() as u16 + 2) as usize;

    let mut spans = vec![Span::styled(
        prefix,
        Style::default()
            .fg(Color::White)
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    )];

    if bar_width > 0 {
        const PALETTE: &[Color] = &[Color::DarkGray, Color::Gray, Color::White, Color::Cyan, Color::LightCyan];
        let segment_width = (bar_width / 4).max(3).min(bar_width);
        let travel = bar_width + segment_width;
        let offset = (app.spinner_tick / 2) as usize % travel;

        spans.push(Span::styled("[", Style::default().fg(Color::DarkGray).bg(Color::DarkGray)));
        for i in 0..bar_width {
            let active_start = offset.saturating_sub(segment_width);
            let is_active = i >= active_start && i < offset.min(bar_width + segment_width);
            let color = if is_active {
                let gradient_idx = ((i + segment_width - active_start) * PALETTE.len()) / (segment_width + 1);
                PALETTE[gradient_idx.min(PALETTE.len() - 1)]
            } else {
                Color::Black
            };
            spans.push(Span::styled("━", Style::default().fg(color).bg(Color::DarkGray)));
        }
        spans.push(Span::styled("]", Style::default().fg(Color::DarkGray).bg(Color::DarkGray)));
    }

    f.render_widget(
        Paragraph::new(Line::from(spans)).style(Style::default().bg(Color::DarkGray)),
        area,
    );
}

fn draw_request_overlay(f: &mut Frame, app: &App, size: Rect) {
    let Some((label, chars)) = &app.request_progress else {
        return;
    };
    let area = centered_rect(44, 7, size);
    f.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(area);
    f.render_widget(block, area);

    const PALETTE: &[Color] = &[
        Color::DarkGray, Color::Gray, Color::White,
        Color::Cyan, Color::LightCyan, Color::White,
        Color::Gray, Color::DarkGray,
    ];

    let tick = (app.spinner_tick / 2) as usize;
    let gradient_spans: Vec<Span> = label.chars().enumerate().map(|(i, ch)| {
        let color = PALETTE[(tick + i) % PALETTE.len()];
        Span::styled(ch.to_string(), Style::default().fg(color).add_modifier(Modifier::BOLD))
    }).collect();

    let approx_tokens = chars / 4;
    let spinner = spinner_frame(app.spinner_tick);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1), Constraint::Length(1)])
        .split(inner);

    f.render_widget(
        Paragraph::new(Line::from(gradient_spans)).alignment(ratatui::layout::Alignment::Center),
        chunks[0],
    );
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(format!("{} ", spinner), Style::default().fg(Color::Cyan)),
            Span::styled(
                format!("~{} tokens received", approx_tokens),
                Style::default().fg(Color::DarkGray),
            ),
        ])).alignment(ratatui::layout::Alignment::Center),
        chunks[2],
    );
}

// ── Help overlay ──────────────────────────────────────────────────────────────

fn draw_help(f: &mut Frame, size: Rect) {
    let area = centered_rect(60, 78, size);
    f.render_widget(Clear, area);

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Help — Aichitect ")
        .border_style(Style::default().fg(Color::Cyan));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let bold = |s: &str, c: Color| {
        Line::from(vec![Span::styled(s.to_string(), Style::default().fg(c).add_modifier(Modifier::BOLD))])
    };

    let help = vec![
        bold("Navigation", Color::Cyan),
        Line::from("  j/k ↑↓        Scroll document"),
        Line::from("  J/K           Select next/prev visible node"),
        Line::from("  Space         Collapse / expand selected heading"),
        Line::from("  Shift+←/→     Collapse/expand headings below"),
        Line::from("  Ctrl+E        Edit current block locally"),
        Line::from("  PgUp/PgDn     Page up/down"),
        Line::from("  g / G         Top / bottom"),
        Line::from("  Esc           Deselect node"),
        Line::from("  Ctrl+F        Search document"),
        Line::from(""),
        bold("Remarks", Color::Yellow),
        Line::from("  r             Add remark on selected node"),
        Line::from("  f             Find all occurrences + write remark (updates all)"),
        Line::from("  S             Send queued remarks to AI"),
        Line::from("  R             Toggle remarks panel"),
        Line::from(""),
        bold("In any input field", Color::White),
        Line::from("  ← →           Move cursor"),
        Line::from("  ↑ ↓           Move cursor up/down line"),
        Line::from("  Home / End    Start / end of line"),
        Line::from("  Alt+Enter     Insert newline"),
        Line::from("  Paste         Auto-collapsed if ≥ 3 lines"),
        Line::from(""),
        bold("Review", Color::Magenta),
        Line::from("  A             Analyze document for issues"),
        Line::from("  j/k           Navigate items"),
        Line::from("  a             Answer the issue"),
        Line::from("  d             Dismiss item"),
        Line::from("  S             Send answered items for revision"),
        Line::from(""),
        bold("Document", Color::Green),
        Line::from("  W             Save"),
        Line::from("  u / U         Undo / Redo"),
        Line::from(""),
        bold("Mouse", Color::White),
        Line::from("  Scroll        Scroll the pane under cursor"),
        Line::from("  Click         Select node in doc pane"),
        Line::from(""),
        Line::from(vec![Span::styled("Press any key to close", Style::default().fg(Color::DarkGray))]),
    ];

    f.render_widget(
        Paragraph::new(Text::from(help)).wrap(Wrap { trim: false }),
        inner,
    );
}

// ── InputBuffer → ratatui Lines ──────────────────────────────────────────────
//
// Splits text on '\n', inserts a cursor marker at the cursor position,
// and shows collapsed pastes with a distinct style.

pub fn input_spans_to_lines(spans: &[InputSpan]) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = vec![Line::from(vec![])];

    for span in spans {
        match span {
            InputSpan::Text(t) => {
                // Split on newlines; each '\n' starts a new Line.
                let mut parts = t.split('\n');
                if let Some(first) = parts.next() {
                    if !first.is_empty() {
                        lines.last_mut().unwrap().spans.push(Span::raw(first.to_string()));
                    }
                }
                for part in parts {
                    lines.push(Line::from(vec![]));
                    if !part.is_empty() {
                        lines.last_mut().unwrap().spans.push(Span::raw(part.to_string()));
                    }
                }
            }
            InputSpan::CollapsedPaste { lines: n, chars } => {
                lines.last_mut().unwrap().spans.push(Span::styled(
                    format!("[pasted text … {} lines / {} chars]", n, chars),
                    Style::default().fg(Color::DarkGray).add_modifier(Modifier::ITALIC),
                ));
            }
            InputSpan::Cursor => {
                // Show cursor as a blinking-style thin block.
                lines.last_mut().unwrap().spans.push(Span::styled(
                    "▌",
                    Style::default().fg(Color::Yellow).add_modifier(Modifier::RAPID_BLINK),
                ));
            }
        }
    }
    lines
}

// ── History browser overlay ───────────────────────────────────────────────────

fn draw_history_browser(f: &mut Frame, app: &App, size: Rect) {
    let area = centered_rect(92, 88, size);
    f.render_widget(Clear, area);

    let outer = Block::default()
        .borders(Borders::ALL)
        .title(" Revision History  (j/k navigate  Enter restore  q close) ")
        .border_style(Style::default().fg(Color::Cyan));
    let inner = outer.inner(area);
    f.render_widget(outer, area);

    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(26), Constraint::Min(10)])
        .split(inner);

    // ── Left: timestamp list ─────────────────────────────────────────────────
    let items: Vec<ListItem> = if app.history_entries.is_empty() {
        vec![ListItem::new("  No snapshots yet")]
    } else {
        app.history_entries.iter().enumerate().map(|(i, e)| {
            let style = if i == app.selected_history {
                Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            ListItem::new(format!("  {}", e.label)).style(style)
        }).collect()
    };

    let mut list_state = ListState::default();
    if !app.history_entries.is_empty() {
        list_state.select(Some(app.selected_history));
    }

    let list = List::new(items)
        .block(Block::default().borders(Borders::RIGHT).border_style(Style::default().fg(Color::DarkGray)))
        .highlight_style(Style::default().bg(Color::DarkGray));
    f.render_stateful_widget(list, chunks[0], &mut list_state);

    // ── Right: preview ────────────────────────────────────────────────────────
    let preview_lines: Vec<Line> = app.history_preview
        .lines()
        .skip(app.history_scroll)
        .take(chunks[1].height as usize)
        .map(|l| Line::from(Span::raw(l.to_string())))
        .collect();

    let preview_block = Block::default()
        .borders(Borders::NONE)
        .style(Style::default().fg(Color::White));

    let total_preview_lines = app.history_preview.lines().count();
    let visible = chunks[1].height as usize;
    let preview_widget = Paragraph::new(Text::from(preview_lines))
        .block(preview_block)
        .style(Style::default().fg(Color::Gray));
    f.render_widget(preview_widget, chunks[1]);

    // Scrollbar for the preview.
    if total_preview_lines > visible {
        let mut ss = ScrollbarState::default()
            .content_length(total_preview_lines)
            .viewport_content_length(visible)
            .position(app.history_scroll);
        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight),
            chunks[1],
            &mut ss,
        );
    }
}

// ── Layout helpers ────────────────────────────────────────────────────────────

fn centered_rect(pct_x: u16, pct_y: u16, r: Rect) -> Rect {
    let pop_v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - pct_y) / 2),
            Constraint::Percentage(pct_y),
            Constraint::Percentage((100 - pct_y) / 2),
        ])
        .split(r);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - pct_x) / 2),
            Constraint::Percentage(pct_x),
            Constraint::Percentage((100 - pct_x) / 2),
        ])
        .split(pop_v[1])[1]
}
