//! Tree-sitter syntax highlighting for code and file previews.
//!
//! ## Why a one-shot full parse (not incremental)
//!
//! Every preview we highlight — a fenced ```` ```code ```` block, a `read_file`
//! result, a `write_file`/`edit_file` diff — is a *static snapshot*. The chat
//! document is cached (see `render::chat::ChatState`) and only rebuilt when the
//! content revision, viewport width, or a collapse toggle changes, so we never
//! hold a persistent syntax tree that we re-edit keystroke-by-keystroke.
//!
//! Incremental parsing (`Parser::parse` fed the previous `Tree`) only pays off
//! when you re-parse the *same* buffer after small edits — an editor's hot path.
//! Here each render highlights a fresh, immutable string once, so a full parse is
//! both simpler and optimal; there is no old tree to reuse. `tree-sitter-highlight`'s
//! `Highlighter` does exactly this one-shot parse+query, and we cache the compiled
//! per-language `HighlightConfiguration` (query compilation, not parsing) so the
//! only repeated cost is the unavoidable parse of the snippet itself.

use std::cell::RefCell;
use std::collections::HashMap;

use ratatui::style::Style;
use tree_sitter_highlight::{Highlight, HighlightConfiguration, HighlightEvent, Highlighter};

use crate::render::theme::Theme;

/// The highlight capture categories we recognise. Tree-sitter matches a capture
/// name like `variable.parameter` or `punctuation.bracket` against the longest
/// matching entry here; anything unmatched renders in the default text colour.
const CAPTURES: &[&str] = &[
    "attribute",
    "comment",
    "constant",
    "constant.builtin",
    "constructor",
    "escape",
    "function",
    "function.builtin",
    "function.method",
    "keyword",
    "label",
    "number",
    "operator",
    "property",
    "punctuation",
    "punctuation.bracket",
    "punctuation.delimiter",
    "string",
    "string.special",
    "tag",
    "type",
    "type.builtin",
    "variable",
    "variable.builtin",
    "variable.parameter",
];

thread_local! {
    /// Compiled per-language configurations, built lazily and reused. Rendering
    /// is single-threaded (the TUI draw loop), so a thread-local is sufficient
    /// and avoids locking.
    static CONFIGS: RefCell<HashMap<&'static str, HighlightConfiguration>> =
        RefCell::new(HashMap::new());
}

/// Map a language token (a fence info-string like `rust`/`py`, or a filename /
/// extension like `main.rs`) to a grammar: its language, a stable name, and its
/// highlights query. Returns `None` for unknown/unsupported languages.
fn grammar(token: &str) -> Option<(tree_sitter::Language, &'static str, &'static str)> {
    let lower = token.trim().to_lowercase();
    // Accept a bare token, a filename, or a dotted path — take the last segment
    // after '.', then also after '/' in case there was no extension.
    let ext = lower.rsplit('.').next().unwrap_or(&lower);
    let ext = ext.rsplit('/').next().unwrap_or(ext);

    let g = match ext {
        "rs" | "rust" => (
            tree_sitter_rust::LANGUAGE.into(),
            "rust",
            tree_sitter_rust::HIGHLIGHTS_QUERY,
        ),
        "py" | "python" | "pyi" => (
            tree_sitter_python::LANGUAGE.into(),
            "python",
            tree_sitter_python::HIGHLIGHTS_QUERY,
        ),
        "js" | "jsx" | "mjs" | "cjs" | "javascript" => (
            tree_sitter_javascript::LANGUAGE.into(),
            "javascript",
            tree_sitter_javascript::HIGHLIGHT_QUERY,
        ),
        "ts" | "typescript" => (
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            "typescript",
            tree_sitter_typescript::HIGHLIGHTS_QUERY,
        ),
        "tsx" => (
            tree_sitter_typescript::LANGUAGE_TSX.into(),
            "tsx",
            tree_sitter_typescript::HIGHLIGHTS_QUERY,
        ),
        "json" | "jsonc" => (
            tree_sitter_json::LANGUAGE.into(),
            "json",
            tree_sitter_json::HIGHLIGHTS_QUERY,
        ),
        "sh" | "bash" | "zsh" | "shell" => (
            tree_sitter_bash::LANGUAGE.into(),
            "bash",
            tree_sitter_bash::HIGHLIGHT_QUERY,
        ),
        "go" | "golang" => (
            tree_sitter_go::LANGUAGE.into(),
            "go",
            tree_sitter_go::HIGHLIGHTS_QUERY,
        ),
        "c" | "h" => (
            tree_sitter_c::LANGUAGE.into(),
            "c",
            tree_sitter_c::HIGHLIGHT_QUERY,
        ),
        "css" | "scss" => (
            tree_sitter_css::LANGUAGE.into(),
            "css",
            tree_sitter_css::HIGHLIGHTS_QUERY,
        ),
        "html" | "htm" | "xml" => (
            tree_sitter_html::LANGUAGE.into(),
            "html",
            tree_sitter_html::HIGHLIGHTS_QUERY,
        ),
        _ => return None,
    };
    Some(g)
}

