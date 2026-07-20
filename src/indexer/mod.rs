/// Python AST dependency extraction.
pub mod ast_deps;
/// Atomic file replacement and history copy rotation.
pub mod atomic;
/// End-to-end indexing pipeline orchestration.
pub mod builder;
pub mod checkpoint;
/// Metadata extraction from script headers and docstrings.
pub mod extractor;
/// Live multi-line directory tree view for interactive scan progress.
mod scan_tree;
/// File-system scanner and language detection.
pub mod scanner;
/// Tree-sitter based dependency extraction for Python and shell.
pub mod treesitter_deps;
