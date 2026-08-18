//! Lightweight LSP integration: spawns language servers, syncs open files,
//! and exposes diagnostics + code intelligence as agent tools.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};

use lsp_types::notification::{Notification, PublishDiagnostics};
use lsp_types::request::{GotoDefinition, HoverRequest, References};
use lsp_types::{
    Diagnostic, DiagnosticSeverity, GotoDefinitionParams, GotoDefinitionResponse,
    HoverParams, HoverResponse, InitializeParams, Location, Position,
    ReferenceParams, TextDocumentContentChangeEvent, TextDocumentIdentifier,
    TextDocumentItem, TextDocumentPositionParams, VersionedTextDocumentIdentifier,
};

/// How to find or install a language server for a given file extension.
fn server_for(path: &Path) -> Option<(&'static str, Vec<&'static str>)> {
    let ext = path.extension()?.to_str()?;
    match ext {
        "rs" => Some(("rust-analyzer", vec![])),
        "py" => Some(("pyright-langserver", vec!["--stdio"])),
        "js" | "ts" | "jsx" | "tsx" => Some(("typescript-language-server", vec!["--stdio"])),
        "go" => Some(("gopls", vec![])),
        "json" => Some(("vscode-json-languageserver", vec!["--stdio"])),
        "css" | "scss" | "less" => Some(("vscode-css-languageserver", vec!["--stdio"])),
        "html" => Some(("vscode-html-languageserver", vec!["--stdio"])),
        "rb" => Some(("solargraph", vec!["socket", "--stdio"])),
        "java" => Some(("jdtls", vec![])),
        _ => None,
    }
}

/// A running LSP server process with its stdin/stdout streams.
struct LspConnection {
    stdin: ChildStdin,
    _child: Child,
    server_capabilities: lsp_types::ServerCapabilities,
    next_id: u64,
    /// Buffered response reader (reader thread feeds into a queue).
    reader: BufReader<std::io::ChildStdout>,
    request_map: HashMap<u64, oneshot::Sender<serde_json::Value>>,
}

impl LspConnection {
    fn send_request(
        &mut self,
        method: &str,
        params: serde_json::Value,
    ) -> tokio::sync::oneshot::Receiver<serde_json::Value> {
        let id = self.next_id;
        self.next_id += 1;
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.request_map.insert(id, tx);
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let payload = format!(
            "Content-Length: {}\r\n\r\n{}",
            msg.to_string().len(),
            msg
        );
        if let Err(error) = self
            .stdin
            .write_all(payload.as_bytes())
            .and_then(|_| self.stdin.flush())
        {
            self.request_map.remove(&id);
            crate::app::toast::warning(format!(
                "LSP request '{}' could not be sent: {}",
                method, error
            ));
        }
        rx
    }

    fn send_notification(&mut self, method: &str, params: serde_json::Value) {
        let msg = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        let payload = format!(
            "Content-Length: {}\r\n\r\n{}",
            msg.to_string().len(),
            msg
        );
        if let Err(error) = self
            .stdin
            .write_all(payload.as_bytes())
            .and_then(|_| self.stdin.flush())
        {
            crate::app::toast::warning(format!(
                "LSP notification '{}' could not be sent: {}",
                method, error
            ));
        }
    }

    fn read_response(&mut self) -> Option<(u64, serde_json::Value)> {
        let mut header = String::new();
        self.reader.read_line(&mut header).ok()?;
        if !header.starts_with("Content-Length:") {
            return None;
        }
        let len: usize = header.trim_start_matches("Content-Length:").trim().parse().ok()?;
        self.reader.read_line(&mut header).ok()?; // consume \r\n or remaining headers
        let mut body = vec![0u8; len];
        self.reader.read_exact(&mut body).ok()?;
        let value: serde_json::Value = serde_json::from_slice(&body).ok()?;
        let id = value.get("id")?.as_u64()?;
        Some((id, value))
    }
}

