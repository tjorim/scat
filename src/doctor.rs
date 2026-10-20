//! Read-only diagnostics. Filesystem/config/SQLite probes live in disposable
//! subprocesses: even a blocked autofs stat cannot block the UI or CLI forever.
use crate::{
    cli::Cli,
    tui::capabilities::{Capabilities, ColorMode, GlyphMode},
};
use anyhow::Result;
use scat_core::core::{
    cache::{CacheOptions, inspect_cache},
    resolve::PathResolver,
    vc::load_vc_config,
};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Read, Seek, SeekFrom},
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const LIMIT: u64 = 64 * 1024;
/// Per-file RPM metadata for packages with thousands of files outgrows `LIMIT`.
const RPM_RECORDS_LIMIT: u64 = 16 * 1024 * 1024;

/// Capture bounded output without a shell. Callers isolate tool spawning in
/// a probe subprocess too, since exec itself can stall on a network PATH.
/// Temporary capture files avoid pipe backpressure deadlocks.
fn capture(command: &mut Command, timeout: Duration) -> Value {
    capture_cancel(command, timeout, None)
}
pub fn capture_cancel(
    command: &mut Command,
    timeout: Duration,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> Value {
    capture_limited(command, timeout, cancel, LIMIT)
}
fn capture_limited(
    command: &mut Command,
    timeout: Duration,
    cancel: Option<&std::sync::atomic::AtomicBool>,
    limit: u64,
) -> Value {
    use std::os::unix::process::CommandExt;
    if cancel.is_some() {
        command.process_group(0);
    }

    let Ok(mut out) = tempfile::tempfile() else {
        return json!({"state":"unknown"});
    };
    let Ok(err) = tempfile::tempfile() else {
        return json!({"state":"unknown"});
    };
    let Ok(stdout) = out.try_clone() else {
        return json!({"state":"unknown"});
    };
    let Ok(stderr) = err.try_clone() else {
        return json!({"state":"unknown"});
    };
    let mut child = match command
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            return json!({"state": if e.kind() == std::io::ErrorKind::NotFound {"unavailable"} else {"unknown"}, "error_kind": format!("{:?}", e.kind())});
        }
    };
    let start = Instant::now();
    loop {
        if out.metadata().is_ok_and(|m| m.len() > limit)
            || err.metadata().is_ok_and(|m| m.len() > limit)
        {
            let _ = child.kill();
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            return json!({"state":"unknown", "reason":"probe output exceeded limit"});
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                let _ = out.seek(SeekFrom::Start(0));
                let mut text = String::new();
                let _ = out.take(limit).read_to_string(&mut text);
                // Raw stderr may contain environment/config secrets: never emit it.
                return json!({"state":"completed", "exit_code":status.code(), "stdout":text});
            }
            Ok(None)
                if start.elapsed() < timeout
                    && !cancel.is_some_and(|c| c.load(std::sync::atomic::Ordering::Relaxed)) =>
            {
                std::thread::sleep(Duration::from_millis(20));
            }
            _ => {
                if cancel.is_some() {
                    // Child created its own process group; terminate its probes too.
                    unsafe {
                        libc::kill(-(child.id() as i32), libc::SIGKILL);
                    }
                }
                let _ = child.kill();
                // Do not wait on an uninterruptible filesystem syscall.
                std::thread::spawn(move || {
                    let _ = child.wait();
                });
                return json!({"state":"unknown", "reason":"probe timed out or could not be waited for"});
            }
        }
    }
}
fn tool(program: &str, args: &[&str]) -> Value {
    tool_limited(program, args, LIMIT)
}
fn tool_limited(program: &str, args: &[&str], limit: u64) -> Value {
    capture_limited(
        Command::new(program).env("LC_ALL", "C").args(args),
        Duration::from_secs(2),
        None,
        limit,
    )
}

