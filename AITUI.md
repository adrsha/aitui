# AITUI Project Instructions

This file is the project-local instruction source for coding agents working in this repository. Prefer `AITUI.md` over tool-specific instruction filenames.

## Project intent

AITUI is a terminal-native AI coding interface written in Rust with Ratatui. Preserve its keyboard-first workflow, compact layout, terminal-defined ANSI palette, and event-driven rendering model.

## Local guidance

- Read `docs/ARCHITECTURE.md`, `docs/DECISIONS.md`, `docs/FEATURES.md`, and `docs/STANDARDS.md` when changing related behavior.
- Keep `render/` focused on transcript/document construction and `ui/` focused on Ratatui widgets.
- Match existing Rust style and keep changes narrowly scoped to the request.
- Run `cargo fmt -- --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test` before finishing.

## UI conventions

- Do not use line-drawn box borders or decorative box outlines to distinguish interface elements. Solid block-character rails are allowed as semantic color accents.
- Use solid opaque background colors, spacing, padding, typography, block rails, and selection states to separate panels and controls.
- Keep colors within the terminal ANSI palette; do not introduce custom RGB colors.
- Preserve readable foreground contrast through `render::theme::fg_guard` or theme helpers.
- Navigation surfaces that support Up/Down must also support Ctrl-K/Ctrl-J.
- The header remains two rows: session tabs first, token/access metadata second. The active cwd belongs beside the model in the bottom status bar and within each session tab.
