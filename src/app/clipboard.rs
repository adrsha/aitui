//! System-clipboard copy via the OSC 52 terminal escape. OSC 52 works across
//! most modern terminals *and over SSH* (the terminal, not the host, owns the
//! clipboard), and needs no external binary — unlike `xclip`/`pbcopy`. The raw
//! escape is written by the renderer (see `ui::flush_clipboard`) so all direct
//! stdout writes stay in the UI layer.

use std::io::Write;
use std::path::{Path, PathBuf};

use base64::Engine;

const MAX_IMAGE_DIMENSION: u32 = 1024;
const MAX_CLIPBOARD_PIXELS: usize = 100_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardContent {
    Image(PathBuf),
    Text(String),
}

/// Read the system clipboard, preferring image pixels over text. Clipboard images
/// are normalized to a bounded PNG so the existing attachment/message pipeline
/// can handle files and clipboard captures identically.
pub fn read() -> Result<ClipboardContent, String> {
    let mut clipboard =
        arboard::Clipboard::new().map_err(|error| format!("clipboard is unavailable: {error}"))?;
    let image_error = match clipboard.get_image() {
        Ok(image) => {
            return save_rgba_image(image.width, image.height, image.bytes.as_ref())
                .map(ClipboardContent::Image)
        }
        Err(error) => error,
    };
    match clipboard.get_text() {
        Ok(text) if !text.is_empty() => Ok(ClipboardContent::Text(text)),
        Ok(_) => Err("clipboard contains no image or text".into()),
        Err(text_error) => Err(format!(
            "clipboard contains no readable image or text (image: {image_error}; text: {text_error})"
        )),
    }
}

fn clipboard_image_dir() -> PathBuf {
    std::env::temp_dir().join("aitui-clipboard")
}

