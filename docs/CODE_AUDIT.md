# Code Audit — Vulnerabilities, Bugs, and Bad Practices

_Last updated: 2026-07-07_

This audit records concrete issues found by reading the codebase and searching for risky patterns. Each section explains the impact, where the issue lives, and how to fix it. Fixes should be taken one at a time so behavior stays easy to verify.

## 1. `edit_file` replaces every matching snippet instead of requiring a unique match

**Status:** Fixed.

**Where:** `src/agent/executor.rs`, in the `ToolKind::Edit` branch.

**Problem:** The tool documentation says `old` must be an exact, unique snippet, but the executor previously checked only `content.contains(old_s)` and then called `content.replace(old_s, new_s)`. `str::replace` replaces every occurrence. If a model gives a common snippet such as `}` or `let x = 1;`, one approved edit can unintentionally alter many locations.

**Impact:** Medium-to-high correctness risk and safety risk for agentic edits. The permission prompt may show a small intended diff, while execution mutates all duplicates across the target file.

**Fix:** `edit_file` now counts `content.matches(old_s)` and rejects both zero matches and multiple matches. It only performs `replacen(old_s, new_s, 1)` when the count is exactly one, preserving the result diff. A regression test verifies duplicate snippets are rejected and the file remains unchanged.

## 2. Tool paths are resolved against `cwd` but are not sandboxed

**Status:** Open.

**Where:** `src/agent/executor.rs::resolve_path`, plus all filesystem tools that call it.

**Problem:** Relative paths are joined to the session working directory, but absolute paths are accepted as-is and relative paths with `..` can escape the project. The feature inventory already notes that path-escape guards are missing.

**Impact:** High security and data-loss risk. If an agent call is approved accidentally or under a broad permission rule, it can read, overwrite, move, or delete files outside the intended project.

**Fix:** Introduce a sandbox root, likely the session cwd by default. Canonicalize existing paths and canonicalize the nearest existing parent for paths that may be created. Reject filesystem operations whose resolved canonical path is outside the root unless a future explicit opt-out setting is enabled. Treat symlink traversal carefully: the post-canonicalized path, not the textual path, must be checked.

## 3. `write_file` overwrites existing files without a permission-time diff gate

**Status:** Open.

**Where:** `src/agent/executor.rs`, `ToolKind::Write`; permission overlay rendering in `src/ui/overlay.rs`.

**Problem:** The executor captures old content and reports a diff after writing, but the permission prompt does not reliably gate the exact old-to-new diff before mutation. The roadmap tracks permission-time diff previews as unfinished.

**Impact:** Medium-to-high safety risk. Users approving a write may not see enough context before a destructive overwrite.

**Fix:** When a pending `write_file` targets an existing file, load the old content during permission preview and render an old→new diff before approval. Keep reads bounded for huge files and show that the preview is truncated if necessary. Avoid executing the write until approval is granted.

## 4. Shell execution remains broad even with timeout and output cap

**Status:** Open.

**Where:** `src/agent/executor.rs::run_shell_command` and `run_shell_with_timeout`.

**Problem:** Shell commands execute through `sh -c` in the project cwd. The code now closes stdin, captures output, and kills long-running commands after a timeout, which is good, but the command still has the full process user's filesystem and network privileges.

**Impact:** High security risk if a malicious or mistaken model command is approved. A command can exfiltrate secrets, mutate unrelated files, or run destructive system operations.

**Fix:** Keep the high-risk permission classification. Add clearer permission UI wording for shell commands, display cwd prominently, and consider optional command allowlists or a container/sandbox mode. Shorten the default timeout for ordinary commands or make it configurable by risk level.

## 5. Web search uses public fallback providers and HTML scraping

**Status:** Open.

**Where:** `src/agent/executor.rs::web_search`, `search_duckduckgo`, `search_bing`, `search_searxng`.

