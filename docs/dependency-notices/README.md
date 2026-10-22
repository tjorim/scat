# Supplemental dependency notices

Some crates omit their original license files from published sources. The
version-specific copies here come from the upstream commit named in each crate's
`.cargo_vcs_info.json`. `manifest.json` records exact source URLs, declared terms
and SHA-256 values. These are unmodified originals, not substitute project terms.

- tree-sitter-json 0.24.8: MIT, upstream root LICENSE.
- profiling 1.0.18: MIT OR Apache-2.0, workspace root license texts.
- pulp-wasm-simd-flag 0.1.1: MIT, workspace root LICENSE.
- zune-inflate 0.2.54: selects the offered Zlib alternative, workspace root
  LICENSE-ZLIB. The crate's own source retains its author/copyright information.

`scripts/release-notices.py` checks the version, declared terms and content digest
before including these originals in the corresponding release bundle. It fails
if an unsupplemented dependency has no original notice text. On updates, inspect
the new published source first; do not carry a supplement forward by assumption.

The ort-sys 2.0.0-rc.13 binary distribution table selects ONNX Runtime 1.28.0.
Its Windows DirectML archive (SHA-256
`f7c654b3729cb9e5ad2a36a0c38e5b48e63bf4eed22968931aed33a0ad0b527d`)
contains no original license/notice files. The ONNX Runtime LICENSE and
ThirdPartyNotices.txt here are pinned to upstream v1.28.0's commit. The manifest
also pins ort-sys's distribution table digest; changes require a fresh review
of native binaries and their notices. These supplement the native runtime,
not the Rust wrapper's own MIT/Apache licenses.
