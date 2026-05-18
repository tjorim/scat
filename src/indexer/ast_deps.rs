use std::collections::{HashMap, HashSet};

use tree_sitter::Node;

// ---------------------------------------------------------------------------
// Data model
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Clone)]
/// AST-derived dependencies and symbol metadata for a Python source file.
pub struct AstDependencies {
    /// Imported module paths discovered in the source.
    pub imports: Vec<String>,
    /// Bare function names discovered in the source.
    pub function_defs: Vec<String>,
    /// Function/class definitions with metadata.
    pub definitions: Vec<FunctionDefinition>,
    /// Function call edges with optional resolution.
    pub calls: Vec<FunctionCall>,
}

#[derive(Debug, Default, Clone)]
/// Function/class definition captured from a Python AST.
pub struct FunctionDefinition {
    /// Function or class name (possibly qualified for nested/class scope).
    pub name: String,
    /// Definition kind (`function` or `class`).
    pub kind: String,
    /// 1-based source line number.
    pub line: usize,
    /// Extracted leading docstring, if present.
    pub docstring: String,
    /// Decorators applied to the definition.
    pub decorators: Vec<String>,
}

#[derive(Debug, Default, Clone)]
/// Function call captured from a Python AST.
pub struct FunctionCall {
    /// Caller scope (`__module__` for top-level calls).
    pub caller: String,
    /// Called function expression.
    pub callee: String,
    /// 1-based source line number.
    pub line: usize,
    /// Resolved target symbol when known.
    pub resolved_target: Option<String>,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Extract Python imports, definitions, and calls from `source`.
pub fn extract_python_deps(parser: &mut tree_sitter::Parser, source: &str) -> AstDependencies {
    extract_python_deps_with_module(parser, source, None)
}

/// Same as `extract_python_deps` but with optional module name for relative
/// import/call target resolution.
pub fn extract_python_deps_with_module(
    parser: &mut tree_sitter::Parser,
    source: &str,
    module_name: Option<&str>,
) -> AstDependencies {
    let mut result = AstDependencies::default();
    let source_bytes = source.as_bytes();

    let tree = match parser.parse(source_bytes, None) {
        Some(t) => t,
        None => return result,
    };

    let root = tree.root_node();
    let mut seen = HashSet::new();
    let mut import_bindings = HashMap::new();

    collect_imports(
        root,
        source_bytes,
        &mut result,
        &mut seen,
        &mut import_bindings,
    );
    collect_definitions_in_scope(root, source_bytes, &mut result, &[]);

    let local_names: HashSet<String> = result.definitions.iter().map(|d| d.name.clone()).collect();
    let mut scope: Vec<String> = Vec::new();
    collect_calls(
        root,
        source_bytes,
        &mut result,
        &mut scope,
        &import_bindings,
        &local_names,
        module_name,
    );

    result
}

// ---------------------------------------------------------------------------
// Tree walker
// ---------------------------------------------------------------------------

fn add_import(name: &str, result: &mut AstDependencies, seen: &mut HashSet<String>) {
    let name = name.trim();
    if !name.is_empty() && !seen.contains(name) {
        seen.insert(name.to_string());
        result.imports.push(name.to_string());
    }
}

fn collect_imports(
    node: Node<'_>,
    source: &[u8],
    result: &mut AstDependencies,
    seen: &mut HashSet<String>,
    import_bindings: &mut HashMap<String, String>,
) {
    match node.kind() {
        "import_statement" => {
            for i in 0..node.child_count() {
                let Some(child) = node.child(i as u32) else {
                    continue;
                };
                match child.kind() {
                    "dotted_name" => {
                        let import_name = text(child, source);
                        add_import(&import_name, result, seen);
                        if let Some(root) = import_name.split('.').next()
                            && !root.is_empty()
                        {
                            import_bindings.insert(root.to_string(), root.to_string());
                        }
                    }
                    "aliased_import" => {
                        let mut import_name = String::new();
                        let mut as_name = String::new();
                        for j in 0..child.child_count() {
                            let Some(grandchild) = child.child(j as u32) else {
                                continue;
                            };
                            if grandchild.kind() == "dotted_name"
                                || grandchild.kind() == "identifier"
                            {
                                if import_name.is_empty() {
                                    import_name = text(grandchild, source);
                                } else {
                                    as_name = text(grandchild, source);
                                }
                            }
                        }
                        if !import_name.is_empty() {
                            add_import(&import_name, result, seen);
                            let local = if as_name.is_empty() {
                                import_name
                                    .split('.')
                                    .next()
                                    .unwrap_or_default()
                                    .to_string()
                            } else {
                                as_name
                            };
                            if !local.is_empty() {
                                import_bindings.insert(local, import_name);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        "import_from_statement" => {
            let mut before_import_kw = true;
            let mut prefix_dots = String::new();
            let mut module_name = String::new();
            let mut post_import_names: Vec<(String, String)> = Vec::new();

            for i in 0..node.child_count() {
                let Some(child) = node.child(i as u32) else {
                    continue;
                };
                match child.kind() {
                    "import" => before_import_kw = false,
                    "relative_import" if before_import_kw => {
                        for j in 0..child.child_count() {
                            let Some(grandchild) = child.child(j as u32) else {
                                continue;
                            };
                            match grandchild.kind() {
                                "import_prefix" => {
                                    prefix_dots = text(grandchild, source);
                                }
                                "dotted_name" => {
                                    module_name = text(grandchild, source);
                                }
                                _ => {}
                            }
                        }
                    }
                    "dotted_name" if before_import_kw => module_name = text(child, source),
                    "dotted_name" if !before_import_kw => {
                        let name = text(child, source);
                        post_import_names.push((name.clone(), name));
                    }
                    "aliased_import" if !before_import_kw => {
                        let mut import_name = String::new();
                        let mut as_name = String::new();
                        for j in 0..child.child_count() {
                            let Some(grandchild) = child.child(j as u32) else {
                                continue;
                            };
                            if grandchild.kind() == "dotted_name"
                                || grandchild.kind() == "identifier"
                            {
                                if import_name.is_empty() {
                                    import_name = text(grandchild, source);
                                } else {
                                    as_name = text(grandchild, source);
                                }
                            }
                        }
                        if !import_name.is_empty() {
                            let local = if as_name.is_empty() {
                                import_name.clone()
                            } else {
                                as_name
                            };
                            post_import_names.push((import_name, local));
                        }
                    }
                    "wildcard_import" if !before_import_kw => {
                        post_import_names.push(("*".to_string(), "*".to_string()));
                    }
                    _ => {}
                }
            }

            if !module_name.is_empty() {
                let dep = format!("{prefix_dots}{module_name}");
                add_import(&dep, result, seen);
                for (imported, local) in &post_import_names {
                    if imported == "*" {
                        import_bindings.insert("*".to_string(), dep.clone());
                        continue;
                    }
                    import_bindings.insert(local.clone(), format!("{dep}.{imported}"));
                }
            } else if !prefix_dots.is_empty() {
                for (imported, local) in &post_import_names {
                    if imported == "*" {
                        continue;
                    }
                    let dep = format!("{prefix_dots}{imported}");
                    add_import(&dep, result, seen);
                    import_bindings.insert(local.clone(), dep);
                }
            }
        }
        _ => {
            for i in 0..node.child_count() {
                if let Some(child) = node.child(i as u32) {
                    collect_imports(child, source, result, seen, import_bindings);
                }
            }
        }
    }
}

fn collect_definitions_in_scope(
    node: Node<'_>,
    source: &[u8],
    result: &mut AstDependencies,
    prefix: &[String],
) {
    for i in 0..node.child_count() {
        let Some(child) = node.child(i as u32) else {
            continue;
        };
        let (def_node, decorators) = match child.kind() {
            "function_definition" | "class_definition" => (Some(child), vec![]),
            "decorated_definition" => {
                let decs = collect_decorators(child, source);
                let inner = (0..child.child_count())
                    .filter_map(|j| child.child(j as u32))
                    .find(|gc| matches!(gc.kind(), "function_definition" | "class_definition"));
                (inner, decs)
            }
            _ => (None, vec![]),
        };
        let Some(def) = def_node else {
            continue;
        };
        add_definition(def, source, result, decorators, prefix);
        if def.kind() == "class_definition"
            && let (Some(name_node), Some(body)) = (
                def.child_by_field_name("name"),
                def.child_by_field_name("body"),
            )
        {
            let name = text(name_node, source);
            if !name.is_empty() {
                let mut new_prefix = prefix.to_vec();
                new_prefix.push(name);
                collect_definitions_in_scope(body, source, result, &new_prefix);
            }
        }
    }
}

fn add_definition(
    node: Node<'_>,
    source: &[u8],
    result: &mut AstDependencies,
    decorators: Vec<String>,
    prefix: &[String],
) {
    let Some(name_node) = node.child_by_field_name("name") else {
        return;
    };
    let bare_name = text(name_node, source);
    if bare_name.is_empty() {
        return;
    }

    let kind = if node.kind() == "class_definition" {
        "class".to_string()
    } else {
        "function".to_string()
    };
    if kind == "function" {
        // function_defs intentionally stores bare (unqualified) names regardless of class
        // scope. It is used by the legacy dependency-resolution path which matches import
        // roots against bare function names, so qualified names like "Worker.run" must not
        // appear here. The `definitions` field carries the fully-qualified form.
        result.function_defs.push(bare_name.clone());
    }

    let name = if prefix.is_empty() {
        bare_name
    } else {
        format!("{}.{}", prefix.join("."), bare_name)
    };

    result.definitions.push(FunctionDefinition {
        name,
        kind,
        line: name_node.start_position().row + 1,
        docstring: extract_docstring(node, source),
        decorators,
    });
}

fn collect_decorators(node: Node<'_>, source: &[u8]) -> Vec<String> {
    let mut decorators = Vec::new();
    for i in 0..node.child_count() {
        let Some(child) = node.child(i as u32) else {
            continue;
        };
        if child.kind() == "decorator" {
            let raw = text(child, source);
            let rendered = raw.trim().trim_start_matches('@').trim().to_string();
            if !rendered.is_empty() {
                decorators.push(rendered);
            }
        }
    }
    decorators
}

fn extract_docstring(node: Node<'_>, source: &[u8]) -> String {
    let Some(body) = node.child_by_field_name("body") else {
        return String::new();
    };
    for i in 0..body.named_child_count() {
        let Some(stmt) = body.named_child(i as u32) else {
            continue;
        };
        if stmt.kind() != "expression_statement" {
            break;
        }
        let raw = text(stmt, source);
        let doc = unquote_string_like(&raw);
        if !doc.is_empty() {
            return doc;
        }
    }
    String::new()
}

fn unquote_string_like(raw: &str) -> String {
    let mut text = raw.trim();
    if text.is_empty() {
        return String::new();
    }
    while let Some(first) = text.chars().next() {
        if matches!(first, 'r' | 'R' | 'u' | 'U' | 'b' | 'B' | 'f' | 'F') {
            text = &text[first.len_utf8()..];
        } else {
            break;
        }
    }
    for quote in ["\"\"\"", "'''", "\"", "'"] {
        if text.starts_with(quote) && text.ends_with(quote) && text.len() >= quote.len() * 2 {
            let inner = &text[quote.len()..text.len() - quote.len()];
            return inner.trim().to_string();
        }
    }
    String::new()
}

fn collect_calls(
    node: Node<'_>,
    source: &[u8],
    result: &mut AstDependencies,
    scope: &mut Vec<String>,
    import_bindings: &HashMap<String, String>,
    local_names: &HashSet<String>,
    module_name: Option<&str>,
) {
    if (node.kind() == "function_definition" || node.kind() == "class_definition")
        && let Some(name_node) = node.child_by_field_name("name")
    {
        let name = text(name_node, source);
        if !name.is_empty() {
            scope.push(name);
            for i in 0..node.child_count() {
                if let Some(child) = node.child(i as u32) {
                    collect_calls(
                        child,
                        source,
                        result,
                        scope,
                        import_bindings,
                        local_names,
                        module_name,
                    );
                }
            }
            let _ = scope.pop();
            return;
        }
    }

    if node.kind() == "call"
        && let Some(function_node) = node.child_by_field_name("function")
    {
        let callee = expr_to_name(function_node, source);
        if !callee.is_empty() {
            let caller = if scope.is_empty() {
                "__module__".to_string()
            } else {
                scope.join(".")
            };
            result.calls.push(FunctionCall {
                caller,
                callee: callee.clone(),
                line: node.start_position().row + 1,
                resolved_target: resolve_target(&callee, import_bindings, local_names, module_name),
            });
        }
    }

    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            collect_calls(
                child,
                source,
                result,
                scope,
                import_bindings,
                local_names,
                module_name,
            );
        }
    }
}

fn expr_to_name(node: Node<'_>, source: &[u8]) -> String {
    match node.kind() {
        "identifier" | "dotted_name" => text(node, source),
        "attribute" => {
            let left = node
                .child_by_field_name("object")
                .map(|n| expr_to_name(n, source))
                .unwrap_or_default();
            let right = node
                .child_by_field_name("attribute")
                .map(|n| text(n, source))
                .unwrap_or_default();
            if left.is_empty() {
                right
            } else if right.is_empty() {
                left
            } else {
                format!("{left}.{right}")
            }
        }
        _ => String::new(),
    }
}

fn resolve_target(
    callee: &str,
    import_bindings: &HashMap<String, String>,
    local_names: &HashSet<String>,
    module_name: Option<&str>,
) -> Option<String> {
    let root = callee.split('.').next().unwrap_or_default();
    if root.is_empty() {
        return None;
    }
    let suffix = callee
        .split_once('.')
        .map(|(_, tail)| tail.to_string())
        .unwrap_or_default();

    if let Some(imported) = import_bindings.get(root)
        && let Some(base) = resolve_relative(imported, module_name)
    {
        if suffix.is_empty() {
            return Some(base);
        }
        return Some(format!("{base}.{suffix}"));
    }

    if local_names.contains(root) {
        if let Some(module) = module_name
            && !module.is_empty()
        {
            return Some(format!("{module}.{callee}"));
        }
        return Some(callee.to_string());
    }

    None
}

fn resolve_relative(name: &str, module_name: Option<&str>) -> Option<String> {
    if !name.starts_with('.') {
        return Some(name.to_string());
    }
    let module_name = module_name?;
    let mut leading_dot_count = 0usize;
    for ch in name.chars() {
        if ch == '.' {
            leading_dot_count += 1;
        } else {
            break;
        }
    }
    let suffix = &name[leading_dot_count..];
    let mut package_parts: Vec<&str> = module_name.split('.').collect();
    if package_parts.len() > 1 {
        package_parts.pop();
    }
    let levels_up = leading_dot_count.saturating_sub(1);
    if levels_up > package_parts.len() {
        return if suffix.is_empty() {
            None
        } else {
            Some(suffix.to_string())
        };
    }
    let base_len = package_parts.len().saturating_sub(levels_up);
    let mut parts: Vec<&str> = package_parts[..base_len].to_vec();
    if !suffix.is_empty() {
        parts.push(suffix);
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("."))
    }
}

fn text(node: Node<'_>, source: &[u8]) -> String {
    node.utf8_text(source).unwrap_or("").trim().to_string()
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_parser() -> tree_sitter::Parser {
        let mut p = tree_sitter::Parser::new();
        p.set_language(&tree_sitter_python::LANGUAGE.into())
            .unwrap();
        p
    }

    #[test]
    fn extracts_simple_import() {
        let mut p = make_parser();
        let deps = extract_python_deps(&mut p, "import os\nimport sys");
        assert!(deps.imports.contains(&"os".to_string()));
        assert!(deps.imports.contains(&"sys".to_string()));
    }

    #[test]
    fn extracts_from_import() {
        let mut p = make_parser();
        let deps = extract_python_deps(&mut p, "from pathlib import Path");
        assert!(deps.imports.contains(&"pathlib".to_string()));
    }

    #[test]
    fn extracts_class_and_definition_metadata() {
        let mut p = make_parser();
        let src = "@dataclass\nclass User:\n    \"\"\"doc\"\"\"\n    pass\n";
        let deps = extract_python_deps(&mut p, src);
        assert_eq!(deps.definitions.len(), 1);
        assert_eq!(deps.definitions[0].name, "User");
        assert_eq!(deps.definitions[0].kind, "class");
        assert_eq!(deps.definitions[0].docstring, "doc");
        assert_eq!(deps.definitions[0].decorators, vec!["dataclass"]);
    }

    #[test]
    fn extracts_and_resolves_local_call() {
        let mut p = make_parser();
        let src = "def helper():\n    pass\n\ndef outer():\n    helper()\n";
        let deps = extract_python_deps_with_module(&mut p, src, Some("pkg.main"));
        assert_eq!(deps.calls.len(), 1);
        assert_eq!(deps.calls[0].caller, "outer");
        assert_eq!(deps.calls[0].callee, "helper");
        assert_eq!(
            deps.calls[0].resolved_target.as_deref(),
            Some("pkg.main.helper")
        );
    }

    #[test]
    fn resolves_from_import_alias_call() {
        let mut p = make_parser();
        let src = "from .helpers import run as start\nstart()\n";
        let deps = extract_python_deps_with_module(&mut p, src, Some("pkg.main"));
        assert_eq!(deps.calls.len(), 1);
        assert_eq!(
            deps.calls[0].resolved_target.as_deref(),
            Some("pkg.helpers.run")
        );
    }

    // -----------------------------------------------------------------------
    // Restored edge-case tests
    // -----------------------------------------------------------------------

    #[test]
    fn extracts_dotted_module() {
        let mut p = make_parser();
        let deps = extract_python_deps(&mut p, "from os.path import join");
        assert!(deps.imports.contains(&"os.path".to_string()));
    }

    #[test]
    fn extracts_relative_import() {
        let mut p = make_parser();
        let deps = extract_python_deps(&mut p, "from . import utils");
        assert!(deps.imports.contains(&".utils".to_string()));
    }

    #[test]
    fn deduplicates_imports() {
        let mut p = make_parser();
        let deps = extract_python_deps(&mut p, "import os\nimport os");
        assert_eq!(
            deps.imports.iter().filter(|s| s.as_str() == "os").count(),
            1
        );
    }

    #[test]
    fn handles_invalid_python_gracefully() {
        let mut p = make_parser();
        // Should not panic; tree-sitter returns a partial/error tree
        let deps = extract_python_deps(&mut p, "this is not python @@@");
        let _ = deps;
    }

    #[test]
    fn extracts_top_level_function_defs() {
        let mut p = make_parser();
        let deps = extract_python_deps(&mut p, "def main():\n    pass\ndef helper():\n    pass\n");
        assert!(deps.function_defs.contains(&"main".to_string()));
        assert!(deps.function_defs.contains(&"helper".to_string()));
    }

    // -----------------------------------------------------------------------
    // New coverage: decorated definitions, wildcard imports, nested scopes
    // -----------------------------------------------------------------------

    #[test]
    fn decorated_function_in_class_body() {
        let mut p = make_parser();
        let src = "class Worker:\n    @staticmethod\n    def run():\n        pass\n";
        let deps = extract_python_deps(&mut p, src);
        // Class and method both indexed
        assert_eq!(deps.definitions.len(), 2);
        let method = deps
            .definitions
            .iter()
            .find(|d| d.name == "Worker.run")
            .unwrap();
        assert_eq!(method.kind, "function");
        assert_eq!(method.decorators, vec!["staticmethod"]);
        // function_defs carries bare name for legacy resolution
        assert!(deps.function_defs.contains(&"run".to_string()));
    }

    #[test]
    fn wildcard_import_no_individual_binding() {
        let mut p = make_parser();
        // `from os import *` — module is recorded as a dependency but individual
        // names are not bound, so unqualified calls are not resolved.
        let src = "from os import *\ngetenv('HOME')\n";
        let deps = extract_python_deps_with_module(&mut p, src, Some("pkg.main"));
        assert!(deps.imports.contains(&"os".to_string()));
        // getenv is not in import_bindings (wildcard), so resolved_target is None
        let call = deps.calls.iter().find(|c| c.callee == "getenv").unwrap();
        assert!(call.resolved_target.is_none());
    }

    #[test]
    fn nested_function_scope_attribution() {
        let mut p = make_parser();
        let src = "def outer():\n    def inner():\n        pass\n    inner()\n";
        let deps = extract_python_deps_with_module(&mut p, src, Some("pkg.main"));
        // The call to inner() is made from outer's scope
        let call = deps.calls.iter().find(|c| c.callee == "inner").unwrap();
        assert_eq!(call.caller, "outer");
    }
}
