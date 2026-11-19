//! Fingerprint of everything that determines a catalog's content besides the
//! script files themselves.
//!
//! A catalog is a function of the files under the scan roots *and* of the
//! settings and code that read them. File mtimes only cover the first part, so
//! each completed build records this fingerprint (in
//! `index_metadata.manifest_fingerprint`, the column that originally held just
//! the vc manifest's fingerprint) and later runs compare it:
//!
//! - `catalog build` skips the rebuild only when the fingerprint is unchanged
//!   (and no file or directory changed);
//! - an incremental build reuses rows of unchanged scripts only when the
//!   *extraction* half is unchanged, so improvements to the extractors reach
//!   scripts that were not edited;
//! - a resumed build only continues a checkpoint made with the same inputs.

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::core::db::SCHEMA_VERSION;
use crate::core::vc::{VcConfig, manifest_fingerprint};

/// Bump when extraction output changes for unchanged inputs without a version
/// bump of the crate (a new header keyword, a dependency-resolution fix, …),
/// so unchanged scripts are re-extracted by the next build.
pub const EXTRACTOR_REVISION: u32 = 2;

/// The `head_lines` a build uses when asked for 0.
const DEFAULT_HEAD_LINES: usize = 10;

/// What a build depends on, in two parts. Rendered as `<extraction>.<inputs>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildFingerprint {
    /// Depends on how a *single script* is read: scat version, extractor
    /// revision, schema and header lines. When it differs, stored rows are
    /// stale even for scripts nobody touched.
    pub extraction: String,
    /// Covers `extraction` plus what is scanned and recorded around it: scan
    /// roots, ignore rules, checkout directory names and the vc manifest.
    pub inputs: String,
}

impl BuildFingerprint {
    /// Compute the fingerprint of a build with these settings.
    ///
    /// `scan_roots` must be the roots actually scanned (the command-line ones,
    /// else the configured ones) and `ignore_files` every ignore file passed to
    /// the scanner; their *contents* count, not their paths, so a temporary
    /// file holding the configured patterns hashes the same on every run.
    pub fn compute(
        scan_roots: &[PathBuf],
        head_lines: usize,
        ignore_files: &[PathBuf],
        vc_config: &VcConfig,
    ) -> Self {
        let head_lines = if head_lines == 0 {
            DEFAULT_HEAD_LINES
        } else {
            head_lines
        };
        let mut extraction = Sha256::new();
        field(&mut extraction, env!("CARGO_PKG_VERSION").as_bytes());
        field(&mut extraction, &EXTRACTOR_REVISION.to_le_bytes());
        field(&mut extraction, &SCHEMA_VERSION.to_le_bytes());
        field(&mut extraction, &head_lines.to_le_bytes());
        let extraction = hex::encode(extraction.finalize());

        let mut inputs = Sha256::new();
        field(&mut inputs, extraction.as_bytes());
        for root in scan_roots {
            field(&mut inputs, root.as_os_str().as_encoded_bytes());
        }
        field(&mut inputs, b"--ignore-files");
        // The global `~/.catignore` is read by the scanner too.
        let global = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(|home| Path::new(&home).join(".catignore"));
        for path in global.iter().chain(ignore_files) {
            match std::fs::read(path) {
                Ok(bytes) => field(&mut inputs, &bytes),
                Err(_) => field(&mut inputs, b"<unreadable>"),
            }
        }
        field(&mut inputs, b"--checkout-dirs");
        for dir in vc_config.all_checkout_dirs() {
            field(&mut inputs, dir.as_bytes());
        }
        // Only when set, so catalogs built without overrides keep their
        // fingerprint across the upgrade that introduced the setting.
        if !vc_config.os_by_root.is_empty() {
            field(&mut inputs, b"--os-by-root");
            for (root, os) in &vc_config.os_by_root {
                field(&mut inputs, root.as_os_str().as_encoded_bytes());
                field(&mut inputs, os.as_bytes());
            }
        }
        field(
            &mut inputs,
            manifest_fingerprint(vc_config.manifest_path.as_deref()).as_bytes(),
        );
        Self {
            extraction,
            inputs: hex::encode(inputs.finalize()),
        }
    }

    /// Parse the stored form. `None` for a value written by an older scat
    /// (a bare manifest fingerprint), which therefore never matches.
    pub fn parse(stored: &str) -> Option<Self> {
        let (extraction, inputs) = stored.split_once('.')?;
        (!extraction.is_empty() && !inputs.is_empty()).then(|| Self {
            extraction: extraction.to_string(),
            inputs: inputs.to_string(),
        })
    }
}

impl std::fmt::Display for BuildFingerprint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.extraction, self.inputs)
    }
}

/// Hash one length-prefixed field, so `("ab", "c")` and `("a", "bc")` differ.
fn field(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compute(roots: &[&str], head: usize, ignore: &[PathBuf], vc: &VcConfig) -> BuildFingerprint {
        let roots: Vec<PathBuf> = roots.iter().map(PathBuf::from).collect();
        BuildFingerprint::compute(&roots, head, ignore, vc)
    }

    #[test]
    fn same_inputs_give_the_same_fingerprint_and_round_trip() {
        let vc = VcConfig::default();
        let a = compute(&["/r"], 10, &[], &vc);
        assert_eq!(a, compute(&["/r"], 10, &[], &vc));
        assert_eq!(BuildFingerprint::parse(&a.to_string()), Some(a));
        assert_eq!(BuildFingerprint::parse("0123abcd"), None, "old format");
        assert_eq!(
            compute(&["/r"], 0, &[], &vc),
            compute(&["/r"], 10, &[], &vc)
        );
    }

    #[test]
    fn every_input_changes_the_inputs_half() {
        let vc = VcConfig::default();
        let base = compute(&["/r"], 10, &[], &vc);
        assert_ne!(base.inputs, compute(&["/r", "/s"], 10, &[], &vc).inputs);
        assert_ne!(base.inputs, compute(&["/s"], 10, &[], &vc).inputs);

        let dir = tempfile::tempdir().unwrap();
        let ignore = dir.path().join("ignore");
        std::fs::write(&ignore, "a.py\n").unwrap();
        let with_ignore = compute(&["/r"], 10, std::slice::from_ref(&ignore), &vc);
        assert_ne!(base.inputs, with_ignore.inputs);
        std::fs::write(&ignore, "b.py\n").unwrap();
        assert_ne!(
            with_ignore.inputs,
            compute(&["/r"], 10, std::slice::from_ref(&ignore), &vc).inputs,
            "ignore file contents count"
        );

        let custom = VcConfig {
            develop_dirs: vec!["WORK".into()],
            ..VcConfig::default()
        };
        assert_ne!(base.inputs, compute(&["/r"], 10, &[], &custom).inputs);
        let named = VcConfig {
            os_by_root: [(PathBuf::from("/r"), "linux".to_string())].into(),
            ..VcConfig::default()
        };
        assert_ne!(base.inputs, compute(&["/r"], 10, &[], &named).inputs);
        // None of these touch how a single script is extracted.
        assert_eq!(
            base.extraction,
            compute(&["/s"], 10, &[], &custom).extraction
        );
        assert_eq!(
            base.extraction,
            compute(&["/r"], 10, &[], &named).extraction
        );
    }

    #[test]
    fn head_lines_change_the_extraction_half() {
        let vc = VcConfig::default();
        assert_ne!(
            compute(&["/r"], 10, &[], &vc).extraction,
            compute(&["/r"], 20, &[], &vc).extraction
        );
    }
}
