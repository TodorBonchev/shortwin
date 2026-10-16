//! **M7 keeper economics** (ARCH §9 M7 "keeper cost measurement … feeding back into ECON §5.8";
//! §8.2 T-SIZE "the number of cranks per batch"; ECON §5.8's cost-model inputs): the real keeper,
//! driven through its own transactions on every venue, measured.
//!
//! - **CU per instruction and venue** (`venue_*`): the keeper's `crank_price` (a steady roll and
//!   record, alone in its transaction), its `liquidate` after a genuine pump (4 % a second), its
//!   `expire` at a healthy position's deadline, and its `sol_settle` calls after an S1 gap (×4 in
//!   one trade: the arming and keeping calls, and the settling one) — each with the compute units
//!   consumed, the limit requested (`⌈simulated × 1.1⌉`, what a leader charges against the
//!   per-account block budget and what the priority fee is paid on) and the transaction's size —
//!   on the mock (test-venue build), Raydium AMM v4 (Fartcoin), CPMM (USELESS), PumpSwap (the
//!   `mature` pool) and the pump curve (the `fresh` coin).
//! - **Batches** (`batch_capacity`, K6): the most `crank_price` instructions of one venue — and of
//!   a mix of the four real venues — that fit a v1 transaction (4,096 B, 64 addresses) and a v0
//!   transaction through the protocol ALT (1,232 B), with the keeper's own builder and size check
//!   (`shortwin_services_common::tx`) on the real per-market account shapes (market, mint and the
//!   ReadSet's per-market keys fresh per market; the shared keys shared), with the priority price
//!   set (both ComputeBudget instructions in v0); cross-checked against the batches the keeper
//!   actually sent.
//! - **Cadence** (`rates`): cranks per market-day under the targeted coverage (the keeper's
//!   own cadence, venue-independent): quiet (a healthy position: every 30 s), near a threshold
//!   (every second), an armed position (one `sol_settle` a second), one foreign price deviation
//!   (a per-slot burst), a genuine 10 % move (`|P_spot/P_ema − 1| > 2 %`), one isolated
//!   LP-wrapped crank on CPMM (liquidity-anomaly coverage), a PumpSwap listing's warm-up.
//! - **Load** (`load`, K8): 50 and 200 covered mock markets, quiet and all due every second:
//!   wall time per tick on the in-process backend, backend calls per tick, batches per tick,
//!   whether every due market was observed in its second.
//!
//! The costs per market-day at 0 / 1,000 / 10,000 / 100,000 µlamports/CU follow exactly from these
//! (5,000 lamports per signature + `⌈limit × price / 10⁶⌉`), unbatched and batched; `M7_COST_WRITE=1`
//! writes the machine-readable `tests/bench/m7_keeper/results.json` and `results.md` for ECON §5.8
//! and `docs/design/sim/keeper_cost.py`; `M7_COST_FULL=1` runs the quiet cadence for an hour
//! (default 15 minutes; the cadence is periodic, so the day's figure is the same).

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::time::Instant;

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::Instruction;
use shortwin::events::SolSettle;
use shortwin_keeper::coverage::Due;
use shortwin_keeper::{ExitResult, events};
use shortwin_keeper_litesvm::Rpc;
use shortwin_litesvm_tests::engine::{VenueMarket, crank_price_ix};
use shortwin_litesvm_tests::fork::{self, AnyPool, PumpCoin, PumpCurve, Swap};
use shortwin_litesvm_tests::pamm::{self, PammCoin, PammUser};
use shortwin_litesvm_tests::pump::{CurveUser, PumpSpec};
use shortwin_math::fees::Direction;
use shortwin_services_common::tx::{self, ComputeBudget, TxFormat};
use solana_keypair::Keypair;
use solana_message::AddressLookupTableAccount;

use crate::common::*;
use crate::raydium::{FART, USELESS, ray_setup};

const SOL: u64 = 1_000_000_000;
/// ECON §5.8's priority-price scenarios, µlamports per CU (O17's ceiling last).
const PRICES: [u64; 4] = [0, 1_000, 10_000, 100_000];
const BASE_FEE: u64 = 5_000;
const DAY: u64 = 86_400;

// ---- facts ------------------------------------------------------------------------------------

/// One landed transaction of the keeper.
#[derive(Clone, Copy, Debug, Default)]
struct Tx {
    units: u64,
    limit: u64,
    bytes: u64,
    sigs: u64,
}

impl Tx {
    /// Lamports at `price` µlamports/CU: the base fee per signature plus `⌈limit × price/10⁶⌉`.
    fn lamports(&self, price: u64) -> u64 {
        self.sigs * BASE_FEE
            + (u128::from(self.limit) * u128::from(price)).div_ceil(1_000_000) as u64
    }
}

/// The transactions of one kind.
#[derive(Clone, Debug, Default)]
struct Kind(Vec<Tx>);

impl Kind {
    fn range(&self, f: impl Fn(&Tx) -> u64) -> (u64, u64) {
        let v: Vec<u64> = self.0.iter().map(f).collect();
        (
            v.iter().copied().min().unwrap_or(0),
            v.iter().copied().max().unwrap_or(0),
        )
    }

    /// The costliest transaction (the largest limit).
    fn worst(&self) -> Tx {
        self.0
            .iter()
            .copied()
            .max_by_key(|t| t.limit)
            .unwrap_or_default()
    }

    fn json(&self) -> String {
        let (cl, ch) = self.range(|t| t.units);
        let (rl, rh) = self.range(|t| t.limit);
        let (bl, bh) = self.range(|t| t.bytes);
        format!(
            "{{\"n\": {}, \"consumed_cu\": [{cl}, {ch}], \"requested_cu\": [{rl}, {rh}], \"bytes\": [{bl}, {bh}]}}",
            self.0.len()
        )
    }

    fn md(&self) -> String {
        if self.0.is_empty() {
            return "—".into();
        }
        let span = |(a, b): (u64, u64)| {
            if a == b {
                sep(a)
            } else {
                format!("{}–{}", sep(a), sep(b))
            }
        };
        format!(
            "{} / {} CU, {} B",
            span(self.range(|t| t.units)),
            span(self.range(|t| t.limit)),
            span(self.range(|t| t.bytes))
        )
    }
}

/// The bench's keeper configuration: the defaults, every market with positions covered. The
/// bench measures the cadence and cost of a **covered** market (ECON §5.8's per-market inputs);
/// its positions are below the global budget's 1-SOL value-at-risk floor (owner decision A of
/// 2026-10-01), where a market would get the idle cadence only (`common::covering_all`).
fn bench_cfg() -> KeeperConfig {
    covering_all(KeeperConfig::default())
}

