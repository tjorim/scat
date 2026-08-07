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
