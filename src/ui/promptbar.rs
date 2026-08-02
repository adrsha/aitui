use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::app::state::{App, PromptHitbox};
use crate::domain::session::Session;
use crate::render::chat::ChatState;
use crate::render::theme::{fg_guard, Theme};
use crate::render::wrap::wrap_words;

fn prompt_text_for(session: &Session, chat: &ChatState) -> Option<String> {
    if chat.stick_bottom {
        return session.last_user_text();
    }
    chat.viewport_message()
        .and_then(|message| session.user_text_at_or_before(message))
        .or_else(|| session.last_user_text())
}

pub(crate) fn prompt_text(app: &App) -> Option<String> {
    prompt_text_for(app.sessions.active(), &app.chat)
}

pub fn height(app: &App, width: u16) -> u16 {
    if width < 4 || prompt_text(app).is_none() {
        return 0;
    }
    if !app.show_last_prompt {
        return 1;
    }
    let prompt = prompt_text(app).unwrap_or_default();
    let inner_width = width.saturating_sub(4).max(1) as usize;
    wrapped_lines(&prompt, inner_width)
        .len()
        .saturating_add(1)
        .min(u16::MAX as usize) as u16
}

fn collapsed_hitboxes(area: Rect, suffix_w: u16) -> (PromptHitbox, PromptHitbox) {
    let goto_area = Rect {
        x: area.x + area.width.saturating_sub(suffix_w),
        y: area.y,
        width: suffix_w,
        height: 1,
    };
    let prompt_area = Rect {
        x: area.x,
        y: area.y,
        width: area.width.saturating_sub(suffix_w),
        height: 1,
    };
    (
        PromptHitbox {
            area: prompt_area,
            msg: None,
        },
        PromptHitbox {
            area: goto_area,
            msg: None,
        },
    )
}

pub fn render(
    f: &mut Frame,
    app: &App,
    area: Rect,
    theme: &Theme,
) -> (Option<PromptHitbox>, Option<PromptHitbox>) {
    if area.width < 4 || area.height == 0 {
        return (None, None);
    }
    let prompt = match prompt_text(app) {
        Some(p) => p,
        None => return (None, None),
    };
    let glyph = if app.show_last_prompt { "▾" } else { "▸" };

    if !app.show_last_prompt {
        let first = prompt.lines().next().unwrap_or_default();
        let label_text = format!(" {} LAST PROMPT ", glyph);
        let suffix = " GOTO ";
        let suffix_w = suffix.width() as u16;
        let prefix_w = label_text.width() as u16;
        let avail = (area.width as usize).saturating_sub((prefix_w + suffix_w) as usize);
        let preview = truncate(first, avail);
        let body_w = preview.width() as u16;
        let total_w = prefix_w + body_w + suffix_w;
        let pad = (area.width as usize).saturating_sub(total_w as usize);

        let label_style = Style::default()
            .fg(fg_guard(theme.accent))
            .add_modifier(Modifier::BOLD);
        let body_style = Style::default().fg(fg_guard(theme.text));
        let block_style = Style::default()
            .bg(theme.accent)
            .fg(fg_guard(Color::Black))
            .add_modifier(Modifier::BOLD);
        let mut spans = vec![
            Span::styled(label_text, label_style),
            Span::styled(preview, body_style),
        ];
        if pad > 0 {
            spans.push(Span::raw(" ".repeat(pad)));
        }
        spans.push(Span::styled(suffix.to_string(), block_style));

        f.render_widget(Paragraph::new(Line::from(spans)), area);
        let (prompt, goto) = collapsed_hitboxes(area, suffix_w);
        return (Some(prompt), Some(goto));
    }

    let inner_width = area.width.saturating_sub(4).max(1) as usize;
    let label_style_exp = Style::default()
        .fg(fg_guard(theme.accent))
        .add_modifier(Modifier::BOLD);
    let mut lines = vec![Line::from(vec![
        Span::styled(format!(" {} LAST PROMPT ", glyph), label_style_exp),
        Span::styled(" click to collapse", Style::default().fg(theme.muted)),
    ])];
    lines.extend(wrapped_lines(&prompt, inner_width).into_iter().map(|line| {
        Line::from(vec![
            Span::raw("  "),
            Span::styled(line, Style::default().fg(fg_guard(theme.text))),
        ])
    }));
    f.render_widget(Paragraph::new(lines), area);
    (Some(PromptHitbox { area, msg: None }), None)
}

