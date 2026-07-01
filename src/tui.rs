use std::io::{self, Stdout};

use crossterm::{
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
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
    execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture, EnableBracketedPaste)?;
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
    execute!(io::stdout(), DisableBracketedPaste, DisableMouseCapture, LeaveAlternateScreen)?;
    disable_raw_mode()?;
    Ok(())
}
