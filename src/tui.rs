use std::io::{self, Stdout};

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
    // Best-effort: ask the terminal to disambiguate modified keys (so Shift+Enter,
    // Ctrl+Enter, etc. are distinguishable). Terminals that don't support the
    // kitty keyboard protocol silently ignore it; key releases are filtered in
    // the input handler so this can't double-fire keystrokes.
    let _ = execute!(
        io::stdout(),
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
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
        let _ = restore();
        default(info);
    }));
}
