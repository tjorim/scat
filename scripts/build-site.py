#!/usr/bin/env python3
"""Assemble the buildless GitHub Pages site: python3 scripts/build-site.py."""
from pathlib import Path
import shutil

ROOT = Path(__file__).resolve().parent.parent
OUTPUT = ROOT / "_site"
if OUTPUT.exists():
    shutil.rmtree(OUTPUT)
OUTPUT.mkdir()
for name in ("index.html", "style.css", "catalog.js"):
    shutil.copy2(ROOT / "site" / name, OUTPUT / name)
for name in ("icon.svg", "icon.png", "icon.ico"):
    shutil.copy2(ROOT / "assets" / name, OUTPUT / name)
(OUTPUT / ".nojekyll").touch()
print(f"Assembled {OUTPUT}")
