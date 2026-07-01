//! Offline mock backend. When mock mode is on (no working API), the user's
//! message is interpreted as a simple command and turned into a real `tool`
//! fence, so the whole agent loop — permission prompts, execution, result
//! rendering, multi-round follow-up — can be exercised with no network.
//!
//! It speaks the same `StreamEvent` channel as the real client, streaming the
//! scripted reply token-by-token so the UI behaves identically.

use std::time::Duration;

use serde_json::json;
use tokio::sync::mpsc;

use super::client::StreamEvent;
use super::models::{ChatRequest, ContentPart, MessageContent, Usage};

/// Spawn a task that streams a scripted reply for `request`. Mirrors
/// `ApiClient::stream` so the caller can't tell the difference.
pub fn stream(request: &ChatRequest) -> mpsc::Receiver<StreamEvent> {
    let reply = build_reply(request);
    // Rough offline token estimate (~4 chars/token) so the top-right counter has
    // something to show in mock mode.
    let prompt_tokens = request.messages.iter().map(|m| message_text(m).len()).sum::<usize>() as u32 / 4;
    let completion_tokens = reply.len() as u32 / 4;
    let (tx, rx) = mpsc::channel(256);
    tokio::spawn(async move {
        for tok in tokenize(&reply) {
            if tx.send(StreamEvent::Token(tok)).await.is_err() {
                return; // receiver dropped (cancelled)
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let _ = tx
            .send(StreamEvent::Usage(Usage {
                prompt_tokens,
                completion_tokens,
                total_tokens: prompt_tokens + completion_tokens,
            }))
            .await;
        let _ = tx.send(StreamEvent::Done).await;
    });
    rx
}

/// Split into small chunks (preserving content exactly) to simulate streaming.
fn tokenize(s: &str) -> Vec<String> {
    s.split_inclusive(' ').map(|w| w.to_string()).collect()
}

/// Build the scripted assistant reply from the latest message.
fn build_reply(request: &ChatRequest) -> String {
    let last = request.messages.last().map(message_text).unwrap_or_default();
    let trimmed = last.trim();

    // A tool just ran (its result is fed back as the latest message). Reply with
    // no further tool calls so the agent loop stops cleanly.
    if trimmed.starts_with("[tool-result]") {
        return "Done — the tool finished (mock mode). Try another command, or type `help`.".to_string();
    }

    // The real command is the last non-empty line (so `@file` context prepended
    // ahead of it is ignored).
    let cmd_line = last.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    let (cmd, arg) = split_first_word(cmd_line);

    match cmd.to_ascii_lowercase().as_str() {
        "read" | "cat" => with_tool("Reading that file…", "read_file", json!({ "path": arg.trim() })),
        "list" | "ls" => {
            let path = if arg.trim().is_empty() { "." } else { arg.trim() };
            with_tool("Listing the directory…", "list_dir", json!({ "path": path }))
        }
        "write" => {
            let (path, content) = split_first_word(arg);
            with_tool("Writing the file…", "write_file", json!({ "path": path, "content": content }))
        }
        "append" => {
            let (path, content) = split_first_word(arg);
            with_tool("Appending to the file…", "append_file", json!({ "path": path, "content": content }))
        }
        "edit" => {
            let (path, rest) = split_first_word(arg);
            match rest.split_once("=>") {
                Some((old, new)) => with_tool(
                    "Editing the file…",
                    "edit_file",
                    json!({ "path": path, "old_string": old.trim(), "new_string": new.trim() }),
                ),
                None => "Usage: `edit <path> <old text> => <new text>` (mock mode)".to_string(),
            }
        }
        "delete" | "rm" => with_tool("Deleting the file…", "delete_file", json!({ "path": arg.trim() })),
        "search" | "grep" => {
            let (pattern, path) = match arg.split_once(" in ") {
                Some((p, d)) => (p.trim(), d.trim()),
                None => (arg.trim(), "."),
            };
            with_tool("Searching files…", "search_files", json!({ "pattern": pattern, "path": path }))
        }
        "run" | "sh" | "shell" => with_tool("Running the command…", "run_shell", json!({ "command": arg.trim() })),
        "demo" => demo_reply(),
        "help" | "" => help_reply(),
        _ => format!("I didn't recognise `{}` (mock mode).\n\n{}", cmd, help_reply()),
    }
}

/// Prose followed by a single `tool` fence the agent will execute.
fn with_tool(prose: &str, name: &str, args: serde_json::Value) -> String {
    let call = json!({ "name": name, "args": args });
    format!("{prose}\n\n```tool\n{call}\n```\n")
}

/// A multi-step showcase: create a file, read it back, then list the directory —
/// three tool calls in one turn (exercises the queue and per-tool permissions).
fn demo_reply() -> String {
    let path = "aitui_mock_demo.txt";
    let write = json!({ "name": "write_file", "args": { "path": path, "content": "Hello from AiTUI mock mode!\n" } });
    let read = json!({ "name": "read_file", "args": { "path": path } });
    let list = json!({ "name": "list_dir", "args": { "path": "." } });
    format!(
        "Quick demo: create a file, read it back, then list the directory.\n\n\
         ```tool\n{write}\n```\n\n```tool\n{read}\n```\n\n```tool\n{list}\n```\n"
    )
}

fn help_reply() -> String {
    "**Mock mode** — no API needed. I turn your message into a tool call so you can \
     exercise the agent. Make sure agent mode is on (Ctrl-A).\n\n\
     Try:\n\
     - `read <path>`\n\
     - `list [dir]`\n\
     - `write <path> <text…>`\n\
     - `append <path> <text…>`\n\
     - `edit <path> <old> => <new>`\n\
     - `delete <path>`\n\
     - `search <pattern> [in <dir>]`\n\
     - `run <command>`\n\
     - `demo` — write, read, then list in one go\n\n\
     Write/edit/run/delete ask for permission first."
        .to_string()
}

fn message_text(m: &super::models::ChatMessage) -> String {
    match &m.content {
        MessageContent::Text(t) => t.clone(),
        MessageContent::Parts(parts) => parts
            .iter()
            .filter_map(|p| match p {
                ContentPart::Text { text } => Some(text.clone()),
                ContentPart::ImageUrl { .. } => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

/// Split off the first whitespace-delimited word; return (word, trimmed rest).
fn split_first_word(s: &str) -> (&str, &str) {
    let s = s.trim_start();
    match s.find(char::is_whitespace) {
        Some(i) => (&s[..i], s[i..].trim_start()),
        None => (s, ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::ChatMessage;

    fn reply_for(text: &str) -> String {
        let req = ChatRequest::new("mock", vec![ChatMessage::user(text)]);
        build_reply(&req)
    }

    #[test]
    fn read_command_emits_read_tool() {
        let r = reply_for("read src/main.rs");
        assert!(r.contains("```tool"));
        assert!(r.contains("\"name\":\"read_file\""));
        assert!(r.contains("src/main.rs"));
    }

    #[test]
    fn write_command_splits_path_and_content() {
        let r = reply_for("write notes.txt hello there");
        assert!(r.contains("\"name\":\"write_file\""));
        assert!(r.contains("notes.txt"));
        assert!(r.contains("hello there"));
    }

    #[test]
    fn edit_command_parses_arrow() {
        let r = reply_for("edit a.txt foo => bar");
        assert!(r.contains("\"name\":\"edit_file\""));
        assert!(r.contains("\"old_string\":\"foo\""));
        assert!(r.contains("\"new_string\":\"bar\""));
    }

    #[test]
    fn search_with_in_clause() {
        let r = reply_for("search TODO in src");
        assert!(r.contains("\"name\":\"search_files\""));
        assert!(r.contains("\"pattern\":\"TODO\""));
        assert!(r.contains("\"path\":\"src\""));
    }

    #[test]
    fn demo_emits_three_tools() {
        let r = reply_for("demo");
        assert_eq!(r.matches("```tool").count(), 3);
    }

    #[test]
    fn tool_result_stops_the_loop() {
        let r = reply_for("[tool-result] Read a.rs (ok)\nsome contents");
        assert!(!r.contains("```tool"));
    }

    #[test]
    fn unknown_command_shows_help() {
        let r = reply_for("flibbertigibbet");
        assert!(r.contains("Mock mode"));
        assert!(!r.contains("```tool"));
    }

    #[test]
    fn command_taken_from_last_line() {
        let r = reply_for("File: x\n```\nctx\n```\n\nread foo.txt");
        assert!(r.contains("\"name\":\"read_file\""));
        assert!(r.contains("foo.txt"));
    }

    #[test]
    fn tokenize_roundtrips() {
        let s = "hello world\n```tool\n{}\n```";
        assert_eq!(tokenize(s).concat(), s);
    }

    #[tokio::test]
    async fn stream_emits_tokens_then_done() {
        let req = ChatRequest::new("mock", vec![ChatMessage::user("read a.txt")]);
        let mut rx = stream(&req);
        let mut text = String::new();
        let mut done = false;
        while let Some(ev) = rx.recv().await {
            match ev {
                StreamEvent::Token(t) => text.push_str(&t),
                StreamEvent::Done => {
                    done = true;
                    break;
                }
                _ => {}
            }
        }
        assert!(done, "stream must end with Done");
        assert!(text.contains("```tool"));
        assert!(text.contains("read_file"));
        assert!(text.contains("a.txt"));
    }
}
