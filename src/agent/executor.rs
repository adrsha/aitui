use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{OnceLock, RwLock};
use std::time::Instant;

use base64::Engine;

use super::tools::{ToolCall, ToolResult};

#[derive(Debug, Clone)]
pub struct SearchSettings {
    pub provider: String,
    pub searxng_url: String,
}

impl Default for SearchSettings {
    fn default() -> Self {
        Self {
            provider: "searxng".to_string(),
            searxng_url: String::new(),
        }
    }
}

static SEARCH_SETTINGS: OnceLock<RwLock<SearchSettings>> = OnceLock::new();

pub fn configure_search(settings: SearchSettings) {
    let lock = SEARCH_SETTINGS.get_or_init(|| RwLock::new(SearchSettings::default()));
    if let Ok(mut guard) = lock.write() {
        *guard = settings;
    }
}

fn search_settings() -> SearchSettings {
    SEARCH_SETTINGS
        .get_or_init(|| RwLock::new(SearchSettings::default()))
        .read()
        .map(|g| g.clone())
        .unwrap_or_default()
}

/// Shared abort flag for an in-flight tool round. Set by `AgentCancel` so a
/// running tool (notably a shell) stops side effects promptly instead of only
/// having its result dropped.
pub type ToolAbort = std::sync::Arc<std::sync::atomic::AtomicBool>;

/// Execute a tool call, returning the result.
/// `cwd` is the base directory for relative paths.
pub fn execute(call: ToolCall, cwd: &Path) -> ToolResult {
    execute_abortable(
        call,
        cwd,
        &std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    )
}

/// Like [`execute`], but aborts promptly (with a "Cancelled by user" result)
/// once `abort` is set — shells are killed at their process group.
pub fn execute_abortable(mut call: ToolCall, cwd: &Path, abort: &ToolAbort) -> ToolResult {
    attach_display_metadata(&mut call, cwd);
    execute_with(call, |call| run(call, cwd, abort))
}

fn attach_display_metadata(call: &mut ToolCall, cwd: &Path) {
    use super::tools::ToolKind;

    if call.kind() != Some(ToolKind::Edit) {
        return;
    }
    let Some(path) = call.args.get("path").and_then(|value| value.as_str()) else {
        return;
    };
    let Some(old) = call
        .args
        .get("old")
        .or_else(|| call.args.get("old_string"))
        .and_then(|value| value.as_str())
    else {
        return;
    };
    let resolved = resolve_path(path, cwd);
    let Ok(content) = crate::agent::file_cache::read_file_content(&resolved) else {
        return;
    };
    let Some(byte_index) = content.find(old) else {
        return;
    };
    let line = content[..byte_index]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count()
        + 1;
    if let Some(args) = call.args.as_object_mut() {
        args.insert("__display_start_line".into(), serde_json::json!(line));
    }
}

fn execute_with(
    call: ToolCall,
    run_tool: impl FnOnce(&ToolCall) -> Result<String, String>,
) -> ToolResult {
    let start = Instant::now();
    let result = crate::tui::catch_recoverable_panic(|| run_tool(&call));
    let duration_ms = start.elapsed().as_millis() as u64;
    match result {
        Ok(Ok(output)) => ToolResult::success(call, output, duration_ms),
        Ok(Err(err)) => ToolResult::failure(call, err, duration_ms),
        Err(payload) => {
            let message = payload
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
                .unwrap_or("unknown panic");
            ToolResult::failure(
                call,
                format!("Tool failed unexpectedly: {}", message),
                duration_ms,
            )
        }
    }
}

fn resolve_path(raw: &str, cwd: &Path) -> PathBuf {
    let p = PathBuf::from(raw);
    if p.is_absolute() {
        p
    } else {
        cwd.join(p)
    }
}

fn run(call: &ToolCall, cwd: &Path, abort: &ToolAbort) -> Result<String, String> {
    use super::tools::ToolKind;

    if let Some(items) = call.expanded_calls()? {
        if items.is_empty() {
            return Err("Batch must contain at least one operation".into());
        }
        let total = items.len();
        let mut output = Vec::with_capacity(total);
        for (index, item) in items.into_iter().enumerate() {
            if abort.load(std::sync::atomic::Ordering::Relaxed) {
                output.push(format!(
                    "## {}/{} · cancelled\nCancelled by user",
                    index + 1,
                    total
                ));
                break;
            }
            let summary = item.summary();
            match run(&item, cwd, abort) {
                Ok(result) => output.push(format!(
                    "## {}/{} · {} · ok\n{}",
                    index + 1,
                    total,
                    summary,
                    result
                )),
                Err(error) => output.push(format!(
                    "## {}/{} · {} · error\n{}",
                    index + 1,
                    total,
                    summary,
                    error
                )),
            }
        }
        return Ok(output.join("\n\n"));
    }

    if abort.load(std::sync::atomic::Ordering::Relaxed) {
        return Err("Cancelled by user".into());
    }

    match call.kind() {
        Some(ToolKind::Read) => {
            let path_str = call
                .args
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or("Missing 'path' argument")?;
            let path = resolve_path(path_str, cwd);
            // Cache-first: a file read earlier in the process (and unchanged on
            // disk) is served from memory, so the agent's repeated reads of the
            // same file cost no IO.
            let (content, cached) = crate::agent::file_cache::read_file(&path)?;
            // Optional line window: offset = 1-based first line, limit = line count.
            let offset = usize_arg(call, "offset");
            let limit = usize_arg(call, "limit");
            let mut output = read_output(&content, path_str, offset, limit);
            if cached && !output.trim().is_empty() {
                output.push_str("\n\n[cached: file unchanged since last read]");
            }
            Ok(output)
        }

        Some(ToolKind::Write) => {
            // FIXME(audit): require an explicit overwrite acknowledgement for existing
            // files, or route updates through `edit`, before broadening write approvals.
            let path_str = call
                .args
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or("Missing 'path' argument")?;
            let content = call
                .args
                .get("content")
                .and_then(|v| v.as_str())
                .ok_or("Missing 'content' argument")?;
            let path = resolve_path(path_str, cwd);
            // Capture the old content first so an update can show a diff.
            let old = crate::agent::file_cache::read_file_content(&path).ok();
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("Cannot create directories: {}", e))?;
            }
            fs::write(&path, content)
                .map_err(|e| format!("Cannot write {}: {}", path.display(), e))?;
            crate::agent::file_cache::store(&path, content);
            match old {
                Some(old) => Ok(format!(
                    "Updated {}\n{}",
                    path.display(),
                    line_diff(&old, content)
                )),
                None => Ok(format!(
                    "Created {} ({} lines)",
                    path.display(),
                    content.lines().count()
                )),
            }
        }

        Some(ToolKind::List) => {
            let path_str = call
                .args
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or(".");
            let path = resolve_path(path_str, cwd);
            // depth: how many levels to descend (1 = just this dir, the default).
            let depth = call
                .args
                .get("depth")
                .and_then(|v| v.as_u64())
                .unwrap_or(1)
                .max(1) as usize;
            let mut lines: Vec<String> = vec![format!("dir {}", path.display())];
            let mut count = 0usize;
            list_recursive(&path, depth, 1, &mut lines, &mut count);
            if lines.len() == 1 {
                lines.push("  (empty)".to_string());
            }
            if count >= LIST_CAP {
                lines.push(format!(
                    "  … (capped at {} entries — narrow the path or lower depth)",
                    LIST_CAP
                ));
            }
            Ok(lines.join("\n"))
        }

        Some(ToolKind::Shell) => {
            let cmd = call
                .args
                .get("command")
                .and_then(|v| v.as_str())
                .ok_or("Missing 'command' argument")?;
            let output = run_shell_command(cmd, cwd, abort)?;

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
                if !result.is_empty() {
                    result.push('\n');
                }
                result.push_str("[stderr]\n");
                result.push_str(&stderr);
            }
            if result.is_empty() {
                result = "(no output)".to_string();
            }
            Ok(truncate(result, 8192))
        }

        Some(ToolKind::Search) => crate::agent::file_search::execute(call, cwd),

        Some(ToolKind::MakeDir) => {
            let path_str = call
                .args
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or("Missing 'path' argument")?;
            let path = resolve_path(path_str, cwd);
            fs::create_dir_all(&path)
                .map_err(|e| format!("Cannot create {}: {}", path.display(), e))?;
            Ok(format!("Created directory {}", path.display()))
        }

        Some(ToolKind::Edit) => {
            // TODO(audit): make edit writes atomic and preserve file metadata where
            // practical; the current read/replace/write path can leave partial files.
            let path_str = call
                .args
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or("Missing 'path' argument")?;
            let old_s = call
                .args
                .get("old")
                .or_else(|| call.args.get("old_string"))
                .and_then(|v| v.as_str())
                .ok_or("edit: missing required 'old' argument")?;
            let new_s = call
                .args
                .get("new")
                .or_else(|| call.args.get("new_string"))
                .and_then(|v| v.as_str())
                .ok_or("edit: missing required 'new' argument")?;
            let path = resolve_path(path_str, cwd);
            let content = crate::agent::file_cache::read_file_content(&path)
                .map_err(|e| format!("Cannot read {}: {}", path.display(), e))?;
            let count = content.matches(old_s).count();
            match count {
                0 => return Err(format!("old_string not found in {}", path.display())),
                1 => {}
                n => {
                    return Err(format!(
                        "old_string matched {} occurrences in {}; include a larger unique snippet from one location. Matching locations:\n{}",
                        n,
                        path.display(),
                        duplicate_edit_context(&content, old_s)
                    ))
                }
            }
            let replaced = content.replacen(old_s, new_s, 1);
            fs::write(&path, &replaced)
                .map_err(|e| format!("Cannot write {}: {}", path.display(), e))?;
            crate::agent::file_cache::store(&path, &replaced);
            Ok(format!(
                "Edit {} (1 occurrence)\n{}",
                path.display(),
                line_diff(&content, &replaced)
            ))
        }

        Some(ToolKind::Delete) => {
            // One delete for both: detect file vs directory tree.
            let path_str = call
                .args
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or("Missing 'path' argument")?;
            let path = resolve_path(path_str, cwd);
            if !path.exists() {
                return Err(format!("Not found: {}", path.display()));
            }
            if path.is_dir() {
                fs::remove_dir_all(&path)
                    .map_err(|e| format!("Cannot delete {}: {}", path.display(), e))?;
                Ok(format!("Removed {}/ (directory)", path.display()))
            } else {
                fs::remove_file(&path)
                    .map_err(|e| format!("Cannot delete {}: {}", path.display(), e))?;
                crate::agent::file_cache::invalidate(&path);
                Ok(format!("Removed {}", path.display()))
            }
        }

        Some(ToolKind::Move) => {
            let (from, to) = from_to(call, cwd)?;
            if !from.exists() {
                return Err(format!("Source not found: {}", from.display()));
            }
            if let Some(parent) = to.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("Cannot create target dir: {}", e))?;
            }
            // Try a fast rename; fall back to copy+remove across filesystems.
            match fs::rename(&from, &to) {
                Ok(()) => {
                    crate::agent::file_cache::invalidate(&from);
                    Ok(format!("Moved {} → {}", from.display(), to.display()))
                }
                Err(_) => {
                    copy_recursive(&from, &to)?;
                    remove_any(&from)?;
                    crate::agent::file_cache::invalidate(&from);
                    Ok(format!("Moved {} → {}", from.display(), to.display()))
                }
            }
        }

        Some(ToolKind::Copy) => {
            let (from, to) = from_to(call, cwd)?;
            if !from.exists() {
                return Err(format!("Source not found: {}", from.display()));
            }
            if let Some(parent) = to.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("Cannot create target dir: {}", e))?;
            }
            copy_recursive(&from, &to)?;
            Ok(format!("Copied {} → {}", from.display(), to.display()))
        }

        Some(ToolKind::WebSearch) => {
            let query = call
                .args
                .get("query")
                .or_else(|| call.args.get("q"))
                .and_then(|v| v.as_str())
                .ok_or("Missing 'query' argument")?;
            web_search(query)
        }

        Some(ToolKind::WebImages) => {
            let query = call
                .args
                .get("query")
                .or_else(|| call.args.get("q"))
                .and_then(|v| v.as_str())
                .ok_or("Missing 'query' argument")?;
            web_image_search(query)
        }

        Some(ToolKind::ReverseImage) => reverse_image_search(call, cwd),

        Some(ToolKind::WebFetch) => {
            let url = call
                .args
                .get("url")
                .and_then(|v| v.as_str())
                .ok_or("Missing 'url' argument")?;
            let body = http_get_text(url)?;
            let text = truncate(strip_html(&body), 8192);
            if text.trim().is_empty() {
                // A blank "(ok)" reads as success to the model and it retries forever.
                // Say plainly that there was no readable text.
                Ok(format!(
                    "Fetched {} but found no readable text — likely a JavaScript-rendered \
                     page. Use web_search to find a direct article URL, or try a different page.",
                    url
                ))
            } else {
                Ok(text)
            }
        }

        Some(ToolKind::Download) => {
            let url = call
                .args
                .get("url")
                .and_then(|v| v.as_str())
                .ok_or("Missing 'url' argument")?;
            let path_str = call
                .args
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or("Missing 'path' argument")?;
            let path = resolve_path(path_str, cwd);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("Cannot create target dir: {}", e))?;
            }
            let (bytes, content_type) = http_get_bytes(url)?;
            if content_type.starts_with("text/html") {
                return Err(format!(
                    "Refusing to save an HTML page as an asset (Content-Type: {}). Use the direct image or file URL instead.",
                    content_type
                ));
            }
            let n = bytes.len();
            fs::write(&path, &bytes)
                .map_err(|e| format!("Cannot write {}: {}", path.display(), e))?;
            crate::agent::file_cache::invalidate(&path);
            Ok(format!(
                "Downloaded {} bytes ({}) → {}",
                n,
                if content_type.is_empty() {
                    "unknown content type"
                } else {
                    &content_type
                },
                path.display()
            ))
        }

        Some(ToolKind::PowerPoint) => generate_powerpoint(call, cwd),

        // These are intercepted by the app layer and never reach the executor;
        // handled here only for match exhaustiveness.
        Some(ToolKind::Todo) => Ok("(todo handled by UI)".into()),
        Some(ToolKind::Ask) => Ok("(ask handled by UI)".into()),
        Some(ToolKind::Plan) => Ok("(plan handled by UI)".into()),
        Some(ToolKind::ProposeStep) => Ok("(propose_step handled by UI)".into()),
        Some(ToolKind::Task) => Ok("(task handled by UI)".into()),
        Some(ToolKind::Finish) => Ok("(finish handled by UI)".into()),

        None => Err(format!("Unknown tool: {}", call.name)),
    }
}

