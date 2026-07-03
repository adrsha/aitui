//! The chat view state: a scroll offset plus a cache of pre-wrapped rows. The
//! document is rebuilt only when the content revision or width changes — never
//! on a scroll — so scrolling is just integer arithmetic and only the visible
//! slice is drawn (virtualization), keeping huge conversations fast.
//!
//! There is intentionally no cursor or vim navigation here: to read or search a
//! conversation with motions, open it in `$EDITOR` (Ctrl-O). The transcript pane
//! only scrolls.

use std::collections::HashSet;

use crate::render::document::RenderedLine;

/// Per-message render cache. Building the chat document re-parses markdown, runs
/// tree-sitter highlighting, and word-wraps every block — expensive to redo for
/// the *whole* transcript on every streamed token. This caches each finalized
/// message's rendered rows keyed by a content signature, so only messages that
/// actually changed (in practice just the streaming one) are rebuilt.
///
/// Owned by the renderer (`ChatState`) as pure layout scaffolding — it holds no
/// domain state, so "rendering is pure" still stands.
#[derive(Default)]
pub struct DocCache {
    /// One slot per stored message, indexed by message position.
    entries: Vec<Option<CacheEntry>>,
    width: usize,
    show_output: bool,
}

struct CacheEntry {
    sig: u64,
    rows: Vec<RenderedLine>,
}

impl DocCache {
    /// Drop everything if the viewport width or the global show-output toggle
    /// changed — both re-wrap / re-collapse every message, so no slot survives.
    pub fn reset_if_env_changed(&mut self, width: usize, show_output: bool) {
        if width != self.width || show_output != self.show_output {
            self.entries.clear();
            self.width = width;
            self.show_output = show_output;
        }
    }

    /// Forget slots past `len` (e.g. after `:clear` or a session switch).
    pub fn truncate(&mut self, len: usize) {
        if self.entries.len() > len {
            self.entries.truncate(len);
        }
    }

    /// Cached rows for message `mi` if its signature still matches.
    pub fn get(&self, mi: usize, sig: u64) -> Option<&[RenderedLine]> {
        match self.entries.get(mi) {
            Some(Some(e)) if e.sig == sig => Some(&e.rows),
            _ => None,
        }
    }

    /// Store freshly built rows for message `mi`.
    pub fn put(&mut self, mi: usize, sig: u64, rows: Vec<RenderedLine>) {
        if mi >= self.entries.len() {
            self.entries.resize_with(mi + 1, || None);
        }
        self.entries[mi] = Some(CacheEntry { sig, rows });
    }
}

#[derive(Default)]
pub struct ChatState {
    pub scroll: usize,
    /// Follow the tail of the conversation as new content streams in.
    pub stick_bottom: bool,
    /// Collapse-state keys the user has flipped from each block's default (a
    /// left-click on a tool-output header toggles that block's `(msg, block)` key).
    pub toggled: HashSet<(usize, usize)>,
    /// After the next rebuild, scroll so the tail of this message's rows sits at
    /// the bottom of the viewport — used to reveal a just-expanded tool output.
    pub focus_msg: Option<usize>,

    // ── cache ───────────────────────────────────────────────────────────────
    doc: Vec<RenderedLine>,
    cache_rev: u64,
    cache_width: usize,
    cache_valid: bool,
}

impl ChatState {
    pub fn new() -> Self {
        Self {
            stick_bottom: true,
            cache_rev: u64::MAX,
            ..Default::default()
        }
    }

    pub fn needs_rebuild(&self, rev: u64, width: usize) -> bool {
        !self.cache_valid || rev != self.cache_rev || width != self.cache_width
    }

    /// Install a freshly built document and refresh the scroll offset.
    pub fn set_doc(&mut self, doc: Vec<RenderedLine>, rev: u64, width: usize, viewport_h: usize) {
        self.doc = doc;
        self.cache_rev = rev;
        self.cache_width = width;
        self.cache_valid = true;

        let max_scroll = self.doc.len().saturating_sub(viewport_h);
        if let Some(mi) = self.focus_msg.take() {
            // Reveal a just-toggled tool output: put the last row of that message
            // at the bottom of the viewport (its "bottom text" in focus).
            if let Some(end) = self.doc.iter().rposition(|r| r.msg == mi) {
                self.scroll = end
                    .saturating_sub(viewport_h.saturating_sub(1))
                    .min(max_scroll);
                self.stick_bottom = self.scroll >= max_scroll;
            }
        } else if self.stick_bottom || self.scroll > max_scroll {
            self.scroll = max_scroll;
        }
    }

    /// Toggle the individual collapsible block `(msg, block)` and request that its
    /// tail be scrolled into view after the document rebuilds.
    pub fn toggle_block(&mut self, key: (usize, usize)) {
        if !self.toggled.insert(key) {
            self.toggled.remove(&key);
        }
        self.focus_msg = Some(key.0);
    }

    /// The `(msg, block)` toggle key for a click on visible row `vp_row` (0-based
    /// from the top of the viewport). Only the exact collapsible header row
    /// responds — regular text rows in the same message do not trigger a toggle.
    pub fn toggle_at_viewport_row(&self, vp_row: usize) -> Option<(usize, usize)> {
        let row = self.doc.get(self.scroll + vp_row)?;
        row.toggle
    }

    pub fn doc(&self) -> &[RenderedLine] {
        &self.doc
    }

