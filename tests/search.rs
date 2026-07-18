/// Integration tests for SearchApi against an in-memory database
/// built with the production schema (schema version 4).
use scat_core::core::db::{SCHEMA_VERSION, create_db};
use scat_core::core::search::{SearchApi, TreeDirection, compare_catalogs};
use scat_core::core::vc::REVISION_TYPE_DEVELOP;
use tempfile::NamedTempFile;

fn make_api() -> (SearchApi, NamedTempFile) {
    let f = NamedTempFile::new().unwrap();
    let conn = create_db(f.path()).unwrap();
    // Insert index_metadata so info() works.
    conn.execute(
        "INSERT INTO index_metadata (id, build_timestamp, schema_version) VALUES (1, '2024-01-01T00:00:00', ?)",
        rusqlite::params![SCHEMA_VERSION],
    ).unwrap();
    let api = SearchApi::new(conn);
    (api, f)
}

fn insert(api: &SearchApi, path: &str, content: &str, language: &str, owner: &str, purpose: &str) {
    api.conn.execute(
        "INSERT INTO scripts (logical_path, language, content, owner, purpose) VALUES (?1,?2,?3,?4,?5)",
        rusqlite::params![path, language, content, owner, purpose],
    ).unwrap();
}

fn insert_diff_script(
    api: &SearchApi,
    path: &str,
    language: &str,
    owner: &str,
    purpose: &str,
    metadata_json: &str,
) {
    api.conn.execute(
        "INSERT INTO scripts (logical_path, language, owner, purpose, metadata_json, tags, entry_points, related)
         VALUES (?1,?2,?3,?4,?5,'[]','[]','[]')",
        rusqlite::params![path, language, owner, purpose, metadata_json],
    ).unwrap();
}

fn id_for(api: &SearchApi, path: &str) -> i64 {
    api.conn
        .query_row(
            "SELECT id FROM scripts WHERE logical_path = ?1",
            rusqlite::params![path],
            |r| r.get(0),
        )
        .unwrap()
}

/// Insert a dependency row and populate `resolved_script_id` when the target
/// script is already in the catalog (mirrors what `resolve_dependency_targets`
/// does at build time).
fn insert_dep(api: &SearchApi, from_path: &str, to_path: &str) {
    api.conn
        .execute(
            "INSERT INTO dependencies (script_id, depends_on_path, resolved_script_id)
             VALUES (
                 (SELECT id FROM scripts WHERE logical_path = ?1),
                 ?2,
                 (SELECT id FROM scripts WHERE logical_path = ?2)
             )",
            rusqlite::params![from_path, to_path],
        )
        .unwrap();
}

/// Insert a resolved `referenced` (path-literal) dependency edge.
fn insert_reference_dep(api: &SearchApi, from_path: &str, to_path: &str) {
    api.conn
        .execute(
            "INSERT INTO dependencies (script_id, depends_on_path, resolved_script_id, kind)
             VALUES (
                 (SELECT id FROM scripts WHERE logical_path = ?1),
                 ?2,
                 (SELECT id FROM scripts WHERE logical_path = ?2),
                 'referenced'
             )",
            rusqlite::params![from_path, to_path],
        )
        .unwrap();
}

fn insert_revision(
    api: &SearchApi,
    logical_path: &str,
    physical_path: &str,
    revision_type: &str,
    age_seconds: Option<i64>,
) {
    api.conn
        .execute(
            "INSERT INTO revisions
             (logical_path, physical_path, revision_type, os_flavor, user, timestamp, age_seconds)
             VALUES (?1, ?2, ?3, 'LINUX', 'jdoe', '20240315_1430', ?4)",
            rusqlite::params![logical_path, physical_path, revision_type, age_seconds],
        )
        .unwrap();
}

// ---------------------------------------------------------------------------
// search / list_scripts
// ---------------------------------------------------------------------------

#[test]
fn search_returns_ranked_results() {
    let (api, _f) = make_api();
    insert(
        &api,
        "/catalog/scripts/a.py",
        "patch patch patch",
        "python",
        "alice",
        "",
    );
    insert(
        &api,
        "/catalog/scripts/b.py",
        "patch deployment",
        "python",
        "bob",
        "",
    );

    let results = api
        .search_with_filters("patch", 10, None, None, None)
        .unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(
        results[0]["logical_path"].as_str().unwrap(),
        "/catalog/scripts/a.py"
    );
}

#[test]
fn search_language_filter() {
    let (api, _f) = make_api();
    insert(
        &api,
        "/catalog/scripts/a.py",
        "common word",
        "python",
        "",
        "",
    );
    insert(
        &api,
        "/catalog/scripts/b.sh",
        "common word",
        "shell",
        "",
        "",
    );

    let results = api
        .search_with_filters("common", 10, Some("python"), None, None)
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0]["logical_path"].as_str().unwrap(),
        "/catalog/scripts/a.py"
    );
}

#[test]
fn search_owner_and_tag_filters_apply_before_limit() {
    let (api, _f) = make_api();
    insert(&api, "/catalog/scripts/a.py", "needle", "python", "bob", "");
    insert(&api, "/catalog/scripts/b.py", "needle", "python", "bob", "");
    insert(
        &api,
        "/catalog/scripts/c.py",
        "needle",
        "python",
        "alice",
        "",
    );
    insert(
        &api,
        "/catalog/scripts/d.py",
        "needle",
        "python",
        "alice",
        "",
    );

    api.conn
        .execute(
            r#"UPDATE scripts SET tags = '["deploy"]' WHERE owner = 'alice'"#,
            [],
        )
        .unwrap();

    let results = api
        .search_with_filters("needle", 2, None, Some("alice"), Some("deploy"))
        .unwrap();
    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|r| r["owner"].as_str() == Some("alice")));

    let results = api
        .search_with_filters("needle", 2, None, Some("alice"), Some("dep"))
        .unwrap();
    assert!(
        results.is_empty(),
        "FTS tag filtering should use exact JSON membership"
    );
}

#[test]
fn search_index_updates_when_script_content_changes() {
    let (api, _f) = make_api();
    insert(
        &api,
        "/catalog/scripts/upd.py",
        "old content",
        "python",
        "",
        "",
    );

    api.conn
        .execute(
            "UPDATE scripts SET content = 'new content' WHERE logical_path = '/catalog/scripts/upd.py'",
            [],
        )
        .unwrap();

    assert!(
        api.search_with_filters("old", 10, None, None, None)
            .unwrap()
            .is_empty()
    );
    let results = api
        .search_with_filters("new", 10, None, None, None)
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0]["logical_path"].as_str().unwrap(),
        "/catalog/scripts/upd.py"
    );
}

