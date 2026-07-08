use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
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

/// Execute a tool call, returning the result.
/// `cwd` is the base directory for relative paths.
pub fn execute(call: ToolCall, cwd: &PathBuf) -> ToolResult {
    let start = Instant::now();
    let result = run(&call, cwd);
    let duration_ms = start.elapsed().as_millis() as u64;
    match result {
        Ok(output) => ToolResult::success(call, output, duration_ms),
        Err(err) => ToolResult::failure(call, err, duration_ms),
    }
}

fn resolve_path(raw: &str, cwd: &Path) -> PathBuf {
    // TODO(audit): harden path containment/symlink handling so mutating tools cannot
    // escape the intended workspace after approval via `..` or symlink pivots.
    let p = PathBuf::from(raw);
    if p.is_absolute() {
        p
    } else {
        cwd.join(p)
    }
}

fn run(call: &ToolCall, cwd: &PathBuf) -> Result<String, String> {
    use super::tools::ToolKind;

    // Legacy tools no longer advertised in the schema, kept so a stray call still
    // does the right thing (append must not silently overwrite like write would).
    match call.name.as_str() {
        "append_file" => return append_legacy(call, cwd),
        "make_dir" => {
            let path_str = call
                .args
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or("Missing 'path' argument")?;
            let path = resolve_path(path_str, cwd);
            fs::create_dir_all(&path)
                .map_err(|e| format!("Cannot create {}: {}", path.display(), e))?;
            return Ok(format!("Created directory {}", path.display()));
        }
        _ => {}
    }

    match call.kind() {
        Some(ToolKind::Read) => {
            let path_str = call
                .args
                .get("path")
                .and_then(|v| v.as_str())
                .ok_or("Missing 'path' argument")?;
            let path = resolve_path(path_str, cwd);
            let content = fs::read_to_string(&path)
                .map_err(|e| format!("Cannot read {}: {}", path.display(), e))?;
            // Optional line window: offset = 1-based first line, limit = line count.
            let offset = usize_arg(call, "offset");
            let limit = usize_arg(call, "limit");
            Ok(read_output(&content, path_str, offset, limit))
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
            let old = fs::read_to_string(&path).ok();
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent)
                    .map_err(|e| format!("Cannot create directories: {}", e))?;
            }
            fs::write(&path, content)
                .map_err(|e| format!("Cannot write {}: {}", path.display(), e))?;
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
            let mut lines: Vec<String> = vec![format!("📂 {}", path.display())];
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
            let output = run_shell_command(cmd, cwd)?;

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

        Some(ToolKind::Search) => {
            let pattern = call
                .args
                .get("pattern")
                .or_else(|| call.args.get("query"))
                .and_then(|v| v.as_str())
                .ok_or("Missing 'pattern' argument")?;
            let path_str = call
                .args
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or(".");
            let path = resolve_path(path_str, cwd);
            // Optional glob to restrict files (e.g. "*.rs"); passed to ripgrep when present.
            let glob = call.args.get("glob").and_then(|v| v.as_str());
            let offset = usize_arg(call, "offset").unwrap_or(1).max(1);
            let limit = usize_arg(call, "limit")
                .unwrap_or(200)
                .clamp(1, SEARCH_PAGE_LIMIT);
            let collect = offset
                .saturating_sub(1)
                .saturating_add(limit)
                .min(SEARCH_COLLECT_LIMIT);

            // Prefer ripgrep: regex, .gitignore-aware, skips binaries, fast. Fall back
            // to the built-in literal-substring walker when `rg` isn't installed.
            let (matches, capped) = match ripgrep(pattern, &path, glob, collect) {
                Some(m) => m,
                None => {
                    let mut m: Vec<String> = Vec::new();
                    search_recursive(&path, pattern, &path, &mut m, 0, collect);
                    let capped = m.len() >= collect;
                    (m, capped)
                }
            };

            if matches.is_empty() {
                Ok(format!(
                    "No matches for '{}' in {}",
                    pattern,
                    path.display()
                ))
            } else {
                let total = matches.len();
                let page: Vec<String> = matches.into_iter().skip(offset - 1).take(limit).collect();
                let shown = page.len();
                let end = offset + shown.saturating_sub(1);
                let total_label = if capped {
                    format!("{}+", total)
                } else {
                    total.to_string()
                };
                let header = if shown == 0 {
                    format!(
                        "{} match(es) for '{}' (showing 0 from offset {}):",
                        total_label, pattern, offset
                    )
                } else {
                    format!(
                        "{} match(es) for '{}' (showing {}-{}):",
                        total_label, pattern, offset, end
                    )
                };
                let mut out = vec![header];
                out.extend(page);
                if capped {
                    out.push(format!(
                        "  … more matches possible; rerun with offset {}",
                        offset
                            .saturating_sub(1)
                            .saturating_add(limit)
                            .saturating_add(1)
                    ));
                }
                Ok(truncate(out.join("\n"), 16_000))
            }
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
            let content = fs::read_to_string(&path)
                .map_err(|e| format!("Cannot read {}: {}", path.display(), e))?;
            let count = content.matches(old_s).count();
            match count {
                0 => return Err(format!("old_string not found in {}", path.display())),
                1 => {}
                n => {
                    return Err(format!(
                        "old_string matched {} occurrences in {}; include a larger unique snippet",
                        n,
                        path.display()
                    ))
                }
            }
            let replaced = content.replacen(old_s, new_s, 1);
            fs::write(&path, &replaced)
                .map_err(|e| format!("Cannot write {}: {}", path.display(), e))?;
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
                Ok(()) => Ok(format!("Moved {} → {}", from.display(), to.display())),
                Err(_) => {
                    copy_recursive(&from, &to)?;
                    remove_any(&from)?;
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
            let bytes = http_get_bytes(url)?;
            let n = bytes.len();
            fs::write(&path, &bytes)
                .map_err(|e| format!("Cannot write {}: {}", path.display(), e))?;
            Ok(format!("Downloaded {} bytes → {}", n, path.display()))
        }

        // These are intercepted by the app layer and never reach the executor;
        // handled here only for match exhaustiveness.
        Some(ToolKind::Todo) => Ok("(todo handled by UI)".into()),
        Some(ToolKind::Ask) => Ok("(ask handled by UI)".into()),
        Some(ToolKind::Plan) => Ok("(plan handled by UI)".into()),
        Some(ToolKind::Finish) => Ok("(finish handled by UI)".into()),

        None => Err(format!("Unknown tool: {}", call.name)),
    }
}

