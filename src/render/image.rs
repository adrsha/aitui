use std::collections::BTreeSet;
use std::io::Write;
use std::path::Path;

use base64::Engine;
use image::imageops::FilterType;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphicsProtocol {
    Kitty,
    Sixel,
}

pub fn graphics_protocol() -> Option<GraphicsProtocol> {
    let term = std::env::var("TERM").unwrap_or_default().to_lowercase();
    let term_program = std::env::var("TERM_PROGRAM")
        .unwrap_or_default()
        .to_lowercase();
    if std::env::var_os("KITTY_WINDOW_ID").is_some()
        || std::env::var_os("WEZTERM_PANE").is_some()
        || term.contains("kitty")
        || term.contains("wezterm")
        || term.contains("ghostty")
        || term_program.contains("wezterm")
        || term_program.contains("ghostty")
    {
        return Some(GraphicsProtocol::Kitty);
    }

    let term_features = std::env::var("TERM_FEATURES")
        .unwrap_or_default()
        .to_lowercase();
    if std::env::var_os("SIXEL_SUPPORT").is_some()
        || term.contains("sixel")
        || term.contains("mlterm")
        || term.contains("yaft")
        || term_program.contains("contour")
        || term_program.contains("mintty")
        || term_features.contains("sixel")
    {
        return Some(GraphicsProtocol::Sixel);
    }
    None
}

/// The 8-byte PNG signature.
const PNG_MAGIC: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];

pub fn is_png(bytes: &[u8]) -> bool {
    bytes.len() >= 8 && bytes[..8] == PNG_MAGIC
}

pub fn display_image(path: &Path, col: u16, row: u16, cols: u16, rows: u16) -> Result<(), String> {
    let protocol =
        graphics_protocol().ok_or_else(|| "No supported terminal graphics protocol".to_string())?;
    let bytes = std::fs::read(path).map_err(|e| format!("Cannot read image: {}", e))?;
    if bytes.is_empty() {
        return Err("Empty image file".into());
    }

    let mut out = std::io::stdout().lock();
    out.write_all(b"\x1b7").map_err(|e| e.to_string())?;
    write!(out, "\x1b[{};{}H", row + 1, col + 1).map_err(|e| e.to_string())?;
    let result = match protocol {
        GraphicsProtocol::Kitty => write_kitty(&mut out, &bytes, cols, rows),
        GraphicsProtocol::Sixel => write_sixel(&mut out, &bytes, cols, rows),
    };
    out.write_all(b"\x1b8").map_err(|e| e.to_string())?;
    result?;
    out.flush().map_err(|e| e.to_string())
}

