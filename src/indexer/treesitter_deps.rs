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
///
/// `pub` (rather than `pub(crate)`) so the TUI's syntax highlighter
/// (`src/tui/highlight.rs`, in the separate `scat` binary crate) can bound
/// `tree-sitter-highlight`'s own reparse under the same deadline.
pub const PARSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

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
    yaml_parser: tree_sitter::Parser,
}

impl TreeSitterExtractor {
    /// Construct a new extractor with Python, Bash and YAML grammars loaded.
    pub fn new() -> Result<Self> {
        let mut python_parser = tree_sitter::Parser::new();
        python_parser
            .set_language(&tree_sitter_python::LANGUAGE.into())
            .map_err(|e| Error::Mapping(e.to_string()))?;

        let mut bash_parser = tree_sitter::Parser::new();
        bash_parser
            .set_language(&tree_sitter_bash::LANGUAGE.into())
            .map_err(|e| Error::Mapping(e.to_string()))?;

        let mut yaml_parser = tree_sitter::Parser::new();
        yaml_parser
            .set_language(&tree_sitter_yaml::LANGUAGE.into())
            .map_err(|e| Error::Mapping(e.to_string()))?;

        Ok(Self {
            python_parser,
            bash_parser,
            yaml_parser,
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

    /// Extract Ansible-style path references from YAML content (see
    /// [`extract_yaml_reference_paths`]). These are path-shaped, not
    /// module-shaped, so callers must fold them into the "referenced"
    /// dependency kind — resolved by [`extract_reference_paths`]'s own
    /// path-resolution passes — rather than treating them as import edges.
    pub fn extract_yaml_deps(&mut self, source: &str) -> Vec<String> {
        let source_bytes = source.as_bytes();
        match parse_with_timeout(&mut self.yaml_parser, source_bytes) {
            Some(tree) => extract_yaml_reference_paths(tree.root_node(), source_bytes),
            None => vec![],
        }
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
// YAML (Ansible) tree walker
// ---------------------------------------------------------------------------

/// How a recognized Ansible reference key's value should be read.
struct AnsibleKeyRule {
    /// `roles`/`include_role` entries are frequently a bare Galaxy role name
    /// rather than a path (e.g. `roles: [common]`), so only path-shaped
    /// entries (containing `/`) are kept for those — matching the issue's
    /// "path-style entries, not Galaxy role names". The other keys always
    /// point at a file, so any scalar value is a candidate.
    path_only: bool,
    /// When a value (or list item) is a mapping — `include_tasks: {file:
    /// ..., apply: {...}}`, `roles: [{role: ..., vars: {...}}]` — only this
    /// one field actually carries a file/role reference; an unrelated
    /// sibling option must not become a candidate even if its value happens
    /// to look like a path.
    reference_field: Option<&'static str>,
}

/// If `raw_key` (stripped of any FQCN module prefix, e.g. `ansible.builtin.`)
/// is a recognized Ansible file-reference key, returns how to read its value.
fn ansible_key_rule(raw_key: &str) -> Option<AnsibleKeyRule> {
    let bare = raw_key.rsplit('.').next().unwrap_or(raw_key);
    match bare {
        "import_playbook" | "vars_files" => Some(AnsibleKeyRule {
            path_only: false,
            reference_field: None,
        }),
        "include_tasks" | "import_tasks" | "include_vars" => Some(AnsibleKeyRule {
            path_only: false,
            reference_field: Some("file"),
        }),
        "include_role" => Some(AnsibleKeyRule {
            path_only: true,
            reference_field: Some("name"),
        }),
        "roles" => Some(AnsibleKeyRule {
            path_only: true,
            reference_field: Some("role"),
        }),
        _ => None,
    }
}

/// Descend through wrapper nodes (`flow_node`/`block_node`) to the first
/// scalar leaf's text — used for reading a mapping key, which is always a
/// single scalar. Iterative (heap-allocated stack, not native recursion) so
/// a pathologically deep value can't overflow the stack — see
/// `collect_ansible_pairs`.
fn first_scalar_text(node: Node<'_>, source: &[u8]) -> Option<String> {
    let mut stack = vec![node];
    while let Some(node) = stack.pop() {
        if let Some(text) = leaf_scalar_text(node, source) {
            return Some(text);
        }
        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
    None
}

/// Text of a scalar leaf node, with quotes stripped for quoted forms.
/// Returns `None` for non-leaf (structural) nodes.
fn leaf_scalar_text(node: Node<'_>, source: &[u8]) -> Option<String> {
    match node.kind() {
        "plain_scalar" | "block_scalar" => Some(node.utf8_text(source).ok()?.trim().to_string()),
        "single_quote_scalar" | "double_quote_scalar" => {
            let raw = node.utf8_text(source).ok()?.trim();
            Some(raw.trim_matches(['\'', '"']).to_string())
        }
        _ => None,
    }
}

/// Collect dependency-edge candidates from a recognized key's value —
/// a bare scalar, a list of scalars/mappings, or a mapping directly —
/// applying `rule` at every level (a mapping only ever contributes its
/// `reference_field`, never an unrelated sibling). Iterative (heap-allocated
/// stack, not native recursion): a pathologically deep or wide YAML value
/// (e.g. deeply nested flow sequences) can't overflow the native stack, the
/// same concern `extract_bash_commands` above already guards against.
fn collect_scalar_candidates(
    root: Node<'_>,
    source: &[u8],
    rule: &AnsibleKeyRule,
    out: &mut Vec<String>,
    seen: &mut HashSet<String>,
) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        match node.kind() {
            "plain_scalar" | "block_scalar" | "single_quote_scalar" | "double_quote_scalar" => {
                if let Some(text) = leaf_scalar_text(node, source)
                    && !text.is_empty()
                    && (!rule.path_only || text.contains('/'))
                    && seen.insert(text.clone())
                {
                    out.push(text);
                }
            }
            // Wrapper nodes and sequences just pass their children through —
            // a list item's own wrapper (block_sequence_item) is unwrapped
            // the same way.
            "flow_node"
            | "block_node"
            | "block_sequence"
            | "block_sequence_item"
            | "flow_sequence" => {
                let mut cursor = node.walk();
                stack.extend(
                    node.named_children(&mut cursor)
                        .filter(|c| !matches!(c.kind(), "anchor" | "tag")),
                );
            }
            "block_mapping" | "flow_mapping" => {
                let Some(field) = rule.reference_field else {
                    continue;
                };
                let mut cursor = node.walk();
                for pair in node.named_children(&mut cursor) {
                    if matches!(pair.kind(), "block_mapping_pair" | "flow_pair")
                        && let (Some(key_node), Some(value_node)) = (
                            pair.child_by_field_name("key"),
                            pair.child_by_field_name("value"),
                        )
                        && first_scalar_text(key_node, source).as_deref() == Some(field)
                    {
                        stack.push(value_node);
                    }
                }
            }
            _ => {}
        }
    }
}

/// Walk the YAML tree looking for mapping pairs keyed on a recognized
/// Ansible reference key (see `ansible_key_rule`) and collect their values
/// as dependency-edge candidates. Iterative (heap-allocated stack, not
/// native recursion) for the same reason `extract_bash_commands` above
/// already is: a deeply nested (but otherwise valid) YAML document must not
/// be able to overflow the native stack just by walking it.
fn collect_ansible_pairs(
    root: Node<'_>,
    source: &[u8],
    out: &mut Vec<String>,
    seen: &mut HashSet<String>,
) {
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if matches!(node.kind(), "block_mapping_pair" | "flow_pair")
            && let (Some(key_node), Some(value_node)) = (
                node.child_by_field_name("key"),
                node.child_by_field_name("value"),
            )
            && let Some(raw_key) = first_scalar_text(key_node, source)
            && let Some(rule) = ansible_key_rule(&raw_key)
        {
            collect_scalar_candidates(value_node, source, &rule, out, seen);
        }

        let mut cursor = node.walk();
        stack.extend(node.named_children(&mut cursor));
    }
}

/// Extract Ansible-style file references (`include_tasks`, `import_tasks`,
/// `import_playbook`, `vars_files`, `include_vars`, path-style
/// `include_role`/`roles` entries, and their `ansible.builtin.`-qualified
/// forms) from parsed YAML.
///
/// Like [`extract_reference_paths`], these are "referenced" (not "import")
/// edges: candidates are resolved against indexed logical paths and silently
/// dropped if nothing matches, so callers must fold the result into the same
/// reference set rather than the module-based import dependencies.
pub fn extract_yaml_reference_paths(root: Node<'_>, source: &[u8]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    collect_ansible_pairs(root, source, &mut out, &mut seen);
    out
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
    std::sync::LazyLock::new(|| Regex::new(r"[\w./\\-]+\.(?:py|sh|bash|ksh|ya?ml)\b").unwrap());

/// Extract path-shaped string literals that point at other scripts.
///
/// This is language-agnostic: it scans the raw file text for tokens that look
/// like a path to a `.py`/`.sh`/`.bash`/`.ksh`/`.yml`/`.yaml` file and contain
/// a `/` separator, regardless of whether they appear inside an
/// `ssh`/`scp`/`rsync` command, a `subprocess`/`paramiko` invocation, or a
/// JSON/YAML manifest list. These "called, not imported" edges are invisible
/// to the AST-based import extractors. The YAML extensions catch opportunistic
/// mentions of a playbook/task file (e.g. `subprocess.run(["ansible-playbook",
/// "/catalog/playbooks/site.yml"])`); structured Ansible key/value references
/// within YAML content itself are handled with more precision by
/// `extract_yaml_reference_paths`.
///
/// Extraction is deliberately liberal — precision comes from resolution: a
/// candidate is only kept as a dependency edge if it matches an indexed
/// script's logical path exactly (see `resolve_reference_targets`), so
/// unrelated path strings (logs, temp files) are discarded rather than stored.
pub fn extract_reference_paths(content: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for m in REFERENCE_PATH_RE.find_iter(content) {
        // A char immediately after the match that could continue the same
        // filename (`templates/site.yml.j2`, `run.sh~`, `lib.py-old`) means
        // the real extension is something else entirely — `\b` alone only
        // rules out a following word character, not `.`/`~`/`-`, all of
        // which are themselves part of the path-char class above. The
        // `regex` crate has no lookahead, so reject here instead.
        if matches!(content.as_bytes().get(m.end()), Some(b'.' | b'~' | b'-')) {
            continue;
        }
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
        "yaml" => "yaml",
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
        assert_eq!(normalise_lang("YAML"), "yaml");
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

    // -----------------------------------------------------------------------
    // YAML (Ansible) reference extraction
    // -----------------------------------------------------------------------

    #[test]
    fn yaml_extracts_import_playbook() {
        let mut ext = TreeSitterExtractor::new().unwrap();
        let refs = ext.extract_yaml_deps("- import_playbook: playbooks/site.yml\n");
        assert_eq!(refs, vec!["playbooks/site.yml".to_string()]);
    }

    #[test]
    fn yaml_extracts_include_and_import_tasks() {
        let mut ext = TreeSitterExtractor::new().unwrap();
        let refs = ext.extract_yaml_deps(
            "- include_tasks: tasks/setup.yml\n- import_tasks: tasks/teardown.yml\n",
        );
        assert!(refs.contains(&"tasks/setup.yml".to_string()));
        assert!(refs.contains(&"tasks/teardown.yml".to_string()));
    }

    #[test]
    fn yaml_extracts_fqcn_qualified_keys() {
        let mut ext = TreeSitterExtractor::new().unwrap();
        let refs = ext.extract_yaml_deps("- ansible.builtin.include_tasks: tasks/setup.yml\n");
        assert_eq!(refs, vec!["tasks/setup.yml".to_string()]);
    }

    #[test]
    fn yaml_extracts_vars_files_and_include_vars_lists() {
        let mut ext = TreeSitterExtractor::new().unwrap();
        let refs = ext.extract_yaml_deps(
            "vars_files:\n  - vars/main.yml\n  - vars/prod.yml\ninclude_vars: vars/extra.yml\n",
        );
        assert!(refs.contains(&"vars/main.yml".to_string()));
        assert!(refs.contains(&"vars/prod.yml".to_string()));
        assert!(refs.contains(&"vars/extra.yml".to_string()));
    }

    #[test]
    fn yaml_include_tasks_mapping_form_resolves_file_key() {
        let mut ext = TreeSitterExtractor::new().unwrap();
        let refs = ext.extract_yaml_deps(
            "- include_tasks:\n    file: tasks/setup.yml\n    apply:\n      tags: setup\n",
        );
        assert!(refs.contains(&"tasks/setup.yml".to_string()));
    }

    #[test]
    fn yaml_include_tasks_ignores_option_fields_even_when_path_shaped() {
        // A sibling option (`apply.tags` here) is not a reference field, so
        // it must never become a candidate — even when its value happens to
        // look like a path to an indexed script.
        let mut ext = TreeSitterExtractor::new().unwrap();
        let refs = ext.extract_yaml_deps(
            "- include_tasks:\n    file: tasks/setup.yml\n    apply:\n      tags: lib/unrelated.yml\n",
        );
        assert_eq!(refs, vec!["tasks/setup.yml".to_string()]);
    }

    #[test]
    fn yaml_roles_mapping_form_ignores_sibling_vars() {
        // `roles: [{role: ..., vars: {...}}]` — only the `role` field is a
        // reference; a `vars` sub-mapping alongside it is not, even if one
        // of its values looks path-shaped.
        let mut ext = TreeSitterExtractor::new().unwrap();
        let refs = ext.extract_yaml_deps(
            "roles:\n  - role: roles/custom/nginx\n    vars:\n      config_file: lib/unrelated.yml\n",
        );
        assert_eq!(refs, vec!["roles/custom/nginx".to_string()]);
    }

    #[test]
    fn yaml_roles_keeps_path_style_entries_and_drops_galaxy_names() {
        let mut ext = TreeSitterExtractor::new().unwrap();
        let refs = ext.extract_yaml_deps("roles:\n  - common\n  - roles/custom/nginx\n");
        assert!(refs.contains(&"roles/custom/nginx".to_string()));
        assert!(!refs.contains(&"common".to_string()));
    }

    #[test]
    fn yaml_include_role_keeps_path_style_name_and_drops_galaxy_name() {
        let mut ext = TreeSitterExtractor::new().unwrap();
        let path_refs = ext.extract_yaml_deps("- include_role:\n    name: roles/custom/nginx\n");
        assert_eq!(path_refs, vec!["roles/custom/nginx".to_string()]);

        let galaxy_refs = ext.extract_yaml_deps("- include_role:\n    name: nginx\n");
        assert!(galaxy_refs.is_empty(), "unexpected: {galaxy_refs:?}");
    }

    #[test]
    fn yaml_ignores_unrelated_keys() {
        let mut ext = TreeSitterExtractor::new().unwrap();
        let refs = ext.extract_yaml_deps(
            "name: Deploy app\nhosts: all\ntasks:\n  - name: run it\n    command: /bin/true\n",
        );
        assert!(refs.is_empty(), "unexpected: {refs:?}");
    }

    #[test]
    fn yaml_malformed_content_does_not_panic() {
        let mut ext = TreeSitterExtractor::new().unwrap();
        // Unterminated flow mapping / mismatched braces — must not panic,
        // an empty result (however partial the parse) is acceptable.
        let _ = ext.extract_yaml_deps("include_tasks: [foo.yml\nfoo: {bar: ");
    }

    #[test]
    fn yaml_empty_source_does_not_panic() {
        let mut ext = TreeSitterExtractor::new().unwrap();
        assert!(ext.extract_yaml_deps("").is_empty());
    }

    #[test]
    fn reference_path_regex_now_matches_yaml_extensions() {
        let refs = extract_reference_paths(
            "ansible-playbook /catalog/playbooks/site.yml and /catalog/vars/main.yaml",
        );
        assert!(refs.contains(&"/catalog/playbooks/site.yml".to_string()));
        assert!(refs.contains(&"/catalog/vars/main.yaml".to_string()));
    }

    #[test]
    fn reference_paths_reject_compound_extensions() {
        // A char right after the matched extension that could continue the
        // same filename (a Jinja-templated YAML file, a shell backup, a
        // dash-suffixed copy) means the real extension is something else —
        // `templates/site.yml` must not be extracted from
        // `templates/site.yml.j2`.
        let refs = extract_reference_paths(
            "see templates/site.yml.j2 and lib/run.sh~ and old/script.py-bak",
        );
        assert!(refs.is_empty(), "unexpected: {refs:?}");
    }

    #[test]
    fn yaml_deep_nesting_does_not_stack_overflow() {
        // The value walk under a recognized key used to be recursive,
        // tracking YAML nesting depth 1:1 with native stack depth. A
        // pathologically (but validly) deep flow sequence must not crash —
        // same concern as `extract_deps_handles_deeply_nested_script_without_stack_overflow`.
        let mut source = String::from("vars_files: ");
        for _ in 0..20_000 {
            source.push('[');
        }
        source.push_str("x/y.yml");
        for _ in 0..20_000 {
            source.push(']');
        }

        let mut ext = TreeSitterExtractor::new().unwrap();
        let refs = ext.extract_yaml_deps(&source);
        assert!(refs.contains(&"x/y.yml".to_string()));
    }
}
