use std::ffi::OsStr;
use std::path::PathBuf;

use clap::{ArgAction, Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "scat",
    about = "Script Catalog — discover, search, and understand scripts.",
    version
)]
pub struct Cli {
    /// Path to the catalog SQLite database.
    #[arg(long, env = "SCAT_DB", global = true)]
    pub(crate) db: Option<PathBuf>,

    /// Path to the scat configuration file.
    #[arg(long, env = "SCAT_CONFIG", global = true)]
    pub(crate) config: Option<PathBuf>,

    /// Read the catalog directly instead of through the host-local cache.
    #[arg(long, env = "SCAT_NO_CACHE", global = true)]
    pub(crate) no_cache: bool,

    /// Directory holding the host-local catalog cache (default: /dev/shm).
    #[arg(long, env = "SCAT_CACHE_DIR", global = true)]
    pub(crate) cache_dir: Option<PathBuf>,

    /// Disable color in output.
    #[arg(long, global = true)]
    pub(crate) no_color: bool,

    /// Increase verbosity (-v debug, -vv trace)
    #[arg(short = 'v', long = "verbose", visible_alias = "debug", action = ArgAction::Count, global = true)]
    pub(crate) verbose: u8,

    #[command(subcommand)]
    pub(crate) command: Commands,
}

pub fn resolve_no_color(flag: bool, no_color_env: Option<&OsStr>) -> bool {
    flag || no_color_env.is_some_and(|value| !value.is_empty())
}

#[derive(Subcommand)]
pub enum Commands {
    /// Full-text search across indexed scripts.
    Search {
        /// Full-text search query (omit to list all). Prefix with `@` to expand a config bookmark.
        text: Option<String>,
        /// Filter by language (e.g. python, shell).
        #[arg(long)]
        lang: Option<String>,
        /// Filter by owner (substring match).
        #[arg(long)]
        owner: Option<String>,
        /// Filter by tag (matches scripts whose tags contain this tag).
        #[arg(long)]
        tag: Option<String>,
        /// Maximum number of results.
        #[arg(long, default_value = "50")]
        limit: usize,
        /// Comma-separated list of fields to display/export.
        /// Defaults to: path, language, owner, purpose.
        /// Available: path, language, owner, purpose, checkout, size, indexed,
        ///            symlink, mtime, tags, entry_points, related
        #[arg(long, value_delimiter = ',')]
        fields: Vec<String>,
        /// Output format for search results.
        #[arg(long, value_enum, default_value_t = SearchOutput::Table)]
        output: SearchOutput,
        /// Regex pattern to match against logical_path and purpose (alternative to full-text search).
        #[arg(long, conflicts_with = "text")]
        regex: Option<String>,
        /// Search by function name (substring): returns scripts that define a matching function.
        #[arg(long, conflicts_with = "text", conflicts_with = "regex")]
        function: Option<String>,
    },

    /// Show details, dep counts, and relationships for a single script.
    Show {
        /// Logical path of the script.
        path: String,
        /// Comma-separated list of fields to display (default: all).
        /// Available: language, owner, purpose, checkout, size, indexed,
        ///            uses, used_by, folder, siblings, symlink, mtime, tags,
        ///            entry_points, related, contributors
        #[arg(long, value_delimiter = ',')]
        fields: Vec<String>,
        /// Output format.
        #[arg(long, value_enum, default_value_t = OutputFormat::Table)]
        output: OutputFormat,
        /// List function definitions (name, kind, line, docstring) instead of metadata.
        #[arg(long)]
        functions: bool,
    },

    /// Show vc checkout state stored in the catalog database.
    Status {
        /// Logical path of the script.
        path: Option<String>,
        /// Show all scripts with checkout or warning state.
        #[arg(long)]
        all: bool,
        /// Output format.
        #[arg(long, value_enum, default_value_t = OutputFormat::Table)]
        output: OutputFormat,
    },

