# TUI and CLI option coverage

Launching `scat tui` only opens read-only catalog clients. No indexing, service,
or vc process starts on launch. F4 opens the searchable action palette; F1
lists contextual actions. Read-only browsing needs no write access to the
published catalog (the optional host cache is separate).

Alt+i opens catalog information; Alt+a opens full audit findings. Edit the CLI
arguments and press Enter to run. PageUp/PageDown scroll findings. F3 copies
output; Ctrl+S saves to a new absolute local file. Audit exit 0 means pass,
1 means warnings with `--strict`, and 2 means errors. Findings remain visible
for nonzero exits. Without embeddings, ordinary checks still work; embedding
checks follow the CLI's skipped-check reporting.

Alt+b and Alt+v open explicit administration dialogs. Enter validates arguments
and previews the executable, target paths and argument vector. F5 executes the
exact preview only if the command has not changed. Build initially defaults to
`--dry-run`; remove it deliberately to publish. These actions run the existing
CLI in a child with the terminal restored, showing progress and errors. Ctrl+C
interrupts the child; press Enter after completion to return. Terminal raw mode,
mouse capture and bracketed paste are restored even when execution fails.
There is no privilege escalation. Build uses existing locks, checkpoints,
incremental scanning and atomic publication. Restart the TUI after publication
to refresh its read connections. Existing nightly cron builds remain supported
and are still the recommended shared-catalog deployment.

Arguments use shell-style quoting solely to split words; no shell executes
inputs. For example `vc -- 'path with spaces' '--literal-flag'` preserves each
argument. The preview includes the configured vc executable (or PATH lookup).
Permissions and passthrough behavior are the same as `scat vc`.

## Coverage matrix

All command arguments below are accepted in the indicated CLI-argument editor.
Global overrides inside editors are rejected to retain the active source paths.

| CLI command / options | TUI workflow or intentional boundary |
| --- | --- |
| `search [text] --lang --owner --tag --limit --regex --function --declares --role` | F6 settings and search; F12 requested export supports every option |
| `search --fields --output` | F12: JSON or CSV, with explicit requested limit; displayed scope uses captured rows |
| `show PATH --fields --functions --output` | Details/functions; F12 requested JSON |
| `status [PATH] --all --output` | F8; F12 requested JSON |
| `deps PATH --tree --depth --output` | F9; F12 requested JSON |
| `symlinks PATH --output` | F10; F12 requested JSON |
| `similar PATH --limit --output` | F7; F12 requested JSON with explicit limit; requires embeddings |
| `diff [PATH] --against --revision-type --revision-user --revision-timestamp --old --new --json` | F11 comparison editor |
| `catalog diff --against --old --new --json` | F11 comparison editor |
| `catalog stats --json` | Statistics charts (`s`); JSON through requested workflow |
| `catalog info --json` | Alt+i |
| `catalog audit --check --strict --stale-days --near-duplicate-threshold --outlier-threshold --json` | Alt+a; repeat `--check` to select multiple checks |
| `catalog build --scan-root --head-lines --ignore-file --keep-copies --dry-run --quiet/-q --no-resume --force --no-incremental --threads --json` | Alt+b; repeat scan roots/ignore files; Enter previews, F5 executes |
| `vc [ARGS...] --json` | Alt+v; use `vc -- ARGS...` for literal passthrough arguments |
| `tui --mapping`, `SCAT_MAPPING` | Startup-only logical-to-local path mapping |
| `--db`, `SCAT_DB` | Startup-only catalog source; administration targets this published path, not the host cache |
| `--config`, `SCAT_CONFIG` | Startup-only config, including scan roots, vc executable and bookmarks |
| `--embeddings`, `SCAT_EMBEDDINGS` | Startup-only optional sidecar path |
| `--cache-dir`, `SCAT_CACHE_DIR`, `--no-cache`, `SCAT_NO_CACHE` | Startup-only host cache policy; read-only workflows use CLI cache behavior |
| `--no-color`, `NO_COLOR` | CLI output color setting; read-only captured output disables color for display; TUI has its own styles |
| `-v/--verbose/--debug`, `-vv`, `RUST_LOG` | Startup logging; child operations retain default CLI logging and environment; not editable globals |
| `completions SHELL` | CLI-only stdout utility for generating shell scripts |
| `--help/-h`, `--version/-V` | CLI-only launch information; F1 and the matrix document TUI controls |

Table-form CLI output maps to native TUI views. Structured output is generated
in the workflow editor; build/vc output appears in the suspended terminal.
