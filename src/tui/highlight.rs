//! Syntax highlighting for script content shown in the catalog preview pane
//! (`src/tui/render/preview.rs`) and the full-screen detail view
//! (`src/tui/detail.rs`), via `tree-sitter-highlight`. Python and Bash reuse
//! the grammars already loaded for dependency extraction
//! (`src/indexer/treesitter_deps.rs`); JSON and YAML are highlighted only
//! (the indexer never tree-sits them).

use std::sync::LazyLock;

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use tree_sitter_highlight::{Highlight, HighlightConfiguration, HighlightEvent, Highlighter};

use scat_core::indexer::treesitter_deps::{normalise_lang, parse_with_timeout};

/// Highlight capture names recognized from the bundled Python/Bash/JSON/YAML
/// `highlights.scm` queries. A `Highlight(i)` event from [`Highlighter`]
/// indexes into this list (set via `HighlightConfiguration::configure`).
const HIGHLIGHT_NAMES: &[&str] = &[
    "comment",
    "string",
    "escape",
    "number",
    "constant",
    "constant.builtin",
    "boolean",
    "keyword",
    "attribute",
    "operator",
    "function",
    "function.builtin",
    "function.method",
    "type",
    "constructor",
    "property",
    "label",
    "punctuation.special",
    "punctuation.bracket",
    "punctuation.delimiter",
    "variable",
];

/// Small fixed palette mapping a highlight capture name to a style,
/// consistent with the TUI's existing colors (`detail::label_style()`'s
/// cyan/bold labels, `render::common::focus_border`'s yellow focus color).
fn style_for(name: &str) -> Style {
    match name {
        "comment" => Style::default().fg(Color::DarkGray),
        "string" => Style::default().fg(Color::Green),
        "escape" | "punctuation.special" => Style::default().fg(Color::LightMagenta),
        "number" | "constant" | "constant.builtin" | "boolean" => {
            Style::default().fg(Color::Yellow)
        }
        "keyword" | "attribute" => Style::default().fg(Color::Magenta),
        "function" | "function.builtin" | "function.method" => Style::default().fg(Color::Blue),
        "type" | "constructor" => Style::default().fg(Color::Cyan),
        "property" => Style::default().fg(Color::LightCyan),
        "label" => Style::default().fg(Color::LightBlue),
        _ => Style::default(),
    }
}

fn build_config(
    language: tree_sitter::Language,
    name: &'static str,
    highlights_query: &'static str,
) -> HighlightConfiguration {
    let mut config = HighlightConfiguration::new(language, name, highlights_query, "", "")
        .unwrap_or_else(|err| panic!("bundled {name} highlights query is invalid: {err}"));
    config.configure(HIGHLIGHT_NAMES);
    config
}

static PYTHON_CONFIG: LazyLock<HighlightConfiguration> = LazyLock::new(|| {
    build_config(
        tree_sitter_python::LANGUAGE.into(),
        "python",
        tree_sitter_python::HIGHLIGHTS_QUERY,
    )
});

static BASH_CONFIG: LazyLock<HighlightConfiguration> = LazyLock::new(|| {
    build_config(
        tree_sitter_bash::LANGUAGE.into(),
        "bash",
        tree_sitter_bash::HIGHLIGHT_QUERY,
    )
});

// `tree-sitter-json`'s bundled query captures object keys as
// `@string.special.key` before the generic `@string`; since a key node is
// always also a `(string)` node, and `tree-sitter-highlight` resolves two
// captures on the same node by keeping the *last*-declared pattern, keys
// render with the plain `string` style here, same as any other string.
static JSON_CONFIG: LazyLock<HighlightConfiguration> = LazyLock::new(|| {
    build_config(
        tree_sitter_json::LANGUAGE.into(),
        "json",
        tree_sitter_json::HIGHLIGHTS_QUERY,
    )
});

static YAML_CONFIG: LazyLock<HighlightConfiguration> = LazyLock::new(|| {
    build_config(
        tree_sitter_yaml::LANGUAGE.into(),
        "yaml",
        tree_sitter_yaml::HIGHLIGHTS_QUERY,
    )
});

