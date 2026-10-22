# scat Quick‑Start Guide

Find, browse, and understand scripts in the catalog. For the complete workflow
and terminal reference, see [TUI and CLI option coverage](TUI_OPTIONS.md).

---

## 0 – Prerequisites

| Requirement | Notes |
|---|---|
| `scat` | Compiled binary (RHEL / Linux x86-64) |
| `SCAT_DB` or `--db` | Path to the shared `scripts.sqlite` catalog |

Set the environment variable once so you never need to type `--db` again:

```bash
export SCAT_DB=/catalog/scat/scripts.sqlite
```

`scat` reads that catalog through a host-local copy in `/dev/shm`, refreshed
automatically after each nightly rebuild — nothing to set up. Use
`--cache-dir` / `SCAT_CACHE_DIR` to put the copy elsewhere, or `--no-cache` /
`SCAT_NO_CACHE=1` to query the shared drive directly.

### Optional: config file

Instead of (or in addition to) `SCAT_DB`, you can point scat at a config
file with `--config <path>` or `SCAT_CONFIG` for the database path, scan
roots, ignore patterns, vc settings, and search bookmarks (`scat search
@name`). See **[scat.example.yaml](scat.example.yaml)** for an annotated
example. CLI flags and env vars always override config-file values.

### Optional: path-mapping file

`scat tui --mapping <path>` / `SCAT_MAPPING` points at a separate file that
maps logical catalog paths to real filesystem paths, used by the `V` key to
open a script's live source. See
**[scat-mapping.example.yaml](scat-mapping.example.yaml)**.
The same file works with `scat doctor PATH --mapping <path>` for host-local
diagnostics; `SCAT_MAPPING` applies to TUI startup.

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
| `←` / `→`, `Home` / `End` (in search) | Move the insertion cursor |
| `Backspace` / `Delete` (in search) | Delete before/after the cursor, or delete the selection |
| `Shift` + cursor keys (in search) | Select text; typing/pasting replaces the selection |
| `Ctrl+A` (in search) | Select the whole query |
| `Ctrl+←` / `Ctrl+→` (in search) | Move by word (add Shift to select) |
| `Ctrl+W` / `Ctrl+Backspace` / `Ctrl+Delete` (in search) | Delete the previous/next word |
| `Ctrl+U` / `Ctrl+K` (in search) | Delete from the cursor to the start/end |
| `F1` / `F4` | Contextual help / searchable command palette |
| `F6` | Edit search mode, filters and result limit |
| `F7` / `F8` | Similar scripts / checkout and warning status |
| `F9` / `F10` | Dependency trees / symlink inspection |
| `Alt+i` / `Alt+a` | Catalog information / audit |
| `Alt+d` | Host diagnostics |
| `Alt+b` / `Alt+v` | Explicit catalog-build / vc administration dialogs |
| `v` / `V` (outside search) | Open indexed content / live source in an external read-only viewer |
| `F2` | Toggle native terminal text selection; Esc also resumes the TUI |
| `F11` | Compare catalog scripts with files/revisions, two files, or catalog snapshots |
| `F12` | Export JSON/search CSV; choose scope, fields and a local save path |
| `F3` | Copy the search selection/query, preview content, script details, diff, or selected script path |
| `Enter` | Move from search to results, or open the selected script detail view |
| `Tab` / `Shift+Tab` | Cycle between search, results, preview, and related panes |
| `↑` / `↓` or `j` / `k` | Navigate results or scroll the focused detail pane |
| `PageUp` / `PageDown` | Scroll the focused preview or related pane |
| `Ctrl+u` / `Ctrl+d` | Scroll the focused detail pane by a half page |
| `Ctrl+b` / `Ctrl+f` | Scroll the focused detail pane by a full page |
| `g` / `G` | Jump to top/bottom of results, preview, detail, or diff |
| `Home` / `End` | Jump to top/bottom of results, preview, detail, or diff |
| `d` (in detail view) | Open a script diff against the most-recent checkout |
| `Tab` (in detail view) | Browse the script's folder: `j`/`k` highlight an entry, `Enter` opens a sibling script or descends into a subfolder, `[` goes up a level, `Backspace` returns |
| `Ctrl+L` | Focus the results list |
| `Escape` | Return to the previous view, exit fullscreen, or quit from browse mode |
| `Backspace` (in detail or diff) | Return to the previous view |
| `Ctrl+C` | Quit from any view |

### Mouse

The TUI also responds to the mouse:

