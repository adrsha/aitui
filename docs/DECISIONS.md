# Decisions (ADR-lite)

Short records of choices that shape the project. Newest first. When a decision is
reversed, don't delete it — add a new entry that supersedes it.

---

## D-025 — CWD is a relative-path base, not a filesystem sandbox
**Date:** 2026-08-02 · **Status:** accepted · **Supersedes:** D-024 path confinement

Agent file tools may access any path permitted by the operating system and AiTUI's
normal permission flow. The session CWD is used only to resolve relative paths;
absolute paths, `..` traversal, and paths whose symlinks resolve outside the CWD are
not rejected by the executor. The `AgentConfig.sandbox` setting and
`configure_sandbox`/`enforce_sandbox` path guards are removed rather than retained as
an alternate execution mode.

Permission classification and user approval remain separate from containment: risky
or uncovered calls can still prompt or be denied, but an approved call is not blocked
merely because its target lies outside the session CWD.

---

## D-024 — Path sandboxing by default, HTTP idle timeout, prompt stream cancellation
**Date:** 2026-07-31 · **Status:** superseded by D-025 for path handling

Tool paths are confined to the workspace root by default. `AgentConfig.sandbox`
(default `true`) gates write/downcall sites: `enforce_sandbox` canonicalizes the target
path after resolving symlinks and walking `..` ancestors to the deepest existing
directory, and refuses absolute paths and any `..`/symlink escape outside the root.
Reading missing files inside the workspace stays legal, so creates via nonexistent
paths outside the root are also refused. `:config`-level opt-out exists per config
(`default_sandbox()`), keeping the escape hatch explicit rather than silent.

Streaming replies carry a 120s idle timeout (`STREAM_IDLE_TIMEOUT`): no bytes within
the window fails the attempt before any emission (silent) or ends the stream
(partial), instead of hanging forever on a dead backend. Retries on transient errors
happen before first emission with 0.5/1/2s exponential backoff, up to four attempts.

Cancellation is prompt, not "download then discard": the receive loop races
`tx.closed()` against the next byte (`tokio::select!` with the closed branch biased),
so dropping the receiver aborts the connection immediately instead of blocking for
the full idle timeout. The abort flag (`AgentCancel`, `ToolAbort`) kills in-flight
shell process groups every 50ms poll and short-circuits pending tool starts.

---

## D-023 — Keep single-tool JSON-schema child agents; add named agent registry with per-agent tool policy
**Date:** 2026-07-31 · **Status:** accepted

Evaluated migrating the child-agent interface to opencode-style free-form task strings and
one generic "batch" tool. Rejected: JSON-schema `workflow(action: agent)` keeps the parent's
typed substate (model/rounds/lease/verification) intact, yields deterministic reducer
testing, and is the established codebase pattern; opencode's batching is already covered by
the parallel read waves of D-022, and background children duplicate progress-lease
supervision.

Adopted opencode ideas instead:

1. **Named agent registry.** `[agents.<name>]` in `config.toml` defines `description`,
   `model`, optional `role` line, and per-agent tool policy. The `workflow(agent)` schema
   gained an optional `agent` name; `start_task_batch` resolves it — model override, role
   line, and policy apply to the child. Unknown names fall back to the inline prompt, so old
   transcripts stay valid. The agent-mode system prompt advertises the registry (name,
   description, model) so the model can pick a configured agent by name.
2. **Per-agent tool policy.** `ToolPolicy { allow, deny }`: empty allowlist = built-in
   read-only surface (`read`/`list`/`search`/`shell`/`web_search`/`web_images`/
   `reverse_image`/`web_fetch`); allowlist entries widen the surface; `deny` always wins.
   Unlike opencode's glob rules, matching is exact on tool names — no path globs yet, since
   executor-level path sandboxing (tool sandboxing) is still out of scope.

`ToolPolicy` and agent names are a `SubtaskSpec`/`Subtask` concern; the main loop, permission
prompts, and parent tool surface are untouched.

### D-023.1 — Agent tree, per-agent transcripts, and parallel-only delegation
**Date:** 2026-08-01

