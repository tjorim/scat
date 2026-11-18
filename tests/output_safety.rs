//! Text from indexed files must not reach the terminal as control sequences in
//! plain-text output, while JSON and CSV stay byte-exact.
use std::path::{Path, PathBuf};
use std::process::Command;

const ESC_SEQ: &str = "\x1b]0;pwned\x07";

struct Catalog {
    _dir: tempfile::TempDir,
    db: PathBuf,
}

fn scat(db: &Path, args: &[&str]) -> (bool, Vec<u8>) {
    let output = Command::new(env!("CARGO_BIN_EXE_scat"))
        .env_remove("SCAT_CONFIG")
        .env_remove("SCAT_DB")
        .arg("--db")
        .arg(db)
        .arg("--no-cache")
        .args(args)
        .output()
        .unwrap();
    (output.status.success(), output.stdout)
}

fn catalog() -> Catalog {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir(&root).unwrap();
    // Hostile header fields, a hostile file name, and a dependency between them.
    std::fs::write(
        root.join("tool.sh"),
        format!(
            "#!/bin/bash\n# @purpose evil{ESC_SEQ}purpose\n# @owner jdoe\\r\x1b[2Jowner\nsource helper.sh\n"
        ),
    )
    .unwrap();
    std::fs::write(
        root.join("helper.sh"),
        "#!/bin/bash\n# @purpose helps\x1b[31m\n",
    )
    .unwrap();
    std::fs::write(root.join("odd\x1b[1;31mname\nERROR forged.sh"), "echo hi\n").unwrap();
    let db = dir.path().join("cat.sqlite");
    let (ok, _) = scat(
        &db,
        &[
            "catalog",
            "build",
            "--scan-root",
            root.to_str().unwrap(),
            "-q",
        ],
    );
    assert!(ok);
    Catalog { _dir: dir, db }
}

fn assert_no_control_chars(label: &str, bytes: &[u8]) {
    let text = String::from_utf8_lossy(bytes);
    for c in text.chars() {
        assert!(
            !(c.is_control() && c != '\n' && c != '\t'),
            "{label} contains control character {c:?}:\n{text}"
        );
    }
}

#[test]
fn plain_text_output_never_contains_control_characters() {
    let c = catalog();
    let tool = c._dir.path().join("root/tool.sh");
    let tool = tool.to_str().unwrap();
    let odd = "root/odd\x1b[1;31mname\nERROR forged.sh";
    let root = c._dir.path().join("root");
    let odd_path = root.join(odd.trim_start_matches("root/"));
    let odd_path = odd_path.to_str().unwrap();

    let commands: Vec<Vec<&str>> = vec![
        vec!["search"],
        vec!["search", "evil"],
        vec!["search", "--fields", "path,owner,purpose,tags"],
        vec!["show", tool],
        vec!["show", odd_path],
        vec!["deps", tool],
        vec!["deps", tool, "--tree"],
        vec!["status", "--all"],
        vec!["symlinks", tool],
        vec!["catalog", "stats"],
        vec!["catalog", "audit"],
    ];
    for args in commands {
        let (ok, stdout) = scat(&c.db, &args);
        assert!(ok, "{args:?} failed");
        assert_no_control_chars(&args.join(" "), &stdout);
    }

    // The visible placeholder is there instead.
    let (_, shown) = scat(&c.db, &["show", tool]);
    let shown = String::from_utf8(shown).unwrap();
    assert!(shown.contains("␛]0;pwned␇"), "{shown}");
}

#[test]
fn a_hostile_file_name_cannot_forge_an_output_line() {
    let c = catalog();
    let (_, listing) = scat(&c.db, &["search"]);
    let listing = String::from_utf8(listing).unwrap();
    assert!(
        listing
            .lines()
            .all(|line| !line.trim_start().starts_with("ERROR forged")),
        "{listing}"
    );
    assert!(
        listing.contains('␊'),
        "newline shown as a placeholder: {listing}"
    );
}

#[test]
fn json_and_csv_stay_byte_exact() {
    let c = catalog();
    let (ok, json) = scat(&c.db, &["search", "evil", "--output", "json"]);
    assert!(ok);
    let json = String::from_utf8(json).unwrap();
    // JSON escapes it itself and round-trips to the original text.
    assert!(json.contains("\\u001b]0;pwned\\u0007"), "{json}");
    let rows: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert!(rows[0]["purpose"].as_str().unwrap().contains(ESC_SEQ));

    let (ok, csv) = scat(&c.db, &["search", "evil", "--output", "csv"]);
    assert!(ok);
    assert!(
        String::from_utf8_lossy(&csv).contains(ESC_SEQ),
        "CSV is for machines and keeps the bytes"
    );
}
