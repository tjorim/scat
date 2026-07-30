mod audit;
mod deps;
mod diff;
mod index;
mod info;
mod search;
mod show;
mod stats;
mod status;
mod symlinks;

pub use audit::cmd_audit;
pub use deps::cmd_deps;
pub use diff::{cmd_diff, cmd_script_diff_catalog, cmd_script_diff_explicit};
pub use index::cmd_index;
pub use info::cmd_info;
pub use search::{SearchOpts, cmd_search, query_uses_fts, sort_by_name_relevance};
pub use show::cmd_show;
pub use stats::cmd_stats;
pub use status::cmd_status;
pub use symlinks::cmd_symlinks;

#[cfg(test)]
mod tests;
