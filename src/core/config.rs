//! The configuration file (`--config` / `SCAT_CONFIG`) and its environment
//! overrides, as one typed [`Config`].
//!
//! The file is strict: an explicitly given path must exist, unknown keys are
//! errors, and relative paths resolve against the file's own directory. The
//! sections map onto the groups below; [`crate::core::vc::VcConfig`] holds only
//! the `vc:` section.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::core::vc::{TimestampZone, VcConfig};
use crate::error::Result;

// ---------------------------------------------------------------------------
// Typed configuration
// ---------------------------------------------------------------------------

/// Where the catalog and its companions live.
#[derive(Debug, Clone, Default)]
pub struct CatalogConfig {
    /// Path to the catalog SQLite database.
    pub db_path: Option<PathBuf>,
    /// Directory holding the host-local catalog cache. Falls back to
    /// [`crate::core::cache::DEFAULT_CACHE_ROOT`] when unset.
    pub cache_dir: Option<PathBuf>,
    /// Path to the `embeddings.sqlite` sidecar published by `scat-embed`.
    /// Falls back to [`crate::core::embeddings::sidecar_path`] (next to
    /// `db_path`) when unset.
    pub embeddings_path: Option<PathBuf>,
}

/// What `scat catalog build` scans.
#[derive(Debug, Clone, Default)]
pub struct IndexConfig {
    /// Root directories to scan recursively when building the catalog.
    pub scan_roots: Vec<PathBuf>,
    /// Gitignore-style patterns applied during catalog scanning.
    pub ignore_patterns: Vec<String>,
}

/// Defaults for the terminal modes, below `--glyphs`/`--colors` and
/// `SCAT_TUI_GLYPHS`/`SCAT_TUI_COLORS` in priority.
#[derive(Debug, Clone, Default)]
pub struct TuiConfig {
    /// Configured glyph mode name (validated by the TUI launcher); `None`
    /// leaves the choice to detection.
    pub glyphs: Option<String>,
    /// Configured palette name (validated by the TUI launcher).
    pub colors: Option<String>,
}

/// The whole configuration.
#[derive(Debug, Clone, Default)]
pub struct Config {
    /// Catalog, cache and sidecar locations.
    pub catalog: CatalogConfig,
    /// Scan roots and ignore patterns for catalog builds.
    pub index: IndexConfig,
    /// vc checkout/archive discovery.
    pub vc: VcConfig,
    /// Terminal mode defaults.
    pub tui: TuiConfig,
    /// Named search aliases; `scat search @name` resolves the alias before dispatch.
    pub bookmarks: HashMap<String, String>,
}

impl Config {
    /// Whether vc integration is configured (scan roots are set; checkout
    /// directories are discovered automatically). Gates the display of
    /// revisions, so stale rows are not shown as current.
    pub fn vc_configured(&self) -> bool {
        !self.index.scan_roots.is_empty()
    }
}

// ---------------------------------------------------------------------------
// File schema
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct VcSection {
    executable: Option<String>,
    /// Directory names treated as DEVELOP-type checkout containers (default: `["DEVELOP"]`).
    develop_dirs: Option<Vec<String>>,
    /// Directory names treated as ARCHIVE-type checkout containers (default: `["ARCHIVE"]`).
    archive_dirs: Option<Vec<String>>,
    /// Path to vc's own manifest of every file path it manages, one absolute
    /// path per line. Optional — when unset, no manifest cross-reference
    /// warnings are produced. See docs/VC_CONTRACT.md.
    manifest_path: Option<String>,
    /// Clock of the timestamps in checkout filenames: `utc` (default),
    /// `local` (this host's zone) or a fixed offset such as `+02:00`.
    timestamp_timezone: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct TuiSection {
    glyphs: Option<String>,
    colors: Option<String>,
}

// Unknown keys are rejected (here and in the sections above): a typo such as
// `scan_rootz` would otherwise be dropped silently, and for the nightly build
// that quietly changes what gets indexed.
#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    db_path: Option<String>,
    cache_dir: Option<String>,
    embeddings_path: Option<String>,
    scan_roots: Option<Vec<String>>,
    ignore_patterns: Option<Vec<String>>,
    vc: Option<VcSection>,
    bookmarks: Option<HashMap<String, String>>,
    tui: Option<TuiSection>,
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

/// Parse the text of a config file. The format follows the extension
/// (`.json` → JSON, `.yml`/`.yaml` → YAML); for any other name it is sniffed
/// from the content, since JSON documents start with `{`.
fn parse_config_text(path: &Path, text: &str) -> Result<ConfigFile> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    let is_json = match ext.as_str() {
        "json" => true,
        "yml" | "yaml" => false,
        _ => text.trim_start().starts_with('{'),
    };
    let parsed = if is_json {
        serde_json::from_str(text).map_err(|e| e.to_string())
    } else {
        yaml_serde::from_str(text).map_err(|e| e.to_string())
    };
    parsed.map_err(|e| {
        crate::error::Error::Validation(format!("invalid config file {}: {e}", path.display()))
    })
}

