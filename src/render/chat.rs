//! The chat view: owns the cursor/scroll/collapse state and a cache of rendered
//! rows. The document is rebuilt only when the content revision or width
//! changes — never on a navigation keystroke — so moving the cursor is just
//! integer indexing into the cached rows. Only the visible slice is drawn
//! (virtualization), so huge conversations stay fast.

use std::collections::HashSet;

use ratatui::layout::{Margin, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};
use ratatui::Frame;

use crate::render::document::RenderedLine;
use crate::render::theme::Theme;

#[derive(Default)]
pub struct ChatState {
    pub cursor: usize,
    pub col: usize,
    pub scroll: usize,
    /// (msg, block) keys the user has flipped from default collapse state.
    pub toggled: HashSet<(usize, usize)>,
    /// Follow the tail of the conversation as new content streams in.
    pub stick_bottom: bool,

    // ── cache ───────────────────────────────────────────────────────────────
    doc: Vec<RenderedLine>,
    cache_rev: u64,
    cache_width: usize,
    cache_valid: bool,
}

impl ChatState {
    pub fn new() -> Self {
        Self { stick_bottom: true, cache_rev: u64::MAX, ..Default::default() }
    }

    pub fn needs_rebuild(&self, rev: u64, width: usize) -> bool {
        !self.cache_valid || rev != self.cache_rev || width != self.cache_width
    }

    /// Install a freshly built document and refresh derived state.
    pub fn set_doc(&mut self, doc: Vec<RenderedLine>, rev: u64, width: usize, viewport_h: usize) {
        self.doc = doc;
        self.cache_rev = rev;
        self.cache_width = width;
        self.cache_valid = true;

        let total = self.doc.len();
        if self.stick_bottom {
            self.cursor = total.saturating_sub(1);
            self.scroll = total.saturating_sub(viewport_h);
        }
        self.clamp(viewport_h);
    }

    pub fn doc(&self) -> &[RenderedLine] {
        &self.doc
    }

    fn line_len(&self, row: usize) -> usize {
        self.doc.get(row).map(|r| r.plain.chars().count()).unwrap_or(0)
    }

    fn clamp(&mut self, viewport_h: usize) {
        let total = self.doc.len();
        if total == 0 {
            self.cursor = 0;
            self.col = 0;
            self.scroll = 0;
            return;
        }
        if self.cursor >= total {
            self.cursor = total - 1;
        }
        let len = self.line_len(self.cursor);
        if len == 0 {
            self.col = 0;
        } else if self.col >= len {
            self.col = len - 1;
        }
        let max_scroll = total.saturating_sub(viewport_h);
        if self.scroll > max_scroll {
            self.scroll = max_scroll;
        }
    }

    /// Keep the cursor inside the viewport and update the follow flag.
    fn follow_cursor(&mut self, viewport_h: usize) {
        if viewport_h == 0 {
            return;
        }
        if self.cursor < self.scroll {
            self.scroll = self.cursor;
        } else if self.cursor >= self.scroll + viewport_h {
            self.scroll = self.cursor + 1 - viewport_h;
        }
        let total = self.doc.len();
        let max_scroll = total.saturating_sub(viewport_h);
        self.stick_bottom = self.cursor + 1 >= total && self.scroll >= max_scroll;
    }

    // ── Navigation (operates purely on the cached rows) ──────────────────────