/// A number with thousands separators.
fn sep(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// The keeper's transactions per kind on one venue.
#[derive(Clone, Debug, Default)]
struct VenueCu {
    crank: Kind,
    liquidate: Kind,
    expire: Kind,
    /// `sol_settle` calls that armed or kept the clock (no settlement).
    settle_keep: Kind,
    /// The `sol_settle` that settled (S1).
    settle_exec: Kind,
}

impl VenueCu {
    /// Books the keeper's landed transactions of `r`: single-market crank batches, exits.
    fn absorb(&mut self, r: &TickReport) {
        for b in r.batches.iter().filter(|b| b.markets.len() == 1) {
            self.crank.0.push(Tx {
                units: b.units,
                limit: u64::from(b.unit_limit),
                bytes: b.bytes as u64,
                sigs: 1,
            });
        }
        for e in &r.exits {
            let ExitResult::Landed {
                outcome,
                unit_limit,
                bytes,
                ..
            } = &e.result
            else {
                continue;
            };
            let t = Tx {
                units: outcome.units_consumed,
                limit: u64::from(*unit_limit),
                bytes: *bytes as u64,
                sigs: 1,
            };
            match e.kind {
                ExitKind::Liquidate => self.liquidate.0.push(t),
                ExitKind::Expire => self.expire.0.push(t),
                ExitKind::SolSettle if events::of::<SolSettle>(outcome).is_empty() => {
                    self.settle_keep.0.push(t)
                }
                ExitKind::SolSettle => self.settle_exec.0.push(t),
            }
        }
    }

    fn json(&self) -> String {
        format!(
            "{{\"crank\": {}, \"liquidate\": {}, \"expire\": {}, \"sol_settle_keep\": {}, \"sol_settle_settle\": {}}}",
            self.crank.json(),
            self.liquidate.json(),
            self.expire.json(),
            self.settle_keep.json(),
            self.settle_exec.json()
        )
    }
}

// ---- venue drivers ----------------------------------------------------------------------------

/// Multiplies a venue's spot by `num/den` with a whale's trade (state kept across calls).
type Pump<'a> = Box<dyn FnMut(&mut Env, u128, u128) + 'a>;

/// Opens `q` for a new borrower after a crank (must succeed).
fn open_on(h: &Harness, m: &impl VenueMarket, q: u64) -> (Borrower, u64) {
    h.with_env(|env| {
        env.crank(m);
        env.warp(1);
        let mut b = env.borrower();
        let (res, nonce) = env.open(m, &mut b, q);
        ok(res);
        (b, nonce)
    })
}

fn landed_kind(r: &TickReport, kind: ExitKind, pos: &Pubkey) -> bool {
    landed_for(r, kind, pos).next().is_some()
}

/// Quiet for `quiet_s` seconds (the steady crank), then a genuine pump of 4 % a second until the
/// keeper's liquidation lands.
fn run_liquidation(
    mut h: Harness,
    m: &impl VenueMarket,
    q: u64,
    mut pump: Pump<'_>,
    cu: &mut VenueCu,
) {
    let (b, nonce) = open_on(&h, m, q);
    let pos = shortwin_litesvm_tests::pda::position(&m.market(), &b.key(), nonce).0;
    for _ in 0..65 {
        let r = h.second().clone();
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        cu.absorb(&r);
    }
    for s in 0..600 {
        h.with_env(|env| {
            env.warp(1);
            pump(env, 104, 100);
        });
        let r = h.tick().clone();
        assert!(r.errors.is_empty(), "second {s}: {:?}", r.errors);
        cu.absorb(&r);
        if landed_kind(&r, ExitKind::Liquidate, &pos) {
            return;
        }
    }
    panic!("no liquidation");
}

/// A healthy position observed to its deadline and expired there.
fn run_expiry(mut h: Harness, m: &impl VenueMarket, q: u64, cu: &mut VenueCu) {
    let (b, nonce) = open_on(&h, m, q);
    let pos = shortwin_litesvm_tests::pda::position(&m.market(), &b.key(), nonce).0;
    let deadline = h.with_env(|env| env.position(m, &b.key(), nonce).deadline);
    h.seconds(3);
    let wait = deadline - 90 - h.now();
    h.idle(wait);
    for s in 0..300 {
        let r = h.second().clone();
        assert!(r.errors.is_empty(), "second {s}: {:?}", r.errors);
        cu.absorb(&r);
        if landed_kind(&r, ExitKind::Expire, &pos) {
            return;
        }
    }
    panic!("no expiry");
}

/// An S1 gap (×4 in one trade, held): the keeper's `sol_settle` every second until it settles.
/// Returns the number of `sol_settle` calls.
fn run_settlement(
    mut h: Harness,
    m: &impl VenueMarket,
    q: u64,
    mut pump: Pump<'_>,
    cu: &mut VenueCu,
) -> u64 {
    let (b, nonce) = open_on(&h, m, q);
    let pos = shortwin_litesvm_tests::pda::position(&m.market(), &b.key(), nonce).0;
    h.seconds(3);
    h.with_env(|env| pump(env, 4, 1));
    let mut calls = 0;
    for s in 0..600 {
        let r = h.second().clone();
        assert!(r.errors.is_empty(), "second {s}: {:?}", r.errors);
        cu.absorb(&r);
        calls += landed_for(&r, ExitKind::SolSettle, &pos).count() as u64;
        if r.exits.iter().any(|e| {
            e.position == pos
                && e.result
                    .landed()
                    .is_some_and(|o| !events::of::<SolSettle>(o).is_empty())
        }) {
            return calls;
        }
        assert!(
            !landed_kind(&r, ExitKind::Liquidate, &pos),
            "the gap was bought back: not an S1 gap"
        );
    }
    panic!("no settlement");
}

fn mock_pump(mm: &MockMarket) -> Pump<'_> {
    let mut whale = None;
    Box::new(move |env, num, den| {
        let w = whale.get_or_insert_with(|| env.actor(mm, 0, 50_000 * SOL));
        let target = env.spot(mm) * num / den;
        env.push_price(mm, w, target);
    })
}

/// A Raydium whale's buy that lifts the spot `×num/den` (constant product; the fee grossed up).
fn ray_pump(pool: AnyPool) -> Pump<'static> {
    let mut whale = None;
    Box::new(move |env, num, den| {
        let (y, _) = pool
            .io_reserves(&env.svm, Direction::Buy)
            .expect("reserves");
        let w = whale.get_or_insert_with(|| {
            let (tok, program) = pool.token();
            fork::trader(env, &tok, program, y.saturating_mul(20), 0)
        });
        let f = (num as f64 / den as f64).sqrt();
        let a = ((y as f64) * (f - 1.0) / 0.99).ceil() as u64;
        ok(fork::swap(
            env,
            &pool,
            w,
            Direction::Buy,
            Swap::ExactIn(a, 0),
        ));
    })
}

fn pamm_pump(pool: pamm::PammPool) -> Pump<'static> {
    let mut whale = None;
    Box::new(move |env, num, den| {
        let w = whale.get_or_insert_with(|| PammUser::new(env, &pool, 50_000 * SOL));
        ok(w.push_price(env, &pool, num, den));
    })
}

/// A curve buy that lifts the spot `×num/den`: the virtual token reserve falls by `√(den/num)`.
fn curve_pump(c: PumpCurve) -> Pump<'static> {
    let mut user = None;
    Box::new(move |env, num, den| {
        let u = user.get_or_insert_with(|| CurveUser::new(env, &c));
        let vt = c.state(&env.svm).virtual_token_reserves;
        let keep = (vt as f64) * (den as f64 / num as f64).sqrt();
        let q = vt.saturating_sub(keep.ceil() as u64);
        ok(u.buy(env, &c, q, u64::MAX));
    })
}

/// The mock's CU facts (a fresh market per run).
fn venue_mock() -> VenueCu {
    let mut cu = VenueCu::default();
    let (h, mms) = setup(1, bench_cfg());
    let pump = mock_pump(&mms[0]);
    run_liquidation(h, &mms[0], Q, pump, &mut cu);
    let (h, mms) = setup(1, bench_cfg());
    run_expiry(h, &mms[0], Q, &mut cu);
    let (h, mms) = setup(1, bench_cfg());
    let pump = mock_pump(&mms[0]);
    run_settlement(h, &mms[0], Q, pump, &mut cu);
    cu
}

