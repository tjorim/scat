use std::env;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CatalogView {
    pub(super) logical_path: String,
    pub(super) content: String,
    /// The `scripts.language` column, e.g. `"python"` or `"shell"`.
    pub(super) language: String,
}

/// What the external read-only viewer should open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ViewTarget {
    /// Indexed catalog content from `scripts.content`, written to a temporary
    /// file so it matches exactly what the TUI/search are showing.
    Catalog(CatalogView),
    /// The live filesystem source file, resolved through the path mapping.
    LiveSource {
        logical_path: String,
        native_path: PathBuf,
        /// The `scripts.language` column, e.g. `"python"` or `"shell"`.
        language: String,
    },
}

/// Open `target` read-only in an external viewer/editor.
///
/// For catalog content this first materializes a temporary file (keeping the
/// script's filename/extension); the temp directory is held alive for the
/// duration of the viewer process. For live source it opens the resolved
/// filesystem path directly.
pub(super) fn open_target(target: &ViewTarget) -> Result<()> {
    match target {
        ViewTarget::Catalog(view) => {
            let language = view.language.clone();
            let (_dir, path) = write_catalog_view_file(view)?;
            open_readonly(&path, &language)
        }
        ViewTarget::LiveSource {
            native_path,
            language,
            ..
        } => open_readonly(native_path, language),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ViewerCommand {
    program: String,
    args: Vec<String>,
    fallback: bool,
}

pub(super) fn write_catalog_view_file(
    target: &CatalogView,
) -> Result<(tempfile::TempDir, PathBuf)> {
    let dir = tempfile::Builder::new()
        .prefix("scat-view-")
        .tempdir()
        .context("failed to create temporary viewer directory")?;
    let path = dir.path().join(safe_view_filename(&target.logical_path));
    fs::write(&path, target.content.as_bytes())
        .with_context(|| format!("failed to write temporary viewer file {}", path.display()))?;
    Ok((dir, path))
}

pub(super) fn open_readonly(path: &Path, language: &str) -> Result<()> {
    let commands = viewer_commands()?;
    let mut last_not_found = None;
    for command in commands {
        match run_viewer_command(&command, path, language) {
            Ok(()) => return Ok(()),
            Err(err) if command.fallback && err.kind() == std::io::ErrorKind::NotFound => {
                last_not_found = Some((command.program, err));
            }
            Err(err) => return Err(err).context("viewer command failed"),
        }
    }

    if let Some((program, err)) = last_not_found {
        Err(err).with_context(|| format!("viewer command '{program}' was not found"))
    } else {
        bail!("no viewer command configured")
    }
}

fn viewer_commands() -> Result<Vec<ViewerCommand>> {
    for key in ["SCAT_EDITOR", "VISUAL", "EDITOR"] {
        if let Ok(value) = env::var(key)
            && !value.trim().is_empty()
        {
            return Ok(vec![
                parse_viewer_command(&value, false)
                    .with_context(|| format!("failed to parse ${key}"))?,
            ]);
        }
    }

    Ok(default_viewer_commands())
}

fn default_viewer_commands() -> Vec<ViewerCommand> {
    vec![
        ViewerCommand {
            program: "view".to_string(),
            args: Vec::new(),
            fallback: true,
        },
        ViewerCommand {
            program: "vim".to_string(),
            args: vec!["-R".to_string()],
            fallback: true,
        },
        ViewerCommand {
            program: "vi".to_string(),
            args: vec!["-R".to_string()],
            fallback: true,
        },
        ViewerCommand {
            program: "less".to_string(),
            args: Vec::new(),
            fallback: true,
        },
    ]
}

fn parse_viewer_command(value: &str, fallback: bool) -> Result<ViewerCommand> {
    let parts: Vec<String> = shell_words::split(value).map_err(|err| anyhow!(err))?;
    let Some((program, args)) = parts.split_first() else {
        bail!("viewer command is empty");
    };
    Ok(ViewerCommand {
        program: program.clone(),
        args: args.to_vec(),
        fallback,
    })
}

fn run_viewer_command(command: &ViewerCommand, path: &Path, language: &str) -> std::io::Result<()> {
    let mut process = Command::new(&command.program);
    let args = args_with_readonly(command, path, language);
    let status = process.args(args).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "viewer command '{}' exited with {status}",
            command.program
        )))
    }
}

fn args_with_readonly(command: &ViewerCommand, path: &Path, language: &str) -> Vec<OsString> {
    let mut args: Vec<OsString> = command.args.iter().map(OsString::from).collect();
    if is_vim_like(&command.program) {
        if !has_vim_readonly_arg(&command.args) {
            args.push(OsString::from("-R"));
        }
        if let Some(filetype) = vim_filetype(language) {
            args.push(OsString::from("-c"));
            args.push(OsString::from(format!("set filetype={filetype}")));
        }
    }
    args.push(path.as_os_str().to_os_string());
    args
}

fn is_vim_like(program: &str) -> bool {
    let name = Path::new(program)
        .file_stem()
        .and_then(OsStr::to_str)
        .unwrap_or(program)
        .to_ascii_lowercase();
    matches!(name.as_str(), "vi" | "vim" | "nvim")
}

fn has_vim_readonly_arg(args: &[String]) -> bool {
    args.iter()
        .any(|arg| matches!(arg.as_str(), "-R" | "-M" | "-m" | "-Z" | "-y"))
}