Child agents form a tree rooted at the main agent (D-020's nesting extended to the UI):
`Subtask` gained `parent_id`; nested children register through `SubtaskEvent::Registered`
and stream their conversation as `SubtaskEvent::Round { role, content }` into
`Subtask.transcript` (assistant replies, tool-call summaries, tool results). Verification
replicas run with `capture: false` and never register nodes. Shared `Arc<AtomicU64>` id
allocator replaces the flat counter so nested ids never collide.

Clicking an agent (sidebar or below-chat panel) sets `view_node`, swapping the chat pane to
the node's own transcript with a `‹ BACK TO ROOT` breadcrumb (`Esc`/click navigate up the
tree); the below-chat panel shows that node's detail plus its clickable children. Sidebar
rows indent by depth and highlight the entered node.

Scheduling contract (prompt + `workflow(agent)` schema): never delegate sequential work —
if one result feeds the next step, do that step in the agent itself. When information is
needed **after** the current task completes, schedule a child agent immediately so it
gathers it in parallel and it is ready on demand. Children run in the background while
the parent finishes its own work; the hand-off to the user happens only after every child
has completed, with the reports fed back for a final synthesis.

---

## D-022 — Parallel read waves with adaptive evidence-gated verification
**Date:** 2026-07-05 · **Status:** accepted

Already-authorized consecutive read-only tool calls execute as a bounded parallel wave.
Permission checks and consumption remain per-call and happen before launch; mutations,
shell, downloads, user interaction, workflow state changes, and unknown calls remain
barriers. Results are buffered by source index and committed in declaration order, so
native tool-call pairing and transcript determinism do not depend on completion timing.

Important delegated checks may opt into adaptive verification. Two context-isolated
replicas run concurrently and return bounded typed findings with evidence. AiTUI accepts
only matching evidence-backed yes/no findings; failures, malformed output, stale local
file evidence, mixed/unknown answers, or disagreement remain unresolved. A third isolated
replica and then an independent verifier are added only when needed. The parent receives a
compact deterministic consensus summary rather than sibling reasoning logs.

This improves the common critical path without making every task pay for a verifier hop,
and improves reliability without letting the producing agent grade itself.

---

## D-021 — Child agents use renewable progress leases with graceful budget finalization
**Date:** 2026-07-04 · **Status:** accepted; refines D-020

Child agents are no longer terminated solely by an abrupt fixed tool-round error. Each
streamed token or reasoning delta renews a configurable progress lease; individual tools
and nested-agent batches have their own timeout, and every child has an absolute duration
ceiling. These time bounds stop silent streams, hung commands, and stalled nested barriers
that a round counter cannot detect.

Round limits remain as deterministic cost guards. At the soft round limit, lease expiry,
operation timeout, or the work portion of the duration limit, AiTUI removes the tool
surface and makes one final synthesis request using evidence already gathered. The hard
round limit is an invariant that reserves this final request rather than discarding all
partial findings. Defaults and overrides live under `[agent]` in `config.toml`:

```toml
[agent]
subagent_soft_rounds = 48
subagent_hard_rounds = 60
subagent_lease_secs = 90
subagent_max_duration_secs = 900
subagent_operation_timeout_secs = 120
```

Nested children inherit the same normalized policy. Existing configs remain compatible
through serde defaults; invalid zero/inverted values are clamped to safe minimums.

---

## D-020 — Checklist subtasks and parallel child agents are separate concepts
**Date:** 2026-07-03 · **Status:** accepted; supersedes D-019 terminology/UI details

`workflow(todo)` owns the parent's ordered checklist, rendered as one-based `SUBTASKS`.
`workflow(agent)` launches parallel child execution contexts; optional one-based
`task_index` metadata associates a child with one parent subtask without conflating the
two state models. The legacy `workflow(task)` spelling remains parser-compatible only.

Child agents appear in a horizontally windowed activity-row tab strip with one-based
labels, persistent highlighting, click selection, `Alt-[` / `Alt-]` cycling, and
left/right switching in the detail dock. Every child gets a transcript entry at launch;
progress updates it in place and completion marks that same entry completed or failed,
while the barrier keeps the turn from being handed off until the whole sibling set has
reported.

