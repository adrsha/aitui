//! Post-response follow-up suggestions.
//!
//! Prompt construction and response parsing are pure so suggestion quality and
//! defensive handling can be tested without a live model.

use serde_json::Value;

const MAX_CONTEXT_CHARS: usize = 4_000;
const MAX_SUGGESTION_CHARS: usize = 180;
pub const MAX_SUGGESTIONS: usize = 3;

pub fn turn_signature(user_message: &str, assistant_response: &str) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    user_message.hash(&mut hasher);
    assistant_response.hash(&mut hasher);
    hasher.finish()
}

pub fn build_prompt(user_message: &str, assistant_response: &str) -> (String, String) {
    let system = "Generate exactly three concise follow-up prompts the user could send next. \
Each prompt must be directly sendable, specific to this conversation, and useful. \
Prefer concrete next actions, verification, clarification, or deeper exploration. \
Do not answer the prompts. Return only a JSON array of three strings.";
    let user = format!(
        "LAST USER MESSAGE:\n{}\n\nLAST ASSISTANT RESPONSE:\n{}",
        tail_chars(user_message, MAX_CONTEXT_CHARS),
        tail_chars(assistant_response, MAX_CONTEXT_CHARS)
    );
    (system.to_string(), user)
}

pub fn parse(reply: &str) -> Vec<String> {
    let Some(array) = extract_json_array(reply) else {
        return Vec::new();
    };
    let Ok(Value::Array(items)) = serde_json::from_str::<Value>(array) else {
        return Vec::new();
    };

    let mut suggestions = Vec::with_capacity(MAX_SUGGESTIONS);
    for item in items {
        let Some(text) = item.as_str() else {
            continue;
        };
        let text = normalize(text);
        if text.is_empty() || suggestions.iter().any(|existing| existing == &text) {
            continue;
        }
        suggestions.push(text);
        if suggestions.len() == MAX_SUGGESTIONS {
            break;
        }
    }
    suggestions
}

pub fn fallback(agent_mode: bool) -> Vec<String> {
    if agent_mode {
        vec![
            "Verify the result with the relevant tests".into(),
            "Show me which files changed and why".into(),
            "Explain any remaining risks or follow-up work".into(),
        ]
    } else {
        vec![
            "Can you expand on the most important point?".into(),
            "Give me a concrete example".into(),
            "What would you recommend doing next?".into(),
        ]
    }
}

fn normalize(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches(['"', '\'', '-', '•'])
        .chars()
        .take(MAX_SUGGESTION_CHARS)
        .collect::<String>()
        .trim()
        .to_string()
}

fn extract_json_array(reply: &str) -> Option<&str> {
    let start = reply.find('[')?;
    let end = reply.rfind(']')?;
    (end > start).then(|| &reply[start..=end])
}

fn tail_chars(text: &str, max: usize) -> String {
    let count = text.chars().count();
    text.chars().skip(count.saturating_sub(max)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parser_accepts_fenced_or_surrounded_json_and_deduplicates() {
        let reply = "Suggestions:\n```json\n[\"Run the tests\", \"Run   the tests\", \"Show the diff\", \"Explain risks\"]\n```";
        assert_eq!(
            parse(reply),
            vec!["Run the tests", "Show the diff", "Explain risks"]
        );
    }

    #[test]
    fn parser_rejects_malformed_or_non_string_items() {
        assert!(parse("not json").is_empty());
        assert_eq!(
            parse("[1, \" useful next step \" ]"),
            vec!["useful next step"]
        );
    }

    #[test]
    fn prompt_bounds_each_context_section() {
        let long = "x".repeat(MAX_CONTEXT_CHARS + 500);
        let (_, user) = build_prompt(&long, &long);
        assert!(user.len() < (MAX_CONTEXT_CHARS * 2) + 100);
    }

    #[test]
    fn fallback_is_action_oriented_in_agent_mode() {
        let suggestions = fallback(true);
        assert_eq!(suggestions.len(), MAX_SUGGESTIONS);
        assert!(suggestions[0].contains("tests"));
    }
}
