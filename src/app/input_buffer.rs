//! A small multiline text buffer with a grid cursor. Holds all editing logic for
//! the message composer (vim-style motions live in the input handler; this just
//! provides the primitive operations). Char-indexed throughout for correct
//! handling of multi-byte text.

#[derive(Debug, Clone)]
pub struct InputBuffer {
    pub lines: Vec<String>,
    pub row: usize,
    pub col: usize,
    /// Anchor of a visual-mode selection (row, col), or None when not selecting.
    pub visual_anchor: Option<(usize, usize)>,
    /// Line-wise visual mode (`V`): the selection always spans whole lines.
    pub visual_line: bool,
}

impl Default for InputBuffer {
    fn default() -> Self {
        Self {
            lines: vec![String::new()],
            row: 0,
            col: 0,
            visual_anchor: None,
            visual_line: false,
        }
    }
}

fn ordered_bounds(a: (usize, usize), b: (usize, usize)) -> ((usize, usize), (usize, usize)) {
    if a <= b {
        (a, b)
    } else {
        (b, a)
    }
}

impl InputBuffer {
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    pub fn take(&mut self) -> String {
        let t = self.text();
        self.lines = vec![String::new()];
        self.row = 0;
        self.col = 0;
        self.end_visual();
        t
    }

    pub fn current_line(&self) -> &str {
        self.lines.get(self.row).map(|s| s.as_str()).unwrap_or("")
    }

    /// Replace the whole buffer with `text`, placing the cursor at the end.
    pub fn set_text(&mut self, text: &str) {
        self.lines = if text.is_empty() {
            vec![String::new()]
        } else {
            text.split('\n').map(|s| s.to_string()).collect()
        };
        self.row = self.lines.len() - 1;
        self.col = self.lines[self.row].chars().count();
        self.end_visual();
    }

    // ── Visual-mode selection ────────────────────────────────────────────────

    /// Start a character-wise selection anchored at the cursor.
    pub fn begin_visual(&mut self) {
        self.visual_anchor = Some((self.row, self.col));
        self.visual_line = false;
    }

    /// Start a line-wise selection (`V`): whole lines from the anchor row.
    pub fn begin_visual_line(&mut self) {
        self.visual_anchor = Some((self.row, self.col));
        self.visual_line = true;
    }

    pub fn end_visual(&mut self) {
        self.visual_anchor = None;
        self.visual_line = false;
    }

    /// Ordered inclusive selection bounds `((r0,c0),(r1,c1))` from anchor→cursor.
    /// In line-wise mode the columns are widened to span the full first/last line.
    pub fn selection_bounds(&self) -> Option<((usize, usize), (usize, usize))> {
        let (ar, ac) = self.visual_anchor?;
        let a = (ar, ac);
        let b = (self.row, self.col);
        let (mut lo, mut hi) = if a <= b { (a, b) } else { (b, a) };
        if self.visual_line {
            lo.1 = 0;
            hi.1 = self.line_chars(hi.0).saturating_sub(1);
        }
        Some((lo, hi))
    }

    /// Whether the character cell at (row, col) is within the selection. Test-only.
    #[cfg(test)]
    pub fn is_selected(&self, row: usize, col: usize) -> bool {
        match self.selection_bounds() {
            Some((a, b)) => (row, col) >= a && (row, col) <= b,
            None => false,
        }
    }

    pub fn selection_text(&self) -> String {
        let Some(((r0, c0), (r1, c1))) = self.selection_bounds() else {
            return String::new();
        };
        if r0 == r1 {
            let chars: Vec<char> = self.lines[r0].chars().collect();
            let end = (c1 + 1).min(chars.len());
            return chars
                .get(c0..end)
                .map(|s| s.iter().collect())
                .unwrap_or_default();
        }
        let mut out = String::new();
        for r in r0..=r1 {
            let chars: Vec<char> = self.lines[r].chars().collect();
            let (s, e) = if r == r0 {
                (c0, chars.len())
            } else if r == r1 {
                (0, (c1 + 1).min(chars.len()))
            } else {
                (0, chars.len())
            };
            out.extend(chars.get(s..e).unwrap_or(&[]).iter());
            if r != r1 {
                out.push('\n');
            }
        }
        out
    }

    /// Delete the selection (inclusive), return its text, place the cursor at the
    /// start, and clear the anchor.
    pub fn delete_selection(&mut self) -> String {
        let Some(((r0, c0), (r1, c1))) = self.selection_bounds() else {
            return String::new();
        };
        let text = self.selection_text();
        self.delete_range((r0, c0), (r1, c1), true);
        self.visual_anchor = None;
        text
    }

