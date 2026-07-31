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
// Publication: journal mode and the host-local cache
// ---------------------------------------------------------------------------

#[test]
fn published_catalog_is_a_single_file_in_rollback_journal_mode() {
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

    // WAL needs an `mmap`-able `-shm` file — which network filesystems
    // generally don't provide — even to open the catalog read-only. The
    // published file must therefore carry a rollback journal and no sidecars.
    let mode: String = open_ro(&db_path)
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
        .unwrap();
    assert_eq!(mode.to_lowercase(), "delete");
    assert!(!db_path.with_extension("sqlite-wal").exists());
    assert!(!db_path.with_extension("sqlite-shm").exists());
}

#[test]
fn a_built_catalog_round_trips_through_the_host_local_cache() {
    use scat_core::core::cache::{CacheOptions, catalog_read_path};

    let dir = tempfile::TempDir::new().unwrap();
    let cache_root = tempfile::TempDir::new().unwrap();
    let root = dir.path().join("scripts");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("deploy.py"), "import os\n").unwrap();

    let db_path = dir.path().join("catalog.sqlite");
    let options = CacheOptions {
        enabled: true,
        root: Some(cache_root.path().to_path_buf()),
    };

    let build = |db_path: &std::path::Path| {
        build_index(
            std::slice::from_ref(&root),
            db_path,
            BuildOptions {
                logical_prefix: "/catalog/scripts".into(),
                head_lines: 10,
                keep_copies: 0,
                vc_config: Some(VcConfig::default()),
                ..Default::default()
            },
        )
        .unwrap();
    };

    build(&db_path);
    let cached = catalog_read_path(&db_path, &options);
    assert_ne!(cached, db_path, "the build should be cached locally");
    assert_eq!(
        script_count(&scat_core::core::db::open_db(&cached).unwrap()),
        1
    );

    // A second nightly build must be picked up, not served stale.
    std::fs::write(root.join("rollback.sh"), "#!/bin/bash\necho ok\n").unwrap();
    build(&db_path);
    let refreshed = catalog_read_path(&db_path, &options);
    assert_eq!(refreshed, cached);
    assert_eq!(
        script_count(&scat_core::core::db::open_db(&refreshed).unwrap()),
        2
    );
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
// Path-literal "referenced" dependencies
// ---------------------------------------------------------------------------

#[test]
fn build_records_referenced_path_dependencies() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().join("scripts");
    std::fs::create_dir(&root).unwrap();

    // Target script, invoked by other scripts via its full logical path.
    std::fs::write(
        root.join("lib.py"),
        // A script mentioning its own logical path must NOT create a self-edge.
        "# defined at /catalog/scripts/lib.py\nprint('lib')\n",
    )
    .unwrap();

    // A shell script that copies + remote-executes lib.py by path, plus a
    // reference to a path that is not indexed (must be dropped) and an
    // unrelated temp path (must be dropped).
    std::fs::write(
        root.join("runner.sh"),
        "#!/bin/bash\n\
         scp /catalog/scripts/lib.py host:/tmp/\n\
         ssh host python3 /catalog/scripts/lib.py\n\
         python3 /catalog/scripts/missing.py\n\
         cat /tmp/scratch.sh\n",
    )
    .unwrap();

    // A JSON manifest listing scripts to run in order.
    std::fs::write(
        root.join("pipeline.json"),
        r#"{"steps": ["/catalog/scripts/lib.py"]}"#,
    )
    .unwrap();

    let db_path = dir.path().join("catalog.sqlite");
    build_index(
        std::slice::from_ref(&root),
        &db_path,
        BuildOptions {
            logical_prefix: "/catalog/scripts".into(),
            head_lines: 5,
            keep_copies: 0,
            vc_config: Some(VcConfig::default()),
            ..Default::default()
        },
    )
    .unwrap();

    let conn = open_ro(&db_path);

    let lib_id: i64 = conn
        .query_row(
            "SELECT id FROM scripts WHERE logical_path = '/catalog/scripts/lib.py'",
            [],
            |r| r.get(0),
        )
        .unwrap();

    // Every referenced edge that survived resolution must be resolved (the
    // unresolved candidates — missing.py, /tmp/scratch.sh — are dropped).
    let unresolved_refs: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM dependencies WHERE kind = 'referenced' AND resolved_script_id IS NULL",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        unresolved_refs, 0,
        "unresolved referenced edges must be dropped"
    );

    // runner.sh and pipeline.json both reference lib.py → two referenced edges,
    // both resolved to lib.py.
    let refs_to_lib: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM dependencies WHERE kind = 'referenced' AND resolved_script_id = ?",
            rusqlite::params![lib_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        refs_to_lib, 2,
        "runner.sh and pipeline.json reference lib.py"
    );

    // lib.py mentioning its own path must not create a self-edge.
    let self_edges: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM dependencies d
             WHERE d.kind = 'referenced' AND d.script_id = ? AND d.resolved_script_id = ?",
            rusqlite::params![lib_id, lib_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        self_edges, 0,
        "a script referencing its own path is not a dependency"
    );
}

