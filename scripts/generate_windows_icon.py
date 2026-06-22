from __future__ import annotations

import argparse
from pathlib import Path
from typing import Iterable

from PIL import Image, ImageEnhance, ImageFilter

SIZES = (16, 24, 32, 48, 64, 128, 256)


def render_size(src: Image.Image, size: int) -> Image.Image:
    # Keep source composition and alpha while improving tiny-size legibility.
    img = src.resize((size, size), Image.Resampling.LANCZOS)
    if size <= 32:
        img = ImageEnhance.Contrast(img).enhance(1.08)
        img = img.filter(ImageFilter.UnsharpMask(radius=0.8, percent=130, threshold=2))
    return img


def save_preview(renders: Iterable[Image.Image], out_path: Path) -> None:
    items = list(renders)
    gap = 8
    width = sum(i.width for i in items) + gap * (len(items) - 1)
    height = max(i.height for i in items)
    preview = Image.new("RGBA", (width, height), (0, 0, 0, 0))
    x = 0
    for img in items:
        y = (height - img.height) // 2
        preview.paste(img, (x, y), img)
        x += img.width + gap
    preview.save(out_path)


def main() -> None:
    parser = argparse.ArgumentParser(description="Generate Windows multi-size .ico from source PNG")
    parser.add_argument("--input", required=True, type=Path)
    parser.add_argument("--png-out", required=True, type=Path)
    parser.add_argument("--ico-out", required=True, type=Path)
    parser.add_argument("--preview-out", required=True, type=Path)
    args = parser.parse_args()

    src = Image.open(args.input).convert("RGBA")
    renders = [render_size(src, s) for s in SIZES]

    args.png_out.parent.mkdir(parents=True, exist_ok=True)
    renders[-1].save(args.png_out)

    args.ico_out.parent.mkdir(parents=True, exist_ok=True)
    renders[-1].save(args.ico_out, format="ICO", sizes=[(s, s) for s in SIZES])

    save_preview([renders[0], renders[1], renders[2]], args.preview_out)


if __name__ == "__main__":
    main()