    pub fn range_text(&self, a: (usize, usize), b: (usize, usize), inclusive: bool) -> String {
        let ((r0, c0), (r1, c1)) = ordered_bounds(a, b);
        if r0 >= self.lines.len() || r1 >= self.lines.len() {
            return String::new();
        }
        if r0 == r1 {
            let chars: Vec<char> = self.lines[r0].chars().collect();
            let end = if inclusive { c1 + 1 } else { c1 }.min(chars.len());
            return chars
                .get(c0.min(chars.len())..end)
                .map(|s| s.iter().collect())
                .unwrap_or_default();
        }
        let mut out = String::new();
        for r in r0..=r1 {
            let chars: Vec<char> = self.lines[r].chars().collect();
            let (s, e) = if r == r0 {
                (c0.min(chars.len()), chars.len())
            } else if r == r1 {
                let end = if inclusive { c1 + 1 } else { c1 }.min(chars.len());
                (0, end)
            } else {
                (0, chars.len())
            };
            out.extend(chars.get(s..e).unwrap_or(&[]).iter());
            if r != r1 {
                out.push('\n');
            }
        }
        out
    }

    pub fn delete_range(
        &mut self,
        a: (usize, usize),
        b: (usize, usize),
        inclusive: bool,
    ) -> String {
        let text = self.range_text(a, b, inclusive);
        let ((r0, c0), (r1, c1)) = ordered_bounds(a, b);
        if r0 >= self.lines.len() || r1 >= self.lines.len() || text.is_empty() {
            return text;
        }
        if r0 == r1 {
            let from = Self::byte_idx(&self.lines[r0], c0.min(self.line_chars(r0)));
            let end_col = if inclusive { c1 + 1 } else { c1 }.min(self.line_chars(r0));
            let to = Self::byte_idx(&self.lines[r0], end_col);
            self.lines[r0].replace_range(from..to, "");
        } else {
            let head = {
                let b = Self::byte_idx(&self.lines[r0], c0.min(self.line_chars(r0)));
                self.lines[r0][..b].to_string()
            };
            let tail = {
                let end_col = if inclusive { c1 + 1 } else { c1 }.min(self.line_chars(r1));
                let b = Self::byte_idx(&self.lines[r1], end_col);
                self.lines[r1][b..].to_string()
            };
            self.lines.drain(r0..=r1);
            self.lines.insert(r0, format!("{}{}", head, tail));
        }
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.row = r0.min(self.lines.len() - 1);
        self.col = c0.min(self.line_chars(self.row));
        text
    }

    pub fn change_line(&mut self) -> String {
        let text = self.yank_line();
        if let Some(line) = self.lines.get_mut(self.row) {
            line.clear();
        }
        self.col = 0;
        text
    }

    pub fn delete_to_line_end(&mut self) -> String {
        let len = self.line_chars(self.row);
        if self.col >= len {
            return String::new();
        }
        self.delete_range((self.row, self.col), (self.row, len), false)
    }

    pub fn change_to_line_end(&mut self) -> String {
        self.delete_to_line_end()
    }

    pub fn open_line_below(&mut self) {
        self.row += 1;
        self.lines.insert(self.row, String::new());
        self.col = 0;
    }

    pub fn open_line_above(&mut self) {
        self.lines.insert(self.row, String::new());
        self.col = 0;
    }

    pub fn cursor(&self) -> (usize, usize) {
        (self.row, self.col)
    }

    fn line_chars(&self, row: usize) -> usize {
        self.lines.get(row).map(|l| l.chars().count()).unwrap_or(0)
    }

    fn byte_idx(line: &str, char_idx: usize) -> usize {
        line.char_indices()
            .nth(char_idx)
            .map(|(b, _)| b)
            .unwrap_or(line.len())
    }

    pub fn insert_char(&mut self, c: char) {
        let b = Self::byte_idx(&self.lines[self.row], self.col);
        self.lines[self.row].insert(b, c);
        self.col += 1;
    }

    pub fn insert_newline(&mut self) {
        let b = Self::byte_idx(&self.lines[self.row], self.col);
        let tail = self.lines[self.row][b..].to_string();
        self.lines[self.row].truncate(b);
        self.lines.insert(self.row + 1, tail);
        self.row += 1;
        self.col = 0;
    }

