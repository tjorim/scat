/// Integration tests for the full indexer pipeline.
///
/// Each test creates a temporary directory containing mock scripts and/or
/// sidecar `.meta.yml` files, runs `build_index`, then opens the resulting
/// database and asserts that scripts, dependencies, and metadata are stored
/// correctly.
use rusqlite::{Connection, OpenFlags};
use scat_core::core::vc::VcConfig;
use scat_core::indexer::builder::{BuildOptions, build_index};

// ---------------------------------------------------------------------------
// Helper
// ---------------------------------------------------------------------------

fn open_ro(path: &std::path::Path) -> Connection {
    Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
}

fn script_count(conn: &Connection) -> i64 {
    conn.query_row("SELECT COUNT(*) FROM scripts", [], |r| r.get(0))
        .unwrap()
}

// ---------------------------------------------------------------------------
// Basic build
// ---------------------------------------------------------------------------

#[test]
fn build_indexes_py_and_sh_scripts() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().join("scripts");
    std::fs::create_dir(&root).unwrap();

    std::fs::write(root.join("deploy.py"), "# @author alice\nimport os\n").unwrap();
    std::fs::write(root.join("health.sh"), "#!/bin/bash\necho ok\n").unwrap();
    // A .txt file that should be ignored by the scanner
    std::fs::write(root.join("README.txt"), "not a script").unwrap();

    let db_path = dir.path().join("catalog.sqlite");
    let result = build_index(
        std::slice::from_ref(&root),
        &db_path,
        BuildOptions {
            logical_prefix: "/catalog/scripts".into(),
            head_lines: 10,
            keep_copies: 0,
            vc_config: Some(VcConfig::default()),
            ..Default::default()
        },
    )
    .unwrap();

    assert_eq!(
        result.scripts_indexed, 2,
        "exactly 2 scripts should be indexed"
    );
    assert!(db_path.exists(), "catalog database must be created");
    assert!(result.errors.is_empty(), "no errors expected");

    let conn = open_ro(&db_path);
    assert_eq!(script_count(&conn), 2);

    // Verify language detection
    let py_lang: String = conn
        .query_row(
            "SELECT language FROM scripts WHERE logical_path LIKE '%deploy.py'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(py_lang, "python");

    let sh_lang: String = conn
        .query_row(
            "SELECT language FROM scripts WHERE logical_path LIKE '%health.sh'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(sh_lang, "shell");
}

// ---------------------------------------------------------------------------
// Header extraction
// ---------------------------------------------------------------------------

#[test]
fn build_extracts_author_and_brief_from_header() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().join("scripts");
    std::fs::create_dir(&root).unwrap();

    std::fs::write(
        root.join("patch.py"),
        "# @brief Apply patch freeze\n# @author devops\nimport sys\n",
    )
    .unwrap();

    let db_path = dir.path().join("catalog.sqlite");
    build_index(
        std::slice::from_ref(&root),
        &db_path,
        BuildOptions {
            logical_prefix: "/catalog/scripts".into(),
            head_lines: 10,
            keep_copies: 0,
            vc_config: Some(VcConfig::default()),
            ..Default::default()
        },
    )
    .unwrap();

    let conn = open_ro(&db_path);
    let (owner, purpose): (String, String) = conn
        .query_row(
            "SELECT owner, purpose FROM scripts WHERE logical_path LIKE '%patch.py'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();

    assert_eq!(owner, "devops");
    assert_eq!(purpose, "Apply patch freeze");
}

// ---------------------------------------------------------------------------
// @history parsing integration
// ---------------------------------------------------------------------------

#[test]
fn build_indexes_multiple_history_lines_into_metadata_json() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().join("scripts");
    std::fs::create_dir(&root).unwrap();

    std::fs::write(
        root.join("versioned.sh"),
        concat!(
            "#!/bin/bash\n",
            "# @brief Versioned script\n",
            "# @author devops\n",
            "# @history 2024-05-10 alice Fixed timeout handling\n",
            "# @history 2024-04-01 bob Initial implementation\n",
            "echo ok\n",
        ),
    )
    .unwrap();

    let db_path = dir.path().join("catalog.sqlite");
    build_index(
        std::slice::from_ref(&root),
        &db_path,
        BuildOptions {
            logical_prefix: "/catalog/scripts".into(),
            head_lines: 10,
            keep_copies: 0,
            vc_config: Some(VcConfig::default()),
            ..Default::default()
        },
    )
    .unwrap();

    let conn = open_ro(&db_path);
    let metadata_json: String = conn
        .query_row(
            "SELECT metadata_json FROM scripts WHERE logical_path LIKE '%versioned.sh'",
            [],
            |r| r.get(0),
        )
        .unwrap();

    let meta: serde_json::Value = serde_json::from_str(&metadata_json).unwrap();

    // history key must be an array preserving both lines in order.
    let history = meta["history"]
        .as_array()
        .expect("history must be an array");
    assert_eq!(history.len(), 2);
    assert_eq!(
        history[0].as_str().unwrap(),
        "2024-05-10 alice Fixed timeout handling"
    );
    assert_eq!(
        history[1].as_str().unwrap(),
        "2024-04-01 bob Initial implementation"
    );

    // history_entries must expose structured parsed fields.
    let entries = meta["history_entries"]
        .as_array()
        .expect("history_entries must be an array");
    assert_eq!(entries.len(), 2);

    let first = &entries[0];
    assert_eq!(
        first["raw"].as_str().unwrap(),
        "2024-05-10 alice Fixed timeout handling"
    );
    assert_eq!(first["date"].as_str().unwrap(), "2024-05-10");
    assert_eq!(first["author"].as_str().unwrap(), "alice");
    assert!(first["version"].is_null());
    assert_eq!(first["summary"].as_str().unwrap(), "Fixed timeout handling");

    let second = &entries[1];
    assert_eq!(second["date"].as_str().unwrap(), "2024-04-01");
    assert_eq!(second["author"].as_str().unwrap(), "bob");
    assert_eq!(
        second["summary"].as_str().unwrap(),
        "Initial implementation"
    );
}

