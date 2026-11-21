# ONNX Runtime notices

`LICENSE` and `ThirdPartyNotices.txt` are copied unmodified from the
`onnxruntime` 1.31.0 PyPI wheel (`onnxruntime/LICENSE`,
`onnxruntime/ThirdPartyNotices.txt`). The Windows `scat-embed` release build
appends them to `THIRD_PARTY_NOTICES.txt` because `ort-sys` statically links
ONNX Runtime, which `cargo about` cannot see.

They are vendored because the build used to download them from
`raw.githubusercontent.com/microsoft/onnxruntime`, which now returns 404.

When an `ort`/`ort-sys` update changes the ONNX Runtime version (see the
`ms@<version>` URLs in `ort-sys`'s `build/download/dist.tsv`), refresh both
files from the matching wheel and update the version above.
