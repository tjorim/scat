use std::collections::HashSet;

use once_cell::sync::Lazy;
use regex::Regex;
use tree_sitter::Node;

use crate::error::{Error, Result};
use crate::indexer::ast_deps::{
    AstDependencies, extract_python_deps, extract_python_deps_with_module,
};

// ---------------------------------------------------------------------------
// Extractor
// ---------------------------------------------------------------------------

/// Tree-sitter based dependency extractor for Python and shell.
pub struct TreeSitterExtractor {
    python_parser: tree_sitter::Parser,
    bash_parser: tree_sitter::Parser,
}

impl TreeSitterExtractor {
    /// Construct a new extractor with Python and Bash grammars loaded.
    pub fn new() -> Result<Self> {
        let mut python_parser = tree_sitter::Parser::new();
        python_parser
            .set_language(&tree_sitter_python::LANGUAGE.into())
            .map_err(|e| Error::Mapping(e.to_string()))?;

        let mut bash_parser = tree_sitter::Parser::new();
        bash_parser
            .set_language(&tree_sitter_bash::LANGUAGE.into())
            .map_err(|e| Error::Mapping(e.to_string()))?;

        Ok(Self {
            python_parser,
            bash_parser,
        })
    }

    /// Extract dependencies for the given language (imports / sourced paths only).
    ///
    /// For Python: returns module-import names via AST. This is the quick imports-only
    /// path; use `extract_python_ast` when definitions and call edges are also needed
    /// (e.g. inside the build pipeline where the full AstDependencies is already computed).
    /// For shell: returns sourced file paths.
    pub fn extract_deps(&mut self, source: &str, language: &str) -> Vec<String> {
        let lang_key = normalise_lang(language);
        match lang_key {
            "python" => extract_python_deps(&mut self.python_parser, source).imports,
            "bash" => {
                let source_bytes = source.as_bytes();
                match self.bash_parser.parse(source_bytes, None) {
                    Some(tree) => {
                        let mut deps = Vec::new();
                        let mut seen = HashSet::new();
                        extract_bash_commands(tree.root_node(), source_bytes, &mut deps, &mut seen);
                        deps
                    }
                    None => extract_deps_fallback(source, language),
                }
            }
            _ => extract_deps_fallback(source, language),
        }
    }

    /// Extract full Python AST dependency payload (imports, defs, calls).
    pub fn extract_python_ast(
        &mut self,
        source: &str,
        module_name: Option<&str>,
    ) -> AstDependencies {
        extract_python_deps_with_module(&mut self.python_parser, source, module_name)
    }
}

// ---------------------------------------------------------------------------
// Bash tree walker
// ---------------------------------------------------------------------------

fn extract_bash_commands(
    node: Node<'_>,
    source: &[u8],
    deps: &mut Vec<String>,
    seen: &mut HashSet<String>,
) {
    if node.kind() == "command" {
        let mut cmd_name = String::new();
        let mut args: Vec<String> = Vec::new();

        for i in 0..node.child_count() {
            let child = match node.child(i as u32) {
                Some(c) => c,
                None => continue,
            };
            match child.kind() {
                "command_name" => {
                    cmd_name = child.utf8_text(source).unwrap_or("").trim().to_string();
                }
                "word" | "string" | "raw_string" | "ansi_c_string" | "concatenation" => {
                    let raw = child.utf8_text(source).unwrap_or("").trim();
                    let stripped = raw.trim_matches('"').trim_matches('\'');
                    args.push(stripped.to_string());
                }
                _ => {}
            }
        }

        if (cmd_name == "source" || cmd_name == ".") && !args.is_empty() {
            let path = &args[0];
            // Only bare-name sources (`source common.sh`) are emitted as import
            // edges, resolved by basename. Path-like sources
            // (`source ../lib/common.sh`) are left to the reference extractor so
            // they resolve correctly by path instead of via a spurious module
            // suffix (the file extension) — see extract_reference_paths.
            if !path.is_empty() && !path.contains('/') && !seen.contains(path) {
                seen.insert(path.clone());
                deps.push(path.clone());
            }
        }
    }

    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            extract_bash_commands(child, source, deps, seen);
        }
    }
}

// ---------------------------------------------------------------------------
// Regex fallback
// ---------------------------------------------------------------------------

static PYTHON_IMPORT_RE: Lazy<Vec<Regex>> = Lazy::new(|| {
    vec![
        Regex::new(r"(?m)^\s*import\s+([\w.]+)").unwrap(),
        Regex::new(r"(?m)^\s*from\s+([\w.]+)\s+import").unwrap(),
    ]
});