    pub fn down(&mut self, h: usize) {
        if self.cursor + 1 < self.doc.len() {
            self.cursor += 1;
            self.clamp(h);
        }
        self.follow_cursor(h);
    }
    pub fn up(&mut self, h: usize) {
        if self.cursor > 0 {
            self.cursor -= 1;
            self.clamp(h);
        }
        self.follow_cursor(h);
    }
    pub fn left(&mut self, h: usize) {
        if self.col > 0 {
            self.col -= 1;
        } else if self.cursor > 0 {
            self.cursor -= 1;
            self.col = self.line_len(self.cursor).saturating_sub(1);
        }
        self.follow_cursor(h);
    }
    pub fn right(&mut self, h: usize) {
        let len = self.line_len(self.cursor);
        if self.col + 1 < len {
            self.col += 1;
        } else if self.cursor + 1 < self.doc.len() {
            self.cursor += 1;
            self.col = 0;
        }
        self.follow_cursor(h);
    }
    pub fn line_start(&mut self) {
        self.col = 0;
    }
    pub fn line_end(&mut self) {
        self.col = self.line_len(self.cursor).saturating_sub(1);
    }
    pub fn top(&mut self, h: usize) {
        self.cursor = 0;
        self.col = 0;
        self.follow_cursor(h);
    }
    pub fn bottom(&mut self, h: usize) {
        self.cursor = self.doc.len().saturating_sub(1);
        self.clamp(h);
        self.follow_cursor(h);
    }
    pub fn page_down(&mut self, h: usize) {
        let step = (h / 2).max(1);
        self.cursor = (self.cursor + step).min(self.doc.len().saturating_sub(1));
        self.clamp(h);
        self.follow_cursor(h);
    }
    pub fn page_up(&mut self, h: usize) {
        let step = (h / 2).max(1);
        self.cursor = self.cursor.saturating_sub(step);
        self.clamp(h);
        self.follow_cursor(h);
    }
    pub fn word_forward(&mut self, h: usize) {
        let line = self.doc.get(self.cursor).map(|r| r.plain.clone()).unwrap_or_default();
        let chars: Vec<char> = line.chars().collect();
        let mut c = self.col;
        while c < chars.len() && !chars[c].is_whitespace() {
            c += 1;
        }
        while c < chars.len() && chars[c].is_whitespace() {
            c += 1;
        }
        if c >= chars.len() && self.cursor + 1 < self.doc.len() {
            self.cursor += 1;
            self.col = 0;
        } else {
            self.col = c.min(chars.len().saturating_sub(1));
        }
        self.follow_cursor(h);
    }
    pub fn word_backward(&mut self, h: usize) {
        let line = self.doc.get(self.cursor).map(|r| r.plain.clone()).unwrap_or_default();
        let chars: Vec<char> = line.chars().collect();
        if self.col == 0 {
            if self.cursor > 0 {
                self.cursor -= 1;
                self.col = self.line_len(self.cursor).saturating_sub(1);
            }
            self.follow_cursor(h);
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
        self.follow_cursor(h);
    }

    /// Wheel / explicit scroll without moving the cursor.
    pub fn scroll_by(&mut self, delta: i32, h: usize) {
        let total = self.doc.len();
        let max_scroll = total.saturating_sub(h);
        if delta < 0 {
            self.scroll = (self.scroll + (-delta) as usize).min(max_scroll);
        } else {
            self.scroll = self.scroll.saturating_sub(delta as usize);
        }
        self.stick_bottom = self.scroll >= max_scroll;
    }

    /// Flip the collapsible region under the cursor, if any. Returns true when a
    /// toggle happened (caller must bump the content revision to force rebuild).
    pub fn toggle_current(&mut self) -> bool {
        if let Some(key) = self.doc.get(self.cursor).and_then(|r| r.toggle) {
            if !self.toggled.remove(&key) {
                self.toggled.insert(key);
            }
            true
        } else {
            false
        }
    }

    /// The toggle key under the cursor (for click handling).
    pub fn cursor_msg(&self) -> Option<usize> {
        self.doc.get(self.cursor).map(|r| r.msg)
    }

    pub fn yank_line(&self) -> Option<String> {
        self.doc.get(self.cursor).map(|r| r.plain.clone())
    }
}

/// Draw the chat panel.
pub fn render(f: &mut Frame, area: Rect, state: &mut ChatState, focused: bool, theme: &Theme, title: &str) {
    let block = Block::default()
        .title(Span::styled(title.to_string(), theme.title()))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(theme.border_style(focused));
    let inner = block.inner(area);
    f.render_widget(block, area);

    let h = inner.height as usize;
    let total = state.doc.len();
    let max_scroll = total.saturating_sub(h);
    if state.scroll > max_scroll {
        state.scroll = max_scroll;
    }
    let start = state.scroll;
    let end = (start + h).min(total);

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(end.saturating_sub(start));
    for (i, row) in state.doc[start..end].iter().enumerate() {
        let abs = start + i;
        if focused && abs == state.cursor {
            lines.push(apply_cursor(row.line.clone(), state.col, theme.cursor()));
        } else {
            lines.push(row.line.clone());
        }
    }

    f.render_widget(Paragraph::new(lines), inner);

    // Scrollbar when content overflows.
    if total > h {
        let mut sb = ScrollbarState::new(total).viewport_content_length(h).position(state.scroll);
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(Some("▲"))
            .end_symbol(Some("▼"))
            .thumb_symbol("█")
            .track_symbol(Some("│"))
            .style(Style::default().fg(theme.border))
            .thumb_style(Style::default().fg(if focused { theme.border_focus } else { theme.muted }));
        f.render_stateful_widget(scrollbar, area.inner(Margin { vertical: 1, horizontal: 0 }), &mut sb);
    }
}

/// Split the span containing `col` so that one character carries the cursor style.
fn apply_cursor(line: Line<'static>, col: usize, cursor: Style) -> Line<'static> {
    let mut out: Vec<Span<'static>> = Vec::new();
    let mut idx = 0usize;
    let mut placed = false;
    for span in line.spans {
        let chars: Vec<char> = span.content.chars().collect();
        let len = chars.len();
        if !placed && col >= idx && col < idx + len {
            let local = col - idx;
            let before: String = chars[..local].iter().collect();
            let cur: String = chars[local..local + 1].iter().collect();
            let after: String = chars[local + 1..].iter().collect();
            if !before.is_empty() {
                out.push(Span::styled(before, span.style));
            }
            out.push(Span::styled(cur, cursor.add_modifier(Modifier::BOLD)));
            if !after.is_empty() {
                out.push(Span::styled(after, span.style));
            }
            placed = true;
        } else {
            out.push(span);
        }
        idx += len;
    }
    if !placed {
        out.push(Span::styled(" ".to_string(), cursor));
    }
    Line::from(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::document::{build, DocMessage};
    use crate::domain::blocks::Block;

    fn sample_state(rows: usize) -> ChatState {
        let body = (0..rows).map(|i| format!("line {}", i)).collect::<Vec<_>>().join("\n");
        let msgs = vec![DocMessage { role: "assistant".into(), blocks: vec![Block::Markdown(body)] }];
        let doc = build(&msgs, 40, &Theme::default(), &HashSet::new());
        let mut s = ChatState::new();
        s.stick_bottom = false;
        s.set_doc(doc, 1, 40, 10);
        s
    }

    #[test]
    fn navigation_moves_cursor_within_bounds() {
        let mut s = sample_state(20);
        s.cursor = 0;
        s.down(10);
        assert_eq!(s.cursor, 1);
        s.up(10);
        assert_eq!(s.cursor, 0);
        s.up(10); // already at top
        assert_eq!(s.cursor, 0);
    }

    #[test]
    fn bottom_and_top_jump() {
        let mut s = sample_state(20);
        s.top(10);
        assert_eq!(s.cursor, 0);
        s.bottom(10);
        assert_eq!(s.cursor, s.doc.len() - 1);
    }

    #[test]
    fn scroll_follows_cursor_down() {
        let mut s = sample_state(30);
        s.cursor = 0;
        s.scroll = 0;
        for _ in 0..15 {
            s.down(10);
        }
        // cursor must stay visible: scroll <= cursor < scroll + 10
        assert!(s.cursor >= s.scroll && s.cursor < s.scroll + 10);
    }

    #[test]
    fn rebuild_only_on_rev_or_width_change() {
        let mut s = sample_state(5);
        assert!(!s.needs_rebuild(1, 40));
        assert!(s.needs_rebuild(2, 40));
        assert!(s.needs_rebuild(1, 50));
    }
}
