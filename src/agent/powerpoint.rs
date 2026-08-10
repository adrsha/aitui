//! Bundled implementation support for the native `specialized(powerpoint)` tool.
//!
//! The generator is authored in Python because `python-pptx` and `lxml` provide
//! the required OOXML support. AiTUI embeds those source files in its Rust binary
//! and materializes them into a process-local temporary package at execution
//! time. This keeps the implementation owned by AiTUI and avoids relying on a
//! repository checkout or a separately installed `animated_pptx` package.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

mod native;

pub use native::{inspect as inspect_native, open_save as open_save_native};

static MATERIALIZE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

const PACKAGE_FILES: &[(&str, &str)] = &[
    (
        "__init__.py",
        include_str!("powerpoint/animated_pptx/__init__.py"),
    ),
    (
        "animator.py",
        include_str!("powerpoint/animated_pptx/animator.py"),
    ),
    (
        "builder.py",
        include_str!("powerpoint/animated_pptx/builder.py"),
    ),
    ("cli.py", include_str!("powerpoint/animated_pptx/cli.py")),
    (
        "editor.py",
        include_str!("powerpoint/animated_pptx/editor.py"),
    ),
    (
        "generator.py",
        include_str!("powerpoint/animated_pptx/generator.py"),
    ),
    (
        "inspect.py",
        include_str!("powerpoint/animated_pptx/inspect.py"),
    ),
    (
        "model.py",
        include_str!("powerpoint/animated_pptx/model.py"),
    ),
    (
        "package_editor.py",
        include_str!("powerpoint/animated_pptx/package_editor.py"),
    ),
    (
        "validator.py",
        include_str!("powerpoint/animated_pptx/validator.py"),
    ),
];

/// Materialize the Python package embedded in the AiTUI executable and return
/// the directory that should be placed on `PYTHONPATH`.
pub fn materialize_embedded_package() -> Result<PathBuf, String> {
    let lock = MATERIALIZE_LOCK.get_or_init(|| Mutex::new(()));
    let _guard = lock
        .lock()
        .map_err(|_| "embedded package materialization lock is poisoned".to_string())?;
    let root = std::env::temp_dir().join(format!(
        "aitui-powerpoint-{}-{}",
        env!("CARGO_PKG_VERSION"),
        std::process::id()
    ));
    let package = root.join("animated_pptx");
    fs::create_dir_all(&package)
        .map_err(|error| format!("cannot create embedded package directory: {error}"))?;
    for (name, contents) in PACKAGE_FILES {
        write_if_changed(&package.join(name), contents.as_bytes())?;
    }
    Ok(root)
}

fn write_if_changed(path: &Path, contents: &[u8]) -> Result<(), String> {
    if matches!(fs::read(path), Ok(existing) if existing == contents) {
        return Ok(());
    }
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, contents)
        .map_err(|error| format!("cannot write {}: {error}", temporary.display()))?;
    fs::rename(&temporary, path)
        .map_err(|error| format!("cannot install {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_package_contains_cli_and_generator() {
        let root = materialize_embedded_package().unwrap();
        assert!(root.join("animated_pptx/__init__.py").is_file());
        assert!(root.join("animated_pptx/cli.py").is_file());
        assert!(root.join("animated_pptx/editor.py").is_file());
        assert!(root.join("animated_pptx/generator.py").is_file());
        assert!(root.join("animated_pptx/inspect.py").is_file());
        assert!(root.join("animated_pptx/package_editor.py").is_file());
    }
}
