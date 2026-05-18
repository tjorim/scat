# Contributing

## Rust toolchain

The Rust implementation uses Rust 2024 edition and supports Rust
1.88.0 or newer.

CI checks the minimum supported Rust version (MSRV) with `cargo check --locked
--all-targets` and runs formatting, clippy, and tests on the current stable
toolchain.

The MSRV is a compatibility floor, not a scheduled maintenance target. Keep it
unchanged while it does not block useful work. It may be raised when dependency
updates, tooling requirements, or meaningful Rust language/library improvements
require a newer compiler. Any MSRV bump must update `Cargo.toml`, CI, this
policy, and `CHANGELOG.md`.

## Rust formatting

Run `cargo fmt` before committing Rust changes.

CI enforces formatting with `cargo fmt --check`.
