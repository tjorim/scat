//! Read-only CLI workflows, captured off the event loop. No shell is involved:
//! clap validates the same arguments as the CLI before a child is started.
use super::{JsonRow, KeyCode, KeyEvent, KeyModifiers, TuiApp};
use crate::cli::{CatalogCommands, Cli, Commands, OutputFormat, SearchOutput};
use anyhow::{Context, Result, bail, ensure};
use clap::{CommandFactory, FromArgMatches};
use std::{
    path::{Path, PathBuf},
    sync::mpsc,
};

pub(super) struct Worker {
    tx: mpsc::Sender<Job>,
    rx: mpsc::Receiver<Result<Completed, String>>,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
struct Completed {
    output: Option<String>,
    message: String,
}
enum Job {
    Run {
        command: String,
        displayed: Option<Vec<JsonRow>>,
    },
    Save {
        path: PathBuf,
        text: String,
    },
}
pub(super) struct Dialog {
    pub fields: [String; 3],
    pub selected: usize,
    pub output: String,
    pub message: String,
    pub busy: bool,
    pub scroll: u16,
    help: bool,
    help_scroll: u16,
    administrative: bool,
    prepared: Option<(String, Vec<String>)>,
    displayed: Vec<JsonRow>,
    worker: Worker,
}

fn parse(command: &str) -> Result<(Vec<String>, Cli)> {
    let args = shell_words::split(command)?;
    // Do not let SCAT_DB/SCAT_CONFIG env defaults appear as user-supplied
    // overrides. The worker receives the already-resolved TUI sources.
    let matches = Cli::command()
        .mut_args(|arg| arg.env(None::<&str>))
        .try_get_matches_from(std::iter::once("scat".to_owned()).chain(args.clone()))?;
    let cli = Cli::from_arg_matches(&matches)?;
    ensure!(
        cli.db.is_none()
            && cli.embeddings.is_none()
            && cli.config.is_none()
            && (matches!(cli.command, Commands::Doctor { .. })
                || (cli.cache_dir.is_none() && !cli.no_cache))
            && !cli.no_color
            && cli.verbose == 0,
        "Global options are fixed to the active TUI catalog and configuration"
    );
    match &cli.command {
        Commands::Doctor { probe: None, .. } => {}
        Commands::Search { output, limit, .. } => {
            ensure!(
                *output != SearchOutput::Table,
                "Choose --output json or csv"
            );
            ensure!(
                *limit > 0 && i64::try_from(*limit).is_ok(),
                "Limit must be positive and within SQLite range"
            );
            ensure!(
                args.iter()
                    .any(|a| a == "--limit" || a.starts_with("--limit=")),
                "Requested searches require an explicit --limit (no hidden default)"
            );
        }
        Commands::Similar { output, limit, .. } => {
            ensure!(*output == OutputFormat::Json, "Choose --output json");
            ensure!(
                *limit > 0
                    && args
                        .iter()
                        .any(|a| a == "--limit" || a.starts_with("--limit=")),
                "Similarity exports require an explicit positive --limit"
            );
        }
        Commands::Show { output, .. }
        | Commands::Status { output, .. }
        | Commands::Deps { output, .. }
        | Commands::Symlinks { output, .. } => {
            ensure!(*output == OutputFormat::Json, "Choose --output json");
        }
        Commands::Catalog {
            command:
                CatalogCommands::Info { .. }
                | CatalogCommands::Audit { .. }
                | CatalogCommands::Stats { .. },
        } => {}
        Commands::Diff { .. }
        | Commands::Catalog {
            command: CatalogCommands::Diff { .. },
        } => {}
        _ => bail!("Choose a read-only command; administration uses its separate entry points"),
    }
    Ok((args, cli))
}
fn prepare_administration(
    command: &str,
    sources: &(PathBuf, PathBuf),
    config: Option<&Path>,
) -> Result<(Vec<String>, String)> {
    let args = shell_words::split(command)?;
    let matches = Cli::command()
        .mut_args(|arg| arg.env(None::<&str>))
        .try_get_matches_from(std::iter::once("scat".to_owned()).chain(args.clone()))?;
    let cli = Cli::from_arg_matches(&matches)?;
    ensure!(
        cli.db.is_none()
            && cli.config.is_none()
            && cli.embeddings.is_none()
            && cli.cache_dir.is_none()
            && !cli.no_cache
            && !cli.no_color
            && cli.verbose == 0,
        "Global settings are fixed to the active TUI sources"
    );
    ensure!(
        matches!(
            cli.command,
            Commands::Catalog {
                command: CatalogCommands::Build { .. }
            } | Commands::Vc { .. }
        ),
        "Only catalog build or vc belongs in administration"
    );
    let mut full = vec![
        "--db".into(),
        sources.0.to_string_lossy().into_owned(),
        "--embeddings".into(),
        sources.1.to_string_lossy().into_owned(),
    ];
    if let Some(config) = config {
        full.extend(["--config".into(), config.to_string_lossy().into_owned()]);
    }
    full.extend(args);
    let mut preview = format!(
        "Executable: {:?}\nArgument vector: {full:#?}\n",
        std::env::current_exe()?
    );
    if let Commands::Catalog {
        command:
            CatalogCommands::Build {
                ref scan_root,
                head_lines,
                ref ignore_file,
                keep_copies,
                dry_run,
                quiet,
                no_resume,
                force,
                no_incremental,
                threads,
                json,
            },
    } = cli.command
    {
        let cfg = scat_core::core::vc::load_vc_config(config)?;
        let roots = if scan_root.is_empty() {
            &cfg.scan_roots
        } else {
            scan_root
        };
        preview.push_str(&format!("Scan roots: {roots:#?}\nHeader lines: {head_lines}; ignore files: {ignore_file:?}\nConfig ignore patterns: {:?}\nCopies retained: {keep_copies}; dry run: {dry_run}; quiet: {quiet}\nNo resume: {no_resume}; force: {force}; no incremental: {no_incremental}\nThreads: {threads:?} (None = logical CPU count); JSON: {json}\n", cfg.ignore_patterns));
    }
    if let Commands::Vc { args, .. } = cli.command {
        let cfg = scat_core::core::vc::load_vc_config(config)?;
        preview.push_str(&format!(
            "vc executable: {:?}\nvc argv: {args:#?}\n",
            cfg.vc_executable.or_else(crate::runtime::which_vc)
        ));
    }
    Ok((full, preview))
}

fn render_displayed(cli: &Cli, rows: &[JsonRow]) -> Result<String> {
    match &cli.command {
        Commands::Search {
            fields,
            output: SearchOutput::Csv,
            ..
        } => Ok(crate::output::render_script_csv(rows, fields)),
        Commands::Search {
            fields,
            output: SearchOutput::Json,
            ..
        } => Ok(
            serde_json::to_string_pretty(&crate::output::script_rows_to_json(rows, fields))? + "\n",
        ),
        _ => bail!(
            "Displayed scope is only for the captured search rows; use requested for other commands"
        ),
    }
}
fn run(
    executable: &Path,
    db: &Path,
    embeddings: &Path,
    command: &str,
    displayed: Option<&[JsonRow]>,
    config: Option<&Path>,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Result<Completed> {
    let (args, cli) = parse(command)?;
    if let Some(rows) = displayed {
        return Ok(Completed {
            output: Some(render_displayed(&cli, rows)?),
            message: format!(
                "Exported {} captured search rows; filters and limit in the command are ignored in displayed scope",
                rows.len()
            ),
        });
    }
    let mut child = std::process::Command::new(executable);
    if let Some(config) = config {
        child.arg("--config").arg(config);
    }
    if matches!(cli.command, Commands::Doctor { .. }) {
        child.arg("--db").arg(db).args(args);
        let result =
            crate::doctor::capture_cancel(&mut child, std::time::Duration::from_secs(50), cancel);
        return Ok(Completed {
            output: result["stdout"].as_str().map(str::to_owned),
            message: if result["exit_code"] == 0 {
                "Host diagnostics complete; Esc closes".into()
            } else {
                "Diagnostics canceled, unavailable or timed out".into()
            },
        });
    }
    let output = child
        .arg("--db")
        .arg(db)
        .arg("--embeddings")
        .arg(embeddings)
        .arg("--no-color")
        .args(args)
        .output()
        .context("Could not start comparison/export worker")?;
    ensure!(
        output.status.success()
            || matches!(
                cli.command,
                Commands::Catalog {
                    command: CatalogCommands::Audit { .. }
                }
            ),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // Warnings must stay visible; successful structured stdout is preserved verbatim.
    Ok(Completed {
        output: Some(String::from_utf8(output.stdout).context("Output is not UTF-8")?),
        message: if matches!(
            cli.command,
            Commands::Catalog {
                command: CatalogCommands::Audit { .. }
            }
        ) {
            format!(
                "Audit exit {} (0: pass; 1: strict warnings; 2: errors). {}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            )
        } else {
            String::from_utf8_lossy(&output.stderr).into_owned()
        },
    })
}
fn save(path: &Path, text: &str) -> Result<()> {
    use std::io::Write;
    ensure!(path.is_absolute(), "Choose an absolute local output path");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| {
            format!(
                "Cannot create {} (existing files are never overwritten)",
                path.display()
            )
        })?;
    file.write_all(text.as_bytes())?;
    file.flush()?;
    Ok(())
}
impl Worker {
    fn new(db: PathBuf, embeddings: PathBuf, config: Option<PathBuf>) -> Result<Self> {
        let executable = std::env::current_exe()?;
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let worker_cancel = cancel.clone();
        let (tx, jobs) = mpsc::channel();
        let (results, rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("tui-workflows".into())
            .spawn(move || {
                while let Ok(job) = jobs.recv() {
                    let result = match job {
                        Job::Run { command, displayed } => run(
                            &executable,
                            &db,
                            &embeddings,
                            &command,
                            displayed.as_deref(),
                            config.as_deref(),
                            Some(&worker_cancel),
                        ),
                        Job::Save { path, text } => save(&path, &text).map(|()| Completed {
                            output: None,
                            message: format!("Saved to {}", path.display()),
                        }),
                    }
                    .map_err(|e| format!("{e:#}"));
                    if results.send(result).is_err() {
                        break;
                    }
                }
            })?;
        Ok(Self { tx, rx, cancel })
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.cancel
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
}
impl TuiApp {
    pub(super) fn open_workflow(&mut self, comparison: bool) {
        let Some((db, embeddings)) = self.workflow_sources.clone() else {
            return;
        };
        let path = self.selected_logical_path().unwrap_or_default();
        let quote = |s: &str| shell_words::quote(s).into_owned();
        let (command, scope) = if comparison {
            (format!("diff {}", quote(&path)), "requested")
        } else if let Some(page) = &self.inspection {
            (page.export_command(), "requested")
        } else if self.mode == super::ViewMode::Detail || self.mode == super::ViewMode::DepGraph {
            (
                format!(
                    "{} {} --output json",
                    if self.mode == super::ViewMode::DepGraph {
                        "deps"
                    } else {
                        "show"
                    },
                    quote(&path)
                ),
                "requested",
            )
        } else {
            (self.search_export_command(), "displayed")
        };
        match Worker::new(db, embeddings, self.workflow_config.clone()) {
            Ok(worker) => {
                self.workflow = Some(Dialog {
                    fields: [command, scope.into(), String::new()],
                    selected: 0,
                    output: String::new(),
                    message: format!(
                        "Captured {} search rows (TUI limit {}). Requested scope runs a fresh query with the command's limit.",
                        self.results.len(),
                        self.result_limit
                    ),
                    busy: false,
                    scroll: 0,
                    help: false,
                    help_scroll: 0,
                    administrative: false,
                    prepared: None,
                    displayed: self.results.clone(),
                    worker,
                });
            }
            Err(e) => self.flash = Some(format!("Workflow failed: {e}")),
        }
    }
    pub(super) fn open_catalog_workflow(&mut self, kind: u8) {
        self.open_workflow(false);
        let selected = self
            .selected_logical_path()
            .map(|p| self.resolver.to_native(&p));
        if let Some(d) = &mut self.workflow {
            d.fields[0] = match kind {
                0 => "catalog info --json",
                1 => "catalog audit --json",
                2 => "catalog build --dry-run",
                4 => "doctor",
                _ => "vc --",
            }
            .into();
            if kind == 4
                && let Some(path) = selected
            {
                d.fields[0] = format!("doctor {} --rpm", shell_words::quote(&path));
            }
            if kind == 4 {
                for flag in &self.doctor_flags {
                    d.fields[0].push(' ');
                    d.fields[0].push_str(&shell_words::quote(flag));
                }
            }
            d.fields[1] = "requested".into();
            d.administrative = matches!(kind, 2 | 3);
            d.message = "Enter: inspect/run read-only request. Administration: Enter previews; F5 executes that exact preview. F1: options.".into();
        }
    }
    pub(super) fn take_administrative_command(&mut self) -> Option<Vec<String>> {
        self.pending_administration.take()
    }
    fn search_export_command(&self) -> String {
        use super::search_worker::{SearchMode, parse_query_filters};
        let query = self.expanded_query.as_deref().unwrap_or(&self.query);
        let o = &self.search_options;
        let p = if o.mode == SearchMode::Text {
            parse_query_filters(query)
        } else {
            super::search_worker::ParsedQuery {
                text: query.into(),
                ..Default::default()
            }
        };
        let mut args = vec!["search".to_owned()];
        if !p.text.is_empty() {
            match o.mode {
                SearchMode::Text => {}
                SearchMode::Regex => args.push("--regex".into()),
                SearchMode::Function => args.push("--function".into()),
                SearchMode::Declares => args.push("--declares".into()),
            }
            args.push(p.text);
        }
        for (flag, value) in [
            ("--lang", o.lang.as_ref().or(p.lang.as_ref())),
            ("--owner", o.owner.as_ref().or(p.owner.as_ref())),
            ("--tag", o.tag.as_ref().or(p.tag.as_ref())),
            ("--role", o.role.as_ref()),
        ] {
            if let Some(value) = value {
                args.extend([flag.into(), value.clone()]);
            }
        }
        args.extend([
            "--limit".into(),
            self.result_limit.to_string(),
            "--output".into(),
            "json".into(),
        ]);
        shell_words::join(args)
    }
    pub(super) fn workflow_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Esc {
            self.workflow = None;
            return;
        }
        let d = self.workflow.as_mut().unwrap();
        if key.code == KeyCode::F(1) {
            d.help = !d.help;
            return;
        }
        if d.help {
            match key.code {
                KeyCode::PageDown | KeyCode::Down => {
                    d.help_scroll = d.help_scroll.saturating_add(10);
                }
                KeyCode::PageUp | KeyCode::Up => d.help_scroll = d.help_scroll.saturating_sub(10),
                _ => {}
            }
            return;
        }
        if d.busy {
            return;
        }
        if d.administrative && key.code == KeyCode::F(5) {
            if let Some((text, args)) = d.prepared.take().filter(|(text, _)| text == &d.fields[0]) {
                self.pending_administration = Some(args);
                d.message = format!("Running {text}");
            } else {
                d.message = "Press Enter to preview the current command first".into();
            }
            return;
        }
        match key.code {
            KeyCode::Tab | KeyCode::Down => d.selected = (d.selected + 1) % 3,
            KeyCode::BackTab | KeyCode::Up => d.selected = (d.selected + 2) % 3,
            KeyCode::PageDown => {
                let last =
                    u16::try_from(d.output.lines().count().saturating_sub(1)).unwrap_or(u16::MAX);
                d.scroll = d.scroll.saturating_add(10).min(last);
            }
            KeyCode::PageUp => d.scroll = d.scroll.saturating_sub(10),
            KeyCode::F(3) if !d.output.is_empty() => {
                d.message = match super::clipboard::copy_to_clipboard(&d.output) {
                    Ok(()) => "Clipboard request sent to terminal (access depends on terminal permissions)".into(),
                    Err(e) => format!("Copy failed: {e}"),
                };
                self.force_full_redraw = true;
            }
            KeyCode::Char('s') if key.modifiers == KeyModifiers::CONTROL => {
                if d.output.is_empty() {
                    d.message = "Generate output with Enter before saving".into();
                    return;
                }
                let job = Job::Save {
                    path: d.fields[2].clone().into(),
                    text: d.output.clone(),
                };
                d.busy = d.worker.tx.send(job).is_ok();
                d.message = if d.busy {
                    "Saving…"
                } else {
                    "Worker disconnected"
                }
                .into();
            }
            KeyCode::Enter if d.administrative => {
                let result = prepare_administration(
                    &d.fields[0],
                    self.workflow_sources.as_ref().unwrap(),
                    self.workflow_config.as_deref(),
                );
                match result {
                    Ok((args, preview)) => {
                        d.output = preview;
                        d.prepared = Some((d.fields[0].clone(), args));
                        d.message = "Review target paths and argv above. F5 explicitly executes; Ctrl+C interrupts while running. Esc cancels. Restart TUI after publishing to refresh catalog.".into();
                    }
                    Err(e) => {
                        d.prepared = None;
                        d.message = format!("{e:#}");
                    }
                }
            }
            KeyCode::Enter => {
                let displayed = match d.fields[1].trim() {
                    "displayed" => Some(d.displayed.clone()),
                    "requested" => None,
                    _ => {
                        d.message = "Scope must be displayed or requested".into();
                        return;
                    }
                };
                d.output.clear();
                d.scroll = 0;
                d.busy = d
                    .worker
                    .tx
                    .send(Job::Run {
                        command: d.fields[0].clone(),
                        displayed,
                    })
                    .is_ok();
                d.message = if d.busy {
                    "Running…"
                } else {
                    "Worker disconnected"
                }
                .into();
            }
            KeyCode::Char('u') if key.modifiers == KeyModifiers::CONTROL => {
                d.fields[d.selected].clear();
                if d.selected != 2 {
                    d.output.clear();
                }
            }
            KeyCode::Backspace => {
                d.fields[d.selected].pop();
                if d.selected != 2 {
                    d.output.clear();
                }
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                d.fields[d.selected].push(c);
                if d.selected != 2 {
                    d.output.clear();
                }
            }
            _ => {}
        }
    }
    pub(super) fn drain_workflow(&mut self) {
        let Some(d) = &mut self.workflow else {
            return;
        };
        match d.worker.rx.try_recv() {
            Ok(result) => {
                d.busy = false;
                match result {
                    Ok(completed) => {
                        if let Some(text) = completed.output {
                            d.output = text;
                        }
                        d.message = format!(
                            "{}\nF3: request clipboard copy; Ctrl+S: save to chosen file",
                            completed.message
                        );
                    }
                    Err(e) => d.message = e,
                }
                self.needs_redraw = true;
            }
            Err(mpsc::TryRecvError::Disconnected) if d.busy => {
                d.busy = false;
                d.message = "Worker disconnected".into();
                self.needs_redraw = true;
            }
            _ => {}
        }
    }
}
pub(super) fn draw(frame: &mut ratatui::Frame<'_>, d: &Dialog) {
    use ratatui::{
        layout::{Constraint, Layout},
        style::{Color, Style},
        widgets::{Block, Borders, Clear, Paragraph, Wrap},
    };
    use unicode_width::UnicodeWidthStr;
    let area = frame.area();
    frame.render_widget(Clear, area);
    if d.help {
        let paragraph = Paragraph::new(
            "F1: return to editor · Esc: close dialog\n\nCatalog: catalog info --json; catalog audit --json --check unowned --stale-days 90\nAudit: --check repeatable; --strict; --near-duplicate-threshold 0.97; --outlier-threshold 0.3. Missing embeddings skips embedding checks.\nAdministration (separate palette entries): Enter previews, F5 executes.\nBuild: catalog build --scan-root '/scripts' --head-lines 10 --ignore-file /path/.catignore --keep-copies 3 --dry-run\nBuild controls: --no-resume --force --no-incremental --threads 4 --quiet --json\nvc: vc -- 'argument with spaces' '--literal-option' (no shell expansion).\nEnter read-only CLI arguments without the scat prefix; quote paths with spaces.\nCatalog script vs live file:\n  diff /catalog/a.py --against '/local/live file.py'\nTwo live files (no catalog content):\n  diff --old /local/old.py --new /local/new.py --json\nCatalog vs indexed revision read from its live path:\n  diff /catalog/a.py --revision-type archive --revision-timestamp 2024\n  diff /catalog/a.py --revision-type develop --revision-user bob\nWhole catalog snapshots (default previous snapshot is CATALOG.1):\n  catalog diff --json\n  catalog diff --against /local/previous.db --json\n  catalog diff --old /local/old.db --new /local/new.db --json\nFresh search: set scope to requested and choose an explicit limit:\n  search --lang python --limit 10000 --fields path,owner,tags --output csv\n  search --regex '.*tool.*' --limit 10000 --output json\nOther JSON commands: show, status, deps, symlinks, similar.\nOnly search supports CSV; --fields follows CLI support (search and show).\nDisplayed scope exports captured search rows; command filters/limits do not apply.\nRequested scope runs the CLI anew, using its matching semantics and limits.\nEnter generates output. F3 requests terminal clipboard access.\nCtrl+S writes generated output to the chosen absolute local file; never overwrites."
        )
        .wrap(Wrap { trim: false })
        .block(Block::default().borders(Borders::ALL).title("Workflow examples | PageUp/PageDown scroll"));
        let rows = paragraph.line_count(area.width.saturating_sub(2));
        let viewport = usize::from(area.height.saturating_sub(2));
        let scroll = usize::from(d.help_scroll).min(rows.saturating_sub(viewport));
        frame.render_widget(
            paragraph.scroll((u16::try_from(scroll).unwrap_or(u16::MAX), 0)),
            area,
        );
        super::render::draw_scrollbar(frame, area, rows, scroll, viewport);
        return;
    }
    let chunks = Layout::vertical([
        Constraint::Length(4),
        Constraint::Length(3),
        Constraint::Length(3),
        Constraint::Length(3),
        Constraint::Min(1),
        Constraint::Length(4),
    ])
    .split(area);
    frame.render_widget(Paragraph::new(
        "F1: examples · Tab: field · Ctrl+U: clear · Enter: preview/generate · F5: execute admin · Esc: close\nF3: clipboard request · Ctrl+S: save · PageUp/PageDown: output\nCLI args: diff PATH --against FILE; diff --old FILE --new FILE\nJSON: --output json / --json; search CSV: --output csv; --fields path,owner"
    ), chunks[0]);
    for (i, label) in [
        "Command / format / fields / requested limit (CLI args; no scat prefix)",
        "Scope: displayed = captured search rows; requested = fresh CLI request",
        "Save to chosen absolute local path (existing files never overwritten)",
    ]
    .iter()
    .enumerate()
    {
        let width = usize::from(chunks[i + 1].width.saturating_sub(2));
        let scroll = if i == d.selected {
            d.fields[i].width().saturating_sub(width)
        } else {
            0
        };
        frame.render_widget(
            Paragraph::new(d.fields[i].as_str())
                .scroll((0, scroll.min(u16::MAX as usize) as u16))
                .block(
                    Block::default()
                        .borders(Borders::ALL)
                        .title(*label)
                        .border_style(if i == d.selected {
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(ratatui::style::Modifier::BOLD)
                        } else {
                            Style::default()
                        }),
                ),
            chunks[i + 1],
        );
    }
    frame.render_widget(
        Paragraph::new(d.output.as_str())
            .scroll((d.scroll, 0))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Generated output (catalog content vs live comparison files)"),
            ),
        chunks[4],
    );
    super::render::draw_scrollbar(
        frame,
        chunks[4],
        d.output.lines().count(),
        usize::from(d.scroll),
        usize::from(chunks[4].height.saturating_sub(2)),
    );
    frame.render_widget(
        Paragraph::new(d.message.as_str())
            .wrap(Wrap { trim: false })
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title("Workflow status"),
            ),
        chunks[5],
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    #[test]
    fn administration_requires_preview_and_preserves_literal_arguments() {
        let db = super::super::make_test_db();
        let mut app = super::super::tests::make_app(db.path());
        app.workflow_sources = Some((db.path().to_owned(), PathBuf::from("/missing")));
        app.open_catalog_workflow(3);
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        app.workflow_key(key(KeyCode::F(5)));
        assert!(app.take_administrative_command().is_none());
        app.workflow.as_mut().unwrap().fields[0] = "vc -- 'a b' ';touch /tmp/no' '--json'".into();
        app.workflow_key(key(KeyCode::Enter));
        assert!(app.take_administrative_command().is_none());
        app.workflow_key(key(KeyCode::F(5)));
        let args = app.take_administrative_command().unwrap();
        assert_eq!(&args[4..], &["vc", "--", "a b", ";touch /tmp/no", "--json"]);
        app.workflow_key(key(KeyCode::Enter));
        app.workflow.as_mut().unwrap().fields[0].push_str(" changed");
        app.workflow_key(key(KeyCode::F(5)));
        assert!(app.take_administrative_command().is_none());
    }

    #[test]
    fn read_only_and_administrative_boundaries_are_disjoint() {
        let sources = (PathBuf::from("/catalog"), PathBuf::from("/embeddings"));
        assert!(parse("catalog info --json").is_ok());
        assert!(parse("catalog audit --check unowned --strict --stale-days 30 --near-duplicate-threshold 0.98 --outlier-threshold 0.2 --json").is_ok());
        assert!(parse("catalog build --dry-run").is_err());
        assert!(parse("vc -- status").is_err());
        assert!(prepare_administration("catalog info", &sources, None).is_err());
        assert!(prepare_administration("catalog build --db /other", &sources, None).is_err());
        assert!(prepare_administration("catalog build --scan-root '/a b' --head-lines 20 --ignore-file /ignore --keep-copies 2 --dry-run --no-resume --force --no-incremental --threads 2", &sources, None).is_ok());
    }

    #[test]
    fn audit_findings_survive_nonzero_exit() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("audit");
        std::fs::write(&exe, "#!/bin/sh\nprintf 'full audit findings'\nexit 2\n").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o700)).unwrap();
        let result = run(
            &exe,
            Path::new("/db"),
            Path::new("/missing"),
            "catalog audit --strict",
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(result.output.as_deref(), Some("full audit findings"));
        assert!(result.message.contains("2"));
    }

    #[test]
    fn accepts_all_comparisons_and_rejects_cli_conflicts_and_mutations() {
        for command in [
            "diff /a --against '/tmp/live file'",
            "diff --old /old --new /new --json",
            "diff /a --revision-type archive --revision-timestamp 2024",
            "diff /a --revision-type develop --revision-user bob",
            "catalog diff",
            "catalog diff --against /previous --json",
            "catalog diff --old /old --new /new",
            "show /a --fields owner,uses --output json",
            "status --all --output json",
            "deps /a --depth 3 --output json",
            "symlinks /a --output json",
            "similar /a --limit 40 --output json",
        ] {
            assert!(parse(command).is_ok(), "{command}");
        }
        for command in [
            "diff /a --against /b --revision-type archive",
            "diff --old /old",
            "diff /a --revision-user bob",
            "catalog build",
            "vc status",
            "tui",
            "search --output json",
            "search --limit 0 --output json",
            "search --limit 100",
            "show /a --output csv",
            "show /a --output json --db /other",
            "deps /a --depth 0 --output json",
        ] {
            assert!(parse(command).is_err(), "{command}");
        }
    }

    #[test]
    fn captured_export_uses_cli_serializers_and_field_order() {
        let rows = vec![
            serde_json::from_value(serde_json::json!({
                "logical_path": "/a,\"b\"", "owner": "one\ntwo"
            }))
            .unwrap(),
        ];
        let (_, cli) =
            parse("search ignored --limit 1 --fields owner,path,tags --output csv").unwrap();
        let csv = render_displayed(&cli, &rows).unwrap();
        assert_eq!(
            csv,
            crate::output::render_script_csv(
                &rows,
                &["owner".into(), "path".into(), "tags".into()]
            )
        );
        assert!(csv.starts_with("owner,path,tags\n\"one\ntwo\""));
        let (_, cli) = parse("search --limit 1 --fields tags,path --output json").unwrap();
        let json: serde_json::Value =
            serde_json::from_str(&render_displayed(&cli, &rows).unwrap()).unwrap();
        assert_eq!(json[0]["tags"], serde_json::json!([]));
        assert_eq!(json[0]["path"], "/a,\"b\"");
        assert!(json[0].get("owner").is_none());
        assert!(render_displayed(&parse("status --all --output json").unwrap().1, &rows).is_err());
    }

    #[test]
    fn save_never_overwrites_files_or_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("export.json");
        save(&path, "first").unwrap();
        assert!(save(&path, "second").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first");
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert!(save(&link, "second").is_err());
        assert!(save(Path::new("relative.json"), "text").is_err());
        assert!(save(&dir.path().join("missing/child"), "text").is_err());
    }

    #[test]
    fn worker_failures_stay_in_dialog_and_closing_preserves_browsing() {
        let db = super::super::make_test_db();
        let mut app = super::super::tests::make_app(db.path());
        app.workflow_sources = Some((db.path().to_owned(), PathBuf::from("/missing")));
        app.mode = super::super::ViewMode::Detail;
        app.focus = super::super::Focus::Revisions;
        app.revisions_selected = 3;
        app.detail_scroll = 12;
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        app.handle_key(key(KeyCode::F(11))).unwrap();
        app.workflow.as_mut().unwrap().fields[0] = "diff --old /a".into();
        app.handle_key(key(KeyCode::Enter)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while app.workflow.as_ref().unwrap().busy {
            app.drain_workflow();
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(app.workflow.as_ref().unwrap().message.contains("--new"));
        assert!(app.error.is_none());
        app.handle_key(key(KeyCode::Esc)).unwrap();
        assert!(app.workflow.is_none());
        assert_eq!(app.mode, super::super::ViewMode::Detail);
        assert_eq!(app.focus, super::super::Focus::Revisions);
        assert_eq!(app.revisions_selected, 3);
        assert_eq!(app.detail_scroll, 12);
    }

    #[test]
    fn export_dialog_renders_fields_at_standard_terminal_size_and_saves_off_thread() {
        let db = super::super::make_test_db();
        let mut app = super::super::tests::make_app(db.path());
        app.workflow_sources = Some((db.path().to_owned(), PathBuf::from("/missing")));
        app.open_workflow(false);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|frame| draw(frame, app.workflow.as_ref().unwrap()))
            .unwrap();
        let screen = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect::<String>();
        for label in [
            "Command / format",
            "Scope:",
            "Save to chosen",
            "Generated output",
            "Workflow status",
        ] {
            assert!(screen.contains(label), "missing {label}");
        }
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        app.workflow_key(key(KeyCode::Enter));
        let wait = |app: &mut TuiApp| {
            let deadline = Instant::now() + Duration::from_secs(3);
            while app.workflow.as_ref().unwrap().busy {
                app.drain_workflow();
                assert!(Instant::now() < deadline);
                std::thread::sleep(Duration::from_millis(1));
            }
        };
        wait(&mut app);
        let text = app.workflow.as_ref().unwrap().output.clone();
        assert!(serde_json::from_str::<serde_json::Value>(&text).is_ok());
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("output.json");
        app.workflow.as_mut().unwrap().selected = 2;
        app.workflow_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(app.workflow.as_ref().unwrap().output, text);
        app.handle_paste(path.to_str().unwrap());
        app.workflow_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        wait(&mut app);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), text);
        assert!(app.workflow.as_ref().unwrap().message.contains("Saved to"));
        app.workflow_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        wait(&mut app);
        assert!(
            app.workflow
                .as_ref()
                .unwrap()
                .message
                .contains("never overwritten")
        );
        assert_eq!(app.workflow.as_ref().unwrap().output, text);
        assert!(app.error.is_none());
    }

