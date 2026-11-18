//! Making text from indexed files safe to print on a terminal.
//!
//! Everything scat shows — a script's `purpose`, `owner`, tags, function
//! names, docstrings, diff bodies, even file names — comes from files that
//! someone else can write. Printed raw, an escape sequence in a header comment
//! is interpreted by the terminal of whoever lists the script: it can retitle
//! the window, rewrite earlier lines (carriage return, cursor movement) to hide
//! what is above it, or write to the clipboard (OSC 52) on terminals that allow
//! it. Plain-text output therefore passes catalog-derived text through
//! [`display_safe`]. JSON and CSV output stay byte-exact: JSON already escapes
//! control characters, and CSV is for machines.

use std::borrow::Cow;

/// Replace characters that a terminal would act on with visible placeholders.
///
/// Escaped: the C0 controls (shown as their Unicode "control picture", `␛`
/// for ESC), DEL (`␡`), the C1 controls U+0080–U+009F (as `\x9b`, since the
/// 8-bit CSI/OSC introducers work on some terminals) and the bidirectional
/// override/embedding/isolate controls that reorder text on screen
/// (`\u{202e}`). Newline and tab are kept, for multi-line values; use
/// [`display_safe_inline`] for a value that must stay on one line.
pub fn display_safe(text: &str) -> Cow<'_, str> {
    escape(text, true)
}

/// [`display_safe`] for a value that must stay on one line (a path, an owner):
/// newline and tab are replaced too, so a file named `a\nERROR …` cannot forge
/// an output line.
pub fn display_safe_inline(text: &str) -> Cow<'_, str> {
    escape(text, false)
}

fn escape(text: &str, keep_whitespace: bool) -> Cow<'_, str> {
    let needs_escape = |c: char| is_dangerous(c) && !(keep_whitespace && matches!(c, '\n' | '\t'));
    if !text.chars().any(needs_escape) {
        return Cow::Borrowed(text);
    }
    let mut out = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        if !needs_escape(c) {
            out.push(c);
        } else if let Some(picture) = control_picture(c) {
            out.push(picture);
        } else if (c as u32) <= 0xff {
            out.push_str(&format!("\\x{:02x}", c as u32));
        } else {
            out.push_str(&format!("\\u{{{:x}}}", c as u32));
        }
    }
    Cow::Owned(out)
}

/// Whether a terminal (or a reader) could act on `c` rather than just show it.
fn is_dangerous(c: char) -> bool {
    matches!(
        c,
        '\u{0000}'..='\u{001f}' // C0
            | '\u{007f}' // DEL
            | '\u{0080}'..='\u{009f}' // C1
            | '\u{202a}'..='\u{202e}' // bidi embedding / override
            | '\u{2066}'..='\u{2069}' // bidi isolates
    )
}

/// U+2400.. "control pictures" for C0 and DEL.
fn control_picture(c: char) -> Option<char> {
    match c as u32 {
        code @ 0x00..=0x1f => char::from_u32(0x2400 + code),
        0x7f => Some('\u{2421}'),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clean_text_is_returned_untouched() {
        for text in [
            "",
            "plain",
            "tabs\tand\nnewlines",
            "ünïcödé — 日本語 ✓",
            "a\u{200e}b",
        ] {
            assert!(matches!(display_safe(text), Cow::Borrowed(_)), "{text:?}");
        }
    }

    #[test]
    fn escape_sequences_become_visible_placeholders() {
        assert_eq!(display_safe("\x1b]0;x\x07"), "␛]0;x␇");
        assert_eq!(display_safe("a\x1b[2Jb"), "a␛[2Jb");
        assert_eq!(display_safe("over\rwrite"), "over␍write");
        assert_eq!(display_safe("nul\0del\x7f"), "nul␀del␡");
        assert_eq!(display_safe("\u{9b}31m"), "\\x9b31m", "8-bit CSI");
        assert_eq!(display_safe("a\u{202e}b"), "a\\u{202e}b");
        assert_eq!(display_safe("\u{2066}x"), "\\u{2066}x");
    }

    #[test]
    fn multi_line_values_keep_newline_and_tab_unless_inline() {
        assert_eq!(display_safe("a\nb\tc"), "a\nb\tc");
        assert_eq!(display_safe_inline("a\nb\tc"), "a␊b␉c");
        assert_eq!(display_safe_inline("x\x1by"), "x␛y");
    }

    #[test]
    fn nothing_dangerous_survives() {
        let nasty: String = ('\u{0}'..='\u{9f}')
            .chain('\u{202a}'..='\u{202e}')
            .chain('\u{2066}'..='\u{2069}')
            .collect();
        for text in [display_safe(&nasty), display_safe_inline(&nasty)] {
            assert!(
                text.chars()
                    .all(|c| !is_dangerous(c) || matches!(c, '\n' | '\t')),
                "{text:?}"
            );
        }
        assert!(
            display_safe_inline(&nasty)
                .chars()
                .all(|c| !is_dangerous(c))
        );
    }
}