/// Diagnostics reported by an LSP server for one file.
#[derive(Debug, Clone)]
pub struct LspDiagnostics {
    pub path: PathBuf,
    pub diagnostics: Vec<LspDiag>,
}

#[derive(Debug, Clone)]
pub struct LspDiag {
    pub severity: String,
    pub message: String,
    pub range: (usize, usize, usize, usize), // start_line, start_col, end_line, end_col
}

/// A single LSP server session bound to a project root.
pub struct LspSession {
    conn: LspConnection,
    root: PathBuf,
    open_files: HashMap<PathBuf, i32>,
}

impl LspSession {
    pub fn start(root: &Path, server_bin: &str, args: &[&str]) -> Result<Self, String> {
        let mut child = Command::new(server_bin)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("Failed to start {}: {}", server_bin, e))?;

        let stdin = child.stdin.take().ok_or("No stdin")?;
        let stdout = child.stdout.take().ok_or("No stdout")?;
        let reader = BufReader::new(stdout);

        let mut conn = LspConnection {
            stdin,
            _child: child,
            server_capabilities: lsp_types::ServerCapabilities::default(),
            next_id: 1,
            reader,
            request_map: HashMap::new(),
        };

        // Initialize
        let init_params = InitializeParams {
            process_id: Some(std::process::id()),
            root_uri: Some(url::Url::from_file_path(root).map_err(|_| "bad path")?),
            capabilities: lsp_types::ClientCapabilities {
                text_document: Some(lsp_types::TextDocumentClientCapabilities {
                    publish_diagnostics: Some(lsp_types::PublishDiagnosticsClientCapabilities {
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            },
            ..Default::default()
        };

        let rx = conn.send_request("initialize", serde_json::to_value(&init_params).unwrap());
        let _ = std::time::Duration::from_secs(10);
        let resp = rx.try_recv().ok();
        match resp {
            Some(result) => {
                let caps: lsp_types::InitializeResult = serde_json::from_value(result).unwrap_or_default();
                conn.server_capabilities = caps.capabilities;
            }
            None => {
                // Try reading raw response
                if let Some((_, result)) = conn.read_response() {
                    if let Some(r) = result.get("result") {
                        let caps: lsp_types::InitializeResult = serde_json::from_value(r.clone()).unwrap_or_default();
                        conn.server_capabilities = caps.capabilities;
                    }
                }
            }
        }

        conn.send_notification("initialized", serde_json::json!({}));

        Ok(LspSession {
            conn,
            root: root.to_path_buf(),
            open_files: HashMap::new(),
        })
    }

    pub fn open_document(&mut self, path: &Path) -> Result<(), String> {
        let uri = url::Url::from_file_path(path).map_err(|_| "bad path")?;
        let version = self.open_files.len() as i32 + 1;
        let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let item = TextDocumentItem {
            uri,
            language_id: language_id(path),
            version,
            text: content,
        };
        self.conn.send_notification("textDocument/didOpen", serde_json::to_value(&item).unwrap());
        self.open_files.insert(path.to_path_buf(), version);
        Ok(())
    }

    pub fn update_document(&mut self, path: &Path) -> Result<(), String> {
        let uri = url::Url::from_file_path(path).map_err(|_| "bad path")?;
        let version = self.open_files.get(path).copied().unwrap_or(0) + 1;
        let content = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
        let params = serde_json::json!({
            "textDocument": {
                "uri": uri,
                "version": version,
            },
            "contentChanges": [{
                "text": content,
            }],
        });
        self.conn.send_notification("textDocument/didChange", params);
        self.open_files.insert(path.to_path_buf(), version);
        Ok(())
    }

    pub fn close_document(&mut self, path: &Path) {
        if let Some(uri) = url::Url::from_file_path(path).ok() {
            self.conn.send_notification("textDocument/didClose", serde_json::json!({
                "textDocument": { "uri": uri },
            }));
        }
        self.open_files.remove(path);
    }

    /// Poll for diagnostics published by the server. Returns all pending diagnostics.
    pub fn poll_diagnostics(&mut self, path: &Path) -> Result<Vec<LspDiag>, String> {
        // Read any pending responses/notifications
        loop {
            let result = match self.conn.read_response() {
                Some((_, val)) => val,
                None => break,
            };
            // Check if it's a publishDiagnostics notification
            if result.get("method").and_then(|m| m.as_str()) == Some("textDocument/publishDiagnostics") {
                if let Some(params) = result.get("params") {
                    let diag_params: lsp_types::PublishDiagnosticsParams = serde_json::from_value(params.clone()).map_err(|e| e.to_string())?;
                    let file_uri = diag_params.uri.to_string();
                    let file_path = if file_uri.starts_with("file://") {
                        PathBuf::from(file_uri.trim_start_matches("file://"))
                    } else {
                        continue;
                    };
                    if file_path == path {
                        let diags: Vec<LspDiag> = diag_params.diagnostics.into_iter().map(|d| {
                            let sev = match d.severity {
                                Some(DiagnosticSeverity::ERROR) => "error",
                                Some(DiagnosticSeverity::WARNING) => "warning",
                                Some(DiagnosticSeverity::INFORMATION) => "info",
                                _ => "hint",
                            };
                            LspDiag {
                                severity: sev.to_string(),
                                message: d.message,
                                range: (
                                    d.range.start.line as usize,
                                    d.range.start.character as usize,
                                    d.range.end.line as usize,
                                    d.range.end.character as usize,
                                ),
                            }
                        }).collect();
                        return Ok(diags);
                    }
                }
            }
        }
        Ok(Vec::new())
    }

    pub fn go_to_definition(&mut self, path: &Path, line: usize, col: usize) -> Result<Vec<Location>, String> {
        let uri = url::Url::from_file_path(path).map_err(|_| "bad path")?;
        let params = GotoDefinitionParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position { line: line as u32, character: col as u32 },
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        let rx = self.conn.send_request("textDocument/definition", serde_json::to_value(&params).unwrap());
        let result = match rx.try_recv() {
            Ok(val) => val,
            Err(_) => {
                // Read synchronously
                loop {
                    let (_, val) = self.conn.read_response().ok_or("no response")?;
                    if val.get("id").is_some() && val.get("result").is_some() {
                        break val.get("result").cloned().unwrap_or_default();
                    }
                }
            }
        };
        let response: GotoDefinitionResponse = serde_json::from_value(result).map_err(|e| e.to_string())?;
        Ok(match response {
            GotoDefinitionResponse::Scalar(loc) => vec![loc],
            GotoDefinitionResponse::Array(locs) => locs,
            GotoDefinitionResponse::Link(links) => links.into_iter().map(|l| l.target_range).collect::<Vec<_>>().into_iter().flat_map(|r| {
                Location::new(r.start, r.end).into_iter()
            }).collect(),
        })
    }

    pub fn find_references(&mut self, path: &Path, line: usize, col: usize) -> Result<Vec<Location>, String> {
        let uri = url::Url::from_file_path(path).map_err(|_| "bad path")?;
        let params = ReferenceParams {
            text_document_position: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position { line: line as u32, character: col as u32 },
            },
            context: lsp_types::ReferenceContext { include_declaration: true },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        };
        let rx = self.conn.send_request("textDocument/references", serde_json::to_value(&params).unwrap());
        let result = match rx.try_recv() {
            Ok(val) => val,
            Err(_) => {
                loop {
                    let (_, val) = self.conn.read_response().ok_or("no response")?;
                    if val.get("id").is_some() && val.get("result").is_some() {
                        break val.get("result").cloned().unwrap_or_default();
                    }
                }
            }
        };
        serde_json::from_value(result).map_err(|e| e.to_string())
    }

    pub fn hover(&mut self, path: &Path, line: usize, col: usize) -> Result<Option<HoverResponse>, String> {
        let uri = url::Url::from_file_path(path).map_err(|_| "bad path")?;
        let params = HoverParams {
            text_document_position_params: TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position { line: line as u32, character: col as u32 },
            },
            work_done_progress_params: Default::default(),
        };
        let rx = self.conn.send_request("textDocument/hover", serde_json::to_value(&params).unwrap());
        let result = match rx.try_recv() {
            Ok(val) => val,
            Err(_) => {
                loop {
                    let (_, val) = self.conn.read_response().ok_or("no response")?;
                    if val.get("id").is_some() && val.get("result").is_some() {
                        break val.get("result").cloned().unwrap_or_default();
                    }
                }
            }
        };
        if result.is_null() { Ok(None) } else { Ok(Some(serde_json::from_value(result).map_err(|e| e.to_string())?)) }
    }
}

/// Central LSP manager — one session per project root.
pub struct LspManager {
    sessions: HashMap<PathBuf, LspSession>,
}

impl LspManager {
    pub fn new() -> Self {
        Self { sessions: HashMap::new() }
    }