/// Legacy `append_file`: append content without overwriting. Not advertised in the
/// current schema, kept so a stray call still appends rather than clobbering.
fn append_legacy(call: &ToolCall, cwd: &PathBuf) -> Result<String, String> {
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
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| format!("Cannot open {}: {}", path.display(), e))?;
    file.write_all(content.as_bytes())
        .map_err(|e| format!("Cannot append to {}: {}", path.display(), e))?;
    Ok(format!(
        "Appended to {} ({} lines)",
        path.display(),
        content.lines().count()
    ))
}

/// Resolve the `from`/`to` (aliases `source`/`dest`/`destination`) path args.
fn from_to(call: &ToolCall, cwd: &PathBuf) -> Result<(PathBuf, PathBuf), String> {
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
    Ok((resolve_path(from, cwd), resolve_path(to, cwd)))
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

/// Run a future to completion from the blocking tool thread (execution already
/// runs inside `tokio::task::spawn_blocking`, so a current-thread block_on here
/// is safe and keeps the executor's synchronous signature).
fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    tokio::runtime::Handle::current().block_on(fut)
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

fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .user_agent("aitui/0.1 (+agent)")
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .map_err(|e| format!("HTTP client error: {}", e))
}

fn http_get_text(url: &str) -> Result<String, String> {
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(format!("Refusing non-http(s) URL: {}", url));
    }
    let client = http_client()?;
    block_on(async move {
        let resp = client
            .get(url)
            .send()
            .await
            .map_err(|e| format!("Request failed: {}", e))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| format!("Read body failed: {}", e))?;
        if !status.is_success() {
            return Err(format!("HTTP {}: {}", status, truncate(text, 500)));
        }
        Ok(text)
    })
}

fn http_get_bytes(url: &str) -> Result<Vec<u8>, String> {
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(format!("Refusing non-http(s) URL: {}", url));
    }
    let client = http_client()?;
    block_on(async move {
        let resp = client
            .get(url)
            .send()
            .await
            .map_err(|e| format!("Request failed: {}", e))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(format!("HTTP {}", status));
        }
        resp.bytes()
            .await
            .map(|b| b.to_vec())
            .map_err(|e| format!("Read body failed: {}", e))
    })
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
        "" | "searx" | "searxng" | "duckduckgo" | "ddg" | "bing"
    ) {
        diagnostics.push(format!(
            "Unknown search provider '{}'; supported: searxng, duckduckgo, bing",
            provider
        ));
    }

    Ok(format!(
        "No parseable search results for '{}'. Diagnostics: {}. Try setting [search].searxng_url or AITUI_SEARXNG_URL to your own SearxNG instance, or web_fetch a specific URL.",
        query,
        diagnostics.join("; ")
    ))
}

