# Architecture & Conventions

> **Scope:** this document records the key design decisions for the Script Catalog (scat).  
> It is intentionally short; please keep it that way.

---

## 1. Logical paths vs OS paths

All paths stored in the SQLite database are **logical paths** — repository-relative
POSIX-style paths (e.g. `/catalog/scripts/foo/bar.py`).

Logical paths are:
- Platform-neutral (the same database is used on Windows and RHEL).
- Stable across machine boundaries.
- Human-readable and copy-paste friendly.

**OS-path resolution happens at runtime**, never at index time.  
The Rust core resolves logical paths to local absolute paths based on the
current platform and a configurable root prefix (e.g. a UNC network share on
Windows, a mount point on Linux).

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
