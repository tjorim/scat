#!/usr/bin/env python3
"""Compatibility entry point; install the pinned Node/npm toolchain and npm ci first."""
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parent.parent

if __name__ == "__main__":
    subprocess.run(["npm", "run", "build"], cwd=ROOT, check=True)
