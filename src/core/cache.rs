//! Host-local cache of the shared catalog database.
//!
//! `scat`'s catalog is built nightly on one server and published to a network
//! share that every client mounts. Reading that file directly is what the
//! whole design used to do, and it puts SQLite's locking on a network
//! filesystem: NFS byte-range locks are a documented source of client/server
//! bugs, and WAL mode additionally needs an `mmap`-able `-shm` file, which
//! network filesystems generally do not provide.
//!
//! This module removes the network filesystem from the read hot path. The
//! first `scat` process on a host that notices the published catalog is newer
//! than what is cached copies it into tmpfs (`/dev/shm` by default — genuinely
//! RAM-backed, no in-memory-SQLite plumbing needed) and every `scat` process
//! on that host — the TUI, one-off CLI queries — reads the tmpfs copy instead.
//! The share is touched for one clean copy per host per rebuild rather than
//! for sustained per-query locking.
//!
//! Three properties make that safe to do behind the caller's back:
//!
//! - **The copy is a SQLite Online Backup, not a `fs::copy`.** The published
//!   file could be mid-swap, or (on an older publisher) carry an uncheckpointed
//!   `-wal` sidecar; the backup API reads through SQLite's own consistent
//!   snapshot mechanism, so the copy is a valid database either way.
//! - **Concurrent refreshes are serialised with an exclusive lock** on a
//!   per-entry lock file, so two processes noticing staleness at the same
//!   moment produce one copy, not two interleaved ones.
//! - **Nothing here is load-bearing.** Every failure — no `/dev/shm`, a full
//!   tmpfs, a cache directory owned by somebody else — falls back to reading
//!   the published catalog in place, which is exactly the old behavior.

use std::fs::{self, DirBuilder, File, TryLockError};
use std::io::IsTerminal;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::{debug, warn};

use crate::core::db::{SchemaCompatibility, open_readonly, schema_compatibility};
use crate::error::{Error, Result};

/// Default cache root — tmpfs on every mainstream Linux distribution.
pub const DEFAULT_CACHE_ROOT: &str = "/dev/shm";

/// Cache directories are created (and required to stay) private to their
/// owner: `/dev/shm` itself is world-writable, so a shared host would
/// otherwise let one user hand another a catalog of their choosing.
const CACHE_DIR_MODE: u32 = 0o700;

/// An entry nobody has read for this long is dropped when the cache next
/// refreshes (and by `scat cache prune`).
pub const DEFAULT_MAX_AGE: Duration = Duration::from_hours(14 * 24);

/// How long a refresh waits for another process's copy of the same entry
/// before giving up and reading the published catalog in place instead.
pub const DEFAULT_LOCK_WAIT: Duration = Duration::from_secs(10);

/// Free space kept in the cache root beyond the copy itself: the larger of
/// this and a tenth of the copy, so one catalog cannot take the last of a
/// shared tmpfs from other users.
const MIN_FREE_HEADROOM: u64 = 64 * 1024 * 1024;

/// Copies at least this large announce themselves on an interactive stderr,
/// since the copy runs before any output (and before the TUI starts).
const NOTICE_MIN_BYTES: u64 = 32 * 1024 * 1024;

/// A hit refreshes an entry's last-use time only when it is older than this,
/// so reading the catalog does not become a write per command.
const TOUCH_INTERVAL: Duration = Duration::from_hours(1);

/// Where and whether to cache the published catalog locally.
#[derive(Debug, Clone)]
pub struct CacheOptions {
    /// When false, callers read the published catalog in place.
    pub enabled: bool,
    /// Cache root; defaults to [`DEFAULT_CACHE_ROOT`] when `None`.
    pub root: Option<PathBuf>,
    /// Upper bound on the bytes this user's cache may hold. A copy that would
    /// exceed it first evicts the least recently used other entries; if it
    /// still does not fit, the published catalog is read in place. `None`
    /// bounds nothing but the free space of the cache root.
    pub max_bytes: Option<u64>,
    /// How long to wait for a refresh already running in another process.
    pub lock_wait: Duration,
}

impl Default for CacheOptions {
    fn default() -> Self {
        Self {
            enabled: true,
            root: None,
            max_bytes: None,
            lock_wait: DEFAULT_LOCK_WAIT,
        }
    }
}

/// Return the path every read-only consumer should open for `source`.
///
/// This is either a host-local, up-to-date copy of `source` or — whenever
/// caching is disabled, impossible, or fails for any reason — `source`
/// itself. It never returns an error: a broken cache degrades to the
/// uncached read path rather than failing the command the user asked for.
///
/// Not specific to the catalog despite the name: any read-only SQLite file
/// published to the same kind of shared drive can go through this — for
/// example the `embeddings.sqlite` sidecar `crates/scat-embed` publishes
/// beside the catalog, cached under its own entry (keyed by its own path,
/// same as any other `source`). Only the catalog has an
/// `index_metadata.build_timestamp` to short-circuit an identity change that
/// still carries the same build; a file without one still caches correctly,
/// it just always recopies on any identity change instead of sometimes
/// re-stamping the existing copy in place.
///
/// Callers that *write* the catalog (the indexer) or that address files
/// alongside it (`catalog diff`, which reads the rotated `.1`/`.2` copies)
/// must keep using the published path instead.
#[must_use]
pub fn catalog_read_path(source: &Path, options: &CacheOptions) -> PathBuf {
    if !options.enabled {
        debug!("local catalog cache disabled; reading the published catalog in place");
        return source.to_path_buf();
    }

    match refresh(source, options) {
        Ok(Some(cached)) => {
            debug!(
                source = %source.display(),
                cached = %cached.display(),
                "reading catalog from local cache"
            );
            cached
        }
        Ok(None) => source.to_path_buf(),
        Err(err) => {
            warn!(
                source = %source.display(),
                error = %err,
                "local catalog cache unavailable; reading the published catalog in place"
            );
            source.to_path_buf()
        }
    }
}