fn search_duckduckgo(query: &str) -> Result<Vec<(String, String, String)>, String> {
    let html = fetch_search_html("https://html.duckduckgo.com/html/", query)?;
    Ok(parse_ddg_results(&html))
}

fn search_bing(query: &str) -> Result<Vec<(String, String, String)>, String> {
    let html = fetch_search_html("https://www.bing.com/search", query)?;
    Ok(parse_bing_results(&html))
}

fn search_searxng(
    query: &str,
    configured_url: &str,
) -> Result<(String, Vec<(String, String, String)>), String> {
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

fn fetch_url_text(url: &str, accept: &str, allow_202: bool) -> Result<String, String> {
    let client = http_client()?;
    let url = url.to_string();
    block_on(async move {
        let resp = client
            .get(&url)
            .header(
                "User-Agent",
                "Mozilla/5.0 (X11; Linux x86_64; rv:123.0) Gecko/20100101 Firefox/123.0",
            )
            .header("Accept", accept)
            .header("Accept-Language", "en-US,en;q=0.9")
            .send()
            .await
            .map_err(|e| format!("Search request failed: {}", e))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| format!("Read body failed: {}", e))?;
        if !(status.is_success() || (allow_202 && status.as_u16() == 202)) {
            return Err(format!("HTTP {}: {}", status, truncate(text, 300)));
        }
        if text.to_lowercase().contains("making sure you")
            && text.to_lowercase().contains("not a bot")
        {
            return Err("bot-check page returned".to_string());
        }
        Ok(text)
    })
}

