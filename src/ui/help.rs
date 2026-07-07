use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Padding, Paragraph};
use ratatui::Frame;

use crate::app::state::App;
use crate::render::theme::Theme;

pub fn render(f: &mut Frame, app: &App, theme: &Theme) {
    let area = f.area();
    let popup = Rect {
        x: area.width / 6,
        y: area.height / 8,
        width: area.width * 2 / 3,
        height: area.height * 3 / 4,
    };

    f.render_widget(Clear, popup);

    let block = Block::default()
        .title(Span::styled(
            " Keybindings ",
            Style::default().add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .padding(Padding::uniform(1));
    let inner = block.inner(popup);
    f.render_widget(block, popup);

    let km = &app.keymap;
    let key = |k: String| {
        Span::styled(
            format!(" {:<16}", k),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )
    };
    let lit = |k: &str| {
        Span::styled(
            format!(" {:<16}", k),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        )
    };
    let val = |v: &str| Span::styled(v.to_string(), Style::default().fg(theme.text));
    let head = |h: &str| {
        Line::from(Span::styled(
            format!(" {}", h),
            Style::default()
                .fg(theme.warning)
                .add_modifier(Modifier::BOLD),
        ))
    };

    let bindings = vec![
        head("Input (vim — fixed)"),
        Line::from(vec![lit("h j k l"), val("Move cursor")]),
        Line::from(vec![lit("w / b"), val("Word forward / back")]),
        Line::from(vec![lit("0 / $"), val("Line start / end")]),
        Line::from(vec![
            lit("a A · o O · I"),
            val("Append · open line · insert at start"),
        ]),
        Line::from(vec![
            lit("x · dd · yy · p"),
            val("Delete char / line · yank · paste"),
        ]),
        Line::from(vec![]),
        head("Mode (configurable)"),
        Line::from(vec![key(km.insert.label()), val("Insert mode")]),
        Line::from(vec![key(km.normal_label()), val("Back to normal mode")]),
        Line::from(vec![key(km.visual.label()), val("Visual mode")]),
        Line::from(vec![key(km.command.label()), val("Command mode")]),
        Line::from(vec![
            lit("Enter"),
            val("Send message  ·  Shift/Alt-Enter or Ctrl-J = newline"),
        ]),
        Line::from(vec![key(km.palette.label()), val("Command palette")]),
        Line::from(vec![key(km.help.label()), val("Toggle this help")]),
        Line::from(vec![]),
        head("Global (configurable)"),
        Line::from(vec![
            key(km.open_editor.label()),
            val("Open conversation in $EDITOR"),
        ]),
        Line::from(vec![
            key(km.open_file.label()),
            val("File browser → open in $EDITOR (toggle)"),
        ]),
        Line::from(vec![key(km.open_shell.label()), val("Drop into a shell")]),
        Line::from(vec![key(km.session_picker.label()), val("Switch session")]),
        Line::from(vec![
            key(km.fork_session.label()),
            val("Fork session (parallel branch)"),
        ]),
        Line::from(vec![
            key(format!(
                "{} / {}",
                km.next_session.label(),
                km.prev_session.label()
            )),
            val("Next / prev session"),
        ]),
        Line::from(vec![
            key(format!(
                "{} / {}",
                km.scroll_up.label(),
                km.scroll_down.label()
            )),
            val("Scroll transcript (page)"),
        ]),
        Line::from(vec![
            key(format!(
                "{} / {}",
                km.scroll_half_down.label(),
                km.scroll_half_up.label()
            )),
            val("Scroll transcript (half-page)"),
        ]),
        Line::from(vec![
            key(format!(
                "{} / {}",
                km.scroll_top.label(),
                km.scroll_bottom.label()
            )),
            val("Scroll to top / bottom"),
        ]),
        Line::from(vec![
            key(km.toggle_output.label()),
            val("Show / hide tool output"),
        ]),
        Line::from(vec![
            key(format!(
                "{} / {}",
                km.file_picker.label(),
                km.model_picker.label()
            )),
            val("File picker / model picker"),
        ]),
        Line::from(vec![
            key(format!(
                "{} / {}",
                km.prev_model.label(),
                km.next_model.label()
            )),
            val("Prev / next model"),
        ]),
        Line::from(vec![key(km.toggle_agent.label()), val("Toggle agent mode")]),
        Line::from(vec![key(km.quit.label()), val("Cancel response / quit")]),
        Line::from(vec![]),
        head("File browser (Ctrl-E / Ctrl-F)"),
        Line::from(vec![
            lit("h j k l"),
            val("Parent dir · down · up · open file / enter dir"),
        ]),
        Line::from(vec![
            lit("Space · Enter"),
            val("Select file(s) · open all selected"),
        ]),
        Line::from(vec![]),
        head("Message actions (commands)"),
        Line::from(vec![lit(":retry :r"), val("Regenerate the last reply")]),
        Line::from(vec![
            lit(":edit-last :el"),
            val("Edit your last message and resend"),
        ]),
        Line::from(vec![
            lit(":copy :y"),
            val("Copy the last reply to the clipboard"),
        ]),
        Line::from(vec![
            lit(":copy-code :yc"),
            val("Copy the last code block to the clipboard"),
        ]),
        Line::from(vec![
            lit("yy · visual y"),
            val("Yank — also copies to the system clipboard (OSC 52)"),
        ]),
        Line::from(vec![]),
        Line::from(vec![lit("@file"), val("Mention a file into the message")]),
        Line::from(vec![
            lit(":skills"),
            val("Toggle skills (personas) — add .md in ~/.config/aitui/skills/"),
        ]),
        Line::from(vec![lit(":w :q :new"), val("Send · quit · new session")]),
        Line::from(vec![
            lit("(edit ~/.config/aitui/config.toml to rebind)"),
            val(""),
        ]),
    ];

    f.render_widget(Paragraph::new(bindings), inner);
}
