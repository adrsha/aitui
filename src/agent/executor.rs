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
                return Err(format!("{} is a directory; use delete_dir to remove it", path.display()));
            }
            fs::remove_file(&path)
                .map_err(|e| format!("Cannot delete {}: {}", path.display(), e))?;
            Ok(format!("Deleted {}", path.display()))
        }

        "make_dir" => {
            let path_str = call.args.get("path").and_then(|v| v.as_str())
                .ok_or("Missing 'path' argument")?;
            let path = resolve_path(path_str, cwd);
            fs::create_dir_all(&path)
                .map_err(|e| format!("Cannot create {}: {}", path.display(), e))?;
            Ok(format!("Created directory {}", path.display()))
        }

        "move_path" => {
            let (from, to) = from_to(call, cwd)?;
            if !from.exists() {
                return Err(format!("Source not found: {}", from.display()));
            }
            if let Some(parent) = to.parent() {
                fs::create_dir_all(parent).map_err(|e| format!("Cannot create target dir: {}", e))?;
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

        "copy_path" => {
            let (from, to) = from_to(call, cwd)?;
            if !from.exists() {
                return Err(format!("Source not found: {}", from.display()));
            }
            if let Some(parent) = to.parent() {
                fs::create_dir_all(parent).map_err(|e| format!("Cannot create target dir: {}", e))?;
            }
            copy_recursive(&from, &to)?;
            Ok(format!("Copied {} → {}", from.display(), to.display()))
        }

        "delete_dir" => {
            let path_str = call.args.get("path").and_then(|v| v.as_str())
                .ok_or("Missing 'path' argument")?;
            let path = resolve_path(path_str, cwd);
            if !path.is_dir() {
                return Err(format!("{} is not a directory (use delete_file)", path.display()));
            }
            fs::remove_dir_all(&path)
                .map_err(|e| format!("Cannot delete {}: {}", path.display(), e))?;
            Ok(format!("Deleted directory {}", path.display()))
        }

        "web_search" => {
            let query = call.args.get("query").or_else(|| call.args.get("q"))
                .and_then(|v| v.as_str())
                .ok_or("Missing 'query' argument")?;
            web_search(query)
        }

        "web_fetch" => {
            let url = call.args.get("url").and_then(|v| v.as_str())
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

        "download_file" => {
            let url = call.args.get("url").and_then(|v| v.as_str())
                .ok_or("Missing 'url' argument")?;
            let path_str = call.args.get("path").and_then(|v| v.as_str())
                .ok_or("Missing 'path' argument")?;
            let path = resolve_path(path_str, cwd);
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|e| format!("Cannot create target dir: {}", e))?;
            }
            let bytes = http_get_bytes(url)?;
            let n = bytes.len();
            fs::write(&path, &bytes).map_err(|e| format!("Cannot write {}: {}", path.display(), e))?;
            Ok(format!("Downloaded {} bytes → {}", n, path.display()))
        }

        name => Err(format!("Unknown tool: {}", name)),
    }
}

/// Resolve the `from`/`to` (aliases `source`/`dest`/`destination`) path args.
fn from_to(call: &ToolCall, cwd: &PathBuf) -> Result<(PathBuf, PathBuf), String> {
    let from = call.args.get("from").or_else(|| call.args.get("source")).and_then(|v| v.as_str())
        .ok_or("Missing 'from' argument")?;
    let to = call.args.get("to").or_else(|| call.args.get("dest")).or_else(|| call.args.get("destination"))
        .and_then(|v| v.as_str())
        .ok_or("Missing 'to' argument")?;
    Ok((resolve_path(from, cwd), resolve_path(to, cwd)))
}

/// Recursively copy a file or directory tree.
fn copy_recursive(from: &Path, to: &Path) -> Result<(), String> {
    if from.is_dir() {
        fs::create_dir_all(to).map_err(|e| format!("Cannot create {}: {}", to.display(), e))?;
        for entry in fs::read_dir(from).map_err(|e| format!("Cannot read {}: {}", from.display(), e))? {
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
    let r = if path.is_dir() { fs::remove_dir_all(path) } else { fs::remove_file(path) };
    r.map_err(|e| format!("Cannot remove {}: {}", path.display(), e))
}

/// Run a future to completion from the blocking tool thread (execution already
/// runs inside `tokio::task::spawn_blocking`, so a current-thread block_on here
/// is safe and keeps the executor's synchronous signature).
fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    tokio::runtime::Handle::current().block_on(fut)
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
        let resp = client.get(url).send().await.map_err(|e| format!("Request failed: {}", e))?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| format!("Read body failed: {}", e))?;
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
        let resp = client.get(url).send().await.map_err(|e| format!("Request failed: {}", e))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(format!("HTTP {}", status));
        }
        resp.bytes().await.map(|b| b.to_vec()).map_err(|e| format!("Read body failed: {}", e))
    })
}

