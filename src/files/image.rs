use std::io::BufReader;
use std::path::Path;
use std::process::{Command, Stdio};

use image::AnimationDecoder;
use serde_json::{json, Value};

const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp"];
const VIDEO_EXTS: &[&str] = &["mp4", "webm", "mov", "mkv", "avi", "m4v"];
const PIXEL_MAX_DIMENSION: u32 = 32;
const PIXEL_SEGMENT_ROWS: u32 = 8;
const MAX_MEDIA_FRAMES: usize = 8;

/// Returns true if the path looks like an image file.
pub fn is_image(path: &Path) -> bool {
    extension_in(path, IMAGE_EXTS)
}

/// Returns true for image, animated image, and common video containers.
pub fn is_media(path: &Path) -> bool {
    is_image(path) || extension_in(path, VIDEO_EXTS)
}

fn extension_in(path: &Path, extensions: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| extensions.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

/// Decode media into bounded, model-readable RGBA pixel data. `offset` is a
/// zero-based frame offset and `limit` is a frame count. Images have one frame;
/// GIFs preserve animation frames; videos are sampled at one frame per second.
pub fn read_media_pixels(
    path: &Path,
    offset: Option<usize>,
    limit: Option<usize>,
) -> Result<String, String> {
    let start = offset.unwrap_or(0);
    let count = limit.unwrap_or(4).clamp(1, MAX_MEDIA_FRAMES);
    let value = if extension_in(path, VIDEO_EXTS) {
        read_video_pixels(path, start, count)?
    } else if path
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case("gif"))
    {
        read_gif_pixels(path, start, count)?
    } else {
        read_image_pixels(path, start)?
    };
    serde_json::to_string_pretty(&value).map_err(|error| error.to_string())
}

fn read_image_pixels(path: &Path, start: usize) -> Result<Value, String> {
    if start > 0 {
        return Err("Image has only frame 0; use offset=0 or omit offset".into());
    }
    let image = image::open(path)
        .map_err(|error| format!("Cannot decode image {}: {}", path.display(), error))?;
    Ok(media_json("image", path, vec![(0, None, image)]))
}

fn read_gif_pixels(path: &Path, start: usize, count: usize) -> Result<Value, String> {
    let file = std::fs::File::open(path)
        .map_err(|error| format!("Cannot read GIF {}: {}", path.display(), error))?;
    let decoder = image::codecs::gif::GifDecoder::new(BufReader::new(file))
        .map_err(|error| format!("Cannot decode GIF {}: {}", path.display(), error))?;
    let frames = decoder
        .into_frames()
        .collect_frames()
        .map_err(|error| format!("Cannot decode GIF frames: {}", error))?;
    let selected = frames
        .into_iter()
        .enumerate()
        .skip(start)
        .take(count)
        .map(|(index, frame)| {
            let (numerator, denominator) = frame.delay().numer_denom_ms();
            let delay_ms = numerator.checked_div(denominator).unwrap_or(0);
            (
                index,
                Some(delay_ms),
                image::DynamicImage::ImageRgba8(frame.into_buffer()),
            )
        })
        .collect();
    Ok(media_json("gif", path, selected))
}