    fn max_scroll(&self, h: usize) -> usize {
        self.doc.len().saturating_sub(h)
    }

    /// How many transcript rows sit below the viewport (hidden past the bottom).
    /// Zero when the tail is visible — drives the "jump to bottom" pill.
    pub fn rows_below(&self, h: usize) -> usize {
        self.max_scroll(h).saturating_sub(self.scroll)
    }

    /// Wheel / line scroll. Positive `delta` scrolls up (toward older content).
    pub fn scroll_by(&mut self, delta: i32, h: usize) {
        let max_scroll = self.max_scroll(h);
        if delta < 0 {
            self.scroll = (self.scroll + (-delta) as usize).min(max_scroll);
        } else {
            self.scroll = self.scroll.saturating_sub(delta as usize);
        }
        self.stick_bottom = self.scroll >= max_scroll;
    }

    pub fn page_up(&mut self, h: usize) {
        self.scroll_by(h.max(1) as i32, h);
    }

    pub fn page_down(&mut self, h: usize) {
        self.scroll_by(-(h.max(1) as i32), h);
    }

    pub fn half_page_up(&mut self, h: usize) {
        self.scroll_by((h / 2).max(1) as i32, h);
    }

    pub fn half_page_down(&mut self, h: usize) {
        self.scroll_by(-((h / 2).max(1) as i32), h);
    }

    pub fn top(&mut self, _h: usize) {
        self.scroll = 0;
        self.stick_bottom = false;
    }

    pub fn bottom(&mut self, h: usize) {
        self.scroll = self.max_scroll(h);
        self.stick_bottom = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::blocks::Block;
    use crate::render::document::{build, DocMessage};
    use crate::render::theme::Theme;

    fn sample_state(rows: usize) -> ChatState {
        let body = (0..rows)
            .map(|i| format!("line {}", i))
            .collect::<Vec<_>>()
            .join("\n");
        let msgs = vec![DocMessage {
            role: "assistant".into(),
            blocks: vec![Block::Markdown(body)],
            duration_ms: None,
            first_ms: None,
            loading: None,
            started_at: None,
        }];
        let doc = build(&msgs, 40, &Theme::default(), &HashSet::new(), false, false);
        let mut s = ChatState::new();
        s.stick_bottom = false;
        s.set_doc(doc, 1, 40, 10);
        s
    }

    #[test]
    fn scroll_clamps_within_bounds() {
        let mut s = sample_state(30);
        s.scroll = 0;
        s.scroll_by(-1000, 10); // scroll way down
        assert_eq!(s.scroll, s.doc().len().saturating_sub(10));
        assert!(s.stick_bottom);
        s.scroll_by(1000, 10); // scroll way up
        assert_eq!(s.scroll, 0);
        assert!(!s.stick_bottom);
    }

    #[test]
    fn top_and_bottom_jump() {
        let mut s = sample_state(40);
        s.top(10);
        assert_eq!(s.scroll, 0);
        s.bottom(10);
        assert_eq!(s.scroll, s.doc().len().saturating_sub(10));
    }

    #[test]
    fn rows_below_reflects_hidden_tail() {
        let mut s = sample_state(40);
        let max = s.doc().len().saturating_sub(10);
        s.top(10); // scrolled to the very top
        assert_eq!(s.rows_below(10), max);
        s.bottom(10); // tail visible → nothing hidden below
        assert_eq!(s.rows_below(10), 0);
    }

    #[test]
    fn rebuild_only_on_rev_or_width_change() {
        let s = sample_state(5);
        assert!(!s.needs_rebuild(1, 40));
        assert!(s.needs_rebuild(2, 40));
        assert!(s.needs_rebuild(1, 50));
    }

    fn one_row(text: &str) -> Vec<RenderedLine> {
        let msgs = vec![DocMessage {
            role: "assistant".into(),
            blocks: vec![Block::Markdown(text.into())],
            duration_ms: None,
            first_ms: None,
            loading: None,
            started_at: None,
        }];
        build(&msgs, 40, &Theme::default(), &HashSet::new(), false, false)
    }

    #[test]
    fn doc_cache_hits_on_matching_sig_and_misses_after_change() {
        let mut c = DocCache::default();
        c.reset_if_env_changed(40, false);
        c.put(0, 111, one_row("hello"));
        // Same signature → hit.
        assert!(c.get(0, 111).is_some());
        // Different signature (content changed) → miss.
        assert!(c.get(0, 222).is_none());
        // Different message index → miss.
        assert!(c.get(1, 111).is_none());
    }

    #[test]
    fn doc_cache_clears_when_width_or_show_output_changes() {
        let mut c = DocCache::default();
        c.reset_if_env_changed(40, false);
        c.put(0, 7, one_row("x"));
        assert!(c.get(0, 7).is_some());
        c.reset_if_env_changed(50, false); // width changed → drop everything
        assert!(c.get(0, 7).is_none());

        c.put(0, 7, one_row("x"));
        c.reset_if_env_changed(50, true); // show_output changed → drop everything
        assert!(c.get(0, 7).is_none());
    }

    #[test]
    fn doc_cache_truncates_removed_messages() {
        let mut c = DocCache::default();
        c.reset_if_env_changed(40, false);
        c.put(0, 1, one_row("a"));
        c.put(1, 2, one_row("b"));
        c.truncate(1); // message 1 removed (e.g. :clear)
        assert!(c.get(0, 1).is_some());
        assert!(c.get(1, 2).is_none());
    }
}