/// Resolve a path from the config file against the directory the file lives
/// in, so the same config behaves identically from any working directory
/// (cron, a service, an interactive shell). Absolute paths are untouched.
fn resolve_config_path(base: &Path, value: String) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() || base.as_os_str().is_empty() {
        path
    } else {
        base.join(path)
    }
}

/// Split a `SCAT_SCAN_ROOTS` value into roots.
///
/// The format is `PATH`-style: roots are separated by `:`. A value without
/// any `:` is split on `,` instead, which is what earlier versions required;
/// a single root whose name contains a comma can therefore be written with a
/// trailing `:`.
fn split_scan_roots_env(value: &str) -> Vec<PathBuf> {
    let separator = if value.contains(':') { ':' } else { ',' };
    value
        .split(separator)
        .map(|p| PathBuf::from(p.trim()))
        .filter(|p| !p.as_os_str().is_empty())
        .collect()
}

/// Load the configuration from an optional file and environment overrides.
///
/// A config path that was given explicitly (`--config`, `SCAT_CONFIG`) must
/// exist: running on with defaults would silently lose `db_path`, bookmarks
/// and scan roots when, say, a network share is not mounted. Unknown keys are
/// errors too. Relative paths in the file are taken relative to the file's
/// own directory.
pub fn load_config(config_file: Option<&Path>) -> Result<Config> {
    let (file_data, base) = if let Some(path) = config_file {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(crate::error::Error::Validation(format!(
                    "config file not found: {}",
                    path.display()
                )));
            }
            Err(e) => {
                return Err(crate::error::Error::Validation(format!(
                    "cannot read config file {}: {e}",
                    path.display()
                )));
            }
        };
        let base = path.parent().unwrap_or(Path::new("")).to_path_buf();
        (parse_config_text(path, &text)?, base)
    } else {
        (ConfigFile::default(), PathBuf::new())
    };
    let rel = |value: String| resolve_config_path(&base, value);

    let db_path = file_data.db_path.map(rel);
    let cache_dir = file_data.cache_dir.map(rel);
    let embeddings_path = file_data.embeddings_path.map(rel);
    let scan_roots = match std::env::var("SCAT_SCAN_ROOTS").ok() {
        Some(s) => split_scan_roots_env(&s),
        None => file_data
            .scan_roots
            .unwrap_or_default()
            .into_iter()
            .map(rel)
            .collect(),
    };
    let ignore_patterns = file_data.ignore_patterns.unwrap_or_default();
    let vc = file_data.vc.unwrap_or_default();
    let tui = file_data.tui.unwrap_or_default();
    let timestamp_zone = match vc.timestamp_timezone.as_deref() {
        Some(value) => TimestampZone::parse(value).map_err(crate::error::Error::Validation)?,
        None => TimestampZone::default(),
    };

    let defaults = VcConfig::default();
    Ok(Config {
        catalog: CatalogConfig {
            db_path,
            cache_dir,
            embeddings_path,
        },
        index: IndexConfig {
            scan_roots,
            ignore_patterns,
        },
        vc: VcConfig {
            // A bare program name (`vc`) is a PATH lookup, not a relative path.
            vc_executable: vc.executable.map(|exe| {
                if exe.contains('/') {
                    rel(exe)
                } else {
                    PathBuf::from(exe)
                }
            }),
            develop_dirs: vc.develop_dirs.unwrap_or(defaults.develop_dirs),
            archive_dirs: vc.archive_dirs.unwrap_or(defaults.archive_dirs),
            manifest_path: vc.manifest_path.map(rel),
            timestamp_zone,
        },
        tui: TuiConfig {
            glyphs: tui.glyphs,
            colors: tui.colors,
        },
        bookmarks: file_data.bookmarks.unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Config loading
    // -----------------------------------------------------------------------

    fn write_config(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn explicit_missing_config_is_an_error_but_no_config_is_fine() {
        let dir = tempfile::tempdir().unwrap();
        let err = load_config(Some(&dir.path().join("nope.yaml")))
            .unwrap_err()
            .to_string();
        assert!(err.contains("config file not found"), "{err}");
        assert!(err.contains("nope.yaml"), "{err}");
        assert!(load_config(None).is_ok());
    }

    #[test]
    fn unknown_keys_are_rejected_at_every_level() {
        let dir = tempfile::tempdir().unwrap();
        for (body, key) in [
            ("scan_rootz: [/a]\n", "scan_rootz"),
            ("vc:\n  develop_dir: [X]\n", "develop_dir"),
            ("tui:\n  glyph: ascii\n", "glyph"),
        ] {
            let path = write_config(dir.path(), "c.yaml", body);
            let err = load_config(Some(&path)).unwrap_err().to_string();
            assert!(err.contains(key), "{err}");
            assert!(err.contains("c.yaml"), "{err}");
        }
    }

    #[test]
    fn format_follows_extension_and_falls_back_to_the_content() {
        let dir = tempfile::tempdir().unwrap();
        let yaml = "db_path: /x/c.sqlite\n";
        let json = "{\"db_path\": \"/x/c.sqlite\"}";
        for (name, body) in [
            ("a.yaml", yaml),
            ("a.yml", yaml),
            ("a.json", json),
            ("scat.conf", yaml),
            ("scat.conf", json),
            ("scat", yaml),
        ] {
            let path = write_config(dir.path(), name, body);
            let cfg = load_config(Some(&path)).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(
                cfg.catalog.db_path,
                Some(PathBuf::from("/x/c.sqlite")),
                "{name}"
            );
        }
    }

    #[test]
    fn relative_paths_resolve_against_the_config_files_directory() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(
            dir.path(),
            "c.yaml",
            "db_path: cat/c.sqlite\ncache_dir: /abs/cache\nscan_roots: [scripts, /abs/root]\n\
             vc:\n  executable: vc\n  manifest_path: m.lst\n",
        );
        let cfg = load_config(Some(&path)).unwrap();
        assert_eq!(cfg.catalog.db_path, Some(dir.path().join("cat/c.sqlite")));
        assert_eq!(cfg.catalog.cache_dir, Some(PathBuf::from("/abs/cache")));
        assert_eq!(
            cfg.index.scan_roots,
            [dir.path().join("scripts"), PathBuf::from("/abs/root")]
        );
        assert_eq!(cfg.vc.vc_executable, Some(PathBuf::from("vc")));
        assert_eq!(cfg.vc.manifest_path, Some(dir.path().join("m.lst")));
    }

    #[test]
    fn scan_roots_env_is_path_style_with_a_comma_fallback() {
        let roots = |s: &str| split_scan_roots_env(s);
        assert_eq!(roots("/a:/b"), [PathBuf::from("/a"), PathBuf::from("/b")]);
        assert_eq!(roots("/a/x,y:"), [PathBuf::from("/a/x,y")]);
        assert_eq!(roots("/a,/b"), [PathBuf::from("/a"), PathBuf::from("/b")]);
        assert_eq!(
            roots(" /a : ::/b "),
            [PathBuf::from("/a"), PathBuf::from("/b")]
        );
        assert!(roots("").is_empty());
    }

    #[test]
    fn timestamp_timezone_comes_from_the_vc_section_of_the_config() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_config(
            dir.path(),
            "c.yaml",
            "vc:\n  timestamp_timezone: \"+02:00\"\n",
        );
        let cfg = load_config(Some(&path)).unwrap();
        assert_eq!(
            cfg.vc.timestamp_zone,
            TimestampZone::parse("+02:00").unwrap()
        );
        assert_eq!(
            load_config(None).unwrap().vc.timestamp_zone,
            TimestampZone::Utc
        );

        let bad = write_config(
            dir.path(),
            "bad.yaml",
            "vc:\n  timestamp_timezone: Europe/Brussels\n",
        );
        let err = load_config(Some(&bad)).unwrap_err().to_string();
        assert!(err.contains("timestamp_timezone"), "{err}");
    }

    #[test]
    fn vc_is_configured_exactly_when_scan_roots_are_set() {
        assert!(!Config::default().vc_configured());
        let config = Config {
            index: IndexConfig {
                scan_roots: vec![PathBuf::from("/some/root")],
                ..IndexConfig::default()
            },
            ..Config::default()
        };
        assert!(config.vc_configured());
    }
}
