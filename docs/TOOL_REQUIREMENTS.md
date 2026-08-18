# Tool requirements

This document is the centralized runtime and build dependency reference for AiTUI's agent tools. The tool catalogue and schemas live in `src/agent/tools.rs`; this page records what must be installed or configured for each tool to work.

## Quick installation checklist

A fully equipped AiTUI installation should have:

- AiTUI itself, built with the Rust toolchain described by `Cargo.toml` and `Cargo.lock`.
- A POSIX-compatible `sh` for `shell` calls.
- Either `curl` or `wget` for web search, fetch, image discovery, reverse-image result retrieval, and downloads.
- `ripgrep` (`rg`) for fast regex-aware file search. This is recommended, not mandatory; AiTUI has a bounded literal-search fallback.
- `ffmpeg` and `ffprobe` for reading video pixel frames.
- `node`, `ffmpeg`, and Chrome or Chromium for `specialized(action: video)`.
- Network access and a reachable configured OpenAI-compatible model endpoint for model-driven agent and child-agent operation.

Example checks:

```sh
sh --version 2>/dev/null || sh -c 'echo sh available'
curl --version || wget --version
rg --version
ffmpeg -version
ffprobe -version
node --version
google-chrome-stable --version || google-chrome --version || chromium --version || chromium-browser --version
```

AiTUI reports a tool error when a mandatory runtime program is unavailable. Optional fallbacks are noted below.

## Filesystem tools

The `file_management` category includes `read`, `write`, `edit`, `list`, `search`, `mkdir`, `move`, `copy`, and `delete`.

| Action | Requirements | Notes |
|---|---|---|
| `read` — text | None beyond filesystem access | Implemented with Rust's standard library and AiTUI's in-process cache. |
| `read` — PNG/JPEG/WebP | None beyond the compiled AiTUI binary | Decoded in-process by the Rust `image` crate and returned as bounded segmented RGBA8 data. |
| `read` — GIF | None beyond the compiled AiTUI binary | Animation frames and delays are decoded in-process by the Rust `image` crate. |
| `read` — MP4/WebM/MOV/MKV/AVI/M4V | `ffmpeg` and `ffprobe` | `ffprobe` discovers video dimensions; `ffmpeg` samples one frame per second and emits RGBA8 pixels. Codec support depends on the installed FFmpeg build. |
| `write`, `edit`, `list`, `mkdir`, `move`, `copy`, `delete` | None beyond filesystem permissions | Implemented with Rust's standard library. AiTUI's permission policy still applies. |
| `search` | `rg` recommended | If `rg` is unavailable, AiTUI performs a bounded recursive literal-substring search. The fallback does not provide full regular-expression or glob behavior. |

## Shell tool

| Tool | Requirements | Notes |
|---|---|---|
| `shell` | A POSIX-compatible `sh` | AiTUI executes commands as `sh -c`, closes stdin, streams stdout/stderr, and applies a timeout. On Unix it also uses the `kill` command for best-effort process-group cancellation. Any compiler, test runner, package manager, or application invoked by the command is an additional requirement of that specific command, not of AiTUI itself. |

## Web tools

All web actions accept only HTTP(S) URLs where a URL is required.

| Action | Requirements | Notes |
|---|---|---|
| `web(action: search)` | Network access; `curl` or `wget` | Uses configured/public search providers with fallbacks. Provider availability, rate limits, and bot challenges can affect results. |
| `web(action: images)` | Network access; `curl` or `wget` | Queries the Wikimedia Commons API and returns preview, original, source, creator, and license metadata. |
| `web(action: reverse_image)` — URL | Network access; `curl` or `wget`; Google Lens availability | Retrieves Google Lens result pages. External provider behavior may change. |
| `web(action: reverse_image)` — local file | Network access; Google Lens availability | The image upload itself uses Rust Reqwest with multipart support; following and reading result pages may use the normal `curl`/`wget` web path. |
| `web(action: fetch)` | Network access; `curl` or `wget` | HTML is converted to Markdown in-process with the Rust `html2md` crate. Image URLs found in static `img`/`source` markup are resolved with `reqwest::Url`. JavaScript-rendered content requires a browser-based workflow and is not rendered by this tool. |
| `web(action: download)` | Network access; `curl` or `wget`; destination write permission | Downloads a direct asset URL. AiTUI rejects responses identified as HTML to avoid saving a page accidentally as an image or asset. |

