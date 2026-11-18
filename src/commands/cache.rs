use std::time::{Duration, SystemTime};

use scat_core::core::cache::{
    CacheOptions, CacheStatus, EntryInfo, Removed, cache_clear, cache_prune, cache_status,
};

use crate::output::{format_size, print_json, render_table};

fn age_seconds(entry: &EntryInfo) -> Option<u64> {
    entry
        .last_used
        .and_then(|t| SystemTime::now().duration_since(t).ok())
        .map(|d| d.as_secs())
}

fn describe_age(entry: &EntryInfo) -> String {
    age_seconds(entry).map_or_else(
        || "—".to_string(),
        |secs| scat_core::core::vc::relative_age(secs as f64),
    )
}

fn entry_json(entry: &EntryInfo) -> serde_json::Value {
    serde_json::json!({
        "key": entry.key,
        "source": entry.source,
        "bytes": entry.bytes,
        "age_seconds": age_seconds(entry),
        "build_timestamp": entry.build_timestamp,
    })
}

/// `scat cache status`.
pub fn cmd_cache_status(options: &CacheOptions, json: bool) {
    let CacheStatus {
        directory,
        entries,
        total_bytes,
    } = cache_status(options);
    if json {
        print_json(&serde_json::json!({
            "enabled": options.enabled,
            "directory": directory,
            "max_bytes": options.max_bytes,
            "total_bytes": total_bytes,
            "entries": entries.iter().map(entry_json).collect::<Vec<_>>(),
        }));
        return;
    }
    if !options.enabled {
        outln!("The local cache is disabled for this run (--no-cache / SCAT_NO_CACHE).");
    }
    let Some(directory) = directory else {
        outln!("No local cache yet: nothing has been copied for this user.");
        return;
    };
    outln!("Cache directory: {}", directory.display());
    let limit = options
        .max_bytes
        .map_or_else(|| "no limit".to_string(), format_size);
    outln!(
        "{} entries, {} in total ({limit}; configure with cache_max_bytes)",
        entries.len(),
        format_size(total_bytes)
    );
    if !entries.is_empty() {
        let rows = entries
            .iter()
            .map(|e| {
                vec![
                    e.source.as_ref().map_or_else(
                        || "(unknown source)".to_string(),
                        |p| p.display().to_string(),
                    ),
                    format_size(e.bytes),
                    describe_age(e),
                    e.build_timestamp.clone().unwrap_or_else(|| "—".to_string()),
                ]
            })
            .collect::<Vec<_>>();
        outln!(
            "{}",
            render_table(&["Source", "Size", "Last used", "Build"], &rows, false)
        );
    }
}

fn report_removed(what: &str, removed: Removed, json: bool) {
    if json {
        print_json(&serde_json::json!({
            "removed_entries": removed.entries,
            "freed_bytes": removed.bytes,
        }));
    } else {
        outln!(
            "{what} {} entries, freeing {}.",
            removed.entries,
            format_size(removed.bytes)
        );
    }
}

/// `scat cache clear`.
pub fn cmd_cache_clear(options: &CacheOptions, json: bool) {
    report_removed("Removed", cache_clear(options), json);
}

/// `scat cache prune`.
pub fn cmd_cache_prune(options: &CacheOptions, max_age_days: u64, json: bool) {
    let max_age = Duration::from_secs(max_age_days.saturating_mul(24 * 3600));
    report_removed("Pruned", cache_prune(options, max_age, true), json);
}