/// The colour for a recognised capture, mapped by its top-level category so that
/// e.g. `variable.parameter` and `variable.builtin` share the `variable` hue.
fn capture_style(name: &str, theme: &Theme) -> Style {
    let base = name.split('.').next().unwrap_or(name);
    let color = match base {
        "keyword" => theme.hl_keyword,
        "function" | "constructor" => theme.hl_function,
        "type" => theme.hl_type,
        "string" | "escape" => theme.hl_string,
        "comment" => theme.hl_comment,
        "number" => theme.hl_number,
        "constant" => theme.hl_constant,
        "property" | "attribute" | "tag" | "label" => theme.hl_property,
        "operator" | "punctuation" => theme.hl_punct,
        "variable" => theme.hl_variable,
        _ => theme.text,
    };
    Style::default().fg(color)
}

/// One styled text segment (kept owned so results outlive the borrow of the
/// per-language config).
pub type Segment = (String, Style);

/// Highlight `code` as `lang`, returning one line of styled segments per source
/// line (no trailing newline lines added). Returns `None` when the language is
/// unsupported or parsing fails, so callers can fall back to plain rendering.
pub fn highlight(code: &str, lang: &str, theme: &Theme) -> Option<Vec<Vec<Segment>>> {
    let (language, name, query) = grammar(lang)?;

    CONFIGS.with(|cell| {
        let mut map = cell.borrow_mut();
        if !map.contains_key(name) {
            let mut cfg = HighlightConfiguration::new(language, name, query, "", "").ok()?;
            cfg.configure(CAPTURES);
            map.insert(name, cfg);
        }
        let cfg = map.get(name)?;
        run(cfg, code, theme)
    })
}

fn run(cfg: &HighlightConfiguration, code: &str, theme: &Theme) -> Option<Vec<Vec<Segment>>> {
    let src = code.as_bytes();
    let mut hl = Highlighter::new();
    let events = hl.highlight(cfg, src, None, |_| None).ok()?;

    let default = Style::default().fg(theme.text);
    let mut lines: Vec<Vec<Segment>> = vec![Vec::new()];
    // Stack of active highlights; the innermost (top) wins.
    let mut stack: Vec<usize> = Vec::new();

    for event in events {
        match event.ok()? {
            HighlightEvent::HighlightStart(Highlight(i)) => stack.push(i),
            HighlightEvent::HighlightEnd => {
                stack.pop();
            }
            HighlightEvent::Source { start, end } => {
                let text = std::str::from_utf8(src.get(start..end)?).ok()?;
                let style = stack
                    .last()
                    .map(|&i| capture_style(CAPTURES[i], theme))
                    .unwrap_or(default);
                // Split across newlines: each '\n' starts a fresh output line.
                let mut first = true;
                for piece in text.split('\n') {
                    if !first {
                        lines.push(Vec::new());
                    }
                    first = false;
                    if !piece.is_empty() {
                        lines.last_mut()?.push((piece.to_string(), style));
                    }
                }
            }
        }
    }
    Some(lines)
}

/// Whether a language token has a grammar (used to decide if a fence/preview
/// should attempt highlighting at all).
pub fn is_supported(lang: &str) -> bool {
    grammar(lang).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_keywords_get_keyword_colour() {
        let theme = Theme::default();
        let lines = highlight("fn main() {}\n", "rust", &theme).unwrap();
        // First line has segments; the `fn` keyword should be styled distinctly
        // from the default text colour.
        let seg = &lines[0];
        let fn_seg = seg.iter().find(|(t, _)| t == "fn").expect("fn segment");
        assert_eq!(fn_seg.1.fg, Some(theme.hl_keyword));
    }

    #[test]
    fn line_count_matches_source() {
        let theme = Theme::default();
        let lines = highlight("let a = 1;\nlet b = 2;\n", "rust", &theme).unwrap();
        // Two code lines + a trailing empty line from the final newline.
        assert!(lines.len() >= 2);
        let plain: String = lines[0].iter().map(|(t, _)| t.as_str()).collect();
        assert!(plain.contains("let a = 1;"));
    }

    #[test]
    fn resolves_by_extension_and_alias() {
        assert!(is_supported("main.rs"));
        assert!(is_supported("py"));
        assert!(is_supported("script.sh"));
        assert!(!is_supported("unknown_lang_xyz"));
        assert!(!is_supported(""));
    }

    #[test]
    fn unsupported_language_returns_none() {
        let theme = Theme::default();
        assert!(highlight("whatever", "brainfuck", &theme).is_none());
    }
}