pub fn run(
    cli: &Cli,
    structured: bool,
    path: Option<&str>,
    mapping: Option<&Path>,
    rpm: bool,
    probe: Option<&str>,
    no_color: bool,
) -> Result<()> {
    if let Some(probe) = probe {
        println!("{}", local_probe(cli, probe, path, mapping, rpm));
        return Ok(());
    }
    let mut report = json!({
        "schema_version":1,
        "host": fs::read_to_string("/proc/sys/kernel/hostname").unwrap_or_else(|_| "unknown".into()).trim(),
        "os": std::env::consts::OS, "architecture":std::env::consts::ARCH,
        "terminal": Capabilities::detect(GlyphMode::Auto, ColorMode::Auto, no_color).report()

    });
    for probe in [
        "release",
        "catalog",
        "script",
        "viewers",
        "fapolicyd",
        "policy",
        "binary",
        "audit",
    ] {
        let mut cmd = Command::new(std::env::current_exe()?);
        if let Some(p) = &cli.config {
            cmd.arg("--config").arg(p);
        }
        if let Some(p) = &cli.db {
            cmd.arg("--db").arg(p);
        }
        if let Some(p) = &cli.cache_dir {
            cmd.arg("--cache-dir").arg(p);
        }
        if cli.no_cache {
            cmd.arg("--no-cache");
        }
        cmd.args(["doctor", "--probe", probe]);
        if let Some(p) = path {
            cmd.arg(p);
        }
        if let Some(p) = mapping {
            cmd.arg("--mapping").arg(p);
        }
        if rpm {
            cmd.arg("--rpm");
        }
        let result = capture(&mut cmd, Duration::from_secs(6));
        report[probe] = if result["exit_code"] == 0 {
            result["stdout"]
                .as_str()
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or_else(|| json!({"state":"unknown", "reason":"invalid probe response"}))
        } else {
            json!({"state":result["state"], "reason":"probe unavailable or exceeded deadline"})
        };
    }
    if structured {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("scat doctor — host-local, read-only diagnostics (schema 1)");
        for (key, value) in report.as_object().unwrap() {
            println!("\n{key}: {}", serde_json::to_string_pretty(value)?);
        }
    }
    Ok(())
}
fn local_probe(
    cli: &Cli,
    probe: &str,
    path: Option<&str>,
    mapping: Option<&Path>,
    rpm: bool,
) -> Value {
    match probe {
        "policy" => json!({
            "selinux": tool("getenforce", &[]),
            "fapolicyd_service": tool("systemctl", &["is-active", "fapolicyd"]),
            "denial_evidence":"See audit for selected-path evidence; EACCES alone is not evidence of a policy denial",
            "suggested_commands":["ausearch -m AVC,USER_AVC,SELINUX_ERR,USER_SELINUX_ERR -ts recent", "ausearch -m FANOTIFY -ts recent", "namei -l -- <path>", "ls -lZ -- <path>"],
            "note":"Service activity does not establish enforcement or a denial. Audit access may require an administrator. Policies remain administrator-owned."
        }),
        "binary" => std::env::current_exe().ok().map(|p| json!({"path":p, "access":access(&p)})).unwrap_or_else(|| json!({"state":"unknown"})),
        "audit" => {
            let Some(logical) = path else {return json!({"state":"not_applicable", "reason":"no selected path"});};
            let resolver = match mapping {Some(p) => PathResolver::from_file(p), None => Ok(PathResolver::new())};
            let Ok(resolver) = resolver else {return json!({"state":"unknown", "reason":"mapping unavailable"});};
            let native = resolver.to_native(logical);
            let native = Path::new(&native);
            let native = if native.is_absolute() {native.to_owned()} else {std::env::current_dir().unwrap_or_default().join(native)};
            let Some(file) = native.to_str() else {return json!({"state":"unknown"});};
            json!({"path":native, "selinux":audit_evidence(file, "AVC,USER_AVC,SELINUX_ERR,USER_SELINUX_ERR"), "fapolicyd":audit_evidence(file, "FANOTIFY"), "note":"Recent matching records are evidence to investigate, not proof of the cause of this scat operation; inaccessible logs or no matches remain unknown"})
        }
        "release" => {
            let release = fs::read_to_string("/etc/os-release").unwrap_or_default();
            json!({"release": release.lines().filter(|s| s.starts_with("PRETTY_NAME=") || s.starts_with("VERSION_ID=") || s.starts_with("ID=")).collect::<Vec<_>>()})
        }
        "fapolicyd" => {
            let config = fs::read_to_string("/etc/fapolicyd/fapolicyd.conf").unwrap_or_default();
            let permissive = config.lines().filter_map(|l| l.split('#').next()).filter_map(|l| l.split_once('=')).find(|(k,_)| k.trim() == "permissive").map(|(_,v)| v.trim());
            json!({"configured_permissive":permissive, "runtime_enforcement":"unknown", "note":"Configuration may differ from the running daemon"})
        }
        "catalog" => catalog(cli),
        "script" => {
            let Some(logical) = path else { return json!({"state":"not_applicable"}); };
            let resolver = match mapping { Some(p) => PathResolver::from_file(p), None => Ok(PathResolver::new()) };
            let Ok(resolver) = resolver else { return json!({"state":"unknown", "reason":"mapping unavailable or invalid", "mapping":mapping}); };
            let native = resolver.to_native(logical);
            let native = Path::new(&native);
            let native = if native.is_absolute() { native.to_owned() } else { std::env::current_dir().unwrap_or_default().join(native) };
            json!({"scope":"queried host filesystem; not catalog provenance", "logical_path":logical, "mapping":mapping, "mapping_resolution": if resolver.to_native(logical) == logical {"identity (no translation applied)"} else {"mapped"}, "resolved_path":native, "access":access(&native), "rpm": if rpm { rpm_file(&native) } else {json!({"state":"not_requested"})}})
        }
        "viewers" => match crate::tui::viewer::viewer_commands() {
            Ok(commands) => json!(commands.into_iter().map(|c| {
                let paths = std::env::var_os("PATH").unwrap_or_default();
                let candidates = if c.program.contains('/') {vec![c.program.clone().into()]} else {std::env::split_paths(&paths).map(|p| p.join(&c.program)).collect()};
                let found = candidates.iter().find(|p| fs::metadata(p).is_ok_and(|m| {use std::os::unix::fs::PermissionsExt; m.is_file() && m.permissions().mode() & 0o111 != 0}));
                json!({"program":Path::new(&c.program).file_name().map(|s| s.to_string_lossy()), "path":found, "access":found.map(|p| access(p)), "execution":"not attempted; policy may still deny execution"})
            }).collect::<Vec<_>>()),
            Err(_) => json!({"state":"unknown", "reason":"invalid viewer command; arguments omitted"}),
        },
        _ => json!({"state":"unknown"}),
    }
}
fn catalog(cli: &Cli) -> Value {
    let Ok(config) = load_vc_config(cli.config.as_deref()) else {
        return json!({"state":"unknown", "reason":"configuration inaccessible or invalid"});
    };
    let Some(db) = cli.db.as_ref().or(config.db_path.as_ref()) else {
        return json!({"state":"not_configured"});
    };
    let options = CacheOptions {
        enabled: !crate::cli::resolve_no_cache(
            cli.no_cache,
            std::env::var_os("SCAT_NO_CACHE").as_deref(),
        ),
        root: cli.cache_dir.clone().or(config.cache_dir),
    };
    let cache = inspect_cache(db, &options);
    let read_path = cache["active_read_path"]
        .as_str()
        .map(Path::new)
        .unwrap_or(db);
    let metadata = scat_core::core::search::open_search_api(read_path)
        .and_then(|api| api.index_metadata())
        .ok();
    let age = metadata
        .as_ref()
        .and_then(|m| {
            m.build_timestamp
                .as_str()
                .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        })
        .map(|t| (chrono::Utc::now() - t.to_utc()).num_seconds());
    json!({"source":db, "source_access":access(db), "cache_access":cache["directory"].as_str().map(|p| access(Path::new(p))), "cache_space": tool("df", &["--output=avail", "-k", "--", options.root.as_deref().unwrap_or(Path::new("/dev/shm")).to_str().unwrap_or("/dev/shm")]), "cache":cache, "catalog_metadata":metadata, "catalog_age_seconds":age})
}
fn access(path: &Path) -> Value {
    use std::os::unix::fs::MetadataExt;
    let metadata = fs::metadata(path);
    let mountinfo = fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
    let absolute = fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    json!({"metadata":metadata.as_ref().ok().map(|m| json!({"uid":m.uid(), "gid":m.gid(), "mode":format!("{:04o}",m.mode() & 0o7777), "size":m.len()})), "readable":fs::File::open(path).is_ok(), "access_by_real_ids":access_bits(path), "error_kind":metadata.err().map(|e| format!("{:?}", e.kind())), "mount":mount_for(&absolute, &mountinfo), "note":"noexec restricts binary execution, not catalog reads; ro restricts writes. Missing paths can include unavailable network/autofs mounts. PermissionDenied does not identify SELinux/fapolicyd."})
}
fn audit_evidence(path: &str, types: &str) -> Value {
    let result = tool(
        "ausearch",
        &["-m", types, "-ts", "recent", "-f", path, "--raw"],
    );
    if result["exit_code"] != 0 {
        return json!({"state":"unknown", "reason":"audit tool/logs unavailable, no matching records, or deadline exceeded"});
    }
    let text = result["stdout"].as_str().unwrap_or("");
    let events = text
        .lines()
        .filter(|line| {
            if types == "FANOTIFY" {
                line.contains("type=FANOTIFY ")
            } else {
                line.contains("type=AVC ")
                    || line.contains("type=USER_AVC ")
                    || line.contains("type=SELINUX_ERR ")
                    || line.contains("type=USER_SELINUX_ERR ")
            }
        })
        .collect::<Vec<_>>();
    let denials = events
        .iter()
        .filter(|line| {
            line.contains("avc:  denied")
                || line.contains("avc: denied")
                || line
                    .split_whitespace()
                    .any(|field| matches!(field, "dec=deny" | "dec=deny_audit" | "resp=2"))
        })
        .count();
    json!({"state":if events.is_empty() {"unknown"} else {"observed"}, "matching_record_count":events.len(), "denial_marker_count":denials, "raw_records":"omitted"})
}