fn venue_raydium(fixture: shortwin_litesvm_tests::raydium::RayFixture) -> VenueCu {
    let mut cu = VenueCu::default();
    let (h, mut ms) = ray_setup(&[fixture], bench_cfg());
    let (rm, q) = ms.remove(0);
    let pump = ray_pump(rm.pool);
    run_liquidation(h, &rm, q, pump, &mut cu);
    let (h, mut ms) = ray_setup(&[fixture], bench_cfg());
    let (rm, q) = ms.remove(0);
    run_expiry(h, &rm, q, &mut cu);
    let (h, mut ms) = ray_setup(&[fixture], bench_cfg());
    let (rm, q) = ms.remove(0);
    let pump = ray_pump(rm.pool);
    run_settlement(h, &rm, q, pump, &mut cu);
    cu
}

/// A PumpSwap market on the `mature` pool, warmed up by the harness (the keeper's warm-up duty is
/// measured in `rates`).
fn pamm_setup() -> (Harness, pamm::PammMarket, u64) {
    let (mut env, p) = Env::fork_pamm_coin(PammCoin::Mature);
    let r = p.reserves(&env.svm).expect("reserves");
    let base = u128::from(r.base);
    let q = u64::try_from((base / 100).min(base * u128::from(10 * SOL) / r.effective_quote.max(1)))
        .expect("u64");
    ok(env.list_pamm(&p.mint, &p.base_program));
    let pm = env.pamm_market_listed(&p.mint, q * 40, 60);
    (Harness::new(env, bench_cfg()), pm, q / 3)
}

fn venue_pamm() -> VenueCu {
    let mut cu = VenueCu::default();
    let (h, pm, q) = pamm_setup();
    let pump = pamm_pump(pm.pool);
    run_liquidation(h, &pm, q, pump, &mut cu);
    let (h, pm, q) = pamm_setup();
    run_expiry(h, &pm, q, &mut cu);
    let (h, pm, q) = pamm_setup();
    let pump = pamm_pump(pm.pool);
    run_settlement(h, &pm, q, pump, &mut cu);
    cu
}

fn curve_setup() -> (Harness, shortwin_litesvm_tests::pump::PumpMarket, u64) {
    let (mut env, c) = Env::fork_pump_coin(PumpCoin::Fresh);
    // The fresh curve holds no real SOL yet (`rQ` is one lamport): a normal user's buys first, so
    // it can pay for a sale (as `pump_it`).
    let u = CurveUser::new(&mut env, &c);
    let vt = c.state(&env.svm).virtual_token_reserves;
    ok(u.buy(&mut env, &c, vt / 7, u64::MAX));
    ok(env.list_pump(&c));
    let q = c.state(&env.svm).virtual_token_reserves / 200;
    let pm = env.pump_market_listed(PumpSpec::new(PumpCoin::Fresh, q * 40), c);
    (Harness::new(env, bench_cfg()), pm, q)
}

fn venue_curve() -> VenueCu {
    let mut cu = VenueCu::default();
    let (h, pm, q) = curve_setup();
    let pump = curve_pump(pm.curve);
    run_liquidation(h, &pm, q, pump, &mut cu);
    let (h, pm, q) = curve_setup();
    run_expiry(h, &pm, q, &mut cu);
    let (h, pm, q) = curve_setup();
    let pump = curve_pump(pm.curve);
    run_settlement(h, &pm, q, pump, &mut cu);
    cu
}

// ---- batch capacity (K6) ----------------------------------------------------------------------

/// A venue's `crank_price` shape: the template instruction of a real market and which of its
/// account positions are per market (the rest — config, the oracle cache, the event authority,
/// the program, the venue's shared statics — are the same for every market).
struct CrankShape {
    name: &'static str,
    template: Instruction,
    per_market: Vec<usize>,
}

impl CrankShape {
    /// `crank_price`'s accounts: `[config, market, mint, oracle_cache, event_authority, program]`,
    /// then the ReadSet; `shared_read` are the ReadSet positions shared by every market.
    fn new(name: &'static str, template: Instruction, shared_read: &[usize]) -> Self {
        let n = template.accounts.len();
        let mut per_market = vec![1, 2];
        per_market.extend((6..n).filter(|i| !shared_read.contains(&(i - 6))));
        Self {
            name,
            template,
            per_market,
        }
    }

    /// A crank of another market of the venue: the per-market keys fresh.
    fn instance(&self) -> Instruction {
        let mut ix = self.template.clone();
        for &i in &self.per_market {
            ix.accounts[i].pubkey = Pubkey::new_unique();
        }
        ix
    }
}

/// The most of `ixs` (a prefix) that fit one `format` transaction built by the keeper's own
/// builder with a priority price (v0: both ComputeBudget instructions), through `alts`.
fn capacity(
    format: TxFormat,
    payer: &Keypair,
    ixs: &[Instruction],
    alts: &[AddressLookupTableAccount],
) -> (usize, usize, usize) {
    let budget = ComputeBudget {
        unit_limit: tx::MAX_COMPUTE_UNITS,
        unit_price: 1,
    };
    let hash = solana_hash::Hash::default();
    let mut best = (0, 0, 0);
    for k in 1..=ixs.len() {
        let Ok(t) = tx::build(format, payer, &[], &ixs[..k], &budget, hash, alts) else {
            break;
        };
        if !tx::fits(format, &t) {
            break;
        }
        best = (k, tx::size(&t), tx::account_count(&t));
    }
    best
}

/// The protocol ALT's static part (A28, `shortwin-build-alt print`).
fn protocol_alt() -> AddressLookupTableAccount {
    AddressLookupTableAccount {
        key: Pubkey::new_unique(),
        addresses: shortwin_keeper::alt::static_entries()
            .into_iter()
            .map(|e| e.key)
            .collect(),
    }
}

/// Per-market tables for `ixs` (`shortwin-build-alt market`'s fallback, OWNER-M7-1): each
/// instruction's accounts that `protocol` lacks, program ids excluded — one table per market.
fn market_tables(
    ixs: &[Instruction],
    protocol: &AddressLookupTableAccount,
) -> Vec<AddressLookupTableAccount> {
    ixs.iter()
        .map(|ix| {
            let mut addresses: Vec<Pubkey> = Vec::new();
            for m in &ix.accounts {
                if m.pubkey != shortwin::ID
                    && !protocol.addresses.contains(&m.pubkey)
                    && !addresses.contains(&m.pubkey)
                {
                    addresses.push(m.pubkey);
                }
            }
            AddressLookupTableAccount {
                key: Pubkey::new_unique(),
                addresses,
            }
        })
        .collect()
}

#[derive(Clone, Debug, Default)]
struct Capacity {
    name: &'static str,
    v1: (usize, usize, usize),
    v0_alt: (usize, usize, usize),
    /// v0 through the protocol ALT and one table per market.
    v0_market_alts: (usize, usize, usize),
    v0_bare: (usize, usize, usize),
    /// A v1 batch of 6 (the keeper's default): bytes, accounts.
    v1_six: (usize, usize),
}

fn capacities(shapes: &[CrankShape]) -> Vec<Capacity> {
    let payer = Keypair::new();
    let alt = [protocol_alt()];
    let mut out = Vec::new();
    let of = |ixs: &[Instruction], name| {
        let v1 = capacity(TxFormat::V1, &payer, ixs, &[]);
        let six = capacity(TxFormat::V1, &payer, &ixs[..6], &[]);
        let mut with_markets = alt.to_vec();
        with_markets.extend(market_tables(ixs, &alt[0]));
        Capacity {
            name,
            v1,
            v0_alt: capacity(TxFormat::V0, &payer, ixs, &alt),
            v0_market_alts: capacity(TxFormat::V0, &payer, ixs, &with_markets),
            v0_bare: capacity(TxFormat::V0, &payer, ixs, &[]),
            v1_six: (six.1, six.2),
        }
    };
    for s in shapes {
        let ixs: Vec<Instruction> = (0..16).map(|_| s.instance()).collect();
        out.push(of(&ixs, s.name));
    }
    // A mix of the four real venues, round robin.
    let real: Vec<&CrankShape> = shapes.iter().filter(|s| s.name != "mock").collect();
    let ixs: Vec<Instruction> = (0..16).map(|i| real[i % real.len()].instance()).collect();
    out.push(of(&ixs, "mixed (AMM v4, CPMM, PumpSwap, curve)"));
    out
}

