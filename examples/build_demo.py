"""Build a small end-to-end animated PowerPoint example."""

from pathlib import Path

from animated_pptx import Animation, Element, Slide, generate_presentation


def main() -> None:
    """Generate a two-slide deck using every supported animation family."""
    slides = (
        Slide(
            elements=(
                Element("title", "text", 0.8, 0.6, 11.7, 0.8, text="Animated PowerPoint"),
                Element(
                    "panel", "shape", 1.0, 2.0, 5.2, 3.5,
                    text="Built with python-pptx\nAnimated with fixed OOXML",
                    shape_type="rounded_rectangle", fill_color="2563EB",
                ),
                Element("note", "text", 7.0, 2.5, 5.0, 1.5, text="Real entrance effects\nand slide transitions"),
            ),
            animations=(
                Animation("fade_in", "title", 0, duration_ms=600),
                Animation("fly_in_left", "panel", 1, duration_ms=700, trigger="after_previous"),
                Animation("wipe", "note", 2, duration_ms=650, trigger="after_previous"),
            ),
            transition="fade",
        ),
        Slide(
            elements=(
                Element("left", "shape", 0.8, 1.2, 3.5, 2.0, text="Fly from right", fill_color="7C3AED"),
                Element("middle", "shape", 4.9, 1.2, 3.5, 2.0, text="Fly from bottom", fill_color="059669"),
                Element("right", "shape", 9.0, 1.2, 3.5, 2.0, text="Zoom", fill_color="DC2626"),
                Element("exit", "text", 3.2, 4.7, 7.0, 0.8, text="This line fades out last"),
            ),
            animations=(
                Animation("fly_in_right", "left", 0),
                Animation("fly_in_bottom", "middle", 1, trigger="with_previous", delay_ms=150),
                Animation("zoom", "right", 2, trigger="after_previous"),
                Animation("fade_out", "exit", 3, trigger="after_previous", delay_ms=500),
            ),
            transition="push_left",
        ),
    )
    output = generate_presentation(slides, Path("examples/output/animated_demo.pptx"))
    print(f"Generated {output}")


if __name__ == "__main__":
    main()
