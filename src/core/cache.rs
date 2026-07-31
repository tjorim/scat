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

use std::fs::{self, DirBuilder, File};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracing::{debug, warn};

use crate::core::db::open_readonly;
use crate::error::{Error, Result};

/// Default cache root — tmpfs on every mainstream Linux distribution.
pub const DEFAULT_CACHE_ROOT: &str = "/dev/shm";

/// Cache directories are created (and required to stay) private to their
/// owner: `/dev/shm` itself is world-writable, so a shared host would
/// otherwise let one user hand another a catalog of their choosing.
const CACHE_DIR_MODE: u32 = 0o700;

/// Where and whether to cache the published catalog locally.
#[derive(Debug, Clone)]
pub struct CacheOptions {
    /// When false, callers read the published catalog in place.
    pub enabled: bool,
    /// Cache root; defaults to [`DEFAULT_CACHE_ROOT`] when `None`.
    pub root: Option<PathBuf>,
}

impl Default for CacheOptions {
    fn default() -> Self {
        Self {
            enabled: true,
            root: None,
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
        return Ok(Some(entry.db));
    }

    let lock = File::options()
        .create(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .open(&entry.lock)?;
    lock.lock()?;
    // Another process may have refreshed the entry while we waited on the
    // lock; re-check before paying for a second copy of the same build.
    if entry.is_current(&stamp) {
        return Ok(Some(entry.db));
    }

    entry.populate(source, &stamp)?;
    Ok(Some(entry.db))
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
    fn populate(&self, source: &Path, stamp: &SourceStamp) -> Result<()> {
        let src = open_readonly(source)?;
        let build_timestamp = build_timestamp(&src)?;

        // The published file changed identity (a rebuild, a rotation, a
        // restore from backup) but still carries the build we already hold.
        // Re-stamp the existing copy instead of pulling every page across the
        // network for a catalog we have byte-for-byte.
        if self.db.is_file()
            && cached_build_timestamp(&self.db).as_deref() == Some(&*build_timestamp)
        {
            return self.write_meta(stamp, build_timestamp);
        }

        // A copy killed between creating its scratch file and renaming it
        // leaves a full database's worth of RAM behind. We hold the lock, so
        // no other refresh of this entry is in flight and every leftover is
        // by definition abandoned.
        self.sweep_scratch_files();

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
        self.write_meta(stamp, build_timestamp)
    }

    /// Record what the cached copy now holds, atomically.
    fn write_meta(&self, stamp: &SourceStamp, build_timestamp: String) -> Result<()> {
        let meta = CacheMeta {
            source: stamp.clone(),
            build_timestamp,
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

    let Some(uid) = current_uid() else {
        debug!("could not determine the current uid; caching disabled");
        return Ok(None);
    };

    // Per-uid so that hosts shared between users cache once per user rather
    // than fighting over one directory nobody else may enter.
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

/// The calling process's real uid, via `/proc/self`'s owner.
fn current_uid() -> Option<u32> {
    fs::metadata("/proc/self").ok().map(|meta| meta.uid())
}

/// A stable, filesystem-safe name for `source`.
///
/// Canonicalised first so that two spellings of the same published catalog
/// (a symlinked mount point, a relative path) share one cache entry instead
/// of each copying the same database into RAM.
fn cache_key(source: &Path) -> String {
    let canonical = fs::canonicalize(source).unwrap_or_else(|_| source.to_path_buf());
    let digest = Sha256::digest(canonical.as_os_str().as_bytes());
    format!("{digest:x}")[..16].to_string()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::db::create_db;
    use tempfile::TempDir;

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
            &root
                .path()
                .join(format!("scat-cache-{}", current_uid().unwrap())),
            &cache_key(&source),
        );
        assert!(entry.is_current(&stamp));
    }

    #[test]
    fn world_writable_cache_directory_is_refused() {
        let dir = TempDir::new().unwrap();
        let root = TempDir::new().unwrap();
        let source = dir.path().join("scripts.sqlite");
        publish(&source, "2026-07-30T02:00:00");

        // Pre-create the per-uid directory with permissions that would let
        // another user on a shared host swap the cached catalog out.
        let cache = root
            .path()
            .join(format!("scat-cache-{}", current_uid().unwrap()));
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
}
