use super::deps::{cmd_deps, render_tree_lines};
use super::index::should_skip_catalog_rebuild;
use super::search::query_uses_fts;
use super::show::{cmd_show, render_revision_lines, revisions_to_json};
use super::stats::render_revision_stats_lines;
use crate::cli::OutputFormat;
use scat_core::core::db::{JsonRow, SCHEMA_VERSION, create_db};
use scat_core::core::search::{RevisionStats, SearchApi};
use scat_core::core::vc::relative_age;
use tempfile::NamedTempFile;

fn make_api() -> (SearchApi, NamedTempFile) {
    let file = NamedTempFile::new().unwrap();
    let conn = create_db(file.path()).unwrap();
    conn.execute(
        "INSERT INTO index_metadata (id, build_timestamp, schema_version)
         VALUES (1, '2024-01-01T00:00:00', ?1)",
        rusqlite::params![SCHEMA_VERSION],
    )
    .unwrap();

    (SearchApi::new(conn), file)
}

fn insert_script(api: &SearchApi, path: &str) {
    api.conn
        .execute(
            "INSERT INTO scripts (logical_path, language, owner, purpose) VALUES (?1,'python','alice','')",
            rusqlite::params![path],
        )
        .unwrap();
}

fn revision_row(os_flavor: &str, user: &str, timestamp: &str, age_seconds: Option<f64>) -> JsonRow {
    let mut row = JsonRow::new();
    row.insert("logical_path".to_string(), "/catalog/scripts/foo.py".into());
    row.insert("physical_path".to_string(), "/tmp/foo_checkout".into());
    row.insert("revision_type".to_string(), "DEVELOP".into());
    row.insert("os_flavor".to_string(), os_flavor.into());
    row.insert("user".to_string(), user.into());
    row.insert("timestamp".to_string(), timestamp.into());
    row.insert(
        "age_seconds".to_string(),
        age_seconds
            .map(serde_json::Value::from)
            .unwrap_or(serde_json::Value::Null),
    );
    row
}

#[test]
fn render_revision_lines_groups_by_os_and_sorts_newest_first() {
    let rows = vec![
        revision_row("ZOS", "alice", "20240101_1000", Some(3_600.0)),
        revision_row("LINUX", "jdoe", "20240102_0900", Some(7_200.0)),
        revision_row("LINUX", "bob", "20240101_0900", Some(86_400.0)),
    ];

    let lines = render_revision_lines(rows);

    assert_eq!(lines.len(), 3);
    assert!(lines[0].contains("LINUX"));
    assert!(lines[0].contains("jdoe"));
    assert!(lines[1].contains("LINUX"));
    assert!(lines[1].contains("bob"));
    assert!(lines[2].contains("ZOS"));
}

#[test]
fn revisions_to_json_emits_structured_array() {
    let rows = vec![revision_row("LINUX", "jdoe", "20240102_0900", Some(120.0))];

    let value = revisions_to_json(rows);
    let array = value.as_array().expect("revisions should be an array");

    assert_eq!(array.len(), 1);
    assert_eq!(array[0]["os_flavor"], "LINUX");
    assert_eq!(array[0]["user"], "jdoe");
    assert_eq!(array[0]["timestamp"], "20240102_0900");
    assert_eq!(array[0]["age_seconds"], 120.0);
}

#[test]
fn query_uses_fts_excludes_path_like_queries() {
    assert!(query_uses_fts("checkmc"));
    assert!(!query_uses_fts("scripts/checkmc"));
    assert!(!query_uses_fts("checkmc.py"));
}

#[test]
fn skip_rebuild_requires_existing_older_file_mtime() {
    assert!(should_skip_catalog_rebuild(100.0, Some(99.9)));
    assert!(!should_skip_catalog_rebuild(100.0, Some(100.0)));
    assert!(!should_skip_catalog_rebuild(100.0, Some(100.1)));
    assert!(!should_skip_catalog_rebuild(100.0, None));
}

#[test]
fn relative_age_clamps_negative_values() {
    assert_eq!(relative_age(-120.0), "0m ago");
}

