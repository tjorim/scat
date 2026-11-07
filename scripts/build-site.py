#!/usr/bin/env python3
"""Compatibility entry point; install the pinned Node/pnpm toolchain and run pnpm install first."""
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parent.parent

if __name__ == "__main__":
    subprocess.run(["pnpm", "run", "build"], cwd=ROOT, check=True)