/// The real venues' crank shapes, from listed markets of the forks and the mock.
fn crank_shapes() -> Vec<CrankShape> {
    let mut v = Vec::new();
    let (h, mms) = setup(1, bench_cfg());
    drop(h);
    v.push(CrankShape::new("mock", crank_price_ix(&mms[0]), &[]));
    let (h, ms) = ray_setup(&[FART, USELESS], bench_cfg());
    drop(h);
    v.push(CrankShape::new(
        "raydium_amm_v4",
        crank_price_ix(&ms[0].0),
        &[],
    ));
    // CPMM: the AmmConfig (ReadSet 1) is shared by every pool on it.
    v.push(CrankShape::new(
        "raydium_cpmm",
        crank_price_ix(&ms[1].0),
        &[1],
    ));
    let (_h, pm, _) = pamm_setup();
    // PumpSwap: P3 GlobalConfig and P4 FeeConfig are the program's statics.
    v.push(CrankShape::new("pumpswap", crank_price_ix(&pm), &[3, 4]));
    let (_h, pm, _) = curve_setup();
    // The curve: Global, pump's FeeConfig, pAMM's FeeConfig and GlobalConfig.
    v.push(CrankShape::new(
        "pump_curve",
        crank_price_ix(&pm),
        &[1, 2, 5, 6],
    ));
    v
}

// ---- cadence (rates) --------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
struct Rates {
    quiet_s: u64,
    quiet_cranks: u64,
    near_s: u64,
    near_cranks: u64,
    armed_settle_calls: u64,
    burst_cranks: u64,
    burst_txs: u64,
    ema_move_cranks: u64,
    liq_trigger_extra: i64,
    pamm_warmup_cranks: u64,
}

fn cranks_of(r: &TickReport, key: &Pubkey) -> u64 {
    r.batches.iter().filter(|b| b.markets.contains(key)).count() as u64
}

fn full() -> bool {
    std::env::var_os("M7_COST_FULL").is_some()
}

fn rates() -> Rates {
    let mut out = Rates::default();
    // Quiet: one healthy position, a tick a second.
    let (mut h, mms) = setup(1, bench_cfg());
    let key = mms[0].market();
    let _ = open(&h, &mms[0], Q);
    h.seconds(2);
    out.quiet_s = if full() { 3_600 } else { 900 };
    for _ in 0..out.quiet_s {
        let r = h.second();
        out.quiet_cranks += cranks_of(r, &key);
        assert!(r.errors.is_empty());
    }
    assert_eq!(h.keeper.metrics.markets[&key].misses, 0);

    // Near a threshold: the price lifted until the position is near (live CR below 1.3·L) but
    // not liquidatable, then held; every second is observed.
    let (mut h, mms) = setup(1, bench_cfg());
    let mm = &mms[0];
    let key = mm.market();
    let _ = open(&h, mm, Q);
    let mut pump = mock_pump(mm);
    h.seconds(2);
    for _ in 0..200 {
        let near = h
            .reports
            .last()
            .and_then(|r| r.evaluations.get(&key))
            .is_some_and(|e| e.iter().any(|e| e.near));
        if near {
            break;
        }
        h.with_env(|env| pump(env, 101, 100));
        h.second();
    }
    h.seconds(120);
    out.near_s = 600;
    for _ in 0..out.near_s {
        let r = h.second().clone();
        assert!(
            r.exits.iter().all(|e| e.result.landed().is_none()),
            "held near, never liquidated: {:?}",
            r.exits
        );
        out.near_cranks += cranks_of(&r, &key);
    }

    // One foreign price deviation: `[buy 3 %, crank_price, sell]` in one transaction, then the
    // keeper ticks every slot (2.5 a second on mainnet; 2 here) for 70 s.
    let (mut h, mms) = setup(1, bench_cfg());
    let mm = &mms[0];
    let key = mm.market();
    let _ = open(&h, mm, Q);
    h.seconds(5);
    h.with_env(|env| {
        let a = env.actor(mm, 1_000_000_000_000_000, 5_000 * SOL);
        let spot = env.spot(mm);
        let (_, _, up) = env.push_price_ix(mm, &a, spot * 103 / 100).expect("up");
        ok(env.send(&[up], &[&a.kp]));
        let crank = crank_price_ix(mm);
        let (_, _, down) = env.push_price_ix(mm, &a, spot).expect("down");
        ok(env.send(&[crank, down], &[&a.kp]));
    });
    let mut bursts = 0;
    for _ in 0..70 {
        for r in [h.second().clone(), h.slot().clone()] {
            out.burst_cranks += cranks_of(&r, &key);
            out.burst_txs += r.batches.len() as u64;
            bursts += r.bursts.len();
        }
    }
    assert_eq!(bursts, 1, "one burst");
    // The idle cadence's two observations in those 70 s are not the burst's.
    out.burst_cranks -= 2;

    // A genuine 10 % move, held: per second until |P_spot/P_ema − 1| ≤ 2 %.
    let (mut h, mms) = setup(1, bench_cfg());
    let mm = &mms[0];
    let key = mm.market();
    let _ = open(&h, mm, Q);
    let mut pump = mock_pump(mm);
    h.seconds(35);
    h.with_env(|env| pump(env, 110, 100));
    for _ in 0..600 {
        let r = h.second().clone();
        if r.due.iter().any(|(k, d)| *k == key && *d == Due::Second) {
            out.ema_move_cranks += cranks_of(&r, &key);
        }
    }

    // Armed: the S1 run's calls (mock).
    let (h, mms) = setup(1, bench_cfg());
    let pump = mock_pump(&mms[0]);
    let mut cu = VenueCu::default();
    out.armed_settle_calls = run_settlement(h, &mms[0], Q, pump, &mut cu);

    // One isolated LP-wrapped crank on CPMM (USELESS, a position of q/4): the keeper's extra
    // observations over a control run (liquidity-anomaly coverage, ≤ liq_cover_s).
    let lp_run = |wrap: bool| -> u64 {
        use shortwin_litesvm_tests::lp;
        let (mut h, ms) = ray_setup(&[USELESS], bench_cfg());
        let (rm, q) = &ms[0];
        let key = rm.market();
        let _ = crate::raydium::ray_open(&h, rm, *q / 4);
        let AnyPool::Cpmm(cp) = rm.pool else {
            unreachable!()
        };
        let who = h.with_env(|env| {
            let add = cp.state(&env.svm).lp_supply / 20;
            let (sol, tok) = lp::lp_needed(env, &cp, add);
            lp::provider(env, &cp, 2 * sol + SOL, 2 * tok, 0)
        });
        let mut observed = 0;
        for s in 1..=90 {
            let r = h.second().clone();
            if s > 30 && r.observed(&key) {
                observed += 1;
            }
            if wrap && s == 30 {
                h.with_env(|env| {
                    let add = cp.state(&env.svm).lp_supply / 20;
                    ok(env.send(
                        &[
                            lp::deposit_ix(&cp, &who, add),
                            crank_price_ix(rm),
                            lp::withdraw_ix(&cp, &who, add),
                        ],
                        &[&who.owner],
                    ));
                });
            }
        }
        observed
    };
    out.liq_trigger_extra = lp_run(true) as i64 - lp_run(false) as i64;

    // A PumpSwap listing nobody observes: the keeper's warm-up.
    let (mut env, p) = Env::fork_pamm_coin(PammCoin::Mature);
    let r = p.reserves(&env.svm).expect("reserves");
    ok(env.list_pamm(&p.mint, &p.base_program));
    let pm = env.pamm_market_listed(&p.mint, r.base / 10, 0);
    let key = pm.market();
    let mut h = Harness::new(env, bench_cfg());
    for _ in 0..20 {
        let r = h.second().clone();
        out.pamm_warmup_cranks += u64::from(r.observed(&key));
    }
    out
}

