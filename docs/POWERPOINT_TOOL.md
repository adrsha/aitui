# Native PowerPoint tool

AiTUI exposes animated PowerPoint generation as a **documented model tool**, not
as a shell command:

```json
{
  "name": "specialized",
  "arguments": {
    "action": "powerpoint",
    "output_path": "deck.pptx",
    "slides": []
  }
}
```

The call follows AiTUI's normal tool pipeline: native function schema (or fenced
fallback) → `ToolCall` parsing → `ToolKind::PowerPoint` → permission review →
executor → transcript result. The model does not invoke Python, a CLI, or the
`shell` tool.

## Implementation location

The implementation belongs to AiTUI and lives under:

- `src/agent/powerpoint.rs` — the small Rust module boundary exported to the executor.
- `src/agent/powerpoint/native.rs` — presentation construction, OOXML animation and
  transition templates, package inspection, validation, preservation checks, and
  atomic file replacement.

The runtime is entirely Rust-based. It uses the `pptx` crate for the presentation
and OPC model and `quick-xml` for XML inspection. It does not spawn an interpreter,
materialize a Python package, inspect `PYTHONPATH`, or require `python-pptx`/`lxml`.

## Tool schema

`specialized(action: "powerpoint")` accepts five top-level operations:

- `operation: "create"` (default) — create a new deck from `slides`.
- `operation: "replace"` — explicitly replace the destination with `slides`.
- `operation: "append"` — append `slides` to `input_path`; `input_path`
  defaults to `output_path`, making in-place append a one-field modifier.
- `operation: "edit"` — apply an ordered `modifiers` array to `input_path`;
  `input_path` again defaults to `output_path` for in-place editing.
- `operation: "inspect"` — read an arbitrary existing deck from `input_path`
  without writing it and return native identities, metadata, capabilities, and
  preservation warnings. `output_path` is not used.

Common fields:

- `output_path` — destination ending in `.pptx`, relative to the session CWD or
  absolute.
- `input_path` — existing source deck for append/edit; optional for in-place work.
- `slides` — ordered slide specifications for create/replace/append.

## Default design and motion policy

The generator defaults to a conservative `libreoffice_safe` profile:

- A 0.375-inch compatibility safe area (5% of slide height) is checked on every edge; full-slide background shapes are exempt.
- Elements extending outside the slide are rejected.
- Accidental intersections are rejected. A background/card shape may contain later content, and an intentional overlay must explicitly set `allow_overlap: true`.
- Text boxes are transparent by default, use Liberation Sans, conservative internal padding, explicit wrapping, and a dark default text color.
- A text-fit estimate rejects boxes that are likely to clip or wrap beyond their height. This is deliberately conservative because PowerPoint and LibreOffice measure text differently.
- Slides sharing `continuity_group` are checked so same-ID persistent anchors retain position and scale. This borrows the video tool's principle of preserving one visual anchor while a beat changes.
- `animation_mode` defaults to `single_click`: the first ordered effect waits for one click and the rest of the beat runs automatically. `explicit` is opt-in for pedagogically necessary step-by-step reveals; `none` suppresses timing entirely.
- Motion is authored by beat rather than by element. Related reveals should complete in roughly 800 ms, use restrained fades/wipes, and end in a stable readable frame.

Deck-wide overrides are available under `design`: `safe_margin`, `overlap_policy` (`error`, `warn`, `allow`), and `continuity_policy` (`error`, `warn`, `off`). The default policies are `error` for overlaps and `warn` for continuity drift.

Each slide may contain:

- `elements`: objects requiring `id`, `type`, `x`, `y`, `width`, and `height`.
  - `type`: `text`, `image`, or `shape`.
  - Optional fields: `text`, `image_path`, `shape_type`, `fill_color`,
    `text_color`, `font_size`, and `allow_overlap`.
  - Shape types: `rectangle`, `ellipse`, `rounded_rectangle`.
  - Coordinates and dimensions are inches on a 16:9 slide.
- `continuity_group`: optional label for related slides whose same-ID anchors should remain fixed.
- `animation_mode`: `none`, `single_click` (default), or `explicit`.
- `animations`: objects requiring `type`, `target`, and unique `order`.
  - Types: `fade_in`, `fly_in_left`, `fly_in_right`, `fly_in_bottom`, `wipe`,
    `zoom`, `fade_out`.
  - Optional: `duration_ms`, `delay_ms`, and `trigger` (`on_click`,
    `with_previous`, `after_previous`).
- `transition`: `fade`, `push_left`, `wipe_left`, or `null`.

Animation targets are element IDs on the same slide. Missing targets fail before
the destination is replaced. Empty decks, empty slides, and slides without
animations are valid.

### Edit modifiers

For `operation: "edit"`, modifiers execute in array order and use zero-based
slide indices:

- `append_slides`: `slides`
- `insert_slides`: `index`, `slides`
- `replace_slide`: `slide_index`, `slide`
- `delete_slides`: `indices`
- `move_slide`: `from_index`, `to_index`
- `clear_slide`: `slide_index`
- `add_elements`: `slide_index`, `elements`
- `update_element`: `slide_index`, `element_id`, `changes`
- `replace_element`: `slide_index`, `element_id`, `element`, optional
  `animation_policy`
- `delete_elements`: `slide_index`, `element_ids`, optional `animation_policy`
- `duplicate_elements`: `slide_index`, `element_ids`, `new_ids`, optional
  `offset_x` and `offset_y`
- `reorder_elements`: `slide_index`, `element_ids`, and `position` (`front` or
  `back`) or a non-negative `index`
- `align_elements`: `slide_index`, at least two `element_ids`, and `alignment`
  (`left`, `center`, `right`, `top`, `middle`, or `bottom`)
