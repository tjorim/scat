# Contributing

## Rust toolchain

The Rust implementation uses Rust 2024 edition and supports Rust
1.95.0 or newer.

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

## Documentation

Start with [README.md](README.md) for the overview and
[docs/QUICKSTART.md](docs/QUICKSTART.md) for everyday workflows. Update the
relevant guide when commands, defaults, configuration or shortcuts change.
Verify examples against `src/cli.rs` and keep the
[TUI coverage matrix](docs/TUI_OPTIONS.md) in sync with new actions.

## Schema and format changes

Changing `SCHEMA_VERSION` (catalog) or `SIDECAR_FORMAT_VERSION` (embeddings
sidecar) is a coordinated upgrade for every deployment. Follow
[docs/COMPATIBILITY.md](docs/COMPATIBILITY.md#changing-a-schema-or-format)
and the compatibility items in [docs/RELEASING.md](docs/RELEASING.md).