#[test]
fn build_history_with_version_field() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().join("scripts");
    std::fs::create_dir(&root).unwrap();

    std::fs::write(
        root.join("tool.py"),
        concat!(
            "# @brief Tool with versioned history\n",
            "# @history 1.2.3 2024-05-10 alice Fixed timeout handling\n",
            "pass\n",
        ),
    )
    .unwrap();

    let db_path = dir.path().join("catalog.sqlite");
    build_index(
        std::slice::from_ref(&root),
        &db_path,
        BuildOptions {
            logical_prefix: "/catalog/scripts".into(),
            head_lines: 10,
            keep_copies: 0,
            vc_config: Some(VcConfig::default()),
            ..Default::default()
        },
    )
    .unwrap();

    let conn = open_ro(&db_path);
    let metadata_json: String = conn
        .query_row(
            "SELECT metadata_json FROM scripts WHERE logical_path LIKE '%tool.py'",
            [],
            |r| r.get(0),
        )
        .unwrap();

    let meta: serde_json::Value = serde_json::from_str(&metadata_json).unwrap();
    let entries = meta["history_entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["version"].as_str().unwrap(), "1.2.3");
    assert_eq!(entries[0]["date"].as_str().unwrap(), "2024-05-10");
    assert_eq!(entries[0]["author"].as_str().unwrap(), "alice");
    assert_eq!(
        entries[0]["summary"].as_str().unwrap(),
        "Fixed timeout handling"
    );
}

#[test]
fn build_history_summary_only_line_is_preserved() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().join("scripts");
    std::fs::create_dir(&root).unwrap();

    std::fs::write(
        root.join("simple.sh"),
        "#!/bin/bash\n# @history Fixed timeout handling\necho ok\n",
    )
    .unwrap();

    let db_path = dir.path().join("catalog.sqlite");
    build_index(
        std::slice::from_ref(&root),
        &db_path,
        BuildOptions {
            logical_prefix: "/catalog/scripts".into(),
            head_lines: 10,
            keep_copies: 0,
            vc_config: Some(VcConfig::default()),
            ..Default::default()
        },
    )
    .unwrap();

    let conn = open_ro(&db_path);
    let metadata_json: String = conn
        .query_row(
            "SELECT metadata_json FROM scripts WHERE logical_path LIKE '%simple.sh'",
            [],
            |r| r.get(0),
        )
        .unwrap();

    let meta: serde_json::Value = serde_json::from_str(&metadata_json).unwrap();
    let entries = meta["history_entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0]["raw"].as_str().unwrap(),
        "Fixed timeout handling"
    );
    assert!(entries[0]["date"].is_null());
    assert!(entries[0]["author"].is_null());
    assert!(entries[0]["version"].is_null());
    assert_eq!(
        entries[0]["summary"].as_str().unwrap(),
        "Fixed timeout handling"
    );
}

