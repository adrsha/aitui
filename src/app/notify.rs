//! Best-effort desktop notifications, so the user notices when the agent needs a
//! decision while they're looking elsewhere. Linux only (`notify-send`); silently
//! no-ops if the binary isn't present or the platform differs.

/// Fire a desktop notification without blocking the UI thread. The work (and the
/// child-process reaping) happens on a detached thread, so a slow or missing
/// `notify-send` never stalls rendering and never leaves a zombie process.
pub fn desktop(title: impl Into<String>, body: impl Into<String>) {
    let title = title.into();
    let body = body.into();
    std::thread::spawn(move || {
        let _ = std::process::Command::new("notify-send")
            .arg("--app-name=AiTUI")
            .arg("--expire-time=8000")
            .arg(title)
            .arg(body)
            .output();
    });
}
