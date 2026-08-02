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
3. **Drain channels** — model list, session stream tokens, agent tool results,
   parallel sub-agent progress/results, access judgments, and **speculative
   read-only tool results** → actions / caches. Any activity marks `dirty`.
4. **Quit check.**

`dispatch()` runs a small work-queue: it applies each action and pushes any
follow-up action returned by the reducer, so one keystroke can fan out into a
short deterministic chain (e.g. `Submit → AttachStream`). Model discovery follows
the same reducer/effect split: startup, API setup, and `:reload-models` call
`App::refresh_models()`, which flips the model chip to loading and spawns a
`/v1/models` fetch whose result is drained by the main loop into `ModelsLoaded` or
`ModelsFailed`.

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
| `app/overlay.rs` | Overlay state: pickers, palette, settings, permission prompts, rich decisions (listed/custom/edited answers), mentions. |
| **`api/`** | OpenAI-compatible HTTP/SSE client. |
| `api/client.rs` | `ApiClient`: `stream()` (SSE → `StreamEvent`s) and `fetch_models()`. |
| `api/models.rs` | Wire types: `ChatRequest`, `ChatMessage`, `MessageContent`, content parts. |
| `api/stream.rs` | SSE line parsing (`data:` framing, `[DONE]`, delta extraction). |
| **`agent/`** | The agentic layer. |
| `agent/tools.rs` | Five model-visible tool categories, operation-level `ToolKind` resolution, risk levels, JSON schemas, system prompt, and permission memory. |
| `agent/parser.rs` | Extract `<tool>…</tool>` calls (and legacy ```` ```tool ```` fences) from model text (+ a streaming parser, currently unused). |
| `agent/executor.rs` | Actually run a `ToolCall` on the filesystem/shell → `ToolResult`. |
| `agent/subtask.rs` | Runs isolated read-only child-agent loops, including safe build/test shell filtering and progress/result events. |
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

### Render/UI boundary

Rendering is split into two layers with a hard ownership line:

- `render/` owns the terminal-independent document model. It converts session
  messages into wrapped `RenderedLine`s, records link spans, and maintains chat
  scroll/cursor/cache state. It must not draw Ratatui widgets or know about frame
  layout.
- `ui/` owns Ratatui presentation. It splits the frame, composes panels, applies
  borders/status/help/overlays, and paints `render/`'s document rows into widgets.
  It must not parse message bodies, wrap markdown/code, or duplicate chat-document
  construction.

The intended data flow is one-way:

```text
Session messages → domain::blocks → render::document/chat → ui::{chat,...} → Frame
```

That boundary keeps parsing/wrapping testable without a terminal backend and keeps
widget layout changes from creating a second rendering path.

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
- an already-authorized consecutive read-only prefix becomes one bounded parallel wave;
- mutation, shell, interaction, todo, child-agent, download, unknown, and permission
  boundaries remain sequential barriers;
- needs approval → opens a `Permission` overlay (`a`/`A`/`d`/`D`).
Parallel read results may finish in any order but are buffered and recorded as `tool`
messages in source order. Speculative cache hits occupy their original result slots.
When the queue drains, `continue_after_tools` streams the model's next turn. A loop
guard caps rounds at `MAX_AGENT_ITERATIONS = 25`.

### Parallel child-agent barrier
Consecutive `workflow(action: "agent")` calls are removed from the parent queue as
one batch. `effects::start_task_batch` launches one `agent::subtask::run` future per
call, records live child-agent state, creates one transcript entry per child in launch
order, and installs a `TaskBarrier`. The activity row becomes a one-based, horizontally
windowed child-agent tab strip; click or `Alt-[` / `Alt-]` switches the highlighted
agent and opens its scrollable detail view. Optional one-based `task_index` metadata
links a child to the parent checklist subtask it owns.

Child agents receive the active model and reasoning settings plus a bounded tool surface:
local read/list/search, read-only web research, recognized build/test/run shell commands,
a child-local `workflow(todo)` checklist, and recursively bounded `workflow(agent)`
delegation. Nested agents remain read-only, are capped by depth and batch size, and cannot
modify the parent checklist. Their lifetime uses a renewable progress lease plus per-operation
and absolute wall-clock deadlines; soft/hard request budgets remain deterministic cost guards.
When any soft bound is reached, the tool surface is removed for one final synthesis request so
partial evidence becomes a report instead of an abrupt limit error. All bounds are configurable
under `[agent]` and inherited by nested children. `main.rs` drains `SubtaskEvent`s; progress
updates both the tab/detail view and that child's transcript entry in place. Completion changes
the same entry to completed/failed immediately, while the parent still waits at the barrier until
every sibling finishes. Later dependent tools therefore never observe a partial batch.

The parent remains responsible for permission-bearing operations, mutations, reconciling
conflicting reports, integration, and final verification. For load-bearing checks, an
`agent` call can include stable `checks` plus `verification = replicate`. AiTUI
then runs two context-isolated replicas concurrently, validates their bounded structured
reports and local file evidence, and votes only over typed evidence-backed answers.
Agreement returns one compact deterministic summary. Disagreement or weak evidence adds
a third isolated replica, then an independent verifier only if the result remains
unresolved; malformed reports fail closed to `unknown`. Raw sibling reasoning is never
added to replica context, and only disputed claims/evidence enter verifier context.
Cancelling the parent aborts unfinished child workers, releases the barrier, marks those
children failed in both UI and transcript, and ignores any already-queued late events.

### Categorized tool contract
The model sees five native functions: `file_management`, `shell`, `web`,
`interaction`, and `workflow`. All except `shell` carry an `action` enum. A
`ToolCall` resolves `(category, action)` to an operation-level `ToolKind` before
permission checks, previews, speculative execution, UI interception, and executor
dispatch. This keeps the model-facing catalogue small while preserving distinct
risk and permission scopes for operations such as read, edit, and delete.

`file_management` includes read/write/edit/list/search/mkdir/move/copy/delete;
`web` includes search/fetch/download; `interaction` includes ask/propose/plan; and
`workflow` includes todo/agent/finish. `todo` is the numbered main checklist (displayed
as subtasks); `agent` is a parallel child execution context and accepts optional one-based
`task_index` ownership metadata. The legacy `task` spelling remains parseable for old
sessions but is no longer advertised. Redundant `append` and `complete_step` operations
were removed: file changes use exact edit or whole-file write, and step completion
is represented by marking the active todo item done after verification.

### Adaptive decisions
`interaction(action: "ask")` and `interaction(action: "propose")` are UI-handled
operations inside the agent queue. Ask covers missing information and ordinary
choices. Propose is reserved for a current step with at least two viable
implementation paths; obvious single-path work bypasses the decision overlay. Both
use `DecisionRequest`: wrapped/scrolling options, a full selected-option detail pane,
a virtual custom-response row (`Tab`), and selected-option editing through `$EDITOR`
(`e` → `DecisionReadback` → `AgentDecisionEdited`). The result is recorded as a normal
tool message, so the next model turn receives the exact listed, edited, or custom text.

Agent prompt enforces one active todo step at a time and todo checks at step start/end.
When multiple paths exist, alternatives must explain tradeoffs, feasibility, and planned
actions; impossible selected paths must be reported immediately with feasible choices.

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
read-only* call (a `file_management` action resolving to `read`, `list`, or
`search`) in the background, keyed by `hash(name,args)` in `App.spec_results`. When the round reaches that call,
`execute_tool` uses the cached result instantly instead of re-running it. Writes,
edits, deletes, shell, and network tools are never speculated. (With the cut above,
the speculated read is typically already done the instant the round starts.)

> **Native function-calling (D-017):** with `api.native_tools` on, the request
> carries `tools` schemas and the model returns structured `tool_calls`; the client
> accumulates the deltas and synthesizes an internal `<tool>` block, so the pipeline
> above is unchanged. `Session::api_messages(native)` translates stored turns to
> `assistant.tool_calls` + `role:"tool"` with `tool_call_id`. In fenced mode (or as
> the auto-fallback when an endpoint rejects `tools`), tool results are instead
> **remapped to `"user"`** so plain completion endpoints accept them.

### Rendering
State is rebuilt into a cached document only when `content_rev` (bumped by
`App::touch()`) or the width changes. `ChatState` holds scroll/cursor over the
rendered rows. Session navigation metadata stays outside that document cache: the
main loop publishes a per-process heartbeat every 500 ms, `SyncSessions` reads
other live clients plus the merged session file, and `ui::{topbar,overlay,statusbar}`
render compact tabs, the detailed sessions menu, and the active model/cwd respectively.

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
- **Prompt-tagged tools:** the agent depends on the model emitting `<tool>…</tool>`
  blocks; brittle vs. native function-calling. Schemas exist but are unused.
- **No request timeout / retry / cancellation mid-tool.**
- **Unrestricted tool paths:** file tools resolve relative paths against the session
  cwd, but do not treat it as a containment boundary. Absolute paths, `..` paths,
  and paths traversing symlinks outside the cwd execute after normal permission
  handling.
