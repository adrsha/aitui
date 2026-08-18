use std::collections::VecDeque;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const MAX_PENDING: usize = 16;
const MAX_ACTIVE: usize = 3;
const MAX_MESSAGE_CHARS: usize = 600;
const WARNING_TTL: Duration = Duration::from_secs(7);
const ERROR_TTL: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastLevel {
    Warning,
    Error,
}

#[derive(Debug, Clone)]
pub struct Toast {
    pub level: ToastLevel,
    pub message: String,
    pub expires_at: Instant,
}

#[derive(Debug, Clone)]
struct PendingToast {
    level: ToastLevel,
    message: String,
}

fn pending() -> &'static Mutex<VecDeque<PendingToast>> {
    static PENDING: OnceLock<Mutex<VecDeque<PendingToast>>> = OnceLock::new();
    PENDING.get_or_init(|| Mutex::new(VecDeque::new()))
}

pub fn warning(message: impl Into<String>) {
    push_pending(ToastLevel::Warning, message.into());
}

pub fn error(message: impl Into<String>) {
    push_pending(ToastLevel::Error, message.into());
}

fn push_pending(level: ToastLevel, message: String) {
    let message = message.trim();
    if message.is_empty() {
        return;
    }
    let mut message: String = message.chars().take(MAX_MESSAGE_CHARS).collect();
    if message.chars().count() == MAX_MESSAGE_CHARS {
        message.push('…');
    }
    let mut queue = pending()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if queue
        .back()
        .is_some_and(|toast| toast.level == level && toast.message == message)
    {
        return;
    }
    while queue.len() >= MAX_PENDING {
        queue.pop_front();
    }
    queue.push_back(PendingToast { level, message });
}

pub fn drain_into(active: &mut VecDeque<Toast>, now: Instant) -> bool {
    let mut changed = prune(active, now);
    let mut queue = pending()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    while let Some(pending) = queue.pop_front() {
        if let Some(existing) = active
            .iter_mut()
            .find(|toast| toast.level == pending.level && toast.message == pending.message)
        {
            existing.expires_at = now + ttl(pending.level);
            changed = true;
            continue;
        }
        while active.len() >= MAX_ACTIVE {
            active.pop_front();
        }
        active.push_back(Toast {
            level: pending.level,
            message: pending.message,
            expires_at: now + ttl(pending.level),
        });
        changed = true;
    }
    changed
}

pub fn prune(active: &mut VecDeque<Toast>, now: Instant) -> bool {
    let before = active.len();
    active.retain(|toast| toast.expires_at > now);
    before != active.len()
}

fn ttl(level: ToastLevel) -> Duration {
    match level {
        ToastLevel::Warning => WARNING_TTL,
        ToastLevel::Error => ERROR_TTL,
    }
}

#[cfg(test)]
pub fn drain_messages() -> Vec<(ToastLevel, String)> {
    let mut active = VecDeque::new();
    drain_into(&mut active, Instant::now());
    active
        .into_iter()
        .map(|toast| (toast.level, toast.message))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_toasts_are_bounded_and_expire() {
        let now = Instant::now();
        let mut active = VecDeque::new();
        for n in 0..5 {
            push_pending(ToastLevel::Warning, format!("warning {n}"));
        }
        assert!(drain_into(&mut active, now));
        assert_eq!(active.len(), MAX_ACTIVE);
        assert_eq!(active.front().unwrap().message, "warning 2");
        assert!(prune(
            &mut active,
            now + WARNING_TTL + Duration::from_millis(1)
        ));
        assert!(active.is_empty());
    }

    #[test]
    fn duplicate_toast_refreshes_instead_of_stacking() {
        let now = Instant::now();
        let mut active = VecDeque::new();
        warning("same warning");
        drain_into(&mut active, now);
        warning("same warning");
        drain_into(&mut active, now + Duration::from_secs(1));
        assert_eq!(active.len(), 1);
        assert!(active[0].expires_at > now + WARNING_TTL);
    }
}