Only one of `curl` and `wget` is required. AiTUI tries `curl` first and falls back to `wget`.

## Specialized tools

### PowerPoint

| Action | Requirements | Notes |
|---|---|---|
| `specialized(action: powerpoint)` | None beyond the compiled AiTUI binary and filesystem access | Creation, append, inspection, editing, validation, OOXML package changes, and atomic saves use the native Rust implementation. Python, `python-pptx`, and `lxml` are not runtime dependencies. LibreOffice or Microsoft PowerPoint is useful for viewing the generated deck but is not required to generate it. See [`POWERPOINT_TOOL.md`](./POWERPOINT_TOOL.md). |

### Video generation

| Action | Requirements | Notes |
|---|---|---|
| `specialized(action: video)` | `node`, `ffmpeg`, and Chrome/Chromium | AiTUI looks for `google-chrome-stable`, `google-chrome`, `chromium`, then `chromium-browser`. Node uses its built-in WebSocket client to drive the Chrome DevTools Protocol; Playwright and Puppeteer are not required. |

Additional video details:

- MP4 output requires an FFmpeg build with the `libx264` encoder.
- WebM output requires an FFmpeg build with the `libvpx-vp9` encoder.
- FFmpeg is also used to create the storyboard image.
- The input is a local HTML/CSS/JavaScript entry file. Its local assets must be readable from that page.
- See [`video-generation.md`](./video-generation.md) for the authoring and rendering contract.

## Interaction and workflow tools

| Category/action | Requirements | Notes |
|---|---|---|
| `interaction(action: ask\|propose\|plan)` | None beyond the active AiTUI interface and applicable filesystem permission for plan files | Interactive requests require an interactive session. In one-shot headless mode they return `input.required`. |
| `workflow(action: agent)` | A reachable configured model endpoint; network access unless the model is local | Named child agents can also require whichever tools their assigned work uses. Their configured allow/deny policy remains an additional boundary. |
| `workflow(action: finish)` | None | Ends the autonomous loop when completion or a blocking condition is reported. |

## Build-time Rust dependencies

Cargo resolves and builds Rust libraries from `Cargo.toml`/`Cargo.lock`. Relevant tool-facing crates include:

- `image` for static images, GIF frames, resizing, and RGBA conversion.
- `html2md` for fetched-page HTML-to-Markdown conversion.
- `reqwest` for URL parsing, HTTP APIs, and multipart reverse-image uploads.
- `serde` and `serde_json` for tool schemas, arguments, and structured output.
- `regex` for extraction and parsing helpers.
- `pptx` and `quick-xml` for native PowerPoint/OOXML support.

These crates are linked into AiTUI and do not need to be installed separately at runtime. Native programs listed in the earlier sections remain external runtime dependencies.

## Capability summary

| Dependency | Mandatory for basic AiTUI? | Enables |
|---|:---:|---|
| Rust-built AiTUI binary | Yes | Core application and all in-process tools |
| `sh` | Only for `shell` | Build/test/run commands |
| `curl` or `wget` | Only for web actions | Search, fetch, image discovery, result retrieval, downloads |
| `rg` | No | Fast regex/glob file search |
| `ffmpeg` + `ffprobe` | Only for video reads | Segmented RGBA video frame reads |
| `node` | Only for video generation | Chrome DevTools capture runner |
| Chrome/Chromium | Only for video generation | Deterministic HTML/CSS/JS frame capture |
| FFmpeg with `libx264` | For MP4 generation | H.264 MP4 output |
| FFmpeg with `libvpx-vp9` | For WebM generation | VP9 WebM output |
| Model endpoint/network | For model-driven operation | Chat, autonomous loops, and child agents |