fn wrapped_lines(text: &str, width: usize) -> Vec<String> {
    text.split('\n')
        .flat_map(|line| wrap_words(line, width))
        .collect()
}

fn truncate(text: &str, max_width: usize) -> String {
    if UnicodeWidthStr::width(text) <= max_width {
        return text.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0usize;
    for ch in text.chars() {
        let width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if used + width >= max_width {
            break;
        }
        out.push(ch);
        used += width;
    }
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::{collapsed_hitboxes, prompt_text_for, truncate, wrapped_lines};
    use crate::api::ChatMessage;
    use crate::domain::blocks::Block;
    use crate::domain::session::Session;
    use crate::render::chat::ChatState;
    use crate::render::document::{build, DocMessage};
    use crate::render::theme::Theme;
    use ratatui::layout::Rect;
    use std::collections::HashSet;

    fn prompt_state() -> (Session, ChatState, usize) {
        let mut session = Session::new(1);
        session.push_message(ChatMessage::user("first prompt"));
        session.push_message(ChatMessage::assistant("first result\nmore"));
        session.push_message(ChatMessage::user("second prompt"));
        session.push_message(ChatMessage::assistant("second result"));
        let messages = [
            ("user", "first prompt"),
            ("assistant", "first result\nmore"),
            ("user", "second prompt"),
            ("assistant", "second result"),
        ]
        .into_iter()
        .map(|(role, text)| DocMessage {
            role: role.into(),
            blocks: vec![Block::Markdown(text.into())],
            duration_ms: None,
            created_at: None,
        })
        .collect::<Vec<_>>();
        let doc = build(
            &messages,
            40,
            &Theme::default(),
            &HashSet::new(),
            false,
            false,
        );
        let first_result = doc.iter().position(|row| row.msg == 1).unwrap();
        let mut chat = ChatState::new();
        chat.set_doc(doc, 1, 40, 3);
        (session, chat, first_result)
    }

    #[test]
    fn scrolled_prompt_follows_the_turn_at_the_viewport_top() {
        let (session, mut chat, first_result) = prompt_state();
        chat.stick_bottom = false;
        chat.scroll = first_result;
        assert_eq!(
            prompt_text_for(&session, &chat).as_deref(),
            Some("first prompt")
        );
    }

    #[test]
    fn bottom_following_prompt_stays_on_the_latest_turn() {
        let (session, mut chat, first_result) = prompt_state();
        chat.scroll = first_result;
        chat.stick_bottom = true;
        assert_eq!(
            prompt_text_for(&session, &chat).as_deref(),
            Some("second prompt")
        );
    }

    #[test]
    fn wrapping_preserves_explicit_newlines() {
        assert_eq!(
            wrapped_lines("one two\nthree", 5),
            vec!["one", "two", "three"]
        );
    }

    #[test]
    fn goto_hitbox_does_not_overlap_last_prompt_toggle() {
        let (prompt, goto) = collapsed_hitboxes(Rect::new(3, 8, 40, 1), 6);
        assert_eq!(prompt.area, Rect::new(3, 8, 34, 1));
        assert_eq!(goto.area, Rect::new(37, 8, 6, 1));
        assert_eq!(prompt.area.x + prompt.area.width, goto.area.x);
    }

    #[test]
    fn collapsed_preview_uses_only_available_width() {
        assert_eq!(truncate("abcdefgh", 5), "abcd…");
        assert_eq!(truncate("abc", 5), "abc");
    }
}
