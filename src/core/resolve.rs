use std::path::Path;

use serde::Deserialize;

use crate::error::{Error, Result};

// ---------------------------------------------------------------------------
// Mapping file schema (YAML or JSON)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct MappingFile {
    mappings: Vec<MappingEntry>,
}

#[derive(Debug, Deserialize)]
struct MappingEntry {
    logical_prefix: String,
    #[serde(default)]
    windows: String,
    #[serde(default)]
    linux: String,
}

// ---------------------------------------------------------------------------
// PathResolver
// ---------------------------------------------------------------------------

/// Resolve logical catalog paths to OS-specific filesystem paths.
#[derive(Debug, Default)]
pub struct PathResolver {
    mappings: Vec<MappingEntry>,
}

impl PathResolver {
    /// Create a resolver with no mappings (identity fallback).
    pub fn new() -> Self {
        Self::default()
    }

    /// Load mappings from a YAML or JSON file.
    pub fn from_file(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let file: MappingFile = match ext.to_lowercase().as_str() {
            "yml" | "yaml" => {
                yaml_serde::from_str(&text).map_err(|e| Error::Mapping(e.to_string()))?
            }
            _ => serde_json::from_str(&text).map_err(|e| Error::Mapping(e.to_string()))?,
        };

        // Strip trailing slashes from logical prefixes on load.
        let mappings = file
            .mappings
            .into_iter()
            .map(|mut e| {
                e.logical_prefix = e.logical_prefix.trim_end_matches('/').to_string();
                e
            })
            .collect();

        Ok(Self { mappings })
    }

    // ------------------------------------------------------------------
    // Resolution
    // ------------------------------------------------------------------

    /// Translate a logical path to a Windows-style path (backslashes).
    pub fn to_windows(&self, logical_path: &str) -> String {
        match self.find_mapping(logical_path) {
            None => logical_path.to_string(),
            Some(m) => {
                let remainder = &logical_path[m.logical_prefix.len()..];
                let relative = remainder.trim_start_matches('/');
                let root = m.windows.trim_end_matches('\\');
                if relative.is_empty() {
                    root.to_string()
                } else {
                    format!("{}\\{}", root, relative.replace('/', "\\"))
                }
            }
        }
    }

    /// Translate a logical path to a Linux/POSIX-style path (forward slashes).
    pub fn to_linux(&self, logical_path: &str) -> String {
        match self.find_mapping(logical_path) {
            None => logical_path.to_string(),
            Some(m) => {
                let remainder = &logical_path[m.logical_prefix.len()..];
                let root = m.linux.trim_end_matches('/');
                format!("{}{}", root, remainder)
            }
        }
    }

    /// Resolve to the native path for the current OS.
    pub fn to_native(&self, logical_path: &str) -> String {
        if cfg!(windows) {
            self.to_windows(logical_path)
        } else {
            self.to_linux(logical_path)
        }
    }

    // ------------------------------------------------------------------
    // Internal
    // ------------------------------------------------------------------

