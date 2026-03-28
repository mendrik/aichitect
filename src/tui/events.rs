use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind, MouseButton};
use super::app::{App, AppMode};
use super::input::InputBuffer;

pub async fn handle_key(app: &mut App, key: KeyEvent) {
    if matches!(key.code, KeyCode::Char('d')) && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.should_quit = true;
        return;
    }

    if matches!(key.code, KeyCode::Char('c')) && key.modifiers.contains(KeyModifiers::CONTROL) {
        app.copy_current_selection();
        return;
    }

    match app.mode {
        AppMode::Normal => handle_normal(app, key).await,
        AppMode::Search => handle_search_mode(app, key),
        AppMode::CreationPrompt => handle_creation_prompt(app, key).await,
        AppMode::DirectEdit => handle_input_mode(app, key, InputTarget::DirectEdit).await,
        AppMode::RemarkEdit => handle_input_mode(app, key, InputTarget::Remark).await,
        AppMode::ReviewMode => handle_review_mode(app, key).await,
        AppMode::ReviewAnswer => handle_input_mode(app, key, InputTarget::ReviewAnswer).await,
        AppMode::HistoryBrowser => handle_history_browser(app, key),
        AppMode::Help => { app.mode = AppMode::Normal; }
    }
}

pub fn handle_mouse(app: &mut App, mouse: MouseEvent) {
    match mouse.kind {
        MouseEventKind::ScrollUp => {
            if in_rect(mouse.column, mouse.row, app.last_side_area) {
                app.side_scroll_up();
            } else if in_rect(mouse.column, mouse.row, Some(app.last_doc_area)) {
                app.scroll_up();
            }
        }
        MouseEventKind::ScrollDown => {
            if in_rect(mouse.column, mouse.row, app.last_side_area) {
                app.side_scroll_down();
            } else if in_rect(mouse.column, mouse.row, Some(app.last_doc_area)) {
                app.scroll_down();
            }
        }
        MouseEventKind::Down(MouseButton::Left) => {
            // Click in doc area: select the node at that row.
            if in_rect(mouse.column, mouse.row, Some(app.last_doc_area)) {
                let area = app.last_doc_area;
                let inner_row = mouse.row.saturating_sub(area.y + 1) as usize;
                let line_idx = app.scroll_offset + inner_row;
                if let Some(line) = app.display_lines.get(line_idx) {
                    if let Some(ni) = line.node_index {
                        app.selected_node = Some(ni);
                    }
                }
            }
        }
        _ => {}
    }
}

pub fn handle_paste(app: &mut App, text: String) {
    match app.mode {
        AppMode::Search | AppMode::RemarkEdit | AppMode::ReviewAnswer | AppMode::CreationPrompt => {
            app.input.paste(text);
            if matches!(app.mode, AppMode::Search) {
                app.update_search();
            }
        }
        _ => {}
    }
}

// ── Normal mode ──────────────────────────────────────────────────────────────

async fn handle_normal(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Char('q') | KeyCode::Char('Q') => app.should_quit = true,
        KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::CONTROL) => app.start_search(),
        KeyCode::Char('?') => app.mode = AppMode::Help,
        KeyCode::Up => { app.clear_occurrences(); app.select_prev_node(); }
        KeyCode::Down => { app.clear_occurrences(); app.select_next_node(); }
        KeyCode::Left if key.modifiers.contains(KeyModifiers::SHIFT) => app.collapse_headings_below(),
        KeyCode::Left => {
            if app.is_on_table() { app.table_prev_col(); } else { app.collapse_heading(); }
        }
        KeyCode::Right if key.modifiers.contains(KeyModifiers::SHIFT) => app.expand_headings_below(),
        KeyCode::Right => {
            if app.is_on_table() { app.table_next_col(); } else { app.expand_heading(); }
        }
        KeyCode::PageUp => app.page_up(),
        KeyCode::PageDown => app.page_down(),
        KeyCode::Home => app.scroll_offset = 0,
        KeyCode::End => {
            app.scroll_offset = app.display_lines.len().saturating_sub(1);
        }
        KeyCode::Enter => app.activate_link(),
        KeyCode::Char('c') => app.toggle_collapse_all(),
        KeyCode::Char('e') => app.start_direct_edit(),
        KeyCode::Char('r') => app.find_and_show_occurrences(),
        KeyCode::Char('R') => {
            app.show_remarks_panel = !app.show_remarks_panel;
        }
        KeyCode::Char('A') => app.open_review_panel().await,
        KeyCode::Char('H') => app.open_history(),
        KeyCode::Char('w') | KeyCode::Char('W') => app.save_doc(),
        KeyCode::Char('u') => app.undo(),
        KeyCode::Char('U') => app.redo(),
        KeyCode::Esc => {
            app.clear_occurrences();
            app.selected_node = None;
            app.selected_table_col = None;
            app.status_message = Some("Press ? for help".to_string());
        }
        _ => {}
    }
}

