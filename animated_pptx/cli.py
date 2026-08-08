"""JSON stdin bridge used by AiTUI's specialized PowerPoint tool."""

from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Any

from .generator import generate_presentation
from .model import Animation, Element, Slide


def presentation_from_spec(spec: dict[str, Any]) -> tuple[tuple[Slide, ...], Path]:
    """Convert a JSON-compatible tool request into validated model objects."""
    if not isinstance(spec, dict):
        raise TypeError("PowerPoint request must be a JSON object")
    output_path = spec.get("output_path")
    if not isinstance(output_path, str) or not output_path.strip():
        raise ValueError("output_path must be a non-empty string")
    raw_slides = spec.get("slides")
    if not isinstance(raw_slides, list):
        raise TypeError("slides must be an array")

    slides: list[Slide] = []
    for slide_index, raw_slide in enumerate(raw_slides, start=1):
        if not isinstance(raw_slide, dict):
            raise TypeError(f"slide {slide_index} must be an object")
        raw_elements = raw_slide.get("elements", [])
        raw_animations = raw_slide.get("animations", [])
        if not isinstance(raw_elements, list):
            raise TypeError(f"slide {slide_index} elements must be an array")
        if not isinstance(raw_animations, list):
            raise TypeError(f"slide {slide_index} animations must be an array")
        elements = tuple(Element(**element) for element in raw_elements)
        animations = tuple(Animation(**animation) for animation in raw_animations)
        slides.append(
            Slide(
                elements=elements,
                animations=animations,
                transition=raw_slide.get("transition"),
            )
        )
    return tuple(slides), Path(output_path)


def main() -> int:
    """Read one request from stdin, generate the deck, and emit JSON."""
    try:
        spec = json.load(sys.stdin)
        slides, output_path = presentation_from_spec(spec)
        generated = generate_presentation(slides, output_path)
        print(json.dumps({"ok": True, "path": str(generated), "slides": len(slides)}))
        return 0
    except Exception as error:  # CLI boundary: return a concise tool-safe failure.
        print(json.dumps({"ok": False, "error": str(error)}), file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