static BASH_SOURCE_RE: Lazy<Vec<Regex>> =
    Lazy::new(|| vec![Regex::new(r#"(?m)^\s*(?:source|\.)\s+["']?([^\s"';]+)["']?"#).unwrap()]);

static REFERENCE_PATH_RE: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"[\w./\\-]+\.(?:py|sh|bash|ksh)\b").unwrap());

/// Extract path-shaped string literals that point at other scripts.
///
/// This is language-agnostic: it scans the raw file text for tokens that look
/// like a path to a `.py`/`.sh`/`.bash`/`.ksh` file and contain a `/`
/// separator, regardless of whether they appear inside an `ssh`/`scp`/`rsync`
/// command, a `subprocess`/`paramiko` invocation, or a JSON/YAML manifest
/// list. These "called, not imported" edges are invisible to the AST-based
/// import extractors.
///
/// Extraction is deliberately liberal — precision comes from resolution: a
/// candidate is only kept as a dependency edge if it matches an indexed
/// script's logical path exactly (see `resolve_reference_targets`), so
/// unrelated path strings (logs, temp files) are discarded rather than stored.
pub fn extract_reference_paths(content: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for m in REFERENCE_PATH_RE.find_iter(content) {
        let candidate = m.as_str();
        // A bare basename (no separator) can never match a full logical path,
        // so drop it here to keep the candidate set small.
        if !candidate.contains('/') {
            continue;
        }
        if seen.insert(candidate.to_string()) {
            out.push(candidate.to_string());
        }
    }
    out
}

/// Regex fallback dependency extraction for Python/shell sources.
pub fn extract_deps_fallback(source: &str, language: &str) -> Vec<String> {
    let patterns: &[Regex] = match normalise_lang(language) {
        "python" => &PYTHON_IMPORT_RE,
        "bash" => &BASH_SOURCE_RE,
        _ => return vec![],
    };

    let mut seen = HashSet::new();
    let mut deps = Vec::new();
    for pat in patterns {
        for cap in pat.captures_iter(source) {
            let dep = cap[1].trim().to_string();
            // Path-like deps (bash `source ../x.sh`) are handled by the
            // reference extractor, not as import edges; Python import captures
            // never contain a separator, so this only filters bash source
            // paths — matching the tree-sitter extractor's bare-name rule.
            if dep.contains('/') {
                continue;
            }
            if !dep.is_empty() && !seen.contains(&dep) {
                seen.insert(dep.clone());
                deps.push(dep);
            }
        }
    }
    deps
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn normalise_lang(language: &str) -> &'static str {
    match language.to_lowercase().as_str() {
        "shell" => "bash",
        "python" => "python",
        _ => "unknown",
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_bare_name_source_command() {
        let mut ext = TreeSitterExtractor::new().unwrap();
        let deps = ext.extract_deps("source lib.sh\necho hello", "shell");
        assert!(deps.contains(&"lib.sh".to_string()));
        assert!(!deps.contains(&"hello".to_string()));
    }

    #[test]
    fn path_like_source_is_not_an_import_edge() {
        // Path-like sources are handled by the reference extractor, not as
        // import edges, so the module resolver never sees them.
        let mut ext = TreeSitterExtractor::new().unwrap();
        assert!(
            ext.extract_deps("source /path/to/lib.sh", "shell")
                .is_empty()
        );
        assert!(ext.extract_deps(". ./common.sh", "shell").is_empty());
    }

    #[test]
    fn ignores_other_commands() {
        let mut ext = TreeSitterExtractor::new().unwrap();
        let deps = ext.extract_deps("echo hello\nls -la", "shell");
        assert!(deps.is_empty());
    }

    #[test]
    fn fallback_python_matches_imports() {
        let deps = extract_deps_fallback("import os\nfrom pathlib import Path", "python");
        assert!(deps.contains(&"os".to_string()));
        assert!(deps.contains(&"pathlib".to_string()));
    }

    #[test]
    fn fallback_bash_matches_bare_source_only() {
        // Bare names become import edges; path-like sources are left to the
        // reference extractor, matching the tree-sitter extractor.
        let deps = extract_deps_fallback("source profile\n. ./lib.sh", "shell");
        assert!(deps.contains(&"profile".to_string()));
        assert!(!deps.contains(&"./lib.sh".to_string()));
    }

    #[test]
    fn reference_paths_from_ssh_and_scp() {
        let content = "scp /catalog/scripts/lib/deploy.py host:/tmp/\n\
                       ssh host python3 /catalog/scripts/jobs/nightly.py";
        let refs = extract_reference_paths(content);
        assert!(refs.contains(&"/catalog/scripts/lib/deploy.py".to_string()));
        assert!(refs.contains(&"/catalog/scripts/jobs/nightly.py".to_string()));
    }

    #[test]
    fn reference_paths_from_paramiko_and_json() {
        let python = "client.exec_command('/catalog/scripts/run.sh --flag')";
        let refs = extract_reference_paths(python);
        assert_eq!(refs, vec!["/catalog/scripts/run.sh".to_string()]);

        let manifest = r#"{"steps": ["/catalog/scripts/a.py", "/catalog/scripts/b.sh"]}"#;
        let refs = extract_reference_paths(manifest);
        assert!(refs.contains(&"/catalog/scripts/a.py".to_string()));
        assert!(refs.contains(&"/catalog/scripts/b.sh".to_string()));
    }

    #[test]
    fn reference_paths_ignore_bare_basenames_and_dedupe() {
        // No separator → cannot match a full logical path, so it is dropped.
        assert!(extract_reference_paths("run foo.py now").is_empty());
        // Repeated path collapses to a single candidate.
        let refs = extract_reference_paths("a/x.py then a/x.py again");
        assert_eq!(refs, vec!["a/x.py".to_string()]);
    }

    #[test]
    fn reference_paths_skip_unrelated_and_similar_extensions() {
        // .bashrc / .shell must not be captured as .bash / .sh.
        let refs = extract_reference_paths("source ~/.bashrc\nedit /etc/foo.shell");
        assert!(refs.is_empty(), "unexpected: {refs:?}");
    }
}
