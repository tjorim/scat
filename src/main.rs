use std::path::{Path, PathBuf};
use std::process;

use anyhow::{Context, Result};
use clap::Parser;
use scat_core::core::resolve::PathResolver;
use scat_core::core::search::open_search_api;
use scat_core::core::vc::load_vc_config;
use tracing::{error, warn};

mod cli;
mod commands;
mod output;
mod runtime;
mod tui;

use crate::cli::{CatalogCommands, Cli, Commands};
use crate::commands::{
    SearchOpts, cmd_audit, cmd_deps, cmd_diff, cmd_index, cmd_info, cmd_script_diff_catalog,
    cmd_script_diff_explicit, cmd_search, cmd_show, cmd_stats, cmd_status, cmd_symlinks,
};
use crate::runtime::{cmd_vc, init_tracing};

// ---------------------------------------------------------------------------
// Entry point
// ---------------------------------------------------------------------------

fn main() {
    let cli = Cli::parse();
    init_tracing(cli.verbose);

    if let Err(e) = run(cli) {
        error!(error = ?e, "command failed");
        process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    let no_color_env = std::env::var_os("NO_COLOR");
    let no_color = cli::resolve_no_color(cli.no_color, no_color_env.as_deref());

    // Load config early; provides db_path fallback and index settings.
    let scat_config =
        load_vc_config(cli.config.as_deref()).with_context(|| "Failed to load config")?;

    // --old/--new diff needs no catalog at all.
    if let Commands::Diff {
        path: None,
        against: _,
        old: Some(ref old_path),
        new: Some(ref new_path),
        json,
    } = cli.command
    {
        return cmd_script_diff_explicit(old_path, new_path, json);
    }

    // vc pass-through needs no catalog either.
    if let Commands::Vc { ref args, json } = cli.command {
        return cmd_vc(args, json, scat_config.vc_executable);
    }

    // Shell completions need no catalog.
    if let Commands::Completions { shell } = cli.command {
        use clap::CommandFactory;
        let mut command = Cli::command();
        clap_complete::generate(shell, &mut command, "scat", &mut std::io::stdout());
        return Ok(());
    }

    // Resolve database path: CLI flag / env var takes precedence over config file.
    let db_path_buf: PathBuf;
    let no_db_hint =
        "No database specified. Use --db <path>, set SCAT_DB, or add db_path to the config file.";
    let db_path: &Path = if let Some(p) = cli.db.as_deref() {
        p
    } else if let Some(ref p) = scat_config.db_path {
        db_path_buf = p.clone();
        &db_path_buf
    } else {
        anyhow::bail!(no_db_hint);
    };

    match cli.command {
        Commands::Tui { mapping } => {
            let resolver = match mapping.as_deref() {
                Some(path) => PathResolver::from_file(path)
                    .with_context(|| format!("Failed to load mapping file: {}", path.display()))?,
                None => PathResolver::new(),
            };
            return tui::run(db_path, resolver);
        }
        Commands::Catalog { command } => {
            return cmd_catalog(command, db_path, no_color, scat_config);
        }
        _ => {}
    }

    let api = open_search_api(db_path)
        .with_context(|| format!("Failed to open database: {}", db_path.display()))?;

    match cli.command {
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
        } => {
            // Resolve @bookmark alias when text starts with '@'.
            let text = text.map(|t| {
                if let Some(alias) = t.strip_prefix('@') {
                    if let Some(expanded) = scat_config.bookmarks.get(alias) {
                        expanded.clone()
                    } else {
                        warn!(alias = %alias, "bookmark not found in config; using alias as query");
                        alias.to_string()
                    }
                } else {
                    t
                }
            });
            cmd_search(
                &api,
                SearchOpts {
                    text,
                    regex,
                    function,
                    lang,
                    owner,
                    tag,
                    limit,
                    fields: &fields,
                    output,
                    no_color,
                },
            )
        }
        Commands::Show {
            path,
            fields,
            output,
            functions,
        } => cmd_show(
            &api,
            &path,
            &fields,
            output,
            scat_config.configured(),
            functions,
        ),
        Commands::Status { path, all, output } => cmd_status(&api, path, all, output, no_color),
        Commands::Deps {
            path,
            tree,
            depth,
            output,
        } => cmd_deps(
            &api,
            &path,
            tree,
            depth.map(|d| usize::try_from(d).unwrap_or(usize::MAX)),
            output,
            no_color,
        ),
        Commands::Symlinks { path, output } => cmd_symlinks(&api, &path, output, no_color),
        Commands::Diff {
            path: Some(logical_path),
            against,
            old: _,
            new: _,
            json,
        } => cmd_script_diff_catalog(&api, &logical_path, against.as_deref(), json),
        Commands::Vc { .. }
        | Commands::Tui { .. }
        | Commands::Catalog { .. }
        | Commands::Completions { .. }
        | Commands::Diff { .. } => unreachable!(),
    }
}

fn cmd_catalog(
    command: CatalogCommands,
    db_path: &std::path::Path,
    no_color: bool,
    config: scat_core::core::vc::VcConfig,
) -> Result<()> {
    if let CatalogCommands::Build {
        ref scan_root,
        ref logical_prefix,
        head_lines,
        ref ignore_file,
        keep_copies,
        dry_run,
        quiet,
        no_resume,
        force,
        no_incremental,
        json,
    } = command
    {
        return cmd_index(
            scan_root,
            db_path,
            logical_prefix,
            head_lines,
            ignore_file,
            keep_copies,
            dry_run,
            config,
            json,
            quiet,
            no_resume,
            force,
            no_incremental,
        );
    }

    if let CatalogCommands::Diff {
        against,
        old,
        new,
        json,
    } = command
    {
        return cmd_diff(db_path, against, old, new, json);
    }

    let api = open_search_api(db_path)
        .with_context(|| format!("Failed to open database: {}", db_path.display()))?;

    match command {
        CatalogCommands::Stats { json } => cmd_stats(&api, json, no_color, config.configured()),
        CatalogCommands::Info { json } => cmd_info(&api, json),
        CatalogCommands::Audit {
            checks,
            strict,
            stale_days,
            json,
        } => cmd_audit(&api, &checks, strict, stale_days, json),
        CatalogCommands::Build { .. } | CatalogCommands::Diff { .. } => unreachable!(),
    }
}
