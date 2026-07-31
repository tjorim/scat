use super::*;
use crate::core::vc::REVISION_TYPE_WORKING;
use std::collections::HashSet;

#[test]
fn scan_records_working_dir_version_copies_as_revisions() {
    // Layout mirrors a vc working directory: the active script name plus
    // the two most recent checked-in copies (`<script>_<timestamp>`).
    let dir = tempfile::TempDir::new().unwrap();
    let scan_root = dir.path().join("linux").join("scripts");
    std::fs::create_dir_all(&scan_root).unwrap();
    std::fs::write(scan_root.join("bar.py"), "# python").unwrap();
    std::fs::write(scan_root.join("bar.py_20260720_090000"), "# python").unwrap();
    std::fs::write(scan_root.join("bar.py_20260610_151550"), "# python").unwrap();

    let shutdown = AtomicBool::new(false);
    let result = scan_paths_with_revisions(
        std::slice::from_ref(&scan_root),
        5,
        &[],
        &[],
        None,
        &shutdown,
    )
    .unwrap();

    assert_eq!(result.scripts.len(), 1, "only the active script indexes");
    assert!(result.scripts[0].physical_path.ends_with("bar.py"));

    assert_eq!(result.revisions.len(), 2);
    let expected_logical_path = scan_root.join("bar.py").to_string_lossy().into_owned();
    for revision in &result.revisions {
        assert_eq!(revision.logical_path, expected_logical_path);
        assert_eq!(revision.revision_type, REVISION_TYPE_WORKING);
        assert_eq!(revision.user, "");
        assert_eq!(revision.os_flavor, "linux");
    }
    let mut timestamps: Vec<&str> = result
        .revisions
        .iter()
        .map(|r| r.timestamp.as_str())
        .collect();
    timestamps.sort_unstable();
    assert_eq!(timestamps, vec!["20260610_151550", "20260720_090000"]);
}

#[test]
fn scan_records_version_copies_of_extensionless_scripts_as_revisions() {
    // The same working-directory layout, for a shell tool carrying no
    // extension: the versions belong to `prepare_release`, not to five
    // scripts that happen to share a prefix.
    let dir = tempfile::TempDir::new().unwrap();
    let scan_root = dir.path().join("linux").join("scripts");
    std::fs::create_dir_all(&scan_root).unwrap();
    std::fs::write(scan_root.join("prepare_release"), "#!/bin/sh\n").unwrap();
    std::fs::write(
        scan_root.join("prepare_release_20260729_140513"),
        "#!/bin/sh\n",
    )
    .unwrap();
    std::fs::write(
        scan_root.join("prepare_release_20260701_105550"),
        "#!/bin/sh\n",
    )
    .unwrap();

    let shutdown = AtomicBool::new(false);
    let result = scan_paths_with_revisions(
        std::slice::from_ref(&scan_root),
        5,
        &[],
        &[],
        None,
        &shutdown,
    )
    .unwrap();

    let paths: Vec<&str> = result
        .scripts
        .iter()
        .map(|s| s.logical_path.as_str())
        .collect();
    let expected_logical_path = scan_root
        .join("prepare_release")
        .to_string_lossy()
        .into_owned();
    assert_eq!(paths, vec![expected_logical_path.as_str()]);

    assert_eq!(result.revisions.len(), 2);
    for revision in &result.revisions {
        assert_eq!(revision.logical_path, expected_logical_path);
        assert_eq!(revision.revision_type, REVISION_TYPE_WORKING);
    }
}

#[test]
fn scan_records_version_copies_behind_an_active_symlink() {
    // vc's steady state: the script name is a symlink to the newest
    // version. Only the symlink indexes, and it keeps pointing at the
    // version it resolves to.
    let dir = tempfile::TempDir::new().unwrap();
    let scan_root = dir.path().join("linux").join("scripts");
    std::fs::create_dir_all(&scan_root).unwrap();
    std::fs::write(
        scan_root.join("prepare_release_20260729_140513"),
        "#!/bin/sh\n",
    )
    .unwrap();
    std::fs::write(
        scan_root.join("prepare_release_20260701_105550"),
        "#!/bin/sh\n",
    )
    .unwrap();
    std::os::unix::fs::symlink(
        "prepare_release_20260729_140513",
        scan_root.join("prepare_release"),
    )
    .unwrap();

    let shutdown = AtomicBool::new(false);
    let result = scan_paths_with_revisions(
        std::slice::from_ref(&scan_root),
        5,
        &[],
        &[],
        None,
        &shutdown,
    )
    .unwrap();

    assert_eq!(result.scripts.len(), 1);
    assert_eq!(
        result.scripts[0].logical_path,
        scan_root.join("prepare_release").to_string_lossy()
    );
    assert_eq!(
        result.scripts[0].symlink_target.as_deref(),
        Some(
            scan_root
                .join("prepare_release_20260729_140513")
                .to_string_lossy()
                .as_ref()
        )
    );
    assert_eq!(result.revisions.len(), 2);
}

