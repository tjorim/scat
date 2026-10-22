#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = ["cairosvg==2.8.2", "pillow==12.2.0"]
# ///
"""Regenerate raster icons from assets/icon.svg: uv run scripts/build-icon.py."""
from pathlib import Path
from io import BytesIO
import cairosvg
from PIL import Image

ROOT = Path(__file__).resolve().parent.parent
svg = ROOT / "assets/icon.svg"
png = cairosvg.svg2png(url=str(svg), output_width=512, output_height=512)
(ROOT / "assets/icon.png").write_bytes(png)
Image.open(BytesIO(png)).save(ROOT / "assets/icon.ico", sizes=[(16, 16), (32, 32), (48, 48), (64, 64), (256, 256)])