#[test]
fn search_index_removes_deleted_scripts() {
    let (api, _f) = make_api();
    insert(
        &api,
        "/catalog/scripts/del.py",
        "deletable content",
        "python",
        "",
        "",
    );

    api.conn
        .execute(
            "DELETE FROM scripts WHERE logical_path = '/catalog/scripts/del.py'",
            [],
        )
        .unwrap();

    assert!(
        api.search_with_filters("deletable", 10, None, None, None)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn list_scripts_returns_all_ordered() {
    let (api, _f) = make_api();
    insert(&api, "/catalog/scripts/z.py", "", "python", "", "");
    insert(&api, "/catalog/scripts/a.py", "", "python", "", "");

    let results = api.list_scripts(None, None, None, 10, 0).unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(
        results[0]["logical_path"].as_str().unwrap(),
        "/catalog/scripts/a.py"
    );
    assert_eq!(
        results[1]["logical_path"].as_str().unwrap(),
        "/catalog/scripts/z.py"
    );
}

#[test]
fn list_scripts_language_filter() {
    let (api, _f) = make_api();
    insert(&api, "/catalog/scripts/a.py", "", "python", "", "");
    insert(&api, "/catalog/scripts/b.sh", "", "shell", "", "");

    let results = api.list_scripts(Some("python"), None, None, 10, 0).unwrap();
    assert_eq!(results.len(), 1);
}

// ---------------------------------------------------------------------------
// get_script
// ---------------------------------------------------------------------------

#[test]
fn get_script_found() {
    let (api, _f) = make_api();
    insert(
        &api,
        "/catalog/scripts/foo.py",
        "content",
        "python",
        "alice",
        "does things",
    );

    let s = api.get_script("/catalog/scripts/foo.py").unwrap().unwrap();
    assert_eq!(
        s["logical_path"].as_str().unwrap(),
        "/catalog/scripts/foo.py"
    );
    assert_eq!(s["owner"].as_str().unwrap(), "alice");
    assert_eq!(s["purpose"].as_str().unwrap(), "does things");
}

#[test]
fn get_script_not_found() {
    let (api, _f) = make_api();
    assert!(
        api.get_script("/catalog/scripts/nonexistent.py")
            .unwrap()
            .is_none()
    );
}

// ---------------------------------------------------------------------------
// stats
// ---------------------------------------------------------------------------

#[test]
fn stats_counts_by_language() {
    let (api, _f) = make_api();
    insert(&api, "/catalog/scripts/a.py", "", "python", "", "");
    insert(&api, "/catalog/scripts/b.py", "", "python", "", "");
    insert(&api, "/catalog/scripts/c.sh", "", "shell", "", "");

    let stats = api.stats().unwrap();
    assert_eq!(stats.total_scripts, 3);
    assert!(stats.revisions.is_none());
    assert!(
        stats
            .by_language
            .iter()
            .any(|l| l.language == "python" && l.count == 2)
    );
    assert!(
        stats
            .by_language
            .iter()
            .any(|l| l.language == "shell" && l.count == 1)
    );
    let json = serde_json::to_value(&stats).unwrap();
    assert!(json.get("revisions").is_none());
}

#[test]
fn stats_include_revision_statistics_when_available() {
    let (api, _f) = make_api();
    insert(&api, "/catalog/scripts/a.py", "", "python", "", "");
    insert(&api, "/catalog/scripts/b.py", "", "python", "", "");
    insert(&api, "/catalog/scripts/c.py", "", "python", "", "");

    for (logical_path, physical_path, revision_type, os_flavor, user, timestamp) in [
        (
            "/catalog/scripts/a.py",
            "/develop/LINUX/a_20240315_1430_alice.py",
            "DEVELOP",
            "LINUX",
            "alice",
            "20240315_1430",
        ),
        (
            "/catalog/scripts/a.py",
            "/develop/ZOS/a_20240316_1430_bob.py",
            "DEVELOP",
            "ZOS",
            "bob",
            "20240316_1430",
        ),
        (
            "/catalog/scripts/b.py",
            "/develop/LINUX/b_20240316_1430_alice.py",
            "DEVELOP",
            "LINUX",
            "alice",
            "20240316_1430",
        ),
        (
            "/catalog/scripts/a.py",
            "/archive/LINUX/a_20240301_1430_alice.py",
            "ARCHIVE",
            "LINUX",
            "alice",
            "20240301_1430",
        ),
        (
            "/catalog/scripts/c.py",
            "/archive/LINUX/c_20240302_1430_carla.py",
            "ARCHIVE",
            "LINUX",
            "carla",
            "20240302_1430",
        ),
        (
            "/catalog/scripts/c.py",
            "/archive/ZOS/c_20240303_1430_carla.py",
            "ARCHIVE",
            "ZOS",
            "carla",
            "20240303_1430",
        ),
    ] {
        api.conn
            .execute(
                "INSERT INTO revisions
                 (logical_path, physical_path, revision_type, os_flavor, user, timestamp)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    logical_path,
                    physical_path,
                    revision_type,
                    os_flavor,
                    user,
                    timestamp
                ],
            )
            .unwrap();
    }

    let stats = api.stats().unwrap();
    let revisions = stats
        .revisions
        .as_ref()
        .expect("revision stats should be present");

    assert_eq!(revisions.scripts_with_active_checkouts, 2);
    assert_eq!(revisions.scripts_with_archive_entries, 2);
    assert_eq!(revisions.total_develop_revision_files, 3);
    assert_eq!(revisions.total_archive_revision_files, 3);
    assert_eq!(revisions.scripts_checked_out_by_multiple_users, 1);

    let json = serde_json::to_value(&stats).unwrap();
    assert_eq!(json["revisions"]["scripts_with_active_checkouts"], 2);
    assert_eq!(json["revisions"]["scripts_with_archive_entries"], 2);
    assert_eq!(json["revisions"]["total_develop_revision_files"], 3);
    assert_eq!(json["revisions"]["total_archive_revision_files"], 3);
    assert_eq!(
        json["revisions"]["scripts_checked_out_by_multiple_users"],
        1
    );
}

// ---------------------------------------------------------------------------
// symlinks_to
// ---------------------------------------------------------------------------

#[test]
fn symlinks_to_returns_matching_symlinks() {
    let (api, _f) = make_api();
    insert(&api, "/catalog/scripts/target.py", "", "python", "", "");
    api.conn
        .execute(
            "INSERT INTO scripts (logical_path, language, symlink_target) VALUES ('/catalog/scripts/link.py','python','/catalog/scripts/target.py')",
            [],
        )
        .unwrap();

    let results = api.symlinks_to("/catalog/scripts/target.py").unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0]["logical_path"].as_str().unwrap(),
        "/catalog/scripts/link.py"
    );
}

