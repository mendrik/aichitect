/// Threshold: pastes with at least this many lines are collapsed in the display.
const COLLAPSE_LINES: usize = 3;

#[derive(Debug, Clone)]
pub struct PasteRegion {
    pub start_char: usize, // inclusive
    pub end_char: usize,   // exclusive
    pub line_count: usize,
    pub char_count: usize,
}

/// An owned text input buffer with cursor, paste-region tracking, and multi-line support.
#[derive(Debug, Clone, Default)]
pub struct InputBuffer {
    text: String,
    /// Cursor position as a char index (0 ..= text.chars().count()).
    cursor: usize,
    paste_regions: Vec<PasteRegion>,
}

/// Segments emitted by [`InputBuffer::render_spans`] for TUI rendering.
#[derive(Debug, Clone)]
pub enum InputSpan {
    Text(String),
    CollapsedPaste { lines: usize, chars: usize },
    /// Marks the cursor insertion point (between chars, not over a char).
    Cursor,
}

impl InputBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    #[allow(dead_code)]
    pub fn cursor_char(&self) -> usize {
        self.cursor
    }

    #[allow(dead_code)]
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn clear(&mut self) {
        self.text.clear();
        self.cursor = 0;
        self.paste_regions.clear();
    }

    // ── Editing ───────────────────────────────────────────────────────────────

    pub fn insert_char(&mut self, c: char) {
        let byte = self.char_to_byte(self.cursor);
        self.text.insert(byte, c);
        self.shift_right(self.cursor, 1);
        self.cursor += 1;
    }

    pub fn insert_newline(&mut self) {
        self.insert_char('\n');
    }

    /// Backspace: if cursor is right after a paste region, delete the whole
    /// region; otherwise delete the single preceding character.
    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        if let Some(idx) = self.paste_regions.iter().position(|r| r.end_char == self.cursor) {
            let r = self.paste_regions.remove(idx);
            let start_byte = self.char_to_byte(r.start_char);
            let end_byte = self.char_to_byte(r.end_char);
            self.text.drain(start_byte..end_byte);
            let len = r.end_char - r.start_char;
            self.shift_left(r.start_char, len);
            self.cursor = r.start_char;
        } else {
            let new_cursor = self.cursor - 1;
            let byte = self.char_to_byte(new_cursor);
            self.text.remove(byte);
            self.shift_left(new_cursor, 1);
            self.cursor = new_cursor;
        }
    }

    /// Insert a pasted string at the cursor. Multi-line pastes (≥ COLLAPSE_LINES)
    /// are stored as a paste region and shown collapsed in the display.
    pub fn paste(&mut self, s: String) {
        if s.is_empty() {
            return;
        }
        let line_count = s.lines().count();
        let char_count = s.chars().count();
        let start_char = self.cursor;
        let byte = self.char_to_byte(start_char);
        self.text.insert_str(byte, &s);
        self.shift_right(start_char, char_count);
        if line_count >= COLLAPSE_LINES {
            self.paste_regions.push(PasteRegion {
                start_char,
                end_char: start_char + char_count,
                line_count,
                char_count,
            });
            self.paste_regions.sort_by_key(|r| r.start_char);
        }
        self.cursor = start_char + char_count;
    }

    // ── Cursor navigation ────────────────────────────────────────────────────

    pub fn move_left(&mut self) {
        if self.cursor == 0 {
            return;
        }
        // If we are at the end of a paste region, jump to its start.
        if let Some(r) = self.paste_regions.iter().find(|r| r.end_char == self.cursor) {
            self.cursor = r.start_char;
        } else {
            self.cursor -= 1;
        }
    }

    pub fn move_right(&mut self) {
        let len = self.text.chars().count();
        if self.cursor >= len {
            return;
        }
        // If we are at the start of a paste region, jump past it.
        if let Some(r) = self.paste_regions.iter().find(|r| r.start_char == self.cursor) {
            self.cursor = r.end_char;
        } else {
            self.cursor += 1;
        }
    }

    pub fn move_home(&mut self) {
        let chars: Vec<char> = self.text.chars().collect();
        let mut pos = self.cursor;
        while pos > 0 && chars.get(pos - 1).copied() != Some('\n') {
            pos -= 1;
        }
        self.cursor = pos;
    }

    pub fn move_end(&mut self) {
        let chars: Vec<char> = self.text.chars().collect();
        let mut pos = self.cursor;
        while pos < chars.len() && chars.get(pos).copied() != Some('\n') {
            pos += 1;
        }
        self.cursor = pos;
    }

    pub fn move_up(&mut self) {
        // Move to the same column on the previous line.
        let chars: Vec<char> = self.text.chars().collect();
        let col = self.col_on_current_line(&chars);
        // find start of current line
        let mut pos = self.cursor;
        while pos > 0 && chars[pos - 1] != '\n' {
            pos -= 1;
        }
        if pos == 0 {
            self.cursor = 0;
            return;
        }
        // pos is now the first char of the current line; pos-1 is '\n'
        let prev_line_end = pos - 1;
        // find start of previous line
        let mut prev_start = prev_line_end;
        while prev_start > 0 && chars[prev_start - 1] != '\n' {
            prev_start -= 1;
        }
        let prev_line_len = prev_line_end - prev_start;
        self.cursor = prev_start + col.min(prev_line_len);
    }

    pub fn move_down(&mut self) {
        let chars: Vec<char> = self.text.chars().collect();
        let col = self.col_on_current_line(&chars);
        // find end of current line
        let mut pos = self.cursor;
        while pos < chars.len() && chars[pos] != '\n' {
            pos += 1;
        }
        if pos >= chars.len() {
            return; // already on last line
        }
        // pos is '\n'; next line starts at pos+1
        let next_start = pos + 1;
        let mut next_end = next_start;
        while next_end < chars.len() && chars[next_end] != '\n' {
            next_end += 1;
        }
        let next_line_len = next_end - next_start;
        self.cursor = next_start + col.min(next_line_len);
    }

    // ── Rendering ────────────────────────────────────────────────────────────

    /// Returns a sequence of [`InputSpan`] for rendering. A [`InputSpan::Cursor`]
    /// is inserted at the cursor position. Paste regions are emitted as a single
    /// [`InputSpan::CollapsedPaste`] span.
    pub fn render_spans(&self) -> Vec<InputSpan> {
        let chars: Vec<char> = self.text.chars().collect();
        let total = chars.len();
        let mut out: Vec<InputSpan> = Vec::new();
        let mut pos = 0usize;
        let mut buf = String::new();

        while pos <= total {
            if pos == self.cursor {
                if !buf.is_empty() {
                    out.push(InputSpan::Text(std::mem::take(&mut buf)));
                }
                out.push(InputSpan::Cursor);
            }
            if pos >= total {
                break;
            }
            if let Some(r) = self.paste_regions.iter().find(|r| r.start_char == pos) {
                if !buf.is_empty() {
                    out.push(InputSpan::Text(std::mem::take(&mut buf)));
                }
                out.push(InputSpan::CollapsedPaste { lines: r.line_count, chars: r.char_count });
                pos = r.end_char;
                continue;
            }
            if self.paste_regions.iter().any(|r| pos > r.start_char && pos < r.end_char) {
                pos += 1;
                continue;
            }
            buf.push(chars[pos]);
            pos += 1;
        }
        if !buf.is_empty() {
            out.push(InputSpan::Text(buf));
        }
        out
    }

    // ── Private helpers ───────────────────────────────────────────────────────

    fn char_to_byte(&self, char_idx: usize) -> usize {
        self.text
            .char_indices()
            .nth(char_idx)
            .map(|(b, _)| b)
            .unwrap_or(self.text.len())
    }

    /// Shift all paste regions that start at or after `from` rightward by `delta`.
    fn shift_right(&mut self, from: usize, delta: usize) {
        for r in &mut self.paste_regions {
            if r.start_char >= from {
                r.start_char += delta;
                r.end_char += delta;
            }
        }
    }

    /// Shift all paste regions that start at or after `from` leftward by `delta`.
    /// Regions that fall entirely within the removed range are dropped.
    fn shift_left(&mut self, from: usize, delta: usize) {
        self.paste_regions.retain_mut(|r| {
            if r.end_char <= from {
                true
            } else if r.start_char >= from + delta {
                r.start_char -= delta;
                r.end_char -= delta;
                true
            } else {
                false // region overlaps the removed range – drop it
            }
        });
    }

    fn col_on_current_line(&self, chars: &[char]) -> usize {
        let mut pos = self.cursor;
        while pos > 0 && chars[pos - 1] != '\n' {
            pos -= 1;
        }
        self.cursor - pos
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_insert_and_cursor() {
        let mut b = InputBuffer::new();
        b.insert_char('h');
        b.insert_char('i');
        assert_eq!(b.text(), "hi");
        assert_eq!(b.cursor_char(), 2);
        b.move_left();
        assert_eq!(b.cursor_char(), 1);
        b.insert_char('!');
        assert_eq!(b.text(), "h!i");
    }

    #[test]
    fn test_paste_collapse() {
        let mut b = InputBuffer::new();
        let big = "line1\nline2\nline3\nline4".to_string();
        b.paste(big.clone());
        let spans = b.render_spans();
        assert!(spans.iter().any(|s| matches!(s, InputSpan::CollapsedPaste { .. })));
        // Full text is still intact.
        assert_eq!(b.text(), big);
    }

    #[test]
    fn test_paste_arrow_skips_region() {
        let mut b = InputBuffer::new();
        b.paste("a\nb\nc\nd".to_string());
        let end = b.cursor_char();
        b.move_left();
        assert_eq!(b.cursor_char(), 0);
        b.move_right();
        assert_eq!(b.cursor_char(), end);
    }

    #[test]
    fn test_backspace_deletes_paste_region() {
        let mut b = InputBuffer::new();
        b.paste("x\ny\nz\nw".to_string());
        let before = b.text().len();
        b.backspace();
        assert_eq!(b.text().len(), 0);
        assert!(before > 0);
    }
}