fn generate_powerpoint(call: &ToolCall, cwd: &Path) -> Result<String, String> {
    let operation = call
        .args
        .get("operation")
        .and_then(|value| value.as_str())
        .unwrap_or("create");
    if !matches!(
        operation,
        "create" | "replace" | "append" | "edit" | "inspect"
    ) {
        return Err(
            "powerpoint: operation must be create, replace, append, edit, or inspect".into(),
        );
    }
    let output_path = call
        .args
        .get("output_path")
        .and_then(|value| value.as_str());
    if operation != "inspect" {
        let output_path = output_path.ok_or("powerpoint: missing 'output_path'")?;
        if !output_path.to_ascii_lowercase().ends_with(".pptx") {
            return Err("powerpoint: output_path must end in .pptx".into());
        }
    }
    if operation == "inspect" {
        let input_path = call
            .args
            .get("input_path")
            .and_then(|value| value.as_str())
            .ok_or("powerpoint: inspect requires 'input_path'")?;
        if !input_path.to_ascii_lowercase().ends_with(".pptx") {
            return Err("powerpoint: input_path must end in .pptx".into());
        }
    }
    if matches!(operation, "create" | "replace" | "append")
        && !call
            .args
            .get("slides")
            .is_some_and(|value| value.is_array())
    {
        return Err(format!("powerpoint: {operation} requires a 'slides' array"));
    }
    if operation == "edit" {
        let has_modifiers = call
            .args
            .get("modifiers")
            .is_some_and(|value| value.is_array());
        let has_package_modifiers = call
            .args
            .get("package_modifiers")
            .is_some_and(|value| value.is_array());
        if !has_modifiers && !has_package_modifiers {
            return Err(
                "powerpoint: edit requires a 'modifiers' or 'package_modifiers' array".into(),
            );
        }
    }
    let destination = output_path.map(|path| resolve_path(path, cwd));
    let mut request = call.args.clone();
    let Some(request) = request.as_object_mut() else {
        return Err("powerpoint: arguments must be an object".into());
    };
    request.remove("action");
    if let Some(input_path) = request
        .get("input_path")
        .and_then(|value| value.as_str())
        .map(|path| resolve_path(path, cwd))
    {
        request.insert(
            "input_path".into(),
            serde_json::Value::String(input_path.to_string_lossy().to_string()),
        );
    }
    if let Some(destination) = destination.as_ref() {
        request.insert(
            "output_path".into(),
            serde_json::Value::String(destination.to_string_lossy().to_string()),
        );
    }
    let payload = serde_json::to_vec(request)
        .map_err(|error| format!("powerpoint: cannot serialize request: {error}"))?;

    let embedded_package = crate::agent::powerpoint::materialize_embedded_package()
        .map_err(|error| format!("powerpoint: {error}"))?;
    let mut candidates = Vec::new();
    if let Some(configured) = std::env::var_os("AITUI_POWERPOINT_PYTHON") {
        candidates.push(PathBuf::from(configured));
    }
    candidates.push(cwd.join(".venv/bin/python"));
    if let Ok(launch_directory) = std::env::current_dir() {
        let launch_python = launch_directory.join(".venv/bin/python");
        if !candidates.contains(&launch_python) {
            candidates.push(launch_python);
        }
    }
    candidates.extend([PathBuf::from("python3"), PathBuf::from("python")]);
    let mut last_error = String::new();
    for python in candidates {
        let mut command = Command::new(&python);
        command
            .arg("-m")
            .arg("animated_pptx.cli")
            .current_dir(cwd)
            .env("PYTHONPATH", &embedded_package)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                last_error = error.to_string();
                continue;
            }
        };
        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(&payload)
                .map_err(|error| format!("powerpoint: cannot send request: {error}"))?;
        }
        let output = child
            .wait_with_output()
            .map_err(|error| format!("powerpoint: generator failed to run: {error}"))?;
        if !output.status.success() {
            let error = String::from_utf8_lossy(&output.stderr).trim().to_string();
            if error.contains("No module named 'pptx'")
                || error.contains("No module named 'lxml'")
                || error.contains("No module named animated_pptx")
            {
                last_error = format!("{}: {error}", python.display());
                continue;
            }
            return Err(if error.is_empty() {
                "powerpoint: generator exited without an error message".into()
            } else {
                format!("powerpoint: {error}")
            });
        }
        let response: serde_json::Value = serde_json::from_slice(&output.stdout)
            .map_err(|error| format!("powerpoint: invalid generator response: {error}"))?;
        if operation == "inspect" {
            let inspection = response
                .get("inspection")
                .ok_or("powerpoint: inspector response omitted inspection data")?;
            return serde_json::to_string_pretty(inspection)
                .map_err(|error| format!("powerpoint: cannot format inspection: {error}"));
        }
        let slide_count = response
            .get("slides")
            .and_then(|value| value.as_u64())
            .ok_or("powerpoint: generator response omitted slide count")?;
        let destination = destination
            .as_ref()
            .ok_or("powerpoint: generator response has no destination")?;
        crate::agent::file_cache::invalidate(destination);
        return Ok(format!(
            "PowerPoint {operation} completed: {} ({} slide{})",
            destination.display(),
            slide_count,
            if slide_count == 1 { "" } else { "s" }
        ));
    }
    Err(format!(
        "powerpoint: Python 3 with python-pptx and lxml is required ({last_error})"
    ))
}

/// Resolve the `from`/`to` (aliases `source`/`dest`/`destination`) path args.
fn from_to(call: &ToolCall, cwd: &Path) -> Result<(PathBuf, PathBuf), String> {
    let from = call
        .args
        .get("from")
        .or_else(|| call.args.get("source"))
        .and_then(|v| v.as_str())
        .ok_or("Missing 'from' argument")?;
    let to = call
        .args
        .get("to")
        .or_else(|| call.args.get("dest"))
        .or_else(|| call.args.get("destination"))
        .and_then(|v| v.as_str())
        .ok_or("Missing 'to' argument")?;
    let (from, to) = (resolve_path(from, cwd), resolve_path(to, cwd));
    Ok((from, to))
}

