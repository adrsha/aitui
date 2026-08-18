//! Browser-based HTML/CSS/JS animation capture for `specialized(video)`.
//!
//! The renderer deliberately uses the Chrome DevTools Protocol through Node's
//! built-in WebSocket client instead of requiring Playwright/Puppeteer. Frames
//! are sought deterministically, then encoded with ffmpeg.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use serde_json::json;

use super::executor::ToolAbort;
use super::tools::ToolCall;

const VIDEO_TIMEOUT: Duration = Duration::from_secs(300);
const MAX_FRAMES: u64 = 3_600;

pub fn render(call: &ToolCall, cwd: &Path, abort: &ToolAbort) -> Result<String, String> {
    let entry = required_string(call, "entry_file")?;
    let output = required_string(call, "output_path")?;
    let entry = resolve(entry, cwd);
    let output = resolve(output, cwd);
    if !entry.is_file() {
        return Err(format!(
            "video: entry_file does not exist: {}",
            entry.display()
        ));
    }
    let extension = output
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !matches!(extension.as_str(), "mp4" | "webm") {
        return Err("video: output_path must end in .mp4 or .webm".into());
    }

    let width = integer(call, "width", 1920)?;
    let height = integer(call, "height", 1080)?;
    let fps = integer(call, "fps", 30)?;
    let duration_ms = integer(call, "duration_ms", 5_000)?;
    if !(320..=4096).contains(&width) || !(240..=4096).contains(&height) {
        return Err("video: width and height must be between 320x240 and 4096x4096".into());
    }
    if !(1..=60).contains(&fps) {
        return Err("video: fps must be between 1 and 60".into());
    }
    if !(100..=120_000).contains(&duration_ms) {
        return Err("video: duration_ms must be between 100 and 120000".into());
    }
    let frame_count = (duration_ms * fps).div_ceil(1000);
    if frame_count == 0 || frame_count > MAX_FRAMES {
        return Err(format!(
            "video: render would create {frame_count} frames; maximum is {MAX_FRAMES}"
        ));
    }

    require_program("node")?;
    require_program("ffmpeg")?;
    let chrome = find_chrome().ok_or(
        "video: Chrome/Chromium is required (tried google-chrome-stable, google-chrome, chromium, chromium-browser)",
    )?;
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("video: cannot create output directory: {error}"))?;
    }

    let temp = temp_dir()?;
    let frames = temp.join("frames");
    let profile = temp.join("chrome-profile");
    fs::create_dir_all(&frames)
        .map_err(|error| format!("video: cannot create frame directory: {error}"))?;
    fs::create_dir_all(&profile)
        .map_err(|error| format!("video: cannot create Chrome profile: {error}"))?;
    let runner = temp.join("capture.mjs");
    let request_path = temp.join("request.json");
    fs::write(&runner, CAPTURE_RUNNER)
        .map_err(|error| format!("video: cannot materialize capture runner: {error}"))?;

    let mut chrome_child = Command::new(chrome)
        .arg("--headless=new")
        .arg("--remote-debugging-port=0")
        .arg(format!("--user-data-dir={}", profile.display()))
        .args([
            "--no-first-run",
            "--no-default-browser-check",
            "--disable-background-networking",
            "--disable-component-update",
            "--disable-sync",
            "--disable-extensions",
            "--hide-scrollbars",
            "--mute-audio",
            "--allow-file-access-from-files",
            "--force-device-scale-factor=1",
            "about:blank",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("video: cannot start Chrome: {error}"))?;

    let result = (|| {
        let websocket = wait_for_devtools(&profile, &mut chrome_child, abort)?;
        let request = json!({
            "websocket": websocket,
            "url": file_url(&entry),
            "frames": frames,
            "width": width,
            "height": height,
            "fps": fps,
            "durationMs": duration_ms,
            "frameCount": frame_count,
        });
        fs::write(
            &request_path,
            serde_json::to_vec(&request)
                .map_err(|error| format!("video: cannot encode capture request: {error}"))?,
        )
        .map_err(|error| format!("video: cannot write capture request: {error}"))?;

        let capture = Command::new("node")
            .arg(&runner)
            .arg(&request_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("video: cannot start capture runner: {error}"))?;
        let capture_output = wait(capture, abort, VIDEO_TIMEOUT, "capture")?;
        if !capture_output.status.success() {
            let detail = String::from_utf8_lossy(&capture_output.stderr);
            return Err(format!("video: browser capture failed: {}", detail.trim()));
        }
        let report: serde_json::Value = serde_json::from_slice(&capture_output.stdout)
            .map_err(|error| format!("video: capture returned invalid diagnostics: {error}"))?;

        let input = frames.join("%06d.png");
        let mut ffmpeg = Command::new("ffmpeg");
        ffmpeg
            .args(["-y", "-loglevel", "error", "-framerate"])
            .arg(fps.to_string())
            .arg("-i")
            .arg(&input);
        if extension == "mp4" {
            ffmpeg.args([
                "-c:v",
                "libx264",
                "-preset",
                "medium",
                "-crf",
                "18",
                "-pix_fmt",
                "yuv420p",
                "-movflags",
                "+faststart",
            ]);
        } else {
            ffmpeg.args([
                "-c:v",
                "libvpx-vp9",
                "-crf",
                "28",
                "-b:v",
                "0",
                "-pix_fmt",
                "yuv420p",
            ]);
        }
        let encode = ffmpeg
            .arg(&output)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| format!("video: cannot start ffmpeg: {error}"))?;
        let encode_output = wait(encode, abort, VIDEO_TIMEOUT, "encoding")?;
        if !encode_output.status.success() {
            return Err(format!(
                "video: ffmpeg failed: {}",
                String::from_utf8_lossy(&encode_output.stderr).trim()
            ));
        }

        let storyboard = output.with_file_name(format!(
            "{}-storyboard.jpg",
            output
                .file_stem()
                .and_then(|value| value.to_str())
                .unwrap_or("video")
        ));
        let sample_fps = 6.0 / (duration_ms as f64 / 1000.0);
        let filter = format!("fps={sample_fps:.6},scale=480:-1,tile=3x2");
        let _ = Command::new("ffmpeg")
            .args(["-y", "-loglevel", "error", "-i"])
            .arg(&output)
            .args(["-vf", &filter, "-frames:v", "1"])
            .arg(&storyboard)
            .status();

        let bytes = fs::metadata(&output)
            .map_err(|error| format!("video: output is unavailable: {error}"))?
            .len();
        crate::agent::file_cache::invalidate(&output);
        crate::agent::file_cache::invalidate(&storyboard);
        let warnings = report["warnings"].as_array().map(Vec::len).unwrap_or(0);
        Ok(format!(
            "Video rendered: {} ({}x{}, {} fps, {} ms, {} frames, {} bytes)\nStoryboard: {}\nLayout diagnostics: {} warning{}\n{}",
            output.display(), width, height, fps, duration_ms, frame_count, bytes,
            storyboard.display(), warnings, if warnings == 1 { "" } else { "s" },
            serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".into())
        ))
    })();

    let _ = chrome_child.kill();
    let _ = chrome_child.wait();
    let _ = fs::remove_dir_all(&temp);
    result
}

