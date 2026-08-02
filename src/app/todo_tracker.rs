//! Parallel task-tracker agent.
//!
//! The active agent never edits the task checklist itself — it only *sees* the
//! current tasks (injected read-only into its system messages). A separate,
//! cheap background model call runs after every completed response and updates
//! the checklist: which items are done, which is in progress, per-item percent,
//! and an overall progress figure. Prompt construction and response parsing are
//! pure so the tracker can be tested without a live model.

use std::hash::{Hash, Hasher};

use serde_json::Value;

use crate::app::state::{TodoItem, TodoStatus, TodoUpdate};

const MAX_CONTEXT_CHARS: usize = 6_000;
const MAX_TODO_CHARS: usize = 400;

/// Signature over the exact input the tracker saw, so a slow result that lands
/// after a newer turn (or a newer checklist) is dropped instead of clobbering it.
/// `child_reports` are completed child-agent summaries the tracker also saw.
pub fn update_signature(
    user_message: &str,
    assistant_response: &str,
    todos: &[TodoItem],
    child_reports: &[(String, String)],
) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    user_message.hash(&mut hasher);
    assistant_response.hash(&mut hasher);
    for todo in todos {
        todo.text.hash(&mut hasher);
        std::mem::discriminant(&todo.status).hash(&mut hasher);
    }
    for (name, body) in child_reports {
        name.hash(&mut hasher);
        body.hash(&mut hasher);
    }
    hasher.finish()
}

