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
(`--mapping`, see [the mapping example](scat-mapping.example.yaml)) that translates a logical
path's leading segment to that host's real root before opening the file.
Absent a mapping, the logical path *is* the real path.

---

## 2. Central catalog publication

In the shared-catalog deployment, the Rust indexer (`src/indexer/`) runs on
the central RHEL CRON server nightly. Only the designated publisher should
replace the shared catalog; ordinary clients consume it read-only.

Rationale:
- Tree-sitter grammars are compiled into the binary; parsing runs during indexing.
- The indexing host has read access to all configured script directories.
- Keeping a single writer eliminates write conflicts and simplifies operations.

Concretely this means:
- The CRON job runs `scat catalog build`.
- Authorized administrators can explicitly invoke the same build through the CLI
  or the TUI administration dialog; launching the TUI never starts a build.
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

## 3a. Host-local catalog cache

Clients normally query a host-local copy of the published catalog.
`core::cache` copies it into tmpfs (`/dev/shm`) once per host per rebuild —
via SQLite's Online Backup API, under an `flock` so concurrent starters
produce one copy — and normal browsing commands open that copy.
`--no-cache` deliberately bypasses it.

The cache is bounded and cleans up after itself, because on a shared login
host it costs (users) × (catalog + sidecar) of tmpfs:

- **Never starves the host.** Before copying, the free space of the cache root
  must cover the copy plus headroom (the larger of 64 MiB and a tenth of the
  copy), and the optional `cache_max_bytes` limit must hold after evicting the
  least recently used other entries. Otherwise nothing is copied and the
  published catalog is read in place.
- **Does not hang.** A refresh waits at most ten seconds for another scat's
  copy of the same entry, then reads the published catalog in place.
- **Cleans up.** Entries record their last use (renewed at most hourly, so
  reading is not a write per command) and the published path they cache; a
  refresh drops entries unused for 14 days and abandoned scratch files. The older
  copy kept while the published catalog is in a schema this scat cannot read is
  not served, stays available for a publisher rollback (see COMPATIBILITY.md) and
  ages out like any other unused entry.
- **Is visible.** `scat cache status|prune|clear` list, prune (also entries
  whose published catalog is gone) and empty it; `scat doctor` reports the
  entry count and total size. A copy of 32 MiB or more announces itself on an
  interactive stderr, since it runs before any output.

For a fleet of login hosts that would otherwise each hold a copy per user, a
shared, read-only location populated once by a privileged job (for example a
tmpfs or a read-only bind mount that every client points `--cache-dir` at) is a
supported deployment: the per-user directory and ownership checks apply to
the directory scat itself creates, so a pre-populated location has to be
laid out by that job. It is not automated by scat.

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

**Lifecycle and cleanup.** The WIP database is always `<db>.wip`, built in
place and never renamed, so SQLite's `-wal`/`-journal` sidecars can't be
separated from it. A build holds an exclusive `flock` on `<db>.lock` (a second
build fails fast; the lock file is left in place). On every exit path the
scratch files are removed (`indexer::scratch::ScratchGuard`) except after an
interrupt, which keeps `<db>.wip` plus `<db>.wip.ckpt` for the next run to
resume (re-checking each checkpointed file's size/mtime/content). A dry run
builds in a private `.scat-dryrun-*` directory beside the catalog and deletes
it; it takes no lock and never touches a real build's resume state. At the
start of a real build, `.tmpXXXXXX` leftovers of earlier versions and killed
dry runs older than a day are swept.

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
- `/dev/shm` is the default cache location; unavailable or unusable caches fall
  back to the published catalog.

The build fails fast on non-Unix targets (`src/lib.rs`).

---

## 3c. Compatibility between publisher, clients and caches

Clients read exactly the catalog schema they were built for; a schema change
is a coordinated upgrade. The decision, the supported combinations, cache and
embeddings-sidecar behavior, and the rollout and rollback steps are recorded in
[COMPATIBILITY.md](COMPATIBILITY.md). This section is deliberately only a pointer.

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

The static project website is documentation, not a web UI: an Astro build of the
landing page and these Markdown files, hosted on GitHub Pages with no server
runtime. Its sample-catalog demo is a fictional, browser-only illustration. See
[the website guide](../site/README.md).
