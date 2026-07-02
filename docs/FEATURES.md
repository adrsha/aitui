# Feature inventory

Status legend: ✅ done & working · 🟡 partial / needs polish · ⬜ not started

## Chat & model I/O

| Feature | Status | Notes |
|---------|:------:|-------|
| OpenAI-compatible chat completions | ✅ | `POST /v1/chat/completions`. |
| Streaming (SSE) token rendering | ✅ | Incremental, sticks to bottom while generating. |
| Reasoning / "thinking" stream | ✅ | Separate `reasoning`/`reasoning_content` deltas, rendered as collapsible `<think>`. |
| Model listing from `/v1/models` | ✅ | Fetched async on startup, falls back to a default list. |
| Model picker (fuzzy) + cycle | ✅ | `Ctrl-M`, `:model`, palette. |
| Request timeout / cancel mid-flight | 🟡 | `CancelStream` drops the receiver; no HTTP-level timeout/abort. |
| Retry / backoff on transient errors | ⬜ | — |
| Image generation | ✅ | Image models (`gpt-image-*`, `dall-e-*`) are routed to `POST /v1/images/generations` instead of chat completions (which 503s them). The PNG is saved under `./aitui-images/img-<ts>.png` and the path is reported back over the normal stream channel; handles both `b64_json` and `url` responses. `api::is_image_model` gates the routing. |
| Token accounting | 🟡 | Requests set `stream_options.include_usage`; the final usage frame → `StreamEvent::Usage`, shown top-right of the chat pane (`↑prompt ↓completion · total`). Mock mode estimates (~4 chars/token). No cost/pricing yet. |

## Agentic workflow

| Feature | Status | Notes |
|---------|:------:|-------|
| Agent mode toggle (per session) | ✅ | `Ctrl-A` / `:agent`; default-on via config. |
| Tool catalogue (15 tools) | ✅ | read/write/edit/append/list/search/shell/delete_file · make_dir/move_path/copy_path/delete_dir · web_search/web_fetch/download_file. `read_file` takes optional `offset`/`limit` line window (+60k cap on whole-file reads); `list_dir` takes `depth` (indented tree, skips .hidden/target/node_modules, 400-entry cap); `search_files` uses ripgrep (regex, gitignore-aware, binary-skip, optional `glob`) with the literal-substring walker as fallback. |
| Web access (search / fetch / download) | ✅ | `web_search` (DuckDuckGo keyless **HTML** endpoint → real result links + snippets, `uddg` redirects decoded; the old IA-JSON API returned nothing for news/most queries), `web_fetch` (URL→text, HTML stripped; reports plainly when a JS-rendered page has no readable text instead of a blank "ok"), `download_file` (URL→file). Run on the `spawn_blocking` tool thread via `Handle::block_on`; http(s)-only, 20s timeout. |
| Filesystem management | ✅ | Create dirs, move/rename, recursive copy, recursive delete — alongside the original read/write/edit/append/delete-file. |
| Tool invocation via ```` ```tool ```` fences | ✅ | Fallback path (`:native off`, or auto after a tools-rejection). |
| **Native function-calling** | ✅ | D-017: sends `tools` schemas; the model returns structured `tool_calls` (streamed deltas accumulated by index in `api/client.rs`, synthesized into an internal ```tool fence so render/execute/cut are unchanged). `api_messages(native)` translates stored turns → `assistant.tool_calls` + `role:"tool"` with `tool_call_id`. Config `api.native_tools` (default on) / `:native`; auto-falls back to fenced if the endpoint 400s on `tools`. |
| Permission prompts + risk levels | ✅ | Low/Medium/High. 8-option menu (↑↓ + ⏎, or `a`/`d` quick once): **allow / deny** × **once · all of this tool type · all in this directory · everything for 10 min**. Scoped choices persist for the session as `PermissionRule`s (kind/directory/timed); deny rules beat allow; timed rules auto-expire. Directory scope resolves via `ToolCall::permission_directory` against the session cwd. |
| Auto-approve read-only tools | ✅ | Configurable; seeded as kind-scoped allow rules. |
| Per-session permission memory | ✅ | `PermissionMemory` holds `always_allow`/`always_deny` (kind) + scoped `rules` (Kind/Directory/Timed); `check(call, cwd)` prunes expired then returns Allow/Deny/ask. |
| Multi-round tool loop + loop guard | ✅ | Capped at 25 rounds. |
| Offline mock/test backend | ✅ | `api/mock.rs` turns messages into real tool calls (`read`, `write`, `edit`, `run`, `demo`, …) so the whole agent loop runs with no API. Auto-on when endpoint empty / `AITUI_MOCK` / `:mock`. |
| Streaming tool-call parsing | ✅ | Per-token `extract_tool_calls` on the partial drives two things: (1) in agent mode the stream is **cut** the moment a complete tool call appears (D-016) — no more runaway turns of redundant calls — and (2) read-only calls are **speculatively pre-run** (below). The `StreamingParser` state machine remains unused. |
| Speculative read-only tool pre-exec | ✅ | D-014: while a reply streams, complete `read_file`/`list_dir`/`search_files` calls are pre-run in the background (keyed by `hash(name,args)` in `spec_results`); `execute_tool` uses the cached result instantly. Never speculates writes/edits/deletes/shell/network. |
| Tool sandboxing / path-escape guards | ⬜ | Executor runs against cwd with no boundary checks. |
| Diff / content preview for edits/writes | ✅ | `edit_file` renders a `-`/`+` diff at the call; write previews are collapsible (D-018). **On update**, the executor also emits a real old→new line diff in the tool *result* (`write_file`/`edit_file`/`append_file`, `line_diff` — common prefix/suffix stripped); tool-result output colours `+`/`-`/`@@` lines (green/red/accent), so `git diff` output is coloured too. New files report "Created", existing "Updated". |
| `edit_file` unique-match safety | ⬜ | Replaces **all** occurrences via `str::replace`. |

