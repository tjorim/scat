/// Integration tests for the vc pipeline: `scan_checkouts` and
/// `infer_warnings`.
///
/// Tests create temporary `scan_root` trees with embedded DEVELOP / ARCHIVE
/// directories, seed them with files whose names match the vc version naming
/// convention (`<script>_<timestamp>[_<user>]`, timestamp at date, minute,
/// or second precision; the user suffix only on DEVELOP checkouts), then
/// assert that `scan_checkouts` discovers them correctly. Warning inference tests seed the
/// DB directly and assert that `infer_warnings` emits the expected warning
/// kinds from indexed revision rows.
use scat_core::core::db::create_db;
use scat_core::core::vc::{
    REVISION_TYPE_ARCHIVE, REVISION_TYPE_DEVELOP, REVISION_TYPE_ROLLBACK, VcConfig, infer_warnings,
    parse_checkout_filename, scan_checkouts,
};
use tempfile::NamedTempFile;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Build a config whose single `scan_root` lives under `<parent>/<os>/<root_name>`,
/// so that `os_flavor` derives to `<os>`.
fn make_config(scan_root: &std::path::Path) -> VcConfig {
    VcConfig {
        scan_roots: vec![scan_root.to_path_buf()],
        ..Default::default()
    }
}

/// Write an empty file whose name follows the checkout convention.
fn touch_checkout(dir: &std::path::Path, name: &str) {
    std::fs::write(dir.join(name), b"").unwrap();
}

fn make_warning_db() -> (rusqlite::Connection, NamedTempFile) {
    let file = NamedTempFile::new().unwrap();
    let conn = create_db(file.path()).unwrap();
    (conn, file)
}

fn insert_script(
    conn: &rusqlite::Connection,
    logical_path: &str,
    mtime: f64,
    symlink_target: Option<&str>,
) {
    conn.execute(
        "INSERT INTO scripts
         (logical_path, language, mtime, symlink_target, vc_warnings)
         VALUES (?1, 'python', ?2, ?3, '[]')",
        rusqlite::params![logical_path, mtime, symlink_target],
    )
    .unwrap();
}

fn insert_revision(
    conn: &rusqlite::Connection,
    logical_path: &str,
    revision_type: &str,
    physical_path: &str,
    timestamp: &str,
) {
    conn.execute(
        "INSERT INTO revisions
         (logical_path, physical_path, revision_type, os_flavor, user, timestamp, age_seconds)
         VALUES (?1, ?2, ?3, 'linux', 'jdoe', ?4, 3600.0)",
        rusqlite::params![logical_path, physical_path, revision_type, timestamp],
    )
    .unwrap();
}

// ---------------------------------------------------------------------------
// parse_checkout_filename – unit-level happy / sad paths
// ---------------------------------------------------------------------------

#[test]
fn parse_valid_checkout_filename() {
    let r = parse_checkout_filename("myscript_20240315_1430_jdoe");
    assert!(r.is_some());
    let (script, ts, user) = r.unwrap();
    assert_eq!(ts, "20240315_1430");
    assert_eq!(user, "jdoe");
    assert!(script.contains("myscript"));
}

#[test]
fn parse_checkout_filename_with_underscores_in_name() {
    let r = parse_checkout_filename("patch_freeze_20240101_0900_alice");
    assert!(r.is_some());
    let (script, ts, user) = r.unwrap();
    assert_eq!(ts, "20240101_0900");
    assert_eq!(user, "alice");
    assert!(script.contains("patch") && script.contains("freeze"));
}

#[test]
fn parse_checkout_filename_no_timestamp_returns_none() {
    assert!(parse_checkout_filename("nodates_here").is_none());
    assert!(parse_checkout_filename("").is_none());
    assert!(parse_checkout_filename("foo_baddate_user").is_none());
}

// ---------------------------------------------------------------------------
// scan_checkouts – basic discovery from scan_root/DEVELOP
// ---------------------------------------------------------------------------

#[test]
fn scan_checkouts_finds_develop_at_scan_root_level() {
    // Structure: <tmp>/linux/scripts/DEVELOP/
    let dir = tempfile::TempDir::new().unwrap();
    let scan_root = dir.path().join("linux").join("scripts");
    let develop = scan_root.join("DEVELOP");
    std::fs::create_dir_all(&develop).unwrap();

    touch_checkout(&develop, "deploy_20240315_1430_jdoe");
    touch_checkout(&develop, "health_20240315_1500_alice");
    std::fs::write(develop.join("README.txt"), b"ignored").unwrap();

    let config = make_config(&scan_root);
    let records = scan_checkouts(&config);

    assert_eq!(
        records.len(),
        2,
        "only checkout-named files should be found"
    );
    let paths: Vec<&str> = records.iter().map(|r| r.logical_path.as_str()).collect();
    assert!(paths.contains(&scan_root.join("deploy").to_string_lossy().as_ref()));
    assert!(paths.contains(&scan_root.join("health").to_string_lossy().as_ref()));
}