**Problem:** The search implementation scrapes public HTML pages and public SearxNG instances when a configured SearxNG URL is not available. This is fragile and can fail due to bot checks, markup changes, or rate limits.

**Impact:** Medium reliability risk. Agent workflows that depend on search can get inconsistent results or diagnostics instead of useful data.

**Fix:** Prefer an explicitly configured SearxNG instance and make fallback behavior clear in config/docs. Add parser fixtures for each provider and tests covering bot-check/no-result pages. Consider marking public fallback search as best-effort in the UI status when used.

## 6. HTML parsing and entity decoding are ad hoc

**Status:** Open.

**Where:** `src/agent/executor.rs::strip_html`, `strip_tags`, `html_unescape`, provider parsers.

**Problem:** HTML text extraction is implemented with simple string scanning and a small entity replacement table. It will miss many valid HTML constructs and can produce poor text for complex pages.

**Impact:** Medium correctness risk for `web_fetch` and search snippets. The model may receive incomplete or misleading page text.

**Fix:** Use a small HTML parsing/text-extraction crate if dependency policy allows, or isolate the current parser behind tests with real-world fixtures. Expand entity handling only if keeping the custom implementation.

## 7. Input layout previously sized by logical lines instead of visual wrapped rows

**Status:** Fixed in this work.

**Where:** `src/ui/mod.rs` and `src/ui/input.rs`.

**Problem:** The input renderer wraps long logical lines, but the top-level layout allocated height from `app.input.lines.len()`. A long single-line prompt therefore only got one text row, and the renderer windowed to the cursor tail.

**Impact:** UX bug. Users could not review the full prompt in the composer before sending.

**Fix:** Compute composer height from wrapped visual row count using the current input width and configured input-height cap. Keep the cap so huge prompts do not crowd out the transcript.

## 8. Model list reload was not exposed as a user action

**Status:** Fixed in this work.

**Where:** `src/app/effects.rs::refresh_models`, `src/app/reducer.rs::run_command`, `src/app/overlay.rs::slash_commands`.

**Problem:** The app already had model refresh machinery for startup and API setup, but users could not retry model discovery after a transient failure without restarting or changing API settings.

**Impact:** UX/reliability bug. A temporary `/v1/models` failure left the model list stuck on mock until restart/setup.

**Fix:** Add a reducer action/command that calls `refresh_models()` and exposes it in the command palette. Preserve current selection when the refreshed list contains it.

---

## 9. `App` has become a god object that mixes unrelated state domains

**Status:** Open.

**Where:** `src/app/state.rs::App`.

**Problem:** `App` stores configuration, keymaps, sessions, chat rendering cache, vim/input state, overlays, model loading, attachments, clipboard/image side effects, skills, agent queues, stream handles, speculative execution caches, mention cache, layout, and the API client in one public struct. Most fields are public, so unrelated modules can couple directly to internal representation.

**Impact:** High maintainability risk. Any feature change can require understanding the whole application state, tests need large hand-built `App` fixtures, and it is easy to accidentally mutate fields without preserving invariants such as `content_rev`, session draft stashing, or stream/session ownership.

**Fix:** Split state into focused sub-states while keeping the Elm-style reducer: `UiState`, `InputState`, `ModelState`, `AgentState`, `StreamState`, `SessionUiState`, and `RuntimeHandles`. Make fields private where possible and expose small mutation methods for invariants such as "chat content changed" and "active session switched". Migrate one cluster at a time, starting with agent/stream state because it already has many invariants.

## 10. The reducer is too large and handles too many feature domains

**Status:** Open.

**Where:** `src/app/reducer.rs::App::apply`, plus helper methods in the same file.

**Problem:** `App::apply` is a 2,500-line action switch handling modes, input editing, command parsing, stream lifecycle, chat scrolling, external programs, sessions, skills, model loading, attachments, overlays, agent permissions, system prompts, and persistence. `Action` in `src/app/action.rs` is correspondingly broad.

