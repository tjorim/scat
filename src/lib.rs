//! Core library for `scat`, a script cataloging and search engine.
//!
//! Public modules:
//! - [`core`]: database, query, and path resolution primitives.
//! - [`indexer`]: scanning, metadata extraction, and dependency indexing.
//! - [`error`]: shared error and [`error::Result`] types.

#![warn(missing_docs)]

/// Core database, query, and path-resolution primitives.
pub mod core;
/// Shared error types for library and CLI operations.
pub mod error;
/// Indexing pipeline components: scan, extract, and persist metadata.
pub mod indexer;