/// Recursively copy a file or directory tree.
fn copy_recursive(from: &Path, to: &Path) -> Result<(), String> {
    if from.is_dir() {
        fs::create_dir_all(to).map_err(|e| format!("Cannot create {}: {}", to.display(), e))?;
        for entry in
            fs::read_dir(from).map_err(|e| format!("Cannot read {}: {}", from.display(), e))?
        {
            let entry = entry.map_err(|e| e.to_string())?;
            copy_recursive(&entry.path(), &to.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        fs::copy(from, to)
            .map(|_| ())
            .map_err(|e| format!("Cannot copy {} → {}: {}", from.display(), to.display(), e))
    }
}

fn remove_any(path: &Path) -> Result<(), String> {
    let r = if path.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };
    r.map_err(|e| format!("Cannot remove {}: {}", path.display(), e))
}

/// A compact line diff of `old` → `new`: strips the common prefix/suffix and shows
/// the changed span as `- ` (removed) then `+ ` (added) lines. Good for the typical
/// single-region edit; a scattered change shows the whole span between first and
/// last difference. Capped so a huge write doesn't flood the transcript.
fn line_diff(old: &str, new: &str) -> String {
    if old == new {
        return "(no changes)".to_string();
    }
    let o: Vec<&str> = old.lines().collect();
    let n: Vec<&str> = new.lines().collect();
    let mut p = 0;
    while p < o.len() && p < n.len() && o[p] == n[p] {
        p += 1;
    }
    let mut s = 0;
    while s < o.len().saturating_sub(p)
        && s < n.len().saturating_sub(p)
        && o[o.len() - 1 - s] == n[n.len() - 1 - s]
    {
        s += 1;
    }
    let removed = &o[p..o.len() - s];
    let added = &n[p..n.len() - s];
    let mut lines: Vec<String> = Vec::new();
    if p > 0 || s > 0 {
        lines.push(format!("@@ line {} @@", p + 1));
    }
    for l in removed {
        lines.push(format!("- {}", l));
    }
    for l in added {
        lines.push(format!("+ {}", l));
    }
    if lines.is_empty() {
        return "(no changes)".to_string();
    }
    truncate(lines.join("\n"), 6000)
}

fn duplicate_edit_context(content: &str, needle: &str) -> String {
    let lines: Vec<&str> = content.lines().collect();
    let mut out = Vec::new();
    for (i, (byte_idx, _)) in content.match_indices(needle).take(8).enumerate() {
        let line_no = content[..byte_idx].bytes().filter(|b| *b == b'\n').count() + 1;
        let start = line_no.saturating_sub(2).max(1);
        let end = (line_no + needle.lines().count() + 1).min(lines.len().max(1));
        let mut block = Vec::new();
        for n in start..=end {
            if let Some(line) = lines.get(n - 1) {
                block.push(format!("{}: {}", n, line));
            }
        }
        out.push(format!(
            "{}. near line {}:\n{}",
            i + 1,
            line_no,
            block.join("\n")
        ));
    }
    truncate(out.join("\n---\n"), 4000)
}

fn command_exists(name: &str) -> bool {
    Command::new(name)
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn curl_get_text(url: &str, accept: Option<&str>) -> Result<String, String> {
    let mut command = Command::new("curl");
    command.args([
        "--fail-with-body",
        "--location",
        "--silent",
        "--show-error",
        "--max-time",
        "20",
        "--user-agent",
        "aitui/0.1 (+agent)",
    ]);
    if let Some(accept) = accept {
        let header = format!("Accept: {}", accept);
        command.args(["--header", header.as_str()]);
    }
    let output = command
        .arg(url)
        .output()
        .map_err(|e| format!("Could not run curl: {}", e))?;
    if !output.status.success() {
        return Err(format!(
            "curl failed: {}",
            truncate(String::from_utf8_lossy(&output.stderr).into_owned(), 500)
        ));
    }
    String::from_utf8(output.stdout).map_err(|e| format!("Response was not UTF-8: {}", e))
}

fn wget_get_text(url: &str, accept: Option<&str>) -> Result<String, String> {
    let mut command = Command::new("wget");
    command.args([
        "--quiet",
        "--timeout=20",
        "--tries=1",
        "--user-agent=aitui/0.1 (+agent)",
        "--output-document=-",
    ]);
    if let Some(accept) = accept {
        command.arg(format!("--header=Accept: {}", accept));
    }
    let output = command
        .arg(url)
        .output()
        .map_err(|e| format!("Could not run wget: {}", e))?;
    if !output.status.success() {
        return Err(format!(
            "wget failed: {}",
            truncate(String::from_utf8_lossy(&output.stderr).into_owned(), 500)
        ));
    }
    String::from_utf8(output.stdout).map_err(|e| format!("Response was not UTF-8: {}", e))
}

fn command_http_get_text(url: &str, accept: Option<&str>) -> Result<String, String> {
    let mut errors = Vec::new();
    if command_exists("curl") {
        match curl_get_text(url, accept) {
            Ok(text) => return Ok(text),
            Err(error) => errors.push(error),
        }
    }
    if command_exists("wget") {
        match wget_get_text(url, accept) {
            Ok(text) => return Ok(text),
            Err(error) => errors.push(error),
        }
    }
    if errors.is_empty() {
        Err("HTTP tools unavailable: install curl or wget".to_string())
    } else {
        Err(errors.join("; "))
    }
}

fn command_http_get_bytes(url: &str) -> Result<(Vec<u8>, String), String> {
    if command_exists("curl") {
        let marker = b"\nAITUI_CONTENT_TYPE:";
        let output = Command::new("curl")
            .args([
                "--fail-with-body",
                "--location",
                "--silent",
                "--show-error",
                "--max-time",
                "20",
                "--user-agent",
                "aitui/0.1 (+agent)",
                "--write-out",
                "\nAITUI_CONTENT_TYPE:%{content_type}",
                url,
            ])
            .output()
            .map_err(|e| format!("Could not run curl: {}", e))?;
        if output.status.success() {
            if let Some(index) = output
                .stdout
                .windows(marker.len())
                .rposition(|window| window == marker)
            {
                let content_type = String::from_utf8_lossy(&output.stdout[index + marker.len()..])
                    .trim()
                    .to_string();
                return Ok((output.stdout[..index].to_vec(), content_type));
            }
            return Ok((output.stdout, String::new()));
        }
    }
    if command_exists("wget") {
        let output = Command::new("wget")
            .args([
                "--quiet",
                "--timeout=20",
                "--tries=1",
                "--user-agent=aitui/0.1 (+agent)",
                "--output-document=-",
                url,
            ])
            .output()
            .map_err(|e| format!("Could not run wget: {}", e))?;
        if output.status.success() {
            let content_type = if looks_like_html(&output.stdout) {
                "text/html".to_string()
            } else {
                String::new()
            };
            return Ok((output.stdout, content_type));
        }
    }
    Err("Download failed with both curl and wget".to_string())
}

fn looks_like_html(bytes: &[u8]) -> bool {
    let prefix = String::from_utf8_lossy(&bytes[..bytes.len().min(256)]).to_lowercase();
    prefix.contains("<!doctype html") || prefix.contains("<html")
}

fn http_get_text(url: &str) -> Result<String, String> {
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(format!("Refusing non-http(s) URL: {}", url));
    }
    command_http_get_text(url, None)
}

fn http_get_bytes(url: &str) -> Result<(Vec<u8>, String), String> {
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(format!("Refusing non-http(s) URL: {}", url));
    }
    command_http_get_bytes(url)
}

fn reverse_image_search(call: &ToolCall, cwd: &Path) -> Result<String, String> {
    let url = if let Some(url) = call.args.get("url").and_then(|v| v.as_str()) {
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Err(format!("Refusing non-http(s) image URL: {}", url));
        }
        url.to_string()
    } else if let Some(path) = call.args.get("path").and_then(|v| v.as_str()) {
        let path = resolve_path(path, cwd);
        if !path.is_file() {
            return Err(format!("Image file not found: {}", path.display()));
        }
        let bytes =
            fs::read(&path).map_err(|e| format!("Cannot read {}: {}", path.display(), e))?;
        let mime = match path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str()
        {
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "webp" => "image/webp",
            _ => "application/octet-stream",
        };
        return google_lens_upload(
            bytes,
            mime,
            path.file_name().and_then(|n| n.to_str()).unwrap_or("image"),
        );
    } else {
        return Err("Pass exactly one of 'url' or 'path' for reverse_image".into());
    };

    let lens = format!(
        "https://lens.google.com/uploadbyurl?url={}",
        urlencode(&url)
    );
    google_lens_get(&lens)
}

fn google_lens_upload(bytes: Vec<u8>, mime: &str, filename: &str) -> Result<String, String> {
    let client = reqwest::blocking::Client::builder()
        .user_agent("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 Chrome/124 Safari/537.36")
        .timeout(std::time::Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|e| format!("HTTP client error: {}", e))?;
    let part = reqwest::blocking::multipart::Part::bytes(bytes)
        .file_name(filename.to_string())
        .mime_str(mime)
        .map_err(|e| format!("Invalid image MIME type: {}", e))?;
    let resp = client
        .post("https://lens.google.com/v3/upload?stcs=1")
        .multipart(reqwest::blocking::multipart::Form::new().part("encoded_image", part))
        .send()
        .map_err(|e| format!("Reverse-image upload failed: {}", e))?;
    let location = resp
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let status = resp.status();
    let body = resp
        .text()
        .map_err(|e| format!("Read response failed: {}", e))?;
    if let Some(location) = location {
        let result_url = reqwest::Url::parse("https://lens.google.com")
            .and_then(|base| base.join(&location))
            .map_err(|e| format!("Google Lens returned an invalid result URL: {}", e))?;
        let result = client
            .get(result_url.clone())
            .send()
            .map_err(|e| format!("Could not open uploaded Google Lens result: {}", e))?;
        let result_status = result.status();
        let result_body = result
            .text()
            .map_err(|e| format!("Read Google Lens result failed: {}", e))?;
        if !result_status.is_success() {
            return Err(format!(
                "Google Lens result returned HTTP {}: {}",
                result_status,
                truncate(strip_html(&result_body), 500)
            ));
        }
        return format_lens_results("Google Lens", &result_body, result_url.as_str());
    }
    if !status.is_success() {
        return Err(format!(
            "Google Lens returned HTTP {}: {}",
            status,
            truncate(strip_html(&body), 500)
        ));
    }
    format_lens_results("Google Lens", &body, "https://lens.google.com")
}

fn google_lens_get(url: &str) -> Result<String, String> {
    let body = http_get_text(url)?;
    format_lens_results("Google Lens", &body, url)
}

fn format_lens_results(provider: &str, html: &str, result_url: &str) -> Result<String, String> {
    let links = extract_http_links(html);
    let mut out = vec![format!(
        "{} reverse-image results: {}",
        provider, result_url
    )];
    for (i, link) in links.iter().take(12).enumerate() {
        out.push(format!("{}. {}", i + 1, link));
    }
    let text = truncate(strip_html(html), 1200);
    if links.is_empty() && text.trim().is_empty() {
        return Err(
            "Google Lens returned no readable matches; open the result URL in a browser.".into(),
        );
    }
    if !text.trim().is_empty() {
        out.push(format!("\n{}", text));
    }
    Ok(truncate(out.join("\n"), 8192))
}

fn extract_http_links(html: &str) -> Vec<String> {
    let mut links = Vec::new();
    let mut rest = html;
    while let Some(idx) = rest.find("href=") {
        rest = &rest[idx + 5..];
        let quote = rest.chars().next().unwrap_or(' ');
        if quote != '\"' && quote != '\'' {
            continue;
        }
        rest = &rest[quote.len_utf8()..];
        let Some(end) = rest.find(quote) else { break };
        let link = html_unescape(&rest[..end]);
        rest = &rest[end + quote.len_utf8()..];
        if (link.starts_with("https://") || link.starts_with("http://"))
            && !link.contains("google.com")
            && !links.contains(&link)
        {
            links.push(link);
        }
    }
    links
}

fn web_image_search(query: &str) -> Result<String, String> {
    let url = format!(
        "https://commons.wikimedia.org/w/api.php?action=query&format=json&formatversion=2&generator=search&gsrnamespace=6&gsrlimit=10&gsrsearch={}&prop=imageinfo&iiprop=url%7Cextmetadata&iiurlwidth=800",
        urlencode(query)
    );
    let body = http_get_text(&url)?;
    let json: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("Invalid Wikimedia response: {}", e))?;
    format_wikimedia_image_results(query, &json)
}

fn format_wikimedia_image_results(query: &str, json: &serde_json::Value) -> Result<String, String> {
    let pages = json
        .pointer("/query/pages")
        .and_then(|value| value.as_array())
        .ok_or("Wikimedia Commons returned no image results")?;

    let metadata = |info: &serde_json::Value, key: &str| {
        info.pointer(&format!("/extmetadata/{}/value", key))
            .and_then(|value| value.as_str())
            .map(|value| strip_html(&format!("<div>{}</div>", value)))
            .unwrap_or_default()
    };
    let mut results = Vec::new();
    for page in pages {
        let Some(info) = page.pointer("/imageinfo/0") else {
            continue;
        };
        let title = page
            .get("title")
            .and_then(|value| value.as_str())
            .unwrap_or("Untitled image")
            .trim_start_matches("File:");
        let preview = info
            .get("thumburl")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let original = info
            .get("url")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let source = info
            .get("descriptionurl")
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        if original.is_empty() || source.is_empty() {
            continue;
        }
        let description = metadata(info, "ImageDescription");
        let creator = metadata(info, "Artist");
        let license = metadata(info, "LicenseShortName");
        let index = results.len() + 1;
        results.push(format!(
            "{}. {}\n   Preview: {}\n   Original: {}\n   Source: {}\n   Description: {}\n   Creator: {}\n   License: {}",
            index,
            title,
            if preview.is_empty() { original } else { preview },
            original,
            source,
            if description.is_empty() { "Not provided" } else { &description },
            if creator.is_empty() { "Not provided" } else { &creator },
            if license.is_empty() { "See source page" } else { &license },
        ));
    }

    if results.is_empty() {
        return Ok(format!(
            "No downloadable Wikimedia Commons images found for '{}'. Try a more specific architectural style, feature, building, or location.",
            query
        ));
    }
    Ok(format!(
        "Wikimedia Commons image results for '{}' (review each source page and license before reuse):\n\n{}",
        query,
        results.join("\n\n")
    ))
}

