use std::collections::HashSet;
use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde::Deserialize;
use serde_json::Value;
use std::cmp::Ordering;
use tracing::{debug, warn};

use crate::core::db::{JsonRow, row_str};
use crate::error::Result;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

static CHECKOUT_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();

/// Matches vc version filenames: `<script>_<timestamp>[_<user>]`.
///
/// Observed timestamp precision varies (`YYYYMMDD`, `YYYYMMDD_HHMM`, or
/// `YYYYMMDD_HHMMSS`), and only DEVELOP checkouts carry the trailing user —
/// ARCHIVE entries and checked-in working-directory copies are
/// `<script>_<timestamp>` with no user suffix. See docs/VC_CONTRACT.md.
fn checkout_re() -> &'static regex::Regex {
    CHECKOUT_RE.get_or_init(|| {
        regex::Regex::new(
            r"^(?P<script>.+)_(?P<timestamp>\d{8}(?:_\d{4}(?:\d{2})?)?)(?:_(?P<user>[^\\/]+))?$",
        )
        .unwrap()
    })
}

// ---------------------------------------------------------------------------
// Config file schema
// ---------------------------------------------------------------------------

fn default_develop_dirs() -> Vec<String> {
    DEFAULT_DEVELOP_DIRS
        .iter()
        .map(std::string::ToString::to_string)
        .collect()
}

fn default_archive_dirs() -> Vec<String> {
    DEFAULT_ARCHIVE_DIRS
        .iter()
        .map(std::string::ToString::to_string)
        .collect()
}

#[derive(Debug, Deserialize)]
struct VcConfigSection {
    executable: Option<String>,
    /// Directory names treated as DEVELOP-type checkout containers (default: `["DEVELOP"]`).
    #[serde(default = "default_develop_dirs")]
    develop_dirs: Vec<String>,
    /// Directory names treated as ARCHIVE-type checkout containers (default: `["ARCHIVE"]`).
    #[serde(default = "default_archive_dirs")]
    archive_dirs: Vec<String>,
    /// Path to vc's own manifest of every file path it manages, one absolute
    /// path per line. Name and location are environment-specific. Optional —
    /// when unset, no manifest cross-reference warnings are produced. See
    /// [`infer_manifest_warnings`] and docs/VC_CONTRACT.md.
    manifest_path: Option<String>,
}

impl Default for VcConfigSection {
    fn default() -> Self {
        Self {
            executable: None,
            develop_dirs: default_develop_dirs(),
            archive_dirs: default_archive_dirs(),
            manifest_path: None,
        }
    }
}

#[derive(Debug, Deserialize, Default)]
struct VcConfigFile {
    db_path: Option<String>,
    cache_dir: Option<String>,
    /// Path to the `embeddings.sqlite` sidecar published by `scat-embed`
    /// (crates/scat-embed). Defaults to `embeddings.sqlite` next to `db_path`
    /// when unset.
    embeddings_path: Option<String>,
    scan_roots: Option<Vec<String>>,
    ignore_patterns: Option<Vec<String>>,
    vc: Option<VcConfigSection>,
    bookmarks: Option<std::collections::HashMap<String, String>>,
}

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Default directory names used as DEVELOP-type checkout containers.
pub const DEFAULT_DEVELOP_DIRS: &[&str] = &["DEVELOP"];
/// Default directory names used as ARCHIVE-type checkout containers.
pub const DEFAULT_ARCHIVE_DIRS: &[&str] = &["ARCHIVE"];

#[derive(Debug, Clone)]
/// Runtime configuration for catalog indexing and vc checkout/archive discovery.
pub struct VcConfig {
    /// Path to the catalog SQLite database.
    pub db_path: Option<PathBuf>,
    /// Directory holding the host-local catalog cache. Falls back to
    /// [`crate::core::cache::DEFAULT_CACHE_ROOT`] when unset.
    pub cache_dir: Option<PathBuf>,
    /// Path to the `embeddings.sqlite` sidecar published by `scat-embed`.
    /// Falls back to [`crate::core::embeddings::sidecar_path`] (next to
    /// `db_path`) when unset.
    pub embeddings_path: Option<PathBuf>,
    /// Root directories to scan recursively when building the catalog.
    pub scan_roots: Vec<PathBuf>,
    /// Gitignore-style patterns applied during catalog scanning.
    pub ignore_patterns: Vec<String>,
    /// Path to the vc executable. Falls back to PATH lookup if not set.
    pub vc_executable: Option<PathBuf>,
    /// Directory names treated as DEVELOP-type checkout containers.
    pub develop_dirs: Vec<String>,
    /// Directory names treated as ARCHIVE-type checkout containers.
    pub archive_dirs: Vec<String>,
    /// Path to vc's own manifest of every file path it manages, one absolute
    /// path per line. Name and location are environment-specific. `None`
    /// disables the manifest cross-reference warnings entirely.
    pub manifest_path: Option<PathBuf>,
    /// Named search aliases; `scat search @name` resolves the alias before dispatch.
    pub bookmarks: std::collections::HashMap<String, String>,
}

