//! short.win services, shared parts (ARCHITECTURE §1.2 `services/common`, §7.1 "Stack"):
//!
//! - [`chain`]: the chain-client abstraction the keeper is generic over ([`Chain`]): clock,
//!   accounts, program accounts, blockhash, simulate, send, recent priority fees, and Jito
//!   bundles (send, pre-flight).
//! - [`rpc`]: its JSON-RPC backend for a real cluster ([`rpc::RpcChain`]). The in-process
//!   LiteSVM backend is TEST-ONLY and lives in `services/keeper-litesvm`.
//! - [`tx`]: transaction building — v1 (SIMD-0385) with a v0 fallback, the compute budget
//!   (`⌈simulated × 1.1⌉`, priority price), sizes and account counts, decompilation, lookup
//!   tables.
//!
//! Host-only: this workspace has its own lockfile (ARCH §1.2) and never enters the SBF build.

pub mod chain;
pub mod rpc;
pub mod tx;

pub use chain::{
    AccountData, BundleOutcome, Chain, ChainClock, InnerIx, Memcmp, TxError, TxOutcome,
};
pub use tx::{ComputeBudget, TxFormat};