#[test]
fn scan_checkouts_skips_hidden_checkout_metadata_file() {
    // vc drops a hidden companion file next to every DEVELOP checkout —
    // `.<checkout-filename>` — recording the stat value and canonical
    // original-version path it was checked out from. It must not surface as
    // a second, bogus checkout of a script named `.deploy`.
    let dir = tempfile::TempDir::new().unwrap();
    let scan_root = dir.path().join("linux").join("scripts");
    let develop = scan_root.join("DEVELOP");
    std::fs::create_dir_all(&develop).unwrap();

    touch_checkout(&develop, "deploy_20240315_1430_jdoe");
    touch_checkout(&develop, ".deploy_20240315_1430_jdoe");

    let config = make_config(&scan_root);
    let records = scan_checkouts(&config);

    assert_eq!(
        records.len(),
        1,
        "the hidden companion file must not be recorded as its own checkout"
    );
    assert_eq!(
        records[0].logical_path,
        scan_root.join("deploy").to_string_lossy()
    );
}

#[test]
fn scan_checkouts_skips_merge_conflict_backup_and_merged_files() {
    // When the live target changed during a checkout, vc runs a merge and
    // leaves a `.org` backup of the pre-merge checkout (and, transiently, a
    // `.merged` file) alongside the real checkout. Neither is a second,
    // independent checkout by a user named "jdoe.org"/"jdoe.merged".
    let dir = tempfile::TempDir::new().unwrap();
    let scan_root = dir.path().join("linux").join("scripts");
    let develop = scan_root.join("DEVELOP");
    std::fs::create_dir_all(&develop).unwrap();

    touch_checkout(&develop, "deploy_20240315_1430_jdoe");
    touch_checkout(&develop, "deploy_20240315_1430_jdoe.org");
    touch_checkout(&develop, "deploy_20240315_1430_jdoe.merged");

    let config = make_config(&scan_root);
    let records = scan_checkouts(&config);

    assert_eq!(
        records.len(),
        1,
        "the .org backup and .merged file must not be recorded as their own checkouts"
    );
    assert_eq!(records[0].user, "jdoe");
}

#[test]
fn scan_checkouts_records_user_timestamp_and_os_flavor() {
    let dir = tempfile::TempDir::new().unwrap();
    let scan_root = dir.path().join("linux").join("scripts");
    let develop = scan_root.join("DEVELOP");
    std::fs::create_dir_all(&develop).unwrap();

    touch_checkout(&develop, "script_20240315_1430_jdoe");

    let config = make_config(&scan_root);
    let records = scan_checkouts(&config);

    assert_eq!(records.len(), 1);
    assert_eq!(records[0].user, "jdoe");
    assert_eq!(records[0].timestamp, "20240315_1430");
    assert_eq!(records[0].revision_type, REVISION_TYPE_DEVELOP);
    // os_flavor is derived from the parent directory of the scan_root
    assert_eq!(records[0].os_flavor, "linux");
}

#[test]
fn scan_checkouts_reclassifies_rollback_displaced_develop_entry() {
    // A rollback moves the version it displaces into DEVELOP as
    // `<script>_<timestamp>_RB_<abbr>` rather than deleting it — that must not
    // be counted as an active in-progress checkout by a user named "RB_abcd".
    let dir = tempfile::TempDir::new().unwrap();
    let scan_root = dir.path().join("linux").join("scripts");
    let develop = scan_root.join("DEVELOP");
    std::fs::create_dir_all(&develop).unwrap();

    touch_checkout(&develop, "deploy_20260910_120000_RB_abcd");

    let config = make_config(&scan_root);
    let records = scan_checkouts(&config);

    assert_eq!(records.len(), 1);
    assert_eq!(records[0].revision_type, REVISION_TYPE_ROLLBACK);
    assert_eq!(
        records[0].user, "abcd",
        "the RB_ marker must be stripped, leaving the real abbreviation"
    );
    assert_eq!(records[0].timestamp, "20260910_120000");
}