#[test]
fn symlinks_to_empty_when_no_symlinks() {
    let (api, _f) = make_api();
    insert(&api, "/catalog/scripts/target.py", "", "python", "", "");
    assert!(
        api.symlinks_to("/catalog/scripts/target.py")
            .unwrap()
            .is_empty()
    );
}

// ---------------------------------------------------------------------------
// list_scripts – pagination and owner filter
// ---------------------------------------------------------------------------

#[test]
fn list_scripts_pagination() {
    let (api, _f) = make_api();
    insert(&api, "/catalog/scripts/a.py", "", "python", "", "");
    insert(&api, "/catalog/scripts/b.py", "", "python", "", "");
    insert(&api, "/catalog/scripts/c.py", "", "python", "", "");

    let page1 = api.list_scripts(None, None, None, 2, 0).unwrap();
    assert_eq!(page1.len(), 2);
    assert_eq!(
        page1[0]["logical_path"].as_str().unwrap(),
        "/catalog/scripts/a.py"
    );

    let page2 = api.list_scripts(None, None, None, 2, 2).unwrap();
    assert_eq!(page2.len(), 1);
    assert_eq!(
        page2[0]["logical_path"].as_str().unwrap(),
        "/catalog/scripts/c.py"
    );
}

#[test]
fn list_scripts_owner_filter() {
    let (api, _f) = make_api();
    insert(&api, "/catalog/scripts/a.py", "", "python", "alice", "");
    insert(&api, "/catalog/scripts/b.py", "", "python", "bob", "");

    let results = api.list_scripts(None, Some("alice"), None, 10, 0).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0]["logical_path"].as_str().unwrap(),
        "/catalog/scripts/a.py"
    );
}

// ---------------------------------------------------------------------------
// related_scripts
// ---------------------------------------------------------------------------

#[test]
fn related_scripts_via_dependencies() {
    let (api, _f) = make_api();
    insert(&api, "/catalog/scripts/a.py", "", "python", "", "");
    insert(&api, "/catalog/scripts/b.py", "", "python", "", "");

    insert_dep(&api, "/catalog/scripts/a.py", "/catalog/scripts/b.py");

    let rel = api.related_scripts("/catalog/scripts/a.py").unwrap();
    assert_eq!(rel.len(), 1);
    assert_eq!(
        rel[0]["logical_path"].as_str().unwrap(),
        "/catalog/scripts/b.py"
    );
}

#[test]
fn related_scripts_via_reverse_dependencies() {
    let (api, _f) = make_api();
    insert(&api, "/catalog/scripts/a.py", "", "python", "", "");
    insert(&api, "/catalog/scripts/b.py", "", "python", "", "");

    // /catalog/scripts/b.py depends on /catalog/scripts/a.py  → a.py is used_by b.py
    insert_dep(&api, "/catalog/scripts/b.py", "/catalog/scripts/a.py");

    let rel = api.related_scripts("/catalog/scripts/a.py").unwrap();
    assert_eq!(rel.len(), 1);
    assert_eq!(
        rel[0]["logical_path"].as_str().unwrap(),
        "/catalog/scripts/b.py"
    );
}

#[test]
fn related_scripts_empty_for_isolated_script() {
    let (api, _f) = make_api();
    insert(&api, "/catalog/scripts/alone.py", "", "python", "", "");
    let rel = api.related_scripts("/catalog/scripts/alone.py").unwrap();
    assert!(rel.is_empty());
}

#[test]
fn related_scripts_not_found() {
    let (api, _f) = make_api();
    let rel = api.related_scripts("/catalog/scripts/ghost.py").unwrap();
    assert!(rel.is_empty());
}

// ---------------------------------------------------------------------------
// dependency_graph
// ---------------------------------------------------------------------------

#[test]
fn dependency_graph_uses_and_used_by() {
    let (api, _f) = make_api();
    insert(
        &api,
        "/catalog/scripts/a.py",
        "",
        "python",
        "alice",
        "script a",
    );
    insert(
        &api,
        "/catalog/scripts/b.py",
        "",
        "python",
        "bob",
        "script b",
    );

    // a.py depends on b.py
    insert_dep(&api, "/catalog/scripts/a.py", "/catalog/scripts/b.py");

    // Graph for a.py: uses=[b.py], used_by=[]
    let g = api.dependency_graph("/catalog/scripts/a.py").unwrap();
    assert_eq!(g.uses.len(), 1);
    assert_eq!(g.uses[0].depends_on_path, "/catalog/scripts/b.py");
    assert!(g.uses[0].indexed, "b.py is in the catalog → indexed=true");
    assert!(g.used_by.is_empty());

    // Graph for b.py: uses=[], used_by=[a.py]
    let g2 = api.dependency_graph("/catalog/scripts/b.py").unwrap();
    assert!(g2.uses.is_empty());
    assert_eq!(g2.used_by.len(), 1);
    assert_eq!(
        g2.used_by[0]["logical_path"].as_str().unwrap(),
        "/catalog/scripts/a.py"
    );
}

#[test]
fn dependency_graph_not_found_returns_empty() {
    let (api, _f) = make_api();
    let g = api.dependency_graph("/catalog/scripts/ghost.py").unwrap();
    assert!(g.uses.is_empty());
    assert!(g.used_by.is_empty());
}

