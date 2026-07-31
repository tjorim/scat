# Architecture & Conventions

> **Scope:** this document records the key design decisions for the Script Catalog (scat).  
> It is intentionally short; please keep it that way.

---

## 1. Logical paths vs OS paths

All paths stored in the SQLite database are **logical paths** — repository-relative
POSIX-style paths (e.g. `/catalog/scripts/foo/bar.py`).

Logical paths are:
- Location-neutral (the same database is used from every client host).
- Stable across machine boundaries.
- Human-readable and copy-paste friendly.

**OS-path resolution happens at runtime**, never at index time.  
The Rust core resolves logical paths to local absolute paths through a
configurable root prefix (a mount point on the client).

**Rule:** never store absolute OS paths in the database.

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

Use SQLite URI mode with `mode=ro`.

Benefits:
- Prevents accidental schema or data mutations from client code.
- Allows the file to be placed on a read-only network share.
- Makes it safe to open the same database from multiple processes simultaneously.

**Rule:** any read-only command that opens the database must use `mode=ro`.
The `index` command is the sole exception — it opens the database read-write
during the build step and then closes it before the file is made available to
clients.

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