    /// List dependencies and dependents of a script.
    Deps {
        /// Logical path of the script.
        path: String,
        /// Show transitive dependency trees instead of the flat direct lists.
        #[arg(long)]
        tree: bool,
        /// Maximum tree depth (implies --tree; default 5).
        #[arg(long, value_parser = clap::value_parser!(u64).range(1..))]
        depth: Option<u64>,
        /// Output format.
        #[arg(long, value_enum, default_value_t = OutputFormat::Table)]
        output: OutputFormat,
    },

    /// Show symlink relationships for a script (both directions).
    Symlinks {
        /// Logical path of the script.
        path: String,
        /// Output format.
        #[arg(long, value_enum, default_value_t = OutputFormat::Table)]
        output: OutputFormat,
    },

    /// Compare a cataloged script against its vc checkout or an explicit file.
    ///
    /// Three modes:
    ///
    ///   scat diff /catalog/foo.py            — active catalog vs most-recent checkout
    ///
    ///   scat diff /catalog/foo.py --against <file>  — active catalog vs explicit file
    ///
    ///   scat diff --old <file> --new <file>  — two explicit files (no catalog)
    Diff {
        /// Logical catalog path of the script to compare.
        /// Mutually exclusive with --old / --new.
        #[arg(
            conflicts_with_all = ["old", "new"],
            required_unless_present_all = ["old", "new"]
        )]
        path: Option<String>,

        /// Compare the active script against this file (checkout or archive path).
        /// --against points to a **file**, unlike `scat catalog diff --against` which
        /// points to a database.
        #[arg(long, conflicts_with_all = ["old", "new"])]
        against: Option<std::path::PathBuf>,

        /// Old file path for explicit two-file comparison. Must be paired with --new.
        /// Mutually exclusive with positional path.
        #[arg(long, requires = "new", conflicts_with = "path")]
        old: Option<std::path::PathBuf>,

        /// New file path for explicit two-file comparison. Must be paired with --old.
        /// Mutually exclusive with positional path.
        #[arg(long, requires = "old", conflicts_with = "path")]
        new: Option<std::path::PathBuf>,

        /// Emit machine-readable JSON output.
        #[arg(long)]
        json: bool,
    },

    /// Wrap external vc tool (pass-through).
    Vc {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
        /// Emit machine-readable JSON output.
        #[arg(long)]
        json: bool,
    },

    /// Launch the interactive TUI browser.
    Tui {
        #[arg(long, env = "SCAT_MAPPING")]
        mapping: Option<PathBuf>,
    },

    /// Catalog management: build, inspect, and compare the database.
    Catalog {
        #[command(subcommand)]
        command: CatalogCommands,
    },

    /// Generate shell completion script on stdout.
    ///
    /// Example: scat completions bash > /etc/bash_completion.d/scat
    Completions {
        /// Shell to generate completions for.
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
}

#[derive(Subcommand)]
pub enum CatalogCommands {
    /// Build (or rebuild) the catalog database by scanning script directories.
    Build {
        /// Root directories to scan recursively (repeatable).
        /// If omitted, scan_roots from the vc config file are used.
        #[arg(long = "scan-root")]
        scan_root: Vec<PathBuf>,
        /// Logical path prefix applied to all discovered scripts (e.g. /catalog/scripts).
        #[arg(long, default_value = "")]
        logical_prefix: String,
        /// Number of header lines to read per file.
        #[arg(long, default_value = "10")]
        head_lines: usize,
        /// Additional ignore files with gitignore-style patterns.
        #[arg(long = "ignore-file")]
        ignore_file: Vec<PathBuf>,
        /// Number of historical database copies to keep.
        #[arg(long, default_value = "3")]
        keep_copies: usize,
        /// Build the index without replacing the live database.
        #[arg(long)]
        dry_run: bool,
        /// Suppress the progress bar (final summary is still printed).
        #[arg(short = 'q', long)]
        quiet: bool,
        /// Ignore any existing checkpoint and start a fresh run.
        #[arg(long)]
        no_resume: bool,
        /// Skip the up-to-date check and always perform a full rebuild.
        #[arg(long)]
        force: bool,
        /// Always rebuild from scratch instead of seeding from the previous
        /// completed build (which skips re-extracting unchanged scripts).
        #[arg(long)]
        no_incremental: bool,
        /// Worker threads for the parallel scan and extraction phases.
        /// Defaults to the number of logical CPUs.
        #[arg(long)]
        threads: Option<usize>,
        /// Emit machine-readable JSON output.
        #[arg(long)]
        json: bool,
    },