fn required_string<'a>(call: &'a ToolCall, key: &str) -> Result<&'a str, String> {
    call.args
        .get(key)
        .and_then(|value| value.as_str())
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("video: missing '{key}'"))
}

fn integer(call: &ToolCall, key: &str, default: u64) -> Result<u64, String> {
    match call.args.get(key) {
        None => Ok(default),
        Some(value) => value
            .as_u64()
            .ok_or_else(|| format!("video: '{key}' must be a positive integer")),
    }
}

fn resolve(raw: &str, cwd: &Path) -> PathBuf {
    let path = PathBuf::from(raw);
    if path.is_absolute() {
        path
    } else {
        cwd.join(path)
    }
}

fn require_program(name: &str) -> Result<(), String> {
    Command::new(name)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|_| format!("video: required program '{name}' was not found"))?;
    Ok(())
}

pub(crate) fn find_chrome() -> Option<&'static str> {
    [
        "google-chrome-stable",
        "google-chrome",
        "chromium",
        "chromium-browser",
    ]
    .into_iter()
    .find(|name| {
        Command::new(name)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    })
}

fn temp_dir() -> Result<PathBuf, String> {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let path = std::env::temp_dir().join(format!("aitui-video-{}-{nonce}", std::process::id()));
    fs::create_dir_all(&path)
        .map_err(|error| format!("video: cannot create temporary directory: {error}"))?;
    Ok(path)
}

