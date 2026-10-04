"""Renders assets/icon/oynx.svg into the PNG and ICO files the build uses.

Run it after changing the SVG:

    python3 -m pip install cairosvg
    python3 scripts/render-icons.py

Each ICO size is rendered from the SVG directly rather than downscaled from a
large bitmap, so the small taskbar and title-bar sizes stay crisp.
"""

import struct
from pathlib import Path

import cairosvg

ICON_DIR = Path(__file__).resolve().parent.parent / "assets" / "icon"
SVG = ICON_DIR / "oynx.svg"
PNG_SIZES = (32, 256)
ICO_SIZES = (16, 20, 24, 32, 40, 48, 64, 128, 256)


def render(size: int) -> bytes:
    return cairosvg.svg2png(url=str(SVG), output_width=size, output_height=size)


def write_ico(path: Path, sizes) -> None:
    images = [render(size) for size in sizes]
    header = struct.pack("<HHH", 0, 1, len(images))
    offset = len(header) + 16 * len(images)
    entries = b""
    for size, data in zip(sizes, images):
        # A width or height of 256 is stored as 0.
        dim = size % 256
        entries += struct.pack("<BBBBHHII", dim, dim, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
    path.write_bytes(header + entries + b"".join(images))


def main() -> None:
    for size in PNG_SIZES:
        (ICON_DIR / f"oynx-{size}.png").write_bytes(render(size))
    write_ico(ICON_DIR / "oynx.ico", ICO_SIZES)


if __name__ == "__main__":
    main()
