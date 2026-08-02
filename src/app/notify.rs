//! Best-effort actionable desktop notifications.
//!
//! Linux notification daemons differ in action support. We request buttons via
//! `notify-send`, but failure or unsupported actions remain a silent no-op and
//! never block the TUI.

use std::sync::mpsc::Sender;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopAction {
    Review,
    AllowOnce,
    DenyOnce,
    AcceptPlan,
    RejectPlan,
}

impl DesktopAction {
    fn id(self) -> &'static str {
        match self {
            Self::Review => "review",
            Self::AllowOnce => "allow_once",
            Self::DenyOnce => "deny_once",
            Self::AcceptPlan => "accept_plan",
            Self::RejectPlan => "reject_plan",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Review => "Review in AiTUI",
            Self::AllowOnce => "Allow once",
            Self::DenyOnce => "Deny once",
            Self::AcceptPlan => "Accept",
            Self::RejectPlan => "Reject",
        }
    }

    fn parse(id: &str) -> Option<Self> {
        match id.trim() {
            "review" | "default" => Some(Self::Review),
            "allow_once" => Some(Self::AllowOnce),
            "deny_once" => Some(Self::DenyOnce),
            "accept_plan" => Some(Self::AcceptPlan),
            "reject_plan" => Some(Self::RejectPlan),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DesktopResponse {
    pub generation: u64,
    pub action: DesktopAction,
}

pub fn desktop(
    title: impl Into<String>,
    body: impl Into<String>,
    actions: &[DesktopAction],
    generation: u64,
    sender: Sender<DesktopResponse>,
) {
    let title = title.into();
    let body = body.into();
    let actions = actions.to_vec();
    std::thread::spawn(move || {
        let mut command = std::process::Command::new("notify-send");
        command
            .arg("--app-name=AiTUI")
            .arg("--hint=string:x-canonical-private-synchronous:aitui")
            .arg("--expire-time=30000")
            .arg("--wait");
        for action in actions {
            command.arg(format!("--action={}={}", action.id(), action.label()));
        }
        let Ok(output) = command.arg(title).arg(body).output() else {
            return;
        };
        if let Ok(id) = String::from_utf8(output.stdout) {
            if let Some(action) = DesktopAction::parse(&id) {
                let _ = sender.send(DesktopResponse { generation, action });
            }
        }
    });
}

pub fn dismiss() {
    std::thread::spawn(|| {
        let _ = std::process::Command::new("notify-send")
            .arg("--app-name=AiTUI")
            .arg("--hint=string:x-canonical-private-synchronous:aitui")
            .arg("--expire-time=1")
            .arg("AiTUI")
            .arg("")
            .status();
    });
}

#[cfg(test)]
mod tests {
    use super::DesktopAction;

    #[test]
    fn notification_action_ids_are_stable_and_unknown_values_are_ignored() {
        assert_eq!(
            DesktopAction::parse("allow_once\n"),
            Some(DesktopAction::AllowOnce)
        );
        assert_eq!(DesktopAction::parse("default"), Some(DesktopAction::Review));
        assert_eq!(DesktopAction::parse("closed"), None);
    }
}