**Impact:** High modularity and scalability risk. Adding a small feature increases the chance of regressions in unrelated areas, merge conflicts concentrate in one file, and it is hard to test a feature without constructing the whole app. The reducer is still a single mutation funnel, but the implementation is no longer locally understandable.

**Fix:** Keep one public `App::apply`, but delegate by domain: `reduce_input`, `reduce_stream`, `reduce_session`, `reduce_overlay`, `reduce_agent`, `reduce_models`, and `reduce_commands`. Use a small `Effect`/follow-up action return type consistently. Move command parsing out of the reducer into a command table that maps aliases to actions.

## 11. Effects module mixes rendering, submission, agent orchestration, and model loading

**Status:** Open.

**Where:** `src/app/effects.rs`.

**Problem:** `effects.rs` contains chat document assembly (`build_chat_doc`, `welcome_doc`), paste handling, message submission, retry/edit/copy helpers, model request construction, image routing, agent tool-loop orchestration, permission/plan/ask handling, speculative execution, model refresh, and tool-result recording.

**Impact:** High maintainability risk. The file name no longer communicates its scope, and feature logic that should evolve independently is coupled through private helper methods on `App`. Rendering document construction also lives outside `render/`, weakening the newly documented `render/`/`ui/` boundary.

**Fix:** Split into focused modules without changing behavior: `app/chat_doc.rs` for session-to-document assembly, `app/submit.rs` for message construction and paste expansion, `app/agent_loop.rs` for permissions/tool queue/speculation, `app/models.rs` for model refresh/selection helpers, and `app/clipboard.rs`/existing modules for copy helpers. Move `welcome_doc` into `render/` or a UI-facing empty-state module.

## 12. Agent executor is a monolithic tool runtime

**Status:** Open.

**Where:** `src/agent/executor.rs`.

**Problem:** One 1,800+ line file handles path resolution, filesystem reads/writes/edits/deletes/move/copy, shell execution and timeout, HTTP fetching, web search provider selection, provider-specific HTML/JSON parsing, HTML stripping, URL encoding/decoding, output formatting, diffing, truncation, and tests.

**Impact:** High safety and scalability risk. The riskiest code in the project is hard to audit because unrelated tool families sit in one match. Adding a new tool increases the chance of breaking shared helpers or bypassing safety checks such as future path sandboxing. Provider-specific search bugs also require touching the same file as filesystem mutation code.

**Fix:** Introduce an `agent/tools/` runtime split: `fs.rs`, `shell.rs`, `web.rs`, `search.rs`, `diff.rs`, `paths.rs`, and `format.rs`, with a small dispatcher in `executor.rs`. Centralize path sandboxing in `paths.rs` so every filesystem tool must pass through it. Move provider parsers and fixtures into search-specific tests.

## 13. Tool definitions and execution are not strongly typed at the boundary

**Status:** Open.

**Where:** `src/agent/tools.rs::ToolCall`, `src/agent/executor.rs::run`, and JSON argument access throughout agent handling.

**Problem:** Tool calls are represented as generic JSON values until execution. Each branch manually extracts required keys (`path`, `content`, `command`, etc.) and supports aliases inline. The schemas, summaries, permission logic, rendering behavior, and executor argument parsing can drift because they are separate stringly-typed implementations.

**Impact:** Medium-to-high correctness risk. A schema change can compile while runtime parsing or permission scoping silently disagrees. It also makes validation and permission previews harder because there is no typed, already-validated command object to inspect.

**Fix:** Parse `ToolCall` into a typed enum before permission or execution, for example `ParsedToolCall::{Read { path, offset, limit }, Edit { path, old, new }, ...}`. Keep raw JSON for transcript/debug display, but make permission checks, previews, and execution consume the typed form. Define aliases during parsing only.

## 14. Command palette and slash/colon commands are duplicated as hand-maintained lists

**Status:** Fixed in this work.

