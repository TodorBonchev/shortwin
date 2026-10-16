//! **T-CASCADE** (ARCH §6.6, §8.2; C0-17; ECON §5.8; M7 K4): many exits on one market in
//! consecutive slots under the per-writable-account block budget — `MAX_WRITABLE_ACCOUNT_UNITS` =
//! 24,000,000 CU (ARCH V10), charged with each transaction's **requested** limit (the keeper's
//! `⌈simulated × 1.1⌉`) against every account it write-locks: the `Market`, its `MarketVault`,
//! the pool vault, the venue's pool accounts and the keeper's own fee payer.
//!
//! **Baskets.** (1) The launch-cap basket on the mock venue: `min_notional` positions (≈ 0.1 SOL
//! each) filling the market's 25-SOL cap (`market_max_notional`), one borrower each, on a pool deep
//! enough that only the launch cap binds. (2) The same on a real PumpSwap pool (the `mature` fork),
//! whose `liquidate` is the heaviest measured (≈ 214 k requested CU, `tests/bench/m7_keeper`): as
//! many `min_notional` positions as its caps admit. **Paths** — those of `cascade.py` scaled to the
//! positions' Tier C terms (they open at a live CR ≈ 2.9 against `L` = 1.5, so `cascade.py`'s ×1.8
//! gap and ×2 pump, sized for Tier A, do not reach `L`): a ×3 gap held (not bought back: S1
//! settlements, one `sol_settle` a second per armed position while it arms), and pumps ×3 over
//! 60 s, ×3 over 10 minutes and ×10 over 10 minutes. The keeper ticks every slot (two a second).
//!
//! **The keeper's policy** (`budget.market_cu_per_slot` 12,000,000 and `budget.payer_cu_per_slot`
//! 20,000,000; `ExitResult::Deferred`): a market's exits stop for the slot at half the account's
//! block budget, most under-collateralised first (`decide::evaluate_all`'s order), and all of the
//! keeper's exits at 20 M (its fee payer is write-locked by every one of its transactions); the
//! rest goes out in the next slot. Each basket runs with the policy and without it (both caps 0:
//! the keeper then asks the `Market` for up to 27 M in one slot, which a leader would not pack;
//! LiteSVM enforces no block limit, so the uncapped run is the optimistic bound on how fast the
//! basket could clear). A single position on the same path is shown for context (its entry price
//! differs from the basket's by the basket's own price impact).
//!
//! **Gate:** with the policy, no slot asks more than 12 M of the `Market` or the `MarketVault`,
//! 20 M of the fee payer, 24 M of any account; the basket clears within 2 s of its first exit and
//! at most 1 s after the uncapped run.
//!
//! A bench (release: a few minutes): CI's M7 bench job runs it with `--release -- --ignored`;
//! `M7_CASCADE_WRITE=1` writes `tests/bench/m7_cascade/results.{md,json}`.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::rc::Rc;

use anchor_lang::prelude::Pubkey;
use shortwin::errors::ShortwinError;
use shortwin::events::SolSettle;
use shortwin_keeper::events;
use shortwin_litesvm_tests::engine::VenueMarket;
use shortwin_litesvm_tests::errors::custom_code;
use shortwin_litesvm_tests::pamm::{PammCoin, PammMarket, PammUser};

use crate::common::*;

/// Agave's per-writable-account block budget (ARCH V10).
const ACCOUNT_BLOCK_CU: u64 = 24_000_000;
/// The keeper's default caps (`BudgetConfig`).
const MARKET_CAP: u64 = 12_000_000;
const PAYER_CAP: u64 = 20_000_000;

/// A `min_notional` position's target proceeds: 0.1012 SOL at the pool's spot before the fee and
/// the impact (≈ 0.1009 SOL received), just above the 0.1-SOL minimum. Each open sells into the
/// pool, so the size is set from the spot at each open.
const NOTIONAL_MIN: u64 = 101_200_000;