impl Default for VcConfig {
    fn default() -> Self {
        Self {
            db_path: None,
            cache_dir: None,
            embeddings_path: None,
            scan_roots: Vec::new(),
            ignore_patterns: Vec::new(),
            vc_executable: None,
            develop_dirs: DEFAULT_DEVELOP_DIRS
                .iter()
                .map(std::string::ToString::to_string)
                .collect(),
            archive_dirs: DEFAULT_ARCHIVE_DIRS
                .iter()
                .map(std::string::ToString::to_string)
                .collect(),
            manifest_path: None,
            bookmarks: std::collections::HashMap::new(),
        }
    }
}

impl VcConfig {
    /// Returns `true` when scan_roots are configured (checkout dirs are discovered automatically).
    pub fn configured(&self) -> bool {
        !self.scan_roots.is_empty()
    }

    /// All directory names that are treated as checkout containers (develop + archive).
    pub fn all_checkout_dirs(&self) -> impl Iterator<Item = &str> {
        self.develop_dirs
            .iter()
            .chain(self.archive_dirs.iter())
            .map(std::string::String::as_str)
    }
}

#[derive(Debug, Clone)]
/// Discovered checkout entry from a DEVELOP tree.
pub struct CheckoutRecord {
    /// Logical script path derived for this checkout.
    pub logical_path: String,
    /// Absolute checkout file path on disk.
    pub physical_path: String,
    /// Revision type for the checkout (`DEVELOP` or `ARCHIVE`).
    pub revision_type: String,
    /// OS flavor folder where the checkout was found.
    pub os_flavor: String,
    /// Checkout owner parsed from filename. Empty for ARCHIVE entries and
    /// other revision filenames that carry no user suffix.
    pub user: String,
    /// Checkout timestamp parsed from filename (`YYYYMMDD`, `YYYYMMDD_HHMM`,
    /// or `YYYYMMDD_HHMMSS`, as observed on disk).
    pub timestamp: String,
    /// Age in seconds based on file mtime, if available.
    pub age_seconds: Option<f64>,
}

#[derive(Debug, Clone)]
/// Warning inferred from checkout/archive/script consistency checks.
pub struct VcWarning {
    /// Logical script path the warning applies to.
    pub logical_path: String,
    /// Machine-readable warning kind.
    pub kind: String,
    /// Human-readable warning message.
    pub message: String,
    /// Additional warning details.
    pub details: serde_json::Map<String, Value>,
}

// Minimal script info needed for warning inference (populated by builder).
#[derive(Debug, Clone)]
/// Minimal indexed-script view used for warning inference.
pub struct ProcessedScript {
    /// Logical path indexed for the script.
    pub logical_path: String,
    /// Source file path used during indexing.
    pub physical_path: String,
    /// Optional logical symlink target path.
    pub symlink_target: Option<String>,
    /// Source file mtime epoch seconds.
    pub mtime: f64,
}

/// Revision type stored in the catalog.
pub const REVISION_TYPE_DEVELOP: &str = "DEVELOP";
/// Revision type stored in the catalog.
pub const REVISION_TYPE_ARCHIVE: &str = "ARCHIVE";
/// Revision type stored in the catalog: a checked-in version copy kept in the
/// working directory next to the active symlink (`<script>_<timestamp>`).
pub const REVISION_TYPE_WORKING: &str = "WORKING";
/// Revision type stored in the catalog: the version a rollback displaced from
/// the working directory into DEVELOP (`<script>_<timestamp>_RB_<abbr>`), kept
/// there as a re-editable candidate rather than deleted. Distinct from
/// [`REVISION_TYPE_DEVELOP`] because vc's own actions put it there, not a user
/// checking the script out to edit it — it must not be counted as an
/// in-progress checkout by "RB" (see [`scan_revision_dir`]'s use of
/// [`ROLLBACK_USER_PREFIX`]).
pub const REVISION_TYPE_ROLLBACK: &str = "ROLLBACK";

/// Marks a DEVELOP-directory user suffix as a rollback-displaced version
/// rather than an active checkout: `<script>_<timestamp>_RB_<abbr>`. The
/// checkout filename regex parses the whole `RB_<abbr>` as the `user` capture
/// group, so this prefix is stripped back off to recover the real abbreviation.
const ROLLBACK_USER_PREFIX: &str = "RB_";

/// Render a revision age in compact human-readable form.
pub fn relative_age(age_seconds: f64) -> String {
    let age = age_seconds.max(0.0);
    if age < 3_600.0 {
        format!("{:.0}m ago", age / 60.0)
    } else if age < 86_400.0 {
        format!("{:.0}h ago", age / 3_600.0)
    } else {
        format!("{:.0}d ago", age / 86_400.0)
    }
}