**Where:** `src/app/commands.rs`, `src/app/reducer.rs::run_command`, and `src/app/overlay.rs::slash_commands`.

**Problem:** Command aliases, descriptions, palette rows, and execution mapping were maintained in separate places. Adding a command required updating both the parser and the discovery list, and there was no compiler-enforced link between them.

**Impact:** Medium UX and maintainability risk. Commands could exist but be undiscoverable, or appear in the palette but fail to run. As command count grows, alias collisions and stale descriptions become more likely.

**Fix:** Added a shared command registry with palette metadata, aliases, and exact-command action factories. The slash palette now builds from that registry, and `run_command` dispatches exact alias-only commands through the same registry before handling stateful or argument-taking commands inline.

## 15. Rendering document construction still carries UI policy and tool-specific behavior

**Status:** Open.

**Where:** `src/render/document.rs` and `src/app/effects.rs::build_chat_doc`.

**Problem:** `render/document.rs` does more than convert blocks to rows. It knows role labels/icons, timing badges, tool-specific visibility rules, write/edit previews, diff rendering, shell/read/web result policies, markdown styling, code highlighting, and empty-state presentation (via `effects.rs::welcome_doc`). Some policy belongs to domain/tool presentation rather than the low-level document model.

**Impact:** Medium modularity risk. Adding a tool or changing permission preview behavior can require touching the central renderer. The file is already 1,600+ lines and likely to grow with every transcript presentation feature.

**Fix:** Split row builders by block family: `markdown.rs`, `code.rs`, `diff.rs`, `tool.rs`, `header.rs`, and `empty.rs`. Keep `render/document.rs` as orchestration over `Block` values. Define a small presentation model for tool results so renderer code does not parse summary strings such as `read(path)` to infer behavior.

## 16. Session persistence is synchronous and repeatedly saves full state

**Status:** Open.

**Where:** Calls to `self.sessions.save()` across `src/app/reducer.rs` and `src/app/effects.rs`; session model in `src/domain/session.rs`.

**Problem:** Many state transitions call `sessions.save()` directly after mutations. The roadmap notes non-blocking session save work, but reducer/effect code still triggers persistence ad hoc and persists broad session data as a unit.

**Impact:** Medium scalability risk. Large transcripts or frequent tool events can cause repeated serialization/write pressure. Scattered save calls also make it easy for new mutations to forget persistence or to save too often.

**Fix:** Introduce a persistence scheduler/dirty flag at the app boundary: reducers mark sessions dirty, and the event loop coalesces saves. Longer term, store large transcripts append-only or per-session rather than rewriting all session state for every small change.

## 17. Context-window trimming is approximate and can drop history silently

**Status:** Open.

**Where:** `src/app/effects.rs::begin_stream_for`, use of `context_window * 3` char budget and `api_messages_windowed`.

**Problem:** Request windowing uses a rough character budget instead of tokenizer-aware accounting. Older turns are dropped proactively, and a context-overflow retry compacts history, but users do not get a clear model of exactly what context was sent.

**Impact:** Medium correctness and UX risk. Important instructions or tool results may fall out of context unexpectedly as transcripts grow. Different providers tokenize text differently, so the same character budget can underfit or overflow.

**Fix:** Track approximate tokens per message at append time and surface "sent N of M turns" in status/debug output. Add provider-specific or configurable token estimation later. Prefer explicit summarization/checkpoint messages over silent dropping when possible.

## 18. Agent cancellation does not fully cancel running tool work

**Status:** Open.

**Where:** `src/app/reducer.rs`, `Action::AgentCancel`; `src/app/effects.rs::execute_tool`; `src/agent/executor.rs::run_shell_with_timeout`.

**Problem:** `AgentCancel` clears overlays, queues, and active-tool UI state, but a `spawn_blocking` tool already running cannot be interrupted generically. Shell has its own timeout, but filesystem, network, search, and long copy operations may still finish and send results after the UI has moved on.

