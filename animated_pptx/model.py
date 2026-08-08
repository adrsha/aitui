"""Typed content model for animated PowerPoint generation."""

from __future__ import annotations

from dataclasses import dataclass, field
from pathlib import Path
from typing import Literal

ElementType = Literal["text", "image", "shape"]
ShapeType = Literal["rectangle", "ellipse", "rounded_rectangle"]
AnimationType = Literal[
    "fade_in", "fly_in_left", "fly_in_right", "fly_in_bottom",
    "wipe", "zoom", "fade_out",
]
TriggerType = Literal["on_click", "with_previous", "after_previous"]
TransitionType = Literal["fade", "push_left", "wipe_left"]

DEFAULT_ANIMATION_DURATION_MS = 500
DEFAULT_ANIMATION_DELAY_MS = 0
MIN_DURATION_MS = 1
MAX_DURATION_MS = 60_000
MAX_DELAY_MS = 60_000


def _require_nonempty(value: str, field_name: str) -> None:
    if not isinstance(value, str) or not value.strip():
        raise ValueError(f"{field_name} must be a non-empty string")


def _require_number(value: float, field_name: str, *, positive: bool = False) -> None:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise TypeError(f"{field_name} must be a number")
    if positive and value <= 0:
        raise ValueError(f"{field_name} must be greater than zero")
    if not positive and value < 0:
        raise ValueError(f"{field_name} must not be negative")


@dataclass(frozen=True, slots=True)
class Element:
    """A text box, image, or basic shape positioned in inches."""

    id: str
    type: ElementType
    x: float
    y: float
    width: float
    height: float
    text: str | None = None
    image_path: str | Path | None = None
    shape_type: ShapeType = "rectangle"
    fill_color: str = "4472C4"
    text_color: str = "FFFFFF"
    font_size: float = 24.0

    def __post_init__(self) -> None:
        _require_nonempty(self.id, "Element.id")
        if self.type not in ("text", "image", "shape"):
            raise ValueError(f"unsupported element type: {self.type!r}")
        for name in ("x", "y"):
            _require_number(getattr(self, name), f"Element.{name}")
        for name in ("width", "height", "font_size"):
            _require_number(getattr(self, name), f"Element.{name}", positive=True)
        if self.type == "text" and not isinstance(self.text, str):
            raise TypeError("text elements require Element.text to be a string")
        if self.type == "image":
            if not isinstance(self.image_path, (str, Path)):
                raise TypeError("image elements require Element.image_path")
            if not Path(self.image_path).is_file():
                raise FileNotFoundError(f"image file does not exist: {self.image_path}")
        if self.type == "shape" and self.shape_type not in (
            "rectangle", "ellipse", "rounded_rectangle"
        ):
            raise ValueError(f"unsupported shape type: {self.shape_type!r}")
        for name in ("fill_color", "text_color"):
            value = getattr(self, name)
            if not isinstance(value, str) or len(value) != 6:
                raise ValueError(f"Element.{name} must be a six-digit RGB hex string")
            try:
                int(value, 16)
            except ValueError as error:
                raise ValueError(
                    f"Element.{name} must be a six-digit RGB hex string"
                ) from error


@dataclass(frozen=True, slots=True)
class Animation:
    """A validated animation from the package's fixed supported set."""

    type: AnimationType
    target: str
    order: int
    duration_ms: int = DEFAULT_ANIMATION_DURATION_MS
    delay_ms: int = DEFAULT_ANIMATION_DELAY_MS
    trigger: TriggerType = "on_click"

    def __post_init__(self) -> None:
        supported = (
            "fade_in", "fly_in_left", "fly_in_right", "fly_in_bottom",
            "wipe", "zoom", "fade_out",
        )
        if self.type not in supported:
            raise ValueError(f"unsupported animation type: {self.type!r}")
        _require_nonempty(self.target, "Animation.target")
        if isinstance(self.order, bool) or not isinstance(self.order, int):
            raise TypeError("Animation.order must be an integer")
        if self.order < 0:
            raise ValueError("Animation.order must not be negative")
        for name, maximum in (
            ("duration_ms", MAX_DURATION_MS), ("delay_ms", MAX_DELAY_MS)
        ):
            value = getattr(self, name)
            if isinstance(value, bool) or not isinstance(value, int):
                raise TypeError(f"Animation.{name} must be an integer")
            minimum = MIN_DURATION_MS if name == "duration_ms" else 0
            if not minimum <= value <= maximum:
                raise ValueError(
                    f"Animation.{name} must be between {minimum} and {maximum}"
                )
        if self.trigger not in ("on_click", "with_previous", "after_previous"):
            raise ValueError(f"unsupported animation trigger: {self.trigger!r}")


@dataclass(frozen=True, slots=True)
class Slide:
    """One slide's elements, animations, and optional transition."""

    elements: tuple[Element, ...] = field(default_factory=tuple)
    animations: tuple[Animation, ...] = field(default_factory=tuple)
    transition: TransitionType | None = None

    def __post_init__(self) -> None:
        if not isinstance(self.elements, tuple) or not all(
            isinstance(element, Element) for element in self.elements
        ):
            raise TypeError("Slide.elements must be a tuple of Element objects")
        if not isinstance(self.animations, tuple) or not all(
            isinstance(animation, Animation) for animation in self.animations
        ):
            raise TypeError("Slide.animations must be a tuple of Animation objects")
        ids = [element.id for element in self.elements]
        duplicates = sorted({element_id for element_id in ids if ids.count(element_id) > 1})
        if duplicates:
            raise ValueError(f"duplicate element IDs on slide: {', '.join(duplicates)}")
        orders = [animation.order for animation in self.animations]
        if len(orders) != len(set(orders)):
            raise ValueError("animation order values must be unique within a slide")
        if self.transition not in (None, "fade", "push_left", "wipe_left"):
            raise ValueError(f"unsupported slide transition: {self.transition!r}")
