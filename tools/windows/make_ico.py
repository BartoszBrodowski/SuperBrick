#!/usr/bin/env python3
"""Regenerate app/assets/SuperBrick.ico from app/assets/icon-1024.png (needs Pillow)."""
from pathlib import Path
from PIL import Image

SIZES = [16, 24, 32, 48, 64, 128, 256]


def main():
    assets = Path(__file__).resolve().parents[2] / "app/assets"
    source = Image.open(assets / "icon-1024.png").convert("RGBA")
    # Pillow resamples the source to each size itself.
    source.save(assets / "SuperBrick.ico", format="ICO", sizes=[(s, s) for s in SIZES])
    print(f"wrote {assets / 'SuperBrick.ico'}")


if __name__ == "__main__":
    main()