/// Web search. SearxNG is the default provider because it is open-source and
/// self-hostable; DuckDuckGo/Bing remain fallback providers so the tool still
/// works when public SearxNG instances rate-limit or block automated requests.
fn web_search(query: &str) -> Result<String, String> {
    // TODO(audit): replace brittle scraped fallbacks with provider-specific clients
    // and structured diagnostics before relying on web_search for critical answers.
    let settings = search_settings();
    let provider = settings.provider.trim().to_lowercase();
    let mut diagnostics = Vec::new();

    let mut tried_primary = false;
    if provider.is_empty() || provider == "searxng" || provider == "searx" {
        tried_primary = true;
        match search_searxng(query, settings.searxng_url.trim()) {
            Ok((provider_name, results)) if !results.is_empty() => {
                return Ok(format_search_results(query, &provider_name, &results));
            }
            Ok((provider_name, _)) => {
                diagnostics.push(format!("{} returned no parseable results", provider_name))
            }
            Err(e) => diagnostics.push(format!("SearxNG failed: {}", e)),
        }
    }

    if provider == "duckduckgo" || provider == "ddg" || tried_primary {
        match search_duckduckgo(query) {
            Ok(results) if !results.is_empty() => {
                return Ok(format_search_results(query, "DuckDuckGo", &results))
            }
            Ok(_) => diagnostics.push(
                "DuckDuckGo returned no parseable results; likely blocked/challenged".to_string(),
            ),
            Err(e) => diagnostics.push(format!("DuckDuckGo failed: {}", e)),
        }
    }

    if provider == "google" {
        match search_google(query) {
            Ok(results) if !results.is_empty() => {
                return Ok(format_search_results(query, "Google", &results))
            }
            Ok(_) => diagnostics.push(
                "Google returned no parseable results; likely blocked/challenged".to_string(),
            ),
            Err(e) => diagnostics.push(format!("Google failed: {}", e)),
        }
    }

    if provider == "bing" || tried_primary || provider == "duckduckgo" || provider == "ddg" {
        match search_bing(query) {
            Ok(results) if !results.is_empty() => {
                return Ok(format_search_results(query, "Bing", &results))
            }
            Ok(_) => diagnostics.push("Bing returned no parseable results".to_string()),
            Err(e) => diagnostics.push(format!("Bing failed: {}", e)),
        }
    }

    if !matches!(
        provider.as_str(),
        "" | "searx" | "searxng" | "duckduckgo" | "ddg" | "bing" | "google"
    ) {
        diagnostics.push(format!(
            "Unknown search provider '{}'; supported: searxng, duckduckgo, bing, google",
            provider
        ));
    }

    Ok(format!(
        "No parseable search results for '{}'. Diagnostics: {}. Try setting [search].searxng_url or AITUI_SEARXNG_URL to your own SearxNG instance, or web_fetch a specific URL.",
        query,
        diagnostics.join("; ")
    ))
}

type SearchResult = (String, String, String);
type SearchResults = Vec<SearchResult>;

fn search_duckduckgo(query: &str) -> Result<SearchResults, String> {
    let html = fetch_search_html("https://html.duckduckgo.com/html/", query)?;
    Ok(parse_ddg_results(&html))
}

fn search_bing(query: &str) -> Result<SearchResults, String> {
    let html = fetch_search_html("https://www.bing.com/search", query)?;
    Ok(parse_bing_results(&html))
}

fn search_google(query: &str) -> Result<SearchResults, String> {
    let html = fetch_search_html("https://www.google.com/search", query)?;
    Ok(parse_google_results(&html))
}

fn search_searxng(query: &str, configured_url: &str) -> Result<(String, SearchResults), String> {
    let mut diagnostics = Vec::new();
    for base in searxng_bases(configured_url) {
        match fetch_searxng_json(&base, query) {
            Ok(json) => {
                let results = parse_searxng_json_results(&json);
                if !results.is_empty() {
                    return Ok((format!("SearxNG {}", normalize_base_url(&base)), results));
                }
                diagnostics.push(format!("{} returned JSON with no parseable results", base));
            }
            Err(e) => diagnostics.push(format!("{}: {}", base, e)),
        }
    }
    Err(diagnostics.join("; "))
}

fn searxng_bases(configured_url: &str) -> Vec<String> {
    let mut bases = Vec::new();
    if !configured_url.trim().is_empty() {
        bases.push(normalize_base_url(configured_url));
    }
    if let Ok(env_url) = std::env::var("AITUI_SEARXNG_URL") {
        if !env_url.trim().is_empty() {
            bases.push(normalize_base_url(&env_url));
        }
    }
    // Public instances are best-effort only; most will eventually rate-limit
    // automated clients. Users should set `searxng_url` for reliable searches.
    for url in [
        "https://search.inetol.net/",
        "https://searx.tiekoetter.com/",
        "https://opnxng.com/",
        "https://baresearch.org/",
    ] {
        bases.push(normalize_base_url(url));
    }
    bases.dedup();
    bases
}

fn normalize_base_url(url: &str) -> String {
    let url = url.trim().trim_end_matches('/');
    url.strip_suffix("/search").unwrap_or(url).to_string()
}

fn fetch_searxng_json(base_url: &str, query: &str) -> Result<String, String> {
    let base = normalize_base_url(base_url);
    if !base.starts_with("http://") && !base.starts_with("https://") {
        return Err(format!("Refusing non-http(s) SearxNG URL: {}", base_url));
    }
    let url = format!("{}/search?q={}&format=json", base, urlencode(query));
    fetch_url_text(&url, "application/json,text/html;q=0.5", false)
}

fn fetch_search_html(base_url: &str, query: &str) -> Result<String, String> {
    let sep = if base_url.contains('?') { '&' } else { '?' };
    let url = format!("{}{}q={}", base_url, sep, urlencode(query));
    fetch_url_text(
        &url,
        "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8",
        true,
    )
}

fn fetch_url_text(url: &str, accept: &str, _allow_202: bool) -> Result<String, String> {
    let text = command_http_get_text(url, Some(accept))?;
    if text.to_lowercase().contains("making sure you") && text.to_lowercase().contains("not a bot")
    {
        return Err("bot-check page returned".to_string());
    }
    Ok(text)
}

fn parse_searxng_json_results(json: &str) -> SearchResults {
    let Ok(root) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let Some(items) = root.get("results").and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for item in items {
        let title = item
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        let url = item
            .get("url")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        let snippet = item
            .get("content")
            .or_else(|| item.get("snippet"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        if title.is_empty() || url.is_empty() {
            continue;
        }
        out.push((strip_tags(title), html_unescape(url), strip_tags(snippet)));
        if out.len() >= 20 {
            break;
        }
    }
    out
}

fn format_search_results(query: &str, provider: &str, results: &[SearchResult]) -> String {
    let mut out = vec![format!("Search results for '{}' ({}):", query, provider)];
    for (i, (title, link, snippet)) in results.iter().take(8).enumerate() {
        if snippet.is_empty() {
            out.push(format!("{}. {}\n   {}", i + 1, title, link));
        } else {
            out.push(format!("{}. {}\n   {}\n   {}", i + 1, title, link, snippet));
        }
    }
    truncate(out.join("\n\n"), 8192)
}

/// Parse DuckDuckGo HTML search results into `(title, url, snippet)` tuples.
/// Result links carry class `result__a` (href is a `/l/?uddg=` redirect we
/// decode); snippets carry class `result__snippet`.
fn parse_ddg_results(html: &str) -> SearchResults {
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(rel) = html[pos..].find("result__a") {
        let a_idx = pos + rel;
        let tag_start = html[..a_idx].rfind("<a").unwrap_or(a_idx);
        let href = extract_attr(&html[tag_start..], "href").unwrap_or_default();
        let link = decode_uddg(&html_unescape(&href));

        // Inner text of the anchor is the title.
        let mut after = a_idx;
        let mut title = String::new();
        if let Some(gt) = html[tag_start..].find('>') {
            let start = tag_start + gt + 1;
            if let Some(close) = html[start..].find("</a>") {
                title = strip_tags(&html[start..start + close]);
                after = start + close;
            }
        }

        // The snippet anchor follows shortly after.
        let mut snippet = String::new();
        if let Some(srel) = html[after..].find("result__snippet") {
            let s_idx = after + srel;
            if let Some(gt) = html[s_idx..].find('>') {
                let start = s_idx + gt + 1;
                if let Some(close) = html[start..].find("</a>") {
                    snippet = strip_tags(&html[start..start + close]);
                }
            }
        }

        if !title.is_empty() && !link.is_empty() {
            out.push((title, link, snippet));
        }
        pos = after + 4;
        if out.len() >= 20 {
            break;
        }
    }
    out
}

/// Parse Bing HTML results. Bing marks organic results as `<li class="b_algo">`
/// with the main title in the first `<h2><a ...>` and the snippet in
/// `<div class="b_caption"><p>...`.
fn parse_bing_results(html: &str) -> SearchResults {
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(rel) = html[pos..].find("b_algo") {
        let item_start = pos + rel;
        let item_end = html[item_start + 1..]
            .find("<li class=\"b_algo\"")
            .map(|n| item_start + 1 + n)
            .unwrap_or_else(|| (item_start + 12_000).min(html.len()));
        let item = &html[item_start..item_end];

        let Some(h2_rel) = item.find("<h2") else {
            pos = item_end;
            continue;
        };
        let h2 = &item[h2_rel..];
        let Some(a_rel) = h2.find("<a") else {
            pos = item_end;
            continue;
        };
        let a = &h2[a_rel..];
        let href = extract_attr(a, "href").unwrap_or_default();
        let link = decode_bing_url(&html_unescape(&href));

        let mut title = String::new();
        if let Some(gt) = a.find('>') {
            let start = gt + 1;
            if let Some(close) = a[start..].find("</a>") {
                title = strip_tags(&a[start..start + close]);
            }
        }

        let mut snippet = String::new();
        if let Some(cap_rel) = item.find("b_caption") {
            let cap = &item[cap_rel..];
            if let Some(p_rel) = cap.find("<p") {
                let p = &cap[p_rel..];
                if let Some(gt) = p.find('>') {
                    let start = gt + 1;
                    if let Some(close) = p[start..].find("</p>") {
                        snippet = strip_tags(&p[start..start + close]);
                    }
                }
            }
        }

        if !title.is_empty() && !link.is_empty() {
            out.push((title, link, snippet));
        }
        pos = item_end;
        if out.len() >= 20 {
            break;
        }
    }
    out
}

/// Parse Google HTML results. Google changes markup often; keep this parser
/// conservative: find organic-looking anchors under `/url?q=` that contain an h3.
fn parse_google_results(html: &str) -> SearchResults {
    let mut out = Vec::new();
    let mut pos = 0;
    while let Some(rel) = html[pos..].find("/url?q=") {
        let href_idx = pos + rel;
        let tag_start = html[..href_idx].rfind("<a").unwrap_or(href_idx);
        let tag = &html[tag_start..(tag_start + 4_000).min(html.len())];
        let href = extract_attr(tag, "href").unwrap_or_default();
        let link = decode_google_url(&html_unescape(&href));
        let Some(h3_rel) = tag.find("<h3") else {
            pos = href_idx + 7;
            continue;
        };
        let h3 = &tag[h3_rel..];
        let Some(gt) = h3.find('>') else {
            pos = href_idx + 7;
            continue;
        };
        let start = gt + 1;
        let Some(close) = h3[start..].find("</h3>") else {
            pos = href_idx + 7;
            continue;
        };
        let title = strip_tags(&h3[start..start + close]);
        if !title.is_empty() && !link.is_empty() {
            out.push((title, link, String::new()));
        }
        pos = href_idx + 7;
        if out.len() >= 20 {
            break;
        }
    }
    out
}

/// Read an HTML attribute value (`name="..."`) from the start of a tag.
fn extract_attr(tag: &str, name: &str) -> Option<String> {
    let pat = format!("{}=\"", name);
    let start = tag.find(&pat)? + pat.len();
    let end = tag[start..].find('"')? + start;
    Some(tag[start..end].to_string())
}

/// Decode a DuckDuckGo result href (`//duckduckgo.com/l/?uddg=<pct-url>&…`) to the
/// real destination URL.
fn decode_uddg(href: &str) -> String {
    if let Some(idx) = href.find("uddg=") {
        let rest = &href[idx + 5..];
        let enc = rest.split('&').next().unwrap_or(rest);
        return pct_decode(enc);
    }
    if href.starts_with("http") {
        href.to_string()
    } else if let Some(stripped) = href.strip_prefix("//") {
        format!("https://{}", stripped)
    } else {
        href.to_string()
    }
}

fn decode_google_url(href: &str) -> String {
    if let Some(idx) = href.find("/url?q=") {
        let rest = &href[idx + 7..];
        let enc = rest.split('&').next().unwrap_or(rest);
        return pct_decode(enc);
    }
    href.to_string()
}

/// Decode Bing click-tracking URLs. Bing often wraps organic result URLs as
/// `/ck/a?...&u=a1<base64url destination>&...`; return the destination when we
/// can decode it, otherwise keep the original href.
fn decode_bing_url(href: &str) -> String {
    if let Some(idx) = href.find("u=") {
        let rest = &href[idx + 2..];
        let enc = rest.split('&').next().unwrap_or(rest);
        let enc = pct_decode(enc);
        let b64 = enc.strip_prefix("a1").unwrap_or(&enc);
        for engine in [
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            &base64::engine::general_purpose::URL_SAFE,
        ] {
            if let Ok(bytes) = engine.decode(b64) {
                let decoded = String::from_utf8_lossy(&bytes).to_string();
                if decoded.starts_with("http://") || decoded.starts_with("https://") {
                    return decoded;
                }
            }
        }
    }
    if href.starts_with("http") {
        href.to_string()
    } else if let Some(stripped) = href.strip_prefix("//") {
        format!("https://{}", stripped)
    } else {
        href.to_string()
    }
}

fn html_unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'")
}

/// Percent-decode a URL-encoded string (also turns `+` into space).
fn pct_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                if let Ok(b) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    out.push(b);
                    i += 3;
                    continue;
                }
                out.push(bytes[i]);
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

