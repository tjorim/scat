# Script Catalog

The **Script Catalog** is a tool for discovering, searching, and understanding scripts.

It provides:
- Fast full‑text search across scripts and metadata
- A keyboard-driven TUI (Text User Interface) for interactive browsing
- A powerful CLI for scripting and advanced use cases
- Code‑aware linking between scripts (imports, dependencies, entry points)
- Offline support for RHEL environments (no internet required)

The catalog is **indexed nightly** on a central CRON server and consumed **read‑only** by engineers on Windows and RHEL systems.

---

## Why this exists

Over time, the number of Python and shell scripts grows significantly:
- Scripts are hard to discover
- Ownership and purpose are unclear
- Related scripts and shared libraries are not obvious
- Knowledge lives in people’s heads or comments

This tool provides a **single searchable index** without:
- Moving scripts
- Forcing Git usage today
- Running central web services
- Opening firewall ports

---

## High‑level architecture

```

            ┌────────────────────┐
            │  RHEL CRON server   │
            │                    │
            │ Nightly index job  │
            │  - scan scripts  │
            │  - parse scripts  │
            │  - run tree-sitter│
            │  - build SQLite   │
            └─────────┬──────────┘
                      │
          scripts.sqlite (read-only)
                      │
    ┌─────────────────┴────────────────┐
    │                                  │

┌───────────────┐                ┌───────────────┐
│ Windows users │                │  RHEL users   │
│               │                │               │
│ scat CLI      │                │ scat CLI      │
│ scat TUI      │                │ scat TUI      │
└───────────────┘                └───────────────┘

````

---

## Features

### Search
- Full‑text search (SQLite FTS5)
- Ranked results
- Search in:
  - Script contents
  - Headers / docstrings
  - Sidecar metadata

### CLI
- Scriptable and automation‑friendly
- JSON output where applicable
- Suitable for power users and CI jobs
- Transitive dependency trees: `scat deps <path> --tree [--depth N]`
- Shell completions: `scat completions bash|zsh|fish|powershell|elvish`

### TUI
- Interactive search and filtering
  - `lang:`, `owner:`, and `tag:` tokens in the search box filter results the
    same way the CLI's `--lang`/`--owner`/`--tag` flags do
    (e.g. `backup lang:python owner:alice`)
- Results list with script preview
- Metadata, checkout state, and related scripts panes
- Keyboard-first navigation
- Open the full selected script in a read-only external viewer:
  - `v` — view the **indexed catalog content** (`scripts.content`, exactly what
    the preview/search show), written to a temp file with the original filename
  - `V` — view the **live filesystem source** resolved through the configured
    path mapping (or the logical path directly when it is already a real file),
    failing clearly when the source file cannot be found
  - The viewer command is taken from `$SCAT_EDITOR`, then `$VISUAL`, then
    `$EDITOR`, falling back to `view`/`vim -R`/`vi -R`/`less` (or `notepad` on
    Windows). Vim-compatible editors are opened read-only so you can search with
    `/` without accidentally editing.

Example commands:
```bash
scat search "patch freeze"
scat show /catalog/scripts/foo/bar.py
scat deps /catalog/scripts/foo/bar.py
scat deps /catalog/scripts/foo/bar.py --tree --depth 3
scat catalog stats
scat tui
scat completions bash
````

***

## Supported script types

*   Python
*   Shell (bash / ksh / sh)

Dependency extraction uses **Tree‑sitter** for accurate parsing.

### Dependency edge kinds

Two kinds of dependency edges are recorded:

*   **`import`** — language-level edges: Python `import`/`from` statements and
    bare-name shell `source`/`.` directives (`source common.sh`), resolved
    through module/basename mapping. A `source`/`.` of a *path*
    (`source ../lib/common.sh`) is treated as a `referenced` edge instead, so it
    resolves by path.
*   **`referenced`** — a script invoked *by path* rather than imported: a shell
    script that `scp`/`ssh`-runs another script, a Python file executing one via
    `subprocess`/`paramiko`, or a JSON/YAML manifest listing scripts to run.
    These are found by scanning the file body for path literals and are kept
    only when the path resolves to an indexed script — either an **absolute**
    logical path matched exactly, or a **relative** path (`./x.py`,
    `../lib/x.py`) resolved against the referencing script's own directory.
    Unrelated strings (logs, temp files, copy destinations) are discarded.
    Resolution is language-agnostic, so cross-language edges (a shell script
    invoking a Python one, or vice versa) are captured. In `scat deps` they show
    as `ref` (flat table) or `(ref)` (tree).