/// Compare revision rows by the display order used by CLI and TUI.
///
/// DEVELOP rows sort first, then ROLLBACK (rollback-displaced versions
/// sitting in DEVELOP as re-editable candidates), then WORKING (checked-in
/// copies in the working directory), then ARCHIVE; within a type, rows are
/// grouped by OS flavor, newest timestamp first, and finally user name.
pub fn compare_revision_rows(a: &JsonRow, b: &JsonRow) -> Ordering {
    revision_type_rank(row_str(a, "revision_type"))
        .cmp(&revision_type_rank(row_str(b, "revision_type")))
        .then_with(|| row_str(a, "os_flavor").cmp(row_str(b, "os_flavor")))
        .then_with(|| row_str(b, "timestamp").cmp(row_str(a, "timestamp")))
        .then_with(|| row_str(a, "user").cmp(row_str(b, "user")))
}

fn revision_type_rank(revision_type: &str) -> u8 {
    match revision_type {
        REVISION_TYPE_DEVELOP | "" => 0,
        REVISION_TYPE_ROLLBACK => 1,
        REVISION_TYPE_WORKING => 2,
        REVISION_TYPE_ARCHIVE => 3,
        _ => 4,
    }
}

// ---------------------------------------------------------------------------
// Config loading
// ---------------------------------------------------------------------------

/// Load vc configuration from optional file and environment overrides.
pub fn load_vc_config(config_file: Option<&Path>) -> Result<VcConfig> {
    let file_data = if let Some(path) = config_file
        && path.exists()
    {
        let text = std::fs::read_to_string(path)?;
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if matches!(ext.to_lowercase().as_str(), "yml" | "yaml") {
            yaml_serde::from_str(&text)?
        } else {
            serde_json::from_str(&text).map_err(crate::error::Error::Json)?
        }
    } else {
        VcConfigFile::default()
    };

    let db_path = file_data.db_path.map(PathBuf::from);
    let cache_dir = file_data.cache_dir.map(PathBuf::from);
    let embeddings_path = file_data.embeddings_path.map(PathBuf::from);
    let scan_roots = match std::env::var("SCAT_SCAN_ROOTS").ok() {
        Some(s) => s
            .split(',')
            .map(|p| PathBuf::from(p.trim()))
            .filter(|p| !p.as_os_str().is_empty())
            .collect(),
        None => file_data
            .scan_roots
            .unwrap_or_default()
            .into_iter()
            .map(PathBuf::from)
            .collect(),
    };
    let ignore_patterns = file_data.ignore_patterns.unwrap_or_default();
    let vc = file_data.vc.unwrap_or_default();

    Ok(VcConfig {
        db_path,
        cache_dir,
        embeddings_path,
        scan_roots,
        ignore_patterns,
        vc_executable: vc.executable.map(PathBuf::from),
        develop_dirs: vc.develop_dirs,
        archive_dirs: vc.archive_dirs,
        manifest_path: vc.manifest_path.map(PathBuf::from),
        bookmarks: file_data.bookmarks.unwrap_or_default(),
    })
}

// ---------------------------------------------------------------------------
// Checkout scanning
// ---------------------------------------------------------------------------

/// Parse a vc version filename into `(script, timestamp, user)`.
///
/// Accepts every observed on-disk form: a DEVELOP checkout
/// (`update_board_firmware.sh_20260720_103044_titd`), an ARCHIVE or
/// checked-in working-directory copy without a user suffix
/// (`update_board_firmware.sh_20240921_135312`), and timestamps at date,
/// minute, or second precision. `user` is an empty string when the filename
/// has no user suffix.
pub fn parse_checkout_filename(filename: &str) -> Option<(String, String, String)> {
    let m = checkout_re().captures(filename)?;
    Some((
        m["script"].to_string(),
        m["timestamp"].to_string(),
        m.name("user")
            .map(|u| u.as_str().to_string())
            .unwrap_or_default(),
    ))
}

