//! Best-effort actionable desktop notifications.
//!
//! Linux notification daemons differ in action support. We request buttons via
//! `notify-send`, but failure or unsupported actions remain a silent no-op and
//! never block the TUI.

use std::io::{BufRead, BufReader, Read};
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc::Sender, Mutex, OnceLock};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopAction {
    Review,
    AllowAll,
    DenyOnce,
    AcceptPlan,
    RejectPlan,
}

impl DesktopAction {
    fn id(self) -> &'static str {
        match self {
            Self::Review => "review",
            Self::AllowAll => "allow_all",
            Self::DenyOnce => "deny_once",
            Self::AcceptPlan => "accept_plan",
            Self::RejectPlan => "reject_plan",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Review => "Review in AiTUI",
            Self::AllowAll => "Allow all",
            Self::DenyOnce => "Deny once",
            Self::AcceptPlan => "Accept",
            Self::RejectPlan => "Reject",
        }
    }

    fn parse(id: &str) -> Option<Self> {
        match id.trim() {
            "review" | "default" => Some(Self::Review),
            // Accept the old ID for notifications delivered before an upgrade.
            "allow_all" | "allow_once" => Some(Self::AllowAll),
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

fn finish_notification(mut child: Child, stderr: Option<std::thread::JoinHandle<String>>) {
    let status = child.wait();
    let stderr = stderr
        .and_then(|reader| reader.join().ok())
        .unwrap_or_default();
    match status {
        Ok(status) if !status.success() || !stderr.trim().is_empty() => {
            report_notification_failure(&stderr)
        }
        Err(error) => {
            crate::app::toast::warning(format!("Desktop notification process failed: {}", error))
        }
        Ok(_) => {}
    }
}

fn report_notification_failure(stderr: &str) {
    let detail = stderr.trim();
    let detail = if detail.is_empty() {
        "notification helper exited unsuccessfully"
    } else {
        detail
    };
    crate::app::toast::warning(format!("Desktop notification failed: {}", detail));
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
            .stdout(Stdio::piped())
            // Never let a background helper inherit the alternate screen. Some
            // notify-send implementations print "Wait timeout expired" here.
            .stderr(Stdio::piped());
        for action in actions {
            command.arg(format!("--action={}={}", action.id(), action.label()));
        }
        let mut child = match command.arg(title).arg(body).spawn() {
            Ok(child) => child,
            Err(error) => {
                crate::app::toast::warning(format!(
                    "Desktop notification could not start: {}",
                    error
                ));
                return;
            }
        };
        let stderr = child.stderr.take().map(|mut stderr| {
            std::thread::spawn(move || {
                let mut output = String::new();
                let _ = stderr.read_to_string(&mut output);
                output
            })
        });
        let id = child.stdout.take().and_then(|stdout| {
            let mut lines = BufReader::new(stdout).lines();
            let id = lines.next()?.ok()?.trim().parse::<u32>().ok()?;
            register_notification(generation, id);
            for line in lines.map_while(Result::ok) {
                if let Some(action) = DesktopAction::parse(&line) {
                    let _ = sender.send(DesktopResponse { generation, action });
                    break;
                }
            }
            Some(id)
        });
        finish_notification(child, stderr);
        if let Some(id) = id {
            clear_notification(generation, id);
        }
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
    use super::{report_notification_failure, DesktopAction};

    #[test]
    fn notification_action_ids_are_stable_and_unknown_values_are_ignored() {
        assert_eq!(
            DesktopAction::parse("allow_all\n"),
            Some(DesktopAction::AllowAll)
        );
        assert_eq!(
            DesktopAction::parse("allow_once\n"),
            Some(DesktopAction::AllowAll)
        );
        assert_eq!(DesktopAction::AllowAll.id(), "allow_all");
        assert_eq!(DesktopAction::AllowAll.label(), "Allow all");
        assert_eq!(DesktopAction::parse("default"), Some(DesktopAction::Review));
        assert_eq!(DesktopAction::parse("closed"), None);
        assert_eq!(DesktopAction::parse("42"), None);
    }

    #[test]
    fn notification_stderr_is_queued_as_a_toast() {
        report_notification_failure("Wait timeout expired\n");
        assert!(crate::app::toast::drain_messages()
            .iter()
            .any(|(level, message)| {
                *level == crate::app::toast::ToastLevel::Warning
                    && message == "Desktop notification failed: Wait timeout expired"
            }));
    }
}
