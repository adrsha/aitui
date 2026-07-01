# Standards — the bar for "very well-written code"

This is the definition of done. A change isn't finished until it meets these.

## Architecture invariants

- **One mutation funnel.** State changes only inside `App::apply`. Effects spawn
  async work and report back as `Action`s — they never reach around the reducer.
- **Rendering is pure.** UI code reads state and draws; it does not mutate domain
  state. The only "state" the renderer owns is layout/cache scaffolding.
- **Domain stays I/O-free.** `domain/` and the pure helpers in `app/state.rs`
  have no network/filesystem side effects, so they're trivially testable.
- **Wire types are isolated.** API JSON shapes live in `api/models.rs`; the rest
  of the app speaks domain types.

## Code quality

- Small, honest functions with a single job. If it needs a comment to explain
  *what* it does, split it; comments should explain *why*.
- Match the surrounding style: naming, comment density, the `// ── section ──`
  banners already used across the codebase.
- No `unwrap()`/`expect()` on fallible runtime paths (I/O, parsing, network).
  `?` with context, or handle and surface via `set_status`. Tests may `unwrap`.
- Prefer `Result` over panics. The TUI must never crash on bad input or a bad
  API response.
- No dead code in committed work. If something is "for later," it belongs in a
  branch or a ROADMAP item — not behind `#[allow(dead_code)]` indefinitely.

## Performance

- Never block the event loop on I/O. Network and tool execution run on tasks.
- Keep the streaming path cheap: rebuild the chat document only when `content_rev`
  or width changes, and rebuild it **per message** (`doc_cache`) so a streamed token
  re-renders only the streaming message, not the whole transcript (D-013).
- Bounded memory: cap tool output, truncate huge files, don't grow buffers
  unboundedly.
- Measure before optimizing (Phase 6). Don't trade clarity for speed without a
  profile that justifies it.

## UX

- Keyboard-first and discoverable: every action reachable without the mouse, and
  findable via `?` help or the command palette.
- Always give feedback: a status-bar message for every meaningful action,
  especially failures.
- Destructive or outward actions (delete, shell, file writes) are gated by an
  explicit, informative confirmation — show *what* will happen.
- Legible at any terminal size; degrade gracefully (collapsible sidebar already
  does this).

## Testing

- Every behavior change ships with a test. The reducer, session, executor, and
  parser are already well-covered — keep that bar.
- Pure logic gets unit tests; integration points (agent loop, SSE parsing) get
  at least one end-to-end test (ROADMAP Phase 2/4).
- `cargo test` green and `cargo build` warning-clean before any change is "done."

## Definition of done (checklist)

- [ ] Builds with zero warnings
- [ ] `cargo test` passes; new behavior is tested
- [ ] No `unwrap`/`expect` on runtime-fallible paths
- [ ] No new dead code / `#[allow(dead_code)]`
- [ ] Status feedback for new user-facing actions
- [ ] `FEATURES.md` + `ROADMAP.md` updated
- [ ] No secrets added to git