// ---- global lines: the O17 pause and duty 11 ---------------------------------------------------

/// The keeper's transactions outside every market's budget: its `set_coverage_pause` (pause and
/// unpause, two signatures: the fee payer and the `coverage_authority`) and duty 11's
/// `crank_sol_usd`.
#[derive(Clone, Debug, Default)]
struct Globals {
    pause: Kind,
    oracle: Kind,
    /// Oracle cranks landed over `oracle_s` seconds of a feed publishing every 55 s.
    oracle_cranks: u64,
    oracle_s: u64,
}

fn globals() -> Globals {
    use crate::coverage_pause::{pause_sent, pauser_cfg, pauser_setup};
    let mut out = Globals::default();
    // The pause after a 45-s outage, and the unpause 600 s after coverage returns.
    let (mut h, mms, _auth) = pauser_setup(1, covering_all(pauser_cfg()));
    let key = mms[0].market();
    let _ = open(&h, &mms[0], Q);
    h.seconds(3);
    h.idle(45);
    for _ in 0..700 {
        let r = h.second().clone();
        for p in &r.coverage_pauses {
            if let ExitResult::Landed {
                outcome,
                unit_limit,
                bytes,
                ..
            } = &p.result
            {
                out.pause.0.push(Tx {
                    units: outcome.units_consumed,
                    limit: u64::from(*unit_limit),
                    bytes: *bytes as u64,
                    sigs: 2,
                });
            }
        }
        if pause_sent(&r, &key, false) {
            break;
        }
    }
    assert_eq!(out.pause.0.len(), 2, "one pause, one unpause");
    // Duty 11 on a feed publishing every 55 s (the sponsored heartbeat), for an hour.
    let (mut h, mms) = setup(1, bench_cfg());
    let _ = open(&h, &mms[0], Q);
    let p0 = crate::oracle::dump().price;
    h.keeper.config_mut().oracle.enabled = true;
    out.oracle_s = 3_600;
    let mut next_pub = h.now();
    for i in 0..out.oracle_s {
        if h.now() >= next_pub {
            let t = h.now();
            let price = p0 + (i as i64 % 7) * p0 / 2_000;
            h.with_env(|env| crate::oracle::plant(env, &crate::oracle::update(t, price)));
            next_pub = t + 55;
        }
        let r = h.second().clone();
        if let Some((
            _,
            ExitResult::Landed {
                outcome,
                unit_limit,
                bytes,
                ..
            },
        )) = &r.oracle
        {
            out.oracle_cranks += 1;
            out.oracle.0.push(Tx {
                units: outcome.units_consumed,
                limit: u64::from(*unit_limit),
                bytes: *bytes as u64,
                sigs: 1,
            });
        }
    }
    out
}

// ---- load (K8) --------------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
struct Load {
    markets: usize,
    per_second: bool,
    ticks: u64,
    wall_ms_avg: f64,
    wall_ms_max: f64,
    calls_per_tick: f64,
    txs_per_tick: f64,
    batches_max: usize,
    observed_every_due: bool,
    sims_per_tick: f64,
    /// A v1 batch of 6 the keeper sent: bytes, accounts, consumed CU, requested CU.
    six: Option<(usize, usize, u64, u32)>,
}

fn load(n: usize, per_second: bool, secs: u32) -> Load {
    let mut cfg = bench_cfg();
    // The idle cadence runs under the global budget's defaults: a fresh keeper funds every
    // market's entitlement at its batched first-crank estimate `⌈5,000 / batch_v1⌉` (M7D-5; as
    // reviewed it took the unbatched 5,000, so 1 SOL covered 85 markets at the harness's 14:13 UTC
    // and the 200-market run's lowest-ranked ones were refused until 00:00 UTC), so all 200 are
    // funded and every due observation lands (`observed_every_due`).
    if per_second {
        // Every market due every second (the per-second SLO for all of them at once): the load is
        // measured, not the budget — no budget (200 markets' per-second entitlements are ≈ 29 SOL
        // a day).
        cfg.budget.global_daily_lamports = 0;
        cfg.coverage.idle_interval_s = 1;
    }
    let mut env = Env::test_venue();
    // The keeper's own work is measured, not the harness's hooks.
    env.check_invariants = false;
    let mms: Vec<MockMarket> = (0..n)
        .map(|_| env.mock_market(MarketSpec::default()))
        .collect();
    let mut h = Harness::new(env, cfg);
    for mm in &mms {
        let _ = open(&h, mm, Q);
    }
    h.second();
    h.second();
    let mut out = Load {
        markets: n,
        per_second,
        observed_every_due: true,
        ..Load::default()
    };
    let mut calls = 0usize;
    let mut txs = 0usize;
    let sims0 = h.keeper.metrics.simulations;
    for _ in 0..secs {
        let before_rpc = h.chain().rpc_log().len();
        let before_tx = h.chain().sent().len();
        let t = Instant::now();
        let r = h.second().clone();
        let ms = t.elapsed().as_secs_f64() * 1e3;
        out.wall_ms_avg += ms;
        out.wall_ms_max = out.wall_ms_max.max(ms);
        let log = h.chain().rpc_log();
        calls += log[before_rpc..].iter().map(Rpc::calls).sum::<usize>();
        txs += h.chain().sent().len() - before_tx;
        out.batches_max = out.batches_max.max(r.batches.len());
        if out.six.is_none()
            && let Some(b) = r
                .batches
                .iter()
                .find(|b| b.markets.len() == 6 && b.format == TxFormat::V1)
        {
            out.six = Some((b.bytes, b.accounts, b.units, b.unit_limit));
        }
        out.observed_every_due &= r.due.iter().all(|(k, _)| r.observed(k));
        out.ticks += 1;
        assert!(r.errors.is_empty(), "{:?}", r.errors);
    }
    out.wall_ms_avg /= out.ticks as f64;
    out.sims_per_tick = (h.keeper.metrics.simulations - sims0) as f64 / out.ticks as f64;
    out.calls_per_tick = calls as f64 / out.ticks as f64;
    out.txs_per_tick = txs as f64 / out.ticks as f64;
    out
}

