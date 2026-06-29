use std::path::Path;

/// Read a text file, returning its contents as a String.
pub fn read_text(path: &Path) -> anyhow::Result<String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("Cannot read {}: {}", path.display(), e))?;
    Ok(content)
}