/// Web search via DuckDuckGo's keyless HTML endpoint, which returns real result
/// links + snippets (the Instant-Answer JSON API only has encyclopedia-style
/// abstracts, so it returns nothing for news / most queries). No API key needed.
fn web_search(query: &str) -> Result<String, String> {
    let client = http_client()?;
    let url = format!("https://html.duckduckgo.com/html/?q={}", urlencode(query));
    let html = block_on(async move {
        // A browser-ish UA + Accept-Language; the html endpoint returns a blank
        // page to unknown agents.
        let resp = client
            .get(&url)
            .header("User-Agent", "Mozilla/5.0 (X11; Linux x86_64; rv:123.0) Gecko/20100101 Firefox/123.0")
            .header("Accept-Language", "en-US,en;q=0.9")
            .send()
            .await
            .map_err(|e| format!("Search failed: {}", e))?;
        let status = resp.status();
        let text = resp.text().await.map_err(|e| format!("Read body failed: {}", e))?;
        if !status.is_success() {
            return Err(format!("Search HTTP {}: {}", status, truncate(text, 300)));
        }
        Ok(text)
    })?;

    let results = parse_ddg_results(&html);
    if results.is_empty() {
        return Ok(format!(
            "No results for '{}'. Try different search terms, or web_fetch a specific URL.",
            query
        ));
    }
    let mut out = vec![format!("Search results for '{}':", query)];
    for (i, (title, link, snippet)) in results.iter().take(8).enumerate() {
        if snippet.is_empty() {
            out.push(format!("{}. {}\n   {}", i + 1, title, link));
        } else {
            out.push(format!("{}. {}\n   {}\n   {}", i + 1, title, link, snippet));
        }
    }
    Ok(truncate(out.join("\n\n"), 8192))
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
        let link = decode_uddg(&href);

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
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// Very small HTML→text reduction: drop script/style, strip tags, collapse
/// whitespace. Good enough to feed a page's readable text back to the model.
fn strip_html(html: &str) -> String {
    let lower = html.to_lowercase();
    // If it doesn't look like HTML, return as-is.
    if !lower.contains("<html") && !lower.contains("<body") && !lower.contains("<div") && !lower.contains("<p") {
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
        if lower[i..].starts_with("<script") { skip_until = Some("</script>"); i += 7; continue; }
        if lower[i..].starts_with("<style") { skip_until = Some("</style>"); i += 6; continue; }
        let c = bytes[i] as char;
        if c == '<' { in_tag = true; }
        else if c == '>' { in_tag = false; out.push(' '); }
        else if !in_tag { out.push(c); }
        i += 1;
    }
    // Collapse runs of whitespace.
    out.split_whitespace().collect::<Vec<_>>().join(" ")
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn make_call(name: &str, args: serde_json::Value) -> ToolCall {
        ToolCall { name: name.into(), args, id: None }
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
        let call = make_call("read_file", serde_json::json!({"path": path.to_str().unwrap()}));
        let result = execute(call, &dir);
        assert!(result.is_ok());
        assert_eq!(result.text(), "hello world");
    }

    #[test]
    fn read_file_missing_returns_error() {
        let dir = tmp_dir();
        let call = make_call("read_file", serde_json::json!({"path": "/nonexistent/path.txt"}));
        let result = execute(call, &dir);
        assert!(!result.is_ok());
        assert!(result.text().contains("Cannot read"));
    }

    #[test]
    fn write_file_creates_file() {
        let dir = tmp_dir();
        let path = dir.join("new_file.txt");
        let call = make_call("write_file", serde_json::json!({
            "path": path.to_str().unwrap(),
            "content": "new content"
        }));
        let result = execute(call, &dir);
        assert!(result.is_ok());
        assert!(result.text().contains("Written"));
        assert_eq!(fs::read_to_string(&path).unwrap(), "new content");
    }

    #[test]
    fn write_file_dot_relative_path_resolves_under_cwd() {
        // Mirrors the model output `{"path":"./src/test.rs", ...}`.
        let dir = tmp_dir();
        let call = make_call("write_file", serde_json::json!({
            "path": "./sub/test.rs",
            "content": "\"Hi there\""
        }));
        let result = execute(call, &dir);
        assert!(result.is_ok(), "write failed: {}", result.text());
        assert_eq!(fs::read_to_string(dir.join("sub/test.rs")).unwrap(), "\"Hi there\"");
    }

    #[test]
    fn write_file_creates_parent_dirs() {
        let dir = tmp_dir();
        let path = dir.join("nested").join("deep").join("file.txt");
        let call = make_call("write_file", serde_json::json!({
            "path": path.to_str().unwrap(),
            "content": "nested"
        }));
        let result = execute(call, &dir);
        assert!(result.is_ok());
        assert!(path.exists());
    }

    #[test]
    fn edit_file_replaces_old_with_new() {
        let dir = tmp_dir();
        let path = dir.join("edit.txt");
        fs::write(&path, "hello world foo").unwrap();
        let call = make_call("edit_file", serde_json::json!({
            "path": path.to_str().unwrap(),
            "old_string": "world",
            "new_string": "there"
        }));
        let result = execute(call, &dir);
        assert!(result.is_ok());
        assert_eq!(fs::read_to_string(&path).unwrap(), "hello there foo");
    }

    #[test]
    fn edit_file_missing_old_string_returns_error() {
        let dir = tmp_dir();
        let path = dir.join("edit_err.txt");
        fs::write(&path, "hello world").unwrap();
        let call = make_call("edit_file", serde_json::json!({
            "path": path.to_str().unwrap(),
            "old_string": "nope",
            "new_string": "there"
        }));
        let result = execute(call, &dir);
        assert!(!result.is_ok());
        assert!(result.text().contains("old_string not found"));
    }

    #[test]
    fn append_file_adds_content() {
        let dir = tmp_dir();
        let path = dir.join("append.txt");
        fs::write(&path, "base").unwrap();
        let call = make_call("append_file", serde_json::json!({
            "path": path.to_str().unwrap(),
            "content": "+more"
        }));
        let result = execute(call, &dir);
        assert!(result.is_ok());
        assert_eq!(fs::read_to_string(&path).unwrap(), "base+more");
    }

    #[test]
    fn delete_file_removes_file() {
        let dir = tmp_dir();
        let path = dir.join("delete_me.txt");
        fs::write(&path, "bye").unwrap();
        let call = make_call("delete_file", serde_json::json!({
            "path": path.to_str().unwrap(),
        }));
        let result = execute(call, &dir);
        assert!(result.is_ok());
        assert!(!path.exists());
        assert!(result.text().contains("Deleted"));
    }

    #[test]
    fn delete_file_refuses_directory() {
        let dir = tmp_dir();
        let sub = dir.join("subdir");
        fs::create_dir_all(&sub).unwrap();
        let call = make_call("delete_file", serde_json::json!({
            "path": sub.to_str().unwrap(),
        }));
        let result = execute(call, &dir);
        assert!(!result.is_ok());
        assert!(result.text().contains("is a directory"));
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
        let call = make_call("move_path", serde_json::json!({"from": "src.txt", "to": "dst.txt"}));
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
        let call = make_call("copy_path", serde_json::json!({"from": "tree", "to": "tree_copy"}));
        let result = execute(call, &dir);
        assert!(result.is_ok(), "{}", result.text());
        assert_eq!(fs::read_to_string(dir.join("tree_copy/sub/f.txt")).unwrap(), "x");
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
    fn delete_file_refuses_directory_points_to_delete_dir() {
        let dir = tmp_dir();
        fs::create_dir_all(dir.join("d")).unwrap();
        let call = make_call("delete_file", serde_json::json!({"path": "d"}));
        let result = execute(call, &dir);
        assert!(!result.is_ok());
        assert!(result.text().contains("delete_dir"));
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
        let call = make_call("run_shell", serde_json::json!({
            "command": "echo hello_from_shell"
        }));
        let result = execute(call, &dir);
        assert!(result.is_ok());
        assert!(result.text().contains("hello_from_shell"));
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