/// Scan DEVELOP and ARCHIVE directories embedded within each scan_root and return
/// discovered revision records. Every script folder carries its own
/// DEVELOP/ARCHIVE containers, at any nesting depth, so the walk recurses the
/// whole tree under each scan_root. Directories reached via symlinks are
/// deduplicated by their canonicalised real path so that OS-variant symlinks
/// (e.g. `alt/scripts → linux/scripts`) are not scanned twice, and symlinks resolving
/// outside all scan_roots are not followed.
/// The `os_flavor` for each record is derived from the parent directory name of the
/// scan_root (e.g. `linux` from `/catalog/linux/scripts`).
pub fn scan_checkouts(config: &VcConfig) -> Vec<CheckoutRecord> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();

    let mut records = Vec::new();
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();

    // Bound symlinked-directory traversal to the configured roots, same as
    // the script scanner: a symlink aliasing another location inside the
    // roots is followed (and deduplicated below), one escaping to an
    // unrelated tree is not.
    let canonical_roots: Vec<PathBuf> = config
        .scan_roots
        .iter()
        .filter_map(|r| std::fs::canonicalize(r).ok())
        .collect();
    // Directories already walked, by canonical path — global across
    // scan_roots so an OS-variant symlink alias (`alt/scripts → linux/scripts`) is
    // walked once, under the first scan_root that reaches it (keeping
    // os_flavor derivation stable).
    let mut visited: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();

    for scan_root in &config.scan_roots {
        let os_flavor = scan_root
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();

        let mut queue = std::collections::VecDeque::from([scan_root.clone()]);
        while let Some(dir) = queue.pop_front() {
            let Ok(canon) = std::fs::canonicalize(&dir) else {
                continue;
            };
            if !visited.insert(canon) {
                continue;
            }

            let mut subdirs: Vec<PathBuf> = match std::fs::read_dir(&dir) {
                Ok(entries) => entries
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.is_dir()) // follows symlinks; bounded + deduped below
                    .collect(),
                Err(err) => {
                    warn!(
                        path = %dir.display(),
                        error = %err,
                        "failed to read directory during checkout scan, skipping"
                    );
                    continue;
                }
            };
            subdirs.sort();

            for subdir in subdirs {
                let name = subdir.file_name().and_then(|n| n.to_str()).unwrap_or("");
                let rev_type = if config.develop_dirs.iter().any(|d| d == name) {
                    Some(REVISION_TYPE_DEVELOP)
                } else if config.archive_dirs.iter().any(|d| d == name) {
                    Some(REVISION_TYPE_ARCHIVE)
                } else {
                    None
                };
                if let Some(rev_type) = rev_type {
                    // A container dir is scanned for revision files but not
                    // descended into for further containers.
                    scan_revision_dir(
                        &subdir,
                        rev_type,
                        &os_flavor,
                        scan_root,
                        now,
                        &mut records,
                        &mut seen,
                    );
                } else if subdir.is_symlink()
                    && !std::fs::canonicalize(&subdir)
                        .is_ok_and(|c| canonical_roots.iter().any(|r| c.starts_with(r)))
                {
                    warn!(
                        path = %subdir.display(),
                        "symlinked directory resolves outside configured scan roots, skipping to avoid unbounded traversal"
                    );
                } else {
                    queue.push_back(subdir);
                }
            }
        }
    }

    records
}

fn scan_revision_dir(
    dir: &Path,
    revision_type: &str,
    os_flavor: &str,
    scan_root: &Path,
    now: f64,
    records: &mut Vec<CheckoutRecord>,
    seen: &mut std::collections::HashSet<PathBuf>,
) {
    let real_path = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
    if !seen.insert(real_path) {
        return;
    }

    // Relative path of the DEVELOP/ARCHIVE dir's parent within scan_root
    // (e.g. "" for root-level, "group1" for a subfolder).
    let container = dir.parent().unwrap_or(scan_root);
    let rel_container = container
        .strip_prefix(scan_root)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();

    // Don't follow symlinks within the DEVELOP/ARCHIVE dir — circular links
    // could cause infinite recursion. Symlinked scan_roots are already handled
    // by the canonicalize deduplication above.
    let mut paths: Vec<PathBuf> = walkdir::WalkDir::new(dir)
        .follow_links(false)
        .into_iter()
        .filter_map(std::result::Result::ok)
        .filter(|e| e.file_type().is_file())
        .map(|e| e.path().to_path_buf())
        .collect();
    paths.sort();

    for path in paths {
        let filename = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };

        // vc writes a hidden companion file alongside every DEVELOP checkout —
        // `.<checkout-filename>`, holding a stat value and the canonical
        // original-version path it was checked out from, used by vc to detect
        // whether the production target changed since checkout. Its name still
        // matches the checkout filename convention once the leading dot is
        // swallowed by the greedy `script` capture (`.deploy_20240315_1430_jdoe`
        // parses as script `.deploy`), which would otherwise register it as a
        // second, bogus checkout of a script named `.deploy`. It's vc's own
        // bookkeeping, not a revision, so skip it.
        if filename.starts_with('.') {
            continue;
        }

        let (script_name, timestamp, user) = match parse_checkout_filename(filename) {
            Some(p) => p,
            None => continue,
        };

        // When the live target changed while a checkout was in progress, vc
        // runs a merge and leaves `<checkout-filename>.org` — a backup of the
        // pre-merge checkout — alongside it, plus a transient `.merged` file
        // during the merge itself. The abbreviation vc encodes in a real
        // checkout filename is always the fixed-width, dot-free token `utel`
        // produces, so a user capture ending in one of these suffixes is
        // never a genuine checkout — it's vc's own merge bookkeeping.
        if matches!(
            Path::new(&user).extension().and_then(|e| e.to_str()),
            Some("org" | "merged")
        ) {
            continue;
        }

        // A rollback moves the version it displaces into DEVELOP as
        // `<script>_<timestamp>_RB_<abbr>` rather than deleting it — see
        // `REVISION_TYPE_ROLLBACK`. The checkout filename regex has no way to
        // tell that apart from a real checkout, so it lands in the `user`
        // capture as `RB_<abbr>`; recover the real abbreviation and
        // reclassify. Only DEVELOP filenames carry a user suffix at all, so
        // this never fires for an ARCHIVE entry.
        let (revision_type, user) = match user.strip_prefix(ROLLBACK_USER_PREFIX) {
            Some(abbr) if revision_type == REVISION_TYPE_DEVELOP && !abbr.is_empty() => {
                (REVISION_TYPE_ROLLBACK, abbr.to_string())
            }
            _ => (revision_type, user),
        };

        // Relative path of the file's parent within the DEVELOP/ARCHIVE dir
        // (e.g. "" for direct children, "subdir" for nested files).
        let rel_in_dir = path
            .parent()
            .and_then(|p| p.strip_prefix(dir).ok())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();

        // Combine non-empty path segments: scan_root / rel_container / rel_in_dir / script_name.
        // Anchoring at scan_root's own absolute path (rather than at `/`)
        // mirrors `scanner::make_logical_path`, so a DEVELOP/ARCHIVE
        // revision's logical_path lines up with the active script's (built
        // the same way) and the two join in the catalog.
        let sub_parts: Vec<&str> = [rel_container.as_str(), rel_in_dir.as_str(), &script_name]
            .into_iter()
            .filter(|s| !s.is_empty() && *s != ".")
            .collect();
        let logical = format!(
            "{}/{}",
            scan_root.to_string_lossy().trim_end_matches('/'),
            sub_parts.join("/")
        );

        let age_seconds = path
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| (now - d.as_secs_f64()).max(0.0));

        records.push(CheckoutRecord {
            logical_path: logical,
            physical_path: path.to_string_lossy().into_owned(),
            revision_type: revision_type.to_string(),
            os_flavor: os_flavor.to_string(),
            user,
            timestamp,
            age_seconds,
        });
    }
}