/// Inspect the existing cache without creating directories, locks or refreshing it.
/// Filesystem access must be isolated by callers when network mounts may stall.
pub fn inspect_cache(source: &Path, options: &CacheOptions) -> serde_json::Value {
    let root = options
        .root
        .as_deref()
        .unwrap_or(Path::new(DEFAULT_CACHE_ROOT));
    let uid = current_uid();
    let dir = root.join(format!("scat-cache-{uid}"));
    let entry = CacheEntry::new(&dir, &cache_key(source));
    let dir_meta = fs::symlink_metadata(&dir);
    let missing = dir_meta
        .as_ref()
        .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound);
    // Same rule as `cache_dir`: owned by us with no group/other access.
    let trusted = dir_meta
        .as_ref()
        .is_ok_and(|m| m.is_dir() && m.uid() == uid && m.mode() & 0o077 == 0);
    let stamp = SourceStamp::of(source);
    let current = stamp.as_ref().is_some_and(|stamp| entry.is_current(stamp));
    let state = if !options.enabled {
        "disabled"
    } else if missing {
        "absent"
    } else if !trusted {
        "unavailable"
    } else if !entry.db.is_file() {
        "absent"
    } else if stamp.is_none() {
        "unknown"
    } else if current {
        "current"
    } else {
        "stale"
    };
    let (entries, total_bytes) = if trusted {
        let entries = list_entries(&dir);
        let total = entries.iter().map(|e| e.bytes).sum::<u64>();
        (entries.len(), total)
    } else {
        (0, 0)
    };
    serde_json::json!({
        "state": state, "root": root, "directory": dir, "path": entry.db,
        "entries": entries, "total_bytes": total_bytes,
        "active_read_path": if options.enabled && trusted && current { &entry.db } else { source },
        "fallback": !(options.enabled && trusted && current),
        "note": "Read path predicted without refreshing; normal reads may refresh a stale cache. Unavailable source never activates a stale copy."
    })
}