#[test]
fn scan_skips_editor_backup_files() {
    // `foo.sh~` is already dropped as an unknown extension; `foo~` has no
    // extension and must not reach the shebang sniff.
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("prepare_release"), "#!/bin/sh\n").unwrap();
    std::fs::write(dir.path().join("prepare_release~"), "#!/bin/sh\n").unwrap();
    std::fs::write(dir.path().join("release_backup.sh"), "#!/bin/bash\n").unwrap();
    std::fs::write(dir.path().join("release_backup.sh~"), "#!/bin/bash\n").unwrap();

    let shutdown = AtomicBool::new(false);
    let records = scan_paths(&[dir.path().to_path_buf()], 5, &[], &[], None, &shutdown).unwrap();

    let mut paths: Vec<&str> = records.iter().map(|r| r.logical_path.as_str()).collect();
    paths.sort_unstable();
    let expected_prepare_release = dir
        .path()
        .join("prepare_release")
        .to_string_lossy()
        .into_owned();
    let expected_release_backup = dir
        .path()
        .join("release_backup.sh")
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        paths,
        vec![
            expected_prepare_release.as_str(),
            expected_release_backup.as_str(),
        ]
    );
}

#[test]
fn nested_double_timestamped_name_is_not_recorded_as_a_working_revision() {
    // A genuine vc-managed checked-in copy carries exactly one timestamp
    // suffix (see docs/VC_CONTRACT.md). A filename with two
    // timestamp-shaped segments doesn't fit that shape: treating it as a
    // WORKING revision would fold the earlier segment into a fabricated
    // "bar.py_20240101_090000" script identity that can never
    // correspond to a real active script.
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(
        dir.path().join("bar.py_20240101_090000_20250101_090000"),
        "#!/usr/bin/env python3\n",
    )
    .unwrap();

    let shutdown = AtomicBool::new(false);
    let result =
        scan_paths_with_revisions(&[dir.path().to_path_buf()], 5, &[], &[], None, &shutdown)
            .unwrap();

    assert!(
        result.revisions.is_empty(),
        "nested double-timestamped name must not be recorded as a revision"
    );
}

#[test]
fn extensionless_date_suffixed_file_stays_a_script() {
    // A shebang script whose name merely ends in digits must not be
    // swallowed as a WORKING revision — only version names whose script
    // part has a script extension qualify.
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("backup_20240101"), "#!/bin/bash\n").unwrap();

    let shutdown = AtomicBool::new(false);
    let result =
        scan_paths_with_revisions(&[dir.path().to_path_buf()], 5, &[], &[], None, &shutdown)
            .unwrap();

    assert_eq!(result.scripts.len(), 1);
    assert!(result.revisions.is_empty());
}

#[test]
fn scan_finds_py_and_sh_files() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("a.py"), "# python").unwrap();
    std::fs::write(dir.path().join("b.sh"), "#!/bin/bash").unwrap();
    std::fs::write(dir.path().join("c.txt"), "ignored").unwrap();

    let shutdown = AtomicBool::new(false);
    let records = scan_paths(&[dir.path().to_path_buf()], 5, &[], &[], None, &shutdown).unwrap();
    assert_eq!(records.len(), 2);
    let paths: Vec<&str> = records.iter().map(|r| r.language.as_str()).collect();
    assert!(paths.contains(&"python"));
    assert!(paths.contains(&"shell"));
}

#[test]
fn scan_respects_root_catignore() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::create_dir(dir.path().join("vendor")).unwrap();
    std::fs::write(dir.path().join(".catignore"), "vendor/\n").unwrap();
    std::fs::write(dir.path().join("keep.py"), "# python").unwrap();
    std::fs::write(dir.path().join("vendor").join("skip.py"), "# python").unwrap();

    let shutdown = AtomicBool::new(false);
    let records = scan_paths(&[dir.path().to_path_buf()], 5, &[], &[], None, &shutdown).unwrap();

    assert_eq!(records.len(), 1);
    assert!(records[0].physical_path.ends_with("keep.py"));
}

#[test]
fn scan_respects_explicit_ignore_files() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::create_dir(dir.path().join("vendor")).unwrap();
    let ignore_file = dir.path().join("custom.catignore");
    std::fs::write(&ignore_file, "vendor/\n").unwrap();
    std::fs::write(dir.path().join("keep.py"), "# python").unwrap();
    std::fs::write(dir.path().join("vendor").join("skip.py"), "# python").unwrap();

    let shutdown = AtomicBool::new(false);
    let records = scan_paths(
        &[dir.path().to_path_buf()],
        5,
        &[ignore_file],
        &[],
        None,
        &shutdown,
    )
    .unwrap();

    assert_eq!(records.len(), 1);
    assert!(records[0].physical_path.ends_with("keep.py"));
}

#[test]
fn checkout_files_in_develop_dir_are_skipped() {
    let dir = tempfile::TempDir::new().unwrap();
    let develop = dir.path().join("DEVELOP");
    std::fs::create_dir_all(&develop).unwrap();
    std::fs::write(
        develop.join("tool_20240315_1430_jdoe"),
        "#!/bin/bash\necho hi",
    )
    .unwrap();

    let shutdown = AtomicBool::new(false);
    let result = scan_paths_with_revisions(
        &[dir.path().to_path_buf()],
        5,
        &[],
        &["DEVELOP"],
        None,
        &shutdown,
    )
    .unwrap();

    assert!(
        result.scripts.is_empty(),
        "DEVELOP dir files must not be indexed as scripts"
    );
}

