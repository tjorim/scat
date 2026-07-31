use std::path::Path;

use anyhow::Result;

use crate::output::print_json;

pub fn cmd_info(
    api: &scat_core::core::search::SearchApi,
    published_path: &Path,
    read_path: &Path,
    json: bool,
) -> Result<()> {
    let meta = api.index_metadata()?;

    if json {
        print_json(&meta);
        return Ok(());
    }

    println!("Last indexed       : {}", meta.build_timestamp);
    println!("Schema version (DB): {}", meta.schema_version);
    println!("Schema version (app): {}", meta.current_schema_version);
    println!("Catalog            : {}", published_path.display());
    // Only worth a line when the two differ: it tells the user this query
    // never touched the network share, and where to look if it went stale.
    if read_path != published_path {
        println!("Local cache        : {}", read_path.display());
    }
    Ok(())
}