fn access_bits(path: &Path) -> Value {
    use std::os::unix::ffi::OsStrExt;
    let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return Value::Null;
    };
    // access() only checks permissions; it never opens for writing or executes.
    let check = |mode| unsafe { libc::access(path.as_ptr(), mode) == 0 };
    json!({"read":check(libc::R_OK), "write":check(libc::W_OK), "execute_or_search":check(libc::X_OK)})
}

fn mount_for(path: &Path, input: &str) -> Option<Value> {
    input.lines().filter_map(|line| {
        let (left,right) = line.split_once(" - ")?;
        let fields:Vec<_> = left.split_whitespace().collect();
        let root = fields.get(4)?.replace("\\040", " ").replace("\\011", "\t").replace("\\134", "\\");
        if !path.starts_with(&root) {return None;}
        let tail:Vec<_> = right.split_whitespace().collect();
        Some((root.len(), json!({"mountpoint":root, "options":safe_mount_options(fields.get(5).copied().unwrap_or("")), "filesystem":tail.first(), "super_options":safe_mount_options(tail.get(2).copied().unwrap_or(""))})))
    }).max_by_key(|(len,_)| *len).map(|(_,v)|v)
}
fn rpm_file(path: &Path) -> Value {
    let Some(path) = path.to_str() else {
        return json!({"state":"unverifiable"});
    };
    let owner = tool(
        "rpm",
        &[
            "-qf",
            "--qf",
            "%{NAME}\t%{VERSION}-%{RELEASE}\t%{ARCH}\n",
            "--",
            path,
        ],
    );
    if owner["state"] != "completed" {
        return json!({"state":"unverifiable", "query":owner});
    }
    if owner["exit_code"] != 0 {
        // A nonzero query can be an RPM database failure, not just unowned.
        let unowned = owner["stdout"]
            .as_str()
            .is_some_and(|s| s.contains("is not owned by any package"));
        return json!({"state":if unowned {"unowned"} else {"unverifiable"}});
    }
    // Query metadata only; never use rpm -Vf (which verifies the whole package)
    // and never execute RPM verification scriptlets.
    let records = tool_limited(
        "rpm",
        &[
            "-qf",
            "--qf",
            "[%{FILENAMES}\t%{FILESIZES}\t%{FILEMODES}\t%{FILEMTIMES}\t%{FILEDIGESTS}\t%{FILELINKTOS}\n]",
            "--",
            path,
        ],
        RPM_RECORDS_LIMIT,
    );
    let algorithm = tool("rpm", &["-qf", "--qf", "%{FILEDIGESTALGO}", "--", path]);
    let status = records["stdout"]
        .as_str()
        .and_then(|text| {
            text.lines().find_map(|line| {
                let fields: Vec<_> = line.split('\t').collect();
                (fields.len() == 6 && fields[0] == path).then(|| {
                    verify_file(
                        Path::new(path),
                        &fields,
                        algorithm["stdout"].as_str().unwrap_or(""),
                    )
                })
            })
        })
        .unwrap_or("unverifiable");
    json!({"state":status, "owner":owner["stdout"], "verification_scope":"selected file size, mode, mtime, link target and digest; owner/group names, capabilities and security labels not verified", "digest_algorithm":algorithm["stdout"]})
}
fn verify_file(path: &Path, fields: &[&str], algorithm: &str) -> &'static str {
    use sha2::{Sha256, Sha512};
    use std::os::unix::fs::MetadataExt;
    let Ok(meta) = fs::symlink_metadata(path) else {
        return "unverifiable";
    };
    if fields[1].parse::<u64>().ok() != Some(meta.len())
        || fields[2].parse::<u32>().ok() != Some(meta.mode())
        || fields[3].parse::<i64>().ok() != Some(meta.mtime())
    {
        return "modified";
    }
    if meta.file_type().is_symlink() {
        return if fs::read_link(path).ok().as_deref() == Some(Path::new(fields[5])) {
            "unchanged"
        } else {
            "modified"
        };
    }
    if !meta.is_file() {
        return "unverifiable";
    }
    let Ok(file) = fs::File::open(path) else {
        return "unverifiable";
    };
    let digest = match algorithm.trim() {
        "8" => {
            let Some(digest) = file_digest::<Sha256>(file) else {
                return "unverifiable";
            };
            digest
        }
        "10" => {
            let Some(digest) = file_digest::<Sha512>(file) else {
                return "unverifiable";
            };
            digest
        }
        _ => return "unverifiable",
    };
    if digest == fields[4] {
        "unchanged"
    } else {
        "modified"
    }
}

