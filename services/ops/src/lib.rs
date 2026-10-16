//! short.win operations (M10 WP-7; ARCH §1.1 "Admin", §3.1, §8.5): **`shortwin-gov`**, the tool
//! that sets up and operates the Squads v4 governance in the owner's layout (owner decisions
//! 2026-10-02, Q4; `governance.example.toml`).
//!
//! | Module | What |
//! |---|---|
//! | [`config`] | the layout file: public keys only; every vault and authority derived; refused when it cannot be right (thresholds, duplicates, time locks, venues, SQ-G8 distinctness) |
//! | [`keys`] | keypair files from the command line only; hardware paths refused; nothing secret ever printed |
//! | [`ixs`] | short.win's admin instructions, the loader's `SetAuthorityChecked` / `Upgrade`, a System transfer; [`ixs::describe`] for reviews |
//! | [`send`] | legacy transactions paid by the acting key: ≤ 1,232 bytes, simulated before sending, failures named by program and Anchor error; `--dry-run` |
//! | [`squads`] | stage (direct, or through a transaction buffer above about 700 bytes of message; an interrupted run's buffer resumed; a duplicate of the newest open transaction refused), approve (`--execute` for the Emergency's two-transaction actions), execute what is stored (co-signers demanded), cancel, close a buffer |
//! | [`verify`] | every PDA and authority against the layout, with a printed report |
//! | [`cli`] | the commands (`create-multisigs`, `init-config`, `stage-upgrade-authority --checked`, `stage-authorities`, `stage-upgrade`, `stage <admin-ix>`, `approve`, `execute`, `cancel`, `close-buffer`, `apply`, `status`, `verify`) |
//!
//! The commands are generic over [`shortwin_services_common::Chain`]: the binary runs them
//! against a JSON-RPC cluster, the end-to-end test (`tests/gov_e2e.rs`) against the mainnet
//! Squads ELF and short.win's release build on LiteSVM, every transaction through the program
//! harness's `Env::send` (the invariant hooks, I4's vault path included).

pub mod cli;
pub mod config;
pub mod ixs;
pub mod keys;
pub mod send;
pub mod squads;
pub mod verify;

pub use cli::run;
pub use config::Layout;
pub use send::{TxFailure, failure};