***

## Metadata

Scripts can optionally include a sidecar metadata file:

    myscript.py
    myscript.py.meta.yml

Example `myscript.py.meta.yml`:

```yaml
owner: owner@example.com
purpose: Verify patch freeze consistency
tags:
  - patching
  - validation
entry_points:
  - main
related:
  - /catalog/scripts/common/logging.py
```

Metadata is indexed together with the script content. When multiple sources
provide the same field, precedence is deterministic:

1. `.meta.yml` sidecar
2. Structured script headers such as `@brief`, `@author`, `@techowner`,
   `@funcowner`, and `@history`
3. Fallback extraction such as a Python module docstring

***

## Configuration

scat can read an optional config file (JSON or YAML, detected from the
extension) for the database path, scan roots, ignore patterns, vc
executable/checkout dirs, and search bookmarks. Point scat at it with
`--config <path>` or `SCAT_CONFIG`; CLI flags and env vars always take
precedence over config-file values.

See **[docs/scat.example.yaml](docs/scat.example.yaml)** for an annotated
example covering every field.

***

## Database

*   SQLite (single file)
*   FTS5 for full‑text search
*   Built nightly by the indexer
*   **Read‑only for all users**

The database contains **logical paths** (e.g. `/catalog/scripts/...`) and is resolved to OS‑specific paths at runtime.

***

## Offline & platform support

✅ Windows  
✅ RHEL  
✅ No internet access required  
✅ Single compiled binary per platform  
✅ No background services  
✅ No open ports

The release artifacts are compiled Rust binaries:

| Platform | Artifact | Rust Target |
|---|---|---|
| RHEL / Linux x86-64 | `scat` | `x86_64-unknown-linux-musl` |
| Windows x86-64 | `scat.exe` | `x86_64-pc-windows-msvc` |

Copy the matching artifact to a directory on `PATH`, set `SCAT_DB` to the
shared `scripts.sqlite` catalog, and run `scat`.

For deployment details, see **[docs/VENDORING.md](docs/VENDORING.md)**.

***

## Tree‑sitter integration

Tree‑sitter parsing is compiled into the Rust binary. No separate grammar
shared library is deployed to user machines.

***

## Indexer (CRON job)

*   Runs nightly on a RHEL CRON server
*   Scans configured paths
*   Updates the SQLite database atomically
*   Old database versions can be kept for rollback

Typical flow:

1.  Scan files
2.  Parse scripts
3.  Extract symbols and dependencies
4.  Index content + metadata
5.  Write new SQLite DB

***

## Project layout (simplified)

    scat/
    ├── src/         # Rust CLI, core search API, and indexer
    ├── tests/       # Rust integration tests
    ├── docs/        # design documents and deployment guides
    └── README.md

***

## Implementation

scat's CLI and indexer are implemented in Rust, producing compiled native
binaries for Windows (x86-64) and RHEL (x86-64). This removes Python version
constraints, vendored wheels, and the `scat.sh` bootstrap script.

### Technical stack

| Concern | Choice |
|---|---|
| CLI | `clap` (derive) |
| SQLite | `rusqlite` with `features = ["bundled-full"]` (bundled SQLite + FTS5) |
| JSON | `serde` + `serde_json` |
| YAML | `yaml_serde` (official YAML org) |
| Tree-sitter | `tree-sitter` + `tree-sitter-python` + `tree-sitter-bash` |
| TUI | `ratatui` with `crossterm` (multi-pane, vim keybindings) |

### Features

- **CLI**: Commands for search, inspection, dependencies, symlinks, diffs, vc pass-through, TUI browsing, and catalog management (`catalog build/stats/info/audit/diff`)
- **Indexer**: Metadata extraction, dependency detection (AST + tree-sitter), atomic operations
- **TUI**: Multi-pane navigation with search, results, metadata, preview, and related scripts
- **Cross-compilation**: CI builds for Linux (musl) and Windows (MSVC)

***

## Non‑goals (by design)

*   No central web UI
*   No live filesystem monitoring
*   No mandatory Git usage
*   No write access from clients
*   No AI or semantic search (for now)

***

## Contributing

Please:

*   Keep changes backward‑compatible
*   Avoid adding runtime dependencies that require internet access
*   Prefer boring, maintainable solutions over clever ones

***

## AI Assistance

This project was developed with significant assistance from AI coding assistants, used for code generation, architecture discussions, and code review. All code has been reviewed and tested by human contributors.

***

## License

See [LICENSE](LICENSE).