fn write_kitty(out: &mut impl Write, bytes: &[u8], cols: u16, rows: u16) -> Result<(), String> {
    if !is_png(bytes) {
        return Err("Not a PNG image (Kitty preview requires PNG)".into());
    }
    write!(out, "\x1b_Ga=d\x1b\\").map_err(|e| e.to_string())?;
    let encoded = base64::engine::general_purpose::STANDARD.encode(bytes);
    let chunks: Vec<&[u8]> = encoded.as_bytes().chunks(4096).collect();
    for (index, chunk) in chunks.iter().enumerate() {
        let more = usize::from(index + 1 < chunks.len());
        if index == 0 {
            write!(out, "\x1b_Ga=T,f=100,c={},r={},m={};", cols, rows, more)
                .map_err(|e| e.to_string())?;
        } else {
            write!(out, "\x1b_Gm={};", more).map_err(|e| e.to_string())?;
        }
        out.write_all(chunk).map_err(|e| e.to_string())?;
        out.write_all(b"\x1b\\").map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn write_sixel(out: &mut impl Write, bytes: &[u8], cols: u16, rows: u16) -> Result<(), String> {
    let image =
        image::load_from_memory(bytes).map_err(|e| format!("Cannot decode image: {}", e))?;
    let max_w = u32::from(cols.max(1)).saturating_mul(8).min(800);
    let max_h = u32::from(rows.max(1)).saturating_mul(16).min(800);
    let image = image.resize(max_w, max_h, FilterType::Triangle).to_rgb8();
    let (width, height) = image.dimensions();
    let indices: Vec<u8> = image
        .pixels()
        .map(|pixel| quantize(pixel[0], pixel[1], pixel[2]))
        .collect();
    let colors: BTreeSet<u8> = indices.iter().copied().collect();

    write!(out, "\x1bPq\"1;1;{};{}", width, height).map_err(|e| e.to_string())?;
    for color in colors {
        let (r, g, b) = palette_rgb(color);
        write!(out, "#{};2;{};{};{}", color, r, g, b).map_err(|e| e.to_string())?;
    }

    for band_y in (0..height).step_by(6) {
        let mut band_colors = BTreeSet::new();
        for y in band_y..(band_y + 6).min(height) {
            let start = (y * width) as usize;
            band_colors.extend(indices[start..start + width as usize].iter().copied());
        }
        for color in band_colors {
            write!(out, "#{}", color).map_err(|e| e.to_string())?;
            let mut sixels = Vec::with_capacity(width as usize);
            for x in 0..width {
                let mut bits = 0u8;
                for bit in 0..6 {
                    let y = band_y + bit;
                    if y < height && indices[(y * width + x) as usize] == color {
                        bits |= 1 << bit;
                    }
                }
                sixels.push(bits + 63);
            }
            while sixels.last() == Some(&b'?') {
                sixels.pop();
            }
            write_sixel_runs(out, &sixels)?;
            out.write_all(b"$").map_err(|e| e.to_string())?;
        }
        out.write_all(b"-").map_err(|e| e.to_string())?;
    }
    out.write_all(b"\x1b\\").map_err(|e| e.to_string())?;
    Ok(())
}

fn write_sixel_runs(out: &mut impl Write, bytes: &[u8]) -> Result<(), String> {
    let mut index = 0usize;
    while index < bytes.len() {
        let byte = bytes[index];
        let mut end = index + 1;
        while end < bytes.len() && bytes[end] == byte {
            end += 1;
        }
        let count = end - index;
        if count >= 4 {
            write!(out, "!{}{}", count, byte as char).map_err(|e| e.to_string())?;
        } else {
            for _ in 0..count {
                out.write_all(&[byte]).map_err(|e| e.to_string())?;
            }
        }
        index = end;
    }
    Ok(())
}

fn quantize(r: u8, g: u8, b: u8) -> u8 {
    (r / 51) * 36 + (g / 51) * 6 + b / 51
}

fn palette_rgb(index: u8) -> (u8, u8, u8) {
    let r = index / 36;
    let g = (index % 36) / 6;
    let b = index % 6;
    (r * 20, g * 20, b * 20)
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
        assert!(!is_png(&[0xff, 0xd8, 0xff, 0xe0]));
        assert!(!is_png(b"GIF89a"));
        assert!(!is_png(&[]));
        assert!(!is_png(&PNG_MAGIC[..7]));
    }

    #[test]
    fn kitty_payload_is_chunked() {
        let mut png = PNG_MAGIC.to_vec();
        png.extend(std::iter::repeat_n(1, 8_000));
        let mut out = Vec::new();
        write_kitty(&mut out, &png, 20, 10).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.matches("\x1b_G").count() >= 3);
        assert!(text.contains("m=1;"));
        assert!(text.contains("m=0;"));
    }

    #[test]
    fn sixel_run_length_encoding_compacts_repetition() {
        let mut out = Vec::new();
        write_sixel_runs(&mut out, b"????AAAA").unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "!4?!4A");
    }

    #[test]
    fn palette_round_trip_uses_sixel_percentages() {
        assert_eq!(quantize(255, 0, 0), 180);
        assert_eq!(palette_rgb(180), (100, 0, 0));
        assert_eq!(quantize(255, 255, 255), 215);
        assert_eq!(palette_rgb(215), (100, 100, 100));
    }
}
