//! Language detection from file extension and shebang.

use std::path::Path;

use regex::Regex;

/// Script file extensions the scanner considers indexable.
pub(super) const SCRIPT_EXTENSIONS: &[&str] = &[
    ".py", ".sh", ".bash", ".ksh", ".yml", ".yaml", ".csv", ".json",
];

/// Detect language from a file extension.
pub fn detect_language(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_lowercase)
        .as_deref()
    {
        Some("py") => "python",
        Some("sh" | "bash" | "ksh") => "shell",
        Some("yml" | "yaml") => "yaml",
        Some("csv") => "csv",
        Some("json") => "json",
        _ => "unknown",
    }
}

static SHEBANG_RE: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new(r"^#!\s*(\S+)(?:\s+(\S+))?").unwrap());

/// Infer language from shebang line.
pub fn shebang_language(first_line: &str) -> Option<&'static str> {
    let m = SHEBANG_RE.captures(first_line)?;
    let cmd = Path::new(m.get(1)?.as_str()).file_name()?.to_str()?;
    let interpreter = if cmd == "env" {
        m.get(2)
            .and_then(|g| Path::new(g.as_str()).file_name()?.to_str())?
    } else {
        cmd
    };
    match interpreter {
        "sh" | "bash" | "ksh" | "ksh93" => Some("shell"),
        "python" | "python2" | "python3" => Some("python"),
        _ => None,
    }
}

/// Read up to `n` lines from the start of `path`.
pub fn read_head(path: &Path, n: usize) -> Vec<String> {
    use std::io::{BufRead, BufReader};
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return vec![],
    };
    let reader = BufReader::new(file);
    let mut lines = Vec::with_capacity(n);
    for line in reader.lines().take(n) {
        match line {
            Ok(l) => lines.push(l),
            Err(_) => break,
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_python_from_extension() {
        assert_eq!(detect_language(Path::new("foo.py")), "python");
        assert_eq!(detect_language(Path::new("FOO.PY")), "python");
    }

    #[test]
    fn detects_shell_from_extension() {
        assert_eq!(detect_language(Path::new("foo.sh")), "shell");
        assert_eq!(detect_language(Path::new("foo.bash")), "shell");
        assert_eq!(detect_language(Path::new("foo.ksh")), "shell");
    }

    #[test]
    fn unknown_for_unsupported() {
        assert_eq!(detect_language(Path::new("foo.rb")), "unknown");
        assert_eq!(detect_language(Path::new("foo")), "unknown");
    }

    #[test]
    fn shebang_direct_bash() {
        assert_eq!(shebang_language("#!/bin/bash"), Some("shell"));
    }

    #[test]
    fn shebang_direct_sh() {
        // How the extensionless shell tools vc manages are detected at all:
        // with no extension to go on, the shebang is the only language signal.
        assert_eq!(shebang_language("#!/bin/sh"), Some("shell"));
        assert_eq!(shebang_language("#! /bin/sh"), Some("shell"));
        assert_eq!(shebang_language("#!/usr/bin/env sh"), Some("shell"));
    }

    #[test]
    fn shebang_env_python3() {
        assert_eq!(shebang_language("#!/usr/bin/env python3"), Some("python"));
    }

    #[test]
    fn shebang_unknown() {
        assert_eq!(shebang_language("#!/usr/bin/env ruby"), None);
        assert_eq!(shebang_language("not a shebang"), None);
    }
}
