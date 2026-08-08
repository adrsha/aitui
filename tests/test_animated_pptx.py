import json
from pathlib import Path
import subprocess
import sys
from zipfile import ZipFile

from lxml import etree
from pptx import Presentation
import pytest

from animated_pptx import Animation, Element, Slide, generate_presentation
from animated_pptx.animator import AnimationTargetError, NS
from animated_pptx.cli import presentation_from_spec


def slide_xml(path: Path, number: int = 1):
    with ZipFile(path) as archive:
        return etree.fromstring(archive.read(f"ppt/slides/slide{number}.xml"))


def test_empty_slide(tmp_path: Path) -> None:
    output = generate_presentation((Slide(),), tmp_path / "empty-slide.pptx")
    reopened = Presentation(output)
    assert len(reopened.slides) == 1
    assert len(reopened.slides[0].shapes) == 0
    assert slide_xml(output).find("p:timing", NS) is None


def test_zero_slides(tmp_path: Path) -> None:
    output = generate_presentation((), tmp_path / "zero-slides.pptx")
    assert len(Presentation(output).slides) == 0


def test_missing_animation_target_raises_without_replacing_destination(tmp_path: Path) -> None:
    destination = tmp_path / "missing.pptx"
    destination.write_bytes(b"existing")
    slides = (
        Slide(
            elements=(Element("present", "text", 1, 1, 4, 1, text="Present"),),
            animations=(Animation("fade_in", "absent", 0),),
        ),
    )
    with pytest.raises(AnimationTargetError, match="slide 1.*'absent'.*does not exist"):
        generate_presentation(slides, destination)
    assert destination.read_bytes() == b"existing"


def test_full_multi_slide_mixed_animation_deck(tmp_path: Path) -> None:
    slides = (
        Slide(
            elements=(
                Element("a", "text", 0.5, 0.5, 4, 1, text="Alpha"),
                Element("b", "shape", 1, 2, 3, 2, text="Beta"),
                Element("c", "text", 5, 2, 4, 1, text="Gamma"),
            ),
            animations=(
                Animation("fade_in", "a", 0, duration_ms=400),
                Animation("fly_in_left", "b", 1, trigger="after_previous"),
                Animation("wipe", "c", 2, trigger="with_previous", delay_ms=100),
            ),
            transition="fade",
        ),
        Slide(
            elements=(
                Element("d", "shape", 1, 1, 3, 2, text="Delta"),
                Element("e", "shape", 5, 1, 3, 2, text="Epsilon"),
                Element("f", "text", 3, 4, 5, 1, text="Zeta"),
            ),
            animations=(
                Animation("fly_in_right", "d", 0),
                Animation("fly_in_bottom", "e", 1, trigger="after_previous"),
                Animation("zoom", "f", 2, trigger="after_previous"),
                Animation("fade_out", "f", 3, trigger="after_previous", delay_ms=250),
            ),
            transition="wipe_left",
        ),
    )
    output = generate_presentation(slides, tmp_path / "mixed.pptx")
    reopened = Presentation(output)
    assert len(reopened.slides) == 2
    assert [[shape.text for shape in slide.shapes] for slide in reopened.slides] == [
        ["Alpha", "Beta", "Gamma"], ["Delta", "Epsilon", "Zeta"]
    ]
    for number, expected_effects in ((1, 3), (2, 4)):
        root = slide_xml(output, number)
        assert root.find("p:timing", NS) is not None
        assert root.find("p:transition", NS) is not None
        effects = root.findall(".//p:animEffect", NS)
        assert len(effects) == expected_effects
        actual_shape_ids = {shape.shape_id for shape in reopened.slides[number - 1].shapes}
        referenced_ids = {
            int(target.get("spid")) for target in root.findall(".//p:spTgt", NS)
        }
        assert referenced_ids <= actual_shape_ids


def test_json_bridge_builds_typed_slide_model(tmp_path: Path) -> None:
    slides, output = presentation_from_spec({
        "output_path": str(tmp_path / "bridge.pptx"),
        "slides": [{
            "elements": [{
                "id": "title", "type": "text", "x": 1, "y": 1,
                "width": 5, "height": 1, "text": "Bridge",
            }],
            "animations": [{"type": "fade_in", "target": "title", "order": 0}],
            "transition": "fade",
        }],
    })
    assert output == tmp_path / "bridge.pptx"
    assert slides[0].elements[0].text == "Bridge"
    assert slides[0].animations[0].type == "fade_in"


def test_json_cli_generates_powerpoint(tmp_path: Path) -> None:
    output = tmp_path / "tool-generated.pptx"
    request = {
        "output_path": str(output),
        "slides": [{
            "elements": [{
                "id": "title", "type": "text", "x": 1, "y": 1,
                "width": 6, "height": 1, "text": "Specialized tool",
            }],
            "animations": [],
            "transition": None,
        }],
    }
    completed = subprocess.run(
        [sys.executable, "-m", "animated_pptx.cli"],
        input=json.dumps(request),
        text=True,
        capture_output=True,
        check=False,
    )
    assert completed.returncode == 0, completed.stderr
    result = json.loads(completed.stdout)
    assert result == {"ok": True, "path": str(output), "slides": 1}
    assert Presentation(output).slides[0].shapes[0].text == "Specialized tool"