/// The per-second SLO over JSON-RPC (K8), derived from the load runs: the keeper awaits its
/// backend calls one after another, so a tick lasts ≈ `wall(n) + calls(n) × RTT`; with every
/// market due every second, `calls` and `wall` are fitted linearly on the 50- and 200-market runs,
/// and the most markets one instance keeps observed every second is the largest `n` with a tick
/// under 1,000 ms. UNVERIFIED against a live RPC (its latency, and whether `sendTransaction`
/// returns before the leader has it).
fn rpc_capacity(loads: &[Load]) -> Vec<(u64, u64)> {
    let all: Vec<&Load> = loads.iter().filter(|l| l.per_second).collect();
    let (lo, hi) = (all[0], all[all.len() - 1]);
    let dn = (hi.markets - lo.markets) as f64;
    let b_calls = (hi.calls_per_tick - lo.calls_per_tick) / dn;
    let a_calls = lo.calls_per_tick - b_calls * lo.markets as f64;
    let b_wall = (hi.wall_ms_avg - lo.wall_ms_avg) / dn;
    let a_wall = lo.wall_ms_avg - b_wall * lo.markets as f64;
    [5u64, 10, 30, 50, 100]
        .into_iter()
        .map(|rtt| {
            let r = rtt as f64;
            let n = (1_000.0 - a_wall - a_calls * r) / (b_wall + b_calls * r);
            (rtt, n.max(0.0).floor() as u64)
        })
        .collect()
}

// ---- costs ------------------------------------------------------------------------------------

/// Lamports per crank on `venue` at `price`: unbatched (the measured single crank), or a share of
/// a batch of `k` (one signature; the batch's limit ≈ `k` × the crank's).
fn per_crank(cu: &VenueCu, k: u64, price: u64) -> f64 {
    let t = cu.crank.worst();
    let prio = (u128::from(t.limit) * u128::from(k) * u128::from(price)).div_ceil(1_000_000) as f64;
    (BASE_FEE as f64 + prio) / k as f64
}

// ---- the report -------------------------------------------------------------------------------