    pub fn backspace(&mut self) {
        if self.col > 0 {
            let b = Self::byte_idx(&self.lines[self.row], self.col - 1);
            self.lines[self.row].remove(b);
            self.col -= 1;
        } else if self.row > 0 {
            let cur = self.lines.remove(self.row);
            self.row -= 1;
            self.col = self.line_chars(self.row);
            self.lines[self.row].push_str(&cur);
        }
    }

    /// Delete the word (and preceding whitespace) before the cursor — Ctrl-W /
    /// Ctrl-Backspace. Falls back to a plain backspace across a line boundary.
    pub fn delete_word_back(&mut self) {
        if self.col == 0 {
            self.backspace();
            return;
        }
        let start = {
            let chars: Vec<char> = self.lines[self.row].chars().collect();
            let mut c = self.col;
            while c > 0 && chars[c - 1].is_whitespace() {
                c -= 1;
            }
            while c > 0 && !chars[c - 1].is_whitespace() {
                c -= 1;
            }
            c
        };
        let from = Self::byte_idx(&self.lines[self.row], start);
        let to = Self::byte_idx(&self.lines[self.row], self.col);
        self.lines[self.row].replace_range(from..to, "");
        self.col = start;
    }

    /// Delete the word after the cursor — Ctrl-Delete.
    pub fn delete_word_forward(&mut self) {
        let end = {
            let chars: Vec<char> = self.lines[self.row].chars().collect();
            let mut c = self.col;
            while c < chars.len() && !chars[c].is_whitespace() {
                c += 1;
            }
            while c < chars.len() && chars[c].is_whitespace() {
                c += 1;
            }
            c
        };
        let from = Self::byte_idx(&self.lines[self.row], self.col);
        let to = Self::byte_idx(&self.lines[self.row], end);
        self.lines[self.row].replace_range(from..to, "");
    }

    pub fn delete_at(&mut self) {
        if self.col < self.line_chars(self.row) {
            let b = Self::byte_idx(&self.lines[self.row], self.col);
            self.lines[self.row].remove(b);
        }
    }

    pub fn delete_line(&mut self) {
        if self.lines.len() > 1 {
            self.lines.remove(self.row);
            if self.row >= self.lines.len() {
                self.row = self.lines.len() - 1;
            }
        } else {
            self.lines[0].clear();
        }
        self.col = self.col.min(self.line_chars(self.row).saturating_sub(1));
    }

    pub fn yank_line(&self) -> String {
        self.lines.get(self.row).cloned().unwrap_or_default()
    }

    pub fn paste(&mut self, text: &str) {
        for c in text.chars() {
            if c == '\n' {
                self.insert_newline();
            } else {
                self.insert_char(c);
            }
        }
    }

