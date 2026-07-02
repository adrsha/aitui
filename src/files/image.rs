use std::path::Path;

const IMAGE_EXTS: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp"];

/// Returns true if the path looks like an image file.
pub fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| IMAGE_EXTS.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
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
