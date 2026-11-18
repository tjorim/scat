use std::path::{Path, PathBuf};
use std::process;

use anyhow::{Context, Result};
use clap::Parser;
use scat_core::core::cache::{CacheOptions, catalog_read_path};
use scat_core::core::config::{Config, load_config};
use scat_core::core::resolve::PathResolver;
use scat_core::core::search::SearchApi;
use scat_core::core::search::open_search_api;
use tracing::{debug, warn};

#[macro_use]
mod output;
mod cli;
mod commands;
mod doctor;
mod runtime;
mod subprocess;
mod tui;

use crate::cli::{CatalogCommands, Cli, Commands};
use crate::commands::{
    AuditOpts, IndexOpts, ScriptDiffOpts, SearchOpts, cmd_audit, cmd_deps, cmd_diff, cmd_index,
    cmd_info, cmd_script_diff_catalog, cmd_script_diff_explicit, cmd_search, cmd_show, cmd_similar,
    cmd_stats, cmd_status, cmd_symlinks,
};
use crate::runtime::{cmd_vc, init_tracing};

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() {
    restore_default_sigpipe();
    let cli = Cli::parse();
    init_tracing(cli.verbose);

    if let Err(e) = run(cli) {
        // Plain stderr, not the logger: `RUST_LOG=off` (or any filter above
        // `error`) must not turn a failure into a silent exit 1. The full
        // chain is on one line; `-v` adds the debug representation.
        eprintln!("error: {e:#}");
        debug!(error = ?e, "command failed");
        process::exit(1);
    }
}