    /// Show catalog statistics.
    Stats {
        /// Emit machine-readable JSON output.
        #[arg(long)]
        json: bool,
    },

    /// Show catalog metadata (build timestamp, schema version).
    Info {
        /// Emit machine-readable JSON output.
        #[arg(long)]
        json: bool,
    },

    /// Run catalog health checks and report findings.
    Audit {
        /// Specific checks to run (repeatable).
        #[arg(long = "check")]
        checks: Vec<String>,
        /// Treat WARN findings as a non-zero exit.
        #[arg(long)]
        strict: bool,
        /// Age threshold in days for stale checkouts.
        #[arg(long, default_value = "90")]
        stale_days: i64,
        /// Emit machine-readable JSON output.
        #[arg(long)]
        json: bool,
    },

    /// Compare whole catalog snapshots.
    Diff {
        /// Previous snapshot path (defaults to <db>.1).
        #[arg(long)]
        against: Option<PathBuf>,
        /// Old database path. Must be provided with --new.
        #[arg(long)]
        old: Option<PathBuf>,
        /// New database path. Must be provided with --old.
        #[arg(long)]
        new: Option<PathBuf>,
        /// Emit machine-readable JSON output.
        #[arg(long)]
        json: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, clap::ValueEnum)]
pub enum SearchOutput {
    Table,
    Csv,
    Json,
}

/// Output format for commands that support table and JSON output (but not CSV).
#[derive(Clone, Copy, Debug, Eq, PartialEq, clap::ValueEnum)]
pub enum OutputFormat {
    Table,
    Json,
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use clap::Parser;

    use super::*;

    #[test]
    fn clap_accepts_global_verbose_before_subcommand() {
        let cli = Cli::try_parse_from([
            "scat",
            "-v",
            "--db",
            "catalog.sqlite",
            "catalog",
            "build",
            "--scan-root",
            ".",
        ])
        .unwrap();

        assert_eq!(cli.verbose, 1);
        assert!(matches!(
            cli.command,
            Commands::Catalog {
                command: CatalogCommands::Build { .. }
            }
        ));
    }

    #[test]
    fn clap_accepts_global_verbose_after_subcommand() {
        let cli =
            Cli::try_parse_from(["scat", "catalog", "build", "--scan-root", ".", "-vv"]).unwrap();

        assert_eq!(cli.verbose, 2);
        assert!(matches!(
            cli.command,
            Commands::Catalog {
                command: CatalogCommands::Build { .. }
            }
        ));
    }

    #[test]
    fn clap_parses_catalog_build_ignore_files() {
        let cli = Cli::try_parse_from([
            "scat",
            "--db",
            "catalog.sqlite",
            "catalog",
            "build",
            "--scan-root",
            ".",
            "--ignore-file",
            ".catignore",
            "--ignore-file",
            "/tmp/global.catignore",
        ])
        .unwrap();

        assert!(matches!(
            cli.command,
            Commands::Catalog {
                command: CatalogCommands::Build { ref ignore_file, .. }
            } if ignore_file == &vec![
                PathBuf::from(".catignore"),
                PathBuf::from("/tmp/global.catignore"),
            ]
        ));
    }