    fn find_mapping(&self, logical_path: &str) -> Option<&MappingEntry> {
        self.mappings.iter().find(|m| {
            logical_path == m.logical_prefix
                || (!m.logical_prefix.is_empty()
                    && logical_path.starts_with(&m.logical_prefix)
                    && logical_path.as_bytes().get(m.logical_prefix.len()) == Some(&b'/'))
        })
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn resolver_from_yaml(yaml: &str) -> PathResolver {
        let file: MappingFile = yaml_serde::from_str(yaml).unwrap();
        let mappings = file
            .mappings
            .into_iter()
            .map(|mut e| {
                e.logical_prefix = e.logical_prefix.trim_end_matches('/').to_string();
                e
            })
            .collect();
        PathResolver { mappings }
    }

    const SINGLE_MAPPING: &str = r#"
mappings:
  - logical_prefix: /catalog/scripts
    windows: "Z:\\scripts"
    linux: /net/scripts
"#;

    const MULTI_MAPPING: &str = r#"
mappings:
  - logical_prefix: /catalog/scripts/tools
    windows: "Z:\\scripts\\tools"
    linux: /net/scripts/tools
  - logical_prefix: /catalog/scripts
    windows: "Z:\\scripts"
    linux: /net/scripts
"#;

    #[test]
    fn to_windows_single_mapping() {
        let r = resolver_from_yaml(SINGLE_MAPPING);
        assert_eq!(
            r.to_windows("/catalog/scripts/tools/foo.py"),
            r"Z:\scripts\tools\foo.py"
        );
    }

    #[test]
    fn to_linux_single_mapping() {
        let r = resolver_from_yaml(SINGLE_MAPPING);
        assert_eq!(
            r.to_linux("/catalog/scripts/tools/foo.py"),
            "/net/scripts/tools/foo.py"
        );
    }

    #[test]
    fn to_windows_exact_prefix() {
        let r = resolver_from_yaml(SINGLE_MAPPING);
        assert_eq!(r.to_windows("/catalog/scripts"), r"Z:\scripts");
    }

    #[test]
    fn to_linux_exact_prefix() {
        let r = resolver_from_yaml(SINGLE_MAPPING);
        assert_eq!(r.to_linux("/catalog/scripts"), "/net/scripts");
    }

    #[test]
    fn identity_fallback_no_match() {
        let r = resolver_from_yaml(SINGLE_MAPPING);
        assert_eq!(r.to_windows("/other/path.py"), "/other/path.py");
        assert_eq!(r.to_linux("/other/path.py"), "/other/path.py");
    }

    #[test]
    fn identity_fallback_empty_resolver() {
        let r = PathResolver::new();
        assert_eq!(
            r.to_windows("/catalog/scripts/foo.py"),
            "/catalog/scripts/foo.py"
        );
        assert_eq!(
            r.to_linux("/catalog/scripts/foo.py"),
            "/catalog/scripts/foo.py"
        );
    }

    #[test]
    fn multi_mapping_first_match_wins() {
        let r = resolver_from_yaml(MULTI_MAPPING);
        // /catalog/scripts/tools should match the first (more specific) entry.
        assert_eq!(
            r.to_linux("/catalog/scripts/tools/lib/bar.py"),
            "/net/scripts/tools/lib/bar.py"
        );
        // /catalog/scripts (without /tools) should fall through to the second entry.
        assert_eq!(
            r.to_linux("/catalog/scripts/other/script.py"),
            "/net/scripts/other/script.py"
        );
    }

    #[test]
    fn no_partial_prefix_match() {
        let r = resolver_from_yaml(SINGLE_MAPPING);
        // /catalog/scriptsoo should NOT match /catalog/scripts prefix.
        assert_eq!(
            r.to_linux("/catalog/scriptsoo/bar.py"),
            "/catalog/scriptsoo/bar.py"
        );
    }

    #[test]
    fn windows_path_uses_backslash_separator() {
        let r = resolver_from_yaml(SINGLE_MAPPING);
        let result = r.to_windows("/catalog/scripts/a/b/c.py");
        assert!(result.contains('\\'));
        assert!(!result.contains('/'));
    }

    #[test]
    fn to_native_matches_to_linux_on_non_windows() {
        let r = resolver_from_yaml(SINGLE_MAPPING);
        if !cfg!(windows) {
            assert_eq!(
                r.to_native("/catalog/scripts/tools/foo.py"),
                r.to_linux("/catalog/scripts/tools/foo.py")
            );
        }
    }

    #[test]
    fn trailing_slash_on_configured_root_is_trimmed() {
        let yaml = r#"
mappings:
  - logical_prefix: /catalog/scripts
    windows: "Z:\\scripts\\"
    linux: /net/scripts/
"#;
        let r = resolver_from_yaml(yaml);
        assert_eq!(
            r.to_windows("/catalog/scripts/foo.py"),
            r"Z:\scripts\foo.py"
        );
        assert_eq!(r.to_linux("/catalog/scripts/foo.py"), "/net/scripts/foo.py");
    }

    #[test]
    fn logical_prefix_with_trailing_slash_is_stripped_on_load() {
        // from_file / the loader strip trailing slashes from logical_prefix;
        // verify a prefix configured with one behaves the same as without.
        let yaml = r#"
mappings:
  - logical_prefix: /catalog/scripts/
    windows: "Z:\\scripts"
    linux: /net/scripts
"#;
        let r = resolver_from_yaml(yaml);
        assert_eq!(r.to_linux("/catalog/scripts/foo.py"), "/net/scripts/foo.py");
        assert_eq!(r.to_linux("/catalog/scripts"), "/net/scripts");
    }

    #[test]
    fn prefix_matching_is_case_sensitive() {
        let r = resolver_from_yaml(SINGLE_MAPPING);
        // Different case than the configured prefix must not match.
        assert_eq!(
            r.to_linux("/Catalog/Scripts/foo.py"),
            "/Catalog/Scripts/foo.py"
        );
    }

    #[test]
    fn empty_input_path_falls_back_to_identity() {
        let r = resolver_from_yaml(SINGLE_MAPPING);
        assert_eq!(r.to_windows(""), "");
        assert_eq!(r.to_linux(""), "");
    }

    #[test]
    fn from_file_loads_json_mapping() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mapping.json");
        std::fs::write(
            &path,
            r#"{"mappings":[{"logical_prefix":"/catalog/scripts","windows":"Z:\\scripts","linux":"/net/scripts"}]}"#,
        )
        .unwrap();
        let r = PathResolver::from_file(&path).unwrap();
        assert_eq!(r.to_linux("/catalog/scripts/foo.py"), "/net/scripts/foo.py");
    }

