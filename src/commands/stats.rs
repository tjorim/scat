use anyhow::Result;

use crate::output::{print_json, render_table};

pub fn cmd_stats(
    api: &scat_core::core::search::SearchApi,
    json: bool,
    no_color: bool,
    vc_configured: bool,
) -> Result<()> {
    let mut data = api.stats()?;
    if !vc_configured {
        data.revisions = None;
        // Checkout-derived: revisions rows can outlive a vc config that was
        // since removed, so gate these the same way `revisions` is gated
        // rather than showing possibly stale vc data as current.
        data.checkout_by_os = Vec::new();
        data.most_active_checkout_users = Vec::new();
        data.checkout_staleness_histogram = Vec::new();
        data.checkout_activity_by_day = Vec::new();
    }

    if json {
        print_json(&data);
        return Ok(());
    }

    println!("Total scripts: {}", data.total_scripts);
    println!("\nBy language:");
    let by_language = data
        .by_language
        .iter()
        .map(|row| vec![row.language.clone(), row.count.to_string()])
        .collect::<Vec<_>>();
    println!(
        "{}",
        render_table(&["Language", "Count"], &by_language, no_color)
    );
    println!("\nBy owner:");
    let by_owner = data
        .by_owner
        .iter()
        .map(|row| vec![row.owner.clone(), row.count.to_string()])
        .collect::<Vec<_>>();
    println!("{}", render_table(&["Owner", "Count"], &by_owner, no_color));
    if !data.most_depended_upon.is_empty() {
        println!("\nMost depended-upon:");
        let most_depended_upon = data
            .most_depended_upon
            .iter()
            .map(|row| vec![row.logical_path.clone(), row.count.to_string()])
            .collect::<Vec<_>>();
        println!(
            "{}",
            render_table(&["Path", "Dependents"], &most_depended_upon, no_color)
        );
    }
    if !data.top_tags.is_empty() {
        println!("\nTop tags:");
        let top_tags = data
            .top_tags
            .iter()
            .map(|row| vec![row.tag.clone(), row.count.to_string()])
            .collect::<Vec<_>>();
        println!("{}", render_table(&["Tag", "Count"], &top_tags, no_color));
    }
    if !data.most_functions.is_empty() {
        println!("\nMost functions:");
        let most_functions = data
            .most_functions
            .iter()
            .map(|row| vec![row.logical_path.clone(), row.count.to_string()])
            .collect::<Vec<_>>();
        println!(
            "{}",
            render_table(&["Path", "Functions"], &most_functions, no_color)
        );
    }
    if !data.checkout_by_os.is_empty() {
        println!("\nCheckouts by OS:");
        let checkout_by_os = data
            .checkout_by_os
            .iter()
            .map(|row| vec![row.os_flavor.clone(), row.count.to_string()])
            .collect::<Vec<_>>();
        println!(
            "{}",
            render_table(&["OS", "Checkouts"], &checkout_by_os, no_color)
        );
    }
    if !data.most_active_checkout_users.is_empty() {
        println!("\nMost active checkout users:");
        let most_active_checkout_users = data
            .most_active_checkout_users
            .iter()
            .map(|row| vec![row.user.clone(), row.count.to_string()])
            .collect::<Vec<_>>();
        println!(
            "{}",
            render_table(
                &["User", "Checkouts"],
                &most_active_checkout_users,
                no_color
            )
        );
    }
    // Both histograms are fixed-bucket (see `bucket_counts`), so they're
    // never structurally empty — check whether anything actually landed in
    // a bucket instead, same "nothing to show" reasoning as the rankings.
    if data.size_histogram.iter().any(|b| b.count > 0) {
        println!("\nScript size distribution:");
        let size_histogram = data
            .size_histogram
            .iter()
            .map(|row| vec![row.label.clone(), row.count.to_string()])
            .collect::<Vec<_>>();
        println!(
            "{}",
            render_table(&["Size", "Scripts"], &size_histogram, no_color)
        );
    }
    if data
        .checkout_staleness_histogram
        .iter()
        .any(|b| b.count > 0)
    {
        println!("\nCheckout staleness distribution:");
        let checkout_staleness_histogram = data
            .checkout_staleness_histogram
            .iter()
            .map(|row| vec![row.label.clone(), row.count.to_string()])
            .collect::<Vec<_>>();
        println!(
            "{}",
            render_table(
                &["Age", "Checkouts"],
                &checkout_staleness_histogram,
                no_color
            )
        );
    }
    if !data.findings_by_check.is_empty() {
        println!("\nAudit findings by check:");
        let findings_by_check = data
            .findings_by_check
            .iter()
            .map(|row| vec![row.check.clone(), row.count.to_string()])
            .collect::<Vec<_>>();
        println!(
            "{}",
            render_table(&["Check", "Findings"], &findings_by_check, no_color)
        );
    }
    if !data.checkout_activity_by_day.is_empty() {
        // A full day-by-day table would be long and isn't very readable as
        // text; the TUI stats view's calendar heatmap is the visual form of
        // this field, so the CLI just summarizes it.
        let busiest = data.checkout_activity_by_day.iter().max_by_key(|d| d.count);
        let busiest_note = busiest
            .map(|b| format!(", busiest {} ({} checkouts)", b.date, b.count))
            .unwrap_or_default();
        println!(
            "\nCheckout activity: {} day(s) with recorded checkouts{busiest_note}",
            data.checkout_activity_by_day.len()
        );
    }
    if let Some(revisions) = &data.revisions {
        println!("\nRevision statistics");
        for line in render_revision_stats_lines(revisions) {
            println!("{line}");
        }
    }
    Ok(())
}

pub fn render_revision_stats_lines(stats: &scat_core::core::search::RevisionStats) -> Vec<String> {
    vec![
        format!(
            "  Scripts with active checkouts: {}",
            stats.scripts_with_active_checkouts
        ),
        format!(
            "  Scripts with archive entries: {}",
            stats.scripts_with_archive_entries
        ),
        format!(
            "  Total DEVELOP revision files: {}",
            stats.total_develop_revision_files
        ),
        format!(
            "  Total ARCHIVE revision files: {}",
            stats.total_archive_revision_files
        ),
        format!(
            "  Scripts with working versions: {}",
            stats.scripts_with_working_versions
        ),
        format!(
            "  Total WORKING revision files: {}",
            stats.total_working_revision_files
        ),
        format!(
            "  Scripts checked out by >1 user: {}",
            stats.scripts_checked_out_by_multiple_users
        ),
    ]
}