Child agents may keep their own local `workflow(todo)` checklist and recursively delegate
bounded read-only `workflow(agent)` batches. Nested delegation is capped by depth and
batch size and never acquires parent mutation or permission authority.

---

## D-019 — Parallel child agents use explicit barriers and read-only capability bounds
**Date:** 2026-07-02 · **Status:** accepted

Consecutive `workflow(action: "task")` calls form one concurrent batch. Each call
starts an isolated child model loop with the parent's current model, reasoning effort,
and reasoning mode. Children run as background tasks: the parent's own tool round
continues immediately (reports land in the conversation as tool messages the parent can
consume mid-round), and the barrier exists solely to defer the hand-off — the turn is not
finished to the user until every child completes. When the parent runs out of work
before the children do, it idles on the barrier; the last child to finish triggers a
final synthesis round fed with every report, and only that round's reply is handed off.
This permits arbitrary dependency-safe formations—parallel then sequential,
sequential then parallel, or repeated alternation—without partial-result races and
without a delegated batch stalling the parent.

Child agents are deliberately bounded to independent investigation and verification:
local read/list/search, read-only web research, and recognized build/test/run or
read-only git shell commands. They cannot mutate files, delegate recursively, ask the
user, or manage the parent's todo state. The parent owns integration, conflicting
evidence, permission-bearing operations, mutations, and final verification, so
parallelism improves latency without weakening correctness.

Runtime state stores live child status, current activity, prompt, cwd, log, duration,
and final report. The sticky task panel renders clickable running/completed/failed
rows; clicking opens a scrollable detail dock. Cancelling the parent releases the
barrier, marks unfinished children failed, and ignores their late events.

---

## D-018 — Tool-call UX: animated preparation, hidden interstitial prose, collapsible writes; API setup prompt
**Date:** 2026-07-01 · **Status:** accepted

Three UX refinements around tool calls (`render/document.rs`) plus a setup prompt:

- **Preparing chip:** while streaming, an unclosed ```tool block (partial JSON) renders
  as an animated `Preparing <tool>…` spinner chip (`render_preparing_tool`,
  `extract_partial_name`) instead of raw JSON — the tool name resolves live.
- **Hide interstitial prose:** in a streaming turn that contains a tool call
  (`is_tool_ish`), the assistant's prose is hidden so only the forming call + reasoning
  show — the "generation around the call" is noise until it runs. Only affects the live
  partial; finalized turns render prose normally.
- **Collapsible writes:** `write_file` previews are collapsed by default to a one-line
  header (a toggle key like tool results); click to expand the full written content.
- **API setup prompt:** a request failing on a missing/relative endpoint
  (`looks_like_base_url_error`) pops an `Overlay::ApiSetup` (URL + key fields); also
  `:setup`. On confirm, saves to config and rebuilds `ApiClient`.

---

## D-017 — Native function-calling via a translation layer (fenced stays internal)
**Date:** 2026-07-01 · **Status:** accepted (implements D-003)

Implemented the OpenAI `tools`/`tool_calls` protocol (ROADMAP Phase 2) **without**
rewriting the internal model. The whole app stores + renders + executes tool calls
as fenced ```` ```tool ```` text (`domain/blocks`, `agent/parser`); rather than
replace that, we translate only at the wire boundary:

