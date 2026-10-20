//! Conservative, query-free terminal configuration shared by every TUI view.
use std::io::IsTerminal;

use clap::ValueEnum;
use ratatui::{
    buffer::Buffer,
    style::{Color, Modifier},
};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum GlyphMode {
    #[default]
    Auto,
    Unicode,
    Ascii,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ColorMode {
    #[default]
    Auto,
    Full,
    Reduced,
    Mono,
}

#[derive(Debug, Clone)]
pub struct Capabilities {
    pub glyphs: GlyphMode,
    pub colors: ColorMode,
    locale: String,
    term: String,
    ssh: bool,
    multiplexer: &'static str,
    interactive: bool,
}

impl Default for Capabilities {
    fn default() -> Self {
        Self {
            glyphs: GlyphMode::Unicode,
            colors: ColorMode::Full,
            locale: String::new(),
            term: String::new(),
            ssh: false,
            multiplexer: "none detected",
            interactive: true,
        }
    }
}

/// Resolve a mode from its flag/env value, then the config file's name for it.
/// Config names are validated here so a typo is reported, not ignored.
pub fn resolve_mode<T: ValueEnum + Default>(
    explicit: Option<T>,
    configured: Option<&str>,
    key: &str,
) -> anyhow::Result<T> {
    if let Some(mode) = explicit {
        return Ok(mode);
    }
    match configured {
        Some(name) => T::from_str(name, true)
            .map_err(|err| anyhow::anyhow!("invalid tui.{key} {name:?} in config file: {err}")),
        None => Ok(T::default()),
    }
}

impl Capabilities {
    pub fn detect(glyphs: GlyphMode, colors: ColorMode, no_color: bool) -> Self {
        Self::resolve(
            glyphs,
            colors,
            no_color,
            |key| std::env::var(key).ok(),
            std::io::stdin().is_terminal() && std::io::stdout().is_terminal(),
        )
    }

    fn resolve(
        glyphs: GlyphMode,
        colors: ColorMode,
        no_color: bool,
        env: impl Fn(&str) -> Option<String>,
        interactive: bool,
    ) -> Self {
        let locale = ["LC_ALL", "LC_CTYPE", "LANG"]
            .into_iter()
            .filter_map(&env)
            .find(|s| !s.is_empty())
            .unwrap_or_default();
        let term = env("TERM").unwrap_or_default();
        let utf8 = locale
            .to_ascii_lowercase()
            .replace('-', "")
            .contains("utf8");
        let glyphs = match glyphs {
            GlyphMode::Auto if utf8 => GlyphMode::Unicode,
            GlyphMode::Auto => GlyphMode::Ascii,
            explicit => explicit,
        };
        let colors = if no_color {
            ColorMode::Mono
        } else {
            match colors {
                ColorMode::Auto
                    if term.contains("256color")
                        || env("COLORTERM")
                            .is_some_and(|v| matches!(v.as_str(), "truecolor" | "24bit")) =>
                {
                    ColorMode::Full
                }
                ColorMode::Auto => ColorMode::Reduced,
                explicit => explicit,
            }
        };
        Self {
            glyphs,
            colors,
            locale,
            term,
            interactive,
            ssh: env("SSH_CONNECTION").is_some() || env("SSH_TTY").is_some(),
            multiplexer: if env("TMUX").is_some()
                || env("TERM").is_some_and(|v| v.starts_with("tmux"))
            {
                "tmux"
            } else if env("STY").is_some() || env("TERM").is_some_and(|v| v.starts_with("screen")) {
                "screen-compatible (possibly tmux)"
            } else {
                "none detected"
            },
        }
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.interactive,
            "TUI requires terminal stdin and stdout; use `scat search` for noninteractive output"
        );
        anyhow::ensure!(
            !self.term.is_empty() && self.term != "dumb",
            "TUI requires a cursor-addressable terminal (TERM is unset or dumb); use `scat search` instead"
        );
        Ok(())
    }

    pub fn report(&self) -> String {
        format!(
            "TERM={:?}; locale={:?}; interactive={}; glyphs={:?}; colors={:?}\nSSH detected={}; multiplexer={}\nGlyph/color support is inferred, not probed. Clipboard availability: unknown. F3 sends an OSC 52 request; terminal permissions and multiplexer forwarding determine delivery. F2 freezes redraws and releases mouse capture; use your local terminal's Copy shortcut, then F2/Esc to resume. Paste uses your terminal's Paste shortcut (bracketed paste supported).",
            self.term,
            self.locale,
            self.interactive,
            self.glyphs,
            self.colors,
            self.ssh,
            self.multiplexer
        )
    }

    /// Normalize the completed frame, including cached syntax and overlays.
    pub(super) fn apply(&self, buffer: &mut Buffer) {
        for cell in &mut buffer.content {
            if self.colors == ColorMode::Mono {
                // Backgrounds mark selected rows/query ranges and active tabs.
                if cell.bg != Color::Reset {
                    cell.modifier.insert(Modifier::REVERSED);
                }
                cell.fg = Color::Reset;
                cell.bg = Color::Reset;
                cell.underline_color = Color::Reset;
            } else if self.colors == ColorMode::Reduced {
                cell.fg = reduced(cell.fg);
                cell.bg = reduced(cell.bg);
                cell.underline_color = reduced(cell.underline_color);
            }
            if self.glyphs == GlyphMode::Ascii && !cell.symbol().is_ascii() {
                // Keep the occupied cell width stable. Non-ASCII catalog text
                // is represented with '?' in this explicitly limited mode.
                let c = cell.symbol().chars().next().unwrap_or(' ');
                let replacement = match c {
                    '─' | '━' | '═' | '╌' | '┄' => "-",
                    '│' | '┃' | '║' => "|",
                    '\u{2500}'..='\u{257f}' => "+",
                    '→' | '▶' | '▸' | '►' | '›' => ">",
                    '←' | '◀' | '◂' | '‹' => "<",
                    '↑' | '▲' | '△' => "^",
                    '↓' | '▼' | '▽' => "v",
                    '…' => ".",
                    '✓' | '✔' => "+",
                    '✗' | '✘' | '×' => "x",
                    '⚠' => "!",
                    '\u{2800}'..='\u{28ff}' => "*",
                    '\u{2580}'..='\u{259f}' => "#",
                    '·' | '•' | '●' => "*",
                    _ => "?",
                };
                cell.set_symbol(replacement);
            }
        }
    }
}

