use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde::Deserialize;
use serde_json::Value;
use tracing::{debug, warn};

use crate::error::Result;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

static CHECKOUT_RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();

fn checkout_re() -> &'static regex::Regex {
    CHECKOUT_RE.get_or_init(|| {
        regex::Regex::new(r"^(?P<script>.+)_(?P<timestamp>\d{8}_\d{4})_(?P<user>[^\\/]+)$").unwrap()
    })
}

// ---------------------------------------------------------------------------
// Config file schema
// ---------------------------------------------------------------------------

fn default_develop_dirs() -> Vec<String> {
    DEFAULT_DEVELOP_DIRS.iter().map(|s| s.to_string()).collect()
}

fn default_archive_dirs() -> Vec<String> {
    DEFAULT_ARCHIVE_DIRS.iter().map(|s| s.to_string()).collect()
}

#[derive(Debug, Deserialize)]
struct VcConfigSection {
    executable: Option<String>,
    /// Directory names treated as DEVELOP-type checkout containers (default: ["DEVELOP"]).
    #[serde(default = "default_develop_dirs")]
    develop_dirs: Vec<String>,
    /// Directory names treated as ARCHIVE-type checkout containers (default: ["ARCHIVE"]).
    #[serde(default = "default_archive_dirs")]
    archive_dirs: Vec<String>,
}

impl Default for VcConfigSection {
    fn default() -> Self {
        Self {
            executable: None,
            develop_dirs: default_develop_dirs(),
            archive_dirs: default_archive_dirs(),
        }
    }
}

#[derive(Debug, Deserialize, Default)]
struct VcConfigFile {
    db_path: Option<String>,
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
    /// Named search aliases; `scat search @name` resolves the alias before dispatch.
    pub bookmarks: std::collections::HashMap<String, String>,
}

impl Default for VcConfig {
    fn default() -> Self {
        Self {
            db_path: None,
            scan_roots: Vec::new(),
            ignore_patterns: Vec::new(),
            vc_executable: None,
            develop_dirs: DEFAULT_DEVELOP_DIRS.iter().map(|s| s.to_string()).collect(),
            archive_dirs: DEFAULT_ARCHIVE_DIRS.iter().map(|s| s.to_string()).collect(),
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
            .map(|s| s.as_str())
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
    /// Checkout owner parsed from filename.
    pub user: String,
    /// Checkout timestamp parsed from filename (`YYYYMMDD_HHMM`).
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

// ---------------------------------------------------------------------------
// Config loading
// ---------------------------------------------------------------------------

/// Load vc configuration from optional file and environment overrides.
pub fn load_vc_config(config_file: Option<&Path>) -> Result<VcConfig> {
    let mut file_data = VcConfigFile::default();

    if let Some(path) = config_file
        && path.exists()
    {
        let text = std::fs::read_to_string(path)?;
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        file_data = if matches!(ext.to_lowercase().as_str(), "yml" | "yaml") {
            yaml_serde::from_str(&text)?
        } else {
            serde_json::from_str(&text).map_err(crate::error::Error::Json)?
        };
    }

    let db_path = file_data.db_path.map(PathBuf::from);
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
        scan_roots,
        ignore_patterns,
        vc_executable: vc.executable.map(PathBuf::from),
        develop_dirs: vc.develop_dirs,
        archive_dirs: vc.archive_dirs,
        bookmarks: file_data.bookmarks.unwrap_or_default(),
    })
}

// ---------------------------------------------------------------------------
// Checkout scanning
// ---------------------------------------------------------------------------

/// Parse checkout filename into `(script, timestamp, user)`.
pub fn parse_checkout_filename(filename: &str) -> Option<(String, String, String)> {
    let m = checkout_re().captures(filename)?;
    Some((
        m["script"].to_string(),
        m["timestamp"].to_string(),
        m["user"].to_string(),
    ))
}

/// Scan DEVELOP and ARCHIVE directories embedded within each scan_root and return
/// discovered revision records. DEVELOP/ARCHIVE dirs are recognised at the scan_root
/// level itself and one level deep (immediate subdirectories). Directories reached
/// via symlinks are deduplicated by their canonicalised real path so that OS-variant
/// symlinks (e.g. `alt/pse → linux/pse`) are not scanned twice.
/// The `os_flavor` for each record is derived from the parent directory name of the
/// scan_root (e.g. `linux` from `/catalog/linux/scripts`).
pub fn scan_checkouts(config: &VcConfig, logical_prefix: &str) -> Vec<CheckoutRecord> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64();