#[test]
fn dependency_graph_unindexed_dependency() {
    let (api, _f) = make_api();
    insert(&api, "/catalog/scripts/a.py", "", "python", "", "");

    // Depend on a path that is NOT in the scripts table — resolved_script_id stays NULL
    insert_dep(&api, "/catalog/scripts/a.py", "/external/lib.py");

    let g = api.dependency_graph("/catalog/scripts/a.py").unwrap();
    assert_eq!(g.uses.len(), 1);
    assert_eq!(g.uses[0].depends_on_path, "/external/lib.py");
    assert!(
        !g.uses[0].indexed,
        "external dep not in catalog → indexed=false"
    );
}

// ---------------------------------------------------------------------------
// dependency_tree
// ---------------------------------------------------------------------------

#[test]
fn dependency_tree_walks_transitive_uses() {
    let (api, _f) = make_api();
    for path in ["a.py", "b.py", "c.py"] {
        insert(
            &api,
            &format!("/catalog/scripts/{path}"),
            "",
            "python",
            "",
            "",
        );
    }
    insert_dep(&api, "/catalog/scripts/a.py", "/catalog/scripts/b.py");
    insert_dep(&api, "/catalog/scripts/b.py", "/catalog/scripts/c.py");
    insert_dep(&api, "/catalog/scripts/a.py", "/external/lib.py");

    let tree = api
        .dependency_tree("/catalog/scripts/a.py", TreeDirection::Uses, 5)
        .unwrap()
        .expect("a.py is indexed");

    assert_eq!(tree.logical_path, "/catalog/scripts/a.py");
    assert_eq!(tree.children.len(), 2);
    let b = &tree.children[0];
    assert_eq!(b.logical_path, "/catalog/scripts/b.py");
    assert_eq!(b.children.len(), 1);
    assert_eq!(b.children[0].logical_path, "/catalog/scripts/c.py");
    assert!(b.children[0].children.is_empty());
    let external = &tree.children[1];
    assert_eq!(external.logical_path, "/external/lib.py");
    assert!(!external.indexed);
    assert!(external.children.is_empty());
}

#[test]
fn dependency_tree_used_by_direction() {
    let (api, _f) = make_api();
    for path in ["a.py", "b.py", "c.py"] {
        insert(
            &api,
            &format!("/catalog/scripts/{path}"),
            "",
            "python",
            "",
            "",
        );
    }
    insert_dep(&api, "/catalog/scripts/a.py", "/catalog/scripts/b.py");
    insert_dep(&api, "/catalog/scripts/b.py", "/catalog/scripts/c.py");

    let tree = api
        .dependency_tree("/catalog/scripts/c.py", TreeDirection::UsedBy, 5)
        .unwrap()
        .expect("c.py is indexed");

    assert_eq!(tree.children.len(), 1);
    assert_eq!(tree.children[0].logical_path, "/catalog/scripts/b.py");
    assert_eq!(tree.children[0].children.len(), 1);
    assert_eq!(
        tree.children[0].children[0].logical_path,
        "/catalog/scripts/a.py"
    );
}

#[test]
fn dependency_tree_marks_cycles() {
    let (api, _f) = make_api();
    insert(&api, "/catalog/scripts/a.py", "", "python", "", "");
    insert(&api, "/catalog/scripts/b.py", "", "python", "", "");
    insert_dep(&api, "/catalog/scripts/a.py", "/catalog/scripts/b.py");
    insert_dep(&api, "/catalog/scripts/b.py", "/catalog/scripts/a.py");

    let tree = api
        .dependency_tree("/catalog/scripts/a.py", TreeDirection::Uses, 10)
        .unwrap()
        .unwrap();

    let b = &tree.children[0];
    let a_again = &b.children[0];
    assert_eq!(a_again.logical_path, "/catalog/scripts/a.py");
    assert!(
        a_again.cycle,
        "back-edge to an ancestor is marked as a cycle"
    );
    assert!(a_again.children.is_empty());
}

#[test]
fn dependency_tree_marks_repeated_subtrees() {
    let (api, _f) = make_api();
    // Diamond: a → b, a → c, b → shared, c → shared, shared → leaf.
    for path in ["a.py", "b.py", "c.py", "shared.py", "leaf.py"] {
        insert(
            &api,
            &format!("/catalog/scripts/{path}"),
            "",
            "python",
            "",
            "",
        );
    }
    insert_dep(&api, "/catalog/scripts/a.py", "/catalog/scripts/b.py");
    insert_dep(&api, "/catalog/scripts/a.py", "/catalog/scripts/c.py");
    insert_dep(&api, "/catalog/scripts/b.py", "/catalog/scripts/shared.py");
    insert_dep(&api, "/catalog/scripts/c.py", "/catalog/scripts/shared.py");
    insert_dep(
        &api,
        "/catalog/scripts/shared.py",
        "/catalog/scripts/leaf.py",
    );

    let tree = api
        .dependency_tree("/catalog/scripts/a.py", TreeDirection::Uses, 10)
        .unwrap()
        .unwrap();

    let first_shared = &tree.children[0].children[0];
    assert!(!first_shared.repeated);
    assert_eq!(first_shared.children.len(), 1, "first visit is expanded");
    let second_shared = &tree.children[1].children[0];
    assert!(second_shared.repeated, "second visit is collapsed");
    assert!(second_shared.children.is_empty());
}

#[test]
fn dependency_tree_truncates_at_depth() {
    let (api, _f) = make_api();
    for path in ["a.py", "b.py", "c.py"] {
        insert(
            &api,
            &format!("/catalog/scripts/{path}"),
            "",
            "python",
            "",
            "",
        );
    }
    insert_dep(&api, "/catalog/scripts/a.py", "/catalog/scripts/b.py");
    insert_dep(&api, "/catalog/scripts/b.py", "/catalog/scripts/c.py");

    let tree = api
        .dependency_tree("/catalog/scripts/a.py", TreeDirection::Uses, 1)
        .unwrap()
        .unwrap();

    let b = &tree.children[0];
    assert!(b.truncated, "b.py has hidden children beyond the limit");
    assert!(b.children.is_empty());

    // A leaf at the depth limit is NOT marked truncated.
    let tree = api
        .dependency_tree("/catalog/scripts/a.py", TreeDirection::Uses, 2)
        .unwrap()
        .unwrap();
    let c = &tree.children[0].children[0];
    assert!(!c.truncated, "c.py has no children to hide");
}

