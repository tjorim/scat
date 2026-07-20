# scat Quick‑Start Guide

> **One page** — everything you need to find, browse, and understand scripts in the catalog.

---

## 0 – Prerequisites

| Requirement | Notes |
|---|---|
| `scat` or `scat.exe` | Compiled binary for your platform |
| `SCAT_DB` or `--db` | Path to the shared `scripts.sqlite` catalog |

Set the environment variable once so you never need to type `--db` again:

```bash
# Linux / RHEL
export SCAT_DB=/catalog/scat/scripts.sqlite

# Windows (PowerShell)
$env:SCAT_DB = "C:\catalog\scat\scripts.sqlite"
```

### Optional: config file

Instead of (or in addition to) `SCAT_DB`, you can point scat at a config
file with `--config <path>` or `SCAT_CONFIG` for the database path, scan
roots, ignore patterns, vc settings, and search bookmarks (`scat search
@name`). See **[scat.example.yaml](scat.example.yaml)** for an annotated
example. CLI flags and env vars always override config-file values.

### Optional: path-mapping file (TUI only)

`scat tui --mapping <path>` / `SCAT_MAPPING` points at a separate file that
maps logical catalog paths to real Windows/Linux filesystem paths, used by
the `V` key to open a script's live source. See
**[scat-mapping.example.yaml](scat-mapping.example.yaml)**.

---

## 1 – Search

### Find scripts by keyword

```bash
scat search "patch freeze"
```

Results are ranked by relevance and include path, language, owner, and purpose.

### Filter by language

```bash
scat search --lang python
scat search --lang shell
```

### Combine keyword and language filter

```bash
scat search "deploy" --lang shell
```

### Increase the result limit (default: 50)

```bash
scat search "backup" --limit 200
```

### Machine‑readable output (JSON / CSV)

```bash
scat search "patch freeze" --output json --fields path,owner
scat search "patch freeze" --output csv --fields path,owner,purpose
```

---

## 2 – Browse (TUI)

Launch the full-screen interactive browser:

```bash
scat tui
```

### TUI keyboard shortcuts

| Key | Action |
|---|---|
| `/` | Focus the search box |
| `Enter` | Move from search to results, or open the selected script detail view |
| `Tab` / `Shift+Tab` | Cycle between search, results, preview, and related panes |
| `↑` / `↓` or `j` / `k` | Navigate results or scroll the focused detail pane |
| `PageUp` / `PageDown` | Scroll the focused preview or related pane |
| `Ctrl+u` / `Ctrl+d` | Scroll the focused detail pane by a half page |
| `Ctrl+b` / `Ctrl+f` | Scroll the focused detail pane by a full page |
| `g` / `G` | Jump to top/bottom of results; `g` resets detail-pane scroll |
| `Home` / `End` | Jump within results; `Home` resets focused detail-pane scroll |
| `d` (in detail view) | Open a script diff against the most-recent checkout |
| `Tab` (in detail view) | Browse the script's folder: `j`/`k` highlight an entry, `Enter` opens a sibling script or descends into a subfolder, `[` goes up a level, `Backspace` returns |
| `Ctrl+L` | Focus the results list |
| `Escape` / `Backspace` | Return from script detail view, or quit from browse mode |
| `q` | Quit |

### Mouse

The TUI also responds to the mouse:

| Action | Effect |
|---|---|
| Click a result | Select it; double-click opens the full detail view |
| Click a dependency / function | Select it; click again (or double-click) to jump to it |
| Click the path (header or Metadata pane, or the `Path` line in the detail view) | Copy the full logical path to the clipboard |
| Click the search box | Focus the search input |
| Scroll wheel | Scroll (or move the selection in) the pane under the cursor |

