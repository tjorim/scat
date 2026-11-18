mod audit;
mod deps;
mod diff;
mod index;
mod info;
mod search;
mod show;
mod similar;
mod stats;
mod status;
mod symlinks;

pub use audit::{AuditOpts, cmd_audit};
pub use deps::cmd_deps;
pub use diff::{ScriptDiffOpts, cmd_diff, cmd_script_diff_catalog, cmd_script_diff_explicit};
pub use index::{IndexOpts, cmd_index};
pub use info::cmd_info;
#[cfg(test)]
pub use search::collect_results;
pub use search::{SearchOpts, cmd_search, search_text};
pub use show::cmd_show;
pub use similar::cmd_similar;
pub use similar::load_similar;
pub use stats::cmd_stats;
pub use status::cmd_status;
pub use symlinks::cmd_symlinks;

#[cfg(test)]
mod tests;
