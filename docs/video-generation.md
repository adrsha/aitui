# HTML/CSS/JS to Video

AiTUI exposes `specialized(video)` to render a local web scene into MP4 or WebM. It starts headless Chrome, seeks the scene to every exact frame time, captures PNG frames, encodes them with ffmpeg, and emits a six-frame storyboard plus layout diagnostics.

## Animation core

The native animation model lives in `src/video/mod.rs`. Its foundational contracts are intentionally renderer-independent:

- `Composition` owns dimensions, duration, frame rate, properties, and nested clips.
- `Property<T>` is a typed constant, curve, reference, or explicitly dependent computed value.
- `Curve<T>` samples immutable ordered keyframes with hold, linear, or cubic-Bezier timing.
- `Clip` instances reusable compositions over an exclusive parent-time interval.
- `TimeTransform` maps parent time to local time with offset, scale, remapping, clamp, loop, ping-pong, or continuation.
- `DependencyEvaluator` validates and topologically compiles the graph, then produces immutable, deterministic `EvaluationSnapshot`s.
- `Renderer` consumes snapshots; it never samples curves or controls time.

The architectural boundary is: **clips transform time, properties produce values, the evaluator resolves snapshots, and renderers only display them**. State machines, behaviors, simulations, editor state, serialization, and CSS/WAAPI playback optimization remain extensions over this core.

## Recommended agent workflow

1. **Translate the prompt into 3–6 beats.** Write down the opening state, visual development, main reveal/payoff, and resting final frame. Give every beat a time range before coding.
2. **Choose the frame first.** Pick the target resolution/aspect ratio and design directly at that size. Common defaults are 1920×1080 at 30 fps.
3. **Build static keyframes before motion.** Establish hierarchy, spacing, type scale, palette, contrast, and alignment for each major composition. Related beats may use different framing rather than sharing one rigid template.
4. **Motivate the camera and subjects.** Before animating, state why each movement occurs: follow an action, reveal information, redirect attention, show a consequence, or transition to a stronger composition. If a movement has no purpose, keep the element still.
5. **Label important elements in markup.** Add `data-video-role="title"`, `hero`, `metric`, `caption`, `cta`, etc. Diagnostics include these labels when an element crosses or clips against the frame.
6. **Animate by beat, not by element.** A beat should have one dominant idea. Stagger supporting elements by roughly 40–120 ms, overlap adjacent transitions, and avoid hard state changes at beat boundaries.
7. **Separate direction from content.** Camera notes, continuity requirements, animation instructions, and other production language guide the animation but do not become visible labels unless the user asks for them.
8. **Render once, inspect twice.** Inspect the returned storyboard for composition and pacing, then inspect overflow/clipping warnings. Revise and rerender before presenting the result.

## Layout and visual-design rules

- Keep critical text and controls inside a **5% safe area** on every edge.
- Use a spacing scale such as `8, 16, 24, 32, 48, 64, 96` rather than arbitrary gaps.
- Limit a scene to one display face and one highly readable UI/body face when possible.
- Use 1–2 accent colors with neutral surfaces; reserve the strongest contrast for the focal point.
- Prefer 45–75 characters per text line and avoid paragraphs in motion scenes.
- Give UI cards a consistent radius, border treatment, elevation, and internal padding.
- Preserve a clear visual anchor through a transition, but allow text, subjects, and framing to move to new compositions between beats.
- Treat the frame as a virtual **2D camera**. Use motivated pans, pushes, pulls, tracking moves, reveals, and reframing; use restrained parallax only between intentionally separated flat layers.
- Camera motion should begin because something changes, follow or reveal that change, then settle into a readable composition. Avoid wandering, constant floating, repeated zooming, or movement added solely for visual activity.
- Keep the current workflow visually and mathematically 2D. Do not imitate 3D camera orbits, perspective staging, volumetric fly-throughs, or 3D object rotation; those belong to future 3D support.
- Test the first, middle, and last frames as standalone compositions. The renderer probes those times automatically.

## Animation rules

- Prefer `transform` and `opacity`; they compose cleanly and avoid layout jitter.
- Typical UI transitions are **150–350 ms**; larger scene reveals are **400–700 ms**.
- Use restrained easing such as `cubic-bezier(.2,.8,.2,1)`. Reserve elastic/overshoot motion for a deliberate payoff.
- Interpolate every property that changes across a beat boundary; crossfade or move outgoing content while incoming content arrives instead of swapping text or geometry on a single frame.
- Keep one continuous, motivated route per moving subject through a transition. Derive position, scale, glow, camera, and opacity from smooth curves rather than resetting local progress at each phase.
- Favor natural motion: credible anticipation when appropriate, smooth acceleration and deceleration, consistent apparent weight, gentle settling, and brief stillness after important actions. Avoid mechanical constant-speed travel unless the subject calls for it.
- Do not move every layer at once. Let stable elements provide visual reference while the subject or virtual camera performs the minimum movement needed to tell the beat.
- Stagger related items, but keep the full group reveal under about 800 ms.
- Leave a short reading hold after important text appears and a resting hold at the end.
- Avoid perpetual animations unless the prompt explicitly needs ambient motion.
- Motion must communicate hierarchy or causality: where an item came from, what changed, and what deserves attention.

## Deterministic page contract

CSS Animations and the Web Animations API are automatically paused and sought to each frame time. For JavaScript state, canvas, WebGL, charts, counters, particle systems, or custom timelines, expose:

```js
window.__aitui = {
  async seek(timeMs) {
    const progress = Math.max(0, Math.min(1, timeMs / 5000));
    // Set all scene state directly from timeMs/progress.
    // Do not depend on wall-clock time or previous frames.
    draw(progress);
  }
};
```

The renderer also dispatches an `aitui:frame` event after seeking:

```js
window.addEventListener('aitui:frame', ({ detail }) => {
  // detail: { timeMs, frame, progress }
});
```

`seek` should be idempotent: calling frame 90 before frame 10 must produce the same pixels as calling them in chronological order. Keep assets local, wait for fonts/images normally, and avoid network-dependent content.

## Tool call

```json
{
  "action": "video",
  "entry_file": "video/scene.html",
  "output_path": "video/final.mp4",
  "width": 1920,
  "height": 1080,
  "duration_ms": 6000,
  "fps": 30
}
```

Outputs:

- the requested `.mp4` or `.webm`;
- `<name>-storyboard.jpg`, a 3×2 contact sheet sampled across the video;
- JSON diagnostics for visible elements crossing the viewport, clipped content, and page scroll overflow.

Runtime requirements are Node.js, ffmpeg, and Google Chrome or Chromium. No Playwright/Puppeteer installation is required.