#[test]
fn dependency_graph_and_tree_carry_edge_kind() {
    let (api, _f) = make_api();
    insert(&api, "/catalog/scripts/a.py", "", "python", "", "");
    insert(&api, "/catalog/scripts/b.py", "", "python", "", "");
    insert(&api, "/catalog/scripts/run.sh", "", "shell", "", "");

    // a.py imports b.py and references run.sh by path.
    insert_dep(&api, "/catalog/scripts/a.py", "/catalog/scripts/b.py");
    insert_reference_dep(&api, "/catalog/scripts/a.py", "/catalog/scripts/run.sh");

    let graph = api.dependency_graph("/catalog/scripts/a.py").unwrap();
    let by_path = |p: &str| {
        graph
            .uses
            .iter()
            .find(|u| u.logical_path == p)
            .unwrap_or_else(|| panic!("missing {p}"))
    };
    assert_eq!(by_path("/catalog/scripts/b.py").kind, "import");
    assert_eq!(by_path("/catalog/scripts/run.sh").kind, "referenced");

    // The tree tags each child with the edge kind used to reach it.
    let tree = api
        .dependency_tree("/catalog/scripts/a.py", TreeDirection::Uses, 5)
        .unwrap()
        .unwrap();
    assert_eq!(tree.via_kind, None, "root has no incoming edge");
    let via = |p: &str| {
        tree.children
            .iter()
            .find(|c| c.logical_path == p)
            .and_then(|c| c.via_kind.as_deref())
    };
    assert_eq!(via("/catalog/scripts/b.py"), Some("import"));
    assert_eq!(via("/catalog/scripts/run.sh"), Some("referenced"));

    // The reverse direction carries the kind too.
    let used_by = api
        .dependency_tree("/catalog/scripts/run.sh", TreeDirection::UsedBy, 5)
        .unwrap()
        .unwrap();
    assert_eq!(
        used_by.children.first().and_then(|c| c.via_kind.as_deref()),
        Some("referenced")
    );
}

#[test]
fn dependency_tree_missing_script_returns_none() {
    let (api, _f) = make_api();
    assert!(
        api.dependency_tree("/catalog/scripts/ghost.py", TreeDirection::Uses, 5)
            .unwrap()
            .is_none()
    );
}

#[test]
fn function_graph_query_helpers() {
    let (api, _f) = make_api();
    insert(&api, "/catalog/scripts/main.py", "", "python", "alice", "");
    insert(&api, "/catalog/scripts/helper.py", "", "python", "bob", "");

    let main_id = id_for(&api, "/catalog/scripts/main.py");
    let helper_id = id_for(&api, "/catalog/scripts/helper.py");

    api.conn
        .execute(
            "INSERT INTO function_definitions
             (script_id, name, kind, line, docstring, decorators)
             VALUES (?1, 'run', 'function', 1, 'doc', '[]')",
            rusqlite::params![helper_id],
        )
        .unwrap();
    api.conn
        .execute(
            "INSERT INTO function_calls
             (script_id, caller, callee, line, resolved_target_name, resolved_target_script_id)
             VALUES (?1, 'entry', 'helper.run', 3, 'helper.run', ?2)",
            rusqlite::params![main_id, helper_id],
        )
        .unwrap();

    let defs = api
        .get_functions_defined_in("/catalog/scripts/helper.py")
        .unwrap();
    let all_callers = api
        .get_callers_of_functions_in("/catalog/scripts/helper.py")
        .unwrap();
    let deps = api
        .get_function_dependencies("/catalog/scripts/main.py")
        .unwrap();

    assert!(!defs.is_empty());
    assert!(!all_callers.is_empty());
    assert!(!deps.is_empty());
    assert_eq!(defs[0]["name"].as_str().unwrap(), "run");
    assert_eq!(all_callers[0]["target_function"].as_str().unwrap(), "run");
    assert_eq!(
        all_callers[0]["logical_path"].as_str().unwrap(),
        "/catalog/scripts/main.py"
    );
    assert_eq!(
        deps[0]["target_logical_path"].as_str().unwrap(),
        "/catalog/scripts/helper.py"
    );
}

// ---------------------------------------------------------------------------
// checkout_status
// ---------------------------------------------------------------------------

#[test]
fn checkout_status_returns_scripts_with_checkout() {
    let (api, _f) = make_api();
    insert(&api, "/catalog/scripts/checked.py", "", "python", "", "");
    insert_revision(
        &api,
        "/catalog/scripts/checked.py",
        "/dev/LINUX/checked.py",
        REVISION_TYPE_DEVELOP,
        None,
    );

    let results = api.checkout_status(None).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0]["logical_path"].as_str().unwrap(),
        "/catalog/scripts/checked.py"
    );
    assert_eq!(results[0]["checkout_user"].as_str().unwrap(), "jdoe");
}

#[test]
fn checkout_status_specific_path() {
    let (api, _f) = make_api();
    insert(
        &api,
        "/catalog/scripts/foo.py",
        "",
        "python",
        "alice",
        "does things",
    );

    let results = api
        .checkout_status(Some("/catalog/scripts/foo.py"))
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0]["owner"].as_str().unwrap(), "alice");
}

#[test]
fn checkout_status_orphan_checkout() {
    let (api, _f) = make_api();
    insert_revision(
        &api,
        "/catalog/scripts/orphan.py",
        "/dev/LINUX/orphan.py",
        REVISION_TYPE_DEVELOP,
        None,
    );

    let results = api.checkout_status(None).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0]["logical_path"].as_str().unwrap(),
        "/catalog/scripts/orphan.py"
    );
    // Should have vc_warnings indicating checkout_without_catalog_entry
    let warnings: serde_json::Value =
        serde_json::from_str(results[0]["vc_warnings"].as_str().unwrap()).unwrap();
    assert!(
        warnings
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w["kind"] == "checkout_without_catalog_entry")
    );
}

// ---------------------------------------------------------------------------
// index_metadata
// ---------------------------------------------------------------------------

#[test]
fn index_metadata_returns_schema_version() {
    let (api, _f) = make_api();
    let meta = api.index_metadata().unwrap();
    assert_eq!(meta.current_schema_version, SCHEMA_VERSION);
    assert_eq!(meta.schema_version.as_i64().unwrap(), SCHEMA_VERSION);
    assert!(meta.build_timestamp.is_string());
}