/// `Q` of a `NOTIONAL_MIN` position at `spot` (WAD lamports per raw unit).
fn q_at(spot: u128) -> u64 {
    u64::try_from(u128::from(NOTIONAL_MIN) * shortwin_math::num::WAD / spot.max(1)).expect("u64")
}

type Path = fn(i64) -> f64;

fn gap_3(s: i64) -> f64 {
    if s >= 1 { 3.0 } else { 1.0 }
}

fn pump_3x_60s(s: i64) -> f64 {
    (3f64.ln() * (s.clamp(0, 60) as f64) / 60.0).exp()
}

fn pump_3x_10min(s: i64) -> f64 {
    (3f64.ln() * (s.clamp(0, 600) as f64) / 600.0).exp()
}

fn pump_10x_10min(s: i64) -> f64 {
    (10f64.ln() * (s.clamp(0, 600) as f64) / 600.0).exp()
}

/// One slot's keeper traffic on the market.
#[derive(Clone, Debug, Default)]
struct SlotRow {
    second: i64,
    slot: u64,
    liquidations: usize,
    settlements: usize,
    deferred: usize,
    /// Requested CU per writable account (named).
    per_account: BTreeMap<&'static str, u64>,
}

#[derive(Clone, Debug, Default)]
struct Cascade {
    venue: &'static str,
    path: &'static str,
    positions: usize,
    /// What stopped the basket's opens (the program's error).
    stopped_by: String,
    market_cap: u64,
    payer_cap: u64,
    rows: Vec<SlotRow>,
    first_exit_s: Option<i64>,
    last_exit_s: Option<i64>,
    liquidations: usize,
    settlements: usize,
    deferred: usize,
    /// The largest per-slot requested CU on any account, and which.
    peak: (u64, &'static str),
    /// The largest number of liquidations landed in one slot.
    peak_liqs: usize,
    exit_cu: (u64, u64),
    /// The first position's tier and `L` (bps).
    tier: u8,
    cr_liq_bps: u32,
}

/// Reads a venue's spot (WAD lamports per raw unit).
type SpotFn = Box<dyn Fn(&Env) -> u128>;
/// Lifts a venue's spot to a target (never lowers it).
type LiftFn = Box<dyn FnMut(&mut Env, u128)>;

/// A basket on a venue, ready to drive: the harness, the market, its named accounts, and how to
/// read and lift its spot.
struct Basket {
    venue: &'static str,
    h: Harness,
    key: Pubkey,
    positions: Vec<Pubkey>,
    stopped_by: String,
    names: Vec<(Pubkey, &'static str)>,
    spot: SpotFn,
    lift: LiftFn,
}

fn cfg(market_cap: u64, payer_cap: u64) -> KeeperConfig {
    // Every market with positions covered: as the basket clears, the market's value at risk falls
    // below the global budget's 1-SOL floor (decision A, 2026-10-01) and its last positions would
    // get the idle cadence only; the bench measures the CU-per-slot policy (`common::covering_all`).
    let mut cfg = covering_all(KeeperConfig::default());
    cfg.budget.market_cu_per_slot = market_cap;
    cfg.budget.payer_cu_per_slot = payer_cap;
    cfg
}

/// The name of an open refusal.
fn refusal(code: Option<u32>) -> String {
    [
        ShortwinError::MarketNotionalCap,
        ShortwinError::CapMarketOi,
        ShortwinError::CapUtilization,
        ShortwinError::CapPositionImpact,
        ShortwinError::CapWallet,
        ShortwinError::InsufficientInventory,
        ShortwinError::BelowMinNotional,
        ShortwinError::PriceDeviatesFromEma,
    ]
    .into_iter()
    .find(|e| Some(u32::from(*e)) == code)
    .map_or_else(|| format!("{code:?}"), |e| format!("{e:?}"))
}

/// Opens up to `n` `NOTIONAL_MIN` positions on `m` (a new borrower each) until a cap refuses one;
/// returns the positions and the refusal.
fn fill(
    env: &mut Env,
    m: &impl VenueMarket,
    n: usize,
    spot: &dyn Fn(&Env) -> u128,
) -> (Vec<Pubkey>, String) {
    env.crank(m);
    env.warp(1);
    let key = m.market();
    let mut positions = Vec::new();
    for _ in 0..n {
        let mut b = env.borrower();
        let q = q_at(spot(env));
        let (res, nonce) = env.open(m, &mut b, q);
        if let Err(f) = res {
            return (positions, refusal(custom_code(&Err(f))));
        }
        positions.push(shortwin_litesvm_tests::pda::position(&key, &b.key(), nonce).0);
    }
    (positions, "—".into())
}

/// The mock launch-cap basket: a pool deep enough (10,000 SOL, the default price and fee) that
/// the fee-scaled open-interest cap is above the 25-SOL launch cap.
fn mock_basket(n: usize, keeper: KeeperConfig) -> Basket {
    let mut env = Env::test_venue();
    let mm = Rc::new(env.mock_market(MarketSpec {
        base: 10_000_000_000_000_000,
        quote: 10_000_000_000_000,
        lend: 1_000_000_000_000_000,
        ..MarketSpec::default()
    }));
    let spot_of = {
        let mm = mm.clone();
        move |env: &Env| env.spot(&mm)
    };
    let (positions, stopped_by) = fill(&mut env, mm.as_ref(), n, &spot_of);
    let h = Harness::new(env, keeper);
    let whale = h.with_env(|env| env.actor(&mm, 0, 200_000 * 1_000_000_000));
    let payer = h.keeper.payer();
    let k = &mm.keys;
    let p = mm.mock();
    let names = vec![
        (mm.market(), "Market"),
        (k.market_vault, "MarketVault"),
        (k.pool_vault, "pool_vault"),
        (k.trader, "trader"),
        (mm.trader_wsol, "trader_wsol"),
        (p.pool, "venue pool"),
        (p.base_vault, "venue base vault"),
        (p.quote_vault, "venue quote vault"),
        (payer, "keeper fee payer"),
    ];
    let key = mm.market();
    Basket {
        venue: "mock",
        h,
        key,
        positions,
        stopped_by,
        names,
        spot: Box::new(spot_of),
        lift: Box::new(move |env, target| env.push_price(&mm, &whale, target)),
    }
}

/// The PumpSwap basket on the `mature` pool: lent tokens worth ≈ 60 SOL, the boost waited out,
/// then as many `NOTIONAL_MIN` positions as the caps admit.
fn pamm_basket(n: usize, keeper: KeeperConfig) -> Basket {
    let (mut env, p) = Env::fork_pamm_coin(PammCoin::Mature);
    let r = p.reserves(&env.svm).expect("reserves");
    let lend =
        u64::try_from(u128::from(r.base) * 60_000_000_000 / r.effective_quote.max(1)).expect("u64");
    ok(env.list_pamm(&p.mint, &p.base_program));
    let pm: PammMarket = env.pamm_market_listed(&p.mint, lend, 60);
    let params = env.config().params;
    loop {
        let m = env.market(&pm.market());
        if pm.pool.boost_amount(&env.svm) <= params.boost_dust_lamports
            || env.now() >= m.graduated_at + i64::from(params.boost_window_s)
        {
            break;
        }
        env.warp(30);
        env.crank(&pm);
    }
    let pool = pm.pool;
    let spot_of = move |env: &Env| pool.spot_wad(&env.svm);
    let (positions, stopped_by) = fill(&mut env, &pm, n, &spot_of);
    let h = Harness::new(env, keeper);
    let whale = h.with_env(|env| PammUser::new(env, &pool, 200_000 * 1_000_000_000));
    let payer = h.keeper.payer();
    let k = pm.keys();
    let names = vec![
        (pm.market(), "Market"),
        (k.market_vault, "MarketVault"),
        (k.pool_vault, "pool_vault"),
        (k.trader, "trader"),
        (pool.pool, "venue pool"),
        (pool.base_vault, "venue base vault"),
        (pool.quote_vault, "venue quote vault"),
        (payer, "keeper fee payer"),
    ];
    let key = pm.market();
    Basket {
        venue: "pumpswap",
        h,
        key,
        positions,
        stopped_by,
        names,
        spot: Box::new(spot_of),
        lift: Box::new(move |env, target| {
            let spot = pool.spot_wad(&env.svm);
            if target > spot {
                // The ratio in millionths (the pool's `base² · den` stays inside u128).
                let num = target * 1_000_000 / spot.max(1);
                ok(whale.push_price(env, &pool, num, 1_000_000));
            }
        }),
    }
}

/// Drives `b` along `path` from the second after the last open, the keeper ticking every slot,
/// until every position has exited.
fn drive(mut b: Basket, path_name: &'static str, path: Path) -> Cascade {
    let key = b.key;
    let (market_cap, payer_cap) = {
        let c = &b.h.keeper.config_mut().budget;
        (c.market_cu_per_slot, c.payer_cu_per_slot)
    };
    let names = b.names.clone();
    let name_of = |k: &Pubkey| names.iter().find(|(x, _)| x == k).map(|(_, n)| *n);
    b.h.seconds(3);
    let p0 = b.h.with_env(|env| (b.spot)(env));
    let mut out = Cascade {
        venue: b.venue,
        path: path_name,
        positions: b.positions.len(),
        stopped_by: b.stopped_by.clone(),
        market_cap,
        payer_cap,
        exit_cu: (u64::MAX, 0),
        ..Cascade::default()
    };
    if let Some(p) = b.positions.first() {
        let pos = b.h.with_env(|env| {
            shortwin_litesvm_tests::read::borsh::<shortwin::state::ShortPosition>(&env.svm, p)
                .expect("position")
        });
        (out.tier, out.cr_liq_bps) = (pos.tier, pos.cr_liq_bps);
    }
    let t0 = b.h.now();
    let mut open = b.positions.len();
    let mut sent_seen = b.h.chain().sent().len();
    for s in 1..=1_800i64 {
        let target = (p0 as f64 * path(s)) as u128;
        {
            let (spot, lift) = (&b.spot, &mut b.lift);
            b.h.with_env(|env| {
                let now = spot(env);
                if target > now && (target - now) * 10_000 > now {
                    lift(env, target);
                }
            });
        }
        for tick in 0..2 {
            let r = if tick == 0 {
                b.h.second().clone()
            } else {
                b.h.slot().clone()
            };
            assert!(r.errors.is_empty(), "{:?}", r.errors);
            let slot = r.clock.expect("clock").slot;
            let mut row = SlotRow {
                second: b.h.now() - t0,
                slot,
                ..SlotRow::default()
            };
            for e in r.exits.iter().filter(|e| e.market == key) {
                match &e.result {
                    ExitResult::Landed {
                        outcome,
                        unit_limit,
                        ..
                    } => {
                        let lim = u64::from(*unit_limit);
                        out.exit_cu = (out.exit_cu.0.min(lim), out.exit_cu.1.max(lim));
                        match e.kind {
                            ExitKind::Liquidate => row.liquidations += 1,
                            ExitKind::SolSettle if !events::of::<SolSettle>(outcome).is_empty() => {
                                row.settlements += 1
                            }
                            _ => {}
                        }
                    }
                    ExitResult::Deferred { .. } => row.deferred += 1,
                    _ => {}
                }
            }
            let sent = b.h.chain().sent();
            for t in &sent[sent_seen..] {
                let lim = u64::from(t.cu_limit.unwrap_or(200_000));
                for w in &t.writable {
                    if let Some(n) = name_of(w) {
                        *row.per_account.entry(n).or_default() += lim;
                    }
                }
            }
            sent_seen = sent.len();
            if row.liquidations + row.settlements > 0 {
                out.first_exit_s.get_or_insert(row.second);
                out.last_exit_s = Some(row.second);
            }
            out.liquidations += row.liquidations;
            out.settlements += row.settlements;
            out.deferred += row.deferred;
            out.peak_liqs = out.peak_liqs.max(row.liquidations);
            for (n, cu) in &row.per_account {
                if *cu > out.peak.0 {
                    out.peak = (*cu, n);
                }
            }
            open -= row.liquidations + row.settlements;
            if row.liquidations + row.settlements + row.deferred > 0 {
                out.rows.push(row);
            }
        }
        if open == 0 {
            break;
        }
    }
    assert_eq!(open, 0, "{} {path_name}: {open} positions left", b.venue);
    out
}

fn md_row(c: &Cascade, uncapped: Option<&Cascade>, isolated: Option<i64>) -> String {
    let s = |v: Option<i64>| v.map_or("—".into(), |x| format!("{x} s"));
    format!(
        "| {} | {} | {} ({}) | {} / {} | {} | {} | {} | {} | {} | {} | {} | {} ({}) | {} |",
        c.venue,
        c.path,
        c.positions,
        c.stopped_by,
        c.market_cap,
        c.payer_cap,
        c.liquidations,
        c.settlements,
        c.deferred,
        s(c.first_exit_s),
        s(c.last_exit_s),
        s(uncapped.and_then(|u| u.last_exit_s)),
        s(isolated),
        c.peak.0,
        c.peak.1,
        c.peak_liqs
    )
}

fn json_row(c: &Cascade, uncapped_cleared: Option<i64>, isolated: Option<i64>) -> String {
    let n = |v: Option<i64>| v.map_or("null".to_string(), |x| x.to_string());
    format!(
        "{{\"venue\": \"{}\", \"path\": \"{}\", \"positions\": {}, \"stopped_by\": \"{}\", \"market_cu_per_slot\": {}, \"payer_cu_per_slot\": {}, \"liquidations\": {}, \"settlements\": {}, \"deferred\": {}, \"first_exit_s\": {}, \"cleared_s\": {}, \"uncapped_cleared_s\": {}, \"isolated_first_exit_s\": {}, \"peak_requested_cu\": {}, \"peak_account\": \"{}\", \"peak_liquidations_per_slot\": {}, \"exit_requested_cu\": [{}, {}], \"tier\": {}, \"cr_liq_bps\": {}}}",
        c.venue,
        c.path,
        c.positions,
        c.stopped_by,
        c.market_cap,
        c.payer_cap,
        c.liquidations,
        c.settlements,
        c.deferred,
        n(c.first_exit_s),
        n(c.last_exit_s),
        n(uncapped_cleared),
        n(isolated),
        c.peak.0,
        c.peak.1,
        c.peak_liqs,
        c.exit_cu.0,
        c.exit_cu.1,
        c.tier,
        c.cr_liq_bps
    )
}

fn slot_table(md: &mut String, title: &str, c: &Cascade) {
    let _ = writeln!(
        md,
        "\n## {title}\n\n| second | slot | liquidations | settlements | deferred | Market | MarketVault | pool_vault | venue pool | keeper fee payer |\n|---|---|---|---|---|---|---|---|---|---|"
    );
    for r in &c.rows {
        let g = |n: &str| r.per_account.get(n).copied().unwrap_or(0);
        let _ = writeln!(
            md,
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            r.second,
            r.slot,
            r.liquidations,
            r.settlements,
            r.deferred,
            g("Market"),
            g("MarketVault"),
            g("pool_vault"),
            g("venue pool"),
            g("keeper fee payer")
        );
    }
}

/// ★ T-CASCADE (module docs). A bench (a few minutes in release, far longer in debug): CI's M7
/// bench job runs it with `--release -- --ignored` (`tests/bench/m7_cascade`).
#[test]
#[ignore = "M7 bench: cargo test --release -- --ignored (tests/bench/m7_cascade)"]
fn t_cascade_a_launch_cap_basket_clears_under_the_block_budget() {
    let paths: [(&'static str, Path); 4] = [
        ("gap ×3", gap_3),
        ("pump ×3 / 60 s", pump_3x_60s),
        ("pump ×3 / 10 min", pump_3x_10min),
        ("pump ×10 / 10 min", pump_10x_10min),
    ];
    // (capped, uncapped, the isolated position's first exit)
    let mut runs: Vec<(Cascade, Cascade, Option<i64>)> = Vec::new();
    for (name, path) in paths {
        let capped = drive(mock_basket(260, cfg(MARKET_CAP, PAYER_CAP)), name, path);
        let uncapped = drive(mock_basket(260, cfg(0, 0)), name, path);
        let alone = drive(mock_basket(1, cfg(MARKET_CAP, PAYER_CAP)), name, path);
        runs.push((capped, uncapped, alone.first_exit_s));
    }
    let (name, path) = ("pump ×3 / 60 s", pump_3x_60s as Path);
    let capped = drive(pamm_basket(260, cfg(MARKET_CAP, PAYER_CAP)), name, path);
    let uncapped = drive(pamm_basket(260, cfg(0, 0)), name, path);
    let alone = drive(pamm_basket(1, cfg(MARKET_CAP, PAYER_CAP)), name, path);
    runs.push((capped, uncapped, alone.first_exit_s));

    let mut md = String::new();
    let _ = writeln!(
        md,
        "# m7_cascade — T-CASCADE (ARCH §6.6, §8.2; C0-17)\n\nGenerated by `services/keeper-litesvm` `cascade::t_cascade_a_launch_cap_basket_clears_under_the_block_budget` (`M7_CASCADE_WRITE=1`; release). Baskets of `min_notional` positions (≈ 0.1 SOL each, one borrower each): on the mock venue filling the 25-SOL launch cap; on the PumpSwap `mature` fork as many as its caps admit. The keeper every slot (two a second). Requested CU = the keeper's `⌈simulated × 1.1⌉` limits, charged per slot against each writable account (`MAX_WRITABLE_ACCOUNT_UNITS` = 24,000,000, ARCH V10). \"uncapped\" = the same basket with both caps 0 (LiteSVM enforces no block limit: the optimistic bound); \"isolated\" = one position alone on the same path (its entry price differs by the basket's own impact).\n\n| venue | path | positions (stopped by) | caps (market / payer, CU per slot) | liquidations | S1/S2 settlements | deferred | first exit | basket cleared | uncapped cleared | isolated first exit | peak requested CU in a slot (account) | peak liquidations in a slot |\n|---|---|---|---|---|---|---|---|---|---|---|---|---|"
    );
    for (c, u, iso) in &runs {
        let _ = writeln!(md, "{}", md_row(c, Some(u), *iso));
    }
    for (_, u, _) in &runs {
        let _ = writeln!(md, "{}", md_row(u, None, None));
    }
    let (c0, _, _) = &runs[0];
    let (cp, _, _) = runs.last().expect("pumpswap");
    let mock_liq = runs[1].0.exit_cu.1.max(1);
    let _ = writeln!(
        md,
        "\nThe positions: mock tier {} (`L` = {} bps); PumpSwap tier {} (`L` = {} bps).",
        c0.tier, c0.cr_liq_bps, cp.tier, cp.cr_liq_bps
    );
    let per_slot = |cu: u64| MARKET_CAP / cu;
    let _ = writeln!(
        md,
        "\nPer-exit requested CU: mock `liquidate` {}–{}; PumpSwap `liquidate` {}–{} (`tests/bench/m7_keeper`: PumpSwap 214,447, the curve 147,131, AMM v4 104,764, CPMM 124,787). At 12 M per market per slot that is {} mock / {} PumpSwap / {} curve / {} AMM v4 / {} CPMM liquidations a slot — {} / {} / {} / {} / {} a second at 2.5 slots a second; a 250-position basket clears in {} / {} / {} / {} / {} slots.",
        runs[1].0.exit_cu.0,
        mock_liq,
        cp.exit_cu.0,
        cp.exit_cu.1,
        per_slot(mock_liq),
        per_slot(214_447),
        per_slot(147_131),
        per_slot(104_764),
        per_slot(124_787),
        per_slot(mock_liq) * 5 / 2,
        per_slot(214_447) * 5 / 2,
        per_slot(147_131) * 5 / 2,
        per_slot(104_764) * 5 / 2,
        per_slot(124_787) * 5 / 2,
        250u64.div_ceil(per_slot(mock_liq)),
        250u64.div_ceil(per_slot(214_447)),
        250u64.div_ceil(per_slot(147_131)),
        250u64.div_ceil(per_slot(104_764)),
        250u64.div_ceil(per_slot(124_787)),
    );
    slot_table(
        &mut md,
        "Slots of the mock gap ×3 basket (S1: one `sol_settle` a second per armed position)",
        &runs[0].0,
    );
    slot_table(
        &mut md,
        "Slots of the mock pump ×3 / 60 s basket",
        &runs[1].0,
    );
    slot_table(&mut md, "Slots of the PumpSwap pump ×3 / 60 s basket", cp);
    slot_table(
        &mut md,
        "The mock pump ×3 / 60 s basket without the policy (both caps 0)",
        &runs[1].1,
    );
    println!("{md}");

    let mut j = String::new();
    let _ = writeln!(
        j,
        "{{\n  \"schema\": \"shortwin.m7.cascade/2\",\n  \"account_block_cu\": {ACCOUNT_BLOCK_CU},\n  \"market_cu_per_slot\": {MARKET_CAP},\n  \"payer_cu_per_slot\": {PAYER_CAP},\n  \"runs\": ["
    );
    let n = runs.len();
    for (i, (c, u, iso)) in runs.iter().enumerate() {
        let _ = writeln!(j, "    {},", json_row(c, u.last_exit_s, *iso));
        let _ = writeln!(
            j,
            "    {}{}",
            json_row(u, None, None),
            if i + 1 < n { "," } else { "" }
        );
    }
    let _ = writeln!(j, "  ]\n}}");
    println!("{j}");

    // ---- the gate ----
    for (c, u, _) in &runs {
        let what = format!("{} {}", c.venue, c.path);
        if c.venue == "mock" {
            assert!(c.positions >= 240, "{what}: {} positions", c.positions);
            assert_eq!(
                c.stopped_by, "MarketNotionalCap",
                "{what}: the launch cap binds"
            );
        }
        assert_eq!(u.positions, c.positions, "{what}");
        // Under the policy no account is asked for more than its block budget in a slot, and no
        // market (nor its vault) for more than its cap.
        assert!(c.peak.0 <= ACCOUNT_BLOCK_CU, "{what}: {:?}", c.peak);
        for r in &c.rows {
            for acc in ["Market", "MarketVault"] {
                assert!(
                    r.per_account.get(acc).copied().unwrap_or(0) <= MARKET_CAP,
                    "{what}: {r:?}"
                );
            }
            assert!(
                r.per_account.get("keeper fee payer").copied().unwrap_or(0) <= PAYER_CAP,
                "{what}: {r:?}"
            );
        }
        // The basket clears within 2 s of its first exit, and the policy costs at most 1 s against
        // the uncapped (optimistic) run.
        let (first, cleared) = (
            c.first_exit_s.expect("an exit"),
            c.last_exit_s.expect("cleared"),
        );
        assert!(
            cleared - first <= 2,
            "{what}: first exit {first} s, cleared {cleared} s"
        );
        let bound = u.last_exit_s.expect("uncapped cleared");
        assert!(
            cleared <= bound + 1,
            "{what}: cleared at {cleared} s, uncapped at {bound} s"
        );
    }
    // The policy is exercised: the mock liquidation cascade defers exits to a later slot, and
    // without it the keeper would ask the `Market` for more than half its block budget.
    assert!(runs[1].0.deferred > 0 && runs[1].1.peak.0 > MARKET_CAP);
    if std::env::var_os("M7_CASCADE_WRITE").is_some() {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/bench/m7_cascade");
        std::fs::create_dir_all(&dir).expect("m7_cascade dir");
        std::fs::write(dir.join("results.md"), md).expect("results.md");
        std::fs::write(dir.join("results.json"), j).expect("results.json");
    }
}
