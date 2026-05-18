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
            if !path.is_empty() && !seen.contains(path) {
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
    fn extracts_source_command() {
        let mut ext = TreeSitterExtractor::new().unwrap();
        let deps = ext.extract_deps("source /path/to/lib.sh\necho hello", "shell");
        assert!(deps.contains(&"/path/to/lib.sh".to_string()));
        assert!(!deps.contains(&"hello".to_string()));
    }

    #[test]
    fn extracts_dot_command() {
        let mut ext = TreeSitterExtractor::new().unwrap();
        let deps = ext.extract_deps(". ./common.sh", "shell");
        assert!(deps.contains(&"./common.sh".to_string()));
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
    fn fallback_bash_matches_source() {
        let deps = extract_deps_fallback("source /etc/profile\n. ./lib.sh", "shell");
        assert!(deps.contains(&"/etc/profile".to_string()));
        assert!(deps.contains(&"./lib.sh".to_string()));
    }
}