fn file_digest<D: sha2::Digest>(mut file: fs::File) -> Option<String> {
    let mut digest = D::new();
    let mut buffer = [0; 65536];
    loop {
        let n = file.read(&mut buffer).ok()?;
        if n == 0 {
            break;
        }
        digest.update(&buffer[..n]);
    }
    Some(hex::encode(digest.finalize()))
}

fn safe_mount_options(options: &str) -> String {
    options
        .split(',')
        .filter(|s| {
            matches!(
                *s,
                "ro" | "rw"
                    | "noexec"
                    | "exec"
                    | "nosuid"
                    | "nodev"
                    | "relatime"
                    | "noatime"
                    | "seclabel"
                    | "soft"
                    | "hard"
                    | "intr"
                    | "sync"
                    | "async"
            )
        })
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;
    #[test]
    fn mount_fixtures_preserve_noexec_ro_and_network_facts() {
        let mounts = "1 0 0:1 / / rw - ext4 /dev/root rw\n2 1 0:2 / /net ro,noexec - nfs server:/export ro\n3 1 0:3 / /auto rw - autofs none rw\n4 1 0:4 / /space\\040dir rw,noexec - tmpfs none rw";
        let mount = mount_for(Path::new("/net/catalog"), mounts).unwrap();
        assert_eq!(mount["options"], "ro,noexec");
        assert_eq!(mount["filesystem"], "nfs");
        assert_eq!(
            mount_for(Path::new("/network"), mounts).unwrap()["filesystem"],
            "ext4"
        );
        assert_eq!(
            mount_for(Path::new("/auto/missing"), mounts).unwrap()["filesystem"],
            "autofs"
        );
        assert_eq!(
            mount_for(Path::new("/space dir/a"), mounts).unwrap()["options"],
            "rw,noexec"
        );
    }
    #[test]
    fn selected_file_verification_detects_changes_and_unknown_algorithms() {
        use sha2::{Digest, Sha256};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("script");
        fs::write(&path, "original").unwrap();
        let m = fs::metadata(&path).unwrap();
        let fields = [
            path.to_str().unwrap().to_owned(),
            m.len().to_string(),
            m.mode().to_string(),
            m.mtime().to_string(),
            hex::encode(Sha256::digest(b"original")),
            String::new(),
        ];
        let fields: Vec<_> = fields.iter().map(String::as_str).collect();
        assert_eq!(verify_file(&path, &fields, "8"), "unchanged");
        assert_eq!(verify_file(&path, &fields, "1"), "unverifiable");
        fs::write(&path, "modified").unwrap();
        assert_eq!(verify_file(&path, &fields, "8"), "modified");
        fs::remove_file(&path).unwrap();
        assert_eq!(verify_file(&path, &fields, "8"), "unverifiable");
    }
    #[test]
    fn commands_are_bounded_and_missing_tools_are_unknown() {
        let start = Instant::now();
        let result = capture(Command::new("sleep").arg("10"), Duration::from_millis(50));
        assert_eq!(result["state"], "unknown");
        assert!(start.elapsed() < Duration::from_secs(1));
        assert_eq!(tool("/nonexistent/scat-tool", &[])["state"], "unavailable");
    }
    #[test]
    fn policy_output_preserves_states_without_inventing_denials() {
        for mode in ["Enforcing", "Permissive", "Disabled"] {
            let result = capture(Command::new("printf").arg(mode), Duration::from_secs(1));
            assert_eq!(result["stdout"], mode);
            assert_eq!(result["exit_code"], 0);
        }
    }
    #[test]
    fn cancellation_terminates_probe() {
        let cancel = std::sync::atomic::AtomicBool::new(true);
        let result = capture_cancel(
            Command::new("sleep").arg("10"),
            Duration::from_secs(10),
            Some(&cancel),
        );
        assert_eq!(result["state"], "unknown");
    }
}