// ---------------------------------------------------------------------------
// Warning inference
// ---------------------------------------------------------------------------

/// Infer consistency warnings from indexed scripts and revisions already stored in the DB.
pub fn infer_warnings(conn: &Connection) -> Result<Vec<VcWarning>> {
    let mut warnings = Vec::new();

    let mut orphan_stmt = conn.prepare(
        "SELECT r.logical_path, COUNT(*) AS revisions
         FROM revisions r
         LEFT JOIN scripts s ON s.logical_path = r.logical_path
         WHERE s.id IS NULL
         GROUP BY r.logical_path
         ORDER BY r.logical_path",
    )?;
    let orphan_rows = orphan_stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    for row in orphan_rows {
        let (logical_path, revisions) = row?;
        let mut details = serde_json::Map::new();
        details.insert("checkouts".into(), Value::Number(revisions.into()));
        warnings.push(VcWarning {
            logical_path,
            kind: "checkout_without_catalog_entry".into(),
            message: "Revision exists in DEVELOP/ARCHIVE but no active catalog entry was indexed."
                .into(),
            details,
        });
    }

    let mut drift_stmt = conn.prepare(
        "SELECT s.logical_path, s.mtime, latest.timestamp
         FROM scripts s
         JOIN (
             SELECT logical_path, MAX(timestamp) AS timestamp
             FROM revisions
             WHERE revision_type = ?1
             GROUP BY logical_path
         ) latest ON latest.logical_path = s.logical_path
         ORDER BY s.logical_path",
    )?;
    let drift_rows = drift_stmt.query_map([REVISION_TYPE_DEVELOP], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<f64>>(1)?.unwrap_or(0.0),
            row.get::<_, String>(2)?,
        ))
    })?;
    for row in drift_rows {
        let (logical_path, mtime, timestamp) = row?;
        let Some(checkout_dt) = parse_checkout_timestamp(&timestamp) else {
            continue;
        };
        #[allow(deprecated)]
        let script_dt = chrono::DateTime::<chrono::Utc>::from_timestamp(
            mtime as i64,
            ((mtime.fract()) * 1_000_000_000.0) as u32,
        )
        .unwrap_or(chrono::DateTime::UNIX_EPOCH);
        if script_dt > checkout_dt {
            let mut details = serde_json::Map::new();
            details.insert("checkout_timestamp".into(), Value::String(timestamp));
            details.insert("active_mtime".into(), Value::String(script_dt.to_rfc3339()));
            warnings.push(VcWarning {
                logical_path,
                kind: "timestamp_drift".into(),
                message: "Active script is newer than the observed DEVELOP checkout timestamp."
                    .into(),
                details,
            });
        }
    }

    let mut symlink_stmt = conn.prepare(
        "SELECT logical_path, symlink_target
         FROM scripts
         WHERE symlink_target IS NOT NULL
           AND TRIM(symlink_target) != ''
         ORDER BY logical_path",
    )?;
    let symlink_rows = symlink_stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in symlink_rows {
        let (logical_path, target) = row?;
        if logical_path == target {
            warnings.push(VcWarning {
                logical_path,
                kind: "self_referential_symlink".into(),
                message: "Symlink target resolves back to the same logical path.".into(),
                details: serde_json::Map::new(),
            });
        } else if !target_name_matches(&logical_path, &target) {
            let mut details = serde_json::Map::new();
            details.insert("symlink_target".into(), Value::String(target));
            warnings.push(VcWarning {
                logical_path,
                kind: "symlink_name_mismatch".into(),
                message: "Symlink target name does not resemble the logical script name.".into(),
                details,
            });
        }
    }

    let mut missing_archive_stmt = conn.prepare(
        "SELECT s.logical_path
         FROM scripts s
         WHERE EXISTS (
             SELECT 1
             FROM revisions d
             WHERE d.logical_path = s.logical_path
               AND d.revision_type = ?1
         )
           AND NOT EXISTS (
               SELECT 1
               FROM revisions a
               WHERE a.logical_path = s.logical_path
                 AND a.revision_type = ?2
         )
         ORDER BY s.logical_path",
    )?;
    let missing_archive_rows = missing_archive_stmt
        .query_map([REVISION_TYPE_DEVELOP, REVISION_TYPE_ARCHIVE], |row| {
            row.get::<_, String>(0)
        })?;
    for row in missing_archive_rows {
        warnings.push(VcWarning {
            logical_path: row?,
            kind: "missing_archive_entries".into(),
            message: "Checkout state exists but no matching ARCHIVE entry was observed.".into(),
            details: serde_json::Map::new(),
        });
    }

    let mut scripttype_stmt = conn.prepare(
        "SELECT logical_path, language, metadata_json
         FROM scripts
         WHERE metadata_json IS NOT NULL AND metadata_json != ''
         ORDER BY logical_path",
    )?;
    let scripttype_rows = scripttype_stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    for row in scripttype_rows {
        let (logical_path, language, metadata_json) = row?;
        let Ok(metadata) = serde_json::from_str::<Value>(&metadata_json) else {
            continue;
        };
        let Some(declared) = metadata.get("scripttype").and_then(Value::as_str) else {
            continue;
        };
        let Some(canonical) = normalize_scripttype(declared) else {
            continue;
        };
        if canonical != language {
            let mut details = serde_json::Map::new();
            details.insert(
                "declared_scripttype".into(),
                Value::String(declared.to_string()),
            );
            details.insert("detected_language".into(), Value::String(language));
            warnings.push(VcWarning {
                logical_path,
                kind: "scripttype_language_mismatch".into(),
                message:
                    "The @scripttype header keyword disagrees with the language scat detected."
                        .into(),
                details,
            });
        }
    }

    debug!(
        warning_count = warnings.len(),
        "completed vc warning inference"
    );
    for warning in &warnings {
        warn!(
            logical_path = %warning.logical_path,
            kind = %warning.kind,
            message = %warning.message,
            "generated vc warning"
        );
    }

    Ok(warnings)
}

