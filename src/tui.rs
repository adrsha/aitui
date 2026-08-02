use std::cell::Cell;
use std::io::{self, Stdout, Write};
use std::panic::{self, AssertUnwindSafe};

use crossterm::{
    event::{
        DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
        EnableFocusChange, EnableMouseCapture, KeyboardEnhancementFlags,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

pub type Tui = Terminal<CrosstermBackend<Stdout>>;

thread_local! {
    static RECOVERABLE_PANIC_DEPTH: Cell<usize> = const { Cell::new(0) };
}

pub fn catch_recoverable_panic<F, T>(f: F) -> std::thread::Result<T>
where
    F: FnOnce() -> T,
{
    struct RecoverablePanicGuard;

    impl Drop for RecoverablePanicGuard {
        fn drop(&mut self) {
            RECOVERABLE_PANIC_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
        }
    }

    RECOVERABLE_PANIC_DEPTH.with(|depth| depth.set(depth.get() + 1));
    let guard = RecoverablePanicGuard;
    let result = panic::catch_unwind(AssertUnwindSafe(f));
    drop(guard);
    result
}

fn should_report_panic() -> bool {
    !RECOVERABLE_PANIC_DEPTH.with(|depth| depth.get() > 0)
}

pub fn init() -> anyhow::Result<Tui> {
    enable_raw_mode()?;
    // Bracketed paste: the terminal hands us a whole paste as one `Event::Paste`
    // (so a big paste isn't replayed key-by-key), which the smart-paste handler
    // turns into a file attachment or a compact placeholder chip.
    execute!(
        io::stdout(),
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableFocusChange,
        EnableBracketedPaste
    )?;
    // Enable button-event tracking so crossterm can report Drag(MouseButton)
    // events. Crossterm's EnableMouseCapture only enables normal tracking
    // (?1000h); the drag variant is present in its event model but needs
    // the additional ?1002h mode bit from the terminal.
    let _ = write!(io::stdout(), "\x1b[?1002h");
    // Best-effort: ask the terminal to disambiguate modified keys (so Shift+Enter,
    // Ctrl+Enter, etc. are distinguishable). Terminals that don't support the
    // kitty keyboard protocol silently ignore it; key releases are filtered in
    // the input handler so this can't double-fire keystrokes.
    let _ = execute!(
        io::stdout(),
        PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
                | KeyboardEnhancementFlags::REPORT_ALL_KEYS_AS_ESCAPE_CODES,
        )
    );
    let backend = CrosstermBackend::new(io::stdout());
    Ok(Terminal::new(backend)?)
}

pub fn restore() -> anyhow::Result<()> {
    // Best-effort and independent: if one step fails we still attempt the rest, so
    // a partial failure can't strand the terminal in raw mode or the alt screen.
    let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
    let _ = execute!(io::stdout(), DisableBracketedPaste);
    let _ = execute!(io::stdout(), DisableFocusChange);
    let _ = execute!(io::stdout(), DisableMouseCapture);
    let _ = write!(io::stdout(), "\x1b[?1002l"); // button-event tracking off
    let _ = execute!(io::stdout(), LeaveAlternateScreen);
    let _ = disable_raw_mode();
    Ok(())
}

/// Install a panic hook that restores the terminal *before* the default hook runs,
/// so a panic anywhere in the render/dispatch loop can never leave the user's shell
/// in raw mode with no echo. The default hook then prints the panic + backtrace to
/// the now-restored terminal. Call once, before `init`.
pub fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if !should_report_panic() {
            return;
        }
        let _ = restore();
        default(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::{catch_recoverable_panic, should_report_panic};

    #[test]
    fn recoverable_panic_scope_suppresses_reporting_only_inside_guard() {
        assert!(should_report_panic());
        let result = catch_recoverable_panic(|| {
            assert!(!should_report_panic());
            panic!("provider failed");
        });
        assert!(result.is_err());
        assert!(should_report_panic());
    }

    #[test]
    fn nested_recoverable_panic_scopes_restore_the_outer_scope() {
        catch_recoverable_panic(|| {
            assert!(!should_report_panic());
            let inner = catch_recoverable_panic(|| panic!("inner provider failed"));
            assert!(inner.is_err());
            assert!(!should_report_panic());
        })
        .unwrap();
        assert!(should_report_panic());
    }
}
