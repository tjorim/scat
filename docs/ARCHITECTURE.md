# Architecture & Conventions

> **Scope:** this document records the key design decisions for the Script Catalog (scat).  
> It is intentionally short; please keep it that way.

---

## 1. Logical paths vs OS paths

The indexer runs on a single CRON server against a fixed set of scan roots,
and every client reads the same shared database, so a script's **logical
path** stored in the SQLite database is simply its absolute filesystem path
on the indexing host (e.g. `/shared/tools/security/foo.sh`) — there is no
separate portable-prefix scheme to keep in sync with it.

For the rarer case where a client's local mount doesn't match the indexing
host's layout, `scat`'s client tools accept an optional mapping file
(`--mapping`, see `docs/scat-mapping.example.yaml`) that translates a logical
path's leading segment to that host's real root before opening the file.
Absent a mapping, the logical path *is* the real path.

---

## 2. CRON-only indexing

The Rust indexer (`src/indexer/`) **runs exclusively on the central RHEL CRON server**, nightly.  
No other machine, user process, or tool is permitted to write to the database.

Rationale:
- Tree-sitter grammar compilation is a one-time, server-side step.
- Indexing requires read access to the script directories, which only the CRON server has.
- Keeping a single writer eliminates write conflicts and simplifies operations.

Concretely this means:
- The `index` command is run only by the CRON job.
- Client commands open the database in **read-only mode**.

---

## 3. Read-only database rule

Every client that consumes `scripts.sqlite` **must open it read-only**.

This is not configurable and is not meant to be: there is no config key, CLI
flag, or environment variable that opens a catalog read-write. Read-only is
unconditional for every client.

Open it through `core::db::open_db` (schema-checked) or
`core::db::open_readonly` (bare). Those two are the only places SQLite's own
`SQLITE_OPEN_READ_ONLY` open flag (the `sqlite3_open_v2()` argument, surfaced
by rusqlite as `Connection::open_with_flags`) is named; `open_db` is built on
`open_readonly`, so the flag appears exactly once in the codebase.

That matters more than it looks. Spelled out at each call site, "always
read-only" would be a convention a new read path could silently break —
opening an existing catalog read-write *succeeds*, and only misbehaves later
by creating `-wal`/`-shm` sidecars on the share. Funnelled through one
function, a bare `Connection::open` in a read path is visibly wrong.

Why the flag rather than a `file:…?mode=ro` URI, which is the other way to
say the same thing to SQLite: both enforce identically, but the flag takes a
path instead of a string, so a catalog path containing `?` or `#` can't be
silently misparsed — and those paths come from user config.

Benefits:
- Prevents accidental schema or data mutations from client code.
- Allows the file to be placed on a read-only network share.
- Makes it safe to open the same database from multiple processes simultaneously.

**Rule:** any read-only command that opens the database must go through
`open_db` or `open_readonly` — never `Connection::open`, and never a
hand-written flag.

`catalog build` is not an exception to that rule — the rule simply never
comes up, because **nothing opens the published `scripts.sqlite` read-write,
ever.** The builder:

- reads the previous build **read-only**, to seed an incremental rebuild;
- writes an entirely separate WIP file (§3a), which is the only thing it
  opens read-write;
- replaces the published path with `rename(2)`;
- hard-links (or copies) it for rotation, without opening it as a database.

That is a stronger invariant than "the writer closes the file before clients
use it", and it is what makes the nightly swap safe underneath live readers:
a reader holding the old inode keeps reading it until it closes, and never
observes a half-written database.

---

## 3a. Clients read a host-local copy, not the share

Clients never query the published catalog on the network share directly.
`core::cache` copies it into tmpfs (`/dev/shm`) once per host per rebuild —
via SQLite's Online Backup API, under an `flock` so concurrent starters
produce one copy — and every read-only command opens that copy.

Rationale:
- SQLite's own documentation flags network filesystems as a known source of
  locking bugs; this takes the share out of the query hot path entirely.
- WAL requires an `mmap`-able `-shm` file that network filesystems generally
  don't provide. For the same reason the indexer publishes the catalog in
  **rollback-journal mode** (`journal_mode = DELETE`) — WAL needs that `-shm`
  even for read-only connections, so a WAL-mode catalog would make every
  reader a writer of the directory holding it. WAL is still used *during* the
  build, on the WIP file, and switched off just before the atomic swap.
- The copy is not load-bearing: any failure falls back to reading the
  published catalog in place.

Writers and anything addressing files *beside* the catalog (`catalog build`,
`catalog diff` and its rotated `.1`/`.2` copies) keep using the published
path.

### The WIP build file is on the share too

The atomic swap is a `rename(2)`, which cannot cross filesystems, so the
indexer's work-in-progress database is created beside the published catalog
— on the share — and built there in WAL mode. That is the one remaining
writer on a network filesystem, and it would need the `-shm` that the
filesystem may not support.

`core::db::create_build_db` sets `locking_mode = EXCLUSIVE` **before**
entering WAL, which makes SQLite hold the WAL index in heap memory and never
create a `-shm` at all. The exclusivity is sound precisely where it would not
be for a shared database: the WIP file has one writer, no readers, and ends
up either swapped into place or discarded.

Ordering is the whole trick — the pragma has no effect once a connection has
made its first WAL-mode access, which is why `create_build_db` sets it before
the `journal_mode` switch and `apply_bulk_build_pragmas` sets it before
anything else on a resumed build's reopened connection.

**Rule:** `create_db` stays in normal locking mode. Only the indexer's own
WIP connections are exclusive; a general caller holding a write connection
must still be able to open the same file again.

---

## 3b. Linux-only clients

scat targets Linux. Windows client support was dropped, which lets the
codebase rely on POSIX semantics rather than paper over the differences:

- `rename(2)` is unconditionally atomic and succeeds against files readers
  hold open, so the indexer's swap needs no rename-aside-and-retry fallback.
- Editor/viewer invocation is `shell_words`-parsed, with no special case for
  backslash-bearing unquoted program paths.
- The path-mapping file's `windows:` root is ignored (files carrying one
  still load).
- `/dev/shm` can be assumed present for the local catalog cache.

The build fails fast on non-Unix targets (`src/lib.rs`).

---

## 4. No web UI

scat deliberately has **no central web service**.

Rationale:
- Requires no open firewall ports (critical in air-gapped environments).
- Works fully offline on air-gapped RHEL machines.
- Eliminates an operations burden (no server to maintain, patch, or restart).
- The CLI covers discovery and automation use cases without a browser.

This decision is **final for v1**.  If a web UI is ever reconsidered, it must be
an optional, opt-in sidecar and must never become a runtime dependency of the
core catalog functionality.
