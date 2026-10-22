# scat artwork

`icon.svg` is the original source: a terminal layered over catalog cards.
Its dark green, lime, paper and clay palette is shared with the website.
`icon.png` (512 px) and `icon.ico` are generated derivatives.

Regenerate both from the repository root with:

```sh
uv run scripts/build-icon.py
```

The script declares its image-tool dependencies; they are only needed to
regenerate artwork, never to build or run scat or publish the static site.
The original SVG and generated PNG/ICO derivatives are licensed under
[Apache License 2.0](../LICENSE), like the original code. See the
[scope and provenance record](../docs/DISTRIBUTION.md).