/// Rust ignores `SIGPIPE`, so a closed pipe (`scat … | head`, `| less` then
/// `q`) turns into an `EPIPE` error from `println!`, which panics and, with
/// `panic = "abort"` in release builds, ends in `SIGABRT`. Restoring the
/// default disposition makes scat end quietly like any other Unix filter.
/// This also stops the ignore setting leaking into `vc` and other children.
fn restore_default_sigpipe() {
    // SAFETY: called first thing in `main`, before any thread is spawned;
    // `signal` with `SIG_DFL` has no preconditions.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

/// The configuration plus the locations derived from it and the command line:
/// everything a command that touches the catalog needs before it can run.
struct Session {
    config: Config,
    /// The *published* catalog. `catalog build` writes it, and `diff` addresses
    /// the rotated `.1`/`.2` copies sitting next to it; both need the real path
    /// rather than a host-local copy of the live file.
    db_path: PathBuf,
    /// The embeddings sidecar: CLI/env, then config, then the conventional
    /// location next to the catalog. It is read through the same host-local
    /// cache as the catalog (it is published to the same kind of shared drive,
    /// by a separate `scat-embed` run rather than by the nightly indexer).
    embeddings_path: PathBuf,
    cache: CacheOptions,
}

impl Session {
    /// Load the config file, then resolve the paths.
    fn load(cli: &Cli) -> Result<Self> {
        let config = load_config(cli.config.as_deref()).with_context(|| "Failed to load config")?;
        Self::with_config(cli, config)
    }

    fn with_config(cli: &Cli, config: Config) -> Result<Self> {
        // CLI flag / env var takes precedence over the config file.
        let db_path = cli.db.clone().or_else(|| config.catalog.db_path.clone()).ok_or_else(|| {
            anyhow::anyhow!(
                "No database specified. Use --db <path>, set SCAT_DB, or add db_path to the config file."
            )
        })?;
        let embeddings_path = cli
            .embeddings
            .clone()
            .or_else(|| config.catalog.embeddings_path.clone())
            .unwrap_or_else(|| scat_core::core::embeddings::sidecar_path(&db_path));
        // Every read-only consumer goes through the host-local cache: the
        // published catalog lives on a network share, and reading it in place
        // puts SQLite's locking on NFS for the whole life of the process. The
        // cache degrades to `db_path` itself whenever it can't be used, so the
        // read path is always valid to open (see `core::cache`).
        let no_cache_env = std::env::var_os("SCAT_NO_CACHE");
        let cache = CacheOptions {
            enabled: !cli::resolve_no_cache(cli.no_cache, no_cache_env.as_deref()),
            root: cli
                .cache_dir
                .clone()
                .or_else(|| config.catalog.cache_dir.clone()),
        };
        Ok(Self {
            config,
            db_path,
            embeddings_path,
            cache,
        })
    }

    /// Where the catalog is read from (the cache copy when one is in use).
    fn catalog_read_path(&self) -> PathBuf {
        catalog_read_path(&self.db_path, &self.cache)
    }

    fn embeddings_read_path(&self) -> PathBuf {
        catalog_read_path(&self.embeddings_path, &self.cache)
    }

    /// Open the catalog read-only for a query command.
    fn open_api(&self) -> Result<SearchApi> {
        open_search_api(&self.catalog_read_path())
            .with_context(|| format!("Failed to open database: {}", self.db_path.display()))
    }
}

/// Dispatch. One exhaustive `match` states what each command needs — nothing,
/// the config, or an opened catalog (via [`Session`]) — so adding a subcommand
/// is a compile error here until it is handled, never a runtime `unreachable!`.
fn run(cli: Cli) -> Result<()> {
    let no_color_env = std::env::var_os("NO_COLOR");
    let no_color = cli::resolve_no_color(cli.no_color, no_color_env.as_deref());

    match &cli.command {
        // ---- Needs nothing: not even a readable config, so a stale
        // `SCAT_CONFIG` cannot break them.
        Commands::FileCheck { path } => {
            println!("{}", path.is_file());
            Ok(())
        }
        Commands::Completions { shell } => {
            use clap::CommandFactory;
            let mut command = Cli::command();
            clap_complete::generate(*shell, &mut command, "scat", &mut std::io::stdout());
            Ok(())
        }
        Commands::Man { dir } => runtime::cmd_man(dir.as_deref()),
        Commands::Doctor {
            json,
            path,
            mapping,
            rpm,
            probe,
        } => doctor::run(
            &cli,
            *json,
            path.as_deref(),
            mapping.as_deref(),
            *rpm,
            probe.as_deref(),
            no_color,
        ),
        Commands::Diff {
            path: None,
            old: Some(old_path),
            new: Some(new_path),
            json,
            ..
        } => cmd_script_diff_explicit(old_path, new_path, *json),

        // ---- Needs the config only.
        Commands::Vc { args, json } => {
            let config =
                load_config(cli.config.as_deref()).with_context(|| "Failed to load config")?;
            cmd_vc(args, *json, config.vc.vc_executable)
        }
        Commands::Tui {
            glyphs,
            colors,
            terminal_info,
            mapping,
        } => run_tui(
            &cli,
            *glyphs,
            *colors,
            *terminal_info,
            mapping.as_deref(),
            no_color,
        ),

        // ---- Needs the catalog.
        Commands::Catalog { command } => run_catalog(&cli, command, no_color),
        Commands::Search {
            text,
            lang,
            owner,
            tag,
            limit,
            fields,
            output,
            regex,
            function,
            declares,
            role,
        } => {
            let session = Session::load(&cli)?;
            let api = session.open_api()?;
            // Resolve @bookmark alias when text starts with '@'.
            let text = text.as_ref().map(|t| {
                if let Some(alias) = t.strip_prefix('@') {
                    if let Some(expanded) = session.config.bookmarks.get(alias) {
                        expanded.clone()
                    } else {
                        warn!(alias = %alias, "bookmark not found in config; using alias as query");
                        alias.to_string()
                    }
                } else {
                    t.clone()
                }
            });
            cmd_search(
                &api,
                SearchOpts {
                    text,
                    regex: regex.clone(),
                    function: function.clone(),
                    declares: declares.clone(),
                    role: role.clone(),
                    lang: lang.clone(),
                    owner: owner.clone(),
                    tag: tag.clone(),
                    limit: *limit,
                    fields,
                    output: *output,
                    no_color,
                },
            )
        }
        Commands::Show {
            path,
            fields,
            output,
            functions,
        } => {
            let session = Session::load(&cli)?;
            let api = session.open_api()?;
            cmd_show(
                &api,
                path,
                fields,
                *output,
                session.config.vc_configured(),
                *functions,
            )
        }
        Commands::Status { path, all, output } => {
            let api = Session::load(&cli)?.open_api()?;
            cmd_status(&api, path.clone(), *all, *output, no_color)
        }
        Commands::Deps {
            path,
            tree,
            depth,
            output,
        } => {
            let api = Session::load(&cli)?.open_api()?;
            cmd_deps(
                &api,
                path,
                *tree,
                depth.map(|d| usize::try_from(d).unwrap_or(usize::MAX)),
                *output,
                no_color,
            )
        }
        Commands::Symlinks { path, output } => {
            let api = Session::load(&cli)?.open_api()?;
            cmd_symlinks(&api, path, *output, no_color)
        }
        Commands::Similar {
            path,
            limit,
            output,
        } => {
            let session = Session::load(&cli)?;
            let api = session.open_api()?;
            cmd_similar(&api, &session.embeddings_read_path(), path, *limit, *output)
        }
        Commands::Diff {
            path: Some(logical_path),
            against,
            revision_type,
            revision_user,
            revision_timestamp,
            json,
            ..
        } => {
            let api = Session::load(&cli)?.open_api()?;
            cmd_script_diff_catalog(
                &api,
                logical_path,
                ScriptDiffOpts {
                    against: against.as_deref(),
                    revision_type: revision_type.map(cli::RevisionTypeArg::as_str),
                    revision_user: revision_user.as_deref(),
                    revision_timestamp: revision_timestamp.as_deref(),
                    json: *json,
                },
            )
        }
        // clap requires either a path or both --old and --new, so this is only
        // reachable through a definition change; report it rather than panic.
        Commands::Diff { .. } => {
            anyhow::bail!("diff needs a catalog path, or both --old and --new")
        }
    }
}

/// `scat tui`: resolve the terminal modes, then the catalog, then run.
fn run_tui(
    cli: &Cli,
    glyphs: Option<tui::capabilities::GlyphMode>,
    colors: Option<tui::capabilities::ColorMode>,
    terminal_info: bool,
    mapping: Option<&Path>,
    no_color: bool,
) -> Result<()> {
    // Flags and environment (resolved by clap) override the config file, which
    // overrides detection. A broken config must not block the diagnostic
    // report, so it only warns there.
    let config = match load_config(cli.config.as_deref()) {
        Ok(config) => config,
        Err(err) if terminal_info => {
            eprintln!("warning: ignoring unreadable config: {err:#}");
            Config::default()
        }
        Err(err) => return Err(err).with_context(|| "Failed to load config"),
    };
    // Diagnose terminal suitability before catalog access or fullscreen setup.
    let terminal = tui::capabilities::Capabilities::detect(
        tui::capabilities::resolve_mode(glyphs, config.tui.glyphs.as_deref(), "glyphs")?,
        tui::capabilities::resolve_mode(colors, config.tui.colors.as_deref(), "colors")?,
        no_color,
    );
    if terminal_info {
        println!("{}", terminal.report());
        return Ok(());
    }
    terminal.validate()?;

    let session = Session::with_config(cli, config)?;
    let resolver = match mapping {
        Some(path) => PathResolver::from_file(path)
            .with_context(|| format!("Failed to load mapping file: {}", path.display()))?,
        None => PathResolver::new(),
    };
    let catalog_read = session.catalog_read_path();
    let embeddings_read = session.embeddings_read_path();
    tui::run(
        tui::CatalogFiles {
            read: &catalog_read,
            published: &session.db_path,
        },
        tui::CatalogFiles {
            read: &embeddings_read,
            published: &session.embeddings_path,
        },
        cli.config.as_deref(),
        resolver,
        tui::RunOptions {
            doctor_flags: {
                let mut flags = Vec::new();
                if !session.cache.enabled {
                    flags.push("--no-cache".into());
                }
                if let Some(root) = &session.cache.root {
                    flags.extend(["--cache-dir".into(), root.to_string_lossy().into_owned()]);
                }
                flags
            },
            bookmarks: session.config.bookmarks,
            terminal_modes: terminal,
        },
    )
}

/// `scat catalog …`.
fn run_catalog(cli: &Cli, command: &CatalogCommands, no_color: bool) -> Result<()> {
    let session = Session::load(cli)?;
    match command {
        CatalogCommands::Build {
            scan_root,
            head_lines,
            ignore_file,
            keep_copies,
            dry_run,
            quiet,
            no_resume,
            force,
            no_incremental,
            threads,
            json,
        } => cmd_index(
            IndexOpts {
                scan_roots: scan_root,
                head_lines: *head_lines,
                ignore_files: ignore_file,
                keep_copies: *keep_copies,
                dry_run: *dry_run,
                json: *json,
                quiet: *quiet,
                no_resume: *no_resume,
                force: *force,
                no_incremental: *no_incremental,
                threads: *threads,
            },
            &session.db_path,
            &session.config,
        ),
        CatalogCommands::Diff {
            against,
            old,
            new,
            json,
        } => cmd_diff(
            &session.db_path,
            against.clone(),
            old.clone(),
            new.clone(),
            *json,
        ),
        CatalogCommands::Info { json } => {
            // Reports an unreadable (older/newer) catalog instead of failing to open it.
            cmd_info(&session.db_path, &session.catalog_read_path(), *json)
                .with_context(|| format!("Failed to read catalog: {}", session.db_path.display()))
        }
        CatalogCommands::Stats { json } => {
            let api = session.open_api()?;
            cmd_stats(&api, *json, no_color, session.config.vc_configured())
        }
        CatalogCommands::Audit {
            checks,
            strict,
            stale_days,
            near_duplicate_threshold,
            outlier_threshold,
            json,
        } => {
            let api = session.open_api()?;
            cmd_audit(
                &api,
                &session.embeddings_read_path(),
                AuditOpts {
                    checks,
                    strict: *strict,
                    stale_days: *stale_days,
                    near_duplicate_threshold: *near_duplicate_threshold,
                    outlier_threshold: *outlier_threshold,
                    json: *json,
                },
            )
        }
    }
}