/// Cache a fresh copy of `source` if needed and return its path, or `None`
/// when this host cannot cache at all (no usable cache root, or `source`
/// isn't a readable regular file).
fn refresh(source: &Path, options: &CacheOptions) -> Result<Option<PathBuf>> {
    let Some(stamp) = SourceStamp::of(source) else {
        // Missing, unreadable, or not a regular file. Returning `None` keeps
        // the caller pointed at the published path so its own error message
        // names the catalog the user configured, not a cache entry.
        return Ok(None);
    };

    let Some(dir) = cache_dir(options)? else {
        return Ok(None);
    };
    let entry = CacheEntry::new(&dir, &cache_key(source));

    // Fast path: one `stat` of the network file plus one small local read.
    // No SQLite connection to the share at all.
    if entry.is_current(&stamp) {
        entry.touch_if_old();
        return Ok(Some(entry.db));
    }

    let lock = File::options()
        .create(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .open(&entry.lock)?;
    // Bounded: a copy stuck on a hung mount must not hang every later scat.
    if !lock_within(&lock, options.lock_wait)? {
        warn!(
            source = %source.display(),
            waited = ?options.lock_wait,
            "another scat is still copying the catalog to the local cache; reading the published catalog in place"
        );
        return Ok(None);
    }
    // Another process may have refreshed the entry while we waited on the
    // lock; re-check before paying for a second copy of the same build.
    if entry.is_current(&stamp) {
        entry.touch_if_old();
        return Ok(Some(entry.db));
    }

    let populated = entry.populate(source, &stamp, options)?;
    // Housekeeping while the lock is held. When the source was refused, this
    // very entry is unused too, so it is not exempt from the age limit.
    prune_old_entries(
        &dir,
        if populated { &entry.key } else { "" },
        DEFAULT_MAX_AGE,
    );
    Ok(populated.then_some(entry.db))
}

/// Take the exclusive lock, retrying until `wait` has elapsed. `Ok(false)`
/// when someone else still holds it.
fn lock_within(lock: &File, wait: Duration) -> std::io::Result<bool> {
    let start = std::time::Instant::now();
    loop {
        match lock.try_lock() {
            Ok(()) => return Ok(true),
            Err(TryLockError::WouldBlock) => {
                if start.elapsed() >= wait {
                    return Ok(false);
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(TryLockError::Error(err)) => return Err(err),
        }
    }
}

// ---------------------------------------------------------------------------
// Cache entries
// ---------------------------------------------------------------------------

/// Marks a file as scratch: written under the entry's lock and renamed into
/// its final name, so anything still carrying it is abandoned.
const SCRATCH_INFIX: &str = ".tmp.";

/// The trio of files making up one cached catalog.
struct CacheEntry {
    /// Directory holding this entry's files.
    dir: PathBuf,
    /// Digest of the published path this entry caches.
    key: String,
    /// The cached database itself.
    db: PathBuf,
    /// Sidecar recording which published build `db` holds.
    meta: PathBuf,
    /// Lock file serialising refreshes of this entry.
    lock: PathBuf,
}

impl CacheEntry {
    fn new(dir: &Path, key: &str) -> Self {
        Self {
            dir: dir.to_path_buf(),
            key: key.to_string(),
            db: dir.join(format!("{key}.sqlite")),
            meta: dir.join(format!("{key}.json")),
            lock: dir.join(format!("{key}.lock")),
        }
    }

    /// A private scratch path this process renames into `<key>.<kind>`.
    ///
    /// The pid keeps two processes that somehow both reach this point (a
    /// stale lock file replaced underneath one of them, say) off each
    /// other's partial writes.
    fn scratch(&self, kind: &str) -> PathBuf {
        self.dir.join(format!(
            "{}.{kind}{SCRATCH_INFIX}{}",
            self.key,
            std::process::id()
        ))
    }

    /// True when the cached copy was made from exactly the file `stamp`
    /// describes.
    fn is_current(&self, stamp: &SourceStamp) -> bool {
        self.db.is_file() && read_meta(&self.meta).is_some_and(|meta| meta.source == *stamp)
    }

    /// Copy `source` into the cache and record `stamp` as what it holds.
    ///
    /// Returns `false`, copying nothing, when `source` is a catalog this scat
    /// cannot read (see [`schema_compatibility`]): the caller then reads the
    /// published file in place and reports the mismatch from there. Caching it
    /// would only park an unopenable catalog in RAM — and a stale entry for an
    /// older, readable build must not be served in its place either.
    fn populate(&self, source: &Path, stamp: &SourceStamp, options: &CacheOptions) -> Result<bool> {
        let src = open_readonly(source)?;
        let schema = schema_version(&src);
        if schema.is_some_and(|v| schema_compatibility(v) != SchemaCompatibility::Compatible) {
            debug!(
                source = %source.display(),
                schema_version = ?schema,
                "published catalog schema is not readable by this scat; not caching it"
            );
            // The older entry we may hold stays: a rollback of the publisher
            // restores that very build and reuses it instead of recopying (see
            // docs/COMPATIBILITY.md). Nothing reads it meanwhile, so the age
            // pruning in `refresh` reclaims it if the upgrade sticks.
            return Ok(false);
        }
        // Only the catalog carries `index_metadata`; other SQLite files this
        // cache is used for (e.g. the scat-embed embeddings sidecar, which
        // has no such table) have no build marker to read, so the shortcut
        // below is simply unavailable for them rather than a hard error —
        // `cached_build_timestamp` reads `None` the same way for the same
        // reason, so it never falsely matches this empty placeholder and a
        // real recopy still happens whenever the file's identity changes.
        let build_timestamp = build_timestamp(&src).unwrap_or_default();

        // The published file changed identity (a rebuild, a rotation, a
        // restore from backup) but still carries the build we already hold.
        // Re-stamp the existing copy instead of pulling every page across the
        // network for a catalog we have byte-for-byte. The schema version is
        // part of "the build": a timestamp alone must never pair a copy of
        // one format with a published file of another.
        if self.db.is_file()
            && cached_build_timestamp(&self.db).as_deref() == Some(&*build_timestamp)
            && cached_schema_version(&self.db) == schema
        {
            self.write_meta(stamp, build_timestamp, source)?;
            return Ok(true);
        }

        // A copy killed between creating its scratch file and renaming it
        // leaves a full database's worth of RAM behind. We hold the lock, so
        // no other refresh of this entry is in flight and every leftover is
        // by definition abandoned.
        self.sweep_scratch_files();

        // Make room, or decline: a copy that does not fit must not eat the
        // rest of a shared tmpfs (the published catalog is read in place).
        if !self.make_room(stamp.len, options) {
            return Ok(false);
        }
        if stamp.len >= NOTICE_MIN_BYTES && std::io::stderr().is_terminal() {
            eprintln!(
                "scat: copying the catalog ({} MiB) to the local cache; this happens once per rebuild…",
                stamp.len / (1024 * 1024)
            );
        }

        let tmp = self.scratch("sqlite");
        if let Err(err) = copy_catalog(&src, &tmp) {
            // A full tmpfs is the expected failure here; leave nothing behind
            // to hold RAM after we fall back to the published catalog.
            let _ = fs::remove_file(&tmp);
            return Err(err);
        }
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;

        // `rename(2)` is atomic even against readers holding the previous
        // copy open: they keep reading the old inode until they close it,
        // and every process that opens the entry afterwards sees the new
        // build. The metadata sidecar is written second on purpose — a crash
        // between the two leaves an entry that simply looks stale and gets
        // rebuilt, never one that looks fresh while holding the wrong build.
        fs::rename(&tmp, &self.db)?;
        self.write_meta(stamp, build_timestamp, source)?;
        Ok(true)
    }

    /// Whether a copy of `needed` bytes fits: within `max_bytes` (evicting the
    /// least recently used *other* entries to get there) and with headroom to
    /// spare in the cache root's filesystem.
    fn make_room(&self, needed: u64, options: &CacheOptions) -> bool {
        if let Some(max) = options.max_bytes {
            let mut entries: Vec<_> = list_entries(&self.dir)
                .into_iter()
                .filter(|e| e.key != self.key)
                .collect();
            let mut held: u64 = entries.iter().map(|e| e.bytes).sum();
            // Oldest use first.
            entries.sort_by_key(|e| e.last_used);
            for victim in &entries {
                if held + needed <= max {
                    break;
                }
                debug!(key = %victim.key, bytes = victim.bytes, "evicting the least recently used cache entry");
                Self::new(&self.dir, &victim.key).remove_files();
                held = held.saturating_sub(victim.bytes);
            }
            if held + needed > max {
                warn!(
                    needed,
                    max,
                    "the catalog does not fit in the local cache limit; reading the published catalog in place"
                );
                return false;
            }
        }
        if let Some(free) = free_bytes(&self.dir) {
            let headroom = MIN_FREE_HEADROOM.max(needed / 10);
            if free < needed.saturating_add(headroom) {
                warn!(
                    free,
                    needed,
                    "not enough free space in the cache root for the catalog; reading the published catalog in place"
                );
                return false;
            }
        }
        true
    }

    /// Remove this entry's database and metadata (the lock file stays: it is
    /// what serialises the next refresh).
    fn remove_files(&self) {
        let _ = fs::remove_file(&self.db);
        let _ = fs::remove_file(&self.meta);
    }

    /// Mark the entry as used now, if its last use is more than
    /// [`TOUCH_INTERVAL`] ago. Best effort: pruning merely sees an older time.
    fn touch_if_old(&self) {
        let stale = fs::metadata(&self.meta)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > TOUCH_INTERVAL);
        if stale && let Ok(file) = File::options().write(true).open(&self.meta) {
            let _ = file.set_modified(SystemTime::now());
        }
    }

    /// Record what the cached copy now holds, atomically.
    fn write_meta(
        &self,
        stamp: &SourceStamp,
        build_timestamp: String,
        source: &Path,
    ) -> Result<()> {
        let meta = CacheMeta {
            source: stamp.clone(),
            build_timestamp,
            source_path: Some(source.to_string_lossy().into_owned()),
        };
        let tmp = self.scratch("json");
        fs::write(&tmp, serde_json::to_vec(&meta)?)?;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600))?;
        fs::rename(&tmp, &self.meta)?;
        Ok(())
    }

    /// Delete scratch files abandoned by an interrupted refresh of this entry.
    fn sweep_scratch_files(&self) {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with(&self.key) && name.contains(SCRATCH_INFIX) {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
}

/// Back up the catalog behind `src` into a fresh database at `dst_path`.
fn copy_catalog(src: &Connection, dst_path: &Path) -> Result<()> {
    let mut dst = Connection::open(dst_path)?;
    {
        let backup = rusqlite::backup::Backup::new(src, &mut dst)?;
        backup.step(-1)?;
    }
    // Rollback-journal mode, deliberately: a WAL database needs a writable,
    // `mmap`-able `-shm` file next to it *even for read-only connections*, so
    // a WAL-mode cache entry would make every reader a writer of the cache
    // directory. In DELETE mode a reader opens the single file and nothing
    // else, and the entry leaves no sidecars occupying tmpfs.
    let _: String = dst.query_row("PRAGMA journal_mode = DELETE", [], |row| row.get(0))?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Freshness metadata
// ---------------------------------------------------------------------------

/// Identity of the published catalog file, as seen by `stat`.
///
/// Compared as a whole: `len` and `mtime` catch an in-place rewrite, `ino`
/// and `dev` catch the indexer's atomic swap (which replaces the file with a
/// brand-new inode, possibly at the same size and timestamp granularity).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SourceStamp {
    len: u64,
    mtime_secs: i64,
    mtime_nanos: i64,
    ino: u64,
    dev: u64,
}

impl SourceStamp {
    fn of(source: &Path) -> Option<Self> {
        let meta = fs::metadata(source).ok()?;
        if !meta.is_file() {
            return None;
        }
        Some(Self {
            len: meta.len(),
            mtime_secs: meta.mtime(),
            mtime_nanos: meta.mtime_nsec(),
            ino: meta.ino(),
            dev: meta.dev(),
        })
    }
}

/// Sidecar contents describing what a cached copy holds.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheMeta {
    source: SourceStamp,
    /// `index_metadata.build_timestamp` of the cached copy — the catalog's
    /// own notion of "which build is this", independent of file identity.
    build_timestamp: String,
    /// The published path this entry caches, for listing and pruning. Absent
    /// in entries written by earlier versions.
    #[serde(default)]
    source_path: Option<String>,
}

fn read_meta(path: &Path) -> Option<CacheMeta> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

fn build_timestamp(conn: &Connection) -> Result<String> {
    conn.query_row(
        "SELECT build_timestamp FROM index_metadata WHERE id = 1",
        [],
        |row| row.get(0),
    )
    .map_err(|e| {
        Error::Validation(format!(
            "catalog has no index_metadata ({e}) — re-index required (scat catalog build)"
        ))
    })
}

fn cached_build_timestamp(path: &Path) -> Option<String> {
    let conn = open_readonly(path).ok()?;
    build_timestamp(&conn).ok()
}

/// `index_metadata.schema_version`, or `None` for a file without one (the
/// embeddings sidecar, or a catalog that has no metadata row yet).
fn schema_version(conn: &Connection) -> Option<i64> {
    conn.query_row(
        "SELECT schema_version FROM index_metadata WHERE id = 1",
        [],
        |row| row.get(0),
    )
    .ok()
}

fn cached_schema_version(path: &Path) -> Option<i64> {
    schema_version(&open_readonly(path).ok()?)
}

// ---------------------------------------------------------------------------
// Inventory, eviction and the `scat cache` commands
// ---------------------------------------------------------------------------

/// One cached file, as the inventory sees it.
#[derive(Debug, Clone)]
pub struct EntryInfo {
    /// The entry's name: a digest of the published path it caches.
    pub key: String,
    /// The published path it caches (unknown for entries made by an older scat).
    pub source: Option<PathBuf>,
    /// Bytes of tmpfs the cached database takes.
    pub bytes: u64,
    /// When it was last read (the metadata sidecar's mtime; see [`TOUCH_INTERVAL`]).
    pub last_used: Option<SystemTime>,
    /// `index_metadata.build_timestamp` of the cached build, when it is a catalog.
    pub build_timestamp: Option<String>,
}

/// What `scat cache status` reports.
#[derive(Debug, Clone)]
pub struct CacheStatus {
    /// This user's cache directory, or `None` when it does not exist (or is
    /// not trustworthy, see [`cache_dir`]).
    pub directory: Option<PathBuf>,
    /// Every entry in it.
    pub entries: Vec<EntryInfo>,
    /// Sum of the entries' bytes.
    pub total_bytes: u64,
}

/// What a clear or prune removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Removed {
    /// Entries removed.
    pub entries: usize,
    /// Bytes of tmpfs freed.
    pub bytes: u64,
}

