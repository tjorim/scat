# Binary Deployment

> **Goal:** deploy `scat` on RHEL as a compiled Rust binary with no runtime
> language or package manager setup on target hosts.

---

## Release artifacts

GitHub Actions builds and uploads one artifact:

| Platform | Artifact | CI target |
|---|---|---|
| RHEL / Linux x86-64 | `scat` | `x86_64-unknown-linux-musl` |

The binary is built for the musl target to avoid host glibc version coupling
on older RHEL systems. Windows is not a supported target: the build fails
fast on non-Unix hosts rather than producing a binary whose atomic swap and
`/dev/shm` catalog cache have no working equivalent.

---

## Install

Copy the artifact to the deployment directory and make it executable:

```bash
install -m 0755 scat /catalog/scat/scat
```

Set `SCAT_DB` once so users do not need to pass `--db` on every command:

```bash
export SCAT_DB=/catalog/scat/scripts.sqlite
```

Nothing else is needed per host. The first `scat` run after each nightly
rebuild copies the catalog into `/dev/shm` and every later run on that host
queries the copy; hosts without a usable `/dev/shm` fall back to reading the
shared drive directly.

---

## Verify

Run these checks before promoting a release artifact:

```bash
scat --help
scat info
scat search patch --limit 5
scat --json search patch --limit 5
```

The binary must also open the existing shared `scripts.sqlite` catalog
read-only and return expected results for a known search term.

---

## CI release checklist

- Tests pass with `cargo test --locked`.
- The release binary builds successfully.
- The binary passes `--help`.
- The binary opens a compatibility `scripts.sqlite` fixture and runs
  `info` and `search`.
- Binary sizes are reported in the workflow logs for release review.

---

## TUI

The compiled Rust binary includes both CLI commands and the interactive TUI.
Users launch the TUI with:

```bash
scat tui
```
