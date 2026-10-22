//! Compatibility contract between a published catalog (plus its embeddings
//! sidecar) and the scat clients that read it. The policy and its rationale
//! are in `docs/COMPATIBILITY.md`; these tests are what keep it true.
//!
//! Fixtures are catalogs stamped with the schema version under test. The
//! open check reads only the stamp, so a stamped current-layout catalog
//! exercises the real read paths exactly as a catalog published by another
//! scat release would.

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use rusqlite::Connection;
use scat_core::core::cache::{CacheOptions, catalog_read_path};
use scat_core::core::db::{
    SCHEMA_VERSION, SchemaCompatibility, create_db, open_db, schema_compatibility,
};
use scat_core::core::embeddings::{
    EmbeddingsSidecar, SIDECAR_FILE_NAME, SIDECAR_FORMAT_VERSION, SidecarError,
};
use scat_core::error::Error;
use tempfile::TempDir;

const OLD_BUILD: &str = "2026-06-01T02:00:00Z";
const NEW_BUILD: &str = "2026-06-02T02:00:00Z";

/// Publish a catalog stamped `schema` at `path`, the way the builder leaves
/// one: complete, closed, in rollback-journal mode (no `-wal`/`-shm`).
fn publish_catalog(path: &Path, schema: i64, build: &str, scripts: &[&str]) {
    let conn = create_db(path).unwrap();
    for script in scripts {
        conn.execute(
            "INSERT INTO scripts (logical_path, language, content) VALUES (?1, 'python', 'print(1)')",
            [script],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO index_metadata (id, build_timestamp, schema_version) VALUES (1, ?1, ?2)",
        rusqlite::params![build, schema],
    )
    .unwrap();
    let _: String = conn
        .query_row("PRAGMA journal_mode = DELETE", [], |r| r.get(0))
        .unwrap();
    conn.close().unwrap();
}

/// Publish a sidecar. `format`/`build` of `None` omit `embedding_metadata`
/// entirely (a sidecar from before versioning existed).
fn publish_sidecar(
    path: &Path,
    format: Option<i64>,
    build: Option<&str>,
    rows: &[(&str, [f32; 2])],
) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE script_embeddings (
            script_id INTEGER PRIMARY KEY, logical_path TEXT NOT NULL,
            model TEXT NOT NULL, dim INTEGER NOT NULL, embedding BLOB NOT NULL);",
    )
    .unwrap();
    if let Some(format) = format {
        conn.execute_batch(
            "CREATE TABLE embedding_metadata (
                id INTEGER PRIMARY KEY CHECK (id = 1), format_version INTEGER NOT NULL,
                catalog_build_timestamp TEXT);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO embedding_metadata VALUES (1, ?1, ?2)",
            rusqlite::params![format, build],
        )
        .unwrap();
    }
    for (id, (logical_path, v)) in (1_i64..).zip(rows) {
        let bytes: Vec<u8> = v.iter().flat_map(|f| f.to_le_bytes()).collect();
        conn.execute(
            "INSERT INTO script_embeddings VALUES (?1, ?2, 'test-model', 2, ?3)",
            rusqlite::params![id, logical_path, bytes],
        )
        .unwrap();
    }
    conn.close().unwrap();
}

/// Every file under `dir` with its bytes: equal before and after means a
/// command neither modified nor created anything beside the catalog.
fn snapshot(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fs::read_dir(dir)
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            (
                e.file_name().to_string_lossy().into_owned(),
                fs::read(e.path()).unwrap(),
            )
        })
        .collect()
}

