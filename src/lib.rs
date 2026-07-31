//! Core library for `scat`, a script cataloging and search engine.
//!
//! Public modules:
//! - [`core`]: database, query, and path resolution primitives.
//! - [`indexer`]: scanning, metadata extraction, and dependency indexing.
//! - [`error`]: shared error and [`error::Result`] types.

#![warn(missing_docs)]

// scat targets Linux clients only. The catalog is published to a network
// share and cached into tmpfs on each host (see `core::cache`), and the
// indexer relies on POSIX rename semantics to swap a new build in underneath
// open readers — neither has a Windows equivalent worth carrying, and every
// deployment has been RHEL-only since the Windows clients were retired.
// Development on other Unixes still builds; only the tmpfs cache silently
// falls back to reading the catalog in place when `/dev/shm` is absent.
#[cfg(not(unix))]
compile_error!("scat supports Linux only; Windows support was removed (see issue #59)");

/// Core database, query, and path-resolution primitives.
pub mod core;
/// Shared error types for library and CLI operations.
pub mod error;
/// Indexing pipeline components: scan, extract, and persist metadata.
pub mod indexer;
