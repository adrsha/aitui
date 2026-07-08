//! Model-driven access control. The user describes, in plain language, what tool
//! access they want auto-granted for the session (a "policy"); a fast judge model
//! then classifies each pending tool call as allow / deny / ask against it.
//!
//! Two hard rules bound the model:
//!   1. A **safety floor** (`needs_hard_prompt`) — destructive / irreversible ops
//!      (delete, dangerous shell, writes escaping the project tree) always prompt
//!      the human, no matter what the policy says. The judge is never asked about
//!      them and cannot approve them.
//!   2. Anything the judge is unsure about, or that comes back malformed, defaults
//!      to **ask** — the safe direction (fall back to the normal prompt).
//!
//! Prompt building and response parsing live here as pure functions so they're
//! testable without the network; `effects.rs` owns the async call itself.

use std::path::Path;

use super::tools::{path_escapes_cwd, ToolCall, ToolKind};

/// What the judge (or the safety floor) decided for one tool call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessVerdict {
    /// Auto-run without prompting.
    Allow,
    /// Auto-skip; the model gets a "denied by policy" result.
    Deny,
    /// Fall back to the normal permission prompt for the human to decide.
    Ask,
}

impl AccessVerdict {
    fn parse(s: &str) -> Self {
        match s.trim().trim_matches(['"', '\'', '.', ',']).to_lowercase().as_str() {
            "allow" | "yes" | "approve" | "approved" | "ok" => AccessVerdict::Allow,
            "deny" | "no" | "reject" | "rejected" | "block" => AccessVerdict::Deny,
            // Anything else — including "ask", "prompt", "unsure", garbage — is the
            // safe fallback: hand it to the human.
            _ => AccessVerdict::Ask,
        }
    }
}

/// The safety floor: a call that always prompts the human regardless of policy.
/// Destructive or irreversible actions can never be auto-approved by the judge.
pub fn needs_hard_prompt(call: &ToolCall, cwd: &Path) -> bool {
    match call.kind() {
        // Permanent, unrecoverable.
        Some(ToolKind::Delete) => true,
        // Shell is a blank cheque; screen for the classic foot-guns.
        Some(ToolKind::Shell) => call
            .get_arg("command")
            .is_some_and(shell_command_is_dangerous),
        // Mutations that reach outside the project tree touch the wider system.
        Some(ToolKind::Write)
        | Some(ToolKind::Edit)
        | Some(ToolKind::Move)
        | Some(ToolKind::Copy)
        | Some(ToolKind::Download) => writes_outside_cwd(call, cwd),
        _ => false,
    }
}

/// Whether a call whose result is a mutation targets a path outside `cwd`.
fn writes_outside_cwd(call: &ToolCall, cwd: &Path) -> bool {
    let keys: &[&str] = match call.kind() {
        Some(ToolKind::Move) | Some(ToolKind::Copy) => &["from", "to"],
        Some(ToolKind::Download) => &["path"],
        _ => &["path"],
    };
    keys.iter()
        .filter_map(|k| call.get_arg(k))
        .any(|p| path_escapes_cwd(p, cwd))
}

/// Substrings that mark a shell command as too dangerous to ever auto-run. Matched
/// against a whitespace-collapsed, lowercased command so `rm   -rf` and `rm -r -f`
/// both trip. Conservative by design: a false positive just means "ask the human".
const DANGEROUS_SHELL: &[&str] = &[
    "rm -rf", "rm -fr", "rm -r -f", "rm -f -r", "rm -r", "rm -f",
    "sudo ", "doas ", "su ",
    "mkfs", "fdisk", "parted", "dd if=", "dd of=",
    "shred", "truncate ", "wipefs",
    ":(){", "fork bomb",
    "shutdown", "reboot", "halt", "poweroff", "init 0", "init 6",
    "chmod -r", "chown -r", "chmod 777",
    "git push --force", "git push -f", "git reset --hard", "git clean -",
    "> /dev/", "of=/dev/",
    "/etc/passwd", "/etc/shadow", "> /etc", ">/etc",
    "curl ", "wget ", // network fetch piped to a shell is the usual vector
    "mv /", "cp /",
    "eval ", "exec ",
    "crontab", "systemctl", "service ",
];

fn shell_command_is_dangerous(command: &str) -> bool {
    let collapsed = command
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    // Pad with spaces so leading/standalone tokens (`rm -r`) match a `" rm -r"`
    // style needle boundary without a full tokenizer.
    let padded = format!(" {} ", collapsed);
    DANGEROUS_SHELL
        .iter()
        .any(|needle| padded.contains(needle) || collapsed.starts_with(needle.trim_start()))
}

/// Build the (system, user) messages sent to the judge model. `calls` are the ones
/// actually being judged (floor calls are handled separately and never sent here).
pub fn build_judge_prompt(policy: &str, calls: &[(usize, String)]) -> (String, String) {
    let system = "You are an access-control classifier for a coding assistant's tool \
calls. The user has given a session policy describing what they are willing to let \
the assistant do WITHOUT being asked each time. For every tool call, decide one of:\n\
  - \"allow\": the policy clearly authorizes this exact action.\n\
  - \"deny\": the policy clearly forbids it.\n\
  - \"ask\": anything not clearly covered, or anything you are unsure about.\n\n\
Be conservative. When in doubt, answer \"ask\" — a human will then decide. Never \
answer \"allow\" for something the policy does not clearly permit. Respond with ONLY \
a JSON array of lowercase strings, one per call, in order. No prose, no code fence.\n\
Example for three calls: [\"allow\",\"ask\",\"deny\"]"
        .to_string();

    let mut user = format!("SESSION POLICY (verbatim from the user):\n{}\n\nTOOL CALLS:\n", policy.trim());
    for (i, (_, desc)) in calls.iter().enumerate() {
        user.push_str(&format!("{}. {}\n", i + 1, desc));
    }
    user.push_str(&format!(
        "\nReturn a JSON array of exactly {} verdict string(s).",
        calls.len()
    ));
    (system, user)
}