// ---------------------------------------------------------------------------
// Sidecar .meta.yml
// ---------------------------------------------------------------------------

#[test]
fn build_reads_header_metadata() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().join("scripts");
    std::fs::create_dir(&root).unwrap();

    std::fs::write(
        root.join("runner.py"),
        "# @author header_owner\n# @brief Header purpose\npass\n",
    )
    .unwrap();

    let db_path = dir.path().join("catalog.sqlite");
    build_index(
        std::slice::from_ref(&root),
        &db_path,
        BuildOptions {
            logical_prefix: "/catalog/scripts".into(),
            head_lines: 10,
            keep_copies: 0,
            vc_config: Some(VcConfig::default()),
            ..Default::default()
        },
    )
    .unwrap();

    let conn = open_ro(&db_path);
    let (owner, purpose): (String, String) = conn
        .query_row(
            "SELECT owner, purpose FROM scripts WHERE logical_path LIKE '%runner.py'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();

    assert_eq!(owner, "header_owner");
    assert_eq!(purpose, "Header purpose");
}

// ---------------------------------------------------------------------------
// Dependencies
// ---------------------------------------------------------------------------

#[test]
fn build_records_python_import_dependencies() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().join("scripts");
    std::fs::create_dir(&root).unwrap();

    // A script that imports another (tree-sitter / AST dep extraction)
    // We use a simple "import" comment form recognised by the fallback regex.
    std::fs::write(root.join("main.py"), "import helper\nprint('hello')\n").unwrap();
    std::fs::write(root.join("helper.py"), "def help(): pass\n").unwrap();

    let db_path = dir.path().join("catalog.sqlite");
    let result = build_index(
        std::slice::from_ref(&root),
        &db_path,
        BuildOptions {
            logical_prefix: "/catalog/scripts".into(),
            head_lines: 10,
            keep_copies: 0,
            vc_config: Some(VcConfig::default()),
            ..Default::default()
        },
    )
    .unwrap();

    assert_eq!(result.scripts_indexed, 2);

    let conn = open_ro(&db_path);
    let dep_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM dependencies", [], |r| r.get(0))
        .unwrap();
    assert_eq!(dep_count, 1, "expected exactly one dependency row");
    let dep_path: String = conn
        .query_row(
            "SELECT depends_on_path FROM dependencies LIMIT 1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(dep_path, "helper");
}

#[test]
fn build_records_python_function_graph() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().join("scripts");
    std::fs::create_dir(&root).unwrap();

    std::fs::write(root.join("helper.py"), "def run():\n    return 1\n").unwrap();
    std::fs::write(
        root.join("main.py"),
        "import helper\n\ndef entry():\n    helper.run()\n",
    )
    .unwrap();

    let db_path = dir.path().join("catalog.sqlite");
    build_index(
        std::slice::from_ref(&root),
        &db_path,
        BuildOptions {
            logical_prefix: "/catalog/scripts".into(),
            head_lines: 10,
            keep_copies: 0,
            vc_config: Some(VcConfig::default()),
            ..Default::default()
        },
    )
    .unwrap();

    let conn = open_ro(&db_path);
    let def_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM function_definitions", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(def_count >= 2);

    let calls: Vec<(String, String, Option<String>)> = {
        let mut stmt = conn
            .prepare(
                "SELECT fc.caller, fc.callee, s.logical_path
                 FROM function_calls fc
                 LEFT JOIN scripts s ON s.id = fc.resolved_target_script_id
                 ORDER BY fc.id",
            )
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    };
    assert!(
        calls
            .iter()
            .any(|(caller, callee, target)| caller == "entry"
                && callee == "helper.run"
                && target.as_deref() == Some("/catalog/scripts/helper.py"))
    );
}

// ---------------------------------------------------------------------------
// index_metadata
// ---------------------------------------------------------------------------

