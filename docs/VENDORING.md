# Binary Deployment

> **Goal:** deploy `scat` on Windows and RHEL as compiled Rust binaries with no
> runtime language or package manager setup on target hosts.

---

## Release artifacts

GitHub Actions builds and uploads two artifacts:

| Platform | Artifact | CI target |
|---|---|---|
| RHEL / Linux x86-64 | `scat` | `x86_64-unknown-linux-musl` |
| Windows x86-64 | `scat.exe` | native Windows MSVC runner |

The Linux binary is built for the musl target to avoid host glibc version
coupling on older RHEL systems.

---

## Install

Copy the matching artifact to the deployment directory and make it executable
on RHEL:

```bash
install -m 0755 scat /catalog/scat/scat
```

On Windows, copy `scat.exe` to the approved tools directory.

Set `SCAT_DB` once so users do not need to pass `--db` on every command:

```bash
export SCAT_DB=/catalog/scat/scripts.sqlite
```

```powershell
$env:SCAT_DB = "C:\catalog\scat\scripts.sqlite"
```

---

## Verify

Run these checks before promoting a release artifact:

```bash
scat --help
scat info
scat search patch --limit 5
scat --json search patch --limit 5
```

For Windows, use the same commands with `scat.exe`.

The binary must also open the existing shared `scripts.sqlite` catalog
read-only and return expected results for a known search term.

---

## CI release checklist

- Linux tests pass with `cargo test --locked`.
- Linux and Windows release binaries build successfully.
- Both binaries pass `--help`.
- Both binaries open a compatibility `scripts.sqlite` fixture and run
  `info` and `search`.
- Binary sizes are reported in the workflow logs for release review.

---

## TUI

The compiled Rust binary includes both CLI commands and the interactive TUI.
Users launch the TUI with:

```bash
scat tui
```