#[test]
fn scan_checkouts_does_not_reclassify_a_genuine_user_named_rb() {
    // A real checkout whose abbreviation happens to start with "rb" (not the
    // uppercase "RB_" marker) must still be treated as an ordinary checkout.
    let dir = tempfile::TempDir::new().unwrap();
    let scan_root = dir.path().join("linux").join("scripts");
    let develop = scan_root.join("DEVELOP");
    std::fs::create_dir_all(&develop).unwrap();

    touch_checkout(&develop, "deploy_20260910_120000_rballoy");

    let config = make_config(&scan_root);
    let records = scan_checkouts(&config);

    assert_eq!(records.len(), 1);
    assert_eq!(records[0].revision_type, REVISION_TYPE_DEVELOP);
    assert_eq!(records[0].user, "rballoy");
}

#[test]
fn scan_checkouts_does_not_reclassify_bare_rb_marker_with_no_abbreviation() {
    // "RB_" with nothing after it isn't a real rollback marker (there's no
    // abbreviation left once stripped), so it's left as an ordinary checkout.
    let dir = tempfile::TempDir::new().unwrap();
    let scan_root = dir.path().join("linux").join("scripts");
    let develop = scan_root.join("DEVELOP");
    std::fs::create_dir_all(&develop).unwrap();

    touch_checkout(&develop, "deploy_20260910_120000_RB_");

    let config = make_config(&scan_root);
    let records = scan_checkouts(&config);

    assert_eq!(records.len(), 1);
    assert_eq!(records[0].revision_type, REVISION_TYPE_DEVELOP);
    assert_eq!(records[0].user, "RB_");
}

#[test]
fn scan_checkouts_handles_seconds_and_userless_archive_names() {
    // Real-world layout: DEVELOP checkouts carry seconds-precision timestamps
    // and a user; ARCHIVE entries have a timestamp only (sometimes date-only).
    let dir = tempfile::TempDir::new().unwrap();
    let scan_root = dir.path().join("linux").join("scripts");
    let develop = scan_root.join("DEVELOP");
    let archive = scan_root.join("ARCHIVE");
    std::fs::create_dir_all(&develop).unwrap();
    std::fs::create_dir_all(&archive).unwrap();

    touch_checkout(&develop, "update_board_firmware.sh_20260720_103044_titd");
    touch_checkout(&archive, "update_board_firmware.sh_20240921_135312");
    touch_checkout(&archive, "update_board_firmware.sh_20240610");

    let config = make_config(&scan_root);
    let mut records = scan_checkouts(&config);
    records.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));

    assert_eq!(records.len(), 3);
    let expected = scan_root
        .join("update_board_firmware.sh")
        .to_string_lossy()
        .into_owned();
    for record in &records {
        assert_eq!(record.logical_path, expected);
    }

    assert_eq!(records[0].timestamp, "20240610");
    assert_eq!(records[0].revision_type, REVISION_TYPE_ARCHIVE);
    assert_eq!(records[0].user, "");

    assert_eq!(records[1].timestamp, "20240921_135312");
    assert_eq!(records[1].revision_type, REVISION_TYPE_ARCHIVE);
    assert_eq!(records[1].user, "");

    assert_eq!(records[2].timestamp, "20260720_103044");
    assert_eq!(records[2].revision_type, REVISION_TYPE_DEVELOP);
    assert_eq!(records[2].user, "titd");
}

#[test]
fn scan_checkouts_returns_empty_when_no_scan_roots() {
    let config = VcConfig::default();
    let records = scan_checkouts(&config);
    assert!(records.is_empty());
}

#[test]
fn scan_checkouts_returns_empty_when_no_develop_or_archive() {
    let dir = tempfile::TempDir::new().unwrap();
    let scan_root = dir.path().join("linux").join("scripts");
    std::fs::create_dir_all(&scan_root).unwrap();
    // No DEVELOP or ARCHIVE dirs

    let config = make_config(&scan_root);
    let records = scan_checkouts(&config);
    assert!(records.is_empty());
}