fn reduced(color: Color) -> Color {
    match color {
        Color::Rgb(r, g, b) => {
            if r.max(g).max(b) < 80 {
                Color::Black
            } else {
                match (r >= 128, g >= 128, b >= 128) {
                    (true, true, true) => Color::White,
                    (true, true, false) => Color::Yellow,
                    (true, false, true) => Color::Magenta,
                    (false, true, true) => Color::Cyan,
                    (true, false, false) => Color::Red,
                    (false, true, false) => Color::Green,
                    (false, false, true) => Color::Blue,
                    _ => Color::White,
                }
            }
        }
        Color::Indexed(n) => reduced(if n < 16 {
            const RGB: [(u8, u8, u8); 8] = [
                (0, 0, 0),
                (192, 0, 0),
                (0, 192, 0),
                (192, 192, 0),
                (0, 0, 192),
                (192, 0, 192),
                (0, 192, 192),
                (192, 192, 192),
            ];
            let (r, g, b) = RGB[usize::from(n % 8)];
            Color::Rgb(r, g, b)
        } else if n >= 232 {
            let v = 8 + (n - 232) * 10;
            Color::Rgb(v, v, v)
        } else {
            let n = n - 16;
            let level = |v| if v == 0 { 0 } else { 55 + 40 * v };
            Color::Rgb(level(n / 36), level(n / 6 % 6), level(n % 6))
        }),
        Color::DarkGray => Color::White,
        Color::Gray | Color::White => Color::White,
        Color::LightRed => Color::Red,
        Color::LightGreen => Color::Green,
        Color::LightYellow => Color::Yellow,
        Color::LightBlue => Color::Blue,
        Color::LightMagenta => Color::Magenta,
        Color::LightCyan => Color::Cyan,
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn locale_precedence_and_overrides() {
        let env = |key: &str| match key {
            "LC_ALL" => Some("C".into()),
            "LANG" => Some("en_US.UTF-8".into()),
            "TERM" => Some("xterm-256color".into()),
            "TMUX" => Some("yes".into()),
            _ => None,
        };
        let c = Capabilities::resolve(GlyphMode::Auto, ColorMode::Auto, false, env, true);
        assert_eq!(c.glyphs, GlyphMode::Ascii);
        assert_eq!(c.colors, ColorMode::Full);
        assert_eq!(c.multiplexer, "tmux");
        let c = Capabilities::resolve(GlyphMode::Unicode, ColorMode::Full, true, env, true);
        assert_eq!(c.glyphs, GlyphMode::Unicode);
        assert_eq!(c.colors, ColorMode::Mono);
    }
    #[test]
    fn rejects_unsuitable_terminals() {
        let c = Capabilities::resolve(
            GlyphMode::Auto,
            ColorMode::Auto,
            false,
            |_| Some("dumb".into()),
            true,
        );
        assert!(c.validate().is_err());
        let c = Capabilities::resolve(
            GlyphMode::Auto,
            ColorMode::Auto,
            false,
            |_| Some("xterm".into()),
            false,
        );
        assert!(c.validate().is_err());
    }
    #[test]
    fn frame_conversion_preserves_selection_and_focus() {
        let mut b = Buffer::empty(ratatui::layout::Rect::new(0, 0, 3, 1));
        b[(0, 0)]
            .set_symbol("┌")
            .set_fg(Color::Yellow)
            .set_style(ratatui::style::Style::new().bold());
        b[(1, 0)].set_symbol("⠋").set_bg(Color::Blue);
        b[(2, 0)].set_symbol("█").set_fg(Color::Rgb(30, 200, 150));
        let c = Capabilities {
            glyphs: GlyphMode::Ascii,
            colors: ColorMode::Mono,
            ..Default::default()
        };
        c.apply(&mut b);
        assert!(
            b.content
                .iter()
                .all(|c| c.symbol().is_ascii() && c.fg == Color::Reset && c.bg == Color::Reset)
        );
        assert!(b[(0, 0)].modifier.contains(Modifier::BOLD));
        assert!(b[(1, 0)].modifier.contains(Modifier::REVERSED));
    }
}