pub fn build_prompt(
    user_message: &str,
    assistant_response: &str,
    todos: &[TodoItem],
    child_reports: &[(String, String)],
) -> (String, String) {
    let system = "You are the task tracker for a coding agent working in a terminal. \
Your ONLY job is to maintain the visible task checklist. You do not run tools and you do not \
answer the user.\n\
Given the last user message, the agent's latest response, completed child-agent reports, and \
the current checklist, produce the updated checklist:\n\
- Create the checklist from the conversation when none exists yet.\n\
- Mark items the agent (or its child agents) has verifiably finished as \"done\".\n\
- Mark exactly one currently active item as \"in_progress\" (the work the agent is doing now).\n\
- Tasks worked on inside child agents still count: use their reports and todo activity.\n\
- Keep item text identical unless the conversation genuinely replans.\n\
- Estimate a percent (0-100) for every item reflecting real progress.\n\
- Estimate an overall_percent (0-100) for the whole checklist.\n\
Reply with ONLY a JSON object, no prose:\n\
{\"overall_percent\": 40, \"items\": [{\"text\": \"...\", \"status\": \"pending|in_progress|done\", \"percent\": 10}]}";
    let current = if todos.is_empty() {
        "(no checklist yet — create one from the conversation)".to_string()
    } else {
        todos
            .iter()
            .enumerate()
            .map(|(i, todo)| {
                let percent = todo.percent.map(|p| format!(" {}%", p)).unwrap_or_default();
                format!(
                    "{}. [{}{}] {}",
                    i + 1,
                    todo.status.name(),
                    percent,
                    todo.text
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let children = if child_reports.is_empty() {
        "(none completed since the last update)".to_string()
    } else {
        child_reports
            .iter()
            .map(|(name, body)| format!("- {}:\n{}", name, tail_chars(body, MAX_CONTEXT_CHARS / 2)))
            .collect::<Vec<_>>()
            .join("\n")
    };
    let user = format!(
        "LAST USER MESSAGE:\n{}\n\nLAST AGENT RESPONSE:\n{}\n\nCHILD AGENT REPORTS:\n{}\n\nCURRENT CHECKLIST:\n{}\n\nReturn the updated checklist JSON.",
        tail_chars(user_message, MAX_CONTEXT_CHARS),
        tail_chars(assistant_response, MAX_CONTEXT_CHARS),
        tail_chars(&children, MAX_CONTEXT_CHARS),
        tail_chars(&current, MAX_TODO_CHARS)
    );
    (system.to_string(), user)
}

/// Parse the tracker's JSON reply into a checklist update. Accepts either a
/// bare array of items or an object with `overall_percent` + `items`.
pub fn parse(reply: &str) -> TodoUpdate {
    let Some(block) = extract_json_block(reply) else {
        return TodoUpdate::default();
    };
    let Ok(value) = serde_json::from_str::<Value>(block) else {
        return TodoUpdate::default();
    };
    let mut update = TodoUpdate::default();
    let items: Vec<Value> = match &value {
        Value::Array(items) => items.clone(),
        Value::Object(map) => {
            if let Some(percent) = map.get("overall_percent").and_then(Value::as_u64) {
                update.overall_percent = Some(percent.min(100) as u8);
            }
            map.get("items")
                .and_then(Value::as_array)
                .or_else(|| map.get("todos").and_then(Value::as_array))
                .cloned()
                .unwrap_or_default()
        }
        _ => return TodoUpdate::default(),
    };
    let mut seen = std::collections::HashSet::new();
    for item in items {
        let text = match item.as_str().or_else(|| {
            item.get("text")
                .or_else(|| item.get("content"))
                .and_then(Value::as_str)
        }) {
            Some(text) => text.trim().to_string(),
            None => continue,
        };
        if text.is_empty() || !seen.insert(text.clone()) {
            continue;
        }
        let status = item
            .get("status")
            .and_then(Value::as_str)
            .map(TodoStatus::parse)
            .unwrap_or(TodoStatus::Pending);
        let percent = item
            .get("percent")
            .and_then(Value::as_u64)
            .map(|p| p.min(100) as u8);
        update.items.push(TodoItem {
            text,
            status,
            percent,
        });
    }
    update
}

fn extract_json_block(reply: &str) -> Option<&str> {
    let trimmed = reply.trim_start();
    if trimmed.starts_with('[') {
        let start = trimmed.find('[')?;
        let end = trimmed.rfind(']')?;
        return (end > start).then(|| &trimmed[start..=end]);
    }
    let start = trimmed.find('{')?;
    let end = trimmed.rfind('}')?;
    (end > start).then(|| &trimmed[start..=end])
}

fn tail_chars(text: &str, max: usize) -> String {
    let count = text.chars().count();
    text.chars().skip(count.saturating_sub(max)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::TodoStatus;

    #[test]
    fn parses_object_with_items_and_overall_percent() {
        let reply = r#"```json
{"overall_percent": 66, "items": [
  {"text": "Fix build", "status": "done", "percent": 100},
  {"text": "Add tests", "status": "in_progress", "percent": 33}
]}
```"#;
        let update = parse(reply);
        assert_eq!(update.overall_percent, Some(66));
        assert_eq!(update.items.len(), 2);
        assert_eq!(update.items[0].status, TodoStatus::Done);
        assert_eq!(update.items[0].percent, Some(100));
        assert_eq!(update.items[1].status, TodoStatus::InProgress);
        assert_eq!(update.items[1].percent, Some(33));
    }

    #[test]
    fn parses_bare_array_and_defaults_missing_fields() {
        let update = parse(r#"[{"text": "one", "status": "done"}]"#);
        assert_eq!(update.overall_percent, None);
        assert_eq!(update.items.len(), 1);
        assert_eq!(update.items[0].percent, None);
    }

    #[test]
    fn rejects_malformed_or_empty_replies() {
        assert!(parse("not json at all").items.is_empty());
        assert!(parse("[]").items.is_empty());
        assert!(parse(r#"{"items": [{"text": "  ", "status": "done"}]}"#)
            .items
            .is_empty());
    }

    #[test]
    fn clamps_percents_to_100_and_deduplicates() {
        let reply = r#"{"overall_percent": 999, "items": [
          {"text": "a", "percent": 500},
          {"text": "a", "percent": 10}
        ]}"#;
        let update = parse(reply);
        assert_eq!(update.overall_percent, Some(100));
        assert_eq!(update.items.len(), 1);
        assert_eq!(update.items[0].percent, Some(100));
    }

    #[test]
    fn signature_changes_with_inputs() {
        let todos = vec![TodoItem {
            text: "Fix build".into(),
            status: TodoStatus::Pending,
            percent: None,
        }];
        let a = update_signature("u", "a", &todos, &[]);
        let b = update_signature("u", "a", &[], &[]);
        let c = update_signature("u", "a2", &todos, &[]);
        let d = update_signature("u", "a", &todos, &[("child".into(), "report".into())]);
        assert_ne!(a, b);
        assert_ne!(a, c);
        assert_ne!(a, d);
    }

    #[test]
    fn prompt_bounds_each_context_section() {
        let long = "x".repeat(MAX_CONTEXT_CHARS + 500);
        let (_, user) = build_prompt(&long, &long, &[], &[]);
        assert!(user.len() < (MAX_CONTEXT_CHARS * 2) + MAX_TODO_CHARS + 200);
    }

    #[test]
    fn prompt_includes_child_reports() {
        let (_, user) = build_prompt(
            "u",
            "a",
            &[],
            &[("researcher".into(), "found the entry point".into())],
        );
        assert!(user.contains("CHILD AGENT REPORTS"));
        assert!(user.contains("researcher"));
        assert!(user.contains("found the entry point"));
    }
}