#[test]
fn scan_checkouts_anchors_logical_path_at_scan_root() {
    // Every host that runs the indexer scans the same scan_root, so a
    // DEVELOP/ARCHIVE revision's logical_path is anchored at scan_root's own
    // absolute path — matching the active script's logical_path (built the
    // same way by the main scanner), so the two join in the catalog instead
    // of the revision silently disappearing from `show`/`search`/`status`.
    let dir = tempfile::TempDir::new().unwrap();
    let scan_root = dir.path().join("linux").join("scripts");
    let develop = scan_root.join("DEVELOP");
    let archive = scan_root.join("ARCHIVE");
    std::fs::create_dir_all(&develop).unwrap();
    std::fs::create_dir_all(&archive).unwrap();

    touch_checkout(&develop, "source_scan.sh_20260729_150613_userA");
    touch_checkout(&archive, "source_scan.sh_20220207_150200");

    let config = make_config(&scan_root);
    let records = scan_checkouts(&config);

    assert_eq!(records.len(), 2);
    let expected = scan_root
        .join("source_scan.sh")
        .to_string_lossy()
        .into_owned();
    for record in &records {
        assert_eq!(record.logical_path, expected);
    }
}

// ---------------------------------------------------------------------------
// scan_checkouts – ARCHIVE tree
// ---------------------------------------------------------------------------

#[test]
fn scan_checkouts_includes_archive_records() {
    let dir = tempfile::TempDir::new().unwrap();
    let scan_root = dir.path().join("linux").join("scripts");
    let archive = scan_root.join("ARCHIVE");
    std::fs::create_dir_all(scan_root.join("DEVELOP")).unwrap();
    std::fs::create_dir_all(&archive).unwrap();

    touch_checkout(&archive, "tool.py_20240101_0900_bob");

    let config = make_config(&scan_root);
    let records = scan_checkouts(&config);

    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0].logical_path,
        scan_root.join("tool.py").to_string_lossy()
    );
    assert_eq!(records[0].revision_type, REVISION_TYPE_ARCHIVE);
    assert_eq!(records[0].user, "bob");
}

// ---------------------------------------------------------------------------
// scan_checkouts – one-level-deep subdirectory DEVELOP/ARCHIVE
// ---------------------------------------------------------------------------

#[test]
fn scan_checkouts_finds_develop_in_subfolder() {
    // Structure: <tmp>/linux/scripts/group1/DEVELOP/
    let dir = tempfile::TempDir::new().unwrap();
    let scan_root = dir.path().join("linux").join("scripts");
    let subdir_develop = scan_root.join("group1").join("DEVELOP");
    std::fs::create_dir_all(&subdir_develop).unwrap();

    touch_checkout(&subdir_develop, "report_20240315_1430_jdoe");

    let config = make_config(&scan_root);
    let records = scan_checkouts(&config);

    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0].logical_path,
        scan_root.join("group1/report").to_string_lossy()
    );
    assert_eq!(records[0].revision_type, REVISION_TYPE_DEVELOP);
}

#[test]
fn scan_checkouts_finds_archive_in_subfolder() {
    let dir = tempfile::TempDir::new().unwrap();
    let scan_root = dir.path().join("linux").join("scripts");
    let subdir_archive = scan_root.join("group1").join("ARCHIVE");
    std::fs::create_dir_all(&subdir_archive).unwrap();

    touch_checkout(&subdir_archive, "report_20240101_1000_alice");

    let config = make_config(&scan_root);
    let records = scan_checkouts(&config);

    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0].logical_path,
        scan_root.join("group1/report").to_string_lossy()
    );
    assert_eq!(records[0].revision_type, REVISION_TYPE_ARCHIVE);
}

#[test]
fn scan_checkouts_mixed_root_and_subfolder() {
    let dir = tempfile::TempDir::new().unwrap();
    let scan_root = dir.path().join("linux").join("scripts");
    let root_develop = scan_root.join("DEVELOP");
    let sub_develop = scan_root.join("utils").join("DEVELOP");
    std::fs::create_dir_all(&root_develop).unwrap();
    std::fs::create_dir_all(&sub_develop).unwrap();

    touch_checkout(&root_develop, "deploy_20240315_1430_jdoe");
    touch_checkout(&sub_develop, "helper_20240315_1430_alice");

    let config = make_config(&scan_root);
    let records = scan_checkouts(&config);

    assert_eq!(records.len(), 2);
    let paths: Vec<&str> = records.iter().map(|r| r.logical_path.as_str()).collect();
    assert!(paths.contains(&scan_root.join("deploy").to_string_lossy().as_ref()));
    assert!(paths.contains(&scan_root.join("utils/helper").to_string_lossy().as_ref()));
}

// ---------------------------------------------------------------------------
// scan_checkouts – symlink deduplication
// ---------------------------------------------------------------------------