/// Map a `scripts.language` value to the vim filetype that turns on its
/// syntax highlighting.
///
/// Vim normally detects filetype from the file's extension (or, failing
/// that, its shebang line) — but many of the shell tools this catalog
/// indexes have no extension at all, and vim's shebang-based fallback
/// (`scripts.vim`) only runs when `:filetype on`/`:syntax on` are active,
/// which isn't guaranteed for a bare `vi -R`. Setting it explicitly makes
/// highlighting depend on scat's own language detection instead of vim's.
fn vim_filetype(language: &str) -> Option<&'static str> {
    match language {
        "python" => Some("python"),
        "shell" => Some("sh"),
        "yaml" => Some("yaml"),
        "json" => Some("json"),
        _ => None,
    }
}

/// Derive a temp-file name for a logical path, preserving the script's own
/// filename (and so its extension, for the viewer's syntax highlighting).
///
/// Only `/` and control characters are rewritten: those are the only
/// characters that can't appear in a Linux filename, and rewriting them is
/// what keeps the name from escaping the temp directory. Characters that are
/// merely illegal on Windows (`:*?"<>|`) are left alone — a script really
/// named `report:daily.py` opens under its own name.
fn safe_view_filename(logical_path: &str) -> String {
    let raw_name = logical_path
        .rsplit('/')
        .find(|part| !part.is_empty())
        .unwrap_or("script");
    let mut name = raw_name
        .chars()
        .map(|ch| {
            if ch.is_control() || ch == '/' {
                '_'
            } else {
                ch
            }
        })
        .collect::<String>();
    if name.is_empty() || name == "." || name == ".." {
        name = "script".to_string();
    }
    name
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::{
        CatalogView, ViewerCommand, args_with_readonly, parse_viewer_command, safe_view_filename,
        vim_filetype, write_catalog_view_file,
    };

    #[test]
    fn parse_viewer_command_handles_quoted_args() {
        let command = parse_viewer_command("vim -R '+set number'", false).unwrap();
        assert_eq!(command.program, "vim");
        assert_eq!(command.args, vec!["-R", "+set number"]);
    }

    #[test]
    fn args_with_readonly_adds_flag_for_vim_like_editor() {
        let command = ViewerCommand {
            program: "vim".to_string(),
            args: Vec::new(),
            fallback: false,
        };

        let args = args_with_readonly(&command, std::path::Path::new("foo.py"), "");

        assert_eq!(args, vec![OsString::from("-R"), OsString::from("foo.py")]);
    }

    #[test]
    fn args_with_readonly_does_not_duplicate_existing_flag() {
        let command = ViewerCommand {
            program: "nvim".to_string(),
            args: vec!["-R".to_string()],
            fallback: false,
        };

        let args = args_with_readonly(&command, std::path::Path::new("foo.py"), "");

        assert_eq!(args, vec![OsString::from("-R"), OsString::from("foo.py")]);
    }

    #[test]
    fn args_with_readonly_sets_filetype_for_known_language() {
        let command = ViewerCommand {
            program: "vim".to_string(),
            args: Vec::new(),
            fallback: false,
        };

        // Extensionless shell tools (indexed via shebang sniffing) are
        // exactly the case vim's own detection can miss.
        let args = args_with_readonly(&command, std::path::Path::new("prepare_release"), "shell");

        assert_eq!(
            args,
            vec![
                OsString::from("-R"),
                OsString::from("-c"),
                OsString::from("set filetype=sh"),
                OsString::from("prepare_release"),
            ]
        );
    }

    #[test]
    fn args_with_readonly_skips_filetype_for_unknown_language() {
        let command = ViewerCommand {
            program: "vim".to_string(),
            args: Vec::new(),
            fallback: false,
        };

        let args = args_with_readonly(&command, std::path::Path::new("data.bin"), "unknown");

        assert_eq!(args, vec![OsString::from("-R"), OsString::from("data.bin")]);
    }

    #[test]
    fn args_with_readonly_ignores_language_for_non_vim_editor() {
        let command = ViewerCommand {
            program: "less".to_string(),
            args: Vec::new(),
            fallback: true,
        };

        let args = args_with_readonly(&command, std::path::Path::new("prepare_release"), "shell");

        assert_eq!(args, vec![OsString::from("prepare_release")]);
    }

    #[test]
    fn vim_filetype_maps_known_languages() {
        assert_eq!(vim_filetype("python"), Some("python"));
        assert_eq!(vim_filetype("shell"), Some("sh"));
        assert_eq!(vim_filetype("yaml"), Some("yaml"));
        assert_eq!(vim_filetype("json"), Some("json"));
        assert_eq!(vim_filetype("csv"), None);
        assert_eq!(vim_filetype("unknown"), None);
        assert_eq!(vim_filetype(""), None);
    }

    #[test]
    fn safe_view_filename_keeps_extension_and_sanitizes_name() {
        // `:` is a perfectly good Linux filename character; keep it rather
        // than mangling the name to satisfy a platform scat no longer targets.
        assert_eq!(
            safe_view_filename("/catalog/scripts/odd:name.py"),
            "odd:name.py"
        );
        // Control characters still go — they are what could make the name
        // unusable, or escape the temp directory.
        assert_eq!(
            safe_view_filename("/catalog/scripts/we\u{7}ird.py"),
            "we_ird.py"
        );
        assert_eq!(safe_view_filename("/catalog/scripts/"), "scripts");
    }

    #[test]
    fn write_catalog_view_file_preserves_filename_and_content() {
        let target = CatalogView {
            logical_path: "/catalog/scripts/foo.py".to_string(),
            content: "print(1)\n".to_string(),
            language: "python".to_string(),
        };

        let (_dir, path) = write_catalog_view_file(&target).unwrap();

        assert_eq!(path.file_name().unwrap(), "foo.py");
        assert_eq!(std::fs::read_to_string(path).unwrap(), "print(1)\n");
    }
}