    #[test]
    fn worker_captures_exact_stdout_and_reports_process_errors_without_a_shell() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let executable = dir.path().join("fake-scat");
        std::fs::write(
            &executable,
            "#!/bin/sh\nprintf '%s\\n' \"$@\"\nprintf 'warning' >&2\n",
        )
        .unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let result = run(
            &executable,
            Path::new("/db"),
            Path::new("/emb"),
            "diff --old '/old;literal' --new '/new file'",
            None,
            Some(Path::new("/config")),
            None,
        )
        .unwrap();
        assert_eq!(
            result.output.unwrap(),
            "--config\n/config\n--db\n/db\n--embeddings\n/emb\n--no-color\ndiff\n--old\n/old;literal\n--new\n/new file\n"
        );
        assert_eq!(result.message, "warning");
        std::fs::write(
            &executable,
            "#!/bin/sh\necho 'sampled content unavailable' >&2\nexit 1\n",
        )
        .unwrap();
        let error = run(
            &executable,
            Path::new("/db"),
            Path::new("/emb"),
            "diff /a",
            None,
            None,
            None,
        )
        .err()
        .unwrap();
        assert!(error.to_string().contains("sampled content unavailable"));
    }

    fn thumb_cells(terminal: &ratatui::Terminal<ratatui::backend::TestBackend>) -> usize {
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .filter(|cell| cell.symbol() == "█")
            .count()
    }

    #[test]
    fn output_and_help_scroll_with_scrollbars_and_clamp() {
        use ratatui::{Terminal, backend::TestBackend};
        let db = super::super::make_test_db();
        let mut app = super::super::tests::make_app(db.path());
        app.workflow_sources = Some((db.path().to_owned(), PathBuf::from("/missing")));
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        app.handle_key(key(KeyCode::F(11))).unwrap();
        let d = app.workflow.as_mut().unwrap();
        d.output = (0..100).map(|i| format!("line {i}\n")).collect();
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
        terminal
            .draw(|f| draw(f, app.workflow.as_ref().unwrap()))
            .unwrap();
        assert!(thumb_cells(&terminal) > 0, "output needs a scrollbar");

        for _ in 0..30 {
            app.workflow_key(key(KeyCode::PageDown));
        }
        assert_eq!(app.workflow.as_ref().unwrap().scroll, 99);

        app.workflow_key(key(KeyCode::F(1)));
        terminal
            .draw(|f| draw(f, app.workflow.as_ref().unwrap()))
            .unwrap();
        assert!(thumb_cells(&terminal) > 0, "help page needs a scrollbar");
        let before = format!("{:?}", terminal.backend().buffer());
        app.workflow_key(key(KeyCode::PageDown));
        terminal
            .draw(|f| draw(f, app.workflow.as_ref().unwrap()))
            .unwrap();
        assert_ne!(before, format!("{:?}", terminal.backend().buffer()));
    }
}
