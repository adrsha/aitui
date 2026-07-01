use std::io::{self, Stdout};

use crossterm::{
    event::{
        DisableMouseCapture, EnableMouseCapture, KeyboardEnhancementFlags,
        PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
    },
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

pub type Tui = Terminal<CrosstermBackend<Stdout>>;

pub fn init() -> anyhow::Result<Tui> {
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture)?;
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
    let _ = execute!(io::stdout(), PopKeyboardEnhancementFlags);
    execute!(io::stdout(), DisableMouseCapture, LeaveAlternateScreen)?;
    disable_raw_mode()?;
    Ok(())
}