**Impact:** Medium safety and correctness risk. A cancelled operation can still mutate files, consume network time, or report stale results into a later state if channel ownership is not carefully cleared.

**Fix:** Add a per-tool cancellation token and execution id. Drop or ignore stale results whose id no longer matches the active round, and use cancellable async clients for network tools. For shell, wire cancellation to process-group kill rather than waiting only for timeout.

## 19. Background work and channels are managed directly in `App`

**Status:** Open.

**Where:** `src/app/state.rs` stream/model/spec/tool channel fields; draining logic in `src/main.rs`.

**Problem:** The main loop knows about every channel type and drains each one with custom logic. `App` stores raw receivers for model loading, streams, agent tools, and speculative execution.

**Impact:** Medium scalability risk. Every new asynchronous feature requires edits to `App`, `main.rs`, and reducer actions. Ordering rules such as cut-stream cleanup and stale speculative results are spread across the event loop and app methods.

**Fix:** Introduce a runtime/event hub that converts background task results into a single internal event channel. Keep feature-specific task handles behind small managers (`StreamManager`, `ToolRunner`, `ModelLoader`) so `main.rs` only polls terminal input and drains one app-event receiver.

## 20. Tests rely on large hand-built app fixtures and module-private behavior

**Status:** Open.

**Where:** Test modules in `src/app/reducer.rs`, `src/app/effects.rs`, `src/agent/executor.rs`, and `src/render/document.rs`.

**Problem:** Reducer tests construct a full `App` with many unrelated fields. Executor tests live in the same monolithic module as production code. This is workable now, but the setup burden grows with every new field and encourages broad integration-style tests for small behavior.

**Impact:** Medium maintainability risk. Refactors become noisy because many tests must be updated for unrelated state changes. It also makes it harder to test isolated reducers or tool parsers without initializing the whole application.

**Fix:** Add test builders for focused sub-states (`AppBuilder`, `SessionBuilder`, `ToolCallBuilder`) and move pure logic into modules with small inputs/outputs. Prefer table-driven tests for command parsing, typed tool parsing, path sandboxing, and document row builders.

## 21. Theme selection is stubbed despite theme infrastructure existing

**Status:** Fixed in this work.

**Where:** `src/app/state.rs::theme`, `src/render/theme.rs`, and config theme field tracked in `docs/ROADMAP.md`.

**Problem:** `App::theme()` previously returned `Theme::default()` unconditionally. Config had a theme field and the renderer used a `Theme` object, but selection did not apply.

**Impact:** Low-to-medium UX and design-completeness risk. It left dead configuration surface area and encouraged future code to hard-code colors because theme selection appeared unsupported.

**Fix:** Added a small theme registry keyed by config name. `App::theme()` now selects from `self.config.ui.theme`; `midnight` preserves the prior default colors, with `default`/`terminal` aliases and a simple `mono` option. Unknown names fall back to `midnight`.

## 22. Root-level placeholder file should be removed or explained

**Status:** Fixed in this work.

**Where:** `src/new.rs`.

**Problem:** `src/new.rs` was present but not referenced by `main.rs` and contained only placeholder content. Unused root-level files create ambiguity about intended modules and can hide accidental scaffolding.

**Impact:** Low maintainability risk, but easy cleanup. It added noise when scanning the project and could confuse contributors about planned architecture.

**Fix:** Deleted the unreferenced placeholder file.

## Suggested remediation order for the next step

1. Remove/explain `src/new.rs` and wire theme selection; both are small, low-risk cleanups.
2. Create the command registry to eliminate command duplication without touching runtime behavior.
3. Split `agent/executor.rs` by tool family, keeping the dispatcher behavior unchanged.
4. Introduce typed parsed tool calls, then use that typed layer for sandboxing and permission previews.
5. Split reducer/effects by domain after the tool runtime boundaries are clearer.
6. Add persistence scheduling and background task managers once the state split creates clean ownership boundaries.