/// Strip HTML tags from a small fragment, decode a few common entities, and
/// collapse whitespace — for result titles/snippets.
fn strip_tags(s: &str) -> String {
    let mut text = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => text.push(c),
            _ => {}
        }
    }
    let text = text
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#x27;", "'")
        .replace("&#39;", "'");
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Minimal percent-encoding for query strings (RFC 3986 unreserved kept).
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

fn starts_with_ascii_case_insensitive(bytes: &[u8], pattern: &[u8]) -> bool {
    bytes
        .get(..pattern.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(pattern))
}

fn tag_name_is(bytes: &[u8], start: usize, name: &[u8], closing: bool) -> bool {
    let mut name_start = start + 1;
    if closing {
        if bytes.get(name_start) != Some(&b'/') {
            return false;
        }
        name_start += 1;
    }
    let Some(candidate) = bytes.get(name_start..name_start + name.len()) else {
        return false;
    };
    if !candidate.eq_ignore_ascii_case(name) {
        return false;
    }
    matches!(
        bytes.get(name_start + name.len()),
        Some(b'>') | Some(b'/') | Some(b' ' | b'\t' | b'\n' | b'\r' | 0x0C)
    )
}

fn tag_end(html: &str, start: usize) -> Option<usize> {
    let mut quote = None;
    for (offset, c) in html[start + 1..].char_indices() {
        match (quote, c) {
            (Some(expected), c) if c == expected => quote = None,
            (None, '\'' | '"') => quote = Some(c),
            (None, '>') => return Some(start + 1 + offset + 1),
            _ => {}
        }
    }
    None
}

fn looks_like_tag_start(bytes: &[u8], start: usize) -> bool {
    matches!(
        bytes.get(start + 1),
        Some(b'a'..=b'z' | b'A'..=b'Z' | b'/' | b'!' | b'?')
    )
}

/// Very small HTML→text reduction: drop script/style, strip tags, collapse
/// whitespace. Good enough to feed a page's readable text back to the model.
fn strip_html(html: &str) -> String {
    // TODO(audit): switch to a real HTML readability/parser pipeline; this reducer
    // loses links/headings and can return boilerplate-heavy text for complex pages.
    let bytes = html.as_bytes();
    let looks_like_html = [b"<html".as_slice(), b"<body", b"<div", b"<p"]
        .iter()
        .any(|tag| {
            bytes
                .windows(tag.len())
                .any(|window| window.eq_ignore_ascii_case(tag))
        });
    if !looks_like_html {
        return html.to_string();
    }

    let mut out = String::with_capacity(html.len() / 2);
    let mut i = 0;
    let mut skip_until: Option<&[u8]> = None;
    while i < bytes.len() {
        if let Some(name) = skip_until {
            if bytes[i] == b'<' && tag_name_is(bytes, i, name, true) {
                if let Some(end) = tag_end(html, i) {
                    i = end;
                    skip_until = None;
                    out.push(' ');
                    continue;
                }
            }
            let c = html[i..].chars().next().expect("valid UTF-8 boundary");
            i += c.len_utf8();
            continue;
        }

        if starts_with_ascii_case_insensitive(&bytes[i..], b"<!--") {
            if let Some(offset) = html[i + 4..].find("-->") {
                i += 4 + offset + 3;
            } else {
                break;
            }
            out.push(' ');
            continue;
        }

        if bytes[i] == b'<' {
            if tag_name_is(bytes, i, b"script", false) {
                if let Some(end) = tag_end(html, i) {
                    i = end;
                    skip_until = Some(b"script");
                    continue;
                }
            } else if tag_name_is(bytes, i, b"style", false) {
                if let Some(end) = tag_end(html, i) {
                    i = end;
                    skip_until = Some(b"style");
                    continue;
                }
            } else if looks_like_tag_start(bytes, i) {
                if let Some(end) = tag_end(html, i) {
                    i = end;
                    out.push(' ');
                    continue;
                }
            }
        }

        let c = html[i..].chars().next().expect("valid UTF-8 boundary");
        out.push(c);
        i += c.len_utf8();
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Max entries a single `list_dir` will emit before it stops descending.
const LIST_CAP: usize = 400;
/// Max bytes returned by a whole-file read before it switches to line paging.
const READ_FULL_BYTE_LIMIT: usize = 60_000;
/// Default number of lines returned for a large read page.
const READ_PAGE_LINES: usize = 400;
/// Hard cap on read page size, including explicit `limit` requests.
const READ_PAGE_LIMIT: usize = 1000;

fn usize_arg(call: &ToolCall, key: &str) -> Option<usize> {
    let v = call.args.get(key)?;
    v.as_u64()
        .map(|n| n as usize)
        .or_else(|| v.as_str()?.parse().ok())
}

fn read_output(content: &str, path: &str, offset: Option<usize>, limit: Option<usize>) -> String {
    if offset.is_none() && limit.is_none() && content.len() <= READ_FULL_BYTE_LIMIT {
        return content.to_string();
    }

    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();
    let start = offset.unwrap_or(1).max(1).min(total.saturating_add(1));
    let requested = limit.unwrap_or(READ_PAGE_LINES).max(1);
    let take = requested.min(READ_PAGE_LIMIT);
    let shown: Vec<&str> = lines
        .iter()
        .skip(start.saturating_sub(1))
        .take(take)
        .copied()
        .collect();
    let shown_len = shown.len();
    let end = if shown_len == 0 {
        start.saturating_sub(1)
    } else {
        start + shown_len - 1
    };
    let mut out = format!("[lines {}-{} of {}]", start, end, total);
    if requested > READ_PAGE_LIMIT {
        out.push_str(&format!(
            "\n[limit capped at {} lines per read]",
            READ_PAGE_LIMIT
        ));
    }
    if !shown.is_empty() {
        out.push('\n');
        out.push_str(&shown.join("\n"));
    }
    if end < total {
        let next = end + 1;
        out.push_str(&format!(
            "\n[next: read(path=\"{}\", offset={}, limit={})]",
            path, next, take
        ));
    }
    out
}

/// Recursively list `dir` up to `max_depth` levels (1 = just this dir). Dirs first,
/// then files, each sorted; hidden and heavy build dirs are skipped. Appends indented
/// lines to `lines` and counts entries so the caller can report a cap hit.
fn list_recursive(
    dir: &Path,
    max_depth: usize,
    depth: usize,
    lines: &mut Vec<String>,
    count: &mut usize,
) {
    if depth > max_depth || *count >= LIST_CAP {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut dirs: Vec<(String, PathBuf)> = Vec::new();
    let mut files: Vec<String> = Vec::new();
    let indent = "  ".repeat(depth);
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || name == "target" || name == "node_modules" {
            continue;
        }
        if entry.path().is_dir() {
            dirs.push((format!("{}dir {}/", indent, name), entry.path()));
        } else {
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            files.push(format!("{}file {}  ({})", indent, name, fmt_size(size)));
        }
    }
    dirs.sort_by(|a, b| a.0.cmp(&b.0));
    files.sort();
    for (line, sub) in dirs {
        if *count >= LIST_CAP {
            return;
        }
        lines.push(line);
        *count += 1;
        list_recursive(&sub, max_depth, depth + 1, lines, count);
    }
    for line in files {
        if *count >= LIST_CAP {
            return;
        }
        lines.push(line);
        *count += 1;
    }
}

fn fmt_size(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{}B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1}KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}MB", bytes as f64 / 1024.0 / 1024.0)
    }
}

/// Hard ceiling on how long a single `shell` call may run before it is killed,
/// so a hang (a command waiting on stdin, a dev server, an infinite loop) can't
/// wedge the agent loop forever. Generous enough for real builds/tests.
const SHELL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// Run a shell command with stdin closed and a wall-clock timeout. On timeout the
/// process (and its group, best-effort) is killed and an error is returned instead
/// of blocking indefinitely. When `abort` is set, the same kill path runs and the
/// result reads "Cancelled by user".
fn run_shell_command(
    cmd: &str,
    cwd: &Path,
    abort: &ToolAbort,
) -> Result<std::process::Output, String> {
    // TODO(audit): replace the ad-hoc `sh -c` runner with explicit command
    // classification; timeout alone is not enough process isolation.
    run_shell_with_timeout(cmd, cwd, SHELL_TIMEOUT, abort)
}

