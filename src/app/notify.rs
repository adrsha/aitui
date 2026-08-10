//! Best-effort actionable desktop notifications.
//!
//! Linux notification daemons differ in action support. We request buttons via
//! `notify-send`, but failure or unsupported actions remain a silent no-op and
//! never block the TUI.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::{mpsc::Sender, Mutex, OnceLock};

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

#[derive(Debug, Default)]
struct ActiveNotification {
    generation: u64,
    id: Option<u32>,
    cancelled: bool,
}

fn active_notification() -> &'static Mutex<ActiveNotification> {
    static ACTIVE: OnceLock<Mutex<ActiveNotification>> = OnceLock::new();
    ACTIVE.get_or_init(|| Mutex::new(ActiveNotification::default()))
}

fn close_notification(id: u32) {
    std::thread::spawn(move || {
        let _ = Command::new("gdbus")
            .args([
                "call",
                "--session",
                "--dest",
                "org.freedesktop.Notifications",
                "--object-path",
                "/org/freedesktop/Notifications",
                "--method",
                "org.freedesktop.Notifications.CloseNotification",
                &id.to_string(),
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    });
}

fn register_notification(generation: u64, id: u32) {
    let close_now = if let Ok(mut active) = active_notification().lock() {
        if active.generation != generation {
            true
        } else if active.cancelled {
            active.id = None;
            true
        } else {
            active.id = Some(id);
            false
        }
    } else {
        true
    };
    if close_now {
        close_notification(id);
    }
}

fn clear_notification(generation: u64, id: u32) {
    if let Ok(mut active) = active_notification().lock() {
        if active.generation == generation && active.id == Some(id) {
            active.id = None;
        }
    }
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

    let previous = if let Ok(mut active) = active_notification().lock() {
        let previous = active.id.take();
        *active = ActiveNotification {
            generation,
            id: None,
            cancelled: false,
        };
        previous
    } else {
        None
    };
    if let Some(id) = previous {
        close_notification(id);
    }

    std::thread::spawn(move || {
        let mut command = Command::new("notify-send");
        command
            .arg("--app-name=AiTUI")
            .arg("--expire-time=30000")
            .arg("--print-id")
            .arg("--wait")
            .stdout(Stdio::piped());
        for action in actions {
            command.arg(format!("--action={}={}", action.id(), action.label()));
        }
        let Ok(mut child) = command.arg(title).arg(body).spawn() else {
            return;
        };
        let Some(stdout) = child.stdout.take() else {
            let _ = child.wait();
            return;
        };
        let mut lines = BufReader::new(stdout).lines();
        let Some(Ok(id_line)) = lines.next() else {
            let _ = child.wait();
            return;
        };
        let Ok(id) = id_line.trim().parse::<u32>() else {
            let _ = child.wait();
            return;
        };
        register_notification(generation, id);

        for line in lines.map_while(Result::ok) {
            if let Some(action) = DesktopAction::parse(&line) {
                let _ = sender.send(DesktopResponse { generation, action });
                break;
            }
        }
        let _ = child.wait();
        clear_notification(generation, id);
    });
}

pub fn dismiss() {
    let id = if let Ok(mut active) = active_notification().lock() {
        active.cancelled = true;
        active.id.take()
    } else {
        None
    };
    if let Some(id) = id {
        close_notification(id);
    }
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
        assert_eq!(DesktopAction::parse("42"), None);
    }
}