fn is_entry_key(name: &str) -> bool {
    name.len() == 16
        && name
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Every entry in `dir`, by key.
fn list_entries(dir: &Path) -> Vec<EntryInfo> {
    let Ok(read) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut keys = std::collections::BTreeSet::new();
    for entry in read.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if let Some(key) = name
            .strip_suffix(".sqlite")
            .or_else(|| name.strip_suffix(".json"))
            && is_entry_key(key)
        {
            keys.insert(key.to_string());
        }
    }
    keys.into_iter()
        .map(|key| {
            let entry = CacheEntry::new(dir, &key);
            let meta = read_meta(&entry.meta);
            let db_meta = fs::metadata(&entry.db).ok();
            EntryInfo {
                bytes: db_meta.as_ref().map_or(0, fs::Metadata::len),
                last_used: fs::metadata(&entry.meta)
                    .and_then(|m| m.modified())
                    .ok()
                    .or_else(|| db_meta.and_then(|m| m.modified().ok())),
                source: meta
                    .as_ref()
                    .and_then(|m| m.source_path.as_deref())
                    .map(PathBuf::from),
                build_timestamp: meta.map(|m| m.build_timestamp).filter(|t| !t.is_empty()),
                key,
            }
        })
        .collect()
}

/// Remove an entry's files, lock file included. Safe while readers have the
/// database open: they keep their inode until they close it.
fn remove_entry(dir: &Path, info: &EntryInfo) {
    let entry = CacheEntry::new(dir, &info.key);
    entry.remove_files();
    let _ = fs::remove_file(&entry.lock);
}

/// Drop entries other than `keep_key` that were last read more than `max_age`
/// ago, and scratch files abandoned by a killed refresh of any entry. Runs
/// after a refresh, so a long-lived host does not accumulate copies of
/// catalogs that moved or were retired. Age only: a "does the source still
/// exist" check would `stat` a network path and could hang here.
fn prune_old_entries(dir: &Path, keep_key: &str, max_age: Duration) {
    let now = SystemTime::now();
    let older = |time: Option<SystemTime>| {
        time.and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age > max_age)
    };
    for info in list_entries(dir) {
        if info.key != keep_key && older(info.last_used) {
            debug!(key = %info.key, source = ?info.source, "pruning unused cache entry");
            remove_entry(dir, &info);
        }
    }
    if let Ok(read) = fs::read_dir(dir) {
        for file in read.flatten() {
            let name = file.file_name();
            if name.to_string_lossy().contains(SCRATCH_INFIX)
                && older(file.metadata().and_then(|m| m.modified()).ok())
            {
                let _ = fs::remove_file(file.path());
            }
        }
    }
}

/// Free bytes (for unprivileged users) on the filesystem holding `dir`.
fn free_bytes(dir: &Path) -> Option<u64> {
    let path = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
    // SAFETY: `statvfs` is plain data, valid when zeroed; the call only writes it.
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: `path` is a valid NUL-terminated string and `stat` a valid out pointer.
    if unsafe { libc::statvfs(path.as_ptr(), &raw mut stat) } != 0 {
        return None;
    }
    Some(stat.f_bavail.saturating_mul(stat.f_frsize))
}

/// This user's cache directory if it exists and is trustworthy, without
/// creating anything (the read-only twin of [`cache_dir`]).
fn existing_cache_dir(options: &CacheOptions) -> Option<PathBuf> {
    let root = options
        .root
        .as_deref()
        .unwrap_or(Path::new(DEFAULT_CACHE_ROOT));
    let uid = current_uid();
    let dir = root.join(format!("scat-cache-{uid}"));
    let meta = fs::symlink_metadata(&dir).ok()?;
    (meta.is_dir() && meta.uid() == uid && meta.mode() & 0o077 == 0).then_some(dir)
}

/// List what this user's cache holds. Read-only.
pub fn cache_status(options: &CacheOptions) -> CacheStatus {
    let directory = existing_cache_dir(options);
    let entries = directory.as_deref().map(list_entries).unwrap_or_default();
    CacheStatus {
        total_bytes: entries.iter().map(|e| e.bytes).sum(),
        directory,
        entries,
    }
}

/// Remove every entry of this user's cache.
pub fn cache_clear(options: &CacheOptions) -> Removed {
    let Some(dir) = existing_cache_dir(options) else {
        return Removed::default();
    };
    let mut removed = Removed::default();
    for info in list_entries(&dir) {
        remove_entry(&dir, &info);
        removed.entries += 1;
        removed.bytes += info.bytes;
    }
    // Abandoned scratch files of killed refreshes go with them.
    if let Ok(read) = fs::read_dir(&dir) {
        for file in read.flatten() {
            if file.file_name().to_string_lossy().contains(SCRATCH_INFIX) {
                removed.bytes += file.metadata().map_or(0, |m| m.len());
                let _ = fs::remove_file(file.path());
            }
        }
    }
    removed
}

