//! System-clipboard copy via the OSC 52 terminal escape. OSC 52 works across
//! most modern terminals *and over SSH* (the terminal, not the host, owns the
//! clipboard), and needs no external binary — unlike `xclip`/`pbcopy`. The raw
//! escape is written by the renderer (see `ui::flush_clipboard`) so all direct
//! stdout writes stay in the UI layer.

use std::io::Write;

use base64::Engine;

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
            let _ = out.write_all(seq.as_bytes());
            let _ = out.flush();
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
    fn multibyte_text_encodes() {
        // Non-ASCII must round-trip through base64 without panicking.
        let seq = osc52_sequence("café ☕").unwrap();
        assert!(seq.starts_with("\x1b]52;c;"));
        assert!(seq.ends_with('\x07'));
    }
}
