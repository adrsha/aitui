use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use super::tools::{ToolCall, ToolResult};

/// Execute a tool call, returning the result.
/// `cwd` is the base directory for relative paths.
pub fn execute(call: ToolCall, cwd: &PathBuf) -> ToolResult {
    let start = Instant::now();
    let result = run(&call, cwd);
    let duration_ms = start.elapsed().as_millis() as u64;
    match result {
        Ok(output) => ToolResult::success(call, output, duration_ms),
        Err(err)   => ToolResult::failure(call, err, duration_ms),
    }
}

fn resolve_path(raw: &str, cwd: &PathBuf) -> PathBuf {
    let p = PathBuf::from(raw);
    if p.is_absolute() { p } else { cwd.join(p) }
}

fn run(call: &ToolCall, cwd: &PathBuf) -> Result<String, String> {
    match call.name.as_str() {
        "read_file" => {
            let path_str = call.args.get("path").and_then(|v| v.as_str())
                .ok_or("Missing 'path' argument")?;
            let path = resolve_path(path_str, cwd);
            fs::read_to_string(&path)
                .map_err(|e| format!("Cannot read {}: {}", path.display(), e))
        }

        "write_file" => {
            let path_str = call.args.get("path").and_then(|v| v.as_str())
                .ok_or("Missing 'path' argument")?;
            let content = call.args.get("content").and_then(|v| v.as_str())
                .ok_or("Missing 'content' argument")?;
            let path = resolve_path(path_str, cwd);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("Cannot create directories: {}", e))?;
            }
            fs::write(&path, content)
                .map_err(|e| format!("Cannot write {}: {}", path.display(), e))?;
            Ok(format!("Written {} bytes to {}", content.len(), path.display()))
        }

        "append_file" => {
            let path_str = call.args.get("path").and_then(|v| v.as_str())
                .ok_or("Missing 'path' argument")?;
            let content = call.args.get("content").and_then(|v| v.as_str())
                .ok_or("Missing 'content' argument")?;
            let path = resolve_path(path_str, cwd);
            let mut file = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .map_err(|e| format!("Cannot open {}: {}", path.display(), e))?;
            file.write_all(content.as_bytes())
                .map_err(|e| format!("Cannot append to {}: {}", path.display(), e))?;
            Ok(format!("Appended {} bytes to {}", content.len(), path.display()))
        }

        "list_dir" => {
            let path_str = call.args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let path = resolve_path(path_str, cwd);
            let entries = fs::read_dir(&path)
                .map_err(|e| format!("Cannot list {}: {}", path.display(), e))?;

            let mut lines: Vec<String> = Vec::new();
            let mut dirs: Vec<String> = Vec::new();
            let mut files: Vec<String> = Vec::new();

            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if entry.path().is_dir() {
                    dirs.push(format!("  📁 {}/", name));
                } else {
                    let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                    files.push(format!("  📄 {}  ({})", name, fmt_size(size)));
                }
            }

            dirs.sort();
            files.sort();
            lines.push(format!("📂 {}", path.display()));
            lines.extend(dirs);
            lines.extend(files);
            if lines.len() == 1 {
                lines.push("  (empty)".to_string());
            }
            Ok(lines.join("\n"))
        }

        "run_shell" => {
            let cmd = call.args.get("command").and_then(|v| v.as_str())
                .ok_or("Missing 'command' argument")?;
            let output = Command::new("sh")
                .arg("-c")
                .arg(cmd)
                .current_dir(cwd)
                .output()
                .map_err(|e| format!("Cannot run command: {}", e))?;

            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let exit_code = output.status.code().unwrap_or(-1);

            let mut result = String::new();
            if exit_code != 0 {
                result.push_str(&format!("[exit {}]\n", exit_code));
            }
            if !stdout.is_empty() {
                result.push_str(&stdout);
            }
            if !stderr.is_empty() {
                if !result.is_empty() { result.push('\n'); }
                result.push_str("[stderr]\n");
                result.push_str(&stderr);
            }
            if result.is_empty() {
                result = "(no output)".to_string();
            }
            Ok(truncate(result, 8192))
        }

        "search_files" => {
            let pattern = call.args.get("pattern").and_then(|v| v.as_str())
                .ok_or("Missing 'pattern' argument")?;
            let path_str = call.args.get("path").and_then(|v| v.as_str()).unwrap_or(".");
            let path = resolve_path(path_str, cwd);

            let mut matches: Vec<String> = Vec::new();
            search_recursive(&path, pattern, &path, &mut matches, 0);

            if matches.is_empty() {
                Ok(format!("No matches for '{}' in {}", pattern, path.display()))
            } else {
                let header = format!("{} match(es) for '{}':", matches.len(), pattern);
                let mut out = vec![header];
                out.extend(matches.into_iter().take(200));
                Ok(truncate(out.join("\n"), 8192))
            }
        }

        "edit_file" => {
            let path_str = call.args.get("path").and_then(|v| v.as_str())
                .ok_or("Missing 'path' argument")?;
            let old_s = call.args.get("old_string").and_then(|v| v.as_str())
                .ok_or("Missing 'old_string' argument")?;
            let new_s = call.args.get("new_string").and_then(|v| v.as_str())
                .ok_or("Missing 'new_string' argument")?;
            let path = resolve_path(path_str, cwd);
            let content = fs::read_to_string(&path)
                .map_err(|e| format!("Cannot read {}: {}", path.display(), e))?;
            if !content.contains(old_s) {
                return Err(format!("old_string not found in {}", path.display()));
            }
            let replaced = content.replace(old_s, new_s);
            fs::write(&path, &replaced)
                .map_err(|e| format!("Cannot write {}: {}", path.display(), e))?;
            let count = content.matches(old_s).count();
            Ok(format!("Edit {} ({} occurrence{})", path.display(), count, if count == 1 { "" } else { "s" }))
        }

        "delete_file" => {
            let path_str = call.args.get("path").and_then(|v| v.as_str())
                .ok_or("Missing 'path' argument")?;
            let path = resolve_path(path_str, cwd);
            if path.is_dir() {
                return Err(format!("{} is a directory; refusing to delete", path.display()));
            }
            fs::remove_file(&path)
                .map_err(|e| format!("Cannot delete {}: {}", path.display(), e))?;
            Ok(format!("Deleted {}", path.display()))
        }

        name => Err(format!("Unknown tool: {}", name)),
    }
}