#[test]
fn scan_checkouts_deduplicates_symlinked_scan_roots() {
    // alt/scripts is a symlink to linux/scripts — the DEVELOP dir should be scanned once.
    let dir = tempfile::TempDir::new().unwrap();
    let linux_scripts = dir.path().join("linux").join("scripts");
    let develop = linux_scripts.join("DEVELOP");
    std::fs::create_dir_all(&develop).unwrap();
    touch_checkout(&develop, "deploy_20240315_1430_jdoe");

    let alt = dir.path().join("alt");
    std::fs::create_dir_all(&alt).unwrap();
    std::os::unix::fs::symlink(&linux_scripts, alt.join("scripts")).unwrap();
    let alt_scripts = alt.join("scripts");

    let config = VcConfig {
        scan_roots: vec![linux_scripts, alt_scripts],
        ..Default::default()
    };
    let records = scan_checkouts(&config);

    assert_eq!(
        records.len(),
        1,
        "symlinked DEVELOP dir must not be scanned twice"
    );
    assert_eq!(
        records[0].os_flavor, "linux",
        "os_flavor must be derived from the first scan_root's parent, not the symlink's"
    );
}

#[test]
fn scan_checkouts_finds_deeply_nested_develop_and_archive() {
    // Script folders can be many subfolders deep, each with its own
    // DEVELOP/ARCHIVE containers; all of them must be discovered.
    let dir = tempfile::TempDir::new().unwrap();
    let scan_root = dir.path().join("linux").join("scripts");
    let deep_develop = scan_root.join("level1").join("level2").join("DEVELOP");
    let deep_archive = scan_root
        .join("level1")
        .join("level2")
        .join("level3")
        .join("ARCHIVE");
    std::fs::create_dir_all(&deep_develop).unwrap();
    std::fs::create_dir_all(&deep_archive).unwrap();
    touch_checkout(&deep_develop, "tool.py_20240315_143044_jdoe");
    touch_checkout(&deep_archive, "old.py_20240101_1000");

    let config = make_config(&scan_root);
    let mut records = scan_checkouts(&config);
    records.sort_by(|a, b| a.logical_path.cmp(&b.logical_path));

    assert_eq!(records.len(), 2);
    assert_eq!(
        records[0].logical_path,
        scan_root
            .join("level1/level2/level3/old.py")
            .to_string_lossy()
    );
    assert_eq!(records[0].revision_type, REVISION_TYPE_ARCHIVE);
    assert_eq!(records[0].user, "");
    assert_eq!(
        records[1].logical_path,
        scan_root.join("level1/level2/tool.py").to_string_lossy()
    );
    assert_eq!(records[1].revision_type, REVISION_TYPE_DEVELOP);
    assert_eq!(records[1].user, "jdoe");
}

// ---------------------------------------------------------------------------
// scan_checkouts – configurable develop_dirs / archive_dirs
// ---------------------------------------------------------------------------

#[test]
fn scan_checkouts_uses_configured_develop_dir_names() {
    let dir = tempfile::TempDir::new().unwrap();
    let scan_root = dir.path().join("linux").join("scripts");
    // Custom develop dir name instead of the default "DEVELOP"
    let working = scan_root.join("WORKING");
    std::fs::create_dir_all(&working).unwrap();
    touch_checkout(&working, "deploy_20240315_1430_jdoe");

    let config = VcConfig {
        scan_roots: vec![scan_root.clone()],
        develop_dirs: vec!["WORKING".to_string()],
        archive_dirs: vec![],
        ..Default::default()
    };
    let records = scan_checkouts(&config);

    assert_eq!(records.len(), 1);
    assert_eq!(records[0].revision_type, REVISION_TYPE_DEVELOP);
    assert_eq!(
        records[0].logical_path,
        scan_root.join("deploy").to_string_lossy()
    );
}

#[test]
fn scan_checkouts_uses_configured_archive_dir_names() {
    let dir = tempfile::TempDir::new().unwrap();
    let scan_root = dir.path().join("linux").join("scripts");
    // Custom archive dir name instead of the default "ARCHIVE"
    let history = scan_root.join("HISTORY");
    std::fs::create_dir_all(&history).unwrap();
    touch_checkout(&history, "tool_20240101_0900_bob");

    let config = VcConfig {
        scan_roots: vec![scan_root.clone()],
        develop_dirs: vec![],
        archive_dirs: vec!["HISTORY".to_string()],
        ..Default::default()
    };
    let records = scan_checkouts(&config);

    assert_eq!(records.len(), 1);
    assert_eq!(records[0].revision_type, REVISION_TYPE_ARCHIVE);
    assert_eq!(
        records[0].logical_path,
        scan_root.join("tool").to_string_lossy()
    );
}