fn scat(db: &Path, cache: Option<&Path>, args: &[&str]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_scat"));
    cmd.env_remove("SCAT_DB")
        .env_remove("SCAT_EMBEDDINGS")
        .env_remove("SCAT_CONFIG")
        .env_remove("SCAT_CACHE_DIR")
        .env_remove("SCAT_NO_CACHE")
        .env("NO_COLOR", "1")
        .arg("--db")
        .arg(db);
    match cache {
        Some(root) => cmd.arg("--cache-dir").arg(root),
        None => cmd.arg("--no-cache"),
    };
    cmd.args(args).output().unwrap()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn cache_options(root: &TempDir) -> CacheOptions {
    CacheOptions {
        enabled: true,
        root: Some(root.path().to_path_buf()),
    }
}

/// Scripts visible through a catalog opened with the real schema-checked path.
fn script_paths(catalog: &Path) -> Vec<String> {
    let conn = open_db(catalog).unwrap();
    let mut stmt = conn
        .prepare("SELECT logical_path FROM scripts ORDER BY logical_path")
        .unwrap();
    stmt.query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

// ---------------------------------------------------------------------------
// Policy
// ---------------------------------------------------------------------------

#[test]
fn policy_is_an_exact_match() {
    assert_eq!(
        schema_compatibility(SCHEMA_VERSION),
        SchemaCompatibility::Compatible
    );
    for older in [SCHEMA_VERSION - 1, 1, 0, -1, i64::MIN] {
        assert_eq!(
            schema_compatibility(older),
            SchemaCompatibility::CatalogOlder,
            "{older}"
        );
    }
    for newer in [SCHEMA_VERSION + 1, SCHEMA_VERSION + 100, i64::MAX] {
        assert_eq!(
            schema_compatibility(newer),
            SchemaCompatibility::CatalogNewer,
            "{newer}"
        );
    }
}

#[test]
fn compatibility_doc_and_embed_crate_track_the_code() {
    // The release checklist's mechanical half: a schema or sidecar-format
    // bump that does not also update the documented table fails here.
    let doc = include_str!("../docs/COMPATIBILITY.md");
    assert!(
        doc.contains(&format!("| {SCHEMA_VERSION} | current |")),
        "docs/COMPATIBILITY.md must list catalog schema {SCHEMA_VERSION} as `current` in its schema table"
    );
    assert!(
        doc.contains(&format!("| {SIDECAR_FORMAT_VERSION} | current |")),
        "docs/COMPATIBILITY.md must list sidecar format {SIDECAR_FORMAT_VERSION} as `current` in its sidecar table"
    );

    // scat-embed cannot depend on scat_core (it builds on Windows), so its
    // copy of the sidecar format constant is pinned here instead.
    let embed = include_str!("../crates/scat-embed/src/main.rs");
    assert!(
        embed.contains(&format!(
            "const SIDECAR_FORMAT_VERSION: i64 = {SIDECAR_FORMAT_VERSION};"
        )),
        "crates/scat-embed must write the sidecar format scat_core reads"
    );
}

// ---------------------------------------------------------------------------
// Catalog: supported and unsupported combinations through the real CLI
// ---------------------------------------------------------------------------

#[test]
fn matching_catalog_opens_through_cli_read_paths_with_and_without_cache() {
    let dir = TempDir::new().unwrap();
    let cache = TempDir::new().unwrap();
    let db = dir.path().join("scripts.sqlite");
    publish_catalog(&db, SCHEMA_VERSION, OLD_BUILD, &["/catalog/scripts/a.py"]);
    let before = snapshot(dir.path());

    for cache in [None, Some(cache.path())] {
        for args in [
            &["catalog", "info"][..],
            &["catalog", "stats"],
            &["search", "a"],
            &["show", "/catalog/scripts/a.py"],
        ] {
            let out = scat(&db, cache, args);
            assert!(out.status.success(), "{args:?}: {}", stderr(&out));
        }
        let info = scat(&db, cache, &["catalog", "info", "--json"]);
        let json: serde_json::Value = serde_json::from_slice(&info.stdout).unwrap();
        assert_eq!(json["compatibility"], "compatible");
    }
    assert_eq!(
        snapshot(dir.path()),
        before,
        "reads must not touch the share"
    );
}

#[test]
fn unsupported_catalogs_fail_clearly_and_are_never_modified_or_cached() {
    for (schema, older) in [
        (SCHEMA_VERSION - 1, true),
        (SCHEMA_VERSION + 1, false),
        (SCHEMA_VERSION + 50, false),
    ] {
        let dir = TempDir::new().unwrap();
        let cache = TempDir::new().unwrap();
        let db = dir.path().join("scripts.sqlite");
        publish_catalog(&db, schema, OLD_BUILD, &["/catalog/scripts/a.py"]);
        let before = snapshot(dir.path());

        for cache in [None, Some(cache.path())] {
            for args in [
                &["catalog", "info"][..],
                &["catalog", "stats"],
                &["catalog", "audit"],
                &["search", "a"],
                &["show", "/catalog/scripts/a.py"],
                &["deps", "/catalog/scripts/a.py"],
            ] {
                let out = scat(&db, cache, args);
                let err = stderr(&out);
                assert!(
                    !out.status.success(),
                    "{args:?} must fail for schema {schema}"
                );
                assert!(
                    err.contains(&format!(
                        "catalog schema is version {schema}, expected {SCHEMA_VERSION}"
                    )),
                    "{args:?}: {err}"
                );
                if older {
                    // Only the publisher can fix an older catalog.
                    assert!(err.contains("the catalog publisher must upgrade"), "{err}");
                    assert!(!err.contains("upgrade scat on this host"), "{err}");
                } else {
                    // Only upgrading this client fixes a newer one — and a
                    // client must never be told to rebuild the share.
                    assert!(err.contains("upgrade scat on this host"), "{err}");
                    assert!(err.contains("do not rebuild the shared catalog"), "{err}");
                    assert!(!err.contains("publisher must"), "{err}");
                }
            }
        }
        assert_eq!(
            snapshot(dir.path()),
            before,
            "schema {schema}: share modified"
        );
        // An unreadable catalog must not be parked in tmpfs: no copy, no
        // metadata. (Empty lock files are the only residue.)
        let cached: Vec<_> = walk(cache.path())
            .into_iter()
            .filter(|p| p.extension().is_some_and(|e| e == "sqlite" || e == "json"))
            .collect();
        assert!(cached.is_empty(), "schema {schema}: cached {cached:?}");
    }
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out
}

#[test]
fn catalog_info_describes_an_unreadable_catalog_before_failing() {
    for (schema, verdict, text) in [
        (
            SCHEMA_VERSION - 1,
            "catalog_older",
            "catalog is older than this scat",
        ),
        (
            SCHEMA_VERSION + 1,
            "catalog_newer",
            "catalog is newer than this scat",
        ),
    ] {
        let dir = TempDir::new().unwrap();
        let db = dir.path().join("scripts.sqlite");
        publish_catalog(&db, schema, OLD_BUILD, &[]);

        let human = scat(&db, None, &["catalog", "info"]);
        assert!(!human.status.success());
        let out = stdout(&human);
        assert!(
            out.contains(&format!("Schema version (DB): {schema}")),
            "{out}"
        );
        assert!(
            out.contains(&format!("Schema version (app): {SCHEMA_VERSION}")),
            "{out}"
        );
        assert!(out.contains(text), "{out}");

        let json = scat(&db, None, &["catalog", "info", "--json"]);
        assert!(!json.status.success(), "scripts rely on the exit code");
        let v: serde_json::Value = serde_json::from_slice(&json.stdout).unwrap();
        assert_eq!(v["schema_version"], schema);
        assert_eq!(v["compatibility"], verdict);
    }
}

#[test]
fn doctor_reports_the_verdict_for_an_unreadable_catalog() {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("scripts.sqlite");
    publish_catalog(&db, SCHEMA_VERSION + 1, OLD_BUILD, &[]);

    let out = scat(&db, None, &["doctor", "--json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        v["catalog"]["catalog_metadata"]["compatibility"],
        "catalog_newer"
    );
}

#[test]
fn file_without_index_metadata_is_not_reported_as_a_schema_mismatch() {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("scripts.sqlite");
    create_db(&db).unwrap().close().unwrap();

    let out = scat(&db, None, &["catalog", "info"]);
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(
        err.contains("no catalog metadata") || err.contains("index_metadata"),
        "{err}"
    );
    assert!(!err.contains("expected"), "{err}");
}

// ---------------------------------------------------------------------------
// Cache across publisher upgrade and rollback
// ---------------------------------------------------------------------------

#[test]
fn cache_follows_publisher_upgrade_and_rollback_without_serving_stale_or_unreadable_copies() {
    let share = TempDir::new().unwrap();
    let root = TempDir::new().unwrap();
    let source = share.path().join("scripts.sqlite");
    let options = cache_options(&root);

    // Steady state: a readable build, cached.
    publish_catalog(
        &source,
        SCHEMA_VERSION,
        OLD_BUILD,
        &["/catalog/scripts/old.py"],
    );
    let cached_old = catalog_read_path(&source, &options);
    assert_ne!(cached_old, source);
    assert_eq!(script_paths(&cached_old), ["/catalog/scripts/old.py"]);
    let old_inode = fs::metadata(&cached_old).unwrap().ino();

    // Keep the outgoing build the way the builder does (`.1` hard link), then
    // the publisher upgrades: a new inode with a schema this client can't read.
    fs::hard_link(&source, share.path().join("scripts.sqlite.1")).unwrap();
    let staged = share.path().join("scripts.sqlite.new");
    publish_catalog(
        &staged,
        SCHEMA_VERSION + 1,
        NEW_BUILD,
        &["/catalog/scripts/new.py"],
    );
    fs::rename(&staged, &source).unwrap();

    // The client is pointed at the published file, never at the stale cached
    // copy of the previous build, and reports the mismatch from there.
    assert_eq!(catalog_read_path(&source, &options), source);
    assert!(matches!(
        open_db(&source),
        Err(Error::SchemaMismatch { found, .. }) if found == SCHEMA_VERSION + 1
    ));

    // Rollback restores the previous inode (rename of the `.1` copy). It is
    // the very file the cache already holds, so the entry is reused, not recopied.
    fs::rename(share.path().join("scripts.sqlite.1"), &source).unwrap();
    let cached_again = catalog_read_path(&source, &options);
    assert_eq!(cached_again, cached_old);
    assert_eq!(fs::metadata(&cached_again).unwrap().ino(), old_inode);
    assert_eq!(script_paths(&cached_again), ["/catalog/scripts/old.py"]);

    // Rollback by rebuilding with the old binary: a brand-new readable build
    // replaces the cache copy rather than being masked by it.
    let rebuilt = share.path().join("scripts.sqlite.rebuilt");
    publish_catalog(
        &rebuilt,
        SCHEMA_VERSION,
        NEW_BUILD,
        &["/catalog/scripts/rebuilt.py"],
    );
    fs::rename(&rebuilt, &source).unwrap();
    let cached_rebuilt = catalog_read_path(&source, &options);
    assert_ne!(cached_rebuilt, source);
    assert_eq!(
        script_paths(&cached_rebuilt),
        ["/catalog/scripts/rebuilt.py"]
    );
}

// ---------------------------------------------------------------------------
// Embeddings sidecar
// ---------------------------------------------------------------------------

#[test]
fn sidecar_formats_are_supported_unsupported_or_absent_explicitly() {
    let dir = TempDir::new().unwrap();
    let path = dir.path().join(SIDECAR_FILE_NAME);
    let rows = [("/a", [1.0, 0.0]), ("/b", [0.9, 0.1])];

    // Missing: the feature is simply not available.
    assert!(matches!(EmbeddingsSidecar::open_checked(&path), Ok(None)));

    // Legacy (no metadata table): supported, with no provenance to check.
    publish_sidecar(&path, None, None, &rows);
    let legacy = EmbeddingsSidecar::open_checked(&path).unwrap().unwrap();
    assert_eq!(legacy.len(), 2);
    assert_eq!(legacy.catalog_build_timestamp(), None);
    assert_eq!(legacy.provenance_note(NEW_BUILD), None);
    fs::remove_file(&path).unwrap();

    // Current format, recorded provenance.
    publish_sidecar(&path, Some(SIDECAR_FORMAT_VERSION), Some(OLD_BUILD), &rows);
    let current = EmbeddingsSidecar::open_checked(&path).unwrap().unwrap();
    assert_eq!(current.catalog_build_timestamp(), Some(OLD_BUILD));
    assert_eq!(current.provenance_note(OLD_BUILD), None);
    fs::remove_file(&path).unwrap();

    // Newer format: refused with an upgrade message, never half-read.
    publish_sidecar(
        &path,
        Some(SIDECAR_FORMAT_VERSION + 1),
        Some(OLD_BUILD),
        &rows,
    );
    let err = EmbeddingsSidecar::open_checked(&path).err().unwrap();
    assert!(matches!(
        err,
        SidecarError::UnsupportedFormat { found, supported, .. }
            if found == SIDECAR_FORMAT_VERSION + 1 && supported == SIDECAR_FORMAT_VERSION
    ));
    assert!(
        err.to_string().contains("upgrade scat on this host"),
        "{err}"
    );
    assert!(
        EmbeddingsSidecar::open(&path).is_none(),
        "optional consumers skip it"
    );
    fs::remove_file(&path).unwrap();

    // Not a sidecar at all.
    fs::write(&path, b"not a database").unwrap();
    let err = EmbeddingsSidecar::open_checked(&path).err().unwrap();
    assert!(matches!(err, SidecarError::Unreadable { .. }));
    assert!(err.to_string().contains("scat-embed"), "{err}");
}

#[test]
fn sidecar_from_another_catalog_build_is_paired_only_with_an_explicit_note() {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("scripts.sqlite");
    let sidecar = dir.path().join(SIDECAR_FILE_NAME);
    publish_catalog(&db, SCHEMA_VERSION, NEW_BUILD, &["/a", "/b"]);
    publish_sidecar(
        &sidecar,
        Some(SIDECAR_FORMAT_VERSION),
        Some(OLD_BUILD),
        &[("/a", [1.0, 0.0]), ("/b", [0.9, 0.1])],
    );
    let before = snapshot(dir.path());

    let out = scat(&db, None, &["similar", "/a", "--output", "json"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("\"/b\""), "{}", stdout(&out));
    let err = stderr(&out);
    assert!(err.contains(OLD_BUILD) && err.contains(NEW_BUILD), "{err}");
    assert!(err.contains("rerun scat-embed"), "{err}");
    assert_eq!(
        snapshot(dir.path()),
        before,
        "reads must not touch the share"
    );

    // Matching builds: no note.
    fs::remove_file(&sidecar).unwrap();
    publish_sidecar(
        &sidecar,
        Some(SIDECAR_FORMAT_VERSION),
        Some(NEW_BUILD),
        &[("/a", [1.0, 0.0]), ("/b", [0.9, 0.1])],
    );
    let out = scat(&db, None, &["similar", "/a", "--output", "json"]);
    assert!(out.status.success());
    assert!(
        !stderr(&out).contains("rerun scat-embed"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn unsupported_sidecar_is_reported_by_similar_and_audit_not_silently_ignored() {
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("scripts.sqlite");
    let sidecar = dir.path().join(SIDECAR_FILE_NAME);
    publish_catalog(&db, SCHEMA_VERSION, OLD_BUILD, &["/a", "/b"]);
    publish_sidecar(
        &sidecar,
        Some(SIDECAR_FORMAT_VERSION + 1),
        Some(OLD_BUILD),
        &[("/a", [1.0, 0.0]), ("/b", [0.9, 0.1])],
    );
    let before = snapshot(dir.path());

    let similar = scat(&db, None, &["similar", "/a"]);
    assert!(!similar.status.success());
    assert!(
        stderr(&similar).contains("upgrade scat on this host"),
        "{}",
        stderr(&similar)
    );

    // Audit still audits the catalog, but says what it skipped and why.
    let audit = scat(&db, None, &["catalog", "audit"]);
    assert!(audit.status.success(), "{}", stderr(&audit));
    assert!(
        stderr(&audit).contains("skipping embedding-based checks"),
        "{}",
        stderr(&audit)
    );
    assert_eq!(snapshot(dir.path()), before);
}

#[test]
fn sidecar_cache_follows_republish_and_rollback_of_the_sidecar() {
    let share = TempDir::new().unwrap();
    let root = TempDir::new().unwrap();
    let path = share.path().join(SIDECAR_FILE_NAME);
    let options = cache_options(&root);
    let rows = [("/a", [1.0, 0.0]), ("/b", [0.9, 0.1])];

    publish_sidecar(&path, Some(SIDECAR_FORMAT_VERSION), Some(OLD_BUILD), &rows);
    let cached = catalog_read_path(&path, &options);
    assert_ne!(cached, path);
    assert_eq!(
        EmbeddingsSidecar::open_checked(&cached)
            .unwrap()
            .unwrap()
            .catalog_build_timestamp(),
        Some(OLD_BUILD)
    );

    // Newer scat-embed publishes a format this client can't read: the cache
    // is a faithful copy, so the refusal is the same through it.
    let staged = share.path().join("embeddings.new");
    publish_sidecar(
        &staged,
        Some(SIDECAR_FORMAT_VERSION + 1),
        Some(NEW_BUILD),
        &rows,
    );
    fs::rename(&staged, &path).unwrap();
    let cached = catalog_read_path(&path, &options);
    assert!(matches!(
        EmbeddingsSidecar::open_checked(&cached),
        Err(SidecarError::UnsupportedFormat { .. })
    ));

    // Rollback to the previous format: the cache moves with it.
    let staged = share.path().join("embeddings.rolled-back");
    publish_sidecar(
        &staged,
        Some(SIDECAR_FORMAT_VERSION),
        Some(OLD_BUILD),
        &rows,
    );
    fs::rename(&staged, &path).unwrap();
    let cached = catalog_read_path(&path, &options);
    assert_eq!(
        EmbeddingsSidecar::open_checked(&cached)
            .unwrap()
            .unwrap()
            .catalog_build_timestamp(),
        Some(OLD_BUILD)
    );
}

#[test]
fn catalog_schema_changes_do_not_invalidate_a_sidecar() {
    // The sidecar pairs by logical path and is independent of the catalog
    // schema, so a schema bump is a catalog-only event: a sidecar built from
    // an older build keeps working (with the provenance note) next to the
    // rebuilt catalog, rather than being refused.
    let dir = TempDir::new().unwrap();
    let db = dir.path().join("scripts.sqlite");
    let sidecar = dir.path().join(SIDECAR_FILE_NAME);
    publish_sidecar(
        &sidecar,
        Some(SIDECAR_FORMAT_VERSION),
        Some(OLD_BUILD),
        &[("/a", [1.0, 0.0]), ("/b", [0.9, 0.1])],
    );
    publish_catalog(&db, SCHEMA_VERSION, NEW_BUILD, &["/a", "/b"]);

    let out = scat(&db, None, &["similar", "/a"]);
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(stdout(&out).contains("/b"));
}
