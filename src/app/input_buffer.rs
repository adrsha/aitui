//! A small multiline text buffer with a grid cursor. Holds all editing logic for
//! the message composer (vim-style motions live in the input handler; this just
//! provides the primitive operations). Char-indexed throughout for correct
//! handling of multi-byte text.

#[derive(Debug, Clone)]
pub struct InputBuffer {
    pub lines: Vec<String>,
    pub row: usize,
    pub col: usize,
}

impl Default for InputBuffer {
    fn default() -> Self {
        Self { lines: vec![String::new()], row: 0, col: 0 }
    }
}

impl InputBuffer {
    pub fn is_empty(&self) -> bool {
        self.lines.len() == 1 && self.lines[0].is_empty()
    }

    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    pub fn take(&mut self) -> String {
        let t = self.text();
        self.lines = vec![String::new()];
        self.row = 0;
        self.col = 0;
        t
    }

    pub fn current_line(&self) -> &str {
        self.lines.get(self.row).map(|s| s.as_str()).unwrap_or("")
    }

    fn line_chars(&self, row: usize) -> usize {
        self.lines.get(row).map(|l| l.chars().count()).unwrap_or(0)
    }

    fn byte_idx(line: &str, char_idx: usize) -> usize {
        line.char_indices().nth(char_idx).map(|(b, _)| b).unwrap_or(line.len())
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

    pub fn delete_word_forward(&mut self) {
        let chars: Vec<char> = self.lines[self.row].chars().collect();
        let mut end = self.col;
        while end < chars.len() && !chars[end].is_whitespace() {
            end += 1;
        }
        while end < chars.len() && chars[end].is_whitespace() {
            end += 1;
        }
        let new: String = chars[..self.col].iter().chain(chars[end..].iter()).collect();
        self.lines[self.row] = new;
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
    pub fn line_end(&mut self) {
        self.col = self.line_chars(self.row).saturating_sub(1);
    }
    pub fn line_end_insert(&mut self) {
        self.col = self.line_chars(self.row);
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
        assert!(b.is_empty());
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
    }
}
