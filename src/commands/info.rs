use anyhow::Result;

use crate::output::print_json;

pub(crate) fn cmd_info(api: &scat_core::core::search::SearchApi, json: bool) -> Result<()> {
    let meta = api.index_metadata()?;

    if json {
        print_json(&meta);
        return Ok(());
    }

    println!("Last indexed       : {}", meta.build_timestamp);
    println!("Schema version (DB): {}", meta.schema_version);
    println!("Schema version (app): {}", meta.current_schema_version);
    Ok(())
}
