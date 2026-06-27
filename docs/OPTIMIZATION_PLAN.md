# scat — Code Review & Optimization Plan

> Reviewer pass over the current `scat` (Script Catalog) implementation.
> The task templates (`IMPLEMENTATION_PLAN`, `TECHNICAL_SPECIFICATION`,
> `PROJECT_REQUEST`, `PROJECT_RULES`) were empty, so this review uses the
> shipped sources of truth instead: `README.md`, `docs/ARCHITECTURE.md`, and
> the code under `src/`. All recommendations respect the architecture rules
> (logical paths only, read-only clients via `mode=ro`/`SQLITE_OPEN_READ_ONLY`,
> CRON-only indexing, no web UI) and the contributing rules
> (backward-compatible, no internet-at-runtime deps, boring over clever).

<analysis>
Here is my detailed review of the current codebase.

## 1. Code Organization & Structure

**Strengths**
- Clean top-level separation: `core` (db/search/resolve/diff/vc), `indexer`
  (scan/extract/deps/atomic/checkpoint), and `tui` (with dedicated async
  workers). The `lib` (`scat_core`) vs `bin` (`scat`) split is correct and lets
  command handlers and rendering live in the binary while query primitives stay
  reusable and testable in the library.
- The async TUI architecture (search/detail/diff workers + monotonic request
  IDs to discard stale responses) is genuinely good design and well tested.
- `#![warn(missing_docs)]` on the library keeps the public API documented.

**Problems**
- **`src/main.rs` is 1,315 lines** and conflates four responsibilities: the
  process entry point, CLI dispatch (`run`/`cmd_catalog`), every command handler
  (`cmd_search`, `cmd_show`, `cmd_status`, `cmd_deps`, `cmd_symlinks`, `cmd_diff`,
  `cmd_index`, `cmd_audit`, …), and presentation helpers
  (`render_revision_lines`, `relative_age`, `print_catalog_diff`). This is the
  single biggest navigability cost in the repo.
- **`src/tui.rs` is 1,849 lines.** `TuiApp` carries ~40 fields and three key
  handlers (`handle_key`, `handle_detail_key`, `handle_detail_diff_key`) whose
  scroll/navigation arms are heavily copy-pasted (see §2). The detail-rendering
  helpers at the bottom of the file are a second concern living in the same file.
- **Stale schema comment.** `src/core/db.rs:15` labels the DDL block
  `DDL (schema version 8)` while `SCHEMA_VERSION = 9` (`db.rs:9`). Comment and
  constant have drifted.
