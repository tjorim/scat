//! `tui.glyphs` / `tui.colors` in the config file: below flags and
//! environment, above detection.
use std::{fs, path::Path, process::Command};

fn terminal_info(config: &Path, args: &[&str], env: &[(&str, &str)]) -> (bool, String, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_scat"));
    cmd.env_remove("SCAT_DB")
        .env_remove("SCAT_TUI_GLYPHS")
        .env_remove("SCAT_TUI_COLORS")
        .env_remove("NO_COLOR")
        .env("TERM", "xterm")
        .env("LC_ALL", "en_US.UTF-8")
        .arg("--config")
        .arg(config)
        .args(["tui", "--terminal-info"])
        .args(args);
    for (key, value) in env {
        cmd.env(key, value);
    }
    let output = cmd.output().unwrap();
    (
        output.status.success(),
        String::from_utf8(output.stdout).unwrap(),
        String::from_utf8(output.stderr).unwrap(),
    )
}

fn config(dir: &Path, body: &str) -> std::path::PathBuf {
    let path = dir.join("scat.yaml");
    fs::write(&path, body).unwrap();
    path
}

#[test]
fn config_sets_modes_and_flags_and_env_override_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = config(dir.path(), "tui:\n  glyphs: ascii\n  colors: reduced\n");

    let (ok, out, _) = terminal_info(&path, &[], &[]);
    assert!(ok);
    assert!(out.contains("glyphs=Ascii; colors=Reduced"), "{out}");

    let (_, out, _) = terminal_info(&path, &[], &[("SCAT_TUI_GLYPHS", "unicode")]);
    assert!(out.contains("glyphs=Unicode; colors=Reduced"), "{out}");

    let (_, out, _) = terminal_info(&path, &["--glyphs", "unicode", "--colors", "full"], &[]);
    assert!(out.contains("glyphs=Unicode; colors=Full"), "{out}");
}

#[test]
fn no_color_still_wins_over_configured_palette() {
    let dir = tempfile::tempdir().unwrap();
    let path = config(dir.path(), "tui:\n  colors: full\n");
    let (_, out, _) = terminal_info(&path, &[], &[("NO_COLOR", "1")]);
    assert!(out.contains("colors=Mono"), "{out}");
}

#[test]
fn unknown_configured_mode_is_reported_not_ignored() {
    let dir = tempfile::tempdir().unwrap();
    let path = config(dir.path(), "tui:\n  glyphs: fancy\n");
    let (ok, _, err) = terminal_info(&path, &[], &[]);
    assert!(!ok);
    assert!(err.contains("tui.glyphs"), "{err}");
}
