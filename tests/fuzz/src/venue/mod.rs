//! The venues the fuzzer trades on: the mock (test-venue build), the pump curve with its
//! graduation, PumpSwap, Raydium AMM v4 and CPMM (the mainnet-fork fixtures of
//! `shortwin_litesvm_tests`). A [`Venue`] builds the venue-side instructions (swaps that move the
//! price by a factor and their unwind, liquidity moves, admin halts and fee changes) for the
//! world, which sends them through [`Ctx::send`] and so through the invariant hooks.

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use shortwin::venues::ExtrasSel;
use shortwin_litesvm_tests::Env;
use shortwin_litesvm_tests::engine::{MockMarket, VenueMarket};
use shortwin_litesvm_tests::pump::PumpMarket;
use shortwin_litesvm_tests::raydium::RayMarket;
use solana_keypair::Keypair;

use crate::action::Kind;
use crate::stats::Ctx;

pub mod curve;
pub mod mock;
pub mod pswap;
pub mod ray;

/// A venue-side move: `front` alone moves the state; `back` (when the venue can quote it on the
/// pre-state) undoes it as closely as a trade can (a swap selling what the front bought, a
/// deposit of what a withdrawal took). `signer` signs both.
pub struct Move {
    pub front: Instruction,
    pub back: Option<Instruction>,
    pub signer: Keypair,
}

/// A market's venue as the fuzzer drives it.
pub trait Venue {
    /// A short label for logs and crash files (fixture, token program).
    fn label(&self) -> String;

    /// The market's current account lists (ReadSet, TradeExtras): the builders of
    /// `shortwin_litesvm_tests::engine` take it.
    fn vm(&self) -> &dyn VenueMarket;

    /// The mock market (the mock's differential exit oracle runs on it).
    fn as_mock(&self) -> Option<&MockMarket> {
        None
    }

    /// The Raydium market on its adopted pool, AMM v4 or CPMM (`raydium_v4_support`'s
    /// venue-generic differential exit oracle runs on it; M10).
    fn as_ray(&self) -> Option<&RayMarket> {
        None
    }

    /// The curve market before its graduation (the curve's differential exit oracle,
    /// [`crate::curve_exit`], runs on it while the market is on the live curve; M10).
    fn as_curve(&self) -> Option<&PumpMarket> {
        None
    }

    /// The lender that seeded the market and its token account.
    fn seed_lender(&self) -> (Keypair, Pubkey);

    /// A new token account of the market's mint owned by `owner` holding `amount`.
    fn fund_tokens(&mut self, env: &mut Env, owner: &Pubkey, amount: u64) -> Pubkey;

    /// Adds `amount` tokens to `account` (minted, or planted on a mainnet mint).
    fn top_up(&mut self, env: &mut Env, account: &Pubkey, amount: u64);

    /// The pricing reserves `(X tokens, Y lamports)` on the live state (`None` when the venue
    /// cannot be read or priced).
    fn reserves(&self, env: &Env) -> Option<(u64, u64)>;

    /// A swap that moves the spot by `×k_ppm/10⁶` (`up`) or `÷` (down), with its unwind.
    fn push(&mut self, env: &mut Env, up: bool, k_ppm: u64) -> Option<Move>;

    /// An honest swap: a buy of about `lamports` worth of tokens, or a sale of `tokens`.
    fn trade(&mut self, env: &mut Env, buy: bool, size: u64) -> Option<Move>;

    /// A third-party liquidity move of `bps`‱ of the pool: `front` removes, `back` re-adds the
    /// same (LP-wrapped bundles use either order). `None` where the venue has no LP the
    /// fuzzer can move.
    fn lp_pair(
        &mut self,
        _env: &mut Env,
        _bps: u32,
    ) -> Option<(Instruction, Instruction, Keypair)> {
        None
    }

    /// The venue's own halt at `level` (0 restores the normal state). `false`: unsupported.
    fn halt(&mut self, _cx: &mut Ctx, _level: u32) -> bool {
        false
    }

    /// A venue fee change to `level` (0 restores). `false`: unsupported.
    fn fee_shock(&mut self, _cx: &mut Ctx, _level: u32) -> bool {
        false
    }

    /// Venue-specific actions (mock knobs, pool selection, the curve's graduation). `None`:
    /// not a kind this venue runs.
    fn special(&mut self, _cx: &mut Ctx, _kind: Kind, _a: u32, _b: u32) -> Option<()> {
        None
    }

    /// The extras selection an open or a close carries, from the action's number `pick`, and
    /// its trailing accounts (the `VenueExtras` account, then the derived keys). `none`
    /// everywhere but the mock ([`Kind::ExtrasSet`]).
    fn extras(&self, _env: &Env, _pick: u32) -> (ExtrasSel, Vec<AccountMeta>) {
        (ExtrasSel::NONE, Vec::new())
    }

    /// After a crank of the market: re-reads what the market's venue is now (the curve's
    /// `→ PumpSwap` transition, an adopted Raydium pool).
    fn after_crank(&mut self, _cx: &mut Ctx) {}

    /// `sweep_trader` with the venue's token leg.
    fn sweep_ix(&self, env: &Env) -> Instruction;

    /// Venue accounts a donation may target beyond the market's own (the PumpSwap boost ATA).
    fn donation_targets(&self) -> Vec<Pubkey> {
        Vec::new()
    }

    /// Venue token accounts a token donation may target (pump-final §12: a curve's
    /// `curve_base_ata` until the graduation, which pump prices a synthetic exit's pool part on
    /// and `migrate_v2` seeds the pool with), with the amount a donation's number `b` gives.
    fn token_donation(&self, _env: &Env, _b: u32) -> Option<(Pubkey, u64)> {
        None
    }

    /// The compute-unit limit of a keeper exit or a bundle on this venue (`None`: the default).
    fn exit_cu(&self) -> Option<u32> {
        None
    }

    /// The `VenueGuard` indices of the venue's programs (upgrade shocks).
    fn guard_programs(&self) -> Vec<u8> {
        Vec::new()
    }

    /// Writes a new deploy slot into the ProgramData of guard program `index`.
    fn set_deploy_slot(&mut self, _env: &mut Env, _index: u8, _slot: u64) {}

    /// The live deploy slot of guard program `index`.
    fn deploy_slot(&self, _env: &Env, _index: u8) -> Option<u64> {
        None
    }

    /// Clears every venue-side disturbance (halt, fees, knobs) for a wind-down.
    fn restore(&mut self, cx: &mut Ctx);

    /// Whether a venue-side disturbance is active (trades may then fail inside the venue).
    fn disturbed(&self, env: &Env) -> bool;
}

/// `ceil(√(x))` in floating point for price-move sizing (constant product).
pub fn sqrt_f(x: f64) -> f64 {
    x.max(0.0).sqrt()
}

/// The spot factor `k_ppm/10⁶` as an `f64` (at least 1).
pub fn factor(k_ppm: u64) -> f64 {
    (k_ppm as f64 / 1e6).max(1.0)
}