fn run_shell_with_timeout(
    cmd: &str,
    cwd: &Path,
    timeout: std::time::Duration,
    abort: &ToolAbort,
) -> Result<std::process::Output, String> {
    use std::process::Stdio;
    use std::sync::atomic::Ordering;
    use std::sync::mpsc;
    use std::time::Instant;

    // `Stdio::null()` on stdin turns a blocking read (e.g. bare `cat`, a REPL)
    // into an immediate EOF rather than an infinite wait. `process_group(0)` puts
    // the shell in its own group so the timeout path can kill the whole tree
    // (`kill -9 -<pid>`), not just the shell.
    #[allow(unused_mut)]
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg(cmd)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let child = command
        .spawn()
        .map_err(|e| format!("Cannot run command: {}", e))?;

    // Capture the pid before moving the child into the waiter thread, so the
    // watchdog can still kill it on timeout or abort.
    let pid = child.id();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });

    let kill = || {
        // Kill the whole process group first (covers children the shell spawned),
        // then the shell itself; ignore failures (it may have just exited).
        let _ = Command::new("kill")
            .arg("-9")
            .arg(format!("-{}", pid))
            .output();
        let _ = Command::new("kill").arg("-9").arg(pid.to_string()).output();
    };
    let deadline = Instant::now() + timeout;
    loop {
        if abort.load(Ordering::Relaxed) {
            kill();
            return Err("Cancelled by user".into());
        }
        match rx.recv_timeout(std::time::Duration::from_millis(50)) {
            Ok(result) => return result.map_err(|e| format!("Command failed: {}", e)),
            Err(mpsc::RecvTimeoutError::Timeout) if Instant::now() >= deadline => {
                kill();
                return Err(format!(
                    "Command timed out after {}s and was killed",
                    timeout.as_secs()
                ));
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err("Command runner thread died".to_string())
            }
        }
    }
}