## Sessions & persistence

| Feature | Status | Notes |
|---------|:------:|-------|
| Multiple sessions (keybind-driven) | ✅ | `Ctrl-N/P` cycle, `Ctrl-S` picker, `:new`/`:delete`. Sidebar removed (D-005). |
| Startup launcher (resume / new) | ✅ | Modal launch screen when any saved session has messages: pick a session to resume or start fresh. Resuming `cd`s to the session's saved folder. `j/k` · `⏎/l` open · `n` new · `Esc` resume current. |
| Parallel sessions | ✅ | Each session streams independently — start/`⑂`-fork a session mid-generation and work there while the first keeps running in the background (`App.streams` is a per-session `Vec`; events route by session id). `is_busy` is per-active-session; the spinner reflects any session. Agent tool rounds are serialized across sessions (`agent_session` + `agent_queue`) since they share one permission UI. |
| Fork session | ✅ | `⑂` (`Ctrl-Y` / `:fork`) duplicates the active session (messages, prompt, agent mode, cwd) into a new branch and switches to it. |
| Per-session working directory | ✅ | Each session records its `cwd`; resuming (launcher or session picker) `cd`s there so file tools / `@`-mentions resolve against the right project. |
| Auto-naming from first message | ✅ | — |
| Manual rename | ✅ | `:rename`. |
| JSON persistence | ✅ | `~/.config/aitui/sessions.json` (now includes each session's `cwd`). |
| Per-session system prompt | ✅ | Settings overlay / `:system`. |
| Global system prompt (config.toml) | ✅ | `[api] system_prompt` in `config.toml` — prepended to every request; per-session prompts stack on top. |
| Send lock while assistant is working | ✅ | No parallel turns yet: `App::is_busy()` (streaming / draining / tools / permission) blocks a new send and pops a `Notice` dialog, but the input stays editable so a follow-up can be composed. Ctrl-C cancels. |
| Skills (toggleable personas / instructions) | ✅ | Markdown files in `~/.config/aitui/skills/` (stem = name); `:skills` picker toggles them (⏎ toggle, stays open, ✓ marks active). Active skills injected as system messages on each request. Seeds a sample `caveman.md` on first run. Status bar shows `✦N` active. Add one = drop a `.md`. |
| Sticky skills | ✅ | Active skills persist across restarts (`~/.config/aitui/active_skills.json`); `ui.sticky_skills` config (default on) toggled at runtime with `:sticky`. |
| Tool-event timeline | ⬜ | `tool_events` field exists but unused. |

## Input & editing

| Feature | Status | Notes |
|---------|:------:|-------|
| Vim-modal input (Normal/Insert/Visual/Command/Operator) | ✅ | Full vim editing on the input box. **Visual mode** now selects (char-wise, multi-line): `v` starts, motions extend, `y` yank / `d`,`x` delete / `c`,`s` change; selection is reverse-video highlighted. Mode shown as a coloured status-bar chip (NORMAL blue / INSERT green / VISUAL magenta / COMMAND yellow). |
| Word / line editing keys | ✅ | Ctrl-W & Ctrl-Backspace delete the previous word; Ctrl-Delete the next word (in insert & command line). |
| Reasoning effort (model versions) | ✅ | `:effort [low\|medium\|high\|off]` (or cycle with bare `:effort`) sets the OpenAI `reasoning_effort` request field for GPT-5 / o-series; `[api] reasoning_effort` config default; shown as a `🧠` status chip. |
| Open conversation in `$EDITOR` (Ctrl-O) | ✅ | Suspends TUI, opens transcript `.md` in nvim/`$EDITOR`, restores. Opens on the **last line** for vim-family editors (`+`), so the latest turn is focused. |
| Vim file browser (Ctrl-E / Ctrl-F) | ✅ | h/j/k/l navigate, Space multi-select, Enter open-all (or current); l/Enter open file or enter dir, h parent. Both Ctrl-E (edit) and Ctrl-F (attach) toggle the browser open/closed; Ctrl-G also closes it. Edited files pre-selected. Opens in `$EDITOR` (multi-file) / attaches. |
| Edited-files tracker | ✅ | Successful write/edit/append tracked (delete removes); status bar shows `✎N`; pre-selected in the Ctrl-E browser. |
| Drop into a shell (Ctrl-G) | ✅ | Suspends TUI → `$SHELL` → returns (`:shell`/`:term`). |
| Multi-line composer | ✅ | Enter sends (= `:w`); Shift/Alt-Enter inserts a newline (needs terminal keyboard-enhancement for Shift+Enter). |
| Command line (`:w`, `:q`, `:new`, …) | ✅ | With history + navigation. |
| Command palette | ✅ | Fuzzy. |
| `@path` file-mention completion | ✅ | Fuzzy file search, inlines file content on send. |
| Smart paste (bracketed) | ✅ | A pasted blob arrives as one `Event::Paste` (bracketed paste enabled in `tui.rs`). Very large (≥50k chars) → written to `./aitui-pastes/paste-<ts>.txt` and attached; medium (≥5 lines or ≥400 chars) → stored and shown as a compact `[PASTED#N-Llines-Cchars]` chip, expanded back to full text on send; small → inserted verbatim. |
| File attachment picker | ✅ | `Ctrl-F`; directory browsing. |
| Image attachments (base64) | ✅ | png/jpeg/gif/webp. |
| Configurable keybindings | ✅ | All action/mode bindings in `[keybinds]` (config.toml), parsed into a precompiled `Keymap`; help overlay shows live bindings. Vim motions stay fixed. Descriptive nvim-style **aliases** accepted (e.g. `insert_to_normal`, `normal_insert`, `send_message`, `toggle_help`, `open_file_picker`, `open_model_picker`); `insert_to_normal` may be a 2-key chord like `jk`. |
| Transcript scrollbar w/ turn markers | ✅ | One-column scrollbar on the transcript's right: proportional thumb + coloured pips marking each turn (cyan = you, gray = assistant, green = tool). `ui/scrollbar.rs`, fed by `RenderedLine.role_start`. |

## UI / UX

| Feature | Status | Notes |
|---------|:------:|-------|
| Borderless UI, terminal colours only | ✅ | D-012: no borders/custom RGB. Turns separated by a coloured left **gutter bar** per role (`▎`, ANSI fg) + blank gaps; tool turns **nest** their bar inside the assistant's (`▎▎`) as children; overlays/input/help are `Clear`+padding+title; selection is reverse-video. Follows the terminal's light/dark theme. `mark_gutter`/`role_gutters` in `render/document.rs`. |
| Flat single-column layout (transcript / input / status) | ✅ | No sidebar (Claude-Code-like). |
| Auto-scroll to bottom on any tool/command | ✅ | Streaming, tool results, session switch, and toggling tool output all stick to the bottom line. |
| Markdown + code-block rendering | ✅ | Via `domain/blocks` + `render/document`. Headings (`#`–`#####`), bullet + ordered lists, block-quotes, and thematic breaks (`---`/`***`/`___` → full-width horizontal rule). Code frames use the accent colour (brighter than the old faint border). |
| Status bar (coloured chips + spinner) | ✅ | Each status (MOCK/agent/output/✎edited/✦skills/model) is a solid **background chip**; "working" shows an animated braille **spinner** instead of the word. |
| Tree-sitter syntax highlighting | ✅ | `render/highlight.rs` highlights fenced code blocks, `read_file` results, and write/edit previews. Grammars: rust, python, js/jsx, ts/tsx, json, bash, go, c, css, html. One-shot full parse (no incremental — previews are static & doc-cached; see D-011). Compiled per-language configs cached thread-local. |
| Token counter (top-right) | ✅ | Last response's `↑prompt ↓completion · total` overlaid on the chat pane's top-right when the endpoint reports usage. |
| Animated "preparing tool call" | ✅ | D-018: while a tool call streams, the raw partial JSON is replaced by an animated `⠿ Preparing <tool>…` chip (tool name resolves live); the assistant's interstitial prose in that streaming turn is hidden so only the forming call + reasoning show. |
| Collapsible write previews | ✅ | D-018: `write_file` calls show a one-line header collapsed by default; **click to expand** the full syntax-highlighted content that was written (like long tool results). |
| Tool output show/hide toggle | ✅ | Long tool output collapses by default. `Ctrl-T` expands/collapses **all** output (shown as an independent `output` status chip — no status-line spam). **Click anywhere on a collapsed tool block** to expand just that one. The click **preserves your reading position** — it only reveals/sticks-to-bottom if you were already at the bottom; toggling while scrolled up leaves the scroll put. |
| Unicode-aware wrapping | ✅ | `unicode-width`. |
| Minimal flat theme | ✅ | Trimmed to the few ANSI colours a flat UI needs. |
| Help overlay | ✅ | `?` — updated for the new keymap. |
| Transcript scrolling | ✅ | Wheel · PgUp/PgDn · Ctrl-Home/End. No cursor (by design). |
| Mouse support | 🟡 | Wheel scroll only; click-to-focus removed with the `Focus` concept. |
| `ui/` widget refactor | ✅ | `render/` = document model, `ui/` = widgets; sidebar deleted. |

## Performance

| Feature | Status | Notes |
|---------|:------:|-------|
| Per-message render cache | ✅ | D-013: each finalized message's rendered rows are cached by content signature (`App.doc_cache`); a streamed token only re-parses/highlights/wraps the one streaming message, not the whole transcript. Streaming cost is flat regardless of history length. |
| Event-driven redraw | ✅ | D-015: `main.rs` draws only when `dirty` or animating; `event::poll` 33 ms while streaming / 250 ms idle. Idle CPU near zero. |
| Non-blocking session save | ✅ | `SessionManager::save` moves serialize + `fs::write` to `spawn_blocking` when a tokio runtime is present (falls back to sync for tests), so finishing a turn doesn't hitch the UI. |
| Cached `@`-mention file list | ✅ | `find_project_files` result cached on `App` (~5 s TTL); typing `@` filters an in-memory list instead of walking the filesystem per keystroke. |

## Config & security

| Feature | Status | Notes |
|---------|:------:|-------|
| TOML config at `~/.config/aitui/` | ✅ | Auto-written on first run. |
| Env-var overrides (`AITUI_ENDPOINT`, `AITUI_API_KEY`) | ✅ | Added 2026-06-30. |
| No secrets baked into binary | ✅ | Hardcoded key removed 2026-06-30. |
| Settings overlay (live edit) | ✅ | Agent default, auto-approve, sizes, system prompt. |
| API setup prompt | ✅ | `:setup` (or the command palette) opens a URL + key modal; auto-pops when a request fails with a missing/relative endpoint ("relative url without a base"). On confirm it saves to config and rebuilds the client. |

## Testing

| Area | Status | Notes |
|------|:------:|-------|
| Reducer unit tests | ✅ | Extensive. |
| Session / manager tests | ✅ | — |
| Executor (tool) tests | ✅ | — |
| Parser tests | ✅ | — |
| State helper tests (fuzzy/mentions) | ✅ | — |
| Agent-loop integration test | ⬜ | No end-to-end coverage. |
| API client / SSE parse tests | 🟡 | `stream.rs` parsing is the natural next target. |
