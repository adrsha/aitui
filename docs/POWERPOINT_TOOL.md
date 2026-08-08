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

- `src/agent/powerpoint.rs` — Rust integration and embedded-package loader.
- `src/agent/powerpoint/animated_pptx/` — Python builder, fixed OOXML animation
  templates, validator, and the private JSON process bridge.

The Python files are compiled into the AiTUI binary with `include_str!`. At tool
execution time AiTUI materializes its bundled package in a process-local temporary
directory and invokes the private bridge itself. It therefore does **not** depend
on the current project containing an `animated_pptx` module, and users/models do
not call `python -m animated_pptx.cli` directly.

The host still needs Python 3 with `python-pptx` and `lxml`; these libraries
provide PowerPoint construction and XML handling. Interpreter discovery checks
`AITUI_POWERPOINT_PYTHON`, the session CWD's `.venv/bin/python`, the directory
where AiTUI was launched from, then `python3` and `python`.

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

Each slide may contain:

- `elements`: objects requiring `id`, `type`, `x`, `y`, `width`, and `height`.
  - `type`: `text`, `image`, or `shape`.
  - Optional fields: `text`, `image_path`, `shape_type`, `fill_color`,
    `text_color`, and `font_size`.
  - Shape types: `rectangle`, `ellipse`, `rounded_rectangle`.
  - Coordinates and dimensions are inches on a 16:9 slide.
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
Stable-selector edits are intentionally reported as unavailable until the next
increment wires these selectors into edit modifiers.

## Advanced OPC/OOXML escape hatch

`package_modifiers` is an ordered, atomic expert interface for controls not yet
represented by the high-level API. Prefer high-level modifiers whenever possible.
It is accepted alongside create/replace/append/edit, and an edit may contain only
`package_modifiers`; package-only edits do not round-trip the deck through
`python-pptx` before patching.

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

Generation and editing are atomic: the result is written to a temporary sibling
and replaces the destination only after validation. Validation reopens the deck
with `python-pptx`, tests ZIP integrity, rejects duplicate ZIP members, parses
every `.xml` and `.rels` member with `lxml`, verifies content-type coverage, and
checks the complete internal relationship graph for malformed, duplicate, or
dangling relationships. Typed generation additionally verifies slide/shape/text
counts plus required timing and transition nodes. The executor invalidates AiTUI's
file cache only after success. The operation is medium-risk because it writes a
file and uses the destination's normal directory-scoped permission flow.

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

- Rust tests verify schema routing, permission/executor integration, bundled
  package materialization, and generation of a real deck through the
  `specialized` call.
- Python tests under `tests/test_animated_pptx.py` cover empty slides, zero-slide
  decks, missing targets, the JSON bridge, mixed-animation decks, append,
  slide/element/animation/transition edit modifiers, native-ID inspection,
  duplicate-name warnings, and byte-for-byte zero-mutation inspection.
