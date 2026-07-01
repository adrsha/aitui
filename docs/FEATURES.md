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
| Token accounting | 🟡 | Requests set `stream_options.include_usage`; the final usage frame → `StreamEvent::Usage`, shown top-right of the chat pane (`↑prompt ↓completion · total`). Mock mode estimates (~4 chars/token). No cost/pricing yet. |

## Agentic workflow

| Feature | Status | Notes |
|---------|:------:|-------|
| Agent mode toggle (per session) | ✅ | `Ctrl-A` / `:agent`; default-on via config. |
| Tool catalogue (15 tools) | ✅ | read/write/edit/append/list/search/shell/delete_file · make_dir/move_path/copy_path/delete_dir · web_search/web_fetch/download_file. |
| Web access (search / fetch / download) | ✅ | `web_search` (DuckDuckGo keyless IA JSON), `web_fetch` (URL→text, HTML stripped), `download_file` (URL→file, e.g. images). Run on the `spawn_blocking` tool thread via `Handle::block_on`; http(s)-only, 20s timeout. |
| Filesystem management | ✅ | Create dirs, move/rename, recursive copy, recursive delete — alongside the original read/write/edit/append/delete-file. |
| Tool invocation via ```` ```tool ```` fences | ✅ | Works, but brittle (see Phase 2). |
| **Native function-calling** | ⬜ | **Target mechanism** — schemas exist, unwired. |
| Permission prompts + risk levels | ✅ | Low/Medium/High; allow-once/all, deny-once/all. |
| Auto-approve read-only tools | ✅ | Configurable. |
| Per-session permission memory | ✅ | `always_allow` / `always_deny`. |
| Multi-round tool loop + loop guard | ✅ | Capped at 25 rounds. |
| Offline mock/test backend | ✅ | `api/mock.rs` turns messages into real tool calls (`read`, `write`, `edit`, `run`, `demo`, …) so the whole agent loop runs with no API. Auto-on when endpoint empty / `AITUI_MOCK` / `:mock`. |
| Streaming tool-call parsing | 🟡 | `StreamingParser` exists but unused; calls parsed post-turn. |
| Tool sandboxing / path-escape guards | ⬜ | Executor runs against cwd with no boundary checks. |
| Diff / content preview for edits/writes | ✅ | `edit_file` renders a `-`/`+` diff and `write_file` previews its body inline (capped at 40 lines), both syntax-highlighted by the file's extension. |
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
| Tool output show/hide toggle | ✅ | Long tool output collapses by default. `Ctrl-T` expands/collapses **all** output (shown as an independent `output` status chip — no status-line spam). **Click anywhere on a collapsed tool block** to expand just that one (`toggle_at_viewport_row` falls back to the block's message, so the role label / gutter / summary all work); its tail scrolls into view. |
| Unicode-aware wrapping | ✅ | `unicode-width`. |
| Minimal flat theme | ✅ | Trimmed to the few ANSI colours a flat UI needs. |
| Help overlay | ✅ | `?` — updated for the new keymap. |
| Transcript scrolling | ✅ | Wheel · PgUp/PgDn · Ctrl-Home/End. No cursor (by design). |
| Mouse support | 🟡 | Wheel scroll only; click-to-focus removed with the `Focus` concept. |
| `ui/` widget refactor | ✅ | `render/` = document model, `ui/` = widgets; sidebar deleted. |

## Config & security

| Feature | Status | Notes |
|---------|:------:|-------|
| TOML config at `~/.config/aitui/` | ✅ | Auto-written on first run. |
| Env-var overrides (`AITUI_ENDPOINT`, `AITUI_API_KEY`) | ✅ | Added 2026-06-30. |
| No secrets baked into binary | ✅ | Hardcoded key removed 2026-06-30. |
| Settings overlay (live edit) | ✅ | Agent default, auto-approve, sizes, system prompt. |

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