fn truncate(s: String, max: usize) -> String {
    if s.len() <= max {
        s
    } else {
        // Slice on a UTF-8 char boundary: `&s[..max]` panics if `max` lands in the
        // middle of a multi-byte char (any non-ASCII tool output near the cap),
        // which was crashing the app mid-tool-call. Walk back to the nearest boundary.
        let mut end = max;
        while end > 0 && !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…\n[truncated {} bytes]", &s[..end], s.len() - end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn make_call(name: &str, args: serde_json::Value) -> ToolCall {
        ToolCall {
            name: name.into(),
            args,
            id: None,
        }
    }

    #[test]
    fn executor_turns_panics_into_failed_tool_results() {
        let call = make_call(
            "web",
            serde_json::json!({"action": "search", "query": "dodo"}),
        );
        let result = execute_with(call, |_| panic!("simulated provider panic"));
        assert!(!result.is_ok());
        assert!(result
            .text()
            .contains("Tool failed unexpectedly: simulated provider panic"));
    }

    #[test]
    fn specialized_powerpoint_generates_a_real_deck() {
        let dir = std::env::temp_dir().join(format!(
            "aitui_powerpoint_tool_{}_{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let call = make_call(
            "specialized",
            serde_json::json!({
                "action": "powerpoint",
                "output_path": "deck.pptx",
                "slides": [{
                    "elements": [{
                        "id": "title", "type": "text", "x": 1, "y": 1,
                        "width": 5, "height": 1, "text": "Generated by AiTUI"
                    }],
                    "animations": [],
                    "transition": null
                }]
            }),
        );
        let result = execute(call, &dir);
        assert!(result.is_ok(), "{}", result.text());
        assert!(dir.join("deck.pptx").is_file());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn specialized_powerpoint_inspects_without_mutating_source() {
        let dir = std::env::temp_dir().join(format!(
            "aitui_powerpoint_inspect_tool_{}_{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let create = make_call(
            "specialized",
            serde_json::json!({
                "action": "powerpoint", "operation": "create",
                "output_path": "inspect.pptx",
                "slides": [{"elements": [{
                    "id": "title", "type": "text", "x": 1, "y": 1,
                    "width": 5, "height": 1, "text": "Inspectable"
                }]}]
            }),
        );
        let created = execute(create, &dir);
        assert!(created.is_ok(), "{}", created.text());
        let path = dir.join("inspect.pptx");
        let before = std::fs::read(&path).unwrap();

        let inspect = make_call(
            "specialized",
            serde_json::json!({
                "action": "powerpoint", "operation": "inspect",
                "input_path": "inspect.pptx"
            }),
        );
        let inspected = execute(inspect, &dir);
        assert!(inspected.is_ok(), "{}", inspected.text());
        let result: serde_json::Value = serde_json::from_str(inspected.text()).unwrap();
        assert_eq!(result["presentation"]["slide_count"], 1);
        assert_eq!(result["slides"][0]["shapes"][0]["text"], "Inspectable");
        assert_eq!(result["preservation"]["source_mutated"], false);
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn specialized_powerpoint_appends_and_edits_with_json_modifiers() {
        let dir = std::env::temp_dir().join(format!(
            "aitui_powerpoint_edit_tool_{}_{}",
            std::process::id(),
            std::thread::current().name().unwrap_or("test")
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let create = make_call(
            "specialized",
            serde_json::json!({
                "action": "powerpoint", "operation": "create",
                "output_path": "editable.pptx",
                "slides": [{"elements": [{
                    "id": "title", "type": "text", "x": 1, "y": 1,
                    "width": 5, "height": 1, "text": "Original"
                }]}]
            }),
        );
        let created = execute(create, &dir);
        assert!(created.is_ok(), "{}", created.text());

        let append = make_call(
            "specialized",
            serde_json::json!({
                "action": "powerpoint", "operation": "append",
                "output_path": "editable.pptx",
                "slides": [{"elements": []}]
            }),
        );
        let appended = execute(append, &dir);
        assert!(appended.is_ok(), "{}", appended.text());
        assert!(appended.text().contains("2 slides"));

        let edit = make_call(
            "specialized",
            serde_json::json!({
                "action": "powerpoint", "operation": "edit",
                "output_path": "editable.pptx",
                "modifiers": [{
                    "operation": "update_element", "slide_index": 0,
                    "element_id": "title", "changes": {"text": "Updated"}
                }, {
                    "operation": "set_transition", "slide_index": 1,
                    "transition": "fade"
                }]
            }),
        );
        let edited = execute(edit, &dir);
        assert!(edited.is_ok(), "{}", edited.text());
        assert!(edited.text().contains("PowerPoint edit completed"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn truncate_does_not_panic_on_multibyte_boundary() {
        // A cap landing inside a multi-byte char used to panic. Build a string where
        // byte `max` falls mid-emoji and confirm it truncates cleanly.
        let s = format!("{}日本語日本語日本語", "a".repeat(9));
        // Multi-byte characters are 3 bytes; max=10 lands inside the first one.
        let out = truncate(s, 10);
        assert!(out.starts_with(&"a".repeat(9)));
        assert!(out.contains("truncated"));
    }

    #[test]
    fn line_diff_shows_changed_region_only() {
        let d = line_diff("a\nb\nc\nd", "a\nB\nc\nd");
        assert!(d.contains("- b"));
        assert!(d.contains("+ B"));
        assert!(
            !d.contains("- a") && !d.contains("- c"),
            "common lines omitted: {}",
            d
        );
        assert_eq!(line_diff("same", "same"), "(no changes)");
    }

    #[test]
    fn pct_decode_and_uddg() {
        assert_eq!(pct_decode("https%3A%2F%2Fa.com%2Fx"), "https://a.com/x");
        assert_eq!(pct_decode("a+b%20c"), "a b c");
        assert_eq!(
            decode_uddg("//duckduckgo.com/l/?uddg=https%3A%2F%2Fnews.com%2Fgame&rut=abc"),
            "https://news.com/game"
        );
        assert_eq!(decode_uddg("https://direct.com/x"), "https://direct.com/x");
    }

    #[test]
    fn parse_ddg_results_extracts_title_url_snippet() {
        let html = r#"
          <div class="result">
            <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fespn.com%2Fmatch&rut=z">Match <b>Report</b></a>
            <a class="result__snippet" href="x">Team A beat Team B 3&#x27;2 last night.</a>
          </div>"#;
        let results = parse_ddg_results(html);
        assert_eq!(results.len(), 1);
        let (title, url, snippet) = &results[0];
        assert_eq!(title, "Match Report");
        assert_eq!(url, "https://espn.com/match");
        assert_eq!(snippet, "Team A beat Team B 3'2 last night.");
    }

    #[test]
    fn parse_ddg_results_empty_on_no_results() {
        assert!(parse_ddg_results("<html><body>nothing here</body></html>").is_empty());
    }

    #[test]
    fn parse_searxng_json_results_extracts_title_url_snippet() {
        let json = r#"{
          "query": "rust crossterm",
          "results": [
            {
              "title": "<b>Crossterm</b> docs",
              "url": "https://docs.rs/crossterm/latest/crossterm/",
              "content": "Terminal manipulation &amp; event handling"
            },
            {
              "title": "Missing URL",
              "content": "ignored"
            }
          ]
        }"#;
        let results = parse_searxng_json_results(json);
        assert_eq!(results.len(), 1);
        let (title, url, snippet) = &results[0];
        assert_eq!(title, "Crossterm docs");
        assert_eq!(url, "https://docs.rs/crossterm/latest/crossterm/");
        assert_eq!(snippet, "Terminal manipulation & event handling");
    }

    #[test]
    fn searxng_bases_prefers_configured_url_and_dedups() {
        let bases = searxng_bases("https://example.com/search/");
        assert_eq!(
            bases.first().map(|s| s.as_str()),
            Some("https://example.com")
        );
        assert_eq!(
            bases
                .iter()
                .filter(|s| s.as_str() == "https://example.com")
                .count(),
            1
        );
    }

    #[test]
    fn parse_bing_results_extracts_and_decodes_redirect() {
        let html = r#"
          <ol id="b_results">
            <li class="b_algo">
              <h2><a target="_blank" href="https://www.bing.com/ck/a?!&amp;&amp;u=a1aHR0cHM6Ly9leGFtcGxlLmNvbS9kb2NzP3E9cnVzdCZsYW5nPWVu&amp;ntb=1">Example <strong>Docs</strong></a></h2>
              <div class="b_caption"><p>A useful &amp; relevant result.</p></div>
            </li>
          </ol>"#;
        let results = parse_bing_results(html);
        assert_eq!(results.len(), 1);
        let (title, url, snippet) = &results[0];
        assert_eq!(title, "Example Docs");
        assert_eq!(url, "https://example.com/docs?q=rust&lang=en");
        assert_eq!(snippet, "A useful & relevant result.");
    }

    #[test]
    fn decode_bing_url_falls_back_to_direct_url() {
        assert_eq!(
            decode_bing_url("https://example.com/direct"),
            "https://example.com/direct"
        );
    }

    #[test]
    fn parse_google_results_extracts_redirect_title() {
        let html = r#"
          <div class="g">
            <a href="/url?q=https%3A%2F%2Fexample.com%2Fpage%3Fq%3Drust&amp;sa=U"><h3>Example <em>Page</em></h3></a>
          </div>"#;
        let results = parse_google_results(html);
        assert_eq!(results.len(), 1);
        let (title, url, snippet) = &results[0];
        assert_eq!(title, "Example Page");
        assert_eq!(url, "https://example.com/page?q=rust");
        assert_eq!(snippet, "");
    }

    #[test]
    fn format_wikimedia_images_includes_direct_urls_and_metadata() {
        let json = serde_json::json!({
            "query": {"pages": [{
                "title": "File:Victorian house.jpg",
                "imageinfo": [{
                    "url": "https://upload.wikimedia.org/house.jpg",
                    "thumburl": "https://upload.wikimedia.org/house-800px.jpg",
                    "descriptionurl": "https://commons.wikimedia.org/wiki/File:Victorian_house.jpg",
                    "extmetadata": {
                        "ImageDescription": {"value": "<b>Queen Anne</b> Victorian house"},
                        "Artist": {"value": "Example photographer"},
                        "LicenseShortName": {"value": "CC BY-SA 4.0"}
                    }
                }]
            }]}
        });
        let text = format_wikimedia_image_results("Victorian house", &json).unwrap();
        assert!(text.contains("Victorian house.jpg"));
        assert!(text.contains("Original: https://upload.wikimedia.org/house.jpg"));
        assert!(text.contains("Description: Queen Anne Victorian house"));
        assert!(text.contains("Creator: Example photographer"));
        assert!(text.contains("License: CC BY-SA 4.0"));
    }

    #[tokio::test]
    #[ignore = "live network test; run manually when changing web_search providers"]
    async fn live_dodo_payments_integration_research_returns_results() {
        let query = "Get information about Dodo Payments and how to implement each step into my own app to integrate payment";
        let text = tokio::task::spawn_blocking(move || web_search(query))
            .await
            .unwrap()
            .unwrap();
        assert!(text.contains("Search results for"), "{}", text);
        assert!(!text.contains("No parseable search results"), "{}", text);
        assert!(text.to_lowercase().contains("dodo"), "{}", text);
    }

    #[tokio::test]
    #[ignore = "live network test; run manually when changing Wikimedia image search"]
    async fn live_victorian_image_search_returns_downloadable_results() {
        let text = tokio::task::spawn_blocking(|| {
            web_image_search("Queen Anne Victorian architecture house")
        })
        .await
        .unwrap()
        .unwrap();
        assert!(text.contains("Wikimedia Commons image results"), "{}", text);
        assert!(
            text.contains("Original: https://upload.wikimedia.org/"),
            "{}",
            text
        );
        assert!(
            text.contains("Source: https://commons.wikimedia.org/"),
            "{}",
            text
        );
        assert!(text.contains("License:"), "{}", text);
    }

    #[tokio::test]
    #[ignore = "live network test; downloads one Wikimedia image into a temp directory"]
    async fn live_victorian_image_result_downloads_as_image() {
        tokio::task::spawn_blocking(|| {
            let text = web_image_search("Queen Anne Victorian architecture house").unwrap();
            let url = text
                .lines()
                .find_map(|line| line.trim().strip_prefix("Original: "))
                .expect("image search should return an original URL");
            let dir = tmp_dir();
            let path = dir.join("victorian-reference");
            let call = ToolCall {
                name: "web".into(),
                args: serde_json::json!({
                    "action": "download",
                    "url": url,
                    "path": path.to_string_lossy()
                }),
                id: None,
            };
            let result = execute(call, &dir);
            assert!(result.is_ok(), "{}", result.text());
            assert!(result.text().contains("image/"), "{}", result.text());
            assert!(fs::metadata(&path).unwrap().len() > 1_000);
            let _ = fs::remove_dir_all(dir);
        })
        .await
        .unwrap();
    }

    fn tmp_dir() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("aitui_test_{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        dir
    }

    #[test]
    fn read_file_returns_contents() {
        let dir = tmp_dir();
        let path = dir.join("test.txt");
        fs::write(&path, "hello world").unwrap();
        let call = make_call(
            "read_file",
            serde_json::json!({"path": path.to_str().unwrap()}),
        );
        let result = execute(call, &dir);
        assert!(result.is_ok());
        assert_eq!(result.text(), "hello world");
    }

    #[test]
    fn large_read_returns_page_with_next_call() {
        let dir = tmp_dir();
        let path = dir.join("large.txt");
        let content = (1..=1200)
            .map(|i| format!("line {i} {}", "x".repeat(80)))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&path, content).unwrap();
        let call = make_call("read", serde_json::json!({"path": path.to_str().unwrap()}));
        let result = execute(call, &dir);
        assert!(result.is_ok());
        let text = result.text();
        assert!(text.starts_with("[lines 1-400 of 1200]"), "{text}");
        assert!(text.contains("line 400"), "{text}");
        assert!(!text.contains("line 401"), "{text}");
        assert!(
            text.contains("offset=401, limit=400"),
            "next read call is shown: {text}"
        );
        assert!(
            !text.contains("[truncated"),
            "large reads should page, not truncate: {text}"
        );
    }

    #[test]
    fn read_file_pages_with_offset_and_limit() {
        let dir = tmp_dir();
        let path = dir.join("paged.txt");
        let content = (1..=10)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&path, content).unwrap();
        let call = make_call(
            "read",
            serde_json::json!({"path": path.to_str().unwrap(), "offset": "4", "limit": "3"}),
        );
        let result = execute(call, &dir);
        assert!(result.is_ok());
        assert_eq!(
            result.text(),
            format!(
                "[lines 4-6 of 10]\nline 4\nline 5\nline 6\n[next: read(path=\"{}\", offset=7, limit=3)]",
                path.to_str().unwrap()
            )
        );
    }

    #[test]
    fn read_file_missing_returns_error() {
        let dir = tmp_dir();
        let call = make_call(
            "read_file",
            serde_json::json!({"path": "missing/inside.txt"}),
        );
        let result = execute(call, &dir);
        assert!(!result.is_ok());
        assert!(result.text().contains("Cannot read"));
    }

    #[test]
    fn write_file_creates_file() {
        let dir = tmp_dir();
        let path = dir.join("new_file.txt");
        let call = make_call(
            "write_file",
            serde_json::json!({
                "path": path.to_str().unwrap(),
                "content": "new content"
            }),
        );
        let result = execute(call, &dir);
        assert!(result.is_ok());
        assert!(
            result.text().contains("Created"),
            "new file reports Created: {}",
            result.text()
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), "new content");
    }

    #[test]
    fn write_file_update_shows_diff() {
        let dir = tmp_dir();
        let path = dir.join("upd.txt");
        fs::write(&path, "line1\nold\nline3").unwrap();
        let call = make_call(
            "write_file",
            serde_json::json!({
                "path": path.to_str().unwrap(),
                "content": "line1\nnew\nline3"
            }),
        );
        let result = execute(call, &dir);
        assert!(result.is_ok());
        let text = result.text();
        assert!(
            text.contains("Updated"),
            "existing file reports Updated: {}",
            text
        );
        assert!(text.contains("- old"), "diff shows removed line: {}", text);
        assert!(text.contains("+ new"), "diff shows added line: {}", text);
    }

    #[test]
    fn write_file_dot_relative_path_resolves_under_cwd() {
        // Mirrors the model output `{"path":"./src/test.rs", ...}`.
        let dir = tmp_dir();
        let call = make_call(
            "write_file",
            serde_json::json!({
                "path": "./sub/test.rs",
                "content": "\"Hi there\""
            }),
        );
        let result = execute(call, &dir);
        assert!(result.is_ok(), "write failed: {}", result.text());
        assert_eq!(
            fs::read_to_string(dir.join("sub/test.rs")).unwrap(),
            "\"Hi there\""
        );
    }

    #[test]
    fn write_file_creates_parent_dirs() {
        let dir = tmp_dir();
        let path = dir.join("nested").join("deep").join("file.txt");
        let call = make_call(
            "write_file",
            serde_json::json!({
                "path": path.to_str().unwrap(),
                "content": "nested"
            }),
        );
        let result = execute(call, &dir);
        assert!(result.is_ok());
        assert!(path.exists());
    }

    #[test]
    fn edit_file_replaces_old_with_new() {
        let dir = tmp_dir();
        let path = dir.join("edit.txt");
        fs::write(&path, "hello world foo").unwrap();
        let call = make_call(
            "edit_file",
            serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "world",
                "new_string": "there"
            }),
        );
        let result = execute(call, &dir);
        assert!(result.is_ok());
        assert_eq!(fs::read_to_string(&path).unwrap(), "hello there foo");
    }

    #[test]
    fn edit_result_keeps_actual_source_start_line_for_rendering() {
        let dir = tmp_dir();
        let path = dir.join("line_number.rs");
        fs::write(&path, "fn one() {}\n\nfn target() {\n    false\n}\n").unwrap();
        let call = make_call(
            "edit_file",
            serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "fn target() {\n    false\n}",
                "new_string": "fn target() {\n    true\n}"
            }),
        );
        let result = execute(call, &dir);
        assert!(result.is_ok());
        assert_eq!(result.call.args["__display_start_line"], 3);
    }

    #[test]
    fn edit_file_rejects_duplicate_old_string() {
        let dir = tmp_dir();
        let path = dir.join("edit_dupe.txt");
        fs::write(&path, "same\nkeep\nsame").unwrap();
        let call = make_call(
            "edit_file",
            serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "same",
                "new_string": "changed"
            }),
        );
        let result = execute(call, &dir);
        assert!(!result.is_ok());
        assert!(result.text().contains("matched 2 occurrences"));
        assert!(result.text().contains("Matching locations"));
        assert!(result.text().contains("near line 1"));
        assert!(result.text().contains("near line 3"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "same\nkeep\nsame");
    }

    #[test]
    fn edit_file_missing_old_string_returns_error() {
        let dir = tmp_dir();
        let path = dir.join("edit_err.txt");
        fs::write(&path, "hello world").unwrap();
        let call = make_call(
            "edit_file",
            serde_json::json!({
                "path": path.to_str().unwrap(),
                "old_string": "nope",
                "new_string": "there"
            }),
        );
        let result = execute(call, &dir);
        assert!(!result.is_ok());
        assert!(result.text().contains("old_string not found"));
    }

    #[test]
    fn delete_removes_file() {
        let dir = tmp_dir();
        let path = dir.join("delete_me.txt");
        fs::write(&path, "bye").unwrap();
        // Canonical name.
        let call = make_call(
            "delete",
            serde_json::json!({
                "path": path.to_str().unwrap(),
            }),
        );
        let result = execute(call, &dir);
        assert!(result.is_ok());
        assert!(!path.exists());
        assert!(result.text().contains("Removed"));
    }

    #[test]
    fn delete_removes_directory_tree() {
        // Merged delete: one tool handles both files and directories.
        let dir = tmp_dir();
        let sub = dir.join("subdir/inner");
        fs::create_dir_all(&sub).unwrap();
        fs::write(sub.join("f"), "x").unwrap();
        let call = make_call(
            "delete",
            serde_json::json!({
                "path": dir.join("subdir").to_str().unwrap(),
            }),
        );
        let result = execute(call, &dir);
        assert!(result.is_ok(), "{}", result.text());
        assert!(!dir.join("subdir").exists());
        assert!(result.text().contains("directory"));
    }

    #[test]
    fn make_dir_creates_nested() {
        let dir = tmp_dir();
        let call = make_call(
            "file_management",
            serde_json::json!({"action": "mkdir", "path": "a/b/c"}),
        );
        let result = execute(call, &dir);
        assert!(result.is_ok(), "{}", result.text());
        assert!(dir.join("a/b/c").is_dir());
    }

    #[test]
    fn move_path_renames_file() {
        let dir = tmp_dir();
        fs::write(dir.join("src.txt"), "hi").unwrap();
        let call = make_call(
            "move_path",
            serde_json::json!({"from": "src.txt", "to": "dst.txt"}),
        );
        let result = execute(call, &dir);
        assert!(result.is_ok(), "{}", result.text());
        assert!(!dir.join("src.txt").exists());
        assert_eq!(fs::read_to_string(dir.join("dst.txt")).unwrap(), "hi");
    }

    #[test]
    fn copy_path_copies_directory_tree() {
        let dir = tmp_dir();
        fs::create_dir_all(dir.join("tree/sub")).unwrap();
        fs::write(dir.join("tree/sub/f.txt"), "x").unwrap();
        let call = make_call(
            "copy_path",
            serde_json::json!({"from": "tree", "to": "tree_copy"}),
        );
        let result = execute(call, &dir);
        assert!(result.is_ok(), "{}", result.text());
        assert_eq!(
            fs::read_to_string(dir.join("tree_copy/sub/f.txt")).unwrap(),
            "x"
        );
        assert!(dir.join("tree/sub/f.txt").exists(), "source preserved");
    }

    #[test]
    fn delete_dir_removes_tree() {
        let dir = tmp_dir();
        fs::create_dir_all(dir.join("gone/inner")).unwrap();
        fs::write(dir.join("gone/inner/f"), "").unwrap();
        let call = make_call("delete_dir", serde_json::json!({"path": "gone"}));
        let result = execute(call, &dir);
        assert!(result.is_ok(), "{}", result.text());
        assert!(!dir.join("gone").exists());
    }

    #[test]
    fn legacy_delete_aliases_still_execute() {
        // Old names (delete_file / delete_dir) alias onto the merged `delete`.
        let dir = tmp_dir();
        fs::write(dir.join("f.txt"), "x").unwrap();
        fs::create_dir_all(dir.join("d")).unwrap();
        assert!(execute(
            make_call("delete_file", serde_json::json!({"path": "f.txt"})),
            &dir
        )
        .is_ok());
        assert!(!dir.join("f.txt").exists());
        assert!(execute(
            make_call("delete_dir", serde_json::json!({"path": "d"})),
            &dir
        )
        .is_ok());
        assert!(!dir.join("d").exists());
    }

    #[test]
    fn strip_html_handles_multibyte_text() {
        let html = "<html><body><p>Résumé — 日本語 😊</p></body></html>";
        assert_eq!(strip_html(html), "Résumé — 日本語 😊");
    }

    #[test]
    fn strip_html_only_skips_actual_script_and_style_tags() {
        let html =
            "<HTML><body><scripture>keep</scripture><STYLE>隠す</STYLE><p>after</p></body></HTML>";
        assert_eq!(strip_html(html), "keep after");
    }

    #[test]
    fn strip_html_ignores_tag_markers_in_comments_and_attributes() {
        let html = r#"<html><!-- <script>fake</script> --><body><div title="<script marker>">Visible</div></body></html>"#;
        assert_eq!(strip_html(html), "Visible");
    }

    #[test]
    fn strip_html_handles_quoted_gt_and_literal_comparisons() {
        let html = r#"<html><body><div title="1 > 0">x > 0 and 1 < 2</div></body></html>"#;
        assert_eq!(strip_html(html), "x > 0 and 1 < 2");
    }

    #[test]
    fn strip_html_extracts_text() {
        let html = "<html><body><script>var x=1;</script><p>Hello <b>world</b></p></body></html>";
        let text = strip_html(html);
        assert!(text.contains("Hello"));
        assert!(text.contains("world"));
        assert!(!text.contains("var x"));
    }

    #[test]
    fn urlencode_escapes_reserved() {
        assert_eq!(urlencode("a b&c"), "a%20b%26c");
        assert_eq!(urlencode("rust-lang.org"), "rust-lang.org");
    }

    #[test]
    fn unknown_tool_returns_error() {
        let dir = tmp_dir();
        let call = make_call("nonexistent_tool", serde_json::json!({}));
        let result = execute(call, &dir);
        assert!(!result.is_ok());
        assert!(result.text().contains("Unknown tool"));
    }

    #[test]
    fn run_shell_executes_command() {
        let dir = tmp_dir();
        let call = make_call(
            "run_shell",
            serde_json::json!({
                "command": "echo hello_from_shell"
            }),
        );
        let result = execute(call, &dir);
        assert!(result.is_ok());
        assert!(result.text().contains("hello_from_shell"));
    }

    #[test]
    fn shell_stdin_is_closed_so_stdin_readers_dont_hang() {
        // `cat` with no args reads stdin; with stdin redirected to /dev/null it must
        // hit EOF immediately and exit rather than blocking the test forever.
        let dir = tmp_dir();
        let abort = super::ToolAbort::default();
        let out = run_shell_command("cat", &dir, &abort).expect("cat should finish on EOF");
        assert!(out.status.success());
    }

    #[test]
    fn shell_captures_stdout_and_exit() {
        let dir = tmp_dir();
        let abort = super::ToolAbort::default();
        let out = run_shell_command("printf done; exit 3", &dir, &abort).unwrap();
        assert_eq!(out.status.code(), Some(3));
        assert_eq!(String::from_utf8_lossy(&out.stdout), "done");
    }

    #[test]
    fn shell_kills_command_that_exceeds_timeout() {
        let dir = tmp_dir();
        let start = std::time::Instant::now();
        let abort = super::ToolAbort::default();
        let err = run_shell_with_timeout(
            "sleep 30",
            &dir,
            std::time::Duration::from_millis(300),
            &abort,
        )
        .unwrap_err();
        // Must return promptly (well under the 30s sleep) with a timeout message.
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
        assert!(err.contains("timed out"), "got: {err}");
    }

    #[test]
    fn shell_kills_command_when_abort_is_set() {
        let dir = tmp_dir();
        let abort = super::ToolAbort::default();
        let killer = abort.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(150));
            killer.store(true, std::sync::atomic::Ordering::Relaxed);
        });
        let start = std::time::Instant::now();
        let err = run_shell_command("sleep 30", &dir, &abort).unwrap_err();
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
        assert_eq!(err, "Cancelled by user");
    }

    #[test]
    fn aborted_execute_returns_cancelled_before_running() {
        let dir = tmp_dir();
        let abort = super::ToolAbort::default();
        abort.store(true, std::sync::atomic::Ordering::Relaxed);
        let call = make_call(
            "run_shell",
            serde_json::json!({ "command": "echo should_not_run" }),
        );
        let result = super::execute_abortable(call, &dir, &abort);
        assert_eq!(result.text(), "Cancelled by user");
    }

    #[test]
    fn absolute_paths_outside_cwd_are_accessible() {
        let dir = tmp_dir();
        let outside =
            std::env::temp_dir().join(format!("aitui_outside_absolute_{}", std::process::id()));
        std::fs::create_dir_all(&outside).unwrap();
        let path = outside.join("agent-access.txt");
        std::fs::write(&path, "outside cwd").unwrap();

        let read = make_call(
            "read_file",
            serde_json::json!({"path": path.to_string_lossy()}),
        );
        let result = super::execute(read, &dir);
        assert!(result.is_ok(), "{}", result.text());
        assert_eq!(result.text(), "outside cwd");

        let written = outside.join("agent-written.txt");
        let write = make_call(
            "write",
            serde_json::json!({"path": written.to_string_lossy(), "content": "written outside"}),
        );
        let result = super::execute(write, &dir);
        assert!(result.is_ok(), "{}", result.text());
        assert_eq!(std::fs::read_to_string(written).unwrap(), "written outside");
    }

    #[test]
    fn parent_relative_paths_outside_cwd_are_accessible() {
        let parent =
            std::env::temp_dir().join(format!("aitui_parent_access_{}", std::process::id()));
        let dir = parent.join("workspace");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(parent.join("sibling.txt"), "sibling").unwrap();

        let call = make_call("read_file", serde_json::json!({"path": "../sibling.txt"}));
        let result = super::execute(call, &dir);
        assert!(result.is_ok(), "{}", result.text());
        assert_eq!(result.text(), "sibling");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_paths_outside_cwd_are_accessible() {
        let dir = tmp_dir();
        let outside =
            std::env::temp_dir().join(format!("aitui_outside_symlink_{}", std::process::id()));
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "secret").unwrap();
        let link = dir.join("outside_link");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&outside, &link).unwrap();

        let call = make_call(
            "read_file",
            serde_json::json!({"path": "outside_link/secret.txt"}),
        );
        let result = super::execute(call, &dir);
        assert!(result.is_ok(), "{}", result.text());
        assert_eq!(result.text(), "secret");
    }

    #[test]
    fn search_defaults_to_first_200_matches_and_reports_next_offset() {
        let dir = tmp_dir();
        let path = dir.join("many.txt");
        let content = (1..=250)
            .map(|n| format!("needle {}", n))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&path, content).unwrap();
        let call = make_call(
            "search",
            serde_json::json!({"pattern": "needle", "path": path.to_str().unwrap()}),
        );
        let result = execute(call, &dir);
        assert!(result.is_ok(), "search failed: {}", result.text());
        let text = result.text();
        assert!(
            text.contains("200+ match(es) for 'needle' (showing 1-200)"),
            "default page header: {}",
            text
        );
        assert!(text.contains("rerun with offset 201"));
        assert!(text.contains("needle 1"));
        assert!(!text.contains("needle 201"));
    }

    #[test]
    fn search_pages_with_string_offset_and_limit_args() {
        let dir = tmp_dir();
        let path = dir.join("many_string_args.txt");
        let content = (1..=5)
            .map(|n| format!("needle {}", n))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(&path, content).unwrap();
        let call = make_call(
            "search",
            serde_json::json!({
                "pattern": "needle",
                "path": path.to_str().unwrap(),
                "offset": "3",
                "limit": "2"
            }),
        );
        let result = execute(call, &dir);
        assert!(result.is_ok(), "search failed: {}", result.text());
        let text = result.text();
        assert!(
            text.contains("4+ match(es) for 'needle' (showing 3-4)"),
            "paged header: {}",
            text
        );
        assert!(!text.contains("needle 2"));
        assert!(text.contains("needle 3"));
        assert!(text.contains("needle 4"));
        assert!(!text.contains("needle 5"));
    }

    #[test]
    fn list_dir_lists_files() {
        let dir = tmp_dir();
        fs::write(dir.join("a.txt"), "").unwrap();
        fs::create_dir_all(dir.join("sub")).unwrap();
        let call = make_call("list_dir", serde_json::json!({"path": "."}));
        let result = execute(call, &dir);
        assert!(result.is_ok());
        assert!(result.text().contains("a.txt"));
        assert!(result.text().contains("sub/"));
    }

    #[test]
    fn resolve_path_absolute_unchanged() {
        let p = resolve_path("/absolute/path", &PathBuf::from("/cwd"));
        assert_eq!(p, PathBuf::from("/absolute/path"));
    }

    #[test]
    fn resolve_path_relative_joins_cwd() {
        let p = resolve_path("relative/path", &PathBuf::from("/cwd"));
        assert_eq!(p, PathBuf::from("/cwd/relative/path"));
    }

    #[test]
    fn fmt_size_helpers() {
        assert_eq!(fmt_size(500), "500B");
        assert_eq!(fmt_size(2048), "2.0KB");
        assert_eq!(fmt_size(1048576), "1.0MB");
    }

    #[test]
    fn truncate_short_preserves() {
        assert_eq!(truncate("hello".into(), 10), "hello");
    }

    #[test]
    fn truncate_long_truncates() {
        let t = truncate("hello world".into(), 5);
        assert!(t.contains("hello"));
        assert!(t.contains("truncated"));
    }
}