    #[test]
    fn clap_parses_catalog_build_force() {
        let cli = Cli::try_parse_from([
            "scat",
            "--db",
            "catalog.sqlite",
            "catalog",
            "build",
            "--scan-root",
            ".",
            "--force",
        ])
        .unwrap();

        assert!(matches!(
            cli.command,
            Commands::Catalog {
                command: CatalogCommands::Build { force: true, .. }
            }
        ));
    }

    #[test]
    fn clap_catalog_build_force_defaults_to_false() {
        let cli = Cli::try_parse_from([
            "scat",
            "--db",
            "catalog.sqlite",
            "catalog",
            "build",
            "--scan-root",
            ".",
        ])
        .unwrap();

        assert!(matches!(
            cli.command,
            Commands::Catalog {
                command: CatalogCommands::Build { force: false, .. }
            }
        ));
    }

    #[test]
    fn clap_accepts_long_debug_flag() {
        let cli = Cli::try_parse_from([
            "scat",
            "--debug",
            "--db",
            "catalog.sqlite",
            "catalog",
            "stats",
        ])
        .unwrap();

        assert_eq!(cli.verbose, 1);
        assert!(matches!(
            cli.command,
            Commands::Catalog {
                command: CatalogCommands::Stats { .. }
            }
        ));
    }

    #[test]
    fn clap_accepts_global_no_color_flag() {
        let cli = Cli::try_parse_from([
            "scat",
            "--no-color",
            "--db",
            "catalog.sqlite",
            "catalog",
            "stats",
        ])
        .unwrap();

        assert!(cli.no_color);
        assert!(matches!(
            cli.command,
            Commands::Catalog {
                command: CatalogCommands::Stats { .. }
            }
        ));
    }

    #[test]
    fn no_color_resolution_uses_flag_or_non_empty_env() {
        assert!(!resolve_no_color(false, None));
        assert!(resolve_no_color(true, None));
        assert!(resolve_no_color(false, Some(OsStr::new("1"))));
        assert!(resolve_no_color(true, Some(OsStr::new("1"))));
    }

    #[test]
    fn no_color_resolution_ignores_empty_env_without_flag() {
        assert!(!resolve_no_color(false, Some(OsStr::new(""))));
        assert!(resolve_no_color(true, Some(OsStr::new(""))));
    }

    #[test]
    fn clap_parses_audit_flags() {
        let cli = Cli::try_parse_from([
            "scat",
            "--db",
            "catalog.sqlite",
            "catalog",
            "audit",
            "--check",
            "unowned",
            "--check",
            "broken-deps",
            "--strict",
            "--stale-days",
            "30",
        ])
        .unwrap();

        assert!(matches!(
            cli.command,
            Commands::Catalog {
                command: CatalogCommands::Audit {
                    ref checks,
                    strict: true,
                    stale_days: 30,
                    ..
                }
            } if checks == &vec!["unowned".to_string(), "broken-deps".to_string()]
        ));
    }

    #[test]
    fn clap_parses_catalog_diff_flags() {
        let cli = Cli::try_parse_from([
            "scat",
            "--db",
            "catalog.sqlite",
            "catalog",
            "diff",
            "--against",
            "catalog.sqlite.1",
            "--json",
        ])
        .unwrap();

        assert!(matches!(
            cli.command,
            Commands::Catalog {
                command: CatalogCommands::Diff {
                    against: Some(ref against),
                    json: true,
                    ..
                }
            } if against == &PathBuf::from("catalog.sqlite.1")
        ));
    }

    #[test]
    fn clap_parses_search_output_and_fields() {
        let cli = Cli::try_parse_from([
            "scat",
            "--db",
            "catalog.sqlite",
            "search",
            "checkmc",
            "--fields",
            "path,owner",
            "--output",
            "csv",
        ])
        .unwrap();

        assert!(matches!(
            cli.command,
            Commands::Search {
                text: Some(ref text),
                ref fields,
                output: SearchOutput::Csv,
                ..
            } if text == "checkmc" && fields == &vec!["path".to_string(), "owner".to_string()]
        ));
    }