/// Highlight `source` for `language` (the `scripts.language` column's
/// values, e.g. `"python"`/`"shell"`/`"json"`/`"yaml"`), returning one
/// [`Line`] per line of `source` — matching `source.lines()` exactly, since
/// callers (scroll math in `render::preview`, click hit-testing in
/// `detail::detail_click_at`) count on one `Line` per source line.
///
/// Falls back to plain, unstyled lines for a language other than Python,
/// Bash, JSON, or YAML, or when parsing doesn't finish within the same
/// deadline [`parse_with_timeout`] enforces for dependency extraction.
pub(super) fn highlight_lines(source: &str, language: &str) -> Vec<Line<'static>> {
    if source.is_empty() {
        return Vec::new();
    }
    let config = match normalise_lang(language) {
        "python" => Some(&*PYTHON_CONFIG),
        "bash" => Some(&*BASH_CONFIG),
        "json" => Some(&*JSON_CONFIG),
        "yaml" => Some(&*YAML_CONFIG),
        _ => None,
    };
    config
        .and_then(|config| try_highlight(source, config))
        .unwrap_or_else(|| plain_lines(source))
}

fn plain_lines(source: &str) -> Vec<Line<'static>> {
    source
        .lines()
        .map(|line| Line::from(line.to_string()))
        .collect()
}

fn try_highlight(source: &str, config: &HighlightConfiguration) -> Option<Vec<Line<'static>>> {
    // `Highlighter::highlight` always reparses internally with no timeout of
    // its own, so probe first with a throwaway parse under the same deadline
    // `parse_with_timeout` guards dependency extraction with — pathological
    // input (deep nesting, a huge minified file) could otherwise stall the
    // detail worker thread indefinitely.
    let mut probe = tree_sitter::Parser::new();
    probe.set_language(&config.language).ok()?;
    parse_with_timeout(&mut probe, source.as_bytes())?;

    let mut highlighter = Highlighter::new();
    let events = highlighter
        .highlight(config, source.as_bytes(), None, None, |_| None)
        .ok()?;

    let mut lines: Vec<Vec<Span<'static>>> = vec![Vec::new()];
    let mut stack: Vec<usize> = Vec::new();
    for event in events {
        match event.ok()? {
            HighlightEvent::HighlightStart(Highlight(index)) => stack.push(index),
            HighlightEvent::HighlightEnd => {
                stack.pop();
            }
            HighlightEvent::Source { start, end } => {
                let text = source.get(start..end)?;
                let style = stack
                    .last()
                    .map_or(Style::default(), |&i| style_for(HIGHLIGHT_NAMES[i]));
                push_text(&mut lines, text, style);
            }
        }
    }
    Some(lines.into_iter().map(Line::from).collect())
}

