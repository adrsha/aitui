# Roadmap — the progress tracker

This is the working checklist. Tick boxes as work lands; keep the phase status
line in [`README.md`](./README.md) in sync.

Status: ✅ done · 🟡 in progress · ⬜ not started

Each phase has an **exit criterion** — the single thing that means "this phase is
actually finished," not just "mostly."

---

## Phase 0 — Baseline ✅

The architectural foundation. _Done (commits `85a173a`, `47eb13f`)._

- [x] Elm-style `Action → reducer → effect` core
- [x] OpenAI-compatible streaming client + model listing
- [x] Sessions with JSON persistence
- [x] Vim-modal multi-line input, command line, palette
- [x] `@mention`, file & image attachments
- [x] Prompt-fenced agent loop with permissions
- [x] Broad unit-test coverage of reducer/session/executor/parser

**Exit:** ✅ app builds, streams, and runs a basic agent round.

---

## Phase 1 — Foundation wiring (renderer split) 🟡

Finish migrating the monolithic renderer into clean `ui/` widgets and delete the
dead `render/`-side duplicates.

- [ ] Decide the boundary: `render/` = document model (blocks→rows), `ui/` = widgets. Document it in `ARCHITECTURE.md`.
- [ ] Port remaining rendering into `ui/{chat,sidebar,input,statusbar,overlay,help}`
- [x] Remove dead `render::chat::render` / `apply_cursor` / unused helpers
- [x] Clear all unused-import / dead-code warnings (build is now warning-clean)
- [x] Honor `KeybindConfig` in `input::handler` (now a precompiled `input/keymap.rs::Keymap`; all action/mode keys configurable in `[keybinds]`)
- [ ] Wire the `theme` config field (theme selection actually applies)

**Exit:** `cargo build` is warning-clean; there is exactly one code path that
draws each panel.

---

## Phase 2 — Native function-calling 🟡-priority ⬜

The highest-leverage step toward agentic reliability. Replace fenced parsing with
the OpenAI `tools` API.

- [ ] Add `tools` + `tool_choice` to `ChatRequest`; send `tool_schemas()`
- [ ] Parse streamed `tool_calls` deltas in `api/stream.rs` (accumulate by index)
- [ ] Represent assistant tool-call turns and `role: "tool"` results natively (with `tool_call_id`)
- [ ] Stop remapping tool results to `user` in `Session::api_messages()`
- [ ] Capability negotiation/fallback to fenced parsing if the endpoint 400s on `tools`
- [ ] Keep `agent_system_prompt` only as light guidance, not the tool protocol
- [ ] Integration test: model emits a tool call → executes → second turn consumes the result

**Exit:** a real end-to-end agent task (read a file, edit it, run a check)
completes using native tool calls against the configured endpoint.

---

## Phase 3 — Agent safety & trust ⬜

Make the agent something you can let run.

- [ ] Diff preview in the permission prompt for `write_file` / `edit_file`
- [ ] `edit_file`: require a unique match (error on 0 or >1) or take an occurrence index
- [ ] Path sandboxing: confine tool paths to the project root by default; explicit opt-out
- [ ] `run_shell`: timeout, output cap (already 8 KiB), and a clear "this runs arbitrary commands" gate
- [ ] Cancel an in-flight tool / agent round cleanly (`AgentCancel` mid-execution)
- [ ] Surface tool errors as first-class, retryable UI events

**Exit:** every mutating tool shows what it will change before it changes it, and
nothing can touch files outside the project without explicit consent.

---

## Phase 4 — Resilience & correctness ⬜

- [ ] HTTP request timeout + cancellable stream (drop = abort the request)
- [ ] Retry with backoff on transient network errors
- [ ] Graceful, legible error surface for API errors (status + body, not a raw string)
- [ ] Usage/token accounting in the statusbar (parse `usage` when present)
- [ ] SSE parser unit tests (`api/stream.rs`)
- [ ] Agent-loop integration test (Phase 2 dependency)
- [ ] Persist permission memory across runs (optional)

**Exit:** flaky networks and bad responses degrade gracefully; the status bar
tells you what happened and what it cost.

---

## Phase 5 — UX polish ⬜

- [ ] Theme selection actually switches palettes; ship 2–3 good themes
- [ ] Finish vim visual mode (selection ops are currently dead code)
- [ ] Discoverable keybinding hints / which-key style affordances
- [ ] Better streaming cursor + "generating…" affordance
- [ ] Copy-to-system-clipboard (beyond internal yank)
- [ ] Session search / filter in the sidebar

**Exit:** a new user can discover every core action without reading the source.

---

## Phase 6 — Performance & architecture hardening ⬜

Only once features are stable. Measure before optimizing.

- [ ] Profile redraw cost on large transcripts; confirm the `content_rev` cache holds up
- [ ] Consider an event-driven loop (wake on input/channel) instead of 16 ms polling
- [ ] Incremental document rebuild (don't rebuild the whole doc per token)
- [ ] Bound session history / transcript memory; lazy-load old sessions
- [ ] Audit allocations on the hot streaming path

**Exit:** smooth at 10k-line transcripts; idle CPU near zero.

---

## Continuous (every phase)

- [ ] Keep `cargo build` warning-clean
- [ ] Keep `cargo test` green; add a test with every behavior change
- [ ] Update `FEATURES.md` status and these checkboxes as part of each change
- [ ] No secrets in git; config stays in `~/.config/aitui/`
