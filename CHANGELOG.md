# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

### Catalog Schema and Indexer

- **`synchronous = NORMAL` and `temp_store = MEMORY` on every connection** ([#47](https://github.com/tjorim/scat/issues/47)) — `create_db` already put new/rebuilt catalogs into WAL journal mode, but didn't pair it with `synchronous = NORMAL` (the recommended setting under WAL: skips the fsync on every commit while still fsyncing at checkpoints, safe because a crash can only roll back the last transaction rather than corrupt the database). Both `create_db` and the read-only `open_db` now also set `temp_store = MEMORY` so temp b-trees (e.g. the `ORDER BY bm25(...)` sort in FTS search) stay off disk. Both are per-connection settings SQLite doesn't persist in the database file, so every connection sets them itself; bulk catalog builds still relax `synchronous` further to `OFF` via `apply_bulk_build_pragmas`.
- **Real-world vc filename parsing** ([#31](https://github.com/tjorim/scat/pull/31)) — the revision filename parser previously required exactly `<script>_YYYYMMDD_HHMM_<user>`, but observed vc trees also use seconds-precision timestamps (`update_board_firmware.sh_20260720_103044_titd`), user-less ARCHIVE names (`update_board_firmware.sh_20240921_135312`), and occasionally date-only stamps — none of which matched, so such environments indexed essentially no DEVELOP or ARCHIVE revisions. The parser now accepts `<script>_<YYYYMMDD[_HHMM[SS]]>[_<user>]`, and timestamp-drift warnings understand all three precisions. `docs/VC_CONTRACT.md` is updated to match observed behavior.
- **Deep DEVELOP/ARCHIVE discovery** ([#31](https://github.com/tjorim/scat/pull/31)) — `scan_checkouts` previously only recognised DEVELOP/ARCHIVE containers at the scan_root level and one subdirectory deep, but every script folder carries its own containers at any nesting depth. The walk now recurses the whole tree under each scan_root (bounding symlinked directories to the configured roots and deduplicating aliases by canonical path, matching the script scanner), so revisions in deeply nested folders are indexed.
- **WORKING revisions** ([#31](https://github.com/tjorim/scat/pull/31)) — the checked-in version copies vc keeps in the working directory next to the active symlink (`<script>_<timestamp>`, no user suffix) are now recorded as revisions with the new type `WORKING` instead of being skipped as unknown extensions. They appear in `scat status` and the TUI revisions pane (sorted between DEVELOP and ARCHIVE); checkout summaries, `scat diff` resolution, and revision stats remain DEVELOP/ARCHIVE-scoped. No schema shape change — the rows appear after the next catalog build.
- **WORKING revisions of extensionless scripts** — the working-directory version copies vc retains beside the active symlink were only recognised as `WORKING` revisions when the script part of the name carried a known script extension (`release_backup.sh_20250311_080827`). For the many vc-managed shell tools that have no extension, every retained version instead fell through to shebang sniffing and was indexed as a script of its own, so a search for `prepare_release` returned the tool plus each of its versions, each with its own preview, metadata, and dependency edges. A version file is now also recognised when a sibling entry named exactly like its script part exists (the active symlink, stat'ed without following it, so a dangling link still counts) — a genuine script whose name merely ends in digits, with no such sibling, still indexes as a script.
- **WORKING revisions are counted in `catalog stats`** — `catalog stats` reported DEVELOP and ARCHIVE totals but nothing for `WORKING`, so the checked-in version copies vc keeps in the working directory were invisible in aggregate. That matters now that the fix above moves them out of `scripts`: without a line of their own, a rebuild looks like an unexplained drop in `total_scripts`. Adds `scripts_with_working_versions` and `total_working_revision_files` to the text and JSON output.
- **Editor backup files are skipped** — `prepare_release~` was indexed as a duplicate of `prepare_release`, while `release_backup.sh~` was already dropped as an unknown extension. Files ending in `~` are now skipped regardless of extension.
- **Scan progress shows the current folder** ([#31](https://github.com/tjorim/scat/pull/31)) — the `scat catalog build` scan spinner now updates on every walked entry and shows the directory currently being traversed; previously the message only refreshed on a rare filter/throttle coincidence and showed a bare filename, so it looked frozen.

- **Schema v10 cleanup** — retired the redundant `vc_checkouts` table and now derives script checkout summaries from `revisions` rows with `revision_type = 'DEVELOP'`. Deployments must run `scat catalog build --force` once so old schema-9 catalogs are rebuilt.
- **Parallel file scanning** ([#42](https://github.com/tjorim/scat/issues/42)) — `catalog build`'s directory walk previously did every file's stat, shebang sniff, and symlink resolution on the single thread doing the walking, which dominates build time on a large catalog. That per-file work is now farmed out across a rayon worker pool in batches (the walk itself, and the pruning/progress logic that depends on visiting entries in order, stays on one thread), cutting scan time on multi-core machines. New `--threads <n>` flag on `scat catalog build` (default: number of logical CPUs) bounds parallelism for both this phase and phase 2's extraction, which already ran on rayon.
- **Schema v11 — path-literal "referenced" dependencies** ([#29](https://github.com/tjorim/scat/pull/29)) — the indexer now records a second kind of dependency edge beyond language imports: a `referenced` edge for scripts invoked *by path* rather than imported. It scans each file's body (across Python, shell, and JSON/YAML manifests) for path-shaped literals ending in `.py`/`.sh`/`.bash`/`.ksh`, so a shell script that `scp`/`ssh`-executes another script, a Python file calling `subprocess`/`paramiko` with a script path, or a JSON manifest listing scripts to run in order all produce edges the AST import extractors could never see. Precision comes from resolution: a candidate is kept only when it resolves to an indexed script — an absolute logical path matched exactly, or a relative path (`./x.py`, `../lib/x.py`) resolved against the referencing script's own directory — and unresolved candidates (logs, temp files, copy destinations) are dropped. Resolution is language-agnostic, so cross-language edges (a shell script invoking a Python one, or vice versa) are captured. A bash `source`/`.` of a *path* (`source ../lib/common.sh`) is now recorded as a `referenced` edge too — previously it produced an `import` edge that the Python module resolver mis-resolved by matching the bare file extension (`sh`) as a module suffix (the same collision also let `import json` wrongly resolve to an indexed `.json` file); bare-name sources (`source common.sh`) remain `import` edges resolved by basename. The `dependencies` table gains a `kind` column (`import` / `referenced`); deployments must run `scat catalog build --force` once so old schema-10 catalogs are rebuilt.

### CLI and Output

- **Shell completions** — added `scat completions <shell>` which prints a completion script for bash, zsh, fish, PowerShell, or elvish on stdout (e.g. `scat completions bash > /etc/bash_completion.d/scat`). Needs no catalog database.
- **Transitive dependency trees** — added `--tree` and `--depth <n>` (default depth 5, implies `--tree`) to `scat deps`, rendering cycle-safe "Uses" and "Used by" trees with box-drawing branches. Repeated subtrees are collapsed and marked `(*)`, back-edges to an ancestor are marked `(cycle)`, unresolved dependencies are marked `(not indexed)`, and nodes with children beyond the depth limit are marked `(…)`; a legend is printed when markers appear. `--output json` emits the nested trees with `cycle`/`repeated`/`truncated` flags.
- **Dependency edge kinds in `scat deps`** — `scat deps` now distinguishes language imports from path references (see schema v11 above): the flat "Uses" table has a `Kind` column (`import` / `ref`), the tree marks path-reference edges with `(ref)` and a legend, and both the flat and tree JSON carry the edge kind (`kind` on flat entries, `via` on tree nodes).
- **`NO_COLOR` support** — `scat` now honors the [`NO_COLOR`](https://no-color.org/) environment variable in addition to the `--no-color` flag when deciding whether to colorize output.
- **Clearer not-found errors** — `scat show` and `scat deps` now report a missing script through the central error path with a clean message instead of a raw `tracing` log line followed by a bare `exit(1)`.
- **Cross-platform basename ranking** — search result name-relevance ranking now splits on both `/` and `\` so basenames resolve correctly for paths carrying Windows-style separators.
- **Symlink arrows survive a narrow terminal** — the `↳` sub-row under a symlinked script repeated the target's full logical path, and the path column truncates from the left to keep filenames visible, so on a narrow terminal the leading `↳` marker — the only thing that made the row mean anything — was the first casualty. A target in the same directory as its symlink (vc's active-version links always are) is now written as a bare filename.
- **Consistent list-field JSON** — `tags`, `entry_points`, and `related` now always serialize as a JSON array in `scat show`/`scat search` output: an absent list renders as `[]` instead of `null`, matching the existing `scat diff` behavior.

### TUI

- **Search filters** — the TUI search box now understands `lang:` (or `language:`), `owner:`, and `tag:` tokens, matching the CLI's `--lang`/`--owner`/`--tag` flags (e.g. `backup lang:python owner:alice`). Active filters are shown in the search pane title, a filter-only query lists all matching scripts, and a filter key with an empty value (mid-typing) is ignored.
- **Path-aware search routing** — TUI queries containing `/` or `.` now route to the same INSTR-based path search the CLI uses instead of erroring as invalid FTS5 syntax, so `jobs/nightly` or `foo.py` find scripts by path fragment.
- **Read-only full-script viewer** ([#15](https://github.com/tjorim/scat/issues/15)) — added a TUI action to open the full selected script in an external read-only viewer/editor, bypassing the `PREVIEW_LINES` preview cap.
  - `v` opens the **indexed catalog content** (`scripts.content`) written to a temp file that preserves the original filename/extension for syntax highlighting.
  - `V` opens the **live filesystem source** resolved through the configured path mapping (falling back to the logical path when it is already a real file), and fails clearly when the source file cannot be found. The existence check runs on a background worker (with stale-request draining) to keep the render loop responsive on slow network mounts.
  - Viewer selection prefers `$SCAT_EDITOR`, then `$VISUAL`, then `$EDITOR`, with a `view`/`vim -R`/`vi -R`/`less` (or `notepad`) fallback; Vim-compatible editors are forced into read-only mode.
  - On Windows, an unquoted editor path with backslashes (e.g. `C:\Tools\vim.exe`) is split on whitespace instead of through `shell_words`, so the backslashes are no longer consumed as shell escapes and the path is preserved.
  - Clarified the preview pane title and footer hints so the catalog-vs-live-source distinction is explicit.
  - When the indexed script is longer than the `PREVIEW_LINES` cap, the preview title shows `first 500 of N lines, v/V for full`, so it is clear the preview is clipped and the full-script viewer is available.
- **Symlink targets are shown** — the TUI never displayed a script's symlink target anywhere, so a vc-managed script gave no indication of which version was live, and the CLI's `↳` sub-row had no counterpart. The results list now appends `→ <target>` to a symlinked script's row (bare filename for a target in the same directory, full path for one elsewhere), and the metadata pane shows the full target on its own `Symlink` line.
- **Result ordering matches the CLI** — TUI results were rendered in raw BM25 order while the CLI re-ranks FTS hits by name relevance, so the script actually named like the query could sit below its own near-namesakes. The TUI now applies the same ranking.
- **WORKING revisions have their own group in the revisions pane** — the pane grouped `DEVELOP` and `ARCHIVE` explicitly and swept everything else into `OTHER`, which is where every working-directory version copy landed. They now appear under a `WORKING` heading between the two, matching their documented sort position.
- **The live version is marked in the revisions pane** — nothing distinguished the version a script's symlink actually resolves to from the other retained copies, and their order does not imply it: a rollback re-points the symlink at an older version and leaves the newer one in place, so the group can hold versions both older *and* newer than the live one. The matching revision is now marked `← active`.
- **Scroll clamping** — the preview, detail, diff, and revisions panes now clamp their scroll offset to the content height, removing the misleading blank-space scrolling past the end of shorter scripts.
- **Consistent empty-field placeholder** — the detail/metadata/functions panes now show an em dash (`—`) for empty values, matching the placeholder already used across the CLI table, `show`, and `status` output instead of a plain hyphen.

### Documentation and Configuration

- **Example config files** ([#31](https://github.com/tjorim/scat/pull/31)) — added `docs/scat.example.yaml` (annotated example for the `--config`/`SCAT_CONFIG` file: `db_path`, `scan_roots`, `ignore_patterns`, `vc.*`, `bookmarks`) and `docs/scat-mapping.example.yaml` (the separate `scat tui --mapping`/`SCAT_MAPPING` path-mapping file), plus a README "Configuration" section and QUICKSTART notes pointing at both. The QUICKSTART `--against` examples now show the real per-folder DEVELOP/ARCHIVE layout and version-suffix naming.

### Internal

- **Typed `scripts` row view** ([#17](https://github.com/tjorim/scat/issues/17)) — introduced `scat_core::core::script_view::ScriptView`, a thin read-only wrapper over a queried `scripts` row that exposes typed accessors for the known columns plus parsed helpers for the JSON-encoded `tags`/`entry_points`/`related`/`metadata_json`/`vc_warnings` fields. The JSON, CSV, table, and TUI detail/metadata renderers (and the catalog-diff field extraction) now share this one definition of each column's name and parsing/fallback semantics instead of re-reading raw `JsonRow` keys by hand. Behavior-preserving: command and TUI output are unchanged.

### Build

- **Leaner release profile** ([#45](https://github.com/tjorim/scat/issues/45)) — `[profile.release]` now sets `lto = "thin"`, `codegen-units = 1`, `panic = "abort"`, and `strip = true` (build dependencies keep a `codegen-units = 4` override so they still compile quickly). On Linux x86_64, the release binary shrank from 12.8 MB to 9.4 MB (about 27% smaller); behavior is unchanged, since nothing in scat relies on unwinding across a panic. Full profile-guided optimization (instrumented build + representative training run) is a larger, separate undertaking and was left out of this pass.

## [Completed] VC Revision Indexing and Catalog Audit Hardening (May 2026)

### Catalog Schema and Indexer

- **Indexed revisions table** — added first-class `revisions` storage for vc `DEVELOP` / `ARCHIVE` rows, including logical path, physical path, revision type, OS flavor, user, timestamp, and age.
- **Revision-aware indexing** — grouped vc revision files under parent script logical paths during catalog builds and persisted revision rows alongside legacy checkout summaries.
- **Schema v8 rollout** — bumped the catalog schema and made old-schema failures actionable with `re-index required (scat catalog build)` guidance.
- **Shared catalog upgrade notes** — documented the required one-time `scat catalog build` after deploying schema v8 binaries.

### Audit, Stats, and TUI

- **DB-backed audit warnings** — moved catalog audit warning inference onto indexed SQLite revision state instead of fresh filesystem scans, so audit output reflects the built catalog.
- **Revision-backed checkout reads** — moved user-facing `scat status`, `scat diff`, and checkout audit reads onto the indexed `revisions` table; `vc_checkouts` remains a derived compatibility summary.
- **Revision statistics** — added optional `revisions` counters to `scat catalog stats` and JSON output, including active checkout scripts, archive-entry scripts, total revision files by type, and multi-user active checkouts.
- **TUI revisions pane** — loads indexed revision rows through the existing background detail worker and groups real `DEVELOP` / `ARCHIVE` entries in the revisions pane.

## [Completed] Export, Ignore Rules, Regex, and TUI Workers (May 2026)

### CLI and Indexer

- [#91](https://github.com/tjorim/scat/issues/91) **Export formats** — added CSV and JSON export support to `scat search` (#96)
- [#92](https://github.com/tjorim/scat/issues/92) **Regex search** — added `--regex` search alongside full-text search, with coverage for regex matching behavior (#99)
- [#93](https://github.com/tjorim/scat/issues/93) **Ignore patterns** — added `.catignore` discovery and explicit `--ignore-file` support to `scat catalog build` (#101)
- [#95](https://github.com/tjorim/scat/issues/95) **MSRV policy** — documented Rust 1.88.0 as the minimum supported Rust version and added a locked `cargo check --all-targets` CI job for it

### TUI

- **Background search worker** — moved TUI searches off the render path with debounce, stale-result guards, and in-flight indicators (#102)
- **Background detail worker** — replaced per-selection detail threads with a long-lived `DetailWorker` to reduce churn during result navigation (#109)

## [Completed] Search, Show, and UX improvements (May 2026)

### CLI

- **`show`** — merged `inspect` into `show`; added `--fields` flag (à la `ps -eo`) with 13 available fields and a sensible default set; auto-resolves symlinks with a clear redirect note; human-readable size formatting
- **`symlinks`** — now shows both directions (outbound target + inbound aliases); groups cleanly when neither applies; correctly handles unindexed targets
- **`search`** — auto-routes to `INSTR`-based path search when query contains `/` or `.` (fixes FTS5 syntax errors); filename relevance re-sort for FTS5 results; symlinks grouped under their targets with `↳` sub-rows; path column truncates from the left so filenames stay visible

### Infrastructure

- [#49](https://github.com/tjorim/scat/issues/49) **`cargo clippy -D warnings`** enforced in CI (#75)
- [#48](https://github.com/tjorim/scat/issues/48) **Integration tests** for core search, indexer, and vc checkout scanning (#76)
- [#55](https://github.com/tjorim/scat/issues/55) **Structured tracing** and global `-v`/`-vv` verbosity (#77)
- [#56](https://github.com/tjorim/scat/issues/56) **Indexing UX** — progress bar, file rate, Ctrl-C abort (#78)
- [#51](https://github.com/tjorim/scat/issues/51) **Rich-style table rendering** — dynamic column widths, left-truncation for paths (#79)
- [#59](https://github.com/tjorim/scat/issues/59) **Schema version validation** on `open_db` (#81)
- [#57](https://github.com/tjorim/scat/issues/57) **`scat catalog diff`** — compare catalog snapshots across index runs (#84)
- [#58](https://github.com/tjorim/scat/issues/58) **`scat catalog audit`** — 7 health checks, filtered execution, strict exit codes (#85)
- [#69](https://github.com/tjorim/scat/issues/69) **Python AST function/class dependency graphs** extracted and persisted (#86)
- **rustdoc coverage** enforced with `missing_docs` warnings (#88)

## [Completed] Rust Migration (May 2026)

Complete rewrite of scat's CLI and indexer from Python to Rust, producing cross-platform native binaries. Tracked in [#40](https://github.com/tjorim/scat/issues/40) and phased in [#35–#39](https://github.com/tjorim/scat/issues).

### Phase 0: Integration Spike ([#35](https://github.com/tjorim/scat/issues/35))
- Validated SQLite (rusqlite) with bundled FTS5
- Validated tree-sitter (Python + Bash grammars)
- Validated YAML/JSON serialization (serde ecosystem)

### Phase 1: Core Layer + CLI ([#36](https://github.com/tjorim/scat/issues/36))
- Implemented SearchApi (full-text search, dependency graph, related scripts)
- Implemented database layer with schema migration support
- Implemented path resolution (logical → physical path mapping)
- Implemented 11 CLI commands: `search`, `show`, `status`, `explain`, `depends`, `stats`, `symlinks`, `info`, `vc`, `index`, `tui`
- Added JSON output support (`--json` flag)

### Phase 2: Indexer ([#37](https://github.com/tjorim/scat/issues/37))
- Implemented file scanner (recursive directory traversal)
- Implemented metadata extractor (headers, docstrings, sidecar YAML)
- Implemented AST-based dependency detection (Python imports)
- Implemented tree-sitter-based dependency detection (shell sourcing)
- Implemented atomic indexing (backup rotation, transactional updates)
- Added vc config YAML/JSON support

### Phase 3: CI/CD, Deployment, and Cleanup ([#38](https://github.com/tjorim/scat/issues/38))
- Set up GitHub Actions workflows for automated builds
- Configured Linux cross-compilation (x86_64-unknown-linux-musl with musl-tools)
- Configured Windows builds (x86_64-pc-windows-msvc)
- Added smoke tests for database compatibility
- Added binary size reporting
- Added artifact uploads for release distribution
- **Cleanup complete**: Removed `wheelhouse/`, `third_party/`, `scat.sh`, `requirements.txt`, `native/`

### Phase 4: TUI ([#39](https://github.com/tjorim/scat/issues/39))
- Implemented multi-pane Ratatui TUI with live search
- Implemented browse mode: search box + results + metadata + preview + related scripts
- Implemented detail mode: full script content with scrolling
- Added vim keybindings (j/k, g/G, Ctrl+u/d, Ctrl+b/f, etc.)
- Added Tab/Shift+Tab pane navigation
- Added terminal state management (raw mode, alternate screen)
- Fixed `--mapping` flag to properly resolve and display native OS paths ([#47](https://github.com/tjorim/scat/issues/47), PR #74)

## Technical Decisions

- **CLI Framework**: `clap` (derive macros for ergonomic CLI definition)
- **Database**: `rusqlite` with bundled SQLite + FTS5 (no external SQLite dependency)
- **Serialization**: `serde` + `serde_json` + `yaml_serde` (official YAML org)
- **Dependency Detection**: Tree-sitter + regex (AST-aware for Python imports, pattern-based for shell)
- **Terminal UI**: `ratatui` + `crossterm` (cross-platform, reactive, vim-keybindings)
- **YAML**: `yaml_serde` (actively maintained by official YAML organization)

## Known Limitations

- TUI shows first 500 lines of script content (configurable via `PREVIEW_LINES`)
- Search limited to 200 results (configurable via `RESULT_LIMIT`)
- No incremental indexing (full rebuild required)