#[test]
fn index_metadata_null_when_no_row() {
    let f = NamedTempFile::new().unwrap();
    let conn = create_db(f.path()).unwrap();
    // No index_metadata row inserted.
    let api = SearchApi::new(conn);
    let meta = api.index_metadata().unwrap();
    assert!(meta.build_timestamp.is_null());
    assert!(meta.schema_version.is_null());
}

// ---------------------------------------------------------------------------
// audit
// ---------------------------------------------------------------------------

#[test]
fn audit_unowned_reports_missing_tech_and_func_owner() {
    let (api, _f) = make_api();
    insert(
        &api,
        "/catalog/scripts/unowned.py",
        "",
        "python",
        "alice",
        "ok",
    );
    insert(&api, "/catalog/scripts/owned.py", "", "python", "bob", "ok");
    api.conn
        .execute(
            "UPDATE scripts SET metadata_json = '{\"techowner\":\"team-a\"}' WHERE logical_path='/catalog/scripts/owned.py'",
            [],
        )
        .unwrap();

    let checks = vec!["unowned".to_string()];
    let result = api.audit(Some(&checks), 90).unwrap();
    assert_eq!(result.findings.len(), 1);
    assert_eq!(result.findings[0].check, "unowned");
    assert_eq!(result.findings[0].severity, "warn");
    assert_eq!(
        result.findings[0].logical_path,
        "/catalog/scripts/unowned.py"
    );
}

#[test]
fn audit_no_purpose_reports_empty_purpose() {
    let (api, _f) = make_api();
    insert(&api, "/catalog/scripts/empty.py", "", "python", "alice", "");
    insert(
        &api,
        "/catalog/scripts/ok.py",
        "",
        "python",
        "alice",
        "has purpose",
    );

    let checks = vec!["no-purpose".to_string()];
    let result = api.audit(Some(&checks), 90).unwrap();
    assert_eq!(result.findings.len(), 1);
    assert_eq!(result.findings[0].logical_path, "/catalog/scripts/empty.py");
}

#[test]
fn audit_broken_deps_reports_missing_targets() {
    let (api, _f) = make_api();
    insert(
        &api,
        "/catalog/scripts/report.py",
        "",
        "python",
        "alice",
        "report",
    );
    insert_dep(
        &api,
        "/catalog/scripts/report.py",
        "/catalog/scripts/missing.py",
    );

    let checks = vec!["broken-deps".to_string()];
    let result = api.audit(Some(&checks), 90).unwrap();
    assert_eq!(result.findings.len(), 1);
    assert_eq!(result.findings[0].severity, "error");
    assert_eq!(
        result.findings[0].logical_path,
        "/catalog/scripts/report.py"
    );
    assert!(
        result.findings[0]
            .detail
            .contains("/catalog/scripts/missing.py")
    );
}

#[test]
fn audit_orphan_checkouts_reports_unindexed_checkout_paths() {
    let (api, _f) = make_api();
    insert_revision(
        &api,
        "/catalog/scripts/orphan.py",
        "/dev/LINUX/orphan.py",
        REVISION_TYPE_DEVELOP,
        None,
    );

    let checks = vec!["orphan-checkouts".to_string()];
    let result = api.audit(Some(&checks), 90).unwrap();
    assert_eq!(result.findings.len(), 1);
    assert_eq!(
        result.findings[0].logical_path,
        "/catalog/scripts/orphan.py"
    );
}

#[test]
fn audit_stale_checkouts_respects_threshold() {
    const DAY_SECONDS: i64 = 86_400;
    let (api, _f) = make_api();
    insert(
        &api,
        "/catalog/scripts/stale.py",
        "",
        "python",
        "alice",
        "stale",
    );
    insert(
        &api,
        "/catalog/scripts/fresh.py",
        "",
        "python",
        "alice",
        "fresh",
    );
    insert_revision(
        &api,
        "/catalog/scripts/stale.py",
        "/dev/LINUX/stale.py",
        REVISION_TYPE_DEVELOP,
        Some(100 * DAY_SECONDS),
    );
    insert_revision(
        &api,
        "/catalog/scripts/fresh.py",
        "/dev/LINUX/fresh.py",
        REVISION_TYPE_DEVELOP,
        Some(DAY_SECONDS),
    );

    let checks = vec!["stale-checkouts".to_string()];
    let result = api.audit(Some(&checks), 90).unwrap();
    assert_eq!(result.findings.len(), 1);
    assert_eq!(result.findings[0].logical_path, "/catalog/scripts/stale.py");
}

#[test]
fn audit_dead_scripts_reports_isolated_scripts() {
    let (api, _f) = make_api();
    insert(
        &api,
        "/catalog/scripts/dead.py",
        "",
        "python",
        "alice",
        "dead",
    );
    insert(
        &api,
        "/catalog/scripts/live.py",
        "",
        "python",
        "alice",
        "live",
    );
    insert(
        &api,
        "/catalog/scripts/uses_live.py",
        "",
        "python",
        "alice",
        "uses",
    );
    insert_dep(
        &api,
        "/catalog/scripts/uses_live.py",
        "/catalog/scripts/live.py",
    );

    let checks = vec!["dead-scripts".to_string()];
    let result = api.audit(Some(&checks), 90).unwrap();
    assert_eq!(result.findings.len(), 2);
    assert!(
        result
            .findings
            .iter()
            .any(|f| f.logical_path == "/catalog/scripts/dead.py")
    );
    assert!(
        result
            .findings
            .iter()
            .any(|f| f.logical_path == "/catalog/scripts/uses_live.py")
    );
    assert!(
        !result
            .findings
            .iter()
            .any(|f| f.logical_path == "/catalog/scripts/live.py")
    );
}

#[test]
fn audit_no_description_requires_brief_or_docstring_metadata() {
    let (api, _f) = make_api();
    insert(
        &api,
        "/catalog/scripts/none.py",
        "",
        "python",
        "alice",
        "n/a",
    );
    insert(
        &api,
        "/catalog/scripts/brief.py",
        "",
        "python",
        "alice",
        "n/a",
    );
    insert(
        &api,
        "/catalog/scripts/doc.py",
        "",
        "python",
        "alice",
        "n/a",
    );
    api.conn
        .execute(
            "UPDATE scripts SET metadata_json = '{\"brief\":\"does work\"}' WHERE logical_path='/catalog/scripts/brief.py'",
            [],
        )
        .unwrap();
    api.conn
        .execute(
            "UPDATE scripts SET metadata_json = '{\"docstring\":\"does work\"}' WHERE logical_path='/catalog/scripts/doc.py'",
            [],
        )
        .unwrap();

    let checks = vec!["no-description".to_string()];
    let result = api.audit(Some(&checks), 90).unwrap();
    assert_eq!(result.findings.len(), 1);
    assert_eq!(result.findings[0].logical_path, "/catalog/scripts/none.py");
}