    pub fn left(&mut self) {
        if self.col > 0 {
            self.col -= 1;
        } else if self.row > 0 {
            self.row -= 1;
            self.col = self.line_chars(self.row);
        }
    }
    pub fn right(&mut self) {
        if self.col < self.line_chars(self.row) {
            self.col += 1;
        } else if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.col = 0;
        }
    }
    pub fn up(&mut self) {
        if self.row > 0 {
            self.row -= 1;
            self.col = self.col.min(self.line_chars(self.row));
        }
    }
    pub fn down(&mut self) {
        if self.row + 1 < self.lines.len() {
            self.row += 1;
            self.col = self.col.min(self.line_chars(self.row));
        }
    }
    pub fn line_start(&mut self) {
        self.col = 0;
    }
    pub fn first_nonblank(&mut self) {
        let chars: Vec<char> = self.lines[self.row].chars().collect();
        self.col = chars.iter().position(|c| !c.is_whitespace()).unwrap_or(0);
    }
    pub fn line_end(&mut self) {
        self.col = self.line_chars(self.row).saturating_sub(1);
    }

    pub fn word_forward(&mut self) {
        let chars: Vec<char> = self.lines[self.row].chars().collect();
        let mut c = self.col;
        while c < chars.len() && !chars[c].is_whitespace() {
            c += 1;
        }
        while c < chars.len() && chars[c].is_whitespace() {
            c += 1;
        }
        self.col = c;
    }
    pub fn word_end(&mut self) {
        let chars: Vec<char> = self.lines[self.row].chars().collect();
        if chars.is_empty() {
            self.col = 0;
            return;
        }
        let mut c = (self.col + 1).min(chars.len() - 1);
        while c < chars.len() && chars[c].is_whitespace() {
            c += 1;
        }
        if c >= chars.len() {
            self.col = chars.len() - 1;
            return;
        }
        while c + 1 < chars.len() && !chars[c + 1].is_whitespace() {
            c += 1;
        }
        self.col = c;
    }
    pub fn word_backward(&mut self) {
        let chars: Vec<char> = self.lines[self.row].chars().collect();
        if self.col == 0 {
            return;
        }
        let mut c = self.col - 1;
        while c > 0 && chars[c].is_whitespace() {
            c -= 1;
        }
        while c > 0 && !chars[c - 1].is_whitespace() {
            c -= 1;
        }
        self.col = c;
    }

    /// Clamp the cursor for normal mode (rests on a character, not past the end).
    pub fn clamp_normal(&mut self) {
        let len = self.line_chars(self.row);
        if len == 0 {
            self.col = 0;
        } else if self.col >= len {
            self.col = len - 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_and_text() {
        let mut b = InputBuffer::default();
        for c in "hi".chars() {
            b.insert_char(c);
        }
        assert_eq!(b.text(), "hi");
        assert_eq!(b.col, 2);
    }

    #[test]
    fn delete_word_back_removes_word_and_space() {
        let mut b = InputBuffer::default();
        b.set_text("hello world");
        b.delete_word_back();
        assert_eq!(b.text(), "hello ");
        assert_eq!(b.col, 6);
    }

    #[test]
    fn delete_word_forward_removes_next_word() {
        let mut b = InputBuffer::default();
        b.set_text("hello world");
        b.col = 0;
        b.delete_word_forward();
        assert_eq!(b.text(), "world");
    }

    #[test]
    fn visual_selection_yank_and_delete() {
        let mut b = InputBuffer::default();
        b.set_text("hello world");
        b.col = 0;
        b.begin_visual();
        b.col = 4; // select "hello" (0..=4 inclusive)
        assert_eq!(b.selection_text(), "hello");
        assert!(b.is_selected(0, 0));
        assert!(b.is_selected(0, 4));
        assert!(!b.is_selected(0, 5));
        let removed = b.delete_selection();
        assert_eq!(removed, "hello");
        assert_eq!(b.text(), " world");
        assert!(b.visual_anchor.is_none());
    }

    #[test]
    fn visual_selection_spans_lines() {
        let mut b = InputBuffer::default();
        b.set_text("ab\ncd");
        b.row = 0;
        b.col = 1;
        b.begin_visual();
        b.row = 1;
        b.col = 0; // select "b\nc"
        assert_eq!(b.selection_text(), "b\nc");
        b.delete_selection();
        assert_eq!(b.text(), "ad");
    }

    #[test]
    fn newline_splits() {
        let mut b = InputBuffer::default();
        b.paste("abcd");
        b.col = 2;
        b.insert_newline();
        assert_eq!(b.lines, vec!["ab".to_string(), "cd".to_string()]);
        assert_eq!((b.row, b.col), (1, 0));
    }

    #[test]
    fn backspace_joins_lines() {
        let mut b = InputBuffer::default();
        b.paste("ab\ncd");
        b.row = 1;
        b.col = 0;
        b.backspace();
        assert_eq!(b.text(), "abcd");
        assert_eq!((b.row, b.col), (0, 2));
    }

    #[test]
    fn take_resets() {
        let mut b = InputBuffer::default();
        b.paste("hello");
        assert_eq!(b.take(), "hello");
        assert_eq!(b.text(), "");
        assert_eq!((b.row, b.col), (0, 0));
    }

    #[test]
    fn unicode_columns() {
        let mut b = InputBuffer::default();
        b.paste("héllo");
        b.col = 5;
        b.backspace();
        assert_eq!(b.text(), "héll");
    }

    #[test]
    fn word_motions() {
        let mut b = InputBuffer::default();
        b.paste("foo bar baz");
        b.col = 0;
        b.word_forward();
        assert_eq!(b.col, 4);
        b.word_forward();
        assert_eq!(b.col, 8);
        b.word_backward();
        assert_eq!(b.col, 4);
        b.word_end();
        assert_eq!(b.col, 6);
    }

    #[test]
    fn delete_range_and_open_lines() {
        let mut b = InputBuffer::default();
        b.set_text("hello world");
        let removed = b.delete_range((0, 0), (0, 4), true);
        assert_eq!(removed, "hello");
        assert_eq!(b.text(), " world");
        b.open_line_below();
        assert_eq!(b.text(), " world\n");
        assert_eq!((b.row, b.col), (1, 0));
        b.open_line_above();
        assert_eq!(b.text(), " world\n\n");
        assert_eq!((b.row, b.col), (1, 0));
    }
}