#[test]
fn cmd_show_missing_script_returns_plain_error() {
    let (api, _file) = make_api();
    let err = cmd_show(
        &api,
        "/catalog/scripts/missing.py",
        &[],
        OutputFormat::Table,
        false,
        false,
    )
    .unwrap_err();

    assert_eq!(
        err.to_string(),
        "script '/catalog/scripts/missing.py' not found in catalog"
    );
}

#[test]
fn cmd_show_accepts_folder_and_siblings_fields() {
    let (api, _file) = make_api();
    insert_script(&api, "/catalog/scripts/a.py");
    insert_script(&api, "/catalog/scripts/b.py");

    for output in [OutputFormat::Table, OutputFormat::Json] {
        cmd_show(
            &api,
            "/catalog/scripts/a.py",
            &["folder".to_string(), "siblings".to_string()],
            output,
            false,
            false,
        )
        .unwrap();
    }
}

#[test]
fn cmd_deps_missing_script_returns_plain_error() {
    let (api, _file) = make_api();
    let err = cmd_deps(
        &api,
        "/catalog/scripts/missing.py",
        false,
        None,
        OutputFormat::Table,
        true,
    )
    .unwrap_err();

    assert_eq!(
        err.to_string(),
        "script '/catalog/scripts/missing.py' not found in catalog"
    );
}

#[test]
fn render_tree_lines_draws_branches_and_markers() {
    use scat_core::core::search::DepsTreeNode;

    let node = |path: &str, children: Vec<DepsTreeNode>| DepsTreeNode {
        logical_path: path.to_string(),
        indexed: true,
        cycle: false,
        repeated: false,
        truncated: false,
        via_kind: Some("import".to_string()),
        children,
    };

    let mut external = node("/external/lib.py", vec![]);
    external.indexed = false;
    let mut shared_again = node("/catalog/shared.py", vec![]);
    shared_again.repeated = true;
    let mut referenced = node("/catalog/runner.sh", vec![]);
    referenced.via_kind = Some("referenced".to_string());
    let root = node(
        "/catalog/a.py",
        vec![
            node(
                "/catalog/b.py",
                vec![node(
                    "/catalog/shared.py",
                    vec![node("/catalog/leaf.py", vec![])],
                )],
            ),
            node("/catalog/c.py", vec![shared_again, external, referenced]),
        ],
    );

    assert_eq!(
        render_tree_lines(&root),
        vec![
            "/catalog/a.py",
            "├── /catalog/b.py",
            "│   └── /catalog/shared.py",
            "│       └── /catalog/leaf.py",
            "└── /catalog/c.py",
            "    ├── /catalog/shared.py (*)",
            "    ├── /external/lib.py (not indexed)",
            "    └── /catalog/runner.sh (ref)",
        ]
    );
}

#[test]
fn query_uses_fts_routes_paths_including_backslashes() {
    assert!(query_uses_fts("patch"), "plain word → FTS");
    assert!(
        !query_uses_fts("jobs/nightly"),
        "forward-slash path → path search"
    );
    assert!(!query_uses_fts("foo.py"), "dotted name → path search");
    assert!(
        !query_uses_fts("scripts\\foo"),
        "Windows backslash path → path search"
    );
}

#[test]
fn render_revision_stats_lines_matches_catalog_stats_labels() {
    let lines = render_revision_stats_lines(&RevisionStats {
        scripts_with_active_checkouts: 12,
        scripts_with_archive_entries: 47,
        total_develop_revision_files: 18,
        total_archive_revision_files: 134,
        scripts_with_working_versions: 51,
        total_working_revision_files: 96,
        scripts_checked_out_by_multiple_users: 2,
    });

    assert_eq!(
        lines,
        vec![
            "  Scripts with active checkouts: 12",
            "  Scripts with archive entries: 47",
            "  Total DEVELOP revision files: 18",
            "  Total ARCHIVE revision files: 134",
            "  Scripts with working versions: 51",
            "  Total WORKING revision files: 96",
            "  Scripts checked out by >1 user: 2",
        ]
    );
}
