use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::agent::ToolCall;

/// Max search matches returned to a single page.
const SEARCH_PAGE_LIMIT: usize = 1000;
/// Max search matches collected internally while paging.
const SEARCH_COLLECT_LIMIT: usize = 20_000;

pub(crate) fn execute(call: &ToolCall, cwd: &Path) -> Result<String, String> {
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
    let glob = call.args.get("glob").and_then(|v| v.as_str());
    let offset = usize_arg(call, "offset").unwrap_or(1).max(1);
    let limit = usize_arg(call, "limit")
        .unwrap_or(200)
        .clamp(1, SEARCH_PAGE_LIMIT);
    let collect = offset
        .saturating_sub(1)
        .saturating_add(limit)
        .min(SEARCH_COLLECT_LIMIT);

    let (matches, capped) = match ripgrep(pattern, &path, glob, collect) {
        Some(m) => m,
        None => {
            let mut m = Vec::new();
            search_recursive(&path, pattern, &path, &mut m, 0, collect);
            let capped = m.len() >= collect;
            (m, capped)
        }
    };

    if matches.is_empty() {
        return Ok(format!("No matches for '{}' in {}", pattern, path.display()));
    }

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

fn resolve_path(path: &str, cwd: &Path) -> PathBuf {
    let p = PathBuf::from(path);
    if p.is_absolute() {
        p
    } else {
        cwd.join(p)
    }
}

fn usize_arg(call: &ToolCall, key: &str) -> Option<usize> {
    let v = call.args.get(key)?;
    v.as_u64()
        .map(|n| n as usize)
        .or_else(|| v.as_str()?.parse().ok())
}

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
        if name.starts_with('.') || name == "target" || name == "node_modules" {
            continue;
        }
        if path.is_dir() {
            search_recursive(&path, pattern, base, matches, depth + 1, collect);
        } else if let Ok(content) = fs::read_to_string(&path) {
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

fn truncate(s: String, max: usize) -> String {
    if s.len() <= max {
        s
    } else {
        let mut end = max;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}\n...[truncated]", &s[..end])
    }
}