    /// Get or create an LSP session for a file. Returns None if no server is available.
    pub fn session_for(&mut self, path: &Path) -> Result<&mut LspSession, String> {
        let root = project_root(path);
        let server_info = server_for(path).ok_or_else(|| format!("No LSP server for {:?}", path))?;
        if !self.sessions.contains_key(&root) {
            let session = LspSession::start(&root, server_info.0, &server_info.1)?;
            self.sessions.insert(root.clone(), session);
        }
        Ok(self.sessions.get_mut(&root).unwrap())
    }

    pub fn open_document(&mut self, path: &Path) -> Result<(), String> {
        let session = self.session_for(path)?;
        session.open_document(path)
    }

    pub fn update_document(&mut self, path: &Path) -> Result<(), String> {
        if let Ok(session) = self.session_for(path) {
            session.update_document(path)
        } else {
            Ok(())
        }
    }

    pub fn poll_diagnostics(&mut self, path: &Path) -> Result<Vec<LspDiag>, String> {
        let session = self.session_for(path)?;
        session.poll_diagnostics(path)
    }

    pub fn go_to_definition(&mut self, path: &Path, line: usize, col: usize) -> Result<Vec<Location>, String> {
        let session = self.session_for(path)?;
        session.go_to_definition(path, line, col)
    }