/// Append `text` (a contiguous `HighlightEvent::Source` chunk) to the
/// in-progress `lines`, starting a new line at each `\n` — never merging or
/// splitting a source line, so the result stays one [`Line`] per line of the
/// original source.
fn push_text(lines: &mut Vec<Vec<Span<'static>>>, text: &str, style: Style) {
    let mut parts = text.split('\n');
    if let Some(first) = parts.next()
        && !first.is_empty()
    {
        lines
            .last_mut()
            .expect("lines always has a current line")
            .push(Span::styled(first.to_string(), style));
    }
    for part in parts {
        lines.push(Vec::new());
        if !part.is_empty() {
            lines
                .last_mut()
                .expect("lines always has a current line")
                .push(Span::styled(part.to_string(), style));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{highlight_lines, style_for};
    use ratatui::style::Style;
    use ratatui::text::Line;

    fn line_text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    #[test]
    fn empty_content_returns_no_lines() {
        assert!(highlight_lines("", "python").is_empty());
        assert!(highlight_lines("", "shell").is_empty());
    }

    #[test]
    fn unsupported_language_falls_back_to_plain_unstyled_lines() {
        let lines = highlight_lines("SELECT 1", "sql");
        assert_eq!(lines.len(), 1);
        assert_eq!(line_text(&lines[0]), "SELECT 1");
        assert_eq!(lines[0].spans.len(), 1);
        assert_eq!(lines[0].spans[0].style, Style::default());
    }

    #[test]
    fn python_keyword_and_string_are_styled() {
        let lines = highlight_lines("import os\nx = \"hi\"", "python");
        assert_eq!(lines.len(), 2);
        assert_eq!(line_text(&lines[0]), "import os");
        assert_eq!(line_text(&lines[1]), "x = \"hi\"");
        assert!(
            lines[0]
                .spans
                .iter()
                .any(|s| s.content.as_ref() == "import" && s.style == style_for("keyword")),
            "expected a styled `import` keyword span: {:?}",
            lines[0].spans
        );
        assert!(
            lines[1]
                .spans
                .iter()
                .any(|s| s.content.contains("hi") && s.style == style_for("string")),
            "expected a styled string span: {:?}",
            lines[1].spans
        );
    }

    #[test]
    fn bash_keyword_and_comment_are_styled() {
        let lines = highlight_lines("# comment\nif true; then echo hi; fi", "shell");
        assert_eq!(lines.len(), 2);
        assert!(
            lines[0]
                .spans
                .iter()
                .any(|s| s.style == style_for("comment")),
            "expected a styled comment span: {:?}",
            lines[0].spans
        );
        assert!(
            lines[1]
                .spans
                .iter()
                .any(|s| s.content.as_ref() == "if" && s.style == style_for("keyword")),
            "expected a styled `if` keyword span: {:?}",
            lines[1].spans
        );
    }

    #[test]
    fn json_string_and_number_are_styled() {
        let lines = highlight_lines("{\n  \"name\": \"hi\",\n  \"n\": 1\n}", "json");
        assert_eq!(lines.len(), 4);
        // Keys render with the same `string` style as values — see the
        // `JSON_CONFIG` doc comment for why the bundled query's own
        // `@string.special.key` capture never wins for a key node.
        assert!(
            lines[1]
                .spans
                .iter()
                .any(|s| s.content.contains("name") && s.style == style_for("string")),
            "expected a styled key span: {:?}",
            lines[1].spans
        );
        assert!(
            lines[1]
                .spans
                .iter()
                .any(|s| s.content.contains("hi") && s.style == style_for("string")),
            "expected a styled string value span: {:?}",
            lines[1].spans
        );
        assert!(
            lines[2]
                .spans
                .iter()
                .any(|s| s.content.as_ref() == "1" && s.style == style_for("number")),
            "expected a styled number span: {:?}",
            lines[2].spans
        );
    }

    #[test]
    fn yaml_key_comment_and_boolean_are_styled() {
        let lines = highlight_lines("# comment\nname: hi\nenabled: true", "yaml");
        assert_eq!(lines.len(), 3);
        assert!(
            lines[0]
                .spans
                .iter()
                .any(|s| s.style == style_for("comment")),
            "expected a styled comment span: {:?}",
            lines[0].spans
        );
        assert!(
            lines[1]
                .spans
                .iter()
                .any(|s| s.content.as_ref() == "name" && s.style == style_for("property")),
            "expected a styled key span: {:?}",
            lines[1].spans
        );
        assert!(
            lines[2]
                .spans
                .iter()
                .any(|s| s.content.as_ref() == "true" && s.style == style_for("boolean")),
            "expected a styled boolean span: {:?}",
            lines[2].spans
        );
    }

    #[test]
    fn line_count_matches_source_lines_exactly() {
        let source = "def f():\n    return 1\n\nf()";
        let lines = highlight_lines(source, "python");
        assert_eq!(lines.len(), source.lines().count());
        for (line, expected) in lines.iter().zip(source.lines()) {
            assert_eq!(line_text(line), expected);
        }
    }

    #[test]
    fn malformed_source_does_not_panic() {
        // Unterminated strings/blocks — tree-sitter's error recovery must
        // not crash the highlighter or our event handling.
        assert!(!highlight_lines("def f(:\n    x = \"unterminated\n", "python").is_empty());
        assert!(!highlight_lines("if [ -z \"\n", "shell").is_empty());
        assert!(!highlight_lines("{\"a\": [1, 2,\n", "json").is_empty());
        assert!(!highlight_lines("key: [1, 2\n  bad indent\n", "yaml").is_empty());
        assert!(!highlight_lines("\u{0}\u{1}\u{2} not really code", "python").is_empty());
    }
}