#[test]
fn scan_checkouts_default_dirs_not_discovered_with_custom_config() {
    let dir = tempfile::TempDir::new().unwrap();
    let scan_root = dir.path().join("linux").join("scripts");
    // Standard DEVELOP dir is present but config does not list it
    let develop = scan_root.join("DEVELOP");
    std::fs::create_dir_all(&develop).unwrap();
    touch_checkout(&develop, "deploy_20240315_1430_jdoe");

    let config = VcConfig {
        scan_roots: vec![scan_root],
        develop_dirs: vec!["WORKING".to_string()],
        archive_dirs: vec![],
        ..Default::default()
    };
    let records = scan_checkouts(&config);

    assert!(
        records.is_empty(),
        "DEVELOP dir should not be scanned when not in develop_dirs"
    );
}

// ---------------------------------------------------------------------------
// infer_warnings – checkout_without_catalog_entry
// ---------------------------------------------------------------------------

#[test]
fn infer_warns_checkout_without_catalog_entry() {
    let (conn, _db) = make_warning_db();
    insert_revision(
        &conn,
        "/catalog/scripts/ghost.py",
        REVISION_TYPE_DEVELOP,
        "/catalog/linux/scripts/DEVELOP/ghost.py_20240315_1430_jdoe",
        "20240315_1430",
    );

    let warnings = infer_warnings(&conn).unwrap();
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].kind, "checkout_without_catalog_entry");
    assert_eq!(warnings[0].logical_path, "/catalog/scripts/ghost.py");
}

#[test]
fn infer_warns_archive_revision_without_catalog_entry() {
    let (conn, _db) = make_warning_db();
    insert_revision(
        &conn,
        "/catalog/scripts/archived_ghost.py",
        REVISION_TYPE_ARCHIVE,
        "/catalog/linux/scripts/ARCHIVE/archived_ghost.py_20240315_1430_jdoe",
        "20240315_1430",
    );

    let warnings = infer_warnings(&conn).unwrap();
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].kind, "checkout_without_catalog_entry");
    assert_eq!(
        warnings[0].logical_path,
        "/catalog/scripts/archived_ghost.py"
    );
}

// ---------------------------------------------------------------------------
// infer_warnings – timestamp_drift
// ---------------------------------------------------------------------------

#[test]
fn infer_warns_timestamp_drift_when_script_newer_than_checkout() {
    let (conn, _db) = make_warning_db();
    let future_mtime = 1_735_689_600_f64; // 2025-01-01T00:00:00Z
    insert_script(&conn, "/catalog/scripts/tool.py", future_mtime, None);
    insert_revision(
        &conn,
        "/catalog/scripts/tool.py",
        REVISION_TYPE_DEVELOP,
        "/catalog/linux/scripts/DEVELOP/tool.py_20240315_1430_jdoe",
        "20240315_1430",
    );

    let warnings = infer_warnings(&conn).unwrap();
    assert!(
        warnings.iter().any(|w| w.kind == "timestamp_drift"),
        "expected a timestamp_drift warning, got: {warnings:?}"
    );
}

#[test]
fn infer_no_timestamp_drift_when_checkout_newer_than_script() {
    let (conn, _db) = make_warning_db();
    let old_mtime = 1_704_067_200_f64; // 2024-01-01T00:00:00Z
    insert_script(&conn, "/catalog/scripts/tool.py", old_mtime, None);
    insert_revision(
        &conn,
        "/catalog/scripts/tool.py",
        REVISION_TYPE_DEVELOP,
        "/catalog/linux/scripts/DEVELOP/tool.py_20240315_1430_jdoe",
        "20240315_1430",
    );

    let warnings = infer_warnings(&conn).unwrap();
    assert!(
        !warnings.iter().any(|w| w.kind == "timestamp_drift"),
        "should NOT produce timestamp_drift when checkout is newer"
    );
}

// ---------------------------------------------------------------------------
// infer_warnings – scripttype_language_mismatch
// ---------------------------------------------------------------------------

