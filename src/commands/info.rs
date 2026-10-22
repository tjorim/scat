use std::path::Path;

use anyhow::Result;
use scat_core::core::db::SchemaCompatibility;
use scat_core::core::search::read_index_metadata;
use scat_core::error::Error;

use crate::output::print_json;

/// Report catalog metadata and whether this scat can read the catalog.
///
/// Unlike every other catalog command this does not require a compatible
/// schema: it is the tool an operator reaches for when something else already
/// failed, and the release smoke check for the compatibility policy. An
/// incompatible catalog is still described in full, then reported as an error
/// (non-zero exit) carrying the same actionable message as every other command.
pub fn cmd_info(published_path: &Path, read_path: &Path, json: bool) -> Result<()> {
    let meta = read_index_metadata(read_path)?;

    if json {
        print_json(&meta);
    } else {
        println!("Last indexed       : {}", meta.build_timestamp);
        println!("Schema version (DB): {}", meta.schema_version);
        println!("Schema version (app): {}", meta.current_schema_version);
        println!("Catalog            : {}", published_path.display());
        // Only worth a line when the two differ: it tells the user this query
        // never touched the network share, and where to look if it went stale.
        if read_path != published_path {
            println!("Local cache        : {}", read_path.display());
        }
        if let Some(compat) = meta.compatibility {
            println!(
                "Compatibility      : {}",
                match compat {
                    SchemaCompatibility::Compatible => "compatible",
                    SchemaCompatibility::CatalogOlder => "catalog is older than this scat",
                    SchemaCompatibility::CatalogNewer => "catalog is newer than this scat",
                }
            );
        }
    }

    match (meta.compatibility, meta.schema_version.as_i64()) {
        (Some(SchemaCompatibility::Compatible), _) => Ok(()),
        (Some(_), Some(found)) => Err(Error::SchemaMismatch {
            found,
            expected: meta.current_schema_version,
        }
        .into()),
        _ => Err(Error::Validation(format!(
            "{} has no catalog metadata — not a scat catalog, or published by a much older scat",
            published_path.display()
        ))
        .into()),
    }
}