// ---------------------------------------------------------------------------
// Managed-file manifest cross-reference (optional; see `VcConfig::manifest_path`)
// ---------------------------------------------------------------------------

/// Load vc's own manifest of every file path it manages (see
/// docs/VC_CONTRACT.md), one absolute path per line.
///
/// The manifest cross-reference is entirely optional (see
/// [`VcConfig::manifest_path`]), so a missing or unreadable file must not
/// fail the whole build — this logs a warning and returns an empty set,
/// which [`infer_manifest_warnings`] treats as "nothing to cross-reference"
/// rather than as an error.
pub fn load_vc_manifest(path: &Path) -> HashSet<String> {
    match std::fs::read_to_string(path) {
        Ok(text) => text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect(),
        Err(err) => {
            warn!(
                path = %path.display(),
                error = %err,
                "failed to read vc's managed-file manifest, skipping manifest cross-reference"
            );
            HashSet::new()
        }
    }
}

/// Cross-reference vc's own manifest of managed files ([`load_vc_manifest`])
/// against what was actually indexed. Assumes manifest entries and
/// `scripts.logical_path` use the same absolute-path convention (true when
/// both are observed from the same host/mount namespace scat scans from).
///
/// Two mismatches are worth surfacing, at different confidence:
/// - `registered_with_vc_but_not_indexed`: vc manages this path but scat
///   never found it — usually a scan-root or ignore-pattern gap, and a
///   strong signal something is missing from the catalog.
/// - `not_registered_with_vc`: scat indexed this path but vc's manifest
///   doesn't list it. Much weaker — an ordinary, non-vc-managed utility
///   script sitting inside a vc-managed tree looks the same, so treat this
///   as a lead to check rather than a confirmed problem.
///
/// Returns no warnings when `manifest` is empty (unconfigured or unreadable).
pub fn infer_manifest_warnings(
    conn: &Connection,
    manifest: &HashSet<String>,
) -> Result<Vec<VcWarning>> {
    if manifest.is_empty() {
        return Ok(Vec::new());
    }

    let mut indexed: HashSet<String> = HashSet::new();
    {
        let mut stmt = conn.prepare("SELECT logical_path FROM scripts")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        for row in rows {
            indexed.insert(row?);
        }
    }

    let mut warnings = Vec::new();

    let mut not_indexed: Vec<&String> = manifest.difference(&indexed).collect();
    not_indexed.sort();
    for logical_path in not_indexed {
        warnings.push(VcWarning {
            logical_path: logical_path.clone(),
            kind: "registered_with_vc_but_not_indexed".into(),
            message: "vc's own manifest lists this script, but scanning never found it.".into(),
            details: serde_json::Map::new(),
        });
    }

    let mut not_registered: Vec<&String> = indexed.difference(manifest).collect();
    not_registered.sort();
    for logical_path in not_registered {
        warnings.push(VcWarning {
            logical_path: logical_path.clone(),
            kind: "not_registered_with_vc".into(),
            message: "This script was indexed but does not appear in vc's managed-file manifest."
                .into(),
            details: serde_json::Map::new(),
        });
    }

    debug!(
        warning_count = warnings.len(),
        "completed vc manifest cross-reference"
    );

    Ok(warnings)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Parse a revision timestamp at any of the observed precisions: `YYYYMMDD`
/// (treated as midnight), `YYYYMMDD_HHMM`, or `YYYYMMDD_HHMMSS`.
fn parse_checkout_timestamp(value: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    let ndt = match value.len() {
        8 => chrono::NaiveDate::parse_from_str(value, "%Y%m%d")
            .ok()?
            .and_hms_opt(0, 0, 0)?,
        13 => chrono::NaiveDateTime::parse_from_str(value, "%Y%m%d_%H%M").ok()?,
        15 => chrono::NaiveDateTime::parse_from_str(value, "%Y%m%d_%H%M%S").ok()?,
        _ => return None,
    };
    Some(ndt.and_utc())
}

fn target_name_matches(logical_path: &str, target: &str) -> bool {
    let logical_name = Path::new(logical_path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let target_name = Path::new(target)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("");
    if logical_name.is_empty() || target_name.is_empty() {
        return true;
    }
    let logical_stem = logical_name
        .rsplit_once('.')
        .map_or(logical_name, |(s, _)| s);
    let target_stem = target_name.rsplit_once('.').map_or(target_name, |(s, _)| s);
    target_stem.starts_with(logical_stem)
        || logical_stem == target_stem
        || logical_stem
            .strip_prefix(target_stem)
            .is_some_and(|suffix| suffix.starts_with(['_', '-']))
}

/// Map a `@scripttype` header value to the canonical language key scat's own
/// detector would produce, when the value unambiguously names one of the
/// languages scat recognizes (see `indexer::scanner::detect_language`).
/// Returns `None` for any other value — an unrecognized taxonomy, a
/// purpose/category rather than a language, or free text — so
/// [`infer_warnings`] only compares when it can be confident, rather than
/// guessing at a `@scripttype` vocabulary this repository has no real sample
/// of.
fn normalize_scripttype(value: &str) -> Option<&'static str> {
    match value.trim().to_lowercase().as_str() {
        "python" | "py" | "python2" | "python3" => Some("python"),
        "shell" | "sh" | "bash" | "ksh" | "ksh93" => Some("shell"),
        "yaml" | "yml" => Some("yaml"),
        "csv" => Some("csv"),
        "json" => Some("json"),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_vc_manifest_parses_one_path_per_line() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(
            file.path(),
            "/catalog/scripts/deploy.sh\n/catalog/scripts/health.py\n",
        )
        .unwrap();

        let paths = load_vc_manifest(file.path());
        assert_eq!(paths.len(), 2);
        assert!(paths.contains("/catalog/scripts/deploy.sh"));
        assert!(paths.contains("/catalog/scripts/health.py"));
    }

    #[test]
    fn load_vc_manifest_trims_whitespace_and_skips_blank_lines() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), "  /catalog/scripts/deploy.sh  \n\n\n").unwrap();

        let paths = load_vc_manifest(file.path());
        assert_eq!(paths.len(), 1);
        assert!(paths.contains("/catalog/scripts/deploy.sh"));
    }

    #[test]
    fn load_vc_manifest_returns_empty_set_for_missing_file() {
        let paths = load_vc_manifest(Path::new("/nonexistent/manifest.lst"));
        assert!(paths.is_empty());
    }

    #[test]
    fn parse_checkout_filename_valid() {
        let r = parse_checkout_filename("foo_20240315_1430_jdoe");
        assert_eq!(
            r,
            Some(("foo".into(), "20240315_1430".into(), "jdoe".into()))
        );
    }

    #[test]
    fn parse_checkout_filename_with_underscores_in_script() {
        let r = parse_checkout_filename("my_script_20240315_1430_alice");
        assert!(r.is_some());
        let (script, ts, user) = r.unwrap();
        assert_eq!(ts, "20240315_1430");
        assert_eq!(user, "alice");
        assert!(script.contains("my_script"));
    }

    #[test]
    fn parse_checkout_filename_invalid() {
        assert!(parse_checkout_filename("nodates").is_none());
        assert!(parse_checkout_filename("foo_baddate_user").is_none());
        // Timestamp must be the last component (bar the optional user):
        // an extension after it means this is not a vc version filename.
        assert!(parse_checkout_filename("data_20240101.csv").is_none());
    }

    #[test]
    fn parse_checkout_filename_seconds_precision_with_user() {
        // Real-world DEVELOP checkout: HHMMSS timestamp plus user.
        let r = parse_checkout_filename("update_board_firmware.sh_20260720_103044_titd");
        assert_eq!(
            r,
            Some((
                "update_board_firmware.sh".into(),
                "20260720_103044".into(),
                "titd".into()
            ))
        );
    }

    #[test]
    fn parse_checkout_filename_without_user() {
        // ARCHIVE entries and checked-in working-dir copies have no user suffix.
        let r = parse_checkout_filename("update_board_firmware.sh_20240921_135312");
        assert_eq!(
            r,
            Some((
                "update_board_firmware.sh".into(),
                "20240921_135312".into(),
                String::new()
            ))
        );
        let r = parse_checkout_filename("update_board_firmware.sh_20240921_1353");
        assert_eq!(
            r,
            Some((
                "update_board_firmware.sh".into(),
                "20240921_1353".into(),
                String::new()
            ))
        );
    }

    #[test]
    fn parse_checkout_filename_date_only_timestamp() {
        let r = parse_checkout_filename("update_board_firmware.sh_20240921");
        assert_eq!(
            r,
            Some((
                "update_board_firmware.sh".into(),
                "20240921".into(),
                String::new()
            ))
        );
        let r = parse_checkout_filename("foo.sh_20240921_titd");
        assert_eq!(r, Some(("foo.sh".into(), "20240921".into(), "titd".into())));
    }

    #[test]
    fn parse_checkout_timestamp_accepts_all_observed_precisions() {
        let date_only = parse_checkout_timestamp("20240921").unwrap();
        let minutes = parse_checkout_timestamp("20240921_1353").unwrap();
        let seconds = parse_checkout_timestamp("20240921_135312").unwrap();
        assert_eq!(date_only.to_rfc3339(), "2024-09-21T00:00:00+00:00");
        assert_eq!(minutes.to_rfc3339(), "2024-09-21T13:53:00+00:00");
        assert_eq!(seconds.to_rfc3339(), "2024-09-21T13:53:12+00:00");
        assert!(parse_checkout_timestamp("garbage").is_none());
        assert!(parse_checkout_timestamp("20240921_13").is_none());
    }

    #[test]
    fn default_config_is_not_configured() {
        let cfg = VcConfig::default();
        assert!(!cfg.configured());
    }

    #[test]
    fn config_with_scan_roots_is_configured() {
        let cfg = VcConfig {
            scan_roots: vec![std::path::PathBuf::from("/some/root")],
            ..Default::default()
        };
        assert!(cfg.configured());
    }

    #[test]
    fn target_name_matches_same_stem() {
        assert!(target_name_matches(
            "/catalog/scripts/foo.py",
            "/archive/foo_20240101_1200.py"
        ));
    }

    #[test]
    fn target_name_does_not_match_different_stem() {
        assert!(!target_name_matches(
            "/catalog/scripts/foo.py",
            "/archive/bar_20240101_1200.py"
        ));
    }

    #[test]
    fn normalize_scripttype_recognizes_known_language_tokens() {
        assert_eq!(normalize_scripttype("python"), Some("python"));
        assert_eq!(normalize_scripttype("Python3"), Some("python"));
        assert_eq!(normalize_scripttype("  SHELL  "), Some("shell"));
        assert_eq!(normalize_scripttype("bash"), Some("shell"));
        assert_eq!(normalize_scripttype("ksh"), Some("shell"));
        assert_eq!(normalize_scripttype("YAML"), Some("yaml"));
        assert_eq!(normalize_scripttype("csv"), Some("csv"));
        assert_eq!(normalize_scripttype("json"), Some("json"));
    }

    #[test]
    fn normalize_scripttype_ignores_unrecognized_values() {
        // An unknown taxonomy (a purpose/category rather than a language, or
        // free text) must not be guessed at — no comparison, no false
        // positive.
        assert_eq!(normalize_scripttype("utility"), None);
        assert_eq!(normalize_scripttype("cronjob"), None);
        assert_eq!(normalize_scripttype(""), None);
    }

    fn revision_row(revision_type: &str, os_flavor: &str, user: &str, timestamp: &str) -> JsonRow {
        let mut row = JsonRow::new();
        row.insert("revision_type".to_string(), revision_type.into());
        row.insert("os_flavor".to_string(), os_flavor.into());
        row.insert("user".to_string(), user.into());
        row.insert("timestamp".to_string(), timestamp.into());
        row
    }

    #[test]
    fn relative_age_clamps_negative_values() {
        assert_eq!(relative_age(-120.0), "0m ago");
    }

    #[test]
    fn compare_revision_rows_orders_develop_os_newest_user() {
        let mut rows = [
            revision_row(REVISION_TYPE_DEVELOP, "ZOS", "alice", "20240101_1000"),
            revision_row(REVISION_TYPE_ARCHIVE, "LINUX", "arch", "20240103_1000"),
            revision_row(REVISION_TYPE_DEVELOP, "LINUX", "bob", "20240101_0900"),
            revision_row(REVISION_TYPE_DEVELOP, "LINUX", "jdoe", "20240102_0900"),
        ];

        rows.sort_by(compare_revision_rows);

        assert_eq!(row_str(&rows[0], "user"), "jdoe");
        assert_eq!(row_str(&rows[1], "user"), "bob");
        assert_eq!(row_str(&rows[2], "user"), "alice");
        assert_eq!(row_str(&rows[3], "user"), "arch");
    }
}
