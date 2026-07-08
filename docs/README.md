# AiTUI — Project Tracking

A terminal user interface for talking to OpenAI-compatible LLM APIs and driving
**agentic workflows** (the model reads, edits, and runs things in your project
with your approval). Written in Rust on `ratatui` + `tokio`.

This `docs/` folder is the single source of truth for **where the project is**
and **where it's going**. It is documentation-as-tracking: update the checkboxes
in [`ROADMAP.md`](./ROADMAP.md) as work lands.

## The vision

> A TUI built with very well-written code, a performant runtime, and a great
> user experience. It speaks OpenAI-compatible APIs and uses the model for
> agentic coding workflows.

Three non-negotiables, in priority order:

1. **Code quality** — clean architecture, small honest functions, real tests.
2. **Performance** — no jank, no blocking the UI thread, bounded memory.
3. **UX** — fast, legible, keyboard-first, discoverable.

Scope decisions (see [`DECISIONS.md`](./DECISIONS.md)):
- **Personal power-tool**, not a public release — features and UX over packaging.
- **Native function-calling** is the target tool-invocation mechanism.
- **No secrets in the binary** — config lives in `~/.config/aitui/`.

## The documents

| File | What it tracks |
|------|----------------|
| [`ARCHITECTURE.md`](./ARCHITECTURE.md) | Module map, data flow, the core types and the event loop. The "skeleton." |
| [`FEATURES.md`](./FEATURES.md) | Every feature, with a ✅ / 🟡 / ⬜ status and notes. |
| [`ROADMAP.md`](./ROADMAP.md) | Phased plan with checkboxes. **This is the progress tracker.** |
| [`DECISIONS.md`](./DECISIONS.md) | Why we chose what we chose (ADR-lite). |
| [`STANDARDS.md`](./STANDARDS.md) | The bar for "very well-written code." Definition of done. |
| [`CODE_AUDIT.md`](./CODE_AUDIT.md) | Detailed vulnerabilities, bugs, and bad practices, with one-at-a-time fixes. |

## Status at a glance

_Last updated: 2026-07-01_

- **Phase 0 (baseline):** ✅ done — modular Elm-style app, streaming chat,
  sessions, vim input, prompt-fenced agent loop.
- **Phase 1 (foundation wiring):** 🟡 in progress — `render/` (document model) and
  `ui/` (widgets) split is settled; build is down to 2 pre-existing WIP warnings.
- **Phase 2 (native tool-calling):** ✅ done — OpenAI `tools`/`tool_calls` via a
  translation layer (fenced stays the internal form); config `:native` + auto-fallback.
- **Phase 6 (performance):** 🟡 landed a first pass — per-message render cache,
  event-driven redraw, non-blocking save, cached `@`-mention walk, and speculative
  read-only tool pre-execution.

See [`ROADMAP.md`](./ROADMAP.md) for the full plan.
