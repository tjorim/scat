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

## Terminal modes and remote sessions

`scat tui --terminal-info` reports resolved modes, effective locale, TERM,
interactive stdin/stdout, SSH and known multiplexer hints without opening a
catalog or changing the terminal. These are environment hints, not capability
probes; no cursor-position or clipboard query is sent. Unknown terminal,
clipboard and forwarding capabilities remain unverified. Detection never uses
the operating system release.

Use `--glyphs auto|unicode|ascii` (`SCAT_TUI_GLYPHS`) and
`--colors auto|full|reduced|mono` (`SCAT_TUI_COLORS`) to override defaults.
Flags override the corresponding environment setting. `--no-color` or nonempty
`NO_COLOR` always selects monochrome, including when `--colors full` is set.
The completed frame is converted consistently across syntax highlighting,
charts, help, palettes, dialogs and status messages. Selection uses reverse
video and focus retains bold borders; warnings/errors also have text labels.
Reduced mode uses the basic eight ANSI colors. Small screens retain the
existing active-pane/tab layout.

Auto glyphs uses the first nonempty value of `LC_ALL`, `LC_CTYPE`, `LANG`;
a locale containing `UTF-8` or `UTF8` enables Unicode. Missing/other locales
select ASCII conservatively. This cannot prove font support; use `--glyphs
ascii` if borders or spinners look broken. ASCII converts borders, arrows,
spinners and charts to single-cell printable characters. Other non-ASCII text
is displayed as `?` (underlying query/catalog/copy text is preserved). Input
continues to use UTF-8 internally; legacy locale input bytes are not transcoded.
Auto color uses full color for `TERM=*256color*` or
`COLORTERM=truecolor|24bit`, otherwise the reduced palette. Explicit Unicode
or full color can override these conservative guesses.

Both stdin and stdout must be terminals; unset `TERM` or `TERM=dumb` is rejected
before fullscreen/raw-mode setup with guidance to use `scat search`. Redirected
output never receives a broken fullscreen UI. `--terminal-info` works with
redirection and dumb terminals.

SSH is inferred from `SSH_CONNECTION`/`SSH_TTY`, tmux from `TMUX`, and
screen-compatible sessions from `STY`/`TERM=screen*`. These do **not** prove
clipboard access. F3 sends a base64 OSC 52 clipboard request; successful writing
only confirms that the request was sent. Over SSH it must reach the **local**
terminal, whose permissions decide whether to accept it. tmux/screen may filter
requests; forwarding depends on their version/configuration. Scat sends the
standard OSC request and does not assume or force multiplexer passthrough.

For reliable native copying, press F2 to freeze redraws and release mouse
capture, select text, then use your local terminal's Copy shortcut (often
Ctrl+Shift+C or Cmd+C). F2/Esc resumes. Native shortcuts vary by terminal;
Ctrl+C inside the application exits. Paste with the terminal's Paste shortcut
(often Ctrl+Shift+V or Cmd+V); bracketed paste inserts normalized text into the
search input rather than executing commands. External viewers suspend raw,
mouse and bracketed-paste modes and restore them on return, including errors.