#[test]
fn infer_warns_scripttype_language_mismatch() {
    let (conn, _db) = make_warning_db();
    conn.execute(
        "INSERT INTO scripts (logical_path, language, metadata_json)
         VALUES ('/catalog/scripts/tool.py', 'python', '{\"scripttype\":\"shell\"}')",
        [],
    )
    .unwrap();

    let warnings = infer_warnings(&conn).unwrap();
    let warning = warnings
        .iter()
        .find(|w| w.kind == "scripttype_language_mismatch")
        .expect("expected a scripttype_language_mismatch warning");
    assert_eq!(warning.details.get("declared_scripttype").unwrap(), "shell");
    assert_eq!(warning.details.get("detected_language").unwrap(), "python");
}

#[test]
fn infer_no_scripttype_warning_when_declared_matches_detected() {
    let (conn, _db) = make_warning_db();
    conn.execute(
        "INSERT INTO scripts (logical_path, language, metadata_json)
         VALUES ('/catalog/scripts/tool.py', 'python', '{\"scripttype\":\"python\"}')",
        [],
    )
    .unwrap();

    let warnings = infer_warnings(&conn).unwrap();
    assert!(
        !warnings
            .iter()
            .any(|w| w.kind == "scripttype_language_mismatch"),
        "should NOT warn when @scripttype agrees with the detected language"
    );
}

#[test]
fn infer_no_scripttype_warning_for_unrecognized_taxonomy() {
    // "utility" isn't a language token scat recognizes, so it must not be
    // guessed at — no comparison, no false positive.
    let (conn, _db) = make_warning_db();
    conn.execute(
        "INSERT INTO scripts (logical_path, language, metadata_json)
         VALUES ('/catalog/scripts/tool.py', 'python', '{\"scripttype\":\"utility\"}')",
        [],
    )
    .unwrap();

    let warnings = infer_warnings(&conn).unwrap();
    assert!(
        !warnings
            .iter()
            .any(|w| w.kind == "scripttype_language_mismatch"),
        "should NOT warn on an unrecognized @scripttype taxonomy"
    );
}

// ---------------------------------------------------------------------------
// infer_warnings – self_referential_symlink
// ---------------------------------------------------------------------------

#[test]
fn infer_warns_self_referential_symlink() {
    let (conn, _db) = make_warning_db();
    insert_script(
        &conn,
        "/catalog/scripts/loop.py",
        0.0,
        Some("/catalog/scripts/loop.py"),
    );

    let warnings = infer_warnings(&conn).unwrap();
    assert!(
        warnings
            .iter()
            .any(|w| w.kind == "self_referential_symlink"),
        "expected self_referential_symlink warning"
    );
}

// ---------------------------------------------------------------------------
// infer_warnings – symlink_name_mismatch
// ---------------------------------------------------------------------------

#[test]
fn infer_warns_symlink_name_mismatch() {
    let (conn, _db) = make_warning_db();
    insert_script(
        &conn,
        "/catalog/scripts/foo.py",
        0.0,
        Some("/catalog/scripts/bar.py"),
    );

    let warnings = infer_warnings(&conn).unwrap();
    assert!(
        warnings.iter().any(|w| w.kind == "symlink_name_mismatch"),
        "expected symlink_name_mismatch warning"
    );
}

#[test]
fn infer_no_symlink_mismatch_when_stems_match() {
    let (conn, _db) = make_warning_db();
    insert_script(
        &conn,
        "/catalog/scripts/foo.py",
        0.0,
        Some("/catalog/linux/scripts/ARCHIVE/foo_20240101_1200.py"),
    );

    let warnings = infer_warnings(&conn).unwrap();
    assert!(
        !warnings.iter().any(|w| w.kind == "symlink_name_mismatch"),
        "should NOT warn when symlink target stem matches"
    );
}

#[test]
fn infer_no_symlink_mismatch_when_logical_stem_extends_target_stem() {
    let (conn, _db) = make_warning_db();
    insert_script(
        &conn,
        "/catalog/scripts/prepare_thing.py",
        0.0,
        Some("/catalog/scripts/prepare.py"),
    );

    let warnings = infer_warnings(&conn).unwrap();
    assert!(
        !warnings.iter().any(|w| w.kind == "symlink_name_mismatch"),
        "should NOT warn when target stem is a prefix of logical stem"
    );
}

#[test]
fn infer_warns_symlink_mismatch_for_partial_prefix_only() {
    let (conn, _db) = make_warning_db();
    insert_script(
        &conn,
        "/catalog/scripts/prepare_thing.py",
        0.0,
        Some("/catalog/scripts/pre.py"),
    );

    let warnings = infer_warnings(&conn).unwrap();
    assert!(
        warnings.iter().any(|w| w.kind == "symlink_name_mismatch"),
        "should warn when only a short partial prefix matches"
    );
}