/// Remove entries not read for `max_age`, and — with `check_sources` — those
/// whose published catalog no longer exists. The source check `stat`s the
/// published path, which can block on a hung mount, so only an explicit
/// `scat cache prune` does it.
pub fn cache_prune(options: &CacheOptions, max_age: Duration, check_sources: bool) -> Removed {
    let Some(dir) = existing_cache_dir(options) else {
        return Removed::default();
    };
    let now = SystemTime::now();
    let mut removed = Removed::default();
    for info in list_entries(&dir) {
        let unused = info
            .last_used
            .and_then(|t| now.duration_since(t).ok())
            .is_some_and(|age| age > max_age);
        let orphaned = check_sources && info.source.as_ref().is_some_and(|p| !p.exists());
        if unused || orphaned {
            remove_entry(&dir, &info);
            removed.entries += 1;
            removed.bytes += info.bytes;
        }
    }
    prune_old_entries(&dir, "", max_age);
    removed
}

// ---------------------------------------------------------------------------
// Cache location
// ---------------------------------------------------------------------------

/// Resolve (and create) this user's cache directory under the configured
/// root, or `None` when the root is unusable.
fn cache_dir(options: &CacheOptions) -> Result<Option<PathBuf>> {
    let root = options
        .root
        .clone()
        .unwrap_or_else(|| PathBuf::from(DEFAULT_CACHE_ROOT));
    if !root.is_dir() {
        debug!(root = %root.display(), "cache root is not a directory; caching disabled");
        return Ok(None);
    }

    // Per-uid so that hosts shared between users cache once per user rather
    // than fighting over one directory nobody else may enter.
    let uid = current_uid();
    let dir = root.join(format!("scat-cache-{uid}"));
    DirBuilder::new()
        .recursive(true)
        .mode(CACHE_DIR_MODE)
        .create(&dir)?;

    // `DirBuilder::mode` only applies to directories this call created, and
    // `/dev/shm` is world-writable, so an existing entry has to be vetted:
    // anything we don't own, that isn't a real directory, or that others can
    // write to could be feeding us someone else's catalog.
    let meta = fs::symlink_metadata(&dir)?;
    if !meta.is_dir() {
        warn!(dir = %dir.display(), "cache directory is not a directory; caching disabled");
        return Ok(None);
    }
    if meta.uid() != uid {
        warn!(
            dir = %dir.display(),
            owner = meta.uid(),
            "cache directory belongs to another user; caching disabled"
        );
        return Ok(None);
    }
    if meta.mode() & 0o077 != 0 {
        warn!(
            dir = %dir.display(),
            mode = format!("{:o}", meta.mode() & 0o777),
            "cache directory is accessible to other users; caching disabled"
        );
        return Ok(None);
    }

    Ok(Some(dir))
}