- **Response:** `api/client.rs` accumulates streamed `tool_calls` deltas by index and,
  at `finish_reason:"tool_calls"`, synthesizes a well-formed ```tool fence (with the
  model's id) emitted as a normal token. Downstream parse/execute/render and the D-016
  cut are untouched. Since *we* generate the fence, there's no parsing brittleness —
  the reason the fenced-from-the-model approach was fragile.
- **Request:** `Session::api_messages(native)` converts a stored assistant turn's
  fenced calls → `assistant{tool_calls:[…]}` and the following results → `role:"tool"`
  with `tool_call_id` (paired positionally, using the assistant call's own id — so no
  separate id store is needed). Orphaned calls (cancelled round) stay fenced so the API
  never sees an unanswered `tool_calls`.

**Config/fallback:** `api.native_tools` (default on) + `:native`. If a stream errors
with a tools-shaped 4xx (`looks_like_tools_error`), native is auto-disabled and the
user resends on the fenced path — so endpoints without `tools` support still work.

**Why translation, not rewrite:** minimal blast radius (mock mode + every
block/render test unchanged), fully reversible, and it keeps one execution/render path.

---

## D-016 — Cut the stream on the first complete tool call (agent mode)
**Date:** 2026-07-01 · **Status:** accepted

The prompt-fenced protocol tells the model to "emit a tool block and nothing after
it"; the app runs it and feeds the result back next turn. But nothing *enforced*
that stop — generation ran to completion, so a model that expects an inline tool
result would see none mid-stream and spiral into a whole turn of redundant tool
calls + confused reasoning ("the tool runner did not return output"). That wasted
turn is the dominant source of perceived slowness.

Now, in agent mode, `reducer` cuts the stream the instant `should_cut_stream` sees
a complete tool call in the partial: finalize the turn, drop the stream handle
(aborting the backend), and defer `StartAgentRound` to a clean main-loop pass so
leftover tokens don't leak into the next stream. `StreamingParser` in
`agent/parser.rs` stays unused — `extract_tool_calls` on the partial is enough.

**Non-goals:** non-agent streams are never cut (no round to run); models that
legitimately batch multiple calls per turn get cut at the first — acceptable for
this single-tool-per-turn protocol.

---

## D-015 — Event-driven redraw (draw on change, not on a fixed clock)
**Date:** 2026-07-01 · **Status:** accepted (supersedes the ~16 ms busy-poll)

The main loop previously redrew unconditionally every iteration and polled input
for 16 ms, so it ran ~250 layout+render passes/sec even fully idle. Now `main.rs`
tracks a `dirty` flag and only draws when something changed (an input event or
channel activity) or when `animating` (a stream/tool in flight, so the spinner
ticks). `event::poll` blocks 33 ms while animating, 250 ms when idle. Idle CPU
drops to near zero; streaming still animates smoothly.

---

## D-014 — Speculative pre-execution of read-only tools during streaming
**Date:** 2026-07-01 · **Status:** accepted

While an agent reply streams, `effects::speculate_read_tools` (run per
`StreamToken`) pre-runs any *complete, side-effect-free read-only* tool call
(`read_file`/`list_dir`/`search_files`) in the background, caching the result by
`hash(name,args)` in `App.spec_results`. When the tool round reaches that call,
`execute_tool` uses the cached result instantly instead of re-running it.

**Why:** the result is ready the instant the turn ends, so the agent's next turn
starts without waiting on I/O. **Safety:** only tools with no side effects are
speculated — never writes/edits/deletes/shell (those still prompt) and never
network tools (`web_*`, to avoid unwanted requests). An unused speculative result
is simply never matched and dropped; state is cleared each new turn in
`begin_stream_for`, which also bumps a `spec_epoch` tagged onto each spawned task
so a result landing after the turn moved on is discarded rather than served stale
(guards against a file changing between rounds). Reuses
`agent::parser::extract_tool_calls` (the previously "unused" streaming-tools parser
now earns its keep).

---

## D-013 — Per-message render cache (incremental chat-doc rebuild)
**Date:** 2026-07-01 · **Status:** accepted (refines D-001 / D-011 caching)

Every streamed token bumps `content_rev`, invalidating the whole chat-doc cache,
which re-parsed markdown, re-ran tree-sitter highlighting, and re-wrapped **every
message in the session** on each draw — cost scaling with total conversation size ×
tokens, so long sessions streamed visibly slower over time.

Now the rebuild is **per-message incremental**: `App.doc_cache`
(`render::chat::DocCache`) caches each finalized message's `RenderedLine`s under a
content signature (role + text + that message's collapse toggles). A streamed token
only rebuilds the single streaming message; everything else is reused verbatim.
Viewport-width or global show-output changes drop the whole cache (they re-wrap /
re-collapse every message). `render/document.rs::build_message` renders one message;
`build()` is now a thin concat over it (unchanged output, so all render tests hold).

**Why:** streaming cost is now flat regardless of history length. The cache is pure
render-owned scaffolding, so "rendering is pure" (D-001) still stands.

---

## D-012 — Borderless UI, terminal colours only (no custom RGB)
**Date:** 2026-07-01 · **Status:** accepted (supersedes the border look of D-005)

Dropped bordered boxes. Structure comes from padding, a blank gap between turns,
and bold colour-coded role headers using the terminal's own ANSI palette, so the
app follows the terminal theme (light or dark). Transcript turns do not receive a
left gutter or border; user, assistant, system, and tool rows align directly with
the chat column.

Tool output follows the same borderless rule. Code/read/write previews no longer
prefix each row with a rail, while edit diffs retain only their semantic line-number
gutter because it is part of the diff itself.

The input box and status bar are `Clear`/plain with `Padding`, no border, no custom
background — `Clear` resets cells to the terminal's default bg. Selection is
reverse-video (inverts the terminal's fg/bg). Syntax-highlight colours stay
ANSI-named (also terminal-defined).

**Interactive popups** (pickers, browser, settings, permission/decision flows,
setup, and notices) use a **solid, titled dock** in the composer slot. The dock
may expand to the full configured bottom-panel region, reflowing the transcript
above it instead of covering conversation text. Keybinding help remains the sole
large transcript-level modal because its long, non-scrolling reference needs more
vertical space than the composer dock can use comfortably. Both use opaque ANSI
background surfaces and padding rather than border glyphs.

**Surface refinement:** all interactive panels now use an opaque ANSI-8 background
surface with padding instead of border glyphs. This applies to overlays, help,
todos, the last-prompt panel, composer, mention completion, and nested decision
fields. Markdown thematic breaks are background bands rather than box-drawing
rules. The transcript minimap likewise uses solid background cells and pairs its
role map with a high-contrast viewport thumb.

**History:** a first pass used explicit RGB `bg_*` bands; reverted per the rule
"always follow terminal colours, no custom RGB" (breaks light terminals). The
`border`/`border_style` helpers were removed.

**Transcript refinement:** conversation turns no longer paint a shared background.
User prompts, assistant responses, tool rows, timing text, expanded thinking, code,
and thematic rules inherit the terminal background. Edit diffs keep the semantic
red/green line-number gutter transparent, then fill the changed content area from
the right edge of that gutter through the transcript width with ANSI-8. Search-tool
output is another deliberate exception: compact adjacent chips separate the search
summary, path, line number, and matched text. Opaque ANSI surfaces otherwise remain
reserved for focused structure rather than whole conversation turns.

**Later refinements (2026-07-01):** deliberate background surfaces use the fixed
**ANSI-256 palette** (not RGB, so still theme-defined): popup docks, keybinding
help, and the input box use `Color::Indexed(8)` (ANSI bright-black grey) so
interactive controls read as panels without border glyphs. Status-bar statuses
are solid background chips (ANSI bg + black fg).

**Panel background + padding (2026-07-01):** the dark panels were switched from
`Indexed(16)` (pure black — read too dark) to `Indexed(8)`. Panels span the full
terminal width (no app-wide margin); the input panel carries its own **internal
padding** (`Padding::new(2, 2, 1, 1)` in `ui/input.rs`) for breathing room around
the composer — the layout allots `input_height + 2` rows so the vertical padding
consumes the slack.

**Role headers (2026-07-01):** each turn's speaker label gets an **icon + its own
colour** in bold, so `you` / `assistant` / `tool` / `system` read as distinct
speakers without relying on a transcript gutter (`render_role_header`).

---

## D-011 — Tree-sitter highlighting uses a one-shot full parse (not incremental)
**Date:** 2026-07-01 · **Status:** accepted

Code/file previews are syntax-highlighted with `tree-sitter-highlight` in
`render/highlight.rs`. Every snippet we highlight — a fenced code block, a
`read_file` result, a write/edit preview — is a **static snapshot**, and the chat
document is already cached (`render::chat::ChatState`), rebuilt only when the
content revision, width, or a collapse toggle changes.

We deliberately do **not** use incremental parsing (`Parser::parse` fed a prior
`Tree`). Incremental parsing only pays off when re-parsing the *same* buffer after
small edits — an editor's hot path. Here each render highlights a fresh immutable
string once, so there is no old tree to reuse; a full parse is both simpler and
optimal. We cache the compiled per-language `HighlightConfiguration` (query
compilation, not parsing) in a thread-local, so the only repeated cost is the
unavoidable parse of the snippet itself.

**Grammars:** rust, python, js/jsx, ts/tsx, json, bash, go, c, css, html. Markdown
is intentionally excluded (its split block/inline grammar adds complexity and we
render prose ourselves). Unknown languages fall back to plain hard-wrapped text.

---

## D-005 — Claude-Code-like flat UI; no sidebar, no chat vim-motions
**Date:** 2026-06-30 · **Status:** accepted

Removed the sidebar and the `Focus` concept entirely. The UI is now a single
column — transcript, input box, one-line status bar — styled flat (no panel
borders on the transcript, minimal ANSI colour, dim role labels). The input box
keeps full vim modal editing; the transcript only **scrolls** (wheel / PgUp /
PgDn / Ctrl-Home / Ctrl-Shift-D).

To read or search history with real motions, `Ctrl-O` opens the conversation as
a markdown file in `$EDITOR` (nvim by default) — the TUI suspends, runs the
editor, and restores. This replaces in-pane cursor navigation, yank, and
link-open.

Session management (previously the sidebar's job) moves to keybinds: `Ctrl-N/P`
cycle, `Ctrl-S` opens a fuzzy session picker, `:delete` removes one.

**Consequence:** large parts of `render/chat.rs` (cursor/word/line nav, collapse
toggle), the `Focus` enum, `MouseClick`, and the sidebar widget were deleted.
The `Theme` shrank to the handful of colours a flat UI needs. Net build warnings
unchanged (all 7 remaining are pre-existing WIP warnings).

---

## D-004 — Secrets out of the binary; config in `~/.config/aitui/`
**Date:** 2026-06-30 · **Status:** accepted

The committed default config contained a real-looking API key and a LAN endpoint.
Removed the hardcoded values: defaults are now empty, the config template is
written to `~/.config/aitui/config.toml` on first run, and `AITUI_ENDPOINT` /
`AITUI_API_KEY` env vars override the file. Config directory renamed
`aichat-tui` → `aitui`.

**Consequence:** existing users must re-enter endpoint/key in the new path (or set
env vars). No secret ships in the binary or git history going forward.

---

## D-003 — Native function-calling is the target tool mechanism
**Date:** 2026-06-30 · **Status:** accepted — **implemented 2026-07-01 (see [D-017])**

Today the agent depends on the model emitting ```` ```tool ```` fenced JSON, which
`agent/parser.rs` scrapes. This is brittle (formatting drift, partial JSON,
collisions with real code fences). We will migrate to the OpenAI `tools` /
`tool_calls` API, with a fallback to fenced parsing for endpoints that don't
support it.

**Why:** reliability is the bottleneck for "agentic perfection." Structured tool
calls remove a whole class of parsing failures. The schemas already exist in
`tools.rs::tool_schemas()`.

---

## D-002 — Scope: personal power-tool
**Date:** 2026-06-30 · **Status:** accepted

This is built for the author, not a public release. The roadmap therefore weighs
**features, correctness, and UX** over packaging, cross-platform polish, and
distribution. Security still matters where it protects *your* machine (agent
sandboxing, no leaked secrets), but installers/brew/AUR are out of scope.

**Consequence:** we can move fast on UX and assume a known environment (Linux,
`xdg-open`, a reachable OpenAI-compatible endpoint).

---

## D-001 — Elm-style unidirectional architecture
**Date:** baseline (commit `85a173a`) · **Status:** accepted

All state lives in `App`. All mutation goes through `App::apply(Action)`. Side
effects are spawned on `tokio` and report back as `Action`s via channels.
Rendering is a pure function of state with a revision-keyed document cache.

**Why:** it makes behavior testable (the reducer is covered by dozens of unit
tests with no I/O) and easy to trace — there is one place state changes. This is
the backbone of the "very well-written code" goal and should be preserved.