    #[test]
    fn clap_parses_search_owner_and_tag_filters() {
        let cli = Cli::try_parse_from([
            "scat",
            "--db",
            "catalog.sqlite",
            "search",
            "--owner",
            "alice",
            "--tag",
            "deploy",
        ])
        .unwrap();

        assert!(matches!(
            cli.command,
            Commands::Search {
                owner: Some(ref owner),
                tag: Some(ref tag),
                text: None,
                ..
            } if owner == "alice" && tag == "deploy"
        ));
    }

    #[test]
    fn clap_parses_search_function_flag() {
        let cli = Cli::try_parse_from([
            "scat",
            "--db",
            "catalog.sqlite",
            "search",
            "--function",
            "deploy_service",
        ])
        .unwrap();

        assert!(matches!(
            cli.command,
            Commands::Search {
                function: Some(ref name),
                text: None,
                regex: None,
                ..
            } if name == "deploy_service"
        ));
    }

    #[test]
    fn clap_parses_show_functions_flag() {
        let cli = Cli::try_parse_from([
            "scat",
            "--db",
            "catalog.sqlite",
            "show",
            "/catalog/scripts/foo.py",
            "--functions",
        ])
        .unwrap();

        assert!(matches!(
            cli.command,
            Commands::Show {
                ref path,
                functions: true,
                ..
            } if path == "/catalog/scripts/foo.py"
        ));
    }

    #[test]
    fn clap_parses_show_contributors_field() {
        let cli = Cli::try_parse_from([
            "scat",
            "--db",
            "catalog.sqlite",
            "show",
            "/catalog/scripts/foo.py",
            "--fields",
            "contributors",
        ])
        .unwrap();

        assert!(matches!(
            cli.command,
            Commands::Show {
                ref fields,
                ..
            } if fields == &vec!["contributors".to_string()]
        ));
    }

    #[test]
    fn clap_parses_show_output_json() {
        let cli = Cli::try_parse_from([
            "scat",
            "--db",
            "catalog.sqlite",
            "show",
            "/catalog/scripts/foo.py",
            "--output",
            "json",
        ])
        .unwrap();

        assert!(matches!(
            cli.command,
            Commands::Show {
                ref path,
                output: OutputFormat::Json,
                ..
            } if path == "/catalog/scripts/foo.py"
        ));
    }

    #[test]
    fn clap_parses_deps_output_json() {
        let cli = Cli::try_parse_from([
            "scat",
            "--db",
            "catalog.sqlite",
            "deps",
            "/catalog/scripts/foo.py",
            "--output",
            "json",
        ])
        .unwrap();

        assert!(matches!(
            cli.command,
            Commands::Deps {
                ref path,
                output: OutputFormat::Json,
                ..
            } if path == "/catalog/scripts/foo.py"
        ));
    }

    #[test]
    fn clap_parses_search_regex_flag() {
        let cli = Cli::try_parse_from([
            "scat",
            "--db",
            "catalog.sqlite",
            "search",
            "--regex",
            r"check.*\.py",
        ])
        .unwrap();

        assert!(matches!(
            cli.command,
            Commands::Search {
                regex: Some(ref pattern),
                text: None,
                ..
            } if pattern == r"check.*\.py"
        ));
    }

    #[test]
    fn clap_regex_and_text_conflict() {
        let result = Cli::try_parse_from([
            "scat",
            "--db",
            "catalog.sqlite",
            "search",
            "checkmc",
            "--regex",
            r"check.*\.py",
        ]);
        assert!(result.is_err());
    }

    #[test]
    fn clap_parses_deps_tree_and_depth() {
        let cli = Cli::try_parse_from([
            "scat",
            "--db",
            "catalog.sqlite",
            "deps",
            "/catalog/scripts/foo.py",
            "--tree",
            "--depth",
            "3",
        ])
        .unwrap();

        assert!(matches!(
            cli.command,
            Commands::Deps {
                tree: true,
                depth: Some(3),
                ..
            }
        ));
    }

