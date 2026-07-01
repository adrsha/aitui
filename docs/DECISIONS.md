# Decisions (ADR-lite)

Short records of choices that shape the project. Newest first. When a decision is
reversed, don't delete it — add a new entry that supersedes it.

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
and a **coloured left gutter bar** per role (`▎`, fg only) — using the terminal's
own ANSI palette so the app **always follows the terminal theme (light or dark)**.
`render/document.rs::mark_gutter` prefixes each turn's rows; `build` reserves the
gutter columns.

Gutters **nest by lineage**: a tool turn is a child of the assistant, so it draws
the assistant's bar *and* its own bar inside it (`▎▎`), rather than replacing it.
User/assistant/system are siblings — one bar each. `role_gutters(role)` returns the
outermost-first colour list (tool → `[assistant, tool]`).

The input box and status bar are `Clear`/plain with `Padding`, no border, no custom
background — `Clear` resets cells to the terminal's default bg. Selection is
reverse-video (inverts the terminal's fg/bg). Syntax-highlight colours stay
ANSI-named (also terminal-defined).

**Modals** (overlays + help) are the exception: they get a **rounded border** and a
**bold title**, and the transcript behind them is **dimmed** (ANSI DIM attribute
added to every cell of the frame in `ui::dim_area`, then the overlay's `Clear`
un-dims its own region). This makes dialogs clearly noticeable. The border uses the
terminal's own fg and DIM is an ANSI attribute — no custom colour, so light/dark
terminals are still honoured. (Refines the original "no borders anywhere" stance:
borders are back for modals only, because a flat borderless dialog was too easy to
miss over a busy transcript.)

**History:** a first pass used explicit RGB `bg_*` bands; reverted per the rule
"always follow terminal colours, no custom RGB" (breaks light terminals). The
`border`/`border_style` helpers were removed.

**Later refinements (2026-07-01):** two deliberate, user-requested exceptions to
the "fg-only" stance, both using the fixed **ANSI-256 palette** (not RGB, so still
theme-defined): (a) **modals** get a rounded border + bold title with the
transcript **dimmed** behind (`ui::dim_area`); (b) **fenced code blocks / the
input box / expanded thinking** get a solid `Color::Indexed(8)` (ANSI bright-black
grey) background band (`render/document.rs::CODE_BG`/`paint_bg`, `ui/input.rs`) —
distinct from the terminal bg so they read as panels with no coloured border.
Status-bar statuses are solid **background chips** (ANSI bg + black fg).

**Panel background + padding (2026-07-01):** the dark panels were switched from
`Indexed(16)` (pure black — read too dark) to `Indexed(8)`. Panels span the full
terminal width (no app-wide margin); the input panel carries its own **internal
padding** (`Padding::new(2, 2, 1, 1)` in `ui/input.rs`) for breathing room around
the composer — the layout allots `input_height + 2` rows so the vertical padding
consumes the slack.

**Role headers (2026-07-01):** each turn's speaker label gets an **icon + its own
colour** (matching the gutter bar) in bold, so `you` / `assistant` / `tool` /
`system` read as distinct speakers instead of muted text (`render_role_header`):
`❯ you` (blue), `✦ assistant` (red), `⚙ tool` (green), `◆ system` (yellow).

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
PgDn / Ctrl-Home / Ctrl-End).

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
