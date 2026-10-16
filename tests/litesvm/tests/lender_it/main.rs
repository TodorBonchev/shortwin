//! Lender-track program tests (ARCH §3.3, §8.2, §8.3; M1 exit criteria): `deposit`,
//! `withdraw`, `claim_sol`, `cover_deficit`, `sync_lender`, `sweep_protocol_fees` on LiteSVM,
//! with the invariants I1, I2, I4, I8 (and I17) after every transaction.
//!
//! - [`common`]: the [`common::Lab`] — one list-only market, lenders that act through the
//!   program, and the borrower side (borrow, rent credit, repay, settle, sweeps) injected into
//!   the accounts with the same `shortwin_math::pool` functions the M2 instructions will call.
//!   Every lender instruction is also checked against the math crate on its pre-state (the
//!   exact composition the handler documents), to the lamport and the share.
//! - [`flows`]: each instruction, both token programs, dust and zero amounts, constraints and
//!   refusals, the prologue (params sync, rent accrual), events, CU.
//! - [`hooks`]: the lender hooks catching what the fidelity review's mutants would leave
//!   (M1-FID-1, M1-FID-4, M1-FID-9, M1-SEC-3).
//! - [`pot`]: T-POT (ARCH §8.2).
//! - [`adversarial`]: the lender cases of ARCH §8.2 / M1 (C0-6, C0-9, C0-13, R3A-2, R3A-10,
//!   R3B-1, R3B-2, V2-2, V2-12).
//! - [`security`]: the M1 security review's lender findings (M1-SEC-2 `min_sol`, M1-SEC-3
//!   the last shares stay while tokens are borrowed).
//! - [`traces`]: `pool_model` traces (`crates/shortwin-math/tests/vectors/pool`) replayed
//!   through the program: the lock-heavy mix, the R3A-2 stranding seeds, the lazy / runs /
//!   dust profiles and the ECON §9.6 fuzz runs.
//!
//! Expected numbers come from `docs/design/sim/pool_model.py` v0.4.2 (each test names the
//! function). Requires the SBF build: `anchor build --tools-version v1.57 --arch v3`.

#![allow(clippy::result_large_err)]

mod adversarial;
mod common;
mod flows;
mod hooks;
mod pot;
mod security;
mod traces;
