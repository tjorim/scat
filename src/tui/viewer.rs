use std::env;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CatalogView {
    pub(super) logical_path: String,
    pub(super) content: String,
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

pub(super) fn open_readonly(path: &Path) -> Result<()> {
    let commands = viewer_commands()?;
    let mut last_not_found = None;
    for command in commands {
        match run_viewer_command(&command, path) {
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
    if cfg!(windows) {
        vec![ViewerCommand {
            program: "notepad".to_string(),
            args: Vec::new(),
            fallback: true,
        }]
    } else {
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
}

fn parse_viewer_command(value: &str, fallback: bool) -> Result<ViewerCommand> {
    let parts = shell_words::split(value).map_err(|err| anyhow!(err))?;
    let Some((program, args)) = parts.split_first() else {
        bail!("viewer command is empty");
    };
    Ok(ViewerCommand {
        program: program.clone(),
        args: args.to_vec(),
        fallback,
    })
}

fn run_viewer_command(command: &ViewerCommand, path: &Path) -> std::io::Result<()> {
    let mut process = Command::new(&command.program);
    let args = args_with_readonly(command, path);
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

fn args_with_readonly(command: &ViewerCommand, path: &Path) -> Vec<String> {
    let mut args = command.args.clone();
    if is_vim_like(&command.program) && !has_vim_readonly_arg(&args) {
        args.push("-R".to_string());
    }
    args.push(path.display().to_string());
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

fn safe_view_filename(logical_path: &str) -> String {
    let raw_name = logical_path
        .rsplit(['/', '\\'])
        .find(|part| !part.is_empty())
        .unwrap_or("script");
    let mut name = raw_name
        .chars()
        .map(|ch| {
            if ch.is_control() || matches!(ch, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
            {
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
    use super::{
        CatalogView, ViewerCommand, args_with_readonly, parse_viewer_command, safe_view_filename,
        write_catalog_view_file,
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

        let args = args_with_readonly(&command, std::path::Path::new("foo.py"));

        assert_eq!(args, vec!["-R", "foo.py"]);
    }

    #[test]
    fn args_with_readonly_does_not_duplicate_existing_flag() {
        let command = ViewerCommand {
            program: "nvim".to_string(),
            args: vec!["-R".to_string()],
            fallback: false,
        };

        let args = args_with_readonly(&command, std::path::Path::new("foo.py"));

        assert_eq!(args, vec!["-R", "foo.py"]);
    }

    #[test]
    fn safe_view_filename_keeps_extension_and_sanitizes_name() {
        assert_eq!(
            safe_view_filename("/catalog/scripts/bad:name.py"),
            "bad_name.py"
        );
        assert_eq!(safe_view_filename("/catalog/scripts/"), "scripts");
    }

    #[test]
    fn write_catalog_view_file_preserves_filename_and_content() {
        let target = CatalogView {
            logical_path: "/catalog/scripts/foo.py".to_string(),
            content: "print(1)\n".to_string(),
        };

        let (_dir, path) = write_catalog_view_file(&target).unwrap();

        assert_eq!(path.file_name().unwrap(), "foo.py");
        assert_eq!(std::fs::read_to_string(path).unwrap(), "print(1)\n");
    }
}