- `distribute_elements`: `slide_index`, at least three `element_ids`, and
  `direction` (`horizontal` or `vertical`)
- `set_animations`: `slide_index`, `animations` (the complete new list)
- `set_transition`: `slide_index`, `transition`

`update_element.changes` supports `text`, `x`, `y`, `width`, `height`,
`rotation`, `flip_horizontal`, `flip_vertical`, `fill_color`, `text_color`, and
`font_size`. Element replacement/deletion preserves unrelated timing by default.
`animation_policy` may be `remove_targeted` (default), `error_if_referenced`, or
`remove_all`; replacing an element with the same ID retargets its existing timing
to the replacement shape.

## Read-only inspection

Inspection is the safe first step before editing an imported deck:

```json
{
  "action": "powerpoint",
  "operation": "inspect",
  "input_path": "reports/imported.pptx"
}
```

The result reports presentation dimensions and core properties, native slide IDs,
slide indices and package parts, layout names, native shape IDs, names, types,
z-order, geometry, rotation, text summaries, placeholders, group children,
animation targets, transitions, slide relationships, and per-object capabilities.
Selectors have the form `{"slide_id": 256}` and
`{"slide_id": 256, "shape_id": 7}`. Duplicate names and opaque container types
such as charts, diagrams, OLE objects, and media produce structured warnings.

Inspection validates the OPC package and relationship graph, opens the deck only
for reading, never calls `save`, and does not invalidate the source file cache.
Its preservation result explicitly reports that no source rewrite occurred.
Stable native slide and shape selectors are supported by high-level edit modifiers;
legacy zero-based indices and shape names remain available for compatibility.

## Advanced OPC/OOXML escape hatch

`package_modifiers` remains in the public schema as the planned expert escape
hatch, but the Rust migration currently rejects non-empty package modifier arrays
rather than silently falling back to Python. Extend `native.rs` with a guarded OPC
operation before relying on one of these operations in production.

Supported operations:

- `patch_xml`: patch an existing XML part using namespace-aware `xpath` and one
  of `set_attributes`, `remove_attributes`, `set_text`, `append_xml`,
  `prepend_xml`, `replace_xml`, or `remove`. A patch must match exactly one
  element unless `allow_multiple: true` is explicit.
- `put_part`: add or replace a package part using exactly one of `text`, `xml`,
  or `base64`.
- `delete_part`: remove a package part. Related relationships and content-type
  declarations must be updated in the same atomic request.
- `put_relationship` / `delete_relationship`: manage a relationship by
  `source_part` and `id`. Use `source_part: "/"` for package-root relationships.
- `set_content_type` / `delete_content_type`: manage an override by `part` or a
  default by `extension`.

Example adding a custom XML part:

```json
{
  "action": "powerpoint",
  "operation": "edit",
  "output_path": "reports/demo.pptx",
  "package_modifiers": [
    {
      "operation": "put_part",
      "part": "customXml/item1.xml",
      "xml": "<metadata xmlns=\"urn:example\"><value>42</value></metadata>"
    },
    {
      "operation": "set_content_type",
      "part": "customXml/item1.xml",
      "content_type": "application/xml"
    },
    {
      "operation": "put_relationship",
      "source_part": "/",
      "id": "rIdCustomMetadata",
      "relationship_type": "http://schemas.openxmlformats.org/officeDocument/2006/relationships/customXml",
      "target": "customXml/item1.xml"
    }
  ]
}
```

Package part names are ZIP-slip checked, XML is parsed with external entity and
network resolution disabled, relationship IDs must be unique, and internal
relationship targets must resolve to existing members. Raw package operations can
still create semantically invalid Office features; the caller is responsible for
the feature-specific OOXML semantics.

## Validation and safety

Generation and editing are atomic: Rust serializes to memory, reopens the package
with the `pptx` crate, verifies slide and shape counts for typed generation, writes
a temporary sibling, syncs it, and only then replaces the destination. Read-only
inspection never saves or rewrites its source. Preservation-checked open/save also
compares every loaded part payload and relationship before commit. The executor
invalidates AiTUI's file cache only after success. The operation is medium-risk
because it writes a file and uses the destination's normal directory-scoped
permission flow.

## Example tool call

```json
{
  "action": "powerpoint",
  "output_path": "reports/demo.pptx",
  "slides": [
    {
      "elements": [
        {
          "id": "title",
          "type": "text",
          "x": 1,
          "y": 0.8,
          "width": 10,
          "height": 1,
          "text": "Generated by AiTUI",
          "font_size": 32
        }
      ],
      "animations": [
        {
          "type": "fade_in",
          "target": "title",
          "order": 0,
          "duration_ms": 500
        }
      ],
      "transition": "fade"
    }
  ]
}
```

## Append example

```json
{
  "action": "powerpoint",
  "operation": "append",
  "output_path": "reports/demo.pptx",
  "slides": [{"elements": [], "animations": [], "transition": null}]
}
```

## In-place edit example

```json
{
  "action": "powerpoint",
  "operation": "edit",
  "output_path": "reports/demo.pptx",
  "modifiers": [
    {
      "operation": "update_element",
      "slide_index": 0,
      "element_id": "title",
      "changes": {"text": "Revised title", "font_size": 36}
    },
    {
      "operation": "insert_slides",
      "index": 1,
      "slides": [{"elements": [], "animations": [], "transition": "fade"}]
    }
  ]
}
```

## Tests

- Rust tests verify schema routing, permission/executor integration, creation of a
  real deck, animation/transition preservation, read-only inspection, append,
  ordered edit dispatch, and atomic preservation-checked open/save.
- The old Python test package is legacy migration material and is not executed by
  the PowerPoint tool or required at runtime.
