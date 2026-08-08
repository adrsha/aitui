"""Generate structurally validated PowerPoint decks with fixed animations."""

from .animator import AnimationTargetError
from .generator import generate_presentation
from .model import Animation, Element, Slide
from .validator import PresentationValidationError, validate_presentation

__all__ = [
    "Animation", "AnimationTargetError", "Element", "PresentationValidationError",
    "Slide", "generate_presentation", "validate_presentation",
]