    let mut records = Vec::new();
    let mut seen: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();

    for scan_root in &config.scan_roots {
        let os_flavor = scan_root
            .parent()
            .and_then(|p| p.file_name())
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();

        // Configured develop/archive dirs at the scan_root level
        for (dirs, rev_type) in [
            (config.develop_dirs.as_slice(), REVISION_TYPE_DEVELOP),
            (config.archive_dirs.as_slice(), REVISION_TYPE_ARCHIVE),
        ] {
            for dir_name in dirs {
                let dir = scan_root.join(dir_name);
                if dir.is_dir() {
                    scan_revision_dir(
                        &dir,
                        rev_type,
                        &os_flavor,
                        scan_root,
                        logical_prefix,
                        now,
                        &mut records,
                        &mut seen,
                    );
                }
            }
        }

        // Configured develop/archive dirs one level deep in immediate subdirectories
        let checkout_dir_names: std::collections::HashSet<&str> =
            config.all_checkout_dirs().collect();
        let mut subdirs: Vec<PathBuf> = std::fs::read_dir(scan_root)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir()) // follows symlinks; canonicalize+seen dedupes
            .filter(|p| {
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                !checkout_dir_names.contains(name)
            })
            .collect();
        subdirs.sort();

        for subdir in subdirs {
            for (dirs, rev_type) in [
                (config.develop_dirs.as_slice(), REVISION_TYPE_DEVELOP),
                (config.archive_dirs.as_slice(), REVISION_TYPE_ARCHIVE),
            ] {
                for dir_name in dirs {
                    let dir = subdir.join(dir_name);
                    if dir.is_dir() {
                        scan_revision_dir(
                            &dir,
                            rev_type,
                            &os_flavor,
                            scan_root,
                            logical_prefix,
                            now,
                            &mut records,
                            &mut seen,
                        );
                    }
                }
            }
        }
    }

    records
}

#[allow(clippy::too_many_arguments)]
fn scan_revision_dir(
    dir: &Path,
    revision_type: &str,
    os_flavor: &str,
    scan_root: &Path,
    logical_prefix: &str,
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
        .map(|p| p.to_string_lossy().replace('\\', "/"))
        .unwrap_or_default();

    // Don't follow symlinks within the DEVELOP/ARCHIVE dir — circular links
    // could cause infinite recursion. Symlinked scan_roots are already handled
    // by the canonicalize deduplication above.
    let mut paths: Vec<PathBuf> = walkdir::WalkDir::new(dir)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.path().to_path_buf())
        .collect();
    paths.sort();

    for path in paths {
        let filename = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };
        let (script_name, timestamp, user) = match parse_checkout_filename(filename) {
            Some(p) => p,
            None => continue,
        };

        // Relative path of the file's parent within the DEVELOP/ARCHIVE dir
        // (e.g. "" for direct children, "subdir" for nested files).
        let rel_in_dir = path
            .parent()
            .and_then(|p| p.strip_prefix(dir).ok())
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();

        // Combine non-empty path segments: prefix / rel_container / rel_in_dir / script_name
        let sub_parts: Vec<&str> = [rel_container.as_str(), rel_in_dir.as_str(), &script_name]
            .into_iter()
            .filter(|s| !s.is_empty() && *s != ".")
            .collect();
        let logical = if logical_prefix.is_empty() {
            format!("/{}", sub_parts.join("/"))
        } else {
            format!(
                "{}/{}",
                logical_prefix.trim_end_matches('/'),
                sub_parts.join("/")
            )
        };

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
// Helpers
// ---------------------------------------------------------------------------

fn parse_checkout_timestamp(value: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::NaiveDateTime::parse_from_str(value, "%Y%m%d_%H%M")
        .ok()
        .map(|ndt| ndt.and_utc())
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

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

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
}