// ---------------------------------------------------------------------------
// infer_warnings – missing_archive_entries
// ---------------------------------------------------------------------------

#[test]
fn infer_warns_missing_archive_entries_when_archive_revision_missing() {
    let (conn, _db) = make_warning_db();
    insert_script(&conn, "/catalog/scripts/tool.py", 0.0, None);
    insert_revision(
        &conn,
        "/catalog/scripts/tool.py",
        REVISION_TYPE_DEVELOP,
        "/catalog/linux/scripts/DEVELOP/tool.py_20240315_1430_jdoe",
        "20240315_1430",
    );

    let warnings = infer_warnings(&conn).unwrap();
    assert!(
        warnings.iter().any(|w| w.kind == "missing_archive_entries"),
        "expected missing_archive_entries warning, got: {warnings:?}"
    );
}

#[test]
fn infer_no_missing_archive_warning_when_archive_revision_exists() {
    let (conn, _db) = make_warning_db();
    insert_script(&conn, "/catalog/scripts/tool.py", 0.0, None);
    insert_revision(
        &conn,
        "/catalog/scripts/tool.py",
        REVISION_TYPE_DEVELOP,
        "/catalog/linux/scripts/DEVELOP/tool.py_20240315_1430_jdoe",
        "20240315_1430",
    );
    insert_revision(
        &conn,
        "/catalog/scripts/tool.py",
        REVISION_TYPE_ARCHIVE,
        "/catalog/linux/scripts/ARCHIVE/tool.py_20240101_1200_jdoe",
        "20240101_1200",
    );

    let warnings = infer_warnings(&conn).unwrap();
    assert!(
        !warnings.iter().any(|w| w.kind == "missing_archive_entries"),
        "should NOT warn when archive entry exists"
    );
}

// ---------------------------------------------------------------------------
// infer_manifest_warnings – managed-file manifest cross-reference
// ---------------------------------------------------------------------------

#[test]
fn infer_manifest_warnings_flags_entry_registered_but_not_indexed() {
    let (conn, _db) = make_warning_db();
    insert_script(&conn, "/catalog/scripts/tool.py", 0.0, None);

    let manifest: std::collections::HashSet<String> = [
        "/catalog/scripts/tool.py".to_string(),
        "/catalog/scripts/missing.sh".to_string(),
    ]
    .into_iter()
    .collect();

    let warnings = scat_core::core::vc::infer_manifest_warnings(&conn, &manifest).unwrap();

    assert_eq!(
        warnings
            .iter()
            .filter(|w| w.kind == "registered_with_vc_but_not_indexed")
            .count(),
        1
    );
    let missing = warnings
        .iter()
        .find(|w| w.kind == "registered_with_vc_but_not_indexed")
        .unwrap();
    assert_eq!(missing.logical_path, "/catalog/scripts/missing.sh");

    // tool.py is registered AND indexed — must not appear in either warning kind.
    assert!(
        !warnings
            .iter()
            .any(|w| w.logical_path == "/catalog/scripts/tool.py")
    );
}

#[test]
fn infer_manifest_warnings_flags_indexed_entry_not_registered() {
    let (conn, _db) = make_warning_db();
    insert_script(&conn, "/catalog/scripts/tool.py", 0.0, None);
    insert_script(&conn, "/catalog/scripts/unregistered.sh", 0.0, None);

    let manifest: std::collections::HashSet<String> = ["/catalog/scripts/tool.py".to_string()]
        .into_iter()
        .collect();

    let warnings = scat_core::core::vc::infer_manifest_warnings(&conn, &manifest).unwrap();

    assert_eq!(
        warnings
            .iter()
            .filter(|w| w.kind == "not_registered_with_vc")
            .count(),
        1
    );
    let extra = warnings
        .iter()
        .find(|w| w.kind == "not_registered_with_vc")
        .unwrap();
    assert_eq!(extra.logical_path, "/catalog/scripts/unregistered.sh");
}

#[test]
fn infer_manifest_warnings_empty_manifest_produces_no_warnings() {
    let (conn, _db) = make_warning_db();
    insert_script(&conn, "/catalog/scripts/tool.py", 0.0, None);

    let warnings =
        scat_core::core::vc::infer_manifest_warnings(&conn, &std::collections::HashSet::new())
            .unwrap();
    assert!(
        warnings.is_empty(),
        "an unconfigured/unreadable manifest must produce no warnings, not false positives"
    );
}