#[test]
fn build_writes_index_metadata() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().join("scripts");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("x.py"), "pass\n").unwrap();

    let db_path = dir.path().join("catalog.sqlite");
    build_index(
        std::slice::from_ref(&root),
        &db_path,
        BuildOptions {
            logical_prefix: "/catalog/scripts".into(),
            head_lines: 10,
            keep_copies: 0,
            vc_config: Some(VcConfig::default()),
            ..Default::default()
        },
    )
    .unwrap();

    let conn = open_ro(&db_path);
    let (ts, sv): (String, i64) = conn
        .query_row(
            "SELECT build_timestamp, schema_version FROM index_metadata WHERE id=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();

    assert!(!ts.is_empty(), "build_timestamp must be set");
    assert_eq!(sv, scat_core::core::db::SCHEMA_VERSION);
}

// ---------------------------------------------------------------------------
// dry_run
// ---------------------------------------------------------------------------

#[test]
fn dry_run_does_not_write_live_database() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().join("scripts");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("a.py"), "pass\n").unwrap();

    let db_path = dir.path().join("catalog.sqlite");
    let result = build_index(
        std::slice::from_ref(&root),
        &db_path,
        BuildOptions {
            logical_prefix: "/catalog/scripts".into(),
            head_lines: 10,
            keep_copies: 0,
            dry_run: true,
            vc_config: Some(VcConfig::default()),
            ..Default::default()
        },
    )
    .unwrap();

    assert!(result.dry_run);
    assert!(
        !db_path.exists(),
        "dry_run must NOT create the live DB file"
    );
}

// ---------------------------------------------------------------------------
// logical_prefix in stored paths
// ---------------------------------------------------------------------------

#[test]
fn build_stores_logical_prefix_in_paths() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().join("scripts");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("tool.py"), "pass\n").unwrap();

    let db_path = dir.path().join("catalog.sqlite");
    build_index(
        std::slice::from_ref(&root),
        &db_path,
        BuildOptions {
            logical_prefix: "/catalog/scripts/tools".into(),
            head_lines: 10,
            keep_copies: 0,
            vc_config: Some(VcConfig::default()),
            ..Default::default()
        },
    )
    .unwrap();

    let conn = open_ro(&db_path);
    let path: String = conn
        .query_row("SELECT logical_path FROM scripts LIMIT 1", [], |r| r.get(0))
        .unwrap();

    assert!(
        path.starts_with("/catalog/scripts/tools/"),
        "logical_path should start with the prefix, got: {path}"
    );
}

// ---------------------------------------------------------------------------
// Dependency resolution (module name → indexed script)
// ---------------------------------------------------------------------------

#[test]
fn build_resolves_absolute_and_relative_dependency_paths() {
    let dir = tempfile::TempDir::new().unwrap();
    let pkg = dir.path().join("pkg");
    std::fs::create_dir(&pkg).unwrap();

    // pkg/utils.py  — the target of both an absolute and a relative import
    std::fs::write(pkg.join("utils.py"), "def helper(): pass\n").unwrap();

    // pkg/main.py   — absolute import: `import utils` or `from pkg import utils`
    std::fs::write(pkg.join("main.py"), "import utils\n").unwrap();

    // pkg/rel.py    — relative import: `from . import utils`
    std::fs::write(pkg.join("rel.py"), "from . import utils\n").unwrap();

    let db_path = dir.path().join("catalog.sqlite");
    build_index(
        std::slice::from_ref(&pkg),
        &db_path,
        BuildOptions {
            logical_prefix: "/catalog/scripts/pkg".into(),
            head_lines: 5,
            keep_copies: 0,
            vc_config: Some(VcConfig::default()),
            ..Default::default()
        },
    )
    .unwrap();

    let conn = open_ro(&db_path);

    // Both main.py and rel.py should resolve their dep to utils.py
    let resolved_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM dependencies WHERE resolved_script_id IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        resolved_count, 2,
        "both absolute and relative deps should resolve"
    );

    // Verify the resolved target is utils.py in both cases
    let utils_id: i64 = conn
        .query_row(
            "SELECT id FROM scripts WHERE logical_path = '/catalog/scripts/pkg/utils.py'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let matches: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM dependencies WHERE resolved_script_id = ?",
            rusqlite::params![utils_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(matches, 2);
}

// ---------------------------------------------------------------------------
// Empty scan root
// ---------------------------------------------------------------------------

#[test]
fn build_empty_root_produces_zero_scripts() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().join("empty");
    std::fs::create_dir(&root).unwrap();

    let db_path = dir.path().join("catalog.sqlite");
    let result = build_index(
        std::slice::from_ref(&root),
        &db_path,
        BuildOptions {
            logical_prefix: "/catalog/scripts".into(),
            head_lines: 10,
            keep_copies: 0,
            vc_config: Some(VcConfig::default()),
            ..Default::default()
        },
    )
    .unwrap();

    assert_eq!(result.scripts_indexed, 0);
    assert!(result.errors.is_empty());

    let conn = open_ro(&db_path);
    assert_eq!(script_count(&conn), 0);
}