#[test]
fn build_resolves_relative_and_cross_language_references() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().join("scripts");
    std::fs::create_dir_all(root.join("lib")).unwrap();
    std::fs::create_dir_all(root.join("jobs")).unwrap();
    std::fs::create_dir_all(root.join("bin")).unwrap();

    std::fs::write(root.join("lib/common.py"), "def go():\n    pass\n").unwrap();
    std::fs::write(root.join("lib/task.sh"), "#!/bin/bash\necho hi\n").unwrap();

    // shell → python via a RELATIVE path (../lib/common.py).
    std::fs::write(
        root.join("jobs/run.sh"),
        "#!/bin/bash\npython3 ../lib/common.py\n",
    )
    .unwrap();

    // python → shell via an ABSOLUTE path (cross-language).
    std::fs::write(
        root.join("jobs/orchestrate.py"),
        "import subprocess\nsubprocess.run([\"/catalog/lib/task.sh\"])\n",
    )
    .unwrap();

    // An extension-less script (indexed via shebang) that OS-branches between
    // a .sh and a .py — both appear as literals, so both must be captured.
    std::fs::write(
        root.join("bin/dispatch"),
        "#!/bin/bash\nif [ \"$(uname)\" = Linux ]; then\n  exec /catalog/lib/task.sh\nelse\n  exec python3 /catalog/lib/common.py\nfi\n",
    )
    .unwrap();

    let db_path = dir.path().join("catalog.sqlite");
    build_index(
        std::slice::from_ref(&root),
        &db_path,
        BuildOptions {
            logical_prefix: "/catalog".into(),
            head_lines: 5,
            keep_copies: 0,
            vc_config: Some(VcConfig::default()),
            ..Default::default()
        },
    )
    .unwrap();

    let conn = open_ro(&db_path);
    let referrers_of = |target: &str| -> Vec<String> {
        let target_id: i64 = conn
            .query_row(
                "SELECT id FROM scripts WHERE logical_path = ?1",
                rusqlite::params![target],
                |r| r.get(0),
            )
            .unwrap();
        let mut stmt = conn
            .prepare(
                "SELECT s.logical_path
                 FROM dependencies d JOIN scripts s ON s.id = d.script_id
                 WHERE d.kind = 'referenced' AND d.resolved_script_id = ?1
                 ORDER BY s.logical_path",
            )
            .unwrap();
        stmt.query_map(rusqlite::params![target_id], |r| r.get::<_, String>(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    };

    // common.py is referenced by the relative shell ref and the dispatch branch.
    assert_eq!(
        referrers_of("/catalog/lib/common.py"),
        vec![
            "/catalog/bin/dispatch".to_string(),
            "/catalog/jobs/run.sh".to_string(),
        ]
    );
    // task.sh is referenced cross-language by the python orchestrator and dispatch.
    assert_eq!(
        referrers_of("/catalog/lib/task.sh"),
        vec![
            "/catalog/bin/dispatch".to_string(),
            "/catalog/jobs/orchestrate.py".to_string(),
        ]
    );
}

#[test]
fn build_source_by_path_resolves_cleanly_without_extension_collision() {
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().join("scripts");
    std::fs::create_dir_all(root.join("lib")).unwrap();
    std::fs::create_dir_all(root.join("jobs")).unwrap();

    std::fs::write(root.join("lib/common.sh"), "#!/bin/bash\necho common\n").unwrap();
    // Absolute and relative `source` of a path — must resolve to common.sh as a
    // single `referenced` edge, with no import edge mis-resolved via the bare
    // `sh` module suffix (which previously produced a bogus self/cross edge).
    std::fs::write(
        root.join("jobs/a.sh"),
        "#!/bin/bash\nsource /catalog/lib/common.sh\n",
    )
    .unwrap();
    std::fs::write(
        root.join("jobs/b.sh"),
        "#!/bin/bash\nsource ../lib/common.sh\n",
    )
    .unwrap();

    let db_path = dir.path().join("catalog.sqlite");
    build_index(
        std::slice::from_ref(&root),
        &db_path,
        BuildOptions {
            logical_prefix: "/catalog".into(),
            head_lines: 5,
            keep_copies: 0,
            vc_config: Some(VcConfig::default()),
            ..Default::default()
        },
    )
    .unwrap();

    let conn = open_ro(&db_path);

    // a.sh and b.sh each have exactly one dependency edge: a referenced edge to
    // common.sh. No import edge, and no bogus self- or cross-edge.
    for caller in ["/catalog/jobs/a.sh", "/catalog/jobs/b.sh"] {
        let edges: Vec<(String, String, Option<i64>)> = {
            let caller_id: i64 = conn
                .query_row(
                    "SELECT id FROM scripts WHERE logical_path = ?1",
                    rusqlite::params![caller],
                    |r| r.get(0),
                )
                .unwrap();
            let mut stmt = conn
                .prepare(
                    "SELECT depends_on_path, kind, resolved_script_id
                     FROM dependencies WHERE script_id = ?1",
                )
                .unwrap();
            stmt.query_map(rusqlite::params![caller_id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect()
        };
        assert_eq!(
            edges.len(),
            1,
            "{caller} should have exactly one edge: {edges:?}"
        );
        assert_eq!(
            edges[0].1, "referenced",
            "{caller} edge should be referenced"
        );
        assert!(edges[0].2.is_some(), "{caller} edge should be resolved");
    }

    // common.sh is used by both a.sh and b.sh.
    let common_id: i64 = conn
        .query_row(
            "SELECT id FROM scripts WHERE logical_path = '/catalog/lib/common.sh'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let user_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM dependencies WHERE resolved_script_id = ?1",
            rusqlite::params![common_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(user_count, 2);
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
        scat_core::indexer::scanner::max_mtime_in_roots(&[dir.path().to_path_buf()], &[], &[])
            .unwrap();
    assert!(mtime.is_none());
}

#[test]
fn max_mtime_returns_positive_epoch_for_existing_files() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("a.py"), "# hello").unwrap();
    let mtime =
        scat_core::indexer::scanner::max_mtime_in_roots(&[dir.path().to_path_buf()], &[], &[])
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
        scat_core::indexer::scanner::max_mtime_in_roots(std::slice::from_ref(&root), &[], &[])
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

// ---------------------------------------------------------------------------
// vc working directories
// ---------------------------------------------------------------------------

#[test]
fn build_indexes_a_vc_working_directory_as_one_script_per_tool() {
    // A vc working directory as it actually looks on disk: an active symlink
    // per tool, the retained version copies vc keeps beside it for rollback,
    // an editor backup, and the DEVELOP/ARCHIVE containers. Only the two
    // tools belong in `scripts`; everything else is a revision of one of them.
    let dir = tempfile::TempDir::new().unwrap();
    let root = dir.path().join("source");
    std::fs::create_dir_all(root.join("DEVELOP")).unwrap();
    std::fs::create_dir_all(root.join("ARCHIVE")).unwrap();

    // An extensionless shell tool, detected by its shebang alone.
    std::fs::write(
        root.join("prepare_release_20260729_140513"),
        "#!/bin/sh\necho release\n",
    )
    .unwrap();
    std::fs::write(
        root.join("prepare_release_20260701_105550"),
        "#!/bin/sh\necho release\n",
    )
    .unwrap();
    std::os::unix::fs::symlink(
        "prepare_release_20260729_140513",
        root.join("prepare_release"),
    )
    .unwrap();
    std::fs::write(root.join("prepare_release~"), "#!/bin/sh\nold\n").unwrap();

    // An unrelated tool in the same directory, differing only in that its
    // name carries an extension — the control for the fix below.
    std::fs::write(
        root.join("release_backup.sh_20250311_080827"),
        "#!/bin/bash\necho backup\n",
    )
    .unwrap();
    std::os::unix::fs::symlink(
        "release_backup.sh_20250311_080827",
        root.join("release_backup.sh"),
    )
    .unwrap();

    std::fs::write(
        root.join("ARCHIVE").join("prepare_release_20260420_082127"),
        "#!/bin/sh\necho old\n",
    )
    .unwrap();
    std::fs::write(
        root.join("DEVELOP")
            .join("prepare_release_20260729_152101_dev"),
        "#!/bin/sh\necho wip\n",
    )
    .unwrap();

    let db_path = dir.path().join("catalog.sqlite");
    build_index(
        std::slice::from_ref(&root),
        &db_path,
        BuildOptions {
            logical_prefix: "/catalog/source".into(),
            head_lines: 10,
            keep_copies: 0,
            vc_config: Some(VcConfig {
                scan_roots: vec![root.clone()],
                ..Default::default()
            }),
            ..Default::default()
        },
    )
    .unwrap();

    let conn = open_ro(&db_path);
    let mut stmt = conn
        .prepare("SELECT logical_path FROM scripts ORDER BY logical_path")
        .unwrap();
    let paths: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        paths,
        vec![
            "/catalog/source/prepare_release".to_string(),
            "/catalog/source/release_backup.sh".to_string(),
        ],
        "version copies, editor backups, and container contents are not scripts"
    );

    // The symlink row carries the target the CLI and TUI render as an arrow.
    let target: String = conn
        .query_row(
            "SELECT symlink_target FROM scripts WHERE logical_path = '/catalog/source/prepare_release'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(target, "/catalog/source/prepare_release_20260729_140513");

    // Every non-script file is still catalogued, as a revision of its tool.
    let revisions: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM revisions WHERE logical_path = '/catalog/source/prepare_release'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(
        revisions, 4,
        "two working copies plus the archive and develop entries"
    );
}