#[test]
fn audit_check_filter_runs_only_selected_checks() {
    let (api, _f) = make_api();
    insert(&api, "/catalog/scripts/a.py", "", "python", "alice", "");
    insert(&api, "/catalog/scripts/b.py", "", "python", "alice", "ok");
    let checks = vec!["no-purpose".to_string()];
    let result = api.audit(Some(&checks), 90).unwrap();
    assert!(result.findings.iter().all(|f| f.check == "no-purpose"));
}

#[test]
fn audit_summary_counts_severities() {
    let (api, _f) = make_api();
    insert(&api, "/catalog/scripts/a.py", "", "python", "", "");
    insert_dep(&api, "/catalog/scripts/a.py", "/catalog/scripts/missing.py");

    let result = api.audit(None, 90).unwrap();
    assert!(result.summary.error >= 1);
    assert!(result.summary.warn >= 1);
    assert!(result.summary.info >= 1);
}

#[test]
fn audit_rejects_unknown_check_name() {
    let (api, _f) = make_api();
    let checks = vec!["not-a-check".to_string()];
    let err = api.audit(Some(&checks), 90).unwrap_err();
    assert!(err.to_string().contains("unknown audit check"));
}

// ---------------------------------------------------------------------------
// search_by_regex
// ---------------------------------------------------------------------------

#[test]
fn regex_matches_logical_path() {
    let (api, _f) = make_api();
    insert(
        &api,
        "/catalog/scripts/check_mc.py",
        "",
        "python",
        "alice",
        "",
    );
    insert(&api, "/catalog/scripts/deploy.sh", "", "shell", "bob", "");

    let results = api
        .search_by_regex_with_filters(r"check.*\.py", 10, None, None, None)
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0]["logical_path"].as_str().unwrap(),
        "/catalog/scripts/check_mc.py"
    );
}

#[test]
fn regex_matches_purpose() {
    let (api, _f) = make_api();
    insert(
        &api,
        "/catalog/scripts/a.py",
        "",
        "python",
        "alice",
        "backup rotation script",
    );
    insert(
        &api,
        "/catalog/scripts/b.py",
        "",
        "python",
        "bob",
        "deployment helper",
    );

    let results = api
        .search_by_regex_with_filters(r"backup", 10, None, None, None)
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0]["logical_path"].as_str().unwrap(),
        "/catalog/scripts/a.py"
    );
}

#[test]
fn regex_no_match_returns_empty() {
    let (api, _f) = make_api();
    insert(&api, "/catalog/scripts/deploy.sh", "", "shell", "", "");

    let results = api
        .search_by_regex_with_filters(r"check.*\.py", 10, None, None, None)
        .unwrap();
    assert!(results.is_empty());
}

#[test]
fn regex_language_filter_applied() {
    let (api, _f) = make_api();
    insert(&api, "/catalog/scripts/check.py", "", "python", "", "");
    insert(&api, "/catalog/scripts/check.sh", "", "shell", "", "");

    let results = api
        .search_by_regex_with_filters(r"check", 10, Some("python"), None, None)
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0]["logical_path"].as_str().unwrap(),
        "/catalog/scripts/check.py"
    );
}

#[test]
fn regex_limit_respected() {
    let (api, _f) = make_api();
    insert(&api, "/catalog/scripts/a_check.py", "", "python", "", "");
    insert(&api, "/catalog/scripts/b_check.py", "", "python", "", "");
    insert(&api, "/catalog/scripts/c_check.py", "", "python", "", "");

    let results = api
        .search_by_regex_with_filters(r"check", 2, None, None, None)
        .unwrap();
    assert_eq!(results.len(), 2);
}

#[test]
fn regex_owner_filter_applies_before_limit() {
    let (api, _f) = make_api();
    insert(&api, "/catalog/scripts/a_check.py", "", "python", "bob", "");
    insert(&api, "/catalog/scripts/b_check.py", "", "python", "bob", "");
    insert(
        &api,
        "/catalog/scripts/c_check.py",
        "",
        "python",
        "alice",
        "",
    );
    insert(
        &api,
        "/catalog/scripts/d_check.py",
        "",
        "python",
        "alice",
        "",
    );

    let results = api
        .search_by_regex_with_filters(r"check", 2, None, Some("alice"), None)
        .unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(
        results
            .iter()
            .map(|r| r["logical_path"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["/catalog/scripts/c_check.py", "/catalog/scripts/d_check.py"]
    );
}

#[test]
fn regex_anchor_start_of_path() {
    let (api, _f) = make_api();
    insert(&api, "/catalog/scripts/backup.py", "", "python", "", "");
    insert(&api, "/other/backup.py", "", "python", "", "");

    let results = api
        .search_by_regex_with_filters(r"^/catalog/scripts/.*backup", 10, None, None, None)
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0]["logical_path"].as_str().unwrap(),
        "/catalog/scripts/backup.py"
    );
}

#[test]
fn regex_invalid_pattern_returns_error() {
    let (api, _f) = make_api();
    let err = api
        .search_by_regex_with_filters(r"[invalid", 10, None, None, None)
        .unwrap_err();
    assert!(err.to_string().contains("invalid regex pattern"));
}

// ---------------------------------------------------------------------------
// catalog diff
// ---------------------------------------------------------------------------

