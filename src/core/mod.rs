/// SQLite schema and query helpers.
pub mod db;
/// Script-level diff: compare active catalog content against vc checkouts or files.
pub mod diff;
/// Path mapping and logical/physical path resolution utilities.
pub mod resolve;
/// vc checkout scanning and warning inference.
pub mod vc;
/// High-level search/query API over the indexed catalog database.
pub mod search;