#[test]
fn checkout_files_in_archive_dir_are_skipped() {
    let dir = tempfile::TempDir::new().unwrap();
    let archive = dir.path().join("ARCHIVE");
    std::fs::create_dir_all(&archive).unwrap();
    std::fs::write(archive.join("tool.py_20240101_1200_alice"), "print('x')").unwrap();

    let shutdown = AtomicBool::new(false);
    let result = scan_paths_with_revisions(
        &[dir.path().to_path_buf()],
        5,
        &[],
        &["ARCHIVE"],
        None,
        &shutdown,
    )
    .unwrap();

    assert!(
        result.scripts.is_empty(),
        "ARCHIVE dir files must not be indexed as scripts"
    );
}

#[test]
fn checkout_files_in_custom_dir_are_skipped() {
    let dir = tempfile::TempDir::new().unwrap();
    let checkout = dir.path().join("WORKING");
    std::fs::create_dir_all(&checkout).unwrap();
    std::fs::write(
        checkout.join("tool_20240315_1430_jdoe"),
        "#!/bin/bash\necho hi",
    )
    .unwrap();

    let shutdown = AtomicBool::new(false);
    let result = scan_paths_with_revisions(
        &[dir.path().to_path_buf()],
        5,
        &[],
        &["WORKING"],
        None,
        &shutdown,
    )
    .unwrap();

    assert!(
        result.scripts.is_empty(),
        "custom checkout dir files must not be indexed as scripts"
    );
}

#[test]
fn scan_skips_oversized_files() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("small.py"), "# python").unwrap();
    let huge = vec![0u8; (candidate::MAX_INDEXABLE_FILE_SIZE_BYTES + 1) as usize];
    std::fs::write(dir.path().join("huge.py"), &huge).unwrap();

    let shutdown = AtomicBool::new(false);
    let records = scan_paths(&[dir.path().to_path_buf()], 5, &[], &[], None, &shutdown).unwrap();

    assert_eq!(records.len(), 1);
    assert!(records[0].physical_path.ends_with("small.py"));
}

#[test]
fn scan_does_not_follow_symlinks_outside_scan_roots() {
    let root = tempfile::TempDir::new().unwrap();
    let outside = tempfile::TempDir::new().unwrap();
    std::fs::write(outside.path().join("secret.py"), "# python").unwrap();
    std::fs::write(root.path().join("keep.py"), "# python").unwrap();
    std::os::unix::fs::symlink(outside.path(), root.path().join("escape")).unwrap();

    let shutdown = AtomicBool::new(false);
    let records = scan_paths(&[root.path().to_path_buf()], 5, &[], &[], None, &shutdown).unwrap();

    assert_eq!(records.len(), 1);
    assert!(records[0].physical_path.ends_with("keep.py"));
}

#[test]
fn scan_follows_symlinks_within_scan_roots() {
    let root = tempfile::TempDir::new().unwrap();
    let real = root.path().join("linux");
    std::fs::create_dir(&real).unwrap();
    std::fs::write(real.join("tool.py"), "# python").unwrap();
    std::os::unix::fs::symlink(&real, root.path().join("alt")).unwrap();

    let shutdown = AtomicBool::new(false);
    let records = scan_paths(&[root.path().to_path_buf()], 5, &[], &[], None, &shutdown).unwrap();

    // Both the real path and the in-root symlink alias are indexed.
    assert_eq!(records.len(), 2);
}

#[test]
fn scan_processes_a_root_spanning_multiple_parallel_batches() {
    // More candidates than SCAN_PROCESS_BATCH_SIZE, so the per-root
    // parallel processing loop runs across several batches — exercises
    // that results from every batch (and every worker thread) land in
    // `records` exactly once, with no duplicates or drops.
    let dir = tempfile::TempDir::new().unwrap();
    let file_count = SCAN_PROCESS_BATCH_SIZE * 2 + 50;
    for i in 0..file_count {
        std::fs::write(dir.path().join(format!("script_{i:04}.py")), "# python").unwrap();
    }

    let shutdown = AtomicBool::new(false);
    let records = scan_paths(&[dir.path().to_path_buf()], 5, &[], &[], None, &shutdown).unwrap();

    assert_eq!(records.len(), file_count);
    let mut seen: HashSet<&str> = HashSet::new();
    for record in &records {
        assert_eq!(record.language, "python");
        assert!(
            seen.insert(record.logical_path.as_str()),
            "duplicate logical_path: {}",
            record.logical_path
        );
    }
}

#[test]
fn scan_respects_shutdown_signal_mid_batch() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::write(dir.path().join("a.py"), "# python").unwrap();
    let shutdown = AtomicBool::new(true);

    let err = scan_paths_with_revisions(&[dir.path().to_path_buf()], 5, &[], &[], None, &shutdown)
        .unwrap_err();

    assert!(matches!(err, Error::Interrupted));
}
