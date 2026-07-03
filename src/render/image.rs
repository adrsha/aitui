use std::io::Write;
use std::path::Path;

use base64::Engine;

/// Check whether the terminal supports the Kitty graphics protocol.
pub fn supports_kitty() -> bool {
    std::env::var("KITTY_WINDOW_ID").is_ok()
        || std::env::var("WEZTERM_PANE").is_ok()
}

/// The 8-byte PNG signature.
const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

/// Whether `bytes` start with the PNG magic signature.
pub fn is_png(bytes: &[u8]) -> bool {
    bytes.len() >= 8 && bytes[..8] == PNG_MAGIC
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_magic_detected_only_for_png() {
        assert!(is_png(&PNG_MAGIC));
        let mut png = PNG_MAGIC.to_vec();
        png.extend_from_slice(b"trailing data");
        assert!(is_png(&png));
        // JPEG magic, GIF, empty, and short buffers are rejected.
        assert!(!is_png(&[0xff, 0xd8, 0xff, 0xe0]));
        assert!(!is_png(b"GIF89a"));
        assert!(!is_png(&[]));
        assert!(!is_png(&PNG_MAGIC[..7]));
    }

    #[test]
    fn display_rejects_non_png() {
        let dir = std::env::temp_dir();
        let p = dir.join("aitui_test_not_a_png.bin");
        std::fs::write(&p, b"\xff\xd8\xff\xe0 jpeg-ish").unwrap();
        let err = display_image(&p, 0, 0, 4, 2).unwrap_err();
        assert!(err.contains("PNG"), "got: {err}");
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn display_reports_missing_file() {
        let err = display_image(std::path::Path::new("/no/such/aitui.png"), 0, 0, 4, 2)
            .unwrap_err();
        assert!(err.contains("Cannot read"), "got: {err}");
    }
}

/// Display a PNG image at the given terminal cell position using the Kitty
/// graphics protocol. The image occupies `rows` × `cols` cells.
pub fn display_image(path: &Path, col: u16, row: u16, cols: u16, rows: u16) -> Result<(), String> {
    let bytes = std::fs::read(path).map_err(|e| format!("Cannot read image: {}", e))?;

    if bytes.is_empty() {
        return Err("Empty image file".into());
    }

    // The escape below advertises f=100 (PNG). Sending non-PNG bytes makes the
    // terminal reject or mis-decode the payload, so bail early with a clear error
    // rather than corrupting the screen.
    if !is_png(&bytes) {
        return Err("Not a PNG image (inline preview supports PNG only)".into());
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
