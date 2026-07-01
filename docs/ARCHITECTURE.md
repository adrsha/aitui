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

A single synchronous loop, but **event-driven**: it only redraws when something
changed rather than spinning at a fixed frame rate.

1. **Draw (conditional)** — render only when `dirty` (an event or channel activity
   this pass) or `animating` (a stream/tool is in flight and the spinner needs to
   tick). Layout + the chat document are recomputed here (doc rebuilt only if stale).
2. **Poll input** — `event::poll` blocks up to **33 ms while animating** (smooth
   spinner) or **250 ms when idle** (near-zero idle CPU); one crossterm event →
   actions → dispatch, and marks `dirty`.
3. **Drain channels** — model list, stream tokens, agent tool results, and
   **speculative read-only tool results** → actions / caches. Any activity marks `dirty`.
4. **Quit check.**

`dispatch()` runs a small work-queue: it applies each action and pushes any
follow-up action returned by the reducer, so one keystroke can fan out into a
short deterministic chain (e.g. `Submit → AttachStream`).

> The tokio runtime is entered via `_guard`; the loop stays synchronous and
> offloads async work to spawned tasks. The redraw is gated on change, so idle
> CPU is negligible and streaming still animates (ROADMAP Phase 6).

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

**Stream cut on tool detection:** the fenced protocol is "emit a tool block and
nothing after it." In agent mode, each `StreamToken` checks `should_cut_stream` —
if the partial already holds a *complete* tool call, the stream is finalized
immediately, its handle dropped (which aborts the backend generation), and
`cut_stream` is set. The main loop then dispatches `StartAgentRound` on a **clean
pass** (after the batch's leftover tokens have drained and no-op'd on the finalized
session), so the round starts without stale tokens bleeding into the next stream.
This stops a model that expects inline tool results from spiralling into a whole
turn of redundant calls — the dominant source of wasted tokens / perceived slowness.

**Speculative pre-execution:** while the reply is still streaming, each
`StreamToken` runs `effects::speculate_read_tools`, which scans the partial with
`agent::parser::extract_tool_calls` and pre-runs any *complete, side-effect-free
read-only* call (`read_file`/`list_dir`/`search_files`) in the background, keyed by
`hash(name,args)` in `App.spec_results`. When the round reaches that call,
`execute_tool` uses the cached result instantly instead of re-running it. Writes,
edits, deletes, shell, and network tools are never speculated. (With the cut above,
the speculated read is typically already done the instant the round starts.)

> **Native function-calling (D-017):** with `api.native_tools` on, the request
> carries `tools` schemas and the model returns structured `tool_calls`; the client
> accumulates the deltas and synthesizes an internal ```tool fence, so the pipeline
> above is unchanged. `Session::api_messages(native)` translates stored turns to
> `assistant.tool_calls` + `role:"tool"` with `tool_call_id`. In fenced mode (or as
> the auto-fallback when an endpoint rejects `tools`), tool results are instead
> **remapped to `"user"`** so plain completion endpoints accept them.

### Rendering
State is rebuilt into a cached document only when `content_rev` (bumped by
`App::touch()`) or the width changes. `ChatState` holds scroll/cursor over the
rendered rows.

The rebuild itself is **per-message incremental** (`App.doc_cache: render::chat::
DocCache`): each finalized message's rendered rows are cached under a content
signature (role + text + its collapse toggles), so a streamed token only re-parses,
re-highlights, and re-wraps **the one streaming message**, not the whole transcript.
The live streaming partial is always rebuilt fresh; width / show-output changes drop
the whole cache. This keeps streaming cost flat regardless of conversation length.

## Known structural debt (tracked in ROADMAP)

- **Renderer split settled:** `render/` = document model, `ui/` = widgets. The old
  dead `render/chat.rs::render` path is gone; the build is down to 2 pre-existing
  WIP warnings (`Action::InputHistory{Prev,Next}`, `InputBuffer::is_selected`).
- **Prompt-fenced tools:** the agent depends on the model emitting ```` ```tool ````
  blocks; brittle vs. native function-calling. Schemas exist but are unused.
- **No request timeout / retry / cancellation mid-tool.**
- **`edit_file` replaces all occurrences** (`str::replace`), not a unique match.
