# Release checklist

Releases are tags (`v*`); the `Rust Release Artifacts` workflow builds the
static binary and RPM and runs the smoke checks below on every push and pull
request. Tick these before tagging.

Before distribution outside the organization, complete the
[licensing and distribution checklist](DISTRIBUTION.md). Retain the matching
license and notice files and review new contributions and third-party exclusions.

## Always

- [ ] `cargo fmt --check`, `cargo clippy --all-targets --locked`, `cargo test --locked`
      (the `tests/compat.rs` suite is the executable half of the compatibility policy).
- [ ] `CHANGELOG.md` `[Unreleased]` is complete and dated for the release.
- [ ] The `Rust Release Artifacts` run is green, including the RPM/UBI jobs.

## Compatibility policy ([COMPATIBILITY.md](COMPATIBILITY.md))

Verified by the release workflow's *Smoke test Linux database compatibility*
step against the built static binary — confirm it ran and passed:

- [ ] A catalog of the **current** schema opens: `catalog info` exits 0 and
      reports `compatible`; `search` works; with the cache on and off.
- [ ] A catalog one schema **older** and one **newer** are refused, with
      the directional message (*catalog is older* / *this scat is older*), a non-zero exit,
      and a byte-identical catalog afterwards.
- [ ] `catalog info` on those two still prints both schema versions and a
      `compatibility` verdict.
- [ ] The schema used by the smoke fixture is the one in `src/core/db.rs`
      (the workflow reads it from there; nothing is hardcoded).

Manual, only if `SCHEMA_VERSION` or `SIDECAR_FORMAT_VERSION` changed:

- [ ] `docs/COMPATIBILITY.md` version tables and `docs/QUICKSTART.md` name the
      new version (`tests/compat.rs` fails if the tables are stale).
- [ ] For a catalog schema change, the changelog entry says "coordinated
      upgrade" and names the publisher and all clients. For a sidecar format
      change, it names the sidecar publisher and clients that can no longer
      read the sidecar.
- [ ] The rollout and rollback steps in COMPATIBILITY.md were walked on a
      staging catalog with the new and previous binaries, including a client
      cache that was warm before the flip.
- [ ] Sidecar publication and rollback were walked in staging with the new
      and previous binaries; `similar` refuses unsupported sidecar formats and
      `catalog audit` reports the skipped sidecar while completing the audit.
- [ ] A maintenance window is announced before the catalog is flipped.