#[test]
fn build_respects_catignore_and_explicit_ignore_files() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().join("scripts");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(root.join("vendor")).unwrap();
    std::fs::create_dir(root.join("generated")).unwrap();
    std::fs::write(root.join(".catignore"), "vendor/\n").unwrap();
    let explicit_ignore = root.join("build.catignore");
    std::fs::write(&explicit_ignore, "generated/\n").unwrap();
    std::fs::write(root.join("keep.py"), "pass\n").unwrap();
    std::fs::write(root.join("vendor").join("skip.py"), "pass\n").unwrap();
    std::fs::write(root.join("generated").join("skip.py"), "pass\n").unwrap();

    let db_path = dir.path().join("catalog.sqlite");
    let result = build_index(
        std::slice::from_ref(&root),
        &db_path,
        BuildOptions {
            logical_prefix: "/catalog/scripts".into(),
            head_lines: 10,
            ignore_files: vec![explicit_ignore],
            keep_copies: 0,
            vc_config: Some(VcConfig::default()),
            ..Default::default()
        },
    )
    .unwrap();

    assert_eq!(result.scripts_indexed, 1);

    let conn = open_ro(&db_path);
    assert_eq!(script_count(&conn), 1);
    let only_path: String = conn
        .query_row("SELECT logical_path FROM scripts LIMIT 1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(only_path, "/catalog/scripts/keep.py");
}

// ---------------------------------------------------------------------------
// Change-detection (max_mtime_in_roots)
// ---------------------------------------------------------------------------

#[test]
fn max_mtime_returns_none_for_no_files() {
    let dir = tempfile::TempDir::new().unwrap();
    // Empty directory — no files present at all.
    let mtime =
        scat_core::indexer::scanner::max_mtime_in_roots(&[dir.path().to_path_buf()], &[]).unwrap();
    assert!(mtime.is_none());
}

#[test]
fn max_mtime_returns_positive_epoch_for_existing_files() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("a.py"), "# hello").unwrap();
    let mtime = scat_core::indexer::scanner::max_mtime_in_roots(&[dir.path().to_path_buf()], &[])
        .unwrap()
        .expect("expected Some mtime");
    assert!(mtime > 1_000_000_000.0, "mtime should be a plausible epoch");
}

#[test]
fn build_timestamp_after_index_is_newer_than_pre_build_mtime() {
    // Build a small catalog, then verify that the stored build_timestamp is
    // at least as recent as the files' mtimes (so a subsequent up-to-date
    // check would see no newer files).
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().join("scripts");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("tool.sh"), "#!/bin/bash\necho hi\n").unwrap();

    let db_path = dir.path().join("catalog.sqlite");

    // Capture max mtime BEFORE the build.
    let pre_build_mtime =
        scat_core::indexer::scanner::max_mtime_in_roots(std::slice::from_ref(&root), &[])
            .unwrap()
            .expect("expected Some mtime");

    build_index(
        std::slice::from_ref(&root),
        &db_path,
        BuildOptions {
            logical_prefix: "/scripts".into(),
            head_lines: 5,
            keep_copies: 0,
            vc_config: Some(VcConfig::default()),
            ..Default::default()
        },
    )
    .unwrap();

    // Read the stored build_timestamp from index_metadata.
    let conn = open_ro(&db_path);
    let ts: String = conn
        .query_row(
            "SELECT build_timestamp FROM index_metadata WHERE id = 1",
            [],
            |r| r.get(0),
        )
        .unwrap();

    let build_epoch = chrono::DateTime::parse_from_rfc3339(&ts)
        .expect("build_timestamp must be RFC 3339")
        .timestamp() as f64;

    // The change-detection check truncates file mtimes to whole seconds before
    // comparing with the stored (whole-second) build_timestamp.  So the
    // relevant assertion is that the build_timestamp (in whole seconds) is >=
    // the file mtime truncated to whole seconds.
    assert!(
        build_epoch >= pre_build_mtime.floor(),
        "build_timestamp ({build_epoch}) should be >= floor(pre-build mtime) ({})",
        pre_build_mtime.floor()
    );
}