    pub fn find_references(&mut self, path: &Path, line: usize, col: usize) -> Result<Vec<Location>, String> {
        let session = self.session_for(path)?;
        session.find_references(path, line, col)
    }

    pub fn hover(&mut self, path: &Path, line: usize, col: usize) -> Result<Option<HoverResponse>, String> {
        let session = self.session_for(path)?;
        session.hover(path, line, col)
    }
}

fn language_id(path: &Path) -> String {
    match path.extension().and_then(|e| e.to_str()) {
        Some("rs") => "rust",
        Some("py") => "python",
        Some("js") => "javascript",
        Some("ts") => "typescript",
        Some("jsx") => "javascriptreact",
        Some("tsx") => "typescriptreact",
        Some("go") => "go",
        Some("json") => "json",
        Some("css") => "css",
        Some("html") => "html",
        Some("rb") => "ruby",
        Some("java") => "java",
        _ => "plaintext",
    }
    .to_string()
}

fn project_root(path: &Path) -> PathBuf {
    let mut dir = if path.is_dir() { path.to_path_buf() } else { path.parent().unwrap_or(Path::new(".")).to_path_buf() };
    // Walk up looking for markers
    for marker in &[".git", "Cargo.toml", "package.json", "go.mod", "pom.xml", "Gemfile", "setup.py", "pyproject.toml"] {
        let mut probe = dir.clone();
        loop {
            if probe.join(marker).exists() {
                return probe;
            }
            if !probe.pop() {
                break;
            }
        }
    }
    dir
}
