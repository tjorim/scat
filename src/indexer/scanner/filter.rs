//! Ignore-file loading and scan-root boundary checks.

use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

pub(super) fn get_external_ignore_paths(ignore_files: &[PathBuf]) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();

    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        let global_ignore = PathBuf::from(home).join(".catignore");
        if global_ignore.is_file() {
            paths.push(global_ignore);
        }
    }

    for ignore_file in ignore_files {
        if !ignore_file.is_file() {
            return Err(Error::Validation(format!(
                "ignore file does not exist: {}",
                ignore_file.display()
            )));
        }
        paths.push(ignore_file.clone());
    }

    Ok(paths)
}

/// Canonicalize each root, dropping any that fail to resolve (e.g. a
/// misconfigured or momentarily-missing root) rather than failing the whole
/// scan over it.
pub(super) fn canonicalize_roots(roots: &[PathBuf]) -> Vec<PathBuf> {
    roots
        .iter()
        .filter_map(|r| std::fs::canonicalize(r).ok())
        .collect()
}

/// Whether `path` canonicalizes to somewhere inside one of `canonical_roots`.
/// Used to bound symlinked-directory traversal: a symlink pointing at another
/// location inside the scan roots (e.g. the documented `alt/pse → linux/pse`
/// OS-variant aliasing) is followed as before, but a symlink pointing outside
/// all configured roots — into an unrelated, potentially huge tree such as a
/// home directory or another mount — is not, since following it could turn a
/// bounded scan into an effectively unbounded one.
pub(super) fn is_within_roots(path: &Path, canonical_roots: &[PathBuf]) -> bool {
    match std::fs::canonicalize(path) {
        Ok(canon) => canonical_roots.iter().any(|r| canon.starts_with(r)),
        Err(_) => false,
    }
}
