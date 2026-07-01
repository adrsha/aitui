# Decisions (ADR-lite)

Short records of choices that shape the project. Newest first. When a decision is
reversed, don't delete it — add a new entry that supersedes it.

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
transcript **dimmed** behind (`ui::dim_area`); (b) **fenced code blocks** get a
solid `Color::Indexed(16)` (pure black) background band (`render/document.rs::
CODE_BG`/`paint_bg`) — darker than most terminals, so code reads as a panel with
no coloured border needed. Status-bar statuses are solid **background chips**
(ANSI bg + black fg) rather than dim fg text.

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
**Date:** 2026-06-30 · **Status:** accepted, not yet implemented (ROADMAP Phase 2)

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