    #[test]
    fn clap_deps_rejects_zero_depth() {
        let result = Cli::try_parse_from([
            "scat",
            "--db",
            "catalog.sqlite",
            "deps",
            "/catalog/scripts/foo.py",
            "--depth",
            "0",
        ]);
        assert!(result.is_err(), "--depth 0 should be rejected");
    }

    #[test]
    fn clap_parses_completions_shell() {
        let cli = Cli::try_parse_from(["scat", "completions", "bash"]).unwrap();
        assert!(matches!(
            cli.command,
            Commands::Completions {
                shell: clap_complete::Shell::Bash
            }
        ));
    }

    #[test]
    fn clap_completions_rejects_unknown_shell() {
        let result = Cli::try_parse_from(["scat", "completions", "tcsh"]);
        assert!(result.is_err());
    }

    // ------------------------------------------------------------------
    // scat diff
    // ------------------------------------------------------------------

    #[test]
    fn clap_parses_diff_path_only() {
        let cli = Cli::try_parse_from([
            "scat",
            "--db",
            "catalog.sqlite",
            "diff",
            "/catalog/scripts/foo.py",
        ])
        .unwrap();

        assert!(matches!(
            cli.command,
            Commands::Diff {
                path: Some(ref p),
                against: None,
                old: None,
                new: None,
                json: false,
            } if p == "/catalog/scripts/foo.py"
        ));
    }

    #[test]
    fn clap_parses_diff_path_with_against() {
        let cli = Cli::try_parse_from([
            "scat",
            "--db",
            "catalog.sqlite",
            "diff",
            "/catalog/scripts/foo.py",
            "--against",
            "/dev/LINUX/foo_20240315_1430_user",
        ])
        .unwrap();

        assert!(matches!(
            cli.command,
            Commands::Diff {
                path: Some(ref p),
                against: Some(ref a),
                ..
            } if p == "/catalog/scripts/foo.py" && a == &PathBuf::from("/dev/LINUX/foo_20240315_1430_user")
        ));
    }

    #[test]
    fn clap_parses_diff_old_new() {
        let cli = Cli::try_parse_from([
            "scat", "diff", "--old", "old.py", "--new", "new.py", "--json",
        ])
        .unwrap();

        assert!(matches!(
            cli.command,
            Commands::Diff {
                path: None,
                old: Some(ref o),
                new: Some(ref n),
                json: true,
                ..
            } if o == &PathBuf::from("old.py") && n == &PathBuf::from("new.py")
        ));
    }

    #[test]
    fn clap_diff_path_and_old_conflict() {
        let result = Cli::try_parse_from([
            "scat",
            "--db",
            "catalog.sqlite",
            "diff",
            "/catalog/scripts/foo.py",
            "--old",
            "old.py",
            "--new",
            "new.py",
        ]);
        assert!(result.is_err(), "path and --old/--new must conflict");
    }

    #[test]
    fn clap_diff_old_without_new_rejected() {
        let result = Cli::try_parse_from(["scat", "diff", "--old", "old.py"]);
        assert!(result.is_err(), "--old without --new should be rejected");
    }

    #[test]
    fn clap_diff_new_without_old_rejected() {
        let result = Cli::try_parse_from(["scat", "diff", "--new", "new.py"]);
        assert!(result.is_err(), "--new without --old should be rejected");
    }

    #[test]
    fn clap_diff_against_and_old_conflict() {
        let result = Cli::try_parse_from([
            "scat",
            "--db",
            "catalog.sqlite",
            "diff",
            "--against",
            "file.py",
            "--old",
            "old.py",
            "--new",
            "new.py",
        ]);
        assert!(result.is_err(), "--against and --old/--new should conflict");
    }
}