fn parse_searxng_json_results(json: &str) -> Vec<(String, String, String)> {
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

fn format_search_results(
    query: &str,
    provider: &str,
    results: &[(String, String, String)],
) -> String {
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
fn parse_ddg_results(html: &str) -> Vec<(String, String, String)> {
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
fn parse_bing_results(html: &str) -> Vec<(String, String, String)> {
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

/// Very small HTML→text reduction: drop script/style, strip tags, collapse
/// whitespace. Good enough to feed a page's readable text back to the model.
fn strip_html(html: &str) -> String {
    // TODO(audit): switch to a real HTML readability/parser pipeline; this reducer
    // loses links/headings and can return boilerplate-heavy text for complex pages.
    let lower = html.to_lowercase();
    // If it doesn't look like HTML, return as-is.
    if !lower.contains("<html")
        && !lower.contains("<body")
        && !lower.contains("<div")
        && !lower.contains("<p")
    {
        return html.to_string();
    }
    let mut out = String::with_capacity(html.len() / 2);
    let bytes = html.as_bytes();
    let mut i = 0;
    let mut in_tag = false;
    let mut skip_until: Option<&str> = None;
    while i < bytes.len() {
        if let Some(close) = skip_until {
            if lower[i..].starts_with(close) {
                i += close.len();
                skip_until = None;
            } else {
                i += 1;
            }
            continue;
        }
        if lower[i..].starts_with("<script") {
            skip_until = Some("</script>");
            i += 7;
            continue;
        }
        if lower[i..].starts_with("<style") {
            skip_until = Some("</style>");
            i += 6;
            continue;
        }
        let c = bytes[i] as char;
        if c == '<' {
            in_tag = true;
        } else if c == '>' {
            in_tag = false;
            out.push(' ');
        } else if !in_tag {
            out.push(c);
        }
        i += 1;
    }
    // Collapse runs of whitespace.
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
/// Max search matches returned to a single page.
const SEARCH_PAGE_LIMIT: usize = 1000;
/// Max search matches collected internally while paging.
const SEARCH_COLLECT_LIMIT: usize = 20_000;

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
            dirs.push((format!("{}📁 {}/", indent, name), entry.path()));
        } else {
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            files.push(format!("{}📄 {}  ({})", indent, name, fmt_size(size)));
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

/// Run ripgrep for `pattern` under `path`, returning `file:line: text` matches.
/// Returns `None` if `rg` isn't on PATH (caller falls back to the built-in walker).
/// `rg`'s own exit code 1 (no matches) maps to an empty vec, not a fallback.
fn ripgrep(
    pattern: &str,
    path: &Path,
    glob: Option<&str>,
    collect: usize,
) -> Option<(Vec<String>, bool)> {
    let mut cmd = Command::new("rg");
    cmd.arg("--line-number")
        .arg("--no-heading")
        .arg("--color=never")
        .arg("--max-columns=300");
    if let Some(g) = glob {
        cmd.arg("--glob").arg(g);
    }
    cmd.arg("-e").arg(pattern).arg(path);
    let output = cmd.output().ok()?;
    // rg missing → Command::output errors above → None. Here rg ran: 0 = matches,
    // 1 = no matches (both fine), 2 = actual error (still return what we parsed).
    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut capped = false;
    let mut out = Vec::new();
    for line in stdout.lines() {
        if out.len() >= collect {
            capped = true;
            break;
        }
        out.push(format!("  {}", line));
    }
    Some((out, capped))
}

fn search_recursive(
    dir: &Path,
    pattern: &str,
    base: &Path,
    matches: &mut Vec<String>,
    depth: usize,
    collect: usize,
) {
    if depth > 8 || matches.len() >= collect {
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
            search_recursive(&path, pattern, base, matches, depth + 1, collect);
        } else {
            if let Ok(content) = fs::read_to_string(&path) {
                for (line_no, line) in content.lines().enumerate() {
                    if matches.len() >= collect {
                        return;
                    }
                    if line.to_lowercase().contains(&pattern.to_lowercase()) {
                        let rel = path.strip_prefix(base).unwrap_or(&path);
                        matches.push(format!(
                            "  {}:{}: {}",
                            rel.display(),
                            line_no + 1,
                            line.trim()
                        ));
                    }
                }
            }
        }
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
/// of blocking indefinitely.
fn run_shell_command(cmd: &str, cwd: &Path) -> Result<std::process::Output, String> {
    // TODO(audit): replace the ad-hoc `sh -c` runner with explicit command
    // classification/sandboxing; timeout alone is not enough isolation.
    run_shell_with_timeout(cmd, cwd, SHELL_TIMEOUT)
}

fn run_shell_with_timeout(
    cmd: &str,
    cwd: &Path,
    timeout: std::time::Duration,
) -> Result<std::process::Output, String> {
    use std::process::Stdio;
    use std::sync::mpsc;

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
    // watchdog can still kill it on timeout.
    let pid = child.id();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(child.wait_with_output());
    });

    match rx.recv_timeout(timeout) {
        Ok(result) => result.map_err(|e| format!("Command failed: {}", e)),
        Err(mpsc::RecvTimeoutError::Timeout) => {
            // Kill the whole process group first (covers children the shell spawned),
            // then the shell itself; ignore failures (it may have just exited).
            let _ = Command::new("kill")
                .arg("-9")
                .arg(format!("-{}", pid))
                .output();
            let _ = Command::new("kill").arg("-9").arg(pid.to_string()).output();
            Err(format!(
                "Command timed out after {}s and was killed",
                timeout.as_secs()
            ))
        }
        Err(mpsc::RecvTimeoutError::Disconnected) => Err("Command runner thread died".to_string()),
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
    fn truncate_does_not_panic_on_multibyte_boundary() {
        // A cap landing inside a multi-byte char used to panic. Build a string where
        // byte `max` falls mid-emoji and confirm it truncates cleanly.
        let s = format!("{}🚀🚀🚀", "a".repeat(9));
        // "🚀" is 4 bytes; max=10 lands inside the first emoji.
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

    #[tokio::test]
    #[ignore = "live network test; run manually when changing web_search providers"]
    async fn live_web_search_returns_results() {
        let text = tokio::task::spawn_blocking(|| web_search("Rust crossterm"))
            .await
            .unwrap()
            .unwrap();
        assert!(text.contains("Search results for"), "{}", text);
        assert!(!text.contains("No parseable search results"), "{}", text);
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
            serde_json::json!({"path": "/nonexistent/path.txt"}),
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
    fn append_file_adds_content() {
        let dir = tmp_dir();
        let path = dir.join("append.txt");
        fs::write(&path, "base").unwrap();
        let call = make_call(
            "append_file",
            serde_json::json!({
                "path": path.to_str().unwrap(),
                "content": "+more"
            }),
        );
        let result = execute(call, &dir);
        assert!(result.is_ok());
        assert_eq!(fs::read_to_string(&path).unwrap(), "base+more");
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
        let call = make_call("make_dir", serde_json::json!({"path": "a/b/c"}));
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
        let out = run_shell_command("cat", &dir).expect("cat should finish on EOF");
        assert!(out.status.success());
    }

    #[test]
    fn shell_captures_stdout_and_exit() {
        let dir = tmp_dir();
        let out = run_shell_command("printf done; exit 3", &dir).unwrap();
        assert_eq!(out.status.code(), Some(3));
        assert_eq!(String::from_utf8_lossy(&out.stdout), "done");
    }

    #[test]
    fn shell_kills_command_that_exceeds_timeout() {
        let dir = tmp_dir();
        let start = std::time::Instant::now();
        let err = run_shell_with_timeout("sleep 30", &dir, std::time::Duration::from_millis(300))
            .unwrap_err();
        // Must return promptly (well under the 30s sleep) with a timeout message.
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
        assert!(err.contains("timed out"), "got: {err}");
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
