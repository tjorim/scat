# Script Catalog

The **Script Catalog** is a tool for discovering, searching, and understanding scripts.

It provides:
- Fast full‑text search across scripts and metadata
- A keyboard-driven TUI (Text User Interface) for interactive browsing
- A powerful CLI for scripting and advanced use cases
- Code‑aware linking between scripts (imports, dependencies, entry points)
- Offline support for RHEL environments (no internet required)

The catalog is **indexed nightly** on a central CRON server and consumed **read‑only** by engineers on RHEL systems.

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
            │  RHEL CRON server  │
            │                    │
            │ Nightly index job  │
            │  - scan scripts    │
            │  - parse scripts   │
            │  - run tree-sitter │
            │  - build SQLite    │
            └─────────┬──────────┘
                      │
          scripts.sqlite (read-only,
           on the shared drive)
                      │
    ┌─────────────────┴────────────────┐
    │  one copy per host, per rebuild   │

┌───────────────┐                ┌───────────────┐
│ RHEL host A   │                │ RHEL host B   │
│               │                │               │
│ /dev/shm copy │                │ /dev/shm copy │
│   ↑ scat CLI  │                │   ↑ scat CLI  │
│   ↑ scat TUI  │                │   ↑ scat TUI  │
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
    `$EDITOR`, falling back to `view`/`vim -R`/`vi -R`/`less`. Vim-compatible
    editors are opened read-only so you can search with `/` without
    accidentally editing.

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

## vc working directories

Scripts under change control keep several files per tool on disk: an active
symlink (`prepare_release`), the version copies vc retains beside it for
rollback (`prepare_release_20260729_140513`), and the DEVELOP/ARCHIVE
containers holding checkouts and older versions.

Only the active script is indexed into the catalog — one entry per tool, not
one per retained version. The rest are recorded as **revisions** of that
script and surface through `scat show`, the TUI's revisions pane, and
`scat catalog stats`, so a search for `prepare_release` returns the tool
rather than every version of it. This holds whether or not the script name
carries an extension. Editor backup files (`prepare_release~`) are skipped.

Container directory names are matched exactly as configured under `vc.develop_dirs`
and `vc.archive_dirs`, so a tree whose containers are spelled differently from the
`DEVELOP`/`ARCHIVE` defaults must list its own spelling there.

Where the catalog shows a symlinked script, it also shows what the link
resolved to at index time — a `↳ <target>` sub-row in the CLI's search table,
a `→ <target>` suffix in the TUI's results list and metadata pane. Since the
catalog is built nightly, a script checked in during the day still shows the
version that was live when the index was last built.

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

Separately, `scat tui` accepts an optional **path-mapping file** (`--mapping
<path>` / `SCAT_MAPPING`) that resolves a script's logical catalog path to
its real filesystem path, used by the `V` (view live filesystem source)
key. See
**[docs/scat-mapping.example.yaml](docs/scat-mapping.example.yaml)**.

***

## Database

*   SQLite (single file)
*   FTS5 for full‑text search
*   Built nightly by the indexer
*   **Read‑only for all users**

The database contains **logical paths** (e.g. `/catalog/scripts/...`) that are resolved to real filesystem paths at runtime.

### Local catalog cache

The published `scripts.sqlite` lives on a shared network drive, but no `scat`
process queries it there. On first use after a rebuild, one process copies the
catalog into tmpfs (`/dev/shm` by default) with SQLite's Online Backup API and
every `scat` process on that host — TUI session or one-off CLI query — reads
the local copy instead. Staleness is detected from the published file's
identity and its `index_metadata.build_timestamp`, so the copy costs one read
per host per nightly rebuild rather than one per invocation.

This keeps SQLite's locking off the network filesystem entirely, which matters
because NFS byte-range locks are a documented source of client/server bugs and
WAL mode needs an `mmap`-able `-shm` file that network filesystems generally
don't provide. For the same reason the published catalog is written in
rollback-journal mode: a WAL database needs that `-shm` file even for
read-only connections.

The cache is never load-bearing. If `/dev/shm` is missing, full, or owned by
another user, `scat` logs a warning and reads the shared drive directly.

| Flag | Env | Effect |
|---|---|---|
| `--cache-dir <path>` | `SCAT_CACHE_DIR` | Cache root (default `/dev/shm`) |
| `--no-cache` | `SCAT_NO_CACHE` | Query the shared drive directly |

`scat catalog info` prints the cache path in use when one is active.

***

## Offline & platform support

✅ RHEL / Linux  
✅ No internet access required  
✅ Single compiled binary  
✅ No background services  
✅ No open ports

❌ Windows — support was dropped; clients and the indexer are Linux-only.
The build fails fast on non-Unix targets rather than producing a binary
whose atomic swap and tmpfs cache have no working equivalent.

The release artifact is a compiled Rust binary:

| Platform | Artifact | Rust Target |
|---|---|---|
| RHEL / Linux x86-64 | `scat` | `x86_64-unknown-linux-musl` |

Copy the artifact to a directory on `PATH`, set `SCAT_DB` to the shared
`scripts.sqlite` catalog, and run `scat`.

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

scat's CLI and indexer are implemented in Rust, producing a compiled native
binary for RHEL (x86-64). This removes Python version constraints, vendored
wheels, and the `scat.sh` bootstrap script.

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
- **Deployment**: CI builds a static `x86_64-unknown-linux-musl` binary

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
