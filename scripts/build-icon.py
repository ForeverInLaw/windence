#!/usr/bin/env python3
"""Renders the app icon from assets/AppIcon.svg into every format the
bundles need: AppIcon.ico for the Windows executable (see build.rs),
AppIcon.icns for the macOS bundle, Store tile PNGs, and cadence-mark.png from the
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
TARGET_SIZES = (16, 20, 24, 30, 32, 36, 40, 48, 60, 64, 72, 80, 96, 256)


def render(svg: Path, size: int) -> Image.Image:
    png = resvg_py.svg_to_bytes(svg_path=str(svg), width=size, height=size)
    return Image.open(BytesIO(bytes(png))).convert("RGBA")


def main() -> None:
    icon = render(ASSETS / "AppIcon.svg", 1024)
    assert icon.getpixel((0, 0))[3] == 0, "icon corners must be transparent"
    icon.save(ASSETS / "AppIcon.ico", sizes=ICO_SIZES)
    icon.save(ASSETS / "AppIcon.icns")
    render(ASSETS / "cadence-mark.svg", 64).save(ASSETS / "cadence-mark.png")
    store_assets = ASSETS.parent / "packaging" / "store" / "Assets"
    store_assets.mkdir(parents=True, exist_ok=True)
    for name, size in (("Square44x44Logo", 44), ("Square150x150Logo", 150), ("StoreLogo", 50)):
        for scale in (100, 200, 400):
            suffix = "" if scale == 100 else f".scale-{scale}"
            render(ASSETS / "AppIcon.svg", size * scale // 100).save(
                store_assets / f"{name}{suffix}.png"
            )
    # Windows selects unplated icons for the taskbar instead of adding a tile.
    for size in TARGET_SIZES:
        target = render(ASSETS / "AppIcon.svg", size)
        for form in ("", "_altform-unplated", "_altform-lightunplated"):
            target.save(store_assets / f"Square44x44Logo.targetsize-{size}{form}.png")
    taskbar_assets = ASSETS / "taskbar"
    taskbar_assets.mkdir(exist_ok=True)
    for name in ("skip-back", "play", "pause", "skip-forward"):
        # A neutral glyph remains visible on both shell backgrounds.
        svg = (ASSETS / "icons" / f"{name}.svg").read_text().replace("currentColor", "#808080")
        png = resvg_py.svg_to_bytes(svg_string=svg, width=256, height=256)
        Image.open(BytesIO(bytes(png))).save(taskbar_assets / f"{name}.png")


if __name__ == "__main__":
    main()
