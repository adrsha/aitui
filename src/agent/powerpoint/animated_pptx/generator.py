"""Public entry point for the build, animate, save, and validate pipeline."""

from __future__ import annotations

import os
from pathlib import Path
from tempfile import NamedTemporaryFile
from typing import Sequence

from .animator import apply_animations
from .builder import build_presentation
from .model import Slide
from .validator import validate_presentation


def generate_presentation(slides: Sequence[Slide], output_path: str | Path) -> Path:
    """Generate an animated PPTX atomically and return its final path."""
    if isinstance(slides, (str, bytes)) or not isinstance(slides, Sequence):
        raise TypeError("slides must be a sequence of Slide objects")
    if not all(isinstance(slide, Slide) for slide in slides):
        raise TypeError("slides must contain only Slide objects")
    if not isinstance(output_path, (str, Path)):
        raise TypeError("output_path must be a string or pathlib.Path")
    destination = Path(output_path)
    if destination.suffix.lower() != ".pptx":
        raise ValueError("output_path must have a .pptx extension")
    destination.parent.mkdir(parents=True, exist_ok=True)

    built = build_presentation(slides)
    apply_animations(built.presentation, slides, built.shape_ids)
    temporary_path: Path | None = None
    try:
        with NamedTemporaryFile(
            prefix=f".{destination.stem}-", suffix=".pptx",
            dir=destination.parent, delete=False
        ) as temporary:
            temporary_path = Path(temporary.name)
        built.presentation.save(str(temporary_path))
        validate_presentation(temporary_path, slides)
        os.replace(temporary_path, destination)
        temporary_path = None
    finally:
        if temporary_path is not None:
            temporary_path.unlink(missing_ok=True)
    return destination