fn sha256_of(path: &std::path::Path) -> String {
    let data = std::fs::read(path).unwrap_or_default();
    let h = solana_sha256_hasher::hash(&data);
    h.to_bytes().iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// A bench (minutes in release): CI's M7 bench job runs it with `--release -- --ignored`
/// (`tests/bench/m7_keeper`).
#[test]
#[ignore = "M7 bench: cargo test --release -- --ignored (tests/bench/m7_keeper)"]
fn m7_keeper_cost_inputs() {
    let t0 = Instant::now();
    let mut venues: BTreeMap<&'static str, VenueCu> = BTreeMap::new();
    let lap = |what: &str| println!("m7_cost: {what} at {:.0} s", t0.elapsed().as_secs_f64());
    venues.insert("mock", venue_mock());
    lap("mock");
    venues.insert("raydium_amm_v4", venue_raydium(FART));
    lap("raydium_amm_v4");
    venues.insert("raydium_cpmm", venue_raydium(USELESS));
    lap("raydium_cpmm");
    venues.insert("pumpswap", venue_pamm());
    lap("pumpswap");
    venues.insert("pump_curve", venue_curve());
    lap("pump_curve");
    let shapes = crank_shapes();
    let caps = capacities(&shapes);
    let glob = globals();
    let rates = rates();
    let loads = [
        load(50, false, 60),
        load(50, true, 20),
        load(200, false, 60),
        load(200, true, 10),
    ];

    // ---- checks (T-CU of the keeper's view; T-SIZE's batch sizes) ----
    for (name, cu) in &venues {
        for (what, k) in [
            ("crank", &cu.crank),
            ("liquidate", &cu.liquidate),
            ("expire", &cu.expire),
            ("sol_settle", &cu.settle_keep),
        ] {
            assert!(!k.0.is_empty(), "{name}: no {what} measured");
            for t in &k.0 {
                assert_eq!(
                    t.limit,
                    u64::from(tx::cu_limit(t.units)),
                    "{name} {what}: ×1.1"
                );
                assert!(t.bytes <= 4_096);
            }
        }
    }
    let cap = |n: &str| caps.iter().find(|c| c.name == n).expect("venue");
    // ARCH §6.4 / §7.1 item 7, measured with the keeper's builder: v0 through the protocol ALT
    // holds 4 Raydium, 5 curve and 3 PumpSwap cranks (M6R-1).
    assert_eq!(cap("raydium_amm_v4").v0_alt.0, 4);
    assert_eq!(cap("raydium_cpmm").v0_alt.0, 4);
    assert_eq!(cap("pumpswap").v0_alt.0, 3);
    assert_eq!(cap("pump_curve").v0_alt.0, 5);
    for c in &caps {
        assert!(c.v1.0 >= 6, "{}: a v1 batch of 6 fits", c.name);
        assert!(c.v1.2 <= 64 && c.v1.1 <= 4_096);
        // A table per market never holds fewer than the protocol ALT alone.
        assert!(c.v0_market_alts.0 >= c.v0_alt.0, "{c:?}");
        assert!(c.v0_market_alts.2 <= 64 && c.v0_market_alts.1 <= 1_232);
    }
    assert_eq!(rates.quiet_cranks * 30, rates.quiet_s, "quiet: every 30 s");
    assert_eq!(rates.near_cranks, rates.near_s, "near: every second");
    assert!((1..=10).contains(&rates.liq_trigger_extra), "{rates:?}");
    assert_eq!(rates.pamm_warmup_cranks, 8, "the PumpSwap warm-up");
    for l in &loads {
        assert!(l.observed_every_due, "{l:?}");
    }
    for k in [&glob.pause, &glob.oracle] {
        for t in &k.0 {
            assert_eq!(t.limit, u64::from(tx::cu_limit(t.units)), "×1.1");
        }
    }
    // Duty 11: one crank per feed update (every 55 s), none in between.
    assert!(
        (glob.oracle_s / 55..=glob.oracle_s / 55 + 1).contains(&glob.oracle_cranks),
        "{} oracle cranks",
        glob.oracle_cranks
    );

    // ---- JSON ----
    let so = |var: &str, rel: &str| {
        std::env::var_os(var).map_or_else(
            || std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(rel),
            std::path::PathBuf::from,
        )
    };
    let quiet_day = rates.quiet_cranks as f64 * DAY as f64 / rates.quiet_s as f64;
    let near_day = rates.near_cranks as f64 * DAY as f64 / rates.near_s as f64;
    let mut j = String::new();
    let _ = writeln!(j, "{{");
    let _ = writeln!(j, "  \"schema\": \"shortwin.m7.keeper_cost/1\",");
    let _ = writeln!(
        j,
        "  \"builds\": {{\"shortwin_so_sha256\": \"{}\", \"test_venue_so_sha256\": \"{}\"}},",
        sha256_of(&so("SHORTWIN_SO", "../../target/deploy/shortwin.so")),
        sha256_of(&so(
            "SHORTWIN_TEST_VENUE_SO",
            "../../target/test-venue/deploy/shortwin.so"
        ))
    );
    let _ = writeln!(
        j,
        "  \"fee_model\": {{\"base_fee_lamports_per_signature\": {BASE_FEE}, \"priority_lamports\": \"ceil(requested_cu * price / 1e6)\", \"requested_cu\": \"ceil(consumed * 1.1)\", \"prices_micro_lamports_per_cu\": [0, 1000, 10000, 100000]}},"
    );
    let _ = writeln!(j, "  \"cu\": {{");
    let n = venues.len();
    for (i, (name, cu)) in venues.iter().enumerate() {
        let _ = writeln!(
            j,
            "    \"{name}\": {}{}",
            cu.json(),
            if i + 1 < n { "," } else { "" }
        );
    }
    let _ = writeln!(j, "  }},");
    let _ = writeln!(j, "  \"crank_batches\": {{");
    for (i, c) in caps.iter().enumerate() {
        let _ = writeln!(
            j,
            "    \"{}\": {{\"v1_max\": {}, \"v1_max_bytes\": {}, \"v1_max_accounts\": {}, \"v1_six_bytes\": {}, \"v1_six_accounts\": {}, \"v0_protocol_alt_max\": {}, \"v0_protocol_alt_bytes\": {}, \"v0_market_alts_max\": {}, \"v0_market_alts_bytes\": {}, \"v0_market_alts_accounts\": {}, \"v0_no_alt_max\": {}}}{}",
            c.name,
            c.v1.0,
            c.v1.1,
            c.v1.2,
            c.v1_six.0,
            c.v1_six.1,
            c.v0_alt.0,
            c.v0_alt.1,
            c.v0_market_alts.0,
            c.v0_market_alts.1,
            c.v0_market_alts.2,
            c.v0_bare.0,
            if i + 1 < caps.len() { "," } else { "" }
        );
    }
    let _ = writeln!(j, "  }},");
    let oracle_day = glob.oracle_cranks as f64 * DAY as f64 / glob.oracle_s as f64;
    let lam = |k: &Kind, p: u64| k.worst().lamports(p);
    let _ = writeln!(
        j,
        "  \"global_lines\": {{\"set_coverage_pause\": {}, \"set_coverage_pause_lamports_by_price\": [{}], \"crank_sol_usd\": {}, \"crank_sol_usd_lamports_by_price\": [{}], \"crank_sol_usd_per_day\": {oracle_day:.0}, \"crank_sol_usd_sol_per_day_by_price\": [{}]}},",
        glob.pause.json(),
        PRICES
            .iter()
            .map(|p| lam(&glob.pause, *p).to_string())
            .collect::<Vec<_>>()
            .join(", "),
        glob.oracle.json(),
        PRICES
            .iter()
            .map(|p| lam(&glob.oracle, *p).to_string())
            .collect::<Vec<_>>()
            .join(", "),
        PRICES
            .iter()
            .map(|p| format!("{:.5}", oracle_day * lam(&glob.oracle, *p) as f64 / 1e9))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let _ = writeln!(
        j,
        "  \"cadence\": {{\"quiet_cranks_per_day\": {quiet_day:.0}, \"near_cranks_per_day\": {near_day:.0}, \"armed_sol_settle_calls_per_episode\": {}, \"price_burst_cranks_per_trigger\": {}, \"genuine_10pct_move_per_second_cranks\": {}, \"liquidity_trigger_extra_cranks\": {}, \"pumpswap_warmup_cranks\": {}, \"measured_s\": {{\"quiet\": {}, \"near\": {}}}}},",
        rates.armed_settle_calls,
        rates.burst_cranks,
        rates.ema_move_cranks,
        rates.liq_trigger_extra,
        rates.pamm_warmup_cranks,
        rates.quiet_s,
        rates.near_s
    );
    // Per market-day, per venue: targeted coverage at a share f of the day near a threshold.
    let _ = writeln!(j, "  \"cost_per_market_day_sol\": {{");
    let shares = [0.0, 0.01, 0.05, 0.20, 1.0];
    for (i, (name, cu)) in venues.iter().enumerate() {
        let v0k = caps
            .iter()
            .find(|c| c.name == *name)
            .map_or(1, |c| c.v0_alt.0 as u64);
        let mut cells = Vec::new();
        for (label, k) in [("unbatched", 1u64), ("v1_k6", 6), ("v0_alt", v0k)] {
            let mut per_price = Vec::new();
            for p in PRICES {
                let c = per_crank(cu, k, p);
                let row: Vec<String> = shares
                    .iter()
                    .map(|f| format!("{:.5}", (f * near_day + (1.0 - f) * quiet_day) * c / 1e9))
                    .collect();
                per_price.push(format!("\"{p}\": [{}]", row.join(", ")));
            }
            cells.push(format!("\"{label}\": {{{}}}", per_price.join(", ")));
        }
        let _ = writeln!(
            j,
            "    \"{name}\": {{\"near_share\": [0, 0.01, 0.05, 0.2, 1], {}}}{}",
            cells.join(", "),
            if i + 1 < n { "," } else { "" }
        );
    }
    let _ = writeln!(j, "  }},");
    let _ = writeln!(j, "  \"exit_lamports_worst\": {{");
    for (i, (name, cu)) in venues.iter().enumerate() {
        let row = |k: &Kind| {
            let t = k.worst();
            let v: Vec<String> = PRICES.iter().map(|p| t.lamports(*p).to_string()).collect();
            format!("[{}]", v.join(", "))
        };
        let _ = writeln!(
            j,
            "    \"{name}\": {{\"liquidate\": {}, \"expire\": {}, \"sol_settle_keep\": {}, \"sol_settle_settle\": {}}}{}",
            row(&cu.liquidate),
            row(&cu.expire),
            row(&cu.settle_keep),
            row(&cu.settle_exec),
            if i + 1 < n { "," } else { "" }
        );
    }
    let _ = writeln!(j, "  }},");
    let _ = writeln!(j, "  \"load\": [");
    for (i, l) in loads.iter().enumerate() {
        let _ = writeln!(
            j,
            "    {{\"markets\": {}, \"all_per_second\": {}, \"ticks\": {}, \"wall_ms_avg\": {:.1}, \"wall_ms_max\": {:.1}, \"backend_calls_per_tick\": {:.1}, \"simulations_per_tick\": {:.1}, \"txs_per_tick\": {:.2}, \"batches_max\": {}, \"every_due_observed\": {}, \"v1_batch_of_6\": {}}}{}",
            l.markets,
            l.per_second,
            l.ticks,
            l.wall_ms_avg,
            l.wall_ms_max,
            l.calls_per_tick,
            l.sims_per_tick,
            l.txs_per_tick,
            l.batches_max,
            l.observed_every_due,
            l.six.map_or("null".to_string(), |(b, a, u, lim)| format!(
                "{{\"bytes\": {b}, \"accounts\": {a}, \"consumed_cu\": {u}, \"requested_cu\": {lim}}}"
            )),
            if i + 1 < loads.len() { "," } else { "" }
        );
    }
    let _ = writeln!(j, "  ],");
    let cap = rpc_capacity(&loads);
    let _ = writeln!(
        j,
        "  \"rpc_per_second_capacity\": {{\"note\": \"derived: sequential backend calls, tick = wall + calls x RTT <= 1000 ms, every market due every second; UNVERIFIED on a live RPC\", \"markets_by_rtt_ms\": {{{}}}}}",
        cap.iter()
            .map(|(r, n)| format!("\"{r}\": {n}"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    let _ = writeln!(j, "}}");

    // ---- Markdown ----
    let mut md = String::new();
    let _ = writeln!(
        md,
        "# m7_keeper — keeper cost inputs, measured (ECON §5.8; ARCH §9 M7)\n\nGenerated by `services/keeper-litesvm` `m7_cost::m7_keeper_cost_inputs` (`M7_COST_WRITE=1`; quiet cadence over {} s). Machine-readable: `results.json`. Every figure is the real keeper's own transaction (its `⌈simulated × 1.1⌉` limit, its size), executed with the invariant hooks.\n",
        rates.quiet_s
    );
    let _ = writeln!(md, "## CU per instruction (consumed / requested, size)\n");
    let _ = writeln!(
        md,
        "| venue | crank_price (alone) | liquidate | expire | sol_settle (arm / keep) | sol_settle (settles, S1) |\n|---|---|---|---|---|---|"
    );
    for (name, cu) in &venues {
        let _ = writeln!(
            md,
            "| {name} | {} | {} | {} | {} | {} |",
            cu.crank.md(),
            cu.liquidate.md(),
            cu.expire.md(),
            cu.settle_keep.md(),
            cu.settle_exec.md()
        );
    }
    let _ = writeln!(
        md,
        "\n## Lamports per exit (the costliest measured; 5,000 + ⌈requested × price / 10⁶⌉)\n\n| venue | kind | 0 | 1,000 | 10,000 | 100,000 µlamports/CU |\n|---|---|---|---|---|---|"
    );
    for (name, cu) in &venues {
        for (what, k) in [
            ("liquidate", &cu.liquidate),
            ("expire", &cu.expire),
            ("sol_settle (keep)", &cu.settle_keep),
            ("sol_settle (settles)", &cu.settle_exec),
        ] {
            let t = k.worst();
            let cells: Vec<String> = PRICES.iter().map(|p| sep(t.lamports(*p))).collect();
            let _ = writeln!(md, "| {name} | {what} | {} |", cells.join(" | "));
        }
    }
    let _ = writeln!(
        md,
        "\n## Global lines (never a market's budget)\n\n| transaction | consumed / requested CU, size | 0 | 1,000 | 10,000 | 100,000 µlamports/CU |\n|---|---|---|---|---|---|"
    );
    for (what, k) in [
        (
            "set_coverage_pause (O17; fee payer + coverage_authority: two signatures; the exit ceiling)",
            &glob.pause,
        ),
        ("crank_sol_usd (duty 11)", &glob.oracle),
    ] {
        let cells: Vec<String> = PRICES.iter().map(|p| sep(lam(k, *p))).collect();
        let _ = writeln!(md, "| {what} | {} | {} |", k.md(), cells.join(" | "));
    }
    let _ = writeln!(
        md,
        "\nDuty 11 on a feed publishing every 55 s: {} cranks in {} s → **{oracle_day:.0} a day**, {} SOL a day at 0 / 1,000 / 10,000 / 100,000 µlamports/CU (one global line for the whole protocol). A coverage pause and its unpause cost two transactions per degraded episode.",
        glob.oracle_cranks,
        glob.oracle_s,
        PRICES
            .iter()
            .map(|p| format!("{:.4}", oracle_day * lam(&glob.oracle, *p) as f64 / 1e9))
            .collect::<Vec<_>>()
            .join(" / ")
    );
    let _ = writeln!(
        md,
        "\n## Cranks per transaction (the keeper's builder; priority price set)\n"
    );
    let _ = writeln!(
        md,
        "| venue | v1 max (B / accounts) | v1 batch of 6 (B / accounts) | v0 + protocol ALT max (B) | v0 + protocol ALT + a table per market, max (B / accounts) | v0 without a table |\n|---|---|---|---|---|---|"
    );
    for c in &caps {
        let _ = writeln!(
            md,
            "| {} | {} ({} / {}) | {} / {} | {} ({}) | {} ({} / {}) | {} |",
            c.name,
            c.v1.0,
            sep(c.v1.1 as u64),
            c.v1.2,
            sep(c.v1_six.0 as u64),
            c.v1_six.1,
            c.v0_alt.0,
            sep(c.v0_alt.1 as u64),
            c.v0_market_alts.0,
            sep(c.v0_market_alts.1 as u64),
            c.v0_market_alts.2,
            c.v0_bare.0
        );
    }
    let _ = writeln!(
        md,
        "\n## Cadence (the keeper's, venue-independent)\n\n| what | measured |\n|---|---|\n| quiet (a healthy position) | {} cranks in {} s → **{quiet_day:.0} per market-day** |\n| near a threshold | {} cranks in {} s → **{near_day:.0} per market-day** |\n| an armed position (S1 after a ×4 gap) | {} `sol_settle` calls |\n| one foreign price deviation (`[buy 3 %, crank, sell]`) | {} cranks (per-slot burst, 2 ticks a second) |\n| a genuine 10 % move, held | {} per-second cranks until `|P_spot/P_ema − 1| ≤ 2 %` |\n| one isolated LP-wrapped crank (CPMM, a position open) | {} cranks above the control |\n| a PumpSwap listing's warm-up | {} cranks |",
        rates.quiet_cranks,
        rates.quiet_s,
        rates.near_cranks,
        rates.near_s,
        rates.armed_settle_calls,
        rates.burst_cranks,
        rates.ema_move_cranks,
        rates.liq_trigger_extra,
        rates.pamm_warmup_cranks
    );
    let _ = writeln!(
        md,
        "\n## Cost per market-day (SOL), targeted coverage: share f of the day near a threshold\n"
    );
    for (name, cu) in &venues {
        let v0k = caps
            .iter()
            .find(|c| c.name == *name)
            .map_or(1, |c| c.v0_alt.0 as u64);
        let _ = writeln!(
            md,
            "**{name}** (crank {} requested CU; v0 batch {v0k})\n\n| f | batching | 0 | 1,000 | 10,000 | 100,000 µlamports/CU |\n|---|---|---|---|---|---|",
            sep(cu.crank.worst().limit)
        );
        for f in shares {
            for (label, k) in [("unbatched", 1u64), ("v1 k = 6", 6), ("v0 + ALT", v0k)] {
                let cells: Vec<String> = PRICES
                    .iter()
                    .map(|p| {
                        format!(
                            "{:.4}",
                            (f * near_day + (1.0 - f) * quiet_day) * per_crank(cu, k, *p) / 1e9
                        )
                    })
                    .collect();
                let _ = writeln!(
                    md,
                    "| {:.0} % | {label} | {} |",
                    f * 100.0,
                    cells.join(" | ")
                );
            }
        }
        let _ = writeln!(md);
    }
    let _ = writeln!(
        md,
        "## Load (the in-process backend; hooks off)\n\n| markets | cadence | ticks | wall ms / tick (avg, max) | reads + sends / tick | simulations / tick | transactions / tick | batches (max) | every due market observed | a v1 batch of 6 sent (B / accounts / CU consumed / requested) |\n|---|---|---|---|---|---|---|---|---|---|"
    );
    for l in &loads {
        let _ = writeln!(
            md,
            "| {} | {} | {} | {:.1}, {:.1} | {:.1} | {:.1} | {:.2} | {} | {} | {} |",
            l.markets,
            if l.per_second {
                "all every second"
            } else {
                "quiet (30 s)"
            },
            l.ticks,
            l.wall_ms_avg,
            l.wall_ms_max,
            l.calls_per_tick,
            l.sims_per_tick,
            l.txs_per_tick,
            l.batches_max,
            l.observed_every_due,
            l.six.map_or("—".to_string(), |(b, a, u, lim)| format!(
                "{} / {a} / {} / {}",
                sep(b as u64),
                sep(u),
                sep(u64::from(lim))
            ))
        );
    }
    let _ = writeln!(
        md,
        "\nOver JSON-RPC (derived, UNVERIFIED on a live RPC): the keeper awaits its backend calls one after another, so a tick lasts ≈ wall + calls × RTT. With every market due every second (calls and wall fitted on the 50- and 200-market runs), one instance keeps this many markets observed every second: {}. Targeted coverage makes a small share of markets due in any second (quiet markets every 30 s), so the covered-market capacity is several times these; above them the keeper needs concurrent calls or more instances (each with its own markets).",
        cap.iter()
            .map(|(r, n)| format!("{n} at {r} ms"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!("{md}");
    println!("{j}");
    println!("m7_keeper_cost_inputs: {:.0} s", t0.elapsed().as_secs_f64());
    if std::env::var_os("M7_COST_WRITE").is_some() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/bench/m7_keeper");
        std::fs::create_dir_all(&dir).expect("m7_keeper dir");
        std::fs::write(dir.join("results.json"), j).expect("results.json");
        std::fs::write(dir.join("results.md"), md).expect("results.md");
    }
}