fn search_recursive(
    dir: &Path,
    pattern: &str,
    base: &Path,
    matches: &mut Vec<String>,
    depth: usize,
) {
    if depth > 8 || matches.len() >= 200 {
        return;
    }
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        // Skip hidden and common non-text dirs
        if name.starts_with('.') || name == "target" || name == "node_modules" {
            continue;
        }
        if path.is_dir() {
            search_recursive(&path, pattern, base, matches, depth + 1);
        } else {
            if let Ok(content) = fs::read_to_string(&path) {
                for (line_no, line) in content.lines().enumerate() {
                    if line.to_lowercase().contains(&pattern.to_lowercase()) {
                        let rel = path.strip_prefix(base).unwrap_or(&path);
                        matches.push(format!("  {}:{}: {}", rel.display(), line_no + 1, line.trim()));
                    }
                }
            }
        }
    }
}

fn fmt_size(bytes: u64) -> String {
    if bytes < 1024 { format!("{}B", bytes) }
    else if bytes < 1024 * 1024 { format!("{:.1}KB", bytes as f64 / 1024.0) }
    else { format!("{:.1}MB", bytes as f64 / 1024.0 / 1024.0) }
}

fn truncate(s: String, max: usize) -> String {
    if s.len() <= max {
        s
    } else {
        format!("{}…\n[truncated {} bytes]", &s[..max], s.len() - max)
    }
}
