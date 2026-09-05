use std::collections::HashSet;

use regex::Regex;
use tree_sitter::Node;

use crate::error::{Error, Result};
use crate::indexer::ast_deps::{
    AstDependencies, extract_python_deps, extract_python_deps_with_module,
};

// ---------------------------------------------------------------------------
// Extractor
// ---------------------------------------------------------------------------

/// Upper bound on how long a single-file parse may run before it is aborted.
/// Pathological input (e.g. a minified/generated file with extreme nesting)
/// can otherwise take a very long time to parse, stalling the whole indexing
/// run on one file; on timeout the parse returns `None` and callers fall
/// back to the regex-based extractor.
const PARSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Parse `source` with a wall-clock deadline, returning `None` if parsing
/// doesn't finish in time (in addition to the normal `None`-on-no-language
/// case). Shared by both the bash extraction below, the Python AST
/// extractor in [`crate::indexer::ast_deps`], and the TUI's syntax
/// highlighter (`src/tui/highlight.rs`, in the separate `scat` binary
/// crate — hence `pub` rather than `pub(crate)`).
pub fn parse_with_timeout(
    parser: &mut tree_sitter::Parser,
    source: &[u8],
) -> Option<tree_sitter::Tree> {
    let deadline = std::time::Instant::now() + PARSE_TIMEOUT;
    let mut progress = |_state: &tree_sitter::ParseState| {
        if std::time::Instant::now() >= deadline {
            std::ops::ControlFlow::Break(())
        } else {
            std::ops::ControlFlow::Continue(())
        }
    };
    parser.parse_with_options(
        &mut |i, _| source.get(i..).unwrap_or_default(),
        None,
        Some(tree_sitter::ParseOptions::new().progress_callback(&mut progress)),
    )
}

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
                match parse_with_timeout(&mut self.bash_parser, source_bytes) {
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

// Iterative (not recursive) pre-order walk, navigated entirely through
// TreeCursor with no Vec allocation at all: a deeply nested script (many
// levels of `if`/command substitution) could otherwise overflow the native
// stack if this recursed, since recursion depth would track AST nesting
// depth directly; and `Node::child(i)`/collecting children into a Vec per
// node costs allocation + O(log i) lookups that a plain cursor walk
// (goto_first_child / goto_next_sibling / goto_parent) avoids entirely.
fn extract_bash_commands(
    root: Node<'_>,
    source: &[u8],
    deps: &mut Vec<String>,
    seen: &mut HashSet<String>,
) {
    let mut cursor = root.walk();
    let mut reached_root = false;
    while !reached_root {
        let node = cursor.node();

        if node.kind() == "command" {
            let mut cmd_name = String::new();
            // Only the first "word"-like child is ever used (matches a
            // `source <first-arg>` invocation), so track just that instead
            // of collecting every argument into a Vec.
            let mut first_arg: Option<String> = None;

            let mut child_cursor = node.walk();
            for child in node.children(&mut child_cursor) {
                match child.kind() {
                    "command_name" => {
                        cmd_name = child.utf8_text(source).unwrap_or("").trim().to_string();
                    }
                    "word" | "string" | "raw_string" | "ansi_c_string" | "concatenation"
                        if first_arg.is_none() =>
                    {
                        let raw = child.utf8_text(source).unwrap_or("").trim();
                        first_arg = Some(raw.trim_matches('"').trim_matches('\'').to_string());
                    }
                    _ => {}
                }
            }

            if (cmd_name == "source" || cmd_name == ".")
                && let Some(path) = first_arg
            {
                // Only bare-name sources (`source common.sh`) are emitted as import
                // edges, resolved by basename. Path-like sources
                // (`source ../lib/common.sh`) are left to the reference extractor so
                // they resolve correctly by path instead of via a spurious module
                // suffix (the file extension) — see extract_reference_paths.
                if !path.is_empty() && !path.contains('/') && !seen.contains(&path) {
                    seen.insert(path.clone());
                    deps.push(path);
                }
            }
        }

        if cursor.goto_first_child() {
            continue;
        }
        if cursor.goto_next_sibling() {
            continue;
        }
        // Backtrack until a next sibling is found, or we've climbed back to
        // (not past) `root` — bounds the walk to root's own subtree.
        loop {
            if !cursor.goto_parent() {
                reached_root = true;
                break;
            }
            if cursor.node() == root {
                reached_root = true;
                break;
            }
            if cursor.goto_next_sibling() {
                break;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Regex fallback
// ---------------------------------------------------------------------------

static PYTHON_IMPORT_RE: std::sync::LazyLock<Vec<Regex>> = std::sync::LazyLock::new(|| {
    vec![
        Regex::new(r"(?m)^\s*import\s+([\w.]+)").unwrap(),
        Regex::new(r"(?m)^\s*from\s+([\w.]+)\s+import").unwrap(),
    ]
});

static BASH_SOURCE_RE: std::sync::LazyLock<Vec<Regex>> = std::sync::LazyLock::new(|| {
    vec![Regex::new(r#"(?m)^\s*(?:source|\.)\s+["']?([^\s"';]+)["']?"#).unwrap()]
});

static REFERENCE_PATH_RE: std::sync::LazyLock<Regex> =
    std::sync::LazyLock::new(|| Regex::new(r"[\w./\\-]+\.(?:py|sh|bash|ksh)\b").unwrap());

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
        // Normalise Windows separators up front so a back-slash reference
        // (`..\lib\common.py`) is captured and resolved the same as a
        // forward-slash one — logical paths are always `/`-separated.
        let candidate = m.as_str().replace('\\', "/");
        // A bare basename (no separator) can never match a full logical path,
        // so drop it here to keep the candidate set small.
        if !candidate.contains('/') {
            continue;
        }
        if seen.insert(candidate.clone()) {
            out.push(candidate);
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

/// Map a `scripts.language` column value to the tree-sitter grammar key
/// that identifies it here and in the TUI's syntax highlighter
/// (`src/tui/highlight.rs`, in the separate `scat` binary crate — hence
/// `pub` rather than `pub(crate)`).
pub fn normalise_lang(language: &str) -> &'static str {
    match language.to_lowercase().as_str() {
        "shell" => "bash",
        "python" => "python",
        "json" => "json",
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
    fn reference_paths_normalise_windows_backslashes() {
        // A pure-backslash reference is captured and normalised to `/` so it
        // resolves the same as a forward-slash path.
        let refs = extract_reference_paths("call ..\\lib\\common.py now");
        assert_eq!(refs, vec!["../lib/common.py".to_string()]);
        // Mixed separators also normalise, and dedupe against the `/` form.
        let refs = extract_reference_paths("a\\b\\c.sh and a/b/c.sh");
        assert_eq!(refs, vec!["a/b/c.sh".to_string()]);
    }

    #[test]
    fn reference_paths_skip_unrelated_and_similar_extensions() {
        // .bashrc / .shell must not be captured as .bash / .sh.
        let refs = extract_reference_paths("source ~/.bashrc\nedit /etc/foo.shell");
        assert!(refs.is_empty(), "unexpected: {refs:?}");
    }

    #[test]
    fn normalise_lang_is_case_insensitive_with_unknown_fallback() {
        assert_eq!(normalise_lang("Shell"), "bash");
        assert_eq!(normalise_lang("PYTHON"), "python");
        assert_eq!(normalise_lang("JSON"), "json");
        assert_eq!(normalise_lang("perl"), "unknown");
        assert_eq!(normalise_lang(""), "unknown");
    }

    #[test]
    fn extract_deps_unknown_language_returns_empty() {
        let mut ext = TreeSitterExtractor::new().unwrap();
        assert!(ext.extract_deps("import os", "ruby").is_empty());
    }

    #[test]
    fn extract_python_ast_returns_full_dependency_payload() {
        let mut ext = TreeSitterExtractor::new().unwrap();
        let deps = ext.extract_python_ast("import os\ndef f():\n    pass\nf()\n", Some("pkg.m"));
        assert!(deps.imports.contains(&"os".to_string()));
        assert_eq!(deps.definitions.len(), 1);
        assert_eq!(deps.calls.len(), 1);
    }

    #[test]
    fn reference_paths_do_not_match_uppercase_extension() {
        // The regex is case-sensitive: `.PY` must not be treated as `.py`.
        let refs = extract_reference_paths("run a/b/SCRIPT.PY now");
        assert!(refs.is_empty(), "unexpected: {refs:?}");
    }

    #[test]
    fn reference_paths_stop_at_trailing_punctuation() {
        let refs = extract_reference_paths("see /catalog/scripts/run.py, then done.");
        assert_eq!(refs, vec!["/catalog/scripts/run.py".to_string()]);
    }

    #[test]
    fn fallback_extract_deps_unknown_language_returns_empty() {
        assert!(extract_deps_fallback("import os", "ruby").is_empty());
    }

    #[test]
    fn empty_source_does_not_panic_either_extractor() {
        let mut ext = TreeSitterExtractor::new().unwrap();
        assert!(ext.extract_deps("", "python").is_empty());
        assert!(ext.extract_deps("", "shell").is_empty());
        assert!(extract_reference_paths("").is_empty());
    }

    #[test]
    fn extract_deps_handles_deeply_nested_script_without_stack_overflow() {
        // The command walk used to be recursive, tracking AST nesting depth
        // 1:1 with native stack depth. A deeply nested script would overflow
        // the stack; the iterative walk must handle this without crashing.
        let mut source = String::new();
        for _ in 0..20_000 {
            source.push_str("if true; then ");
        }
        source.push_str("source lib.sh");
        for _ in 0..20_000 {
            source.push_str("; fi");
        }

        let mut ext = TreeSitterExtractor::new().unwrap();
        let deps = ext.extract_deps(&source, "shell");
        assert!(deps.contains(&"lib.sh".to_string()));
    }
}
