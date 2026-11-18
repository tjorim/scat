//! `scat catalog build` skips the rebuild only when nothing it is built from
//! has changed: not just newer files, but deletions, moved-in files, checkout
//! changes and changed settings too.
use std::path::{Path, PathBuf};
use std::process::Command;

const OLD: u64 = 1_577_836_800; // 2020-01-01

fn pin_old(path: &Path) {
    std::fs::File::open(path)
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(OLD))
        .unwrap();
}

struct Fixture {
    _dir: tempfile::TempDir,
    root: PathBuf,
    db: PathBuf,
}

impl Fixture {
    /// A tree whose files and directories are all far older than any build.
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir_all(root.join("DEVELOP")).unwrap();
        std::fs::write(root.join("a.sh"), "#!/bin/bash\n# @purpose alpha\n").unwrap();
        std::fs::write(root.join("b.sh"), "#!/bin/bash\n# @purpose beta\n").unwrap();
        let fixture = Self {
            db: dir.path().join("cat.sqlite"),
            root,
            _dir: dir,
        };
        fixture.pin_tree();
        fixture
    }

    fn pin_tree(&self) {
        for entry in [
            self.root.join("a.sh"),
            self.root.join("b.sh"),
            self.root.join("DEVELOP"),
            self.root.clone(),
        ] {
            if entry.exists() {
                pin_old(&entry);
            }
        }
    }

    fn scat(&self, args: &[&str]) -> (bool, String) {
        let output = Command::new(env!("CARGO_BIN_EXE_scat"))
            .env_remove("SCAT_CONFIG")
            .env_remove("SCAT_SCAN_ROOTS")
            .env_remove("SCAT_DB")
            .arg("--db")
            .arg(&self.db)
            .arg("--no-cache")
            .args(args)
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&output.stdout).into_owned()
            + &String::from_utf8_lossy(&output.stderr);
        (output.status.success(), text)
    }

    fn build(&self, extra: &[&str]) -> String {
        let root = self.root.to_str().unwrap();
        let mut args = vec!["catalog", "build", "--scan-root", root, "-q"];
        args.extend_from_slice(extra);
        let (ok, text) = self.scat(&args);
        assert!(ok, "build failed: {text}");
        text
    }

    /// Whether a second build is a no-op.
    fn is_up_to_date(&self, extra: &[&str]) -> bool {
        self.build(extra).contains("up to date")
    }

    fn searchable(&self, word: &str) -> bool {
        let (ok, text) = self.scat(&["search", word, "--output", "json"]);
        assert!(ok, "{text}");
        text.trim() != "[]"
    }
}

#[test]
fn unchanged_tree_and_settings_are_skipped() {
    let f = Fixture::new();
    assert!(!f.is_up_to_date(&[]), "the first build builds");
    assert!(f.is_up_to_date(&[]));
    assert!(f.is_up_to_date(&[]));
}

#[test]
fn a_deleted_script_is_not_left_searchable() {
    let f = Fixture::new();
    f.build(&[]);
    assert!(f.searchable("alpha"));
    std::fs::remove_file(f.root.join("a.sh")).unwrap();
    assert!(!f.is_up_to_date(&[]));
    assert!(!f.searchable("alpha"));
    assert!(f.searchable("beta"));
}

#[test]
fn a_file_moved_in_with_an_old_mtime_is_picked_up() {
    let f = Fixture::new();
    f.build(&[]);
    let outside = f.root.parent().unwrap().join("c.sh");
    std::fs::write(&outside, "#!/bin/bash\n# @purpose gamma\n").unwrap();
    pin_old(&outside);
    std::fs::rename(&outside, f.root.join("c.sh")).unwrap();
    assert!(!f.is_up_to_date(&[]));
    assert!(f.searchable("gamma"));
}

#[test]
fn a_new_checkout_is_not_skipped() {
    let f = Fixture::new();
    f.build(&[]);
    std::fs::write(
        f.root.join("DEVELOP").join("a.sh_20261112_101010_abcd"),
        "x",
    )
    .unwrap();
    assert!(!f.is_up_to_date(&[]));
    let (ok, text) = f.scat(&["status", "--all"]);
    assert!(ok && text.contains("abcd"), "{text}");
}

#[test]
fn changed_ignore_rules_and_header_lines_are_not_skipped() {
    let f = Fixture::new();
    f.build(&[]);

    let ignore = f.root.parent().unwrap().join("ignore.txt");
    std::fs::write(&ignore, "b.sh\n").unwrap();
    let ignore_arg = ignore.to_str().unwrap();
    assert!(!f.is_up_to_date(&["--ignore-file", ignore_arg]));
    assert!(
        !f.searchable("beta"),
        "the ignored script leaves the catalog"
    );
    assert!(f.is_up_to_date(&["--ignore-file", ignore_arg]));

    std::fs::write(&ignore, "a.sh\n").unwrap();
    assert!(
        !f.is_up_to_date(&["--ignore-file", ignore_arg]),
        "editing the ignore file changes what is indexed"
    );

    assert!(!f.is_up_to_date(&["--ignore-file", ignore_arg, "--head-lines", "5"]));
}

#[test]
fn a_changed_set_of_scan_roots_is_not_skipped() {
    let f = Fixture::new();
    f.build(&[]);
    let other = f.root.parent().unwrap().join("other");
    std::fs::create_dir(&other).unwrap();
    std::fs::write(other.join("d.sh"), "#!/bin/bash\n# @purpose delta\n").unwrap();
    pin_old(&other.join("d.sh"));
    pin_old(&other);
    let (ok, text) = f.scat(&[
        "catalog",
        "build",
        "--scan-root",
        f.root.to_str().unwrap(),
        "--scan-root",
        other.to_str().unwrap(),
        "-q",
    ]);
    assert!(ok, "{text}");
    assert!(!text.contains("up to date"), "{text}");
    assert!(f.searchable("delta"));
}
