use std::io::Write;
use std::path::Path;

use base64::Engine;

/// Check whether the terminal supports the Kitty graphics protocol.
pub fn supports_kitty() -> bool {
    std::env::var("KITTY_WINDOW_ID").is_ok()
        || std::env::var("WEZTERM_PANE").is_ok()
}

/// Display a PNG image at the given terminal cell position using the Kitty
/// graphics protocol. The image occupies `rows` × `cols` cells.
pub fn display_image(path: &Path, col: u16, row: u16, cols: u16, rows: u16) -> Result<(), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("Cannot read image: {}", e))?;

    if bytes.is_empty() {
        return Err("Empty image file".into());
    }

    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let mut out = std::io::stdout().lock();

    // Delete any previously placed images so we don't accumulate ghost images.
    let _ = write!(out, "\x1b_Ga=d\x1b\\");
    // Move cursor to target position.
    let _ = write!(out, "\x1b[{};{}H", row + 1, col + 1);
    // Transmit and place the image at the cursor position.
    // a=T: transmit and place, f=100: PNG, c=cols, r=rows, m=0: last chunk
    let _ = write!(
        out,
        "\x1b_Ga=T,f=100,c={},r={},m=0;{}\x1b\\",
        cols, rows, b64
    );
    let _ = out.flush();
    Ok(())
}