- **Redundant intermediate table: `vc_checkouts`.** The table is created
  (`db.rs:87`) and written by the indexer (`indexer/builder/pipeline.rs`). No
  *client query* reads it — all user-facing revision reads go through the
  first-class `revisions` table (the DDL comment itself says "prefer the
  first-class revisions table below"). Its one remaining consumer is the
  indexer's own `apply_checkout_summaries` (`pipeline.rs:407`), which `GROUP`s it
  to populate the `scripts.checkout_*` columns. Since `revisions` already holds
  the same per-checkout rows (filterable by `revision_type = 'DEVELOP'`),
  `vc_checkouts` is a redundant intermediate: extra write + index work each
  nightly build and a second source of truth for the same data. Retiring it means
  repointing `apply_checkout_summaries` at `revisions`, not simply deleting it.
- **Test-only public API.** `search.rs` exposes both `_with_filters` methods and
  their thin non-filter wrappers (`search`, `search_by_path`, `search_by_regex`,
  `search_scripts_by_function`, `get_callers_of`) plus `db::fts_query`. Grep
  shows the non-filter variants are called only from `tests/` (and two from the
  TUI workers). They inflate the documented public surface without production
  callers.

## 2. Code Quality & Best Practices

**Strengths**
- Consistent `thiserror` core error + `anyhow` at the binary boundary; SQL is
  always parameterized; FTS filters are applied in SQLite *before* `LIMIT`.
- Good Unicode-aware table truncation, CSV escaping, and SQLite-param chunking
  (`related_scripts` honors the 999-variable limit).

**Problems**
- **Duplicated filter SQL.** The `language`/`owner`/`tag` predicates exist twice:
  `append_script_filters` (`search.rs:178`) and inline inside
  `fts_query_filtered` (`db.rs:261-274`). Worse, `list_scripts` (`search.rs:343`)
  hand-writes **eight** near-identical SQL strings for every combination of the
  three optional filters instead of composing them dynamically. Three copies of
  the same logic that can silently drift.
- **Duplicated formatting/sorting.** `relative_age` is byte-for-byte identical in
  `main.rs:637` and `tui.rs:1215`. Revision ordering is implemented twice
  (`main.rs::render_revision_lines` and `tui.rs::sort_checkouts`) with the same
  DEVELOP-first / os / newest-first intent.
- **Scattered row-string helpers.** `str_field` (output.rs, tui.rs), `str_val`
  (search.rs), `json_str_field`/`display_field` (tui.rs) are four
  slightly-different "read a column as a string" helpers. They differ in their
  empty/`—`/`-` fallbacks, which is exactly the kind of subtle inconsistency that
  leaks into output.
- **`process::exit(1)` inside command handlers.** `cmd_show` (`main.rs:428`) and
  `cmd_deps` (`main.rs:703`) call `error!(...)` then `process::exit(1)` on
  "script not found", bypassing the single error funnel in `main()` (which
  already logs and exits 1). This makes those handlers untestable for the
  not-found path and splits the exit-code policy across the file.
- **Massive match duplication in the TUI.** The `match self.focus { Preview =>
  &mut self.preview_scroll, Revisions => &mut self.revisions_scroll, _ =>
  unreachable!() }` block is repeated ~7 times in `handle_key`, and the
  `scroll_by(self.detail_scroll, …)` ladder repeats across the two detail
  handlers. A `scroll_target()` accessor + a small "apply scroll delta from key"
  helper would remove dozens of lines and the `unreachable!()` fragility.
- **`which_vc` shells out.** `runtime.rs:92` spawns `which`/`where` once per
  candidate name to locate the vc executable, which is slow and platform-fragile;
  a direct `PATH` walk (respecting `PATHEXT` on Windows) avoids subprocesses.
  `cmd_vc` also does `vc_exe.unwrap()` after an `is_none()` early-return, which
  reads as a latent panic even though it is currently safe.

## 3. UI/UX

**Strengths**
- Helpful empty states ("No results found.", "No dependencies found for …"),
  `--no-color` honored in all `comfy-table` output, left-truncation of long paths
  so the filename stays visible, and a name-relevance re-rank on FTS results so
  exact filename matches float to the top.
- TUI has a footer hint row, fullscreen toggle, debounced search, and vim-style
  navigation.

**Problems**
- **`NO_COLOR` env var ignored.** The de-facto cross-tool convention
  (`https://no-color.org`) is unsupported; only the explicit `--no-color` flag
  disables color. Air-gapped RHEL users piping into logs would expect `NO_COLOR`
  to work.
- **Not-found errors are log lines, not messages.** Because default verbosity is
  `warn`, `scat show /nope` emits a structured `tracing` record
  (`ERROR scat... script not found path=/nope`) rather than a clean
  `error: script '/nope' not found in catalog`. Functional, but inconsistent with
  the polished `anyhow` context messages used elsewhere.
- **Color is hard-coded in the TUI.** Cyan headers/labels are emitted
  unconditionally; the CLI `--no-color`/`NO_COLOR` intent does not reach the TUI.
- **README technical-stack drift.** README §Implementation claims *"11 commands
  (search, show, status, explain, depends, stats, symlinks, info, vc, index,
  tui)"*. The actual surface is `search/show/status/deps/symlinks/diff/vc/tui`
  plus `catalog {build,stats,info,audit,diff}` — there is no `explain`, `depends`
  is `deps`, and `index/stats/info` moved under `catalog`. User-facing docs no
  longer match the CLI.

Overall: a mature, well-tested codebase. The highest-leverage work is *de-
duplication* (filter SQL, formatting helpers, TUI scroll handling) and *file
decomposition* (`main.rs`, `tui.rs`), plus small correctness/UX polish. None of
the changes below alter the database contract, the read-only rule, or
externally observed behavior except where explicitly noted (NO_COLOR, cleaner
error text).
</analysis>

# Optimization Plan

Each step is atomic, preserves existing behavior (unless noted), keeps within
~20 file edits, and ends with a verifiable success criterion. Recommended order
is top-to-bottom; dependencies are called out where they exist. After every
step: `cargo fmt`, `cargo clippy --all-targets`, and `cargo test` must stay
green.

## Documentation & Quick Wins

- [x] **Step 1: Fix stale schema-version comment and README command list**
  - **Task**: Correct the DDL banner comment to stop referencing "version 8",
    and update the README technical-stack command list to match the real CLI
    surface (`search/show/status/deps/symlinks/diff/vc/tui` +
    `catalog {build,stats,info,audit,diff}`). Pure docs/comment change.
  - **Files**:
    - `src/core/db.rs`: change the `DDL (schema version 8)` comment to reference
      `SCHEMA_VERSION` generically (e.g. "DDL — see `SCHEMA_VERSION`").
    - `README.md`: fix the "11 commands (…)" sentence and the Features bullet.
  - **Step Dependencies**: None.
  - **User Instructions**: None.
  - **Success Criteria**: No source comment claims a hard-coded schema number
    that disagrees with `SCHEMA_VERSION`; `README` command names all resolve to
    real `clap` subcommands.

- [x] **Step 2: Honor the `NO_COLOR` environment variable**
  - **Task**: Treat color as disabled when `--no-color` is passed *or* the
    `NO_COLOR` env var is present and non-empty. Compute this once in `main`/CLI
    and thread the existing `no_color: bool` through unchanged.
  - **Files**:
    - `src/cli.rs`: after parsing, OR `no_color` with
      `std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty())` (or do it in
      `main::run`).
    - `src/main.rs`: ensure the resolved flag is what handlers receive.
  - **Step Dependencies**: None.
  - **Success Criteria**: `NO_COLOR=1 scat search foo` produces no ANSI escapes;
    a new unit test asserts the resolution logic. Existing `--no-color` tests
    still pass.

- [x] **Step 3: Clean user-facing "not found" messages**
  - **Task**: Replace the `error!(...) + process::exit(1)` pattern in `cmd_show`
    and `cmd_deps` with returned `anyhow` errors carrying a plain message
    (`script '{path}' not found in catalog`). Let the existing `main()` funnel
    handle logging and exit code 1.
  - **Files**:
    - `src/main.rs`: `cmd_show`, `cmd_deps` (and audit any sibling handler doing
      the same) return `anyhow::bail!` instead of exiting.
  - **Step Dependencies**: None.
  - **Success Criteria**: `scat show /missing` exits 1 with a single clean
    `error:`-prefixed line; handlers no longer call `process::exit` for
    not-found; a unit/integration test covers the not-found path.

## De-duplication (Quality)

- [x] **Step 4: Centralize row→string accessors**
  - **Task**: Introduce one shared set of row accessors (`str_field` with the
    `—` fallback for display, plus a raw `str_or_empty`) in a single module and
    have `output.rs`, `main.rs`, `tui.rs`, and `core/search.rs` use them instead
    of their private `str_field`/`str_val`/`json_str_field`/`display_field`
    copies. Preserve each call site's *current* fallback semantics explicitly
    (display `—` vs raw `""` vs TUI `-`).
  - **Files**:
    - `src/output.rs` (or a new `src/core/row.rs` in the lib if shared with
      `scat_core`): canonical helpers.
    - `src/main.rs`, `src/tui.rs`, `src/core/search.rs`: replace local copies.
  - **Step Dependencies**: None (do before Step 6/7 to reduce churn).
  - **Success Criteria**: Only one definition of each accessor remains; output of
    `search`, `show`, `status`, and the TUI detail pane is byte-identical to
    pre-change (covered by existing tests).

- [ ] **Step 5: Unify the language/owner/tag filter SQL**
  - **Task**: Make `append_script_filters` the single source of the filter
    predicates. Rewrite `list_scripts` to build SQL dynamically via that helper
    (collapsing the 8-arm match into one path) and have `fts_query_filtered`
    reuse the same predicate text (parameterized for the `s.`-qualified column
    names). Keep generated SQL semantically identical.
  - **Files**:
    - `src/core/search.rs`: rewrite `list_scripts`; export/share the filter
      builder.
    - `src/core/db.rs`: have `fts_query_filtered` call the shared builder.
  - **Step Dependencies**: None.
  - **Success Criteria**: The three filter combinations behave identically;
    `tests/search.rs` (filters, listing, FTS) passes unchanged; the 8-arm match
    is gone.

- [ ] **Step 6: De-duplicate `relative_age` and revision sorting**
  - **Task**: Move `relative_age` into the library (e.g. `core/vc.rs` or a small
    `core/format.rs`) and call it from both `main.rs` and `tui.rs`. Extract the
    shared DEVELOP-first/os/newest-first revision comparator into one function
    used by `main::render_revision_lines` and `tui::sort_checkouts`.
  - **Files**:
    - `src/core/vc.rs` (or `src/core/format.rs`): `relative_age`, revision
      comparator.
    - `src/main.rs`, `src/tui.rs`: delete local copies, call shared.
  - **Step Dependencies**: Step 4 (shared accessors) recommended first.
  - **Success Criteria**: One definition each; `render_revision_lines` and
    `sort_checkouts` tests still pass; identical ordering/age output.

- [ ] **Step 7: Collapse duplicated TUI scroll handling**
  - **Task**: Add `fn scroll_target(&mut self) -> Option<&mut u16>` returning the
    active pane's scroll field, and a helper that maps a key event to a scroll
    delta. Replace the ~7 repeated `match self.focus { … }` blocks in `handle_key`
    and the parallel ladders in `handle_detail_key`/`handle_detail_diff_key`.
  - **Files**:
    - `src/tui.rs`: add accessor + delta helper; rewrite the scroll arms.
  - **Step Dependencies**: None (behavioral parity only).
  - **Success Criteria**: All existing TUI key tests pass; the `unreachable!()`
    scroll arms are removed; net line count drops substantially.

## Structure & Schema

- [ ] **Step 8: Split command handlers out of `main.rs`**
  - **Task**: Move the `cmd_*` handlers and their presentation helpers into a new
    `src/commands/` module tree (e.g. `commands/search.rs`, `commands/show.rs`,
    `commands/catalog.rs`, `commands/diff.rs`), leaving `main.rs` as entry point +
    `run`/dispatch only. Mechanical move; no logic changes.
  - **Files**:
    - New: `src/commands/mod.rs` and per-command files.
    - `src/main.rs`: keep `main`, `run`, `cmd_catalog` dispatch; `mod commands;`.
    - (Adjust `mod` visibility on shared helpers as needed — still ≤20 files.)
  - **Step Dependencies**: Steps 3, 6 (so moved code is already de-duplicated).
  - **Success Criteria**: `main.rs` under ~300 lines; `cargo test` green; no
    behavior change.

- [ ] **Step 9: Split TUI rendering helpers out of `tui.rs`**
  - **Task**: Move the detail/preview line builders and small pure formatters
    (`detail_lines`, `warning_messages`, `json_string_array`, `field_line`,
    `section`, …) into the existing `tui/render.rs` (or a new `tui/detail.rs`),
    leaving `tui.rs` focused on `TuiApp` state + the event loop.
  - **Files**:
    - `src/tui/render.rs` or new `src/tui/detail.rs`: receive the helpers.
    - `src/tui.rs`: remove moved helpers, import them.
  - **Step Dependencies**: Step 7 (do the scroll cleanup first).
  - **Success Criteria**: `tui.rs` materially smaller; all TUI tests pass;
    rendering unchanged.

- [ ] **Step 10: Retire the redundant `vc_checkouts` table**
  - **Task**: Stop writing the derived `vc_checkouts` summary, drop it from the
    DDL, and bump `SCHEMA_VERSION` (9 → 10). The table's only consumer is the
    indexer's `apply_checkout_summaries` (`pipeline.rs:407`), which uses it to
    fill the `scripts.checkout_*` columns — so this step **must** repoint that
    `UPDATE` at the `revisions` table, grouping rows where
    `revision_type = 'DEVELOP'` (the same predicate the client `checkout_status`
    query already uses). Dropping the table without that refactor breaks indexing
    with a "no such table: vc_checkouts" error. Because clients reject mismatched
    schemas and the indexer is CRON-only, this is a clean re-index, not a live
    migration — document it.
  - **Files**:
    - `src/core/db.rs`: remove the `vc_checkouts` DDL + its index; bump
      `SCHEMA_VERSION`.
    - `src/indexer/builder/pipeline.rs`: remove the `vc_checkouts` write path
      **and** rewrite `apply_checkout_summaries` to `GROUP` `revisions`
      (`WHERE revision_type = 'DEVELOP'`) instead of `vc_checkouts`.
    - `CHANGELOG.md`: note the schema bump + "re-index required".
  - **Step Dependencies**: Steps 1, 8 (avoids churn against moved code).
  - **User Instructions**: After deploy, the nightly CRON job must run
    `scat catalog build --force` once to regenerate at schema 10; old DBs will be
    rejected with the existing `SchemaMismatch` guidance.
  - **Success Criteria**: No `vc_checkouts` reference remains in `src/`; a fresh
    `catalog build` produces a schema-10 DB; all revision-backed features
    (`status`, `show`, `stats`, audit, TUI checkouts) still work.

## Lower-Priority Polish

- [ ] **Step 11: Tidy the test-only public API surface**
  - **Task**: For the non-filter convenience wrappers used only by tests
    (`search`, `search_by_path`, `search_by_regex`, `search_scripts_by_function`,
    `get_callers_of`, `db::fts_query`), either (a) update the tests to call the
    `_with_filters` form and delete the wrappers, or (b) keep them but document
    them as thin convenience shims. Prefer (a) to shrink the documented surface.
  - **Files**:
    - `src/core/search.rs`, `src/core/db.rs`: remove/annotate wrappers.
    - `tests/search.rs`, `tests/db.rs`: update call sites.
  - **Step Dependencies**: Step 5 (filter unification) first.
  - **Success Criteria**: `cargo test` passes; public API has no undocumented
    test-only methods; `missing_docs` stays clean.

- [ ] **Step 12: Replace `which_vc` subprocess lookup**
  - **Task**: Resolve the vc executable by scanning `PATH` directly
    (honoring `PATHEXT` on Windows) instead of spawning `which`/`where`. Remove
    the `vc_exe.unwrap()` by restructuring `cmd_vc` to bind the resolved path in
    one place.
  - **Files**:
    - `src/runtime.rs`: rewrite `which_vc`; refactor `cmd_vc` to avoid `unwrap`.
  - **Step Dependencies**: None.
  - **Success Criteria**: `scat vc …` still finds `vc`/`vc.py` on PATH with no
    child `which`/`where` process; a unit test covers PATH resolution; no
    `unwrap` on the executable.

---

## Logical Next Step

After these steps land, the natural follow-on is a **shared "row view" type** in
`scat_core` that replaces the pervasive `JsonRow` (`serde_json::Map`) lookups
with typed accessors for the well-known `scripts` columns. Most helpers in
`output.rs`/`tui.rs`/`search.rs` re-parse the same `logical_path`, `language`,
`tags`, `metadata_json`, and `vc_warnings` fields by hand; a thin typed wrapper
would eliminate the remaining stringly-typed duplication, make the JSON/CSV/table
renderers share one field map, and unlock compile-time checks for the field
names that are currently string literals scattered across the binary. That is a
larger, behavior-preserving refactor best done once the smaller de-duplication
steps above have consolidated the call sites.
