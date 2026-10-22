//! Keyboard-accessible structured search settings. Changes apply atomically.
use super::search_worker::{SearchMode, SearchOptions};
use super::{KeyCode, KeyEvent, TuiApp};

pub(super) struct Editor {
    pub fields: [String; 6],
    pub selected: usize,
    pub error: Option<String>,
}
impl TuiApp {
    pub(super) fn open_search_settings(&mut self) {
        let o = &self.search_options;
        self.search_settings = Some(Editor {
            fields: [
                match o.mode {
                    SearchMode::Text => "text",
                    SearchMode::Regex => "regex",
                    SearchMode::Function => "function",
                    SearchMode::Declares => "declares",
                }
                .into(),
                o.role.clone().unwrap_or_default(),
                o.lang.clone().unwrap_or_default(),
                o.owner.clone().unwrap_or_default(),
                o.tag.clone().unwrap_or_default(),
                self.result_limit.to_string(),
            ],
            selected: 0,
            error: None,
        });
    }
    pub(super) fn handle_settings_key(&mut self, key: KeyEvent) {
        let editor = self.search_settings.as_mut().unwrap();
        if key.code == KeyCode::Char('u') && key.modifiers == super::KeyModifiers::CONTROL {
            editor.fields[editor.selected].clear();
            return;
        }
        match key.code {
            KeyCode::Esc => self.search_settings = None,
            KeyCode::Tab | KeyCode::Down => editor.selected = (editor.selected + 1) % 6,
            KeyCode::BackTab | KeyCode::Up => editor.selected = (editor.selected + 5) % 6,
            KeyCode::Backspace => {
                editor.fields[editor.selected].pop();
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(super::KeyModifiers::CONTROL | super::KeyModifiers::ALT) =>
            {
                editor.fields[editor.selected].push(c);
            }
            KeyCode::Enter => match editor.parse() {
                Ok((options, limit)) => {
                    self.search_options = options;
                    self.result_limit = limit;
                    self.search_settings = None;
                    self.schedule_query();
                }
                Err(error) => editor.error = Some(error),
            },
            _ => {}
        }
    }
}
impl Editor {
    fn parse(&self) -> Result<(SearchOptions, usize), String> {
        let mode = match self.fields[0].trim() {
            "text" => SearchMode::Text,
            "regex" => SearchMode::Regex,
            "function" => SearchMode::Function,
            "declares" => SearchMode::Declares,
            _ => return Err("Mode must be text, regex, function or declares".into()),
        };
        let value = |i: usize| {
            let s = self.fields[i].trim();
            (!s.is_empty()).then(|| s.to_owned())
        };
        let role = value(1);
        if let Some(role) = &role {
            if mode != SearchMode::Declares {
                return Err("Clear role or select declares mode".into());
            }
            if ![
                "parentfile",
                "childfile",
                "inputfile",
                "outputfile",
                "paramfile",
            ]
            .contains(&role.as_str())
            {
                return Err(
                    "Role: parentfile, childfile, inputfile, outputfile or paramfile".into(),
                );
            }
        }
        let limit = self.fields[5]
            .parse::<usize>()
            .map_err(|_| "Limit must be a positive integer")?;
        scat_core::core::db::validate_search_limit(limit).map_err(|e| e.to_string())?;
        Ok((
            SearchOptions {
                mode,
                role,
                lang: value(2),
                owner: value(3),
                tag: value(4),
            },
            limit,
        ))
    }
}

pub(super) fn draw(frame: &mut ratatui::Frame<'_>, app: &TuiApp) {
    use ratatui::{
        layout::Rect,
        widgets::{Block, Borders, Clear, Paragraph},
    };
    let Some(editor) = &app.search_settings else {
        return;
    };
    let screen = frame.area();
    let width = screen.width.min(92);
    let height = screen.height.min(14);
    let area = Rect::new(
        (screen.width - width) / 2,
        (screen.height - height) / 2,
        width,
        height,
    );
    let mut lines = vec![
        "Tab/Up/Down: field | Ctrl+U: clear | Enter: apply | Esc: cancel".to_owned(),
        "Modes: text, regex, function, declares (query stays in search box)".into(),
        "Roles: parentfile, childfile, inputfile, outputfile, paramfile".into(),
    ];
    for (i, label) in [
        "Mode",
        "Role (blank = all)",
        "Language",
        "Owner",
        "Tag",
        "Result limit",
    ]
    .iter()
    .enumerate()
    {
        lines.push(format!(
            "{} {label}: {}",
            if i == editor.selected { ">" } else { " " },
            editor.fields[i]
        ));
    }
    if let Some(error) = &editor.error {
        lines.push(error.clone());
    }
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines.join("\n")).block(
            Block::default()
                .borders(Borders::ALL)
                .title("Search settings"),
        ),
        area,
    );
}
