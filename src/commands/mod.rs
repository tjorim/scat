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

pub(crate) use audit::cmd_audit;
pub(crate) use deps::cmd_deps;
pub(crate) use diff::{cmd_diff, cmd_script_diff_catalog, cmd_script_diff_explicit};
pub(crate) use index::cmd_index;
pub(crate) use info::cmd_info;
pub(crate) use search::{SearchOpts, cmd_search, query_uses_fts, sort_by_name_relevance};
pub(crate) use show::cmd_show;
pub(crate) use stats::cmd_stats;
pub(crate) use status::cmd_status;
pub(crate) use symlinks::cmd_symlinks;

#[cfg(test)]
mod tests;