fn wait_for_devtools(
    profile: &Path,
    child: &mut Child,
    abort: &ToolAbort,
) -> Result<String, String> {
    let marker = profile.join("DevToolsActivePort");
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if abort.load(Ordering::Relaxed) {
            return Err("Cancelled by user".into());
        }
        if let Some(status) = child
            .try_wait()
            .map_err(|error| format!("video: cannot inspect Chrome: {error}"))?
        {
            return Err(format!("video: Chrome exited before capture ({status})"));
        }
        if let Ok(text) = fs::read_to_string(&marker) {
            let mut lines = text.lines();
            if let (Some(port), Some(path)) = (lines.next(), lines.next()) {
                return Ok(format!("ws://127.0.0.1:{port}{path}"));
            }
        }
        if Instant::now() >= deadline {
            return Err("video: timed out waiting for Chrome DevTools".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn wait(
    mut child: Child,
    abort: &ToolAbort,
    timeout: Duration,
    phase: &str,
) -> Result<std::process::Output, String> {
    let deadline = Instant::now() + timeout;
    loop {
        if abort.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Cancelled by user".into());
        }
        if child
            .try_wait()
            .map_err(|error| format!("video: cannot inspect {phase}: {error}"))?
            .is_some()
        {
            return child
                .wait_with_output()
                .map_err(|error| format!("video: cannot collect {phase} output: {error}"));
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!(
                "video: {phase} timed out after {}s",
                timeout.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn file_url(path: &Path) -> String {
    let raw = path.to_string_lossy();
    let encoded = raw
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect::<String>();
    format!("file://{encoded}")
}

const CAPTURE_RUNNER: &str = r#"
import fs from 'node:fs';
const request = JSON.parse(fs.readFileSync(process.argv[2], 'utf8'));
const ws = new WebSocket(request.websocket);
let sequence = 0;
const pending = new Map();
const events = new Map();
ws.onmessage = event => {
  const message = JSON.parse(event.data);
  if (message.id && pending.has(message.id)) {
    const { resolve, reject } = pending.get(message.id); pending.delete(message.id);
    message.error ? reject(new Error(message.error.message)) : resolve(message.result || {});
  } else if (message.method) {
    for (const resolve of events.get(message.method) || []) resolve(message.params || {});
  }
};
await new Promise((resolve, reject) => { ws.onopen = resolve; ws.onerror = () => reject(new Error('DevTools WebSocket failed')); });
const send = (method, params = {}, sessionId) => new Promise((resolve, reject) => {
  const id = ++sequence; pending.set(id, { resolve, reject });
  ws.send(JSON.stringify({ id, method, params, ...(sessionId ? {sessionId} : {}) }));
});
const { targetId } = await send('Target.createTarget', { url: 'about:blank' });
const { sessionId } = await send('Target.attachToTarget', { targetId, flatten: true });
await send('Page.enable', {}, sessionId);
await send('Runtime.enable', {}, sessionId);
await send('Emulation.setDeviceMetricsOverride', { width: request.width, height: request.height, deviceScaleFactor: 1, mobile: false }, sessionId);
const loaded = new Promise(resolve => { const list = events.get('Page.loadEventFired') || []; list.push(resolve); events.set('Page.loadEventFired', list); });
await send('Page.navigate', { url: request.url }, sessionId); await loaded;
await send('Runtime.evaluate', { expression: `Promise.all([document.fonts?.ready, ...Array.from(document.images).map(i => i.complete ? null : new Promise(r => { i.onload=i.onerror=r }))]).then(() => true)`, awaitPromise: true, returnByValue: true }, sessionId);
const warnings = [];
const diagnostics = [];
for (let frame = 0; frame < request.frameCount; frame++) {
  const timeMs = frame * 1000 / request.fps;
  const expression = `(async()=>{ const t=${timeMs}; if (window.__aitui && typeof window.__aitui.seek === 'function') await window.__aitui.seek(t); document.getAnimations().forEach(a=>{a.pause();try{a.currentTime=t}catch(_){}}); window.dispatchEvent(new CustomEvent('aitui:frame',{detail:{timeMs:t,frame:${frame},progress:t/${request.durationMs}}})); await new Promise(r=>requestAnimationFrame(()=>requestAnimationFrame(r))); return true })()`;
  await send('Runtime.evaluate', { expression, awaitPromise: true, returnByValue: true }, sessionId);
  if (frame === 0 || frame === Math.floor((request.frameCount - 1) / 2) || frame === request.frameCount - 1) {
    const probe = await send('Runtime.evaluate', { expression: `(()=>{const vw=innerWidth,vh=innerHeight;const visible=e=>{const r=e.getBoundingClientRect();if(r.width<=0||r.height<=0)return false;for(let n=e;n&&n!==document;n=n.parentElement){const s=getComputedStyle(n);if(s.display==='none'||s.visibility==='hidden'||+s.opacity<=.001)return false}return true};const all=[...document.querySelectorAll('body *')].filter(visible);const overflow=all.filter(e=>{const r=e.getBoundingClientRect();return r.left<-.5||r.top<-.5||r.right>vw+.5||r.bottom>vh+.5}).slice(0,20).map(e=>({tag:e.tagName,id:e.id,role:e.dataset.videoRole||'',rect:[e.getBoundingClientRect().left,e.getBoundingClientRect().top,e.getBoundingClientRect().right,e.getBoundingClientRect().bottom].map(Math.round)}));const clipped=all.filter(e=>e.scrollWidth>e.clientWidth+1||e.scrollHeight>e.clientHeight+1).slice(0,20).map(e=>({tag:e.tagName,id:e.id,role:e.dataset.videoRole||''}));return {timeMs:${timeMs},overflow,clipped,body:[document.body.scrollWidth,document.body.scrollHeight],viewport:[vw,vh]}})()`, returnByValue: true }, sessionId);
    diagnostics.push(probe.result.value);
  }
  const shot = await send('Page.captureScreenshot', { format: 'png', fromSurface: true, captureBeyondViewport: false }, sessionId);
  fs.writeFileSync(`${request.frames}/${String(frame).padStart(6,'0')}.png`, Buffer.from(shot.data, 'base64'));
}
for (const probe of diagnostics) {
  if (probe.body[0] > probe.viewport[0] || probe.body[1] > probe.viewport[1]) warnings.push(`page scroll area exceeds viewport at ${Math.round(probe.timeMs)}ms`);
  if (probe.overflow.length) warnings.push(`${probe.overflow.length} visible elements cross the frame at ${Math.round(probe.timeMs)}ms`);
  if (probe.clipped.length) warnings.push(`${probe.clipped.length} elements have clipped content at ${Math.round(probe.timeMs)}ms`);
}
console.log(JSON.stringify({warnings:[...new Set(warnings)], probes:diagnostics}));
ws.close();
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_urls_escape_spaces_and_hashes() {
        assert_eq!(
            file_url(Path::new("/tmp/a b#c.html")),
            "file:///tmp/a%20b%23c.html"
        );
    }

    #[test]
    fn runner_supports_seek_and_layout_diagnostics() {
        assert!(CAPTURE_RUNNER.contains("window.__aitui.seek"));
        assert!(CAPTURE_RUNNER.contains("Page.captureScreenshot"));
        assert!(CAPTURE_RUNNER.contains("clipped"));
    }

    #[test]
    #[ignore = "set AITUI_VIDEO_ENTRY and AITUI_VIDEO_OUTPUT to render a local scene"]
    fn renders_video_from_environment() {
        let entry = std::env::var("AITUI_VIDEO_ENTRY").expect("AITUI_VIDEO_ENTRY is required");
        let output = std::env::var("AITUI_VIDEO_OUTPUT").expect("AITUI_VIDEO_OUTPUT is required");
        let width = std::env::var("AITUI_VIDEO_WIDTH")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1920);
        let height = std::env::var("AITUI_VIDEO_HEIGHT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1080);
        let duration_ms = std::env::var("AITUI_VIDEO_DURATION_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(5000);
        let fps = std::env::var("AITUI_VIDEO_FPS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(30);
        let call = ToolCall {
            name: "specialized".into(),
            args: json!({"action":"video","entry_file":entry,"output_path":output,"width":width,"height":height,"duration_ms":duration_ms,"fps":fps}),
            id: None,
        };
        let abort = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        println!("{}", render(&call, Path::new("/"), &abort).unwrap());
    }

    #[test]
    #[ignore = "requires Chrome, Node.js, and ffmpeg"]
    fn renders_a_real_html_animation() {
        let dir = temp_dir().unwrap();
        let html = dir.join("scene.html");
        fs::write(
            &html,
            r#"<!doctype html><style>html,body{margin:0;width:100%;height:100%;overflow:hidden;background:#101827}.card{position:absolute;left:80px;top:80px;width:200px;height:100px;border-radius:20px;background:#5eead4;animation:enter 1s both}@keyframes enter{from{opacity:0;transform:translateY(40px)}to{opacity:1;transform:none}}</style><div class=card data-video-role=hero></div>"#,
        )
        .unwrap();
        let call = ToolCall {
            name: "specialized".into(),
            args: json!({
                "action": "video",
                "entry_file": html,
                "output_path": dir.join("scene.mp4"),
                "width": 640,
                "height": 360,
                "duration_ms": 1000,
                "fps": 10
            }),
            id: None,
        };
        let abort = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let report = render(&call, Path::new("/"), &abort).unwrap();
        assert!(
            report.contains("Layout diagnostics: 0 warnings"),
            "{report}"
        );
        assert!(dir.join("scene.mp4").metadata().unwrap().len() > 0);
        assert!(dir.join("scene-storyboard.jpg").metadata().unwrap().len() > 0);
        let _ = fs::remove_dir_all(dir);
    }
}
