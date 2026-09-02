#!/usr/bin/env python3
"""Renders the app icon from assets/AppIcon.svg into every format the
bundles need: AppIcon.ico for the Windows executable (see build.rs),
AppIcon.icns for the macOS bundle, and cadence-mark.png from the
in-app mark.

Rendering goes through resvg so the area outside the rounded tile stays
transparent; the platform thumbnailers paint it white, which the taskbar
then shows as a square around the icon.

    pip install resvg-py pillow
    python scripts/build-icon.py
"""

from io import BytesIO
from pathlib import Path

import resvg_py
from PIL import Image

ASSETS = Path(__file__).resolve().parent.parent / "assets"
ICO_SIZES = [(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)]


def render(svg: Path, size: int) -> Image.Image:
    png = resvg_py.svg_to_bytes(svg_path=str(svg), width=size, height=size)
    return Image.open(BytesIO(bytes(png))).convert("RGBA")


def main() -> None:
    icon = render(ASSETS / "AppIcon.svg", 1024)
    assert icon.getpixel((0, 0))[3] == 0, "icon corners must be transparent"
    icon.save(ASSETS / "AppIcon.ico", sizes=ICO_SIZES)
    icon.save(ASSETS / "AppIcon.icns")
    render(ASSETS / "cadence-mark.svg", 64).save(ASSETS / "cadence-mark.png")


if __name__ == "__main__":
    main()
