//! short.win program tests: admin, config, latch, layout, constraint cases and the harness
//! itself (one binary, so LiteSVM is linked once; the other areas are the sibling binaries,
//! see `tests/litesvm/README.md`).
//!
//! Requires `target/deploy/shortwin.so`: `anchor build --tools-version v1.57 --arch v3`.

// Helpers return LiteSVM's `TransactionResult` (see the harness crate).
#![allow(clippy::result_large_err)]

mod admin;
mod constraints;
mod cu;
mod harness;
mod latch;
mod layout;
