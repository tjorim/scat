/// SQLite schema and query helpers.
pub mod db;
/// Script-level diff: compare active catalog content against vc checkouts or files.
pub mod diff;
/// Path mapping and logical/physical path resolution utilities.
pub mod resolve;
/// Typed, read-only view over a queried `scripts` table row.
pub mod script_view;
/// High-level search/query API over the indexed catalog database.
pub mod search;
/// vc checkout scanning and warning inference.
pub mod vc;