fn handle_search_mode(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Esc => app.cancel_search(),
        KeyCode::Enter => {
            let forward = !key.modifiers.contains(KeyModifiers::SHIFT);
            app.advance_search(forward);
        }
        _ => {
            if handle_common_input_key(&mut app.input, key) {
                app.update_search();
            }
        }
    }
}

// ── Creation-prompt mode ─────────────────────────────────────────────────────

async fn handle_creation_prompt(app: &mut App, key: KeyEvent) {
    if handle_common_input_key(&mut app.input, key) {
        return;
    }
    match key.code {
        KeyCode::Enter => {
            // Alt+Enter already handled by handle_common_input_key (newline).
            app.submit_creation_prompt().await;
        }
        KeyCode::Esc => {
            app.should_quit = true;
        }
        _ => {}
    }
}

// ── Remark / review-answer input mode ────────────────────────────────────────

enum InputTarget {
    DirectEdit,
    Remark,
    ReviewAnswer,
}

async fn handle_input_mode(app: &mut App, key: KeyEvent, target: InputTarget) {
    if handle_common_input_key(&mut app.input, key) {
        return;
    }
    match key.code {
        KeyCode::Enter => match target {
            InputTarget::DirectEdit => app.submit_direct_edit(),
            InputTarget::Remark => app.submit_remark().await,
            InputTarget::ReviewAnswer => app.submit_review_answer().await,
        },
        KeyCode::Esc => {
            app.clear_occurrences();
            app.cancel_input();
        }
        _ => {}
    }
}

// ── Review-mode navigation ────────────────────────────────────────────────────

async fn handle_review_mode(app: &mut App, key: KeyEvent) {
    let pending_len = app.review_store.pending().len();
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => {
            app.mode = AppMode::Normal;
            app.status_message = Some("Exited review mode.".to_string());
        }
        KeyCode::Char('A') => {
            if app.review_store.is_empty() {
                app.open_review_panel().await;
            }
        }
        KeyCode::Down => {
            if pending_len > 0 {
                app.selected_review = Some(
                    app.selected_review
                        .map(|i| (i + 1).min(pending_len - 1))
                        .unwrap_or(0),
                );
            }
        }
        KeyCode::Up => {
            if pending_len > 0 {
                app.selected_review = Some(
                    app.selected_review
                        .map(|i| i.saturating_sub(1))
                        .unwrap_or(0),
                );
            }
        }
        KeyCode::Char('a') => app.start_review_answer(),
        KeyCode::Char('y') => app.accept_resolution().await,
        KeyCode::Char('d') => app.dismiss_review(),
        KeyCode::Char('x') => app.clear_review_results(),
        _ => {}
    }
}

// ── Shared single-line / multi-line input key handler ────────────────────────
//
// Returns `true` if the key was consumed (so the caller skips its own match).

fn handle_common_input_key(buf: &mut InputBuffer, key: KeyEvent) -> bool {
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let control = key.modifiers.contains(KeyModifiers::CONTROL);

    match key.code {
        // Alt+Enter → literal newline (multi-line input).
        KeyCode::Enter if alt => {
            buf.insert_newline();
            true
        }
        KeyCode::Char('h') if control => {
            buf.backspace();
            true
        }
        KeyCode::Char(c) => {
            if control {
                return false;
            }
            buf.insert_char(c);
            true
        }
        KeyCode::Backspace => {
            buf.backspace();
            true
        }
        KeyCode::Left => {
            buf.move_left();
            true
        }
        KeyCode::Right => {
            buf.move_right();
            true
        }
        KeyCode::Up => {
            buf.move_up();
            true
        }
        KeyCode::Down => {
            buf.move_down();
            true
        }
        KeyCode::Home => {
            buf.move_home();
            true
        }
        KeyCode::End => {
            buf.move_end();
            true
        }
        _ => false,
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn in_rect(col: u16, row: u16, rect: Option<ratatui::layout::Rect>) -> bool {
    if let Some(r) = rect {
        col >= r.x && col < r.x + r.width && row >= r.y && row < r.y + r.height
    } else {
        false
    }
}

// ── History browser ───────────────────────────────────────────────────────────

fn handle_history_browser(app: &mut App, key: KeyEvent) {
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => {
            app.mode = AppMode::Normal;
            app.status_message = Some("History closed.".to_string());
        }
        KeyCode::Down => app.history_next(),
        KeyCode::Up => app.history_prev(),
        KeyCode::PageDown => {
            app.history_scroll = app.history_scroll.saturating_add(10);
        }
        KeyCode::PageUp => {
            app.history_scroll = app.history_scroll.saturating_sub(10);
        }
        KeyCode::Enter => app.restore_history(),
        _ => {}
    }
}