fn read_video_pixels(path: &Path, start: usize, count: usize) -> Result<Value, String> {
    let dimensions = Command::new("ffprobe")
        .args([
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=width,height",
            "-of",
            "csv=s=x:p=0",
        ])
        .arg(path)
        .output()
        .map_err(|error| format!("Cannot run ffprobe: {}", error))?;
    if !dimensions.status.success() {
        return Err("ffprobe could not inspect the video".into());
    }
    let dimensions = String::from_utf8_lossy(&dimensions.stdout);
    let (source_width, source_height) = dimensions
        .trim()
        .split_once('x')
        .and_then(|(width, height)| Some((width.parse::<u32>().ok()?, height.parse::<u32>().ok()?)))
        .ok_or("ffprobe returned invalid video dimensions")?;
    let (width, height) = thumbnail_dimensions(source_width, source_height);
    let filter = format!("fps=1,scale={width}:{height}:flags=area");
    let mut child = Command::new("ffmpeg")
        .args(["-v", "error", "-ss", &start.to_string(), "-i"])
        .arg(path)
        .args([
            "-vf",
            &filter,
            "-frames:v",
            &count.to_string(),
            "-f",
            "rawvideo",
            "-pix_fmt",
            "rgba",
            "pipe:1",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|error| format!("Cannot run ffmpeg: {}", error))?;
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(
        child.stdout.as_mut().ok_or("ffmpeg stdout unavailable")?,
        &mut bytes,
    )
    .map_err(|error| format!("Cannot read ffmpeg pixels: {}", error))?;
    let status = child.wait().map_err(|error| error.to_string())?;
    if !status.success() {
        return Err("ffmpeg could not decode video frames".into());
    }
    let frame_len = width as usize * height as usize * 4;
    let frames = bytes
        .chunks_exact(frame_len)
        .enumerate()
        .map(|(index, rgba)| {
            let image = image::RgbaImage::from_raw(width, height, rgba.to_vec())
                .expect("raw frame length was validated");
            (
                start + index,
                Some(1000),
                image::DynamicImage::ImageRgba8(image),
            )
        })
        .collect();
    Ok(media_json("video", path, frames))
}

fn media_json(
    kind: &str,
    path: &Path,
    frames: Vec<(usize, Option<u32>, image::DynamicImage)>,
) -> Value {
    let frames: Vec<Value> = frames
        .into_iter()
        .map(|(index, duration_ms, image)| {
            let image =
                if image.width() > PIXEL_MAX_DIMENSION || image.height() > PIXEL_MAX_DIMENSION {
                    image.thumbnail(PIXEL_MAX_DIMENSION, PIXEL_MAX_DIMENSION)
                } else {
                    image
                };
            let rgba = image.to_rgba8();
            let (width, height) = rgba.dimensions();
            let segments: Vec<Value> = (0..height)
                .step_by(PIXEL_SEGMENT_ROWS as usize)
                .map(|row_start| {
                    let row_end = (row_start + PIXEL_SEGMENT_ROWS).min(height);
                    let mut pixels =
                        Vec::with_capacity(((row_end - row_start) * width * 4) as usize);
                    for y in row_start..row_end {
                        for x in 0..width {
                            pixels.extend_from_slice(&rgba.get_pixel(x, y).0);
                        }
                    }
                    json!({"rows": [row_start, row_end], "rgba": pixels})
                })
                .collect();
            json!({
                "index": index,
                "duration_ms": duration_ms,
                "width": width,
                "height": height,
                "pixel_format": "RGBA8",
                "segments": segments
            })
        })
        .collect();
    json!({
        "media_type": kind,
        "path": path.display().to_string(),
        "note": format!("Pixels are aspect-preserving thumbnails capped at {PIXEL_MAX_DIMENSION}x{PIXEL_MAX_DIMENSION}; segments contain flattened RGBA8 values grouped by row ranges."),
        "frames": frames
    })
}

fn thumbnail_dimensions(width: u32, height: u32) -> (u32, u32) {
    if width <= PIXEL_MAX_DIMENSION && height <= PIXEL_MAX_DIMENSION {
        return (width.max(1), height.max(1));
    }
    let scale =
        (PIXEL_MAX_DIMENSION as f64 / width as f64).min(PIXEL_MAX_DIMENSION as f64 / height as f64);
    (
        (width as f64 * scale).round().max(1.0) as u32,
        (height as f64 * scale).round().max(1.0) as u32,
    )
}

/// Load an image file, resize it to a reasonable maximum, and return
/// (base64_encoded_bytes, mime_type).
pub fn load_image_base64(path: &Path) -> anyhow::Result<(String, String)> {
    use base64::Engine;

    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("png")
        .to_lowercase();

    let mime = match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => "image/png",
    };

    // Load and optionally downscale using the `image` crate.
    let img = image::open(path)
        .map_err(|e| anyhow::anyhow!("Cannot open image {}: {}", path.display(), e))?;

    // Cap at 1024×1024 to keep token costs reasonable.
    let img = img.thumbnail(1024, 1024);

    let mut buf = Vec::new();
    let format = match mime {
        "image/jpeg" => image::ImageFormat::Jpeg,
        "image/gif" => image::ImageFormat::Gif,
        "image/webp" => image::ImageFormat::WebP,
        _ => image::ImageFormat::Png,
    };
    img.write_to(&mut std::io::Cursor::new(&mut buf), format)
        .map_err(|e| anyhow::anyhow!("Failed to encode image: {}", e))?;

    let b64 = base64::engine::general_purpose::STANDARD.encode(&buf);
    Ok((b64, mime.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_detection_is_case_insensitive() {
        assert!(is_image(Path::new("photo.JPEG")));
        assert!(is_media(Path::new("clip.MP4")));
        assert!(!is_media(Path::new("notes.txt")));
    }

    #[test]
    fn thumbnail_dimensions_preserve_aspect_ratio_and_bounds() {
        assert_eq!(thumbnail_dimensions(16, 8), (16, 8));
        assert_eq!(thumbnail_dimensions(64, 32), (32, 16));
        assert_eq!(thumbnail_dimensions(32, 64), (16, 32));
    }

    #[test]
    fn image_pixels_are_bounded_and_segmented() {
        let path = std::env::temp_dir().join(format!(
            "aitui-image-pixels-{}-{}.png",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        let image = image::RgbaImage::from_pixel(64, 16, image::Rgba([1, 2, 3, 255]));
        image.save(&path).unwrap();

        let output = read_media_pixels(&path, None, None).unwrap();
        let value: Value = serde_json::from_str(&output).unwrap();
        let frame = &value["frames"][0];
        assert_eq!(value["media_type"], "image");
        assert_eq!(frame["width"], 32);
        assert_eq!(frame["height"], 8);
        assert_eq!(frame["segments"].as_array().unwrap().len(), 1);
        assert_eq!(
            frame["segments"][0]["rgba"].as_array().unwrap().len(),
            32 * 8 * 4
        );

        std::fs::remove_file(path).unwrap();
    }
}