/// Remove an AiTUI-owned clipboard image after it is replaced or consumed. User
/// attachments are never touched; only our exact temp directory and filename
/// prefix are eligible.
pub fn remove_managed_image(path: &Path) {
    let managed = path.parent() == Some(clipboard_image_dir().as_path())
        && path
            .file_name()
            .and_then(|value| value.to_str())
            .is_some_and(|name| name.starts_with("clipboard-") && name.ends_with(".png"));
    if managed {
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
pub fn save_test_image() -> PathBuf {
    save_rgba_image(1, 1, &[1, 2, 3, 255]).expect("test clipboard image")
}

fn save_rgba_image(width: usize, height: usize, bytes: &[u8]) -> Result<PathBuf, String> {
    let pixels = width
        .checked_mul(height)
        .ok_or("clipboard image dimensions overflow")?;
    if width == 0 || height == 0 {
        return Err("clipboard image has zero width or height".into());
    }
    if pixels > MAX_CLIPBOARD_PIXELS {
        return Err(format!(
            "clipboard image is too large: {width}x{height} pixels"
        ));
    }
    let expected = pixels
        .checked_mul(4)
        .ok_or("clipboard image byte length overflows")?;
    if bytes.len() != expected {
        return Err(format!(
            "clipboard image has invalid RGBA data: expected {expected} bytes, got {}",
            bytes.len()
        ));
    }
    let width = u32::try_from(width).map_err(|_| "clipboard image width exceeds u32")?;
    let height = u32::try_from(height).map_err(|_| "clipboard image height exceeds u32")?;
    let rgba = image::RgbaImage::from_raw(width, height, bytes.to_vec())
        .ok_or("clipboard image RGBA data is invalid")?;
    let image =
        image::DynamicImage::ImageRgba8(rgba).thumbnail(MAX_IMAGE_DIMENSION, MAX_IMAGE_DIMENSION);
    let dir = clipboard_image_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|error| format!("cannot create clipboard image directory: {error}"))?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let path = dir.join(format!("clipboard-{}-{stamp}.png", std::process::id()));
    save_png_atomic(&image, &path)?;
    Ok(path)
}

fn save_png_atomic(image: &image::DynamicImage, path: &Path) -> Result<(), String> {
    let temporary = path.with_extension(format!("png.tmp-{}", std::process::id()));
    let result = (|| {
        let file = std::fs::File::create(&temporary)
            .map_err(|error| format!("cannot create clipboard image: {error}"))?;
        let mut writer = std::io::BufWriter::new(file);
        image
            .write_to(&mut writer, image::ImageFormat::Png)
            .map_err(|error| format!("cannot encode clipboard image: {error}"))?;
        writer
            .flush()
            .map_err(|error| format!("cannot flush clipboard image: {error}"))?;
        std::fs::rename(&temporary, path)
            .map_err(|error| format!("cannot finalize clipboard image: {error}"))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

/// Terminals cap how much OSC 52 data they accept (commonly ~74 KB after base64,
/// or the payload is silently dropped). Refuse anything that would clearly blow
/// that so a huge copy fails loudly rather than appearing to work.
const MAX_COPY_BYTES: usize = 64 * 1024;

/// Build the OSC 52 escape sequence that sets the system clipboard to `text`.
/// Returns `None` if the text is empty or too large to transmit reliably.
pub fn osc52_sequence(text: &str) -> Option<String> {
    if text.is_empty() || text.len() > MAX_COPY_BYTES {
        return None;
    }
    let b64 = base64::engine::general_purpose::STANDARD.encode(text.as_bytes());
    // ESC ] 52 ; c ; <base64> BEL — target `c` is the clipboard selection.
    Some(format!("\x1b]52;c;{}\x07", b64))
}

/// Write `text` to the system clipboard via OSC 52. Returns whether the sequence
/// was emitted (false when the text was empty or over the size cap).
pub fn copy(text: &str) -> bool {
    match osc52_sequence(text) {
        Some(seq) => {
            let mut out = std::io::stdout().lock();
            if let Err(error) = out.write_all(seq.as_bytes()).and_then(|_| out.flush()) {
                crate::app::toast::error(format!("Failed to copy to clipboard: {}", error));
                return false;
            }
            true
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sequence_wraps_base64_payload() {
        let seq = osc52_sequence("hi").unwrap();
        // "hi" → base64 "aGk=", framed by the OSC 52 clipboard escape.
        assert_eq!(seq, "\x1b]52;c;aGk=\x07");
    }

    #[test]
    fn rejects_empty_and_oversized() {
        assert!(osc52_sequence("").is_none());
        let huge = "x".repeat(MAX_COPY_BYTES + 1);
        assert!(osc52_sequence(&huge).is_none());
        // Right at the cap is still allowed.
        assert!(osc52_sequence(&"x".repeat(MAX_COPY_BYTES)).is_some());
    }

    #[test]
    fn clipboard_rgba_is_saved_as_bounded_png() {
        let bytes = vec![255_u8; 2048 * 1024 * 4];
        let path = save_rgba_image(2048, 1024, &bytes).unwrap();
        let decoded = image::open(&path).unwrap();
        assert_eq!(decoded.width(), 1024);
        assert_eq!(decoded.height(), 512);
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn clipboard_rgba_rejects_invalid_dimensions_and_length() {
        assert!(save_rgba_image(0, 1, &[]).is_err());
        assert!(save_rgba_image(2, 2, &[0; 15]).is_err());
        assert!(save_rgba_image(usize::MAX, 2, &[]).is_err());
    }

    #[test]
    fn managed_cleanup_never_removes_arbitrary_user_files() {
        let user_path =
            std::env::temp_dir().join(format!("aitui-user-file-{}.png", std::process::id()));
        std::fs::write(&user_path, b"keep").unwrap();
        remove_managed_image(&user_path);
        assert!(user_path.exists());
        std::fs::remove_file(user_path).unwrap();

        let path = save_rgba_image(1, 1, &[1, 2, 3, 255]).unwrap();
        remove_managed_image(&path);
        assert!(!path.exists());
    }

    #[test]
    fn multibyte_text_encodes() {
        // Non-ASCII must round-trip through base64 without panicking.
        let seq = osc52_sequence("café ☕").unwrap();
        assert!(seq.starts_with("\x1b]52;c;"));
        assert!(seq.ends_with('\x07'));
    }
}