Copy uses the OSC 52 escape sequence, so it reaches your local clipboard even
over SSH. To select text by hand instead (bypassing the app's mouse handling),
hold **Shift** while dragging — the terminal's own selection then works as
usual.

The details pane shows metadata, checkout state, source preview, and related
or dependent scripts for the highlighted result. Press `Enter` on a result to
open a full script detail view with tags, entry points, warnings, checkout
rows, dependency summary, folder siblings, and source preview.

---

## 3 – Inspect a script

Show full details for a known script path:

```bash
scat show /catalog/scripts/patch_check.py
```

### JSON output

```bash
scat show /catalog/scripts/patch_check.py --output json
```

### Folder context

Opt-in fields list the script's parent directory and the other scripts
indexed in the same folder:

```bash
scat show /catalog/scripts/patch_check.py --fields folder,siblings
```

---

## 4 – Dependencies

List what a script depends on and what depends on it:

```bash
scat deps /catalog/scripts/patch_check.py
```

---

## 5 – Script diff

Compare a cataloged script against its checked-out vc copy or an explicit file.

> **vc contract**: `scat diff` is read-only. It never writes to vc-managed
> DEVELOP or ARCHIVE directories.

### Compare active catalog script with its most-recent vc checkout

```bash
scat diff /catalog/foo/bar.py
```

The catalog's stored content is shown as `---` and the most-recent indexed
DEVELOP revision is shown as `+++`. If multiple DEVELOP revisions exist, the one
with the latest timestamp is used automatically.

### Compare against a specific checkout or archive file

```bash
# DEVELOP checkout (filename carries timestamp and user)
scat diff /catalog/foo/bar.py --against /catalog/foo/DEVELOP/bar.py_20240510_103044_alice

# ARCHIVE copy (filename carries timestamp only)
scat diff /catalog/foo/bar.py --against /catalog/foo/ARCHIVE/bar.py_20240510_103044
```

`--against` points to a **file** on disk, unlike `scat catalog diff --against` which
points to a database snapshot.

### Compare any two explicit files

No catalog access is needed; `--db` / `SCAT_DB` is optional for this mode:

```bash
scat diff --old old_version.py --new new_version.py
```

### Machine-readable JSON output

```bash
scat diff /catalog/foo/bar.py --json
```

The JSON output includes:

| Field | Type | Description |
|---|---|---|
| `logical_path` | `string \| null` | Catalog path; `null` when using `--old`/`--new` |
| `old_path` | `string` | Label for the left (old) side |
| `new_path` | `string` | Label for the right (new) side |
| `old_kind` | `string` | `active`, `checkout`, or `explicit` |
| `new_kind` | `string` | `active`, `checkout`, or `explicit` |
| `hunks` | `array` | Changed regions, each with `old_start`, `old_count`, `new_start`, `new_count`, and `lines` |

Each element in `lines` has `kind` (`context`, `delete`, or `insert`) and `content`.

### Error hints

| Situation | Message / next step |
|---|---|
| No checkouts indexed | Run `scat status /catalog/foo.py` or re-index with a vc config, or pass `--against` |
| Script not in catalog | Check the path with `scat search` or use `--old`/`--new` directly |
| Checkout file missing on disk | Pass `--against` with the actual file path |

---

## 6 – Catalog statistics

```bash
scat catalog stats
```

Shows total script count, breakdown by language, and breakdown by owner.
When vc revision data has been indexed and vc is configured, it also prints
revision statistics for DEVELOP/ARCHIVE entries.

```bash
scat catalog stats --json
```

JSON output includes the same revision counters under a `revisions` key when
that data is available.

---

## 7 – Index information

Check when the catalog was last built and which schema version is in use:

```bash
scat catalog info
```

### Shared catalog schema upgrades

Schema v8 stores DEVELOP and ARCHIVE revision rows in the shared catalog. After
deploying a binary with schema v8 support, rebuild the shared database once with
`scat catalog build` before read-only clients use it. If a client reports
`re-index required (scat catalog build)`, the shared catalog is still on an old
schema and needs that rebuild.

---

## 8 – Common CLI patterns

### List all Python scripts

```bash
scat search --lang python --limit 1000
```

### Find scripts owned by a specific user

```bash
scat search --output json | jq '[.[] | select(.owner == "alice@example.com")]'
```

### Check whether a specific script exists

```bash
scat show /catalog/scripts/my_script.py && echo "found" || echo "not found"
```

### Export the full catalog to JSON

```bash
scat search --output json --limit 10000 > catalog.json
```

### Pipe results to `grep`

```bash
scat search "deploy" | grep ".ps1"
```

---

## 9 – Troubleshooting

| Symptom | Fix |
|---|---|
| `Error: No database specified` | Set `SCAT_DB` or pass `--db <path>` |
| `Error: Database file not found` | Check that the path to `scripts.sqlite` is correct and accessible |
| `Error: Database error` | The catalog may be corrupt — ask the CRON team to rebuild it |
| Schema version mismatch warning | Catalog was built with an older version; re‑index or upgrade `scat` |

---

## 10 – Getting help

```bash
scat --help
scat search --help
scat show --help
scat deps --help
scat diff --help
scat catalog stats --help
scat catalog info --help
scat catalog diff --help
scat tui --help
```