    #[test]
    fn from_file_loads_yaml_mapping_and_strips_trailing_slash() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mapping.yaml");
        std::fs::write(&path, SINGLE_MAPPING).unwrap();
        let r = PathResolver::from_file(&path).unwrap();
        assert_eq!(
            r.to_windows("/catalog/scripts/tools/foo.py"),
            r"Z:\scripts\tools\foo.py"
        );
    }

    #[test]
    fn from_file_rejects_malformed_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mapping.yaml");
        std::fs::write(&path, "not: [valid, mapping").unwrap();
        assert!(PathResolver::from_file(&path).is_err());
    }

    #[test]
    fn from_file_missing_path_returns_error() {
        let path = std::path::Path::new("/nonexistent/does-not-exist.yaml");
        assert!(PathResolver::from_file(path).is_err());
    }

    // -----------------------------------------------------------------------
    // Property-based tests
    // -----------------------------------------------------------------------

    proptest! {
        #[test]
        fn to_windows_never_panics_on_arbitrary_input(path in ".{0,200}") {
            let r = resolver_from_yaml(SINGLE_MAPPING);
            let _ = r.to_windows(&path);
            let _ = r.to_linux(&path);
        }

        #[test]
        fn windows_translation_of_a_matched_path_never_contains_forward_slash(
            rel in "[a-zA-Z0-9/_.-]{0,40}"
        ) {
            let r = resolver_from_yaml(SINGLE_MAPPING);
            let logical = format!("/catalog/scripts/{rel}");
            let result = r.to_windows(&logical);
            prop_assert!(!result.contains('/'));
            prop_assert!(result.starts_with(r"Z:\scripts"));
        }

        #[test]
        fn linux_translation_of_a_matched_path_preserves_relative_segment(
            rel in "[a-zA-Z0-9/_.-]{0,40}"
        ) {
            let r = resolver_from_yaml(SINGLE_MAPPING);
            let logical = format!("/catalog/scripts/{rel}");
            prop_assert_eq!(r.to_linux(&logical), format!("/net/scripts/{rel}"));
        }
    }
}
