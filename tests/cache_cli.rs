//! `scat cache status|clear|prune` against a real catalog and an isolated
//! cache root (never the host's `/dev/shm`).
use std::path::{Path, PathBuf};
use std::process::Command;

struct Env {
    dir: tempfile::TempDir,
    cache_root: PathBuf,
    db: PathBuf,
}

fn scat(env: &Env, args: &[&str]) -> (bool, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_scat"))
        .env_remove("SCAT_CONFIG")
        .env_remove("SCAT_DB")
        .env_remove("SCAT_NO_CACHE")
        .arg("--db")
        .arg(&env.db)
        .arg("--cache-dir")
        .arg(&env.cache_root)
        .args(args)
        .output()
        .unwrap();
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

fn env() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("root");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("a.sh"), "#!/bin/bash\n# @purpose alpha\n").unwrap();
    let cache_root = dir.path().join("cache");
    std::fs::create_dir(&cache_root).unwrap();
    let env = Env {
        db: dir.path().join("cat.sqlite"),
        cache_root,
        dir,
    };
    let (ok, _, err) = scat(
        &env,
        &[
            "catalog",
            "build",
            "--scan-root",
            root.to_str().unwrap(),
            "-q",
        ],
    );
    assert!(ok, "{err}");
    env
}

fn status_json(env: &Env) -> serde_json::Value {
    let (ok, out, err) = scat(env, &["cache", "status", "--json"]);
    assert!(ok, "{err}");
    serde_json::from_str(&out).unwrap()
}

#[test]
fn status_clear_and_prune_work_on_the_users_cache() {
    let env = env();

    let (ok, text, _) = scat(&env, &["cache", "status"]);
    assert!(ok);
    assert!(text.contains("No local cache yet"), "{text}");
    assert!(status_json(&env)["entries"].as_array().unwrap().is_empty());

    // Reading the catalog populates the cache.
    let (ok, _, err) = scat(&env, &["catalog", "stats"]);
    assert!(ok, "{err}");
    let status = status_json(&env);
    let entries = status["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(
        Path::new(entries[0]["source"].as_str().unwrap()),
        env.db.canonicalize().unwrap_or(env.db.clone()).as_path()
    );
    assert!(entries[0]["bytes"].as_u64().unwrap() > 0);
    assert_eq!(status["total_bytes"], entries[0]["bytes"]);

    let (_, text, _) = scat(&env, &["cache", "status"]);
    assert!(text.contains("1 entries"), "{text}");
    assert!(text.contains("cat.sqlite"), "{text}");

    // Prune keeps a recently used entry whose catalog still exists...
    let (ok, text, _) = scat(&env, &["cache", "prune"]);
    assert!(ok);
    assert!(text.contains("Pruned 0 entries"), "{text}");
    assert_eq!(status_json(&env)["entries"].as_array().unwrap().len(), 1);

    // ...and drops it once the published catalog is gone.
    let kept = env.dir.path().join("moved.sqlite");
    std::fs::rename(&env.db, &kept).unwrap();
    let (ok, text, _) = scat(&env, &["cache", "prune", "--json"]);
    assert!(ok);
    let pruned: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(pruned["removed_entries"], 1);
    assert!(status_json(&env)["entries"].as_array().unwrap().is_empty());

    // Clear removes everything and says how much it freed.
    std::fs::rename(&kept, &env.db).unwrap();
    scat(&env, &["catalog", "stats"]);
    assert_eq!(status_json(&env)["entries"].as_array().unwrap().len(), 1);
    let (ok, text, _) = scat(&env, &["cache", "clear"]);
    assert!(ok);
    assert!(text.contains("Removed 1 entries"), "{text}");
    assert!(status_json(&env)["entries"].as_array().unwrap().is_empty());
}

#[test]
fn a_size_limit_in_the_config_keeps_the_catalog_out_of_the_cache() {
    let env = env();
    let config = env.dir.path().join("scat.yaml");
    std::fs::write(&config, "cache_max_bytes: 1K\n").unwrap();
    let config_arg = config.to_str().unwrap();

    // Too big for the limit: still answered, from the published file.
    let (ok, _, err) = scat(&env, &["--config", config_arg, "catalog", "stats"]);
    assert!(ok, "{err}");
    assert!(
        status_json(&env)["entries"].as_array().unwrap().is_empty(),
        "nothing may be cached over the limit"
    );
    let (_, text, _) = scat(&env, &["--config", config_arg, "cache", "status", "--json"]);
    let status: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(status["max_bytes"], 1024);
}