/// A compact one-line description of a call for the judge (kind + summary + whether
/// it escapes the project tree), avoiding dumping huge file contents into the prompt.
pub fn describe_call(call: &ToolCall, cwd: &Path) -> String {
    let kind = call.kind().map(|k| k.name()).unwrap_or(&call.name);
    let escapes = writes_outside_cwd(call, cwd);
    if escapes {
        format!("[{}] {} — TARGETS A PATH OUTSIDE THE PROJECT", kind, call.summary())
    } else {
        format!("[{}] {}", kind, call.summary())
    }
}

/// Parse the judge's reply into exactly `n` verdicts. Extracts the first JSON array
/// (tolerating a stray code fence or surrounding prose); any missing / extra / bad
/// entry defaults to `Ask`, so a malformed reply degrades to "prompt the human".
pub fn parse_verdicts(reply: &str, n: usize) -> Vec<AccessVerdict> {
    let mut out = vec![AccessVerdict::Ask; n];
    let Some(arr) = extract_json_array(reply) else {
        return out;
    };
    if let Ok(serde_json::Value::Array(items)) = serde_json::from_str::<serde_json::Value>(&arr) {
        for (i, item) in items.iter().take(n).enumerate() {
            if let Some(s) = item.as_str() {
                out[i] = AccessVerdict::parse(s);
            }
        }
    }
    out
}

/// Slice out the first `[...]` span so a reply wrapped in ```json fences or prose
/// still parses.
fn extract_json_array(reply: &str) -> Option<String> {
    let start = reply.find('[')?;
    let end = reply.rfind(']')?;
    if end > start {
        Some(reply[start..=end].to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn call(name: &str, args: serde_json::Value) -> ToolCall {
        ToolCall {
            name: name.to_string(),
            args,
            id: None,
        }
    }

    #[test]
    fn delete_always_floors() {
        let cwd = PathBuf::from("/proj");
        assert!(needs_hard_prompt(
            &call("delete", serde_json::json!({"path": "a.txt"})),
            &cwd
        ));
    }

    #[test]
    fn dangerous_shell_floors_safe_shell_does_not() {
        let cwd = PathBuf::from("/proj");
        assert!(needs_hard_prompt(
            &call("shell", serde_json::json!({"command": "rm -rf build"})),
            &cwd
        ));
        assert!(needs_hard_prompt(
            &call("shell", serde_json::json!({"command": "sudo systemctl restart x"})),
            &cwd
        ));
        assert!(!needs_hard_prompt(
            &call("shell", serde_json::json!({"command": "cargo test"})),
            &cwd
        ));
    }

    #[test]
    fn write_outside_cwd_floors() {
        let cwd = PathBuf::from("/proj");
        assert!(needs_hard_prompt(
            &call("write", serde_json::json!({"path": "/etc/hosts", "content": "x"})),
            &cwd
        ));
        assert!(!needs_hard_prompt(
            &call("write", serde_json::json!({"path": "src/x.rs", "content": "x"})),
            &cwd
        ));
        assert!(needs_hard_prompt(
            &call("write", serde_json::json!({"path": "../../secret", "content": "x"})),
            &cwd
        ));
    }

    #[test]
    fn read_never_floors() {
        let cwd = PathBuf::from("/proj");
        assert!(!needs_hard_prompt(
            &call("read", serde_json::json!({"path": "anything"})),
            &cwd
        ));
    }

    #[test]
    fn parse_verdicts_maps_and_pads() {
        let v = parse_verdicts("[\"allow\", \"deny\", \"ask\"]", 3);
        assert_eq!(v, vec![AccessVerdict::Allow, AccessVerdict::Deny, AccessVerdict::Ask]);
        // Too few → remaining default to Ask.
        let v = parse_verdicts("[\"allow\"]", 3);
        assert_eq!(v, vec![AccessVerdict::Allow, AccessVerdict::Ask, AccessVerdict::Ask]);
        // Garbage → all Ask.
        let v = parse_verdicts("sorry, I cannot", 2);
        assert_eq!(v, vec![AccessVerdict::Ask, AccessVerdict::Ask]);
    }

    #[test]
    fn parse_verdicts_tolerates_fence_and_prose() {
        let reply = "Here you go:\n```json\n[\"allow\", \"allow\"]\n```";
        let v = parse_verdicts(reply, 2);
        assert_eq!(v, vec![AccessVerdict::Allow, AccessVerdict::Allow]);
    }

    #[test]
    fn unknown_verdict_word_falls_back_to_ask() {
        let v = parse_verdicts("[\"maybe\", \"allow\"]", 2);
        assert_eq!(v, vec![AccessVerdict::Ask, AccessVerdict::Allow]);
    }
}
