//! TEST-ONLY helpers (feature `embedded-pg`; INDEXER.md §11). FOUNDATION.
//!
//! - [`pg`]: a PostgreSQL server in user space (no Docker, no root), with fresh migrated
//!   databases for each test.

pub mod pg;