| Action | Effect |
|---|---|
| Click a result | Select it; double-click opens the full detail view |
| Click a dependency / function | Select it; click again (or double-click) to jump to it |
| Click the path (header or Metadata pane, or the `Path` line in the detail view) | Copy the full logical path to the clipboard |
| Click / drag in the search box | Place the insertion cursor / select query text |
| Scroll wheel | Scroll (or move the selection in) the pane under the cursor |

Press **F2** to select arbitrary screen text using your terminal's normal
mouse drag and copy shortcut. The TUI releases the mouse and pauses background
updates so the selection stays intact. Press **F2** or **Esc** to resume.
**Shift+drag** also bypasses mouse capture in terminals that support it.

**F3** copies the selected query text (or the whole query when nothing is
selected). In other panes it copies the focused preview, details, diff, or
selected script path. Clicking a script path still copies that path.
These actions request clipboard access via OSC 52, including over SSH;
your terminal or multiplexer must allow it. The TUI reports the request,
but cannot confirm that the terminal accepted it. Native terminal selection
and copy provide an alternative when OSC 52 is unavailable.

Use your **terminal's paste shortcut** (commonly Ctrl+Shift+V or Cmd+V) in
search. On terminals supporting bracketed paste, pasted text is inserted in
one operation at the cursor, replacing any selection; newlines and tabs become
spaces and other control characters are removed. Pasting in other panes is
ignored with a hint to focus search. On terminals without bracketed paste,
input arrives as ordinary keystrokes, so paste single-line text into search.
**Ctrl+C quits the app**; use your terminal's copy shortcut to copy native
selections. Esc clears a query selection before exiting search.

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

### Select an indexed revision

```bash
scat diff /catalog/foo/bar.py --revision-type ARCHIVE
```

Select DEVELOP, WORKING, ARCHIVE or ROLLBACK; optionally narrow with
`--revision-timestamp`, or `--revision-user` for DEVELOP/ROLLBACK checkouts.
Use `scat status /catalog/foo/bar.py` to inspect available revisions.

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
| `old_kind` | `string` | `active`, `checkout`, `revision`, or `explicit` |
| `new_kind` | `string` | `active`, `checkout`, `revision`, or `explicit` |
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

The current catalog schema is **v14**. A client reads exactly the schema it was
built for, so a schema change is a coordinated upgrade of the publisher, the
catalog and the clients. `scat catalog info` prints the catalog's and the
client's schema versions and a `compatibility` verdict even for a catalog the
client refuses, and exits non-zero when it is unreadable.

The error says which side is behind:

- *the catalog is older than this scat* — the publisher must upgrade scat and
  rebuild: `scat --config /path/to/site.yaml catalog build`.
- *this scat is older than the catalog* — upgrade scat on this host.

Ordinary read-only clients never rebuild the shared catalog. The decision,
supported combinations, rollout and rollback steps are in
[COMPATIBILITY.md](COMPATIBILITY.md).

---

## 8 – Common CLI patterns

### List all Python scripts

```bash
scat search --lang python --limit 1000
```

### Find scripts owned by a specific user

```bash
scat search --owner alice@example.com --limit 1000
```

### Check whether a specific script exists

```bash
scat show /catalog/scripts/my_script.py && echo "found" || echo "not found"
```

### Export up to 10,000 matching scripts to JSON

```bash
scat search --output json --limit 10000 > catalog.json
```

### Pipe results to `grep`

```bash
scat search "deploy" | grep -F ".sh"
```

---

## 9 – Troubleshooting

| Symptom | Fix |
|---|---|
| `Error: No database specified` | Set `SCAT_DB` or pass `--db <path>` |
| `Error: Database file not found` | Check that the path to `scripts.sqlite` is correct and accessible |
| Database access error | Run `scat doctor --json`; check the reported path, permissions and availability before asking the publisher to rebuild |
| `catalog schema is version N, expected M` | Run `scat catalog info`; the message says whether to upgrade this client or have the publisher upgrade and rebuild — see [COMPATIBILITY.md](COMPATIBILITY.md) |
| `note: embeddings sidecar … was built from catalog build …` | The sidecar was built from a different catalog build; rerun `scat-embed` against the current catalog and republish it |

---

Use `scat doctor` without a working catalog for read-only host diagnostics.
For a specific script, include its path and optionally query local RPM metadata:

```bash
scat doctor --json
scat doctor /catalog/scripts/patch_check.py --mapping mapping.yaml --rpm --json
```

## 10 – Getting help

```bash
scat --help
scat doctor --help
scat search --help
scat show --help
scat deps --help
scat diff --help
scat catalog stats --help
scat catalog info --help
scat catalog diff --help
scat tui --help
```
