# Architecture — the skeleton

AiTUI follows a **unidirectional, Elm-style** architecture. Input events become
`Action`s; a single reducer applies each action to the `App` state and may
return one follow-up action; side effects (network, tool execution) run on
`tokio` tasks and report back through channels as more actions. Rendering is a
pure function of state.

```
            ┌────────────────────────────────────────────────────┐
            │                      main.rs                        │
            │  event loop: draw → poll input → drain channels     │
            └───────────────┬──────────────────────┬─────────────┘
                            │                      │
                  crossterm event           tokio channels
                            │              (stream / models / tool)
                            ▼                      ▼
                  input::handler ──► Vec<Action> ──► dispatch()
                            │                      │
                            ▼                      ▼
                  ┌───────────────────────────────────────┐
                  │  App::apply(Action) -> Option<Action>  │   reducer.rs
                  │  (the ONLY place state mutates)        │
                  └──────────────┬────────────────────────┘
                                 │ calls into
                                 ▼
                         effects.rs  (submit, stream, agent tool loop)
                                 │ spawns
                                 ▼
                     api::client / agent::execute  ──► channel ──► Action
```

## The event loop (`main.rs`)

A single synchronous loop, ticking at ~16 ms (`event::poll` timeout):

1. **Draw** — recompute layout, rebuild the chat document if stale, render.
2. **Poll input** — translate one crossterm event into actions, dispatch.
3. **Drain channels** — model list, stream tokens, agent tool results → actions.
4. **Quit check.**

`dispatch()` runs a small work-queue: it applies each action and pushes any
follow-up action returned by the reducer, so one keystroke can fan out into a
short deterministic chain (e.g. `Submit → AttachStream`).

> ⚠️ The tokio runtime is entered via `_guard` but the loop itself is blocking.
> Async work is offloaded to spawned tasks. This is fine today; revisit if the
> draw/poll cadence ever needs to be event-driven (see ROADMAP Phase 6).

## Module map

| Module | Responsibility |
|--------|----------------|
| `main.rs` | Runtime init, the event loop, channel draining, `dispatch`. |
| `tui.rs` | Terminal setup/teardown (raw mode, alt screen). |
| **`app/`** | The core state machine. |
| `app/state.rs` | `App` struct (all state) + pure helpers (mentions, fuzzy, file walk). |
| `app/action.rs` | The `Action` enum — every possible state transition. |
| `app/reducer.rs` | `App::apply` — the single mutation funnel. Heavily unit-tested. |
| `app/effects.rs` | Side-effecting methods: `submit`, `begin_stream`, agent tool loop, chat-doc building. |
| `app/input_buffer.rs` | Multi-line text buffer with vim-style editing primitives. |
| `app/overlay.rs` | Overlay state: pickers, palette, settings, permission prompt, mentions. |
| **`api/`** | OpenAI-compatible HTTP/SSE client. |
| `api/client.rs` | `ApiClient`: `stream()` (SSE → `StreamEvent`s) and `fetch_models()`. |
| `api/models.rs` | Wire types: `ChatRequest`, `ChatMessage`, `MessageContent`, content parts. |
| `api/stream.rs` | SSE line parsing (`data:` framing, `[DONE]`, delta extraction). |
| **`agent/`** | The agentic layer. |
| `agent/tools.rs` | Tool catalogue: `ToolKind`, risk levels, JSON schemas, system prompt, permission memory. |
| `agent/parser.rs` | Extract ```` ```tool ```` fenced calls from model text (+ a streaming parser, currently unused). |
| `agent/executor.rs` | Actually run a `ToolCall` on the filesystem/shell → `ToolResult`. |
| **`domain/`** | Pure domain model. |
| `domain/session.rs` | `Session` + `SessionManager`: history, streaming accumulation, JSON persistence. |
| `domain/blocks.rs` | Parse a message body into renderable `Block`s (markdown, code, think, tool). |
| **`render/`** | Document model: turn messages into wrapped, styled rows. |
| `render/document.rs` | `build()` — blocks → `RenderedLine`s; link extraction. |
| `render/chat.rs` | `ChatState` — scroll/cursor/selection over the rendered document + doc cache. |
| `render/wrap.rs` | Unicode-aware line wrapping. |
| `render/theme.rs` | Color themes. |
| **`ui/`** | Ratatui widgets (the in-progress refactor target). |
| `ui/mod.rs` | Top-level `render(frame, app)`; composes the panels. |
| `ui/layout.rs` | Splits the frame into sidebar / chat / input / statusbar rects. |
| `ui/{chat,sidebar,input,statusbar,overlay,help}.rs` | Per-panel widgets. |
| **`config/`** | `Config` load/save (TOML at `~/.config/aitui/config.toml`, env overrides). |
| **`files/`** | File reading + image encoding (base64) for attachments. |
| `input/handler.rs` | Event → `Action` translation (focus- and mode-aware). |
| `input/vim.rs` | `VimMode` enum and helpers (Normal/Insert/Visual/Command/Operator). |

## Key data flows

### Sending a message
`Submit` → `effects::submit` expands `@mentions`, builds a `ChatMessage`, pushes
it, auto-names the session → `begin_stream` opens an SSE `mpsc::Receiver` →
returns `AttachStream(rx)`. The loop drains `rx`, dispatching `StreamToken` /
`StreamReasoning` / `StreamDone`. `StreamDone` finalizes the message, persists,
and — in agent mode — kicks off the tool round.

### Agent tool round
`StreamDone` (agent mode) → `start_agent_round` parses tool calls from the last
assistant message → `process_next_tool` checks `PermissionMemory`:
- auto-allowed → `execute_tool` (spawned on `spawn_blocking`) → `AgentToolResult`
- needs approval → opens a `Permission` overlay (`a`/`A`/`d`/`D`)
Each result is recorded as a `tool` message; when the queue drains,
`continue_after_tools` streams the model's next turn. A loop guard caps rounds at
`MAX_AGENT_ITERATIONS = 25`.

> Tool results are stored as `role: "tool"` messages but **remapped to `"user"`**
> in `Session::api_messages()` so plain completion endpoints accept them. Native
> function-calling (Phase 2) will replace this with proper `tool` role + `tool_call_id`.

### Rendering
State is rebuilt into a cached document only when `content_rev` (bumped by
`App::touch()`) or the width changes. `ChatState` holds scroll/cursor over the
rendered rows. This keeps redraws cheap during streaming.

## Known structural debt (tracked in ROADMAP)

- **Dual renderers:** `render/` and `ui/` coexist mid-refactor; several `ui/`
  functions and `render/chat.rs::render` are dead code today (~24 warnings).
- **Prompt-fenced tools:** the agent depends on the model emitting ```` ```tool ````
  blocks; brittle vs. native function-calling. Schemas exist but are unused.
- **No request timeout / retry / cancellation mid-tool.**
- **`edit_file` replaces all occurrences** (`str::replace`), not a unique match.