#[test]
fn compare_catalogs_reports_added_removed_changed_and_dependency_deltas() {
    let (old_api, old_file) = make_api();
    let (new_api, new_file) = make_api();

    insert_diff_script(
        &old_api,
        "/catalog/scripts/old_report.py",
        "python",
        "legacy",
        "old report",
        "{}",
    );
    insert_diff_script(
        &old_api,
        "/catalog/scripts/etl/load.py",
        "python",
        "etl",
        "load data",
        r#"{"techowner":"Alice"}"#,
    );
    insert_diff_script(
        &old_api,
        "/catalog/scripts/etl/transform.py",
        "python",
        "etl",
        "transform",
        "{}",
    );
    insert_diff_script(
        &new_api,
        "/catalog/scripts/new_tool.py",
        "python",
        "tools",
        "new tool",
        "{}",
    );
    insert_diff_script(
        &new_api,
        "/catalog/scripts/etl/load.py",
        "python",
        "etl",
        "load data",
        r#"{"techowner":"Bob"}"#,
    );
    insert_diff_script(
        &new_api,
        "/catalog/scripts/etl/transform.py",
        "python",
        "etl",
        "transform",
        "{}",
    );

    insert_dep(
        &old_api,
        "/catalog/scripts/etl/transform.py",
        "/catalog/scripts/legacy/parse.py",
    );
    insert_dep(
        &new_api,
        "/catalog/scripts/etl/transform.py",
        "/catalog/scripts/util/helper.py",
    );

    let diff = compare_catalogs(old_file.path(), new_file.path()).unwrap();

    assert_eq!(diff.added.len(), 1);
    assert_eq!(diff.added[0].logical_path, "/catalog/scripts/new_tool.py");
    assert_eq!(diff.removed.len(), 1);
    assert_eq!(
        diff.removed[0].logical_path,
        "/catalog/scripts/old_report.py"
    );

    let load = diff
        .changed
        .iter()
        .find(|row| row.logical_path == "/catalog/scripts/etl/load.py")
        .unwrap();
    let techowner = load.fields.get("techowner").unwrap();
    assert_eq!(techowner[0], serde_json::json!("Alice"));
    assert_eq!(techowner[1], serde_json::json!("Bob"));

    let transform = diff
        .changed
        .iter()
        .find(|row| row.logical_path == "/catalog/scripts/etl/transform.py")
        .unwrap();
    assert_eq!(
        transform.deps_added,
        vec!["/catalog/scripts/util/helper.py"]
    );
    assert_eq!(
        transform.deps_removed,
        vec!["/catalog/scripts/legacy/parse.py"]
    );
}

// ---------------------------------------------------------------------------
// search_scripts_by_function
// ---------------------------------------------------------------------------

#[test]
fn search_scripts_by_function_substring_match() {
    let (api, _f) = make_api();
    insert(
        &api,
        "/catalog/scripts/helper.py",
        "",
        "python",
        "alice",
        "",
    );
    insert(&api, "/catalog/scripts/main.py", "", "python", "bob", "");

    let helper_id = id_for(&api, "/catalog/scripts/helper.py");
    api.conn
        .execute(
            "INSERT INTO function_definitions
             (script_id, name, kind, line, docstring, decorators)
             VALUES (?1, 'deploy_service', 'function', 10, 'Deploys the service', '[]')",
            rusqlite::params![helper_id],
        )
        .unwrap();
    api.conn
        .execute(
            "INSERT INTO function_definitions
             (script_id, name, kind, line, docstring, decorators)
             VALUES (?1, 'rollback', 'function', 20, 'Rolls back', '[]')",
            rusqlite::params![helper_id],
        )
        .unwrap();

    let results = api
        .search_scripts_by_function_with_filters("deploy", 10, None, None, None)
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0]["logical_path"].as_str().unwrap(),
        "/catalog/scripts/helper.py"
    );

    let results = api
        .search_scripts_by_function_with_filters("roll", 10, None, None, None)
        .unwrap();
    assert_eq!(results.len(), 1);

    let results = api
        .search_scripts_by_function_with_filters("nonexistent_fn", 10, None, None, None)
        .unwrap();
    assert!(results.is_empty());
}

#[test]
fn search_scripts_by_function_case_insensitive() {
    let (api, _f) = make_api();
    insert(&api, "/catalog/scripts/a.py", "", "python", "alice", "");
    let script_id = id_for(&api, "/catalog/scripts/a.py");
    api.conn
        .execute(
            "INSERT INTO function_definitions
             (script_id, name, kind, line, docstring, decorators)
             VALUES (?1, 'MyHelper', 'function', 1, '', '[]')",
            rusqlite::params![script_id],
        )
        .unwrap();

    let results = api
        .search_scripts_by_function_with_filters("myhelper", 10, None, None, None)
        .unwrap();
    assert_eq!(results.len(), 1);
    let results = api
        .search_scripts_by_function_with_filters("MYHELPER", 10, None, None, None)
        .unwrap();
    assert_eq!(results.len(), 1);
}

// ---------------------------------------------------------------------------
// list_scripts – tag filter
// ---------------------------------------------------------------------------

#[test]
fn list_scripts_tag_filter() {
    let (api, _f) = make_api();
    api.conn
        .execute(
            "INSERT INTO scripts (logical_path, language, owner, tags) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                "/catalog/scripts/a.py",
                "python",
                "alice",
                r#"["deploy","backup"]"#
            ],
        )
        .unwrap();
    api.conn
        .execute(
            "INSERT INTO scripts (logical_path, language, owner, tags) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params!["/catalog/scripts/b.py", "python", "bob", r#"["backup"]"#],
        )
        .unwrap();

    let results = api.list_scripts(None, None, Some("deploy"), 10, 0).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0]["logical_path"].as_str().unwrap(),
        "/catalog/scripts/a.py"
    );

    let results = api.list_scripts(None, None, Some("dep"), 10, 0).unwrap();
    assert!(
        results.is_empty(),
        "tag filtering should use exact JSON membership"
    );

    let results = api.list_scripts(None, None, Some("backup"), 10, 0).unwrap();
    assert_eq!(results.len(), 2);

    let results = api
        .list_scripts(None, None, Some("nonexistent"), 10, 0)
        .unwrap();
    assert!(results.is_empty());
}

#[test]
fn list_scripts_owner_and_tag_filter() {
    let (api, _f) = make_api();
    api.conn
        .execute(
            "INSERT INTO scripts (logical_path, language, owner, tags) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params!["/catalog/scripts/a.py", "python", "alice", r#"["deploy"]"#],
        )
        .unwrap();
    api.conn
        .execute(
            "INSERT INTO scripts (logical_path, language, owner, tags) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params!["/catalog/scripts/b.py", "python", "bob", r#"["deploy"]"#],
        )
        .unwrap();

    let results = api
        .list_scripts(None, Some("alice"), Some("deploy"), 10, 0)
        .unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(
        results[0]["logical_path"].as_str().unwrap(),
        "/catalog/scripts/a.py"
    );
}