/// The calling process's effective uid: the one file-ownership checks (and new
/// files) use, and available without `/proc`.
fn current_uid() -> u32 {
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

/// A stable, filesystem-safe name for `source`.
///
/// Canonicalised first so that two spellings of the same published catalog
/// (a symlinked mount point, a relative path) share one cache entry instead
/// of each copying the same database into RAM.
fn cache_key(source: &Path) -> String {
    let canonical = fs::canonicalize(source).unwrap_or_else(|_| source.to_path_buf());
    let digest = Sha256::digest(canonical.as_os_str().as_bytes());
    hex::encode(digest)[..16].to_string()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::db::create_db;
    use tempfile::TempDir;

    #[test]
    fn inspection_is_read_only_and_distinguishes_cache_states() {
        let dir = TempDir::new().unwrap();
        let source = dir.path().join("source.sqlite");
        publish(&source, "2026-01-01T00:00:00Z");
        let root = dir.path().join("cache");
        fs::create_dir(&root).unwrap();
        let options = CacheOptions {
            enabled: true,
            root: Some(root.clone()),
            ..CacheOptions::default()
        };
        assert_eq!(inspect_cache(&source, &options)["state"], "absent");
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
        let cached = catalog_read_path(&source, &options);
        assert_ne!(cached, source);
        assert_eq!(inspect_cache(&source, &options)["state"], "current");
        // Group/other-accessible cache directories are untrusted.
        let cache_dir = cached.parent().unwrap();
        fs::set_permissions(cache_dir, fs::Permissions::from_mode(0o750)).unwrap();
        assert_eq!(inspect_cache(&source, &options)["state"], "unavailable");
        fs::set_permissions(cache_dir, fs::Permissions::from_mode(0o700)).unwrap();
        let conn = open_readonly(&source).unwrap();
        drop(conn);
        // Change identity without depending on timestamp resolution.
        let old = dir.path().join("old.sqlite");
        fs::rename(&source, old).unwrap();
        publish(&source, "2026-01-02T00:00:00Z");
        let stale = inspect_cache(&source, &options);
        assert_eq!(stale["state"], "stale");
        assert_eq!(stale["active_read_path"], source.to_str().unwrap());
        fs::remove_file(&cached).unwrap();
        assert_eq!(inspect_cache(&source, &options)["state"], "absent");
        assert_eq!(
            inspect_cache(
                &source,
                &CacheOptions {
                    enabled: false,
                    ..options
                }
            )["state"],
            "disabled"
        );
    }

    /// Build a published catalog with the given `build_timestamp`.
    fn publish(path: &Path, build_timestamp: &str) {
        let conn = create_db(path).unwrap();
        conn.execute(
            "INSERT INTO scripts (logical_path, language, content)
             VALUES ('/catalog/scripts/foo.py', 'python', 'print(1)')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT OR REPLACE INTO index_metadata (id, build_timestamp, schema_version)
             VALUES (1, ?, ?)",
            rusqlite::params![build_timestamp, crate::core::db::SCHEMA_VERSION],
        )
        .unwrap();
        drop(conn);
    }

    /// Cache into a temp directory rather than the host's real `/dev/shm`.
    fn options(root: &TempDir) -> CacheOptions {
        CacheOptions {
            enabled: true,
            root: Some(root.path().to_path_buf()),
            ..CacheOptions::default()
        }
    }

    fn ino_of(path: &Path) -> u64 {
        fs::metadata(path).unwrap().ino()
    }

    #[test]
    fn disabled_cache_reads_the_published_catalog_in_place() {
        let dir = TempDir::new().unwrap();
        let source = dir.path().join("scripts.sqlite");
        publish(&source, "2026-07-30T02:00:00");

        let options = CacheOptions {
            enabled: false,
            root: Some(dir.path().to_path_buf()),
            ..CacheOptions::default()
        };
        assert_eq!(catalog_read_path(&source, &options), source);
    }

    #[test]
    fn missing_cache_root_falls_back_to_the_published_catalog() {
        let dir = TempDir::new().unwrap();
        let source = dir.path().join("scripts.sqlite");
        publish(&source, "2026-07-30T02:00:00");

        let options = CacheOptions {
            enabled: true,
            root: Some(dir.path().join("no-such-tmpfs")),
            ..CacheOptions::default()
        };
        assert_eq!(catalog_read_path(&source, &options), source);
    }

    #[test]
    fn missing_catalog_falls_back_to_the_published_path() {
        let dir = TempDir::new().unwrap();
        let root = TempDir::new().unwrap();
        let source = dir.path().join("scripts.sqlite");

        // No file to cache: the caller must still see the path it configured
        // so its own "database not found" names the real catalog.
        assert_eq!(catalog_read_path(&source, &options(&root)), source);
    }

    #[test]
    fn caches_the_catalog_and_serves_a_readable_copy() {
        let dir = TempDir::new().unwrap();
        let root = TempDir::new().unwrap();
        let source = dir.path().join("scripts.sqlite");
        publish(&source, "2026-07-30T02:00:00");

        let cached = catalog_read_path(&source, &options(&root));
        assert_ne!(cached, source);
        assert!(cached.starts_with(root.path()));

        // The copy is a real, openable catalog, not just bytes on disk.
        let conn = crate::core::db::open_db(&cached).unwrap();
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM scripts", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 1);
    }

    #[test]
    fn cached_copy_is_in_rollback_journal_mode() {
        let dir = TempDir::new().unwrap();
        let root = TempDir::new().unwrap();
        let source = dir.path().join("scripts.sqlite");
        publish(&source, "2026-07-30T02:00:00");

        let cached = catalog_read_path(&source, &options(&root));

        // WAL would require every read-only reader to create a `-shm`
        // alongside the entry; DELETE keeps readers strictly read-only.
        let conn = open_readonly(&cached).unwrap();
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode.to_lowercase(), "delete");
        assert!(!cached.with_extension("sqlite-wal").exists());
        assert!(!cached.with_extension("sqlite-shm").exists());
    }

    #[test]
    fn unchanged_catalog_is_not_recopied() {
        let dir = TempDir::new().unwrap();
        let root = TempDir::new().unwrap();
        let source = dir.path().join("scripts.sqlite");
        publish(&source, "2026-07-30T02:00:00");

        let first = catalog_read_path(&source, &options(&root));
        let first_ino = ino_of(&first);
        let second = catalog_read_path(&source, &options(&root));

        assert_eq!(first, second);
        // A recopy would have renamed a brand-new inode into place.
        assert_eq!(ino_of(&second), first_ino);
    }

    #[test]
    fn rebuilt_catalog_is_recopied() {
        let dir = TempDir::new().unwrap();
        let root = TempDir::new().unwrap();
        let source = dir.path().join("scripts.sqlite");
        publish(&source, "2026-07-30T02:00:00");

        let cached = catalog_read_path(&source, &options(&root));
        let before = ino_of(&cached);

        // Republish as the indexer does: build elsewhere, swap into place.
        let rebuilt = dir.path().join("rebuilt.sqlite");
        publish(&rebuilt, "2026-07-31T02:00:00");
        fs::rename(&rebuilt, &source).unwrap();

        let refreshed = catalog_read_path(&source, &options(&root));
        assert_eq!(refreshed, cached);
        assert_ne!(ino_of(&refreshed), before);

        let conn = open_readonly(&refreshed).unwrap();
        let stamp: String = conn
            .query_row(
                "SELECT build_timestamp FROM index_metadata WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stamp, "2026-07-31T02:00:00");
    }

    #[test]
    fn republished_identical_build_reuses_the_existing_copy() {
        let dir = TempDir::new().unwrap();
        let root = TempDir::new().unwrap();
        let source = dir.path().join("scripts.sqlite");
        publish(&source, "2026-07-30T02:00:00");

        let cached = catalog_read_path(&source, &options(&root));
        let before = ino_of(&cached);

        // Same build_timestamp, different inode — e.g. a restore from one of
        // the rotated copies. Nothing to re-copy across the network.
        let republished = dir.path().join("republished.sqlite");
        publish(&republished, "2026-07-30T02:00:00");
        fs::rename(&republished, &source).unwrap();

        let refreshed = catalog_read_path(&source, &options(&root));
        assert_eq!(ino_of(&refreshed), before);
        // …but the entry is re-stamped, so the next call takes the fast path.
        let stamp = SourceStamp::of(&source).unwrap();
        let entry = CacheEntry::new(
            &root.path().join(format!("scat-cache-{}", current_uid())),
            &cache_key(&source),
        );
        assert!(entry.is_current(&stamp));
    }

    #[test]
    fn same_build_timestamp_with_another_schema_is_recopied_not_restamped() {
        let dir = TempDir::new().unwrap();
        let root = TempDir::new().unwrap();
        let source = dir.path().join("scripts.sqlite");
        let build = "2026-07-30T02:00:00";
        publish(&source, build);
        let cached = catalog_read_path(&source, &options(&root));

        // A newer scat on the same host (cache entries are per-user, shared
        // across scat versions) left a copy of the *same-timestamp* build in
        // a schema this scat cannot read.
        let newer = dir.path().join("newer.sqlite");
        publish(&newer, build);
        rusqlite::Connection::open(&newer)
            .unwrap()
            .execute(
                "UPDATE index_metadata SET schema_version = schema_version + 1",
                [],
            )
            .unwrap();
        fs::copy(&newer, &cached).unwrap();

        // The published file is replaced by the readable build: the entry must
        // be recopied, not re-stamped on the strength of the timestamp alone.
        let republished = dir.path().join("republished.sqlite");
        publish(&republished, build);
        fs::rename(&republished, &source).unwrap();

        let refreshed = catalog_read_path(&source, &options(&root));
        assert_ne!(refreshed, source);
        assert!(crate::core::db::open_db(&refreshed).is_ok());
    }

    #[test]
    fn world_writable_cache_directory_is_refused() {
        let dir = TempDir::new().unwrap();
        let root = TempDir::new().unwrap();
        let source = dir.path().join("scripts.sqlite");
        publish(&source, "2026-07-30T02:00:00");

        // Pre-create the per-uid directory with permissions that would let
        // another user on a shared host swap the cached catalog out.
        let cache = root.path().join(format!("scat-cache-{}", current_uid()));
        fs::create_dir_all(&cache).unwrap();
        fs::set_permissions(&cache, fs::Permissions::from_mode(0o777)).unwrap();

        assert_eq!(catalog_read_path(&source, &options(&root)), source);
    }

    #[test]
    fn cache_directory_is_private_to_its_owner() {
        let dir = TempDir::new().unwrap();
        let root = TempDir::new().unwrap();
        let source = dir.path().join("scripts.sqlite");
        publish(&source, "2026-07-30T02:00:00");

        let cached = catalog_read_path(&source, &options(&root));
        let cache_dir = cached.parent().unwrap();

        assert_eq!(fs::metadata(cache_dir).unwrap().mode() & 0o777, 0o700);
        assert_eq!(fs::metadata(&cached).unwrap().mode() & 0o777, 0o600);
    }

    #[test]
    fn scratch_files_from_an_interrupted_refresh_are_swept() {
        let dir = TempDir::new().unwrap();
        let root = TempDir::new().unwrap();
        let source = dir.path().join("scripts.sqlite");
        publish(&source, "2026-07-30T02:00:00");

        let cached = catalog_read_path(&source, &options(&root));
        let cache = cached.parent().unwrap();

        // A copy killed before its rename would leave this behind, holding a
        // whole database's worth of tmpfs until something removes it.
        let orphan = cache.join(format!("{}.sqlite.tmp.999999", cache_key(&source)));
        fs::write(&orphan, b"abandoned").unwrap();

        let rebuilt = dir.path().join("rebuilt.sqlite");
        publish(&rebuilt, "2026-07-31T02:00:00");
        fs::rename(&rebuilt, &source).unwrap();
        let _ = catalog_read_path(&source, &options(&root));

        assert!(!orphan.exists());
    }

    /// Write a plain SQLite file with no `index_metadata` table, mimicking
    /// the scat-embed embeddings sidecar rather than the catalog.
    fn publish_non_catalog_sqlite(path: &Path, marker: &str) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch("CREATE TABLE t (v TEXT NOT NULL);")
            .unwrap();
        conn.execute("INSERT INTO t (v) VALUES (?1)", [marker])
            .unwrap();
    }

    #[test]
    fn caches_a_sqlite_file_with_no_index_metadata_table() {
        let dir = TempDir::new().unwrap();
        let root = TempDir::new().unwrap();
        let source = dir.path().join("embeddings.sqlite");
        publish_non_catalog_sqlite(&source, "v1");

        let cached = catalog_read_path(&source, &options(&root));
        assert_ne!(cached, source);
        assert!(cached.is_file());

        let conn = open_readonly(&cached).unwrap();
        let value: String = conn
            .query_row("SELECT v FROM t", [], |row| row.get(0))
            .unwrap();
        assert_eq!(value, "v1");
    }

    #[test]
    fn recopies_a_non_catalog_sqlite_file_when_it_changes() {
        let dir = TempDir::new().unwrap();
        let root = TempDir::new().unwrap();
        let source = dir.path().join("embeddings.sqlite");
        publish_non_catalog_sqlite(&source, "v1");

        let cached = catalog_read_path(&source, &options(&root));
        let before = ino_of(&cached);

        // Republish, as scat-embed's atomic rename-based publish does.
        let rebuilt = dir.path().join("rebuilt.sqlite");
        publish_non_catalog_sqlite(&rebuilt, "v2");
        fs::rename(&rebuilt, &source).unwrap();

        let refreshed = catalog_read_path(&source, &options(&root));
        assert_eq!(refreshed, cached);
        assert_ne!(ino_of(&refreshed), before);

        let conn = open_readonly(&refreshed).unwrap();
        let value: String = conn
            .query_row("SELECT v FROM t", [], |row| row.get(0))
            .unwrap();
        assert_eq!(value, "v2");
    }

    #[test]
    fn distinct_catalogs_get_distinct_entries() {
        let dir = TempDir::new().unwrap();
        let root = TempDir::new().unwrap();
        let one = dir.path().join("one.sqlite");
        let two = dir.path().join("two.sqlite");
        publish(&one, "2026-07-30T02:00:00");
        publish(&two, "2026-07-30T02:00:00");

        assert_ne!(
            catalog_read_path(&one, &options(&root)),
            catalog_read_path(&two, &options(&root))
        );
    }

    #[test]
    fn concurrent_refreshes_produce_one_consistent_copy() {
        let dir = TempDir::new().unwrap();
        let root = TempDir::new().unwrap();
        let source = dir.path().join("scripts.sqlite");
        publish(&source, "2026-07-30T02:00:00");

        let paths: Vec<PathBuf> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| {
                    let source = source.clone();
                    let options = options(&root);
                    scope.spawn(move || catalog_read_path(&source, &options))
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });

        // Every racer ends up on the same entry, and it is a valid catalog.
        assert!(paths.windows(2).all(|pair| pair[0] == pair[1]));
        assert!(crate::core::db::open_db(&paths[0]).is_ok());
    }

    // -----------------------------------------------------------------------
    // Bounds and cleanup
    // -----------------------------------------------------------------------

    fn age_entry(entry_dir: &Path, key_of: &Path, secs_ago: u64) {
        let entry = CacheEntry::new(entry_dir, &cache_key(key_of));
        let when = SystemTime::now() - Duration::from_secs(secs_ago);
        File::options()
            .write(true)
            .open(&entry.meta)
            .unwrap()
            .set_modified(when)
            .unwrap();
    }

    fn cache_dir_of(root: &TempDir) -> PathBuf {
        root.path().join(format!("scat-cache-{}", current_uid()))
    }

    #[test]
    fn a_refused_catalog_keeps_the_older_entry_for_a_rollback_but_ages_it_out() {
        let dir = TempDir::new().unwrap();
        let root = TempDir::new().unwrap();
        let source = dir.path().join("scripts.sqlite");
        publish(&source, "2026-07-30T02:00:00");
        let cached = catalog_read_path(&source, &options(&root));
        assert_ne!(cached, source);

        let newer = dir.path().join("newer.sqlite");
        publish(&newer, "2026-07-31T02:00:00");
        rusqlite::Connection::open(&newer)
            .unwrap()
            .execute(
                "UPDATE index_metadata SET schema_version = schema_version + 1",
                [],
            )
            .unwrap();
        fs::rename(&newer, &source).unwrap();

        // Refused, read in place; the previous build's copy is not served but
        // survives, so a rollback can reuse it.
        assert_eq!(catalog_read_path(&source, &options(&root)), source);
        assert!(cached.exists());

        // If the upgrade sticks, nothing reads that copy and it is reclaimed.
        age_entry(&cache_dir_of(&root), &source, 30 * 24 * 3600);
        assert_eq!(catalog_read_path(&source, &options(&root)), source);
        assert!(
            !cached.exists(),
            "an unused superseded copy must not hold RAM forever"
        );
    }

    #[test]
    fn refreshing_prunes_entries_nobody_has_read_for_a_long_time() {
        let dir = TempDir::new().unwrap();
        let root = TempDir::new().unwrap();
        let old = dir.path().join("old.sqlite");
        let recent = dir.path().join("recent.sqlite");
        let current = dir.path().join("current.sqlite");
        for (path, ts) in [(&old, "a"), (&recent, "b"), (&current, "c")] {
            publish(path, ts);
        }
        let old_copy = catalog_read_path(&old, &options(&root));
        let recent_copy = catalog_read_path(&recent, &options(&root));
        let cache = cache_dir_of(&root);
        age_entry(&cache, &old, 30 * 24 * 3600);
        age_entry(&cache, &recent, 2 * 24 * 3600);

        // Caching another catalog sweeps the long-unused entry only.
        let current_copy = catalog_read_path(&current, &options(&root));
        assert!(current_copy.exists() && recent_copy.exists());
        assert!(!old_copy.exists());
        assert!(!cache.join(format!("{}.json", cache_key(&old))).exists());
        assert!(!cache.join(format!("{}.lock", cache_key(&old))).exists());
    }

    #[test]
    fn a_cache_hit_refreshes_the_last_use_time_only_when_it_is_old() {
        let dir = TempDir::new().unwrap();
        let root = TempDir::new().unwrap();
        let source = dir.path().join("scripts.sqlite");
        publish(&source, "2026-07-30T02:00:00");
        let _ = catalog_read_path(&source, &options(&root));
        let cache = cache_dir_of(&root);
        let meta = cache.join(format!("{}.json", cache_key(&source)));
        let mtime = || fs::metadata(&meta).unwrap().modified().unwrap();

        age_entry(&cache, &source, 3 * 3600);
        let aged = mtime();
        let _ = catalog_read_path(&source, &options(&root));
        assert!(mtime() > aged, "an old last-use time is renewed on a hit");

        let fresh = mtime();
        let _ = catalog_read_path(&source, &options(&root));
        assert_eq!(
            mtime(),
            fresh,
            "a recent one is left alone (no write per command)"
        );
    }

    #[test]
    fn the_size_limit_evicts_the_least_recently_used_entry_or_declines() {
        let dir = TempDir::new().unwrap();
        let root = TempDir::new().unwrap();
        let first = dir.path().join("first.sqlite");
        let second = dir.path().join("second.sqlite");
        let third = dir.path().join("third.sqlite");
        for (path, ts) in [(&first, "a"), (&second, "b"), (&third, "c")] {
            publish(path, ts);
        }
        let size = fs::metadata(&first).unwrap().len();
        // Room for two copies, not three.
        let limited = CacheOptions {
            max_bytes: Some(size * 2 + size / 2),
            ..options(&root)
        };
        let first_copy = catalog_read_path(&first, &limited);
        let second_copy = catalog_read_path(&second, &limited);
        let cache = cache_dir_of(&root);
        age_entry(&cache, &first, 3600 * 5);

        let third_copy = catalog_read_path(&third, &limited);
        assert_ne!(third_copy, third, "it was cached");
        assert!(
            !first_copy.exists(),
            "the least recently used entry made room"
        );
        assert!(second_copy.exists());

        // A limit smaller than one copy declines to cache at all.
        let tiny = CacheOptions {
            max_bytes: Some(size / 2),
            ..options(&root)
        };
        let fourth = dir.path().join("fourth.sqlite");
        publish(&fourth, "d");
        assert_eq!(catalog_read_path(&fourth, &tiny), fourth);
    }

    #[test]
    fn a_refresh_that_cannot_get_the_lock_reads_the_published_catalog() {
        let dir = TempDir::new().unwrap();
        let root = TempDir::new().unwrap();
        let source = dir.path().join("scripts.sqlite");
        publish(&source, "2026-07-30T02:00:00");
        let cache = cache_dir_of(&root);
        DirBuilder::new().mode(0o700).create(&cache).unwrap();
        // Another scat stuck mid-copy: it holds the entry's lock.
        let holder = File::options()
            .create(true)
            .write(true)
            .truncate(false)
            .open(cache.join(format!("{}.lock", cache_key(&source))))
            .unwrap();
        holder.lock().unwrap();

        let started = std::time::Instant::now();
        let impatient = CacheOptions {
            lock_wait: Duration::from_millis(150),
            ..options(&root)
        };
        assert_eq!(catalog_read_path(&source, &impatient), source);
        assert!(started.elapsed() < Duration::from_secs(5), "bounded wait");

        drop(holder);
        assert_ne!(catalog_read_path(&source, &options(&root)), source);
    }

    #[test]
    fn free_space_is_measurable_for_a_real_directory() {
        let dir = TempDir::new().unwrap();
        assert!(free_bytes(dir.path()).is_some_and(|free| free > 0));
        assert!(free_bytes(&dir.path().join("missing")).is_none());
    }

    #[test]
    fn status_clear_and_prune_manage_this_users_entries() {
        let dir = TempDir::new().unwrap();
        let root = TempDir::new().unwrap();
        let opts = options(&root);
        assert!(
            cache_status(&opts).directory.is_none(),
            "nothing created yet"
        );
        assert_eq!(cache_clear(&opts), Removed::default());

        let kept = dir.path().join("kept.sqlite");
        let gone = dir.path().join("gone.sqlite");
        let stale = dir.path().join("stale.sqlite");
        for (path, ts) in [(&kept, "a"), (&gone, "b"), (&stale, "c")] {
            publish(path, ts);
            let _ = catalog_read_path(path, &opts);
        }
        let status = cache_status(&opts);
        assert_eq!(status.entries.len(), 3);
        assert!(status.total_bytes > 0);
        let kept_info = status
            .entries
            .iter()
            .find(|e| e.source.as_deref() == Some(kept.as_path()))
            .unwrap();
        assert_eq!(kept_info.build_timestamp.as_deref(), Some("a"));

        // `prune` drops what nobody reads any more, and — checking sources —
        // what was retired from the share.
        age_entry(&cache_dir_of(&root), &stale, 40 * 24 * 3600);
        fs::remove_file(&gone).unwrap();
        let removed = cache_prune(&opts, DEFAULT_MAX_AGE, true);
        assert_eq!(removed.entries, 2);
        assert!(removed.bytes > 0);
        let remaining = cache_status(&opts).entries;
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].source.as_deref(), Some(kept.as_path()));

        // Without the source check a retired catalog is only pruned by age.
        fs::remove_file(&kept).unwrap();
        assert_eq!(cache_prune(&opts, DEFAULT_MAX_AGE, false).entries, 0);

        let cleared = cache_clear(&opts);
        assert_eq!(cleared.entries, 1);
        assert!(cache_status(&opts).entries.is_empty());
    }

    #[test]
    fn inspection_reports_how_much_the_cache_holds() {
        let dir = TempDir::new().unwrap();
        let root = TempDir::new().unwrap();
        let source = dir.path().join("scripts.sqlite");
        publish(&source, "2026-07-30T02:00:00");
        let opts = options(&root);
        let _ = catalog_read_path(&source, &opts);
        let report = inspect_cache(&source, &opts);
        assert_eq!(report["entries"], 1);
        assert!(report["total_bytes"].as_u64().unwrap() > 0);
    }
}
