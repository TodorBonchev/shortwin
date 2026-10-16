//! short.win protocol math (ARCHITECTURE §2.2; ECONOMICS §2).
//!
//! Shared bit-for-bit by the program, the keeper and the WASM frontend. `no_std`, zero
//! runtime dependencies, checked arithmetic. Fallible functions return
//! `Result<_, MathError>`, or `Option` where the binding API of ARCH §2.2 says a value
//! is undefined (quotes). Rounding always favours the pool and lenders (ECON §2; see
//! [`num::Rounding`]). Normative references: the ECON formulas, `sim/pool_model.py` v0.4.1
//! and `sim/shortwin_sim.py` v0.4.1 with their default switches (ARCH §8.1).
#![no_std]
#![forbid(unsafe_code)]
#![deny(
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::unwrap_used
)]

#[cfg(test)]
extern crate std;

pub mod error;
pub mod num;

pub mod bounds;
pub mod caps;
pub mod consts;
pub mod fees;
pub mod observe;
pub mod params;
pub mod persist;
pub mod pool;
pub mod pool_select;
pub mod quote;
pub mod rent;
pub mod risk;
pub mod settle;
pub mod tier;
pub mod waterfall;

pub use error::MathError;
pub use num::{Rounding, U256};

/// Version of this crate, reported by the WASM facade.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
