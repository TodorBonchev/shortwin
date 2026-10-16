//! The upgrade window (pump-final §7, §15 "Release coordination"; review R-8; §14 "Keeper"): the
//! keeper across short.win's own Governance upgrade from release 1.0.0 to the build under test,
//! on pump's 2026-10-13 build.
//!
//! A Governance upgrade executes at a slot nobody chooses, so the keeper ships first, with both
//! curve TradeExtras lists — shortwin 1.0.0's 20 keys (V2: `sell_v2` / `buy_v2`) and the build under
//! test's 10 (V3: `sell_v3` / `buy_v3`) — and picks the one the deployed short.win parses, per
//! short.win's ProgramData deploy slot, by simulation: V3 first at a new deploy slot (and at
//! start-up), V2 when V3 fails on account parsing, the format that gets past account parsing kept
//! until the deploy slot changes. Every send is simulated first, so no curve exit is sent with the
//! wrong list.
//!
//! [`the_keeper_liquidates_with_v2_on_1_0_0_and_with_v3_on_the_first_tick_after_the_upgrade`]:
//! short.win 1.0.0 (`tests/fixtures/shortwin/shortwin_1_0_0.so`, checked against AUDIT §0's
//! hashes) with two curve markets whose positions 1.0.0's adapter opened (the v2 list). A real pump
//! makes the first market's two positions liquidatable: the keeper's first curve exit simulates V3
//! (refused `BadRemainingAccounts`: 1.0.0 takes 20 keys), then V2, and lands with V2; the second
//! goes with V2 at once (known for the deploy slot). The keeper then stops ticking while the second
//! market is pumped and cranked until its position is liquidatable on 1.0.0; the loader's upgrade
//! to the build under test lands (`Env::upgrade_shortwin`: `ExtendProgram`, then `Upgrade` signed
//! by the upgrade authority, at the test's slot); and the keeper's **first tick after the
//! upgrade** liquidates that position with V3 — a new deploy slot, simulated with V3 first, which
//! parses. No transaction the keeper sent failed.
//!
//! [`on_1_0_0_a_position_past_rt_waits_on_sol_settle_and_a_stale_deploy_slot_switches_at_once`]
//! (§15 step 1): on 1.0.0 the crowd leaves `near166`'s curve `rT = q/2` under an expired position
//! of `q`. 1.0.0 parses the v2 list and refuses the buy past `rT` (`CurveStraddle`); the keeper
//! sends the position's `sol_settle` instead, at most once a second, and pages nothing. After the
//! upgrade a node still behind it answers the keeper's header read with 1.0.0's deploy slot: the
//! known V2 is refused on account parsing (`VenueAccountMismatch`), V3 is simulated at once and
//! the exit lands past `rT` (pump's synthetic migration), the market graduating in the same tick.
//!
//! [`an_exit_simulated_before_the_deploy_slot_read_is_not_sent`] (AIR-01's lag): on 1.0.0 the
//! keeper expires a position with V2; then, past a second position's deadline, a node of its RPC
//! pool stays on 1.0.0's bank for the keeper's next two exit simulations
//! (`LiteSvmChain::lag_exit_simulations`) and the upgrade lands. The keeper reads the new deploy
//! slot, and the exit's simulation, from a bank before it, is not sent — not V3's refusal by
//! 1.0.0 then V2 passing there and failing on chain — nor taken as evidence of a format; once the
//! node has caught up, V3 parses and the exit lands. No send failed.
//!
//! [`neither_list_parsing_is_alerted_once_and_a_format_found_on_the_deploy_slot_stays`]: the
//! backend refuses every curve exit's simulation on account parsing
//! (`LiteSvmChain::refuse_exit_simulations`). With no format known, `CurveExtrasFormatUnknown`
//! is raised once over every attempt (each refusal still `ExitRefused`), no format is known, and
//! each attempt counts in `curve_format_failures`; once the refusals stop, the next exit finds V3.
//! Then, on the deploy slot whose format that exit found, another position's VenueExtras fail
//! after the TradeExtras parser (`LiteSvmChain::refuse_parsed_exit_simulations`: V3 refused,
//! V2 by the parser at T2): its own refusal — no format alert, the format kept, and the known V3's
//! refusal the one booked and paged, with the program's error line.

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::AccountMeta;
use shortwin::errors::ShortwinError;
use shortwin_keeper::venue::CurveExtrasFormat;
use shortwin_litesvm_tests::engine::{Borrower, VenueMarket, expire_ix, liquidate_ix};
use shortwin_litesvm_tests::env::{
    SHORTWIN_1_0_0_EXECUTABLE_HASH, program_so_path, shortwin_1_0_0_so, trimmed_elf,
};
use shortwin_litesvm_tests::fixtures::MarketKeys;
use shortwin_litesvm_tests::fork::{self, PumpCoin, PumpCurve};
use shortwin_litesvm_tests::pump::{self, CurveUser, PumpMarket, PumpSpec};
use shortwin_litesvm_tests::{pda, read};
use solana_signer::Signer;
use solana_transaction::Transaction;

use crate::common::*;

const SOL: u64 = 1_000_000_000;

/// A curve market as shortwin 1.0.0's clients address it: the same ReadSet, the v2-era
/// TradeExtras T0–T19 (`pump::v2_trade_extras`).
struct V2<'a> {
    pm: &'a PumpMarket,
    extras: Vec<AccountMeta>,
}

impl VenueMarket for V2<'_> {
    fn keys(&self) -> &MarketKeys {
        &self.pm.keys
    }
    fn read_set(&self, writable: bool) -> Vec<AccountMeta> {
        self.pm.read_set(writable)
    }
    fn trade_extras(&self) -> Vec<AccountMeta> {
        self.extras.clone()
    }
    fn cu_limit(&self) -> Option<u32> {
        self.pm.cu_limit()
    }
}

fn v2<'a>(env: &Env, pm: &'a PumpMarket) -> V2<'a> {
    V2 {
        pm,
        extras: pump::v2_trade_extras(env, pm),
    }
}

/// short.win 1.0.0 on the pump fork (as T-SELF-UPGRADE's): `initialize_config` with the harness
/// defaults, the current pump, PumpSwap and pump-fees builds and statics, `coins`, mainnet's Rent
/// and `near166`'s snapshot clock.
fn fork_on_1_0_0(coins: &[PumpCoin]) -> (Env, Vec<PumpCurve>) {
    let mut env = Env::with_program_so(&shortwin_1_0_0_so());
    assert_eq!(
        env.deployed_shortwin_hash(),
        SHORTWIN_1_0_0_EXECUTABLE_HASH,
        "1.0.0 runs"
    );
    let args = env.default_init_args();
    ok(env.initialize_config(args));
    env.load_pump_programs();
    env.load_pump_statics();
    let curves = coins.iter().map(|c| env.load_pump_coin(*c)).collect();
    env.set_pump_rent();
    let s = PumpCoin::Near166.snapshot();
    env.set_clock(s.block_time, s.slot);
    (env, curves)
}

/// Lists `c` on 1.0.0, lends `40·q` (`q = vT/200`), waits out the warm-up and cranks.
fn add_market(env: &mut Env, c: PumpCurve) -> (PumpMarket, u64) {
    ok(env.list_pump(&c));
    let q = c.state(&env.svm).virtual_token_reserves / 200;
    let pm = env.pump_market_listed(PumpSpec::new(c.coin, q * 40), c);
    env.crank(&pm);
    (pm, q)
}

/// An open of `q` by 1.0.0's curve adapter (the v2 list).
fn open_v2(env: &mut Env, pm: &PumpMarket, q: u64) -> (Borrower, u64) {
    let mut b = env.borrower();
    let view = v2(env, pm);
    let (res, nonce) = env.open(&view, &mut b, q);
    ok(res);
    (b, nonce)
}

/// The crowd's real pump of `c`: a buy of 40 % of `vT` (T-SELF-UPGRADE's).
fn pump_curve(env: &mut Env, c: &PumpCurve) {
    let crowd = CurveUser::new(env, c);
    env.fund(&crowd.kp.pubkey(), 2_000 * SOL);
    let vt = c.state(&env.svm).virtual_token_reserves;
    ok(crowd.buy(env, c, vt / 10 * 4, u64::MAX));
}

/// Whether an outside keeper's `liquidate` with `view`'s lists (after the compute budget) would
/// land now.
fn would_liquidate(
    env: &Env,
    view: &impl VenueMarket,
    kp: &solana_keypair::Keypair,
    borrower: &Pubkey,
    nonce: u64,
) -> Result<(), Option<u32>> {
    would_land(env, kp, liquidate_ix(view, &kp.pubkey(), borrower, nonce))
}

/// Whether an outside keeper's exit `ix` (after the compute budget) would land now.
fn would_land(
    env: &Env,
    kp: &solana_keypair::Keypair,
    ix: anchor_lang::solana_program::instruction::Instruction,
) -> Result<(), Option<u32>> {
    let tx = Transaction::new_signed_with_payer(
        &[pump::cu_limit_ix(pump::CURVE_KEEPER_EXIT_CU), ix],
        Some(&kp.pubkey()),
        &[kp],
        env.svm.latest_blockhash(),
    );
    match env.svm.simulate_transaction(tx) {
        Ok(_) => Ok(()),
        Err(f) => Err(shortwin_litesvm_tests::errors::custom_code(&Err(f))),
    }
}

/// The crowd buys `c` until `rt_left` real tokens remain (a real `buy_v2`, never past `rT`).
fn buy_down_to(env: &mut Env, c: &PumpCurve, rt_left: u64) {
    let crowd = CurveUser::new(env, c);
    env.fund(&crowd.kp.pubkey(), 2_000 * SOL);
    let rt = c.state(&env.svm).real_token_reserves;
    ok(crowd.buy(env, c, rt - rt_left, u64::MAX));
    assert_eq!(c.state(&env.svm).real_token_reserves, rt_left);
}

/// Warps past `deadline` with a crank at most every 10 minutes, the last one a second after it
/// (the observation chain kept; the exits' reference fresh).
fn to_deadline(env: &mut Env, pm: &PumpMarket, deadline: i64) {
    while env.now() < deadline + 1 {
        let step = (deadline + 1 - env.now()).min(600);
        env.warp(step);
        env.crank(pm);
    }
}

/// The `CurveExtrasFormatUnknown` alerts among `alerts`.
fn format_unknown(alerts: &[Alert]) -> Vec<Alert> {
    alerts
        .iter()
        .filter(|a| matches!(a, Alert::CurveExtrasFormatUnknown { .. }))
        .cloned()
        .collect()
}

/// short.win's ProgramData deploy slot now.
fn deploy_slot(h: &Harness) -> u64 {
    h.with_env(|env| {
        let a = env
            .svm
            .get_account(&pda::program_data())
            .expect("ProgramData");
        shortwin_venue_state::program_data::deploy_slot(&a.owner.to_bytes(), &a.data)
            .expect("a deploy slot")
    })
}

/// The upgrade window (module docs).
#[test]
fn the_keeper_liquidates_with_v2_on_1_0_0_and_with_v3_on_the_first_tick_after_the_upgrade() {
    let (mut env, curves) = fork_on_1_0_0(&[PumpCoin::T22_115, PumpCoin::Legacy115]);
    let (m1, q1) = add_market(&mut env, curves[0]);
    let (m2, q2) = add_market(&mut env, curves[1]);
    env.warp(1);
    // 1.0.0 takes the v2 list only: a 10-key (v3) open is refused before any CPI.
    let mut x = env.borrower();
    let res = env.open(&m1, &mut x, q1).0;
    assert_eq!(
        shortwin_litesvm_tests::errors::custom_code(&res),
        Some(u32::from(ShortwinError::BadRemainingAccounts))
    );
    let (a, na) = open_v2(&mut env, &m1, q1);
    let (c, nc) = open_v2(&mut env, &m1, q1);
    let (b, nb) = open_v2(&mut env, &m2, q2);
    env.crank(&m1);
    env.crank(&m2);
    let pos = |pm: &PumpMarket, w: &Borrower, n: u64| pda::position(&pm.market(), &w.key(), n).0;
    let (pa, pc, pb) = (pos(&m1, &a, na), pos(&m1, &c, nc), pos(&m2, &b, nb));
    let mut h = Harness::new(env, covering_all(KeeperConfig::default()));
    h.chain().set_priority_fees(vec![5_000]);
    let slot0 = deploy_slot(&h);
    for _ in 0..3 {
        let r = h.second().clone();
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert!(r.exits.is_empty(), "nothing to exit yet");
    }
    assert_eq!(h.keeper.curve_format(), None, "no curve exit yet");

    // A real pump of the first market: the keeper liquidates both positions on 1.0.0 with V2.
    h.with_env(|env| pump_curve(env, &m1.curve));
    let mut probes = Vec::new();
    let mut v2_exits = Vec::new();
    for _ in 0..900 {
        let r = h.second().clone();
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        probes.extend(r.curve_formats.iter().cloned());
        for e in &r.exits {
            if e.result.landed().is_some() && e.kind == shortwin_keeper::ExitKind::Liquidate {
                assert_eq!(
                    e.curve_format,
                    Some(CurveExtrasFormat::V2),
                    "1.0.0: the v2 list"
                );
                v2_exits.push(e.position);
            }
        }
        if v2_exits.len() == 2 {
            break;
        }
    }
    v2_exits.sort();
    let mut want = vec![pa, pc];
    want.sort();
    assert_eq!(v2_exits, want, "both liquidated on 1.0.0");
    // The first curve exit found the format: V3 refused on account parsing, then V2; the second
    // went with V2 at once.
    assert_eq!(probes.len(), 1, "{probes:?}");
    assert_eq!(probes[0].deploy_slot, Some(slot0));
    assert_eq!(
        probes[0].refused,
        [(
            CurveExtrasFormat::V3,
            Some(u32::from(ShortwinError::BadRemainingAccounts))
        )]
    );
    assert_eq!(probes[0].chosen, Some(CurveExtrasFormat::V2));
    assert_eq!(
        h.keeper.curve_format(),
        Some((Some(slot0), CurveExtrasFormat::V2))
    );
    let sent_before = h.chain().sent().len();

    // The keeper stops ticking. The second market is pumped and cranked until its position is
    // liquidatable on 1.0.0 (an outside key's v2 liquidation would land).
    let outsider = h.with_env(|env| env.funded_keypair());
    h.with_env(|env| pump_curve(env, &m2.curve));
    let mut ready = false;
    for _ in 0..900 {
        ready = h.with_env(|env| {
            env.warp(1);
            env.crank(&m2);
            let view = v2(env, &m2);
            would_liquidate(env, &view, &outsider, &b.key(), nb).is_ok()
        });
        if ready {
            break;
        }
    }
    assert!(
        ready,
        "the second market's position became liquidatable on 1.0.0"
    );
    assert!(read::borsh::<shortwin::state::ShortPosition>(&h.env.borrow().svm, &pb).is_some());

    // Governance's upgrade to the build under test, at the test's slot.
    let new_elf = std::fs::read(program_so_path()).expect("the build under test");
    let upgraded_at = h.with_env(|env| env.upgrade_shortwin(&new_elf));
    assert_eq!(
        h.with_env(|env| env.deployed_shortwin_hash()),
        fork::sha256_hex(trimmed_elf(&new_elf)),
        "the build under test runs"
    );
    let slot1 = deploy_slot(&h);
    assert_eq!(slot1, upgraded_at);
    assert_ne!(slot1, slot0);
    // The position is still liquidatable on the new build, now with the v3 list (a fresh
    // reference: the crank the keeper would otherwise make first).
    h.with_env(|env| {
        env.crank(&m2);
        assert!(would_liquidate(env, &m2, &outsider, &b.key(), nb).is_ok());
        let view = v2(env, &m2);
        assert!(
            matches!(
                would_liquidate(env, &view, &outsider, &b.key(), nb),
                Err(Some(code)) if code == u32::from(ShortwinError::BadRemainingAccounts)
                    || code == u32::from(ShortwinError::VenueAccountMismatch)
            ),
            "the v2 list no longer parses"
        );
    });

    // The keeper's first tick after the upgrade liquidates with V3.
    let r = h.second().clone();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    let e = r
        .exits
        .iter()
        .find(|e| e.position == pb && e.result.landed().is_some())
        .unwrap_or_else(|| panic!("the first tick after the upgrade: {:?}", r.exits));
    assert_eq!(e.kind, shortwin_keeper::ExitKind::Liquidate);
    assert_eq!(
        e.curve_format,
        Some(CurveExtrasFormat::V3),
        "the build under test: v3"
    );
    assert_eq!(r.curve_formats.len(), 1, "{:?}", r.curve_formats);
    let probe = &r.curve_formats[0];
    assert_eq!(probe.deploy_slot, Some(slot1));
    assert!(probe.refused.is_empty(), "V3 first, and it parses");
    assert_eq!(probe.chosen, Some(CurveExtrasFormat::V3));
    assert_eq!(
        h.keeper.curve_format(),
        Some((Some(slot1), CurveExtrasFormat::V3))
    );
    assert!(read::borsh::<shortwin::state::ShortPosition>(&h.env.borrow().svm, &pb).is_none());

    // No failed send, before or after the upgrade.
    let sent = h.chain().sent();
    assert!(sent.len() > sent_before);
    let failed: Vec<_> = sent.iter().filter(|s| !s.ok).collect();
    assert!(failed.is_empty(), "failed sends: {failed:?}");
    assert!(
        !r.alerts
            .iter()
            .any(|a| matches!(a, Alert::CurveExtrasFormatUnknown { .. })),
        "{:?}",
        r.alerts
    );
    eprintln!(
        "upgrade window: 1.0.0 at deploy slot {slot0} (V2 after a V3 probe), upgraded at slot \
         {slot1}, liquidated with V3 on the first tick after it; {} sends, none failed",
        sent.len()
    );
}

/// §15 step 1 on 1.0.0, then a stale deploy-slot read across the upgrade (module docs).
#[test]
fn on_1_0_0_a_position_past_rt_waits_on_sol_settle_and_a_stale_deploy_slot_switches_at_once() {
    let (mut env, curves) = fork_on_1_0_0(&[PumpCoin::Near166]);
    let (pm, q) = add_market(&mut env, curves[0]);
    env.warp(1);
    let (b, nb) = open_v2(&mut env, &pm, q);
    env.crank(&pm);
    env.warp(2);
    // The crowd leaves `rT = q/2` (T-SELF-UPGRADE's synthetic expiry), then the deadline passes.
    buy_down_to(&mut env, &pm.curve, q / 2);
    let deadline = env.position(&pm, &b.key(), nb).deadline;
    to_deadline(&mut env, &pm, deadline);
    let p = pda::position(&pm.market(), &b.key(), nb).0;
    let mut h = Harness::new(env, covering_all(KeeperConfig::default()));
    h.chain().set_priority_fees(vec![5_000]);
    let slot0 = deploy_slot(&h);
    let straddle = u32::from(ShortwinError::CurveStraddle);

    // On 1.0.0: the buy past `rT` refused, `sol_settle` instead, nothing paged.
    let mut waited = 0;
    for _ in 0..8 {
        let r = h.second().clone();
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert!(
            !r.alerts.iter().any(|a| matches!(
                a,
                Alert::ExitRefused { .. } | Alert::CurveExtrasFormatUnknown { .. }
            )),
            "{:?}",
            r.alerts
        );
        let mine: Vec<_> = r.exits.iter().filter(|e| e.position == p).collect();
        assert!(
            mine.iter()
                .filter(|e| e.kind == ExitKind::SolSettle)
                .count()
                <= 1,
            "one sol_settle a second: {mine:?}"
        );
        let Some(refusal) = mine.iter().find(|e| e.kind != ExitKind::SolSettle) else {
            continue;
        };
        assert_eq!(refusal.curve_format, Some(CurveExtrasFormat::V2), "1.0.0");
        assert!(
            matches!(&refusal.result, ExitResult::Refused { code: Some(c), .. } if *c == straddle),
            "{:?}",
            refusal.result
        );
        let settle = mine
            .iter()
            .find(|e| e.kind == ExitKind::SolSettle)
            .unwrap_or_else(|| panic!("sol_settle follows the refusal: {mine:?}"));
        let out = settle
            .result
            .landed()
            .unwrap_or_else(|| panic!("{:?}", settle.result));
        assert!(
            shortwin_keeper::events::of::<shortwin::events::SolSettle>(out).is_empty(),
            "1.0.0 prices the straddle: its buyback is allowed, nothing settles"
        );
        waited += 1;
    }
    assert!(waited >= 4, "the keeper waited {waited} s with sol_settle");
    assert!(read::borsh::<shortwin::state::ShortPosition>(&h.env.borrow().svm, &p).is_some());
    let probes: Vec<_> = h
        .reports
        .iter()
        .flat_map(|r| r.curve_formats.iter().cloned())
        .collect();
    assert_eq!(probes.len(), 1, "{probes:?}");
    assert_eq!(
        probes[0].refused,
        [(
            CurveExtrasFormat::V3,
            Some(u32::from(ShortwinError::BadRemainingAccounts))
        )]
    );
    assert_eq!(probes[0].chosen, Some(CurveExtrasFormat::V2));
    assert_eq!(
        h.keeper.curve_format(),
        Some((Some(slot0), CurveExtrasFormat::V2))
    );

    // The upgrade lands; a node still behind it answers the keeper's next header read of
    // short.win's ProgramData with 1.0.0's deploy slot.
    let new_elf = std::fs::read(program_so_path()).expect("the build under test");
    let upgraded_at = h.with_env(|env| env.upgrade_shortwin(&new_elf));
    let slot1 = deploy_slot(&h);
    assert_eq!(slot1, upgraded_at);
    h.chain().lag_headers_of(pda::program_data(), slot0, 1);
    let mut exit = None;
    for _ in 0..3 {
        let r = h.second().clone();
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        if let Some(e) = r.exits.iter().find(|e| {
            e.position == p && e.kind != ExitKind::SolSettle && e.result.landed().is_some()
        }) {
            exit = Some((e.clone(), r.clone()));
            break;
        }
    }
    let (e, r) = exit.expect("the exit past rT after the upgrade");
    assert_eq!(e.curve_format, Some(CurveExtrasFormat::V3));
    // The known V2 (for the stale read's deploy slot) refused on account parsing; V3 at once.
    assert_eq!(r.curve_formats.len(), 1, "{:?}", r.curve_formats);
    let probe = &r.curve_formats[0];
    assert_eq!(probe.deploy_slot, Some(slot0), "the stale read");
    assert_eq!(
        probe.refused,
        [(
            CurveExtrasFormat::V2,
            Some(u32::from(ShortwinError::VenueAccountMismatch))
        )]
    );
    assert_eq!(probe.chosen, Some(CurveExtrasFormat::V3));
    assert_eq!(
        h.keeper.curve_format(),
        Some((Some(slot0), CurveExtrasFormat::V3))
    );
    // pump's synthetic migration completed the curve; the keeper graduated it in the same tick.
    let out = e.result.landed().expect("landed");
    let changes: Vec<(u8, u8)> =
        shortwin_keeper::events::of::<shortwin::events::VenueStateChanged>(out)
            .iter()
            .map(|c| (c.from, c.to))
            .collect();
    assert_eq!(
        changes,
        [(
            shortwin::state::VenueKind::PumpCurve as u8,
            shortwin::state::VenueKind::PumpMigrating as u8
        )]
    );
    assert!(r.migrations.iter().any(|m| m.result.landed().is_some()));
    assert!(r.transitions.iter().any(|t| t.result.landed().is_some()));
    assert!(read::borsh::<shortwin::state::ShortPosition>(&h.env.borrow().svm, &p).is_none());
    assert!(
        h.reports
            .iter()
            .all(|r| format_unknown(&r.alerts).is_empty()),
        "no format alert"
    );
    let sent = h.chain().sent();
    let failed: Vec<_> = sent.iter().filter(|s| !s.ok).collect();
    assert!(failed.is_empty(), "failed sends: {failed:?}");
    eprintln!(
        "1.0.0: {waited} s of sol_settle past rT (CurveStraddle, unpaged); after the upgrade at \
         slot {slot1}, a header read of slot {slot0}: V2 refused, V3 at once, {} sends, none \
         failed",
        sent.len()
    );
}

/// A node behind the upgrade answering the keeper's curve exit simulations (module docs).
#[test]
fn an_exit_simulated_before_the_deploy_slot_read_is_not_sent() {
    let (mut env, curves) = fork_on_1_0_0(&[PumpCoin::Near166]);
    let (pm, q) = add_market(&mut env, curves[0]);
    env.warp(1);
    let (a, na) = open_v2(&mut env, &pm, q);
    env.crank(&pm);
    env.warp(30);
    env.crank(&pm);
    env.warp(1);
    let (b, nb) = open_v2(&mut env, &pm, q);
    let deadline_a = env.position(&pm, &a.key(), na).deadline;
    let deadline_b = env.position(&pm, &b.key(), nb).deadline;
    assert!(deadline_b > deadline_a + 20);
    to_deadline(&mut env, &pm, deadline_a);
    let pa = pda::position(&pm.market(), &a.key(), na).0;
    let pb = pda::position(&pm.market(), &b.key(), nb).0;
    let mut h = Harness::new(env, covering_all(KeeperConfig::default()));
    h.chain().set_priority_fees(vec![5_000]);
    let slot0 = deploy_slot(&h);

    // On 1.0.0 the keeper expires the first position with V2 (after a V3 probe).
    let mut expired = false;
    for _ in 0..5 {
        let r = h.second().clone();
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        if let Some(e) = r
            .exits
            .iter()
            .find(|e| e.position == pa && e.result.landed().is_some())
        {
            assert_eq!(
                (e.kind, e.curve_format),
                (ExitKind::Expire, Some(CurveExtrasFormat::V2))
            );
            expired = true;
            break;
        }
    }
    assert!(expired, "the first position expired on 1.0.0");
    assert_eq!(
        h.keeper.curve_format(),
        Some((Some(slot0), CurveExtrasFormat::V2))
    );

    // The keeper stops ticking until the second position's deadline has passed (its last crank's
    // bucket completed). A node of its RPC pool stays on this bank — 1.0.0, where the position
    // expires with the v2 list — for the keeper's next two exit simulations; the upgrade lands a
    // second later, and the v2 list no longer parses.
    let outsider = h.with_env(|env| env.funded_keypair());
    h.with_env(|env| {
        to_deadline(env, &pm, deadline_b);
        env.warp(1);
        let view = v2(env, &pm);
        assert_eq!(
            would_land(
                env,
                &outsider,
                expire_ix(&view, &outsider.pubkey(), &b.key(), nb)
            ),
            Ok(()),
            "1.0.0 expires it with the v2 list"
        );
    });
    let stale = h.with_env(|env| env.clock().slot);
    h.chain().lag_exit_simulations(2);
    h.with_env(|env| env.warp(1));
    let new_elf = std::fs::read(program_so_path()).expect("the build under test");
    let upgraded_at = h.with_env(|env| env.upgrade_shortwin(&new_elf));
    let slot1 = deploy_slot(&h);
    assert_eq!(slot1, upgraded_at);
    assert!(slot1 > stale, "{slot1} > {stale}");
    h.with_env(|env| {
        env.warp(1);
        let view = v2(env, &pm);
        assert!(
            matches!(
                would_land(env, &outsider, expire_ix(&view, &outsider.pubkey(), &b.key(), nb)),
                Err(Some(c)) if c == u32::from(ShortwinError::VenueAccountMismatch)
            ),
            "the build under test refuses the v2 list"
        );
    });

    // The keeper reads the new deploy slot. Its V3 exit, simulated on 1.0.0's bank, is not sent
    // and teaches nothing, twice: no format found, none alerted, nothing paged.
    let behind = format!("before short.win's deploy slot {slot1}");
    let mut held = 0;
    let mut landed = None;
    for i in 0..10 {
        let r = h.second().clone();
        assert!(
            !r.alerts.iter().any(|a| matches!(
                a,
                Alert::ExitRefused { .. } | Alert::CurveExtrasFormatUnknown { .. }
            )),
            "{i}: {:?}",
            r.alerts
        );
        let mine: Vec<_> = r.exits.iter().filter(|e| e.position == pb).collect();
        match mine.as_slice() {
            [] => assert!(r.errors.is_empty(), "{i}: {:?}", r.errors),
            [e] if matches!(&e.result, ExitResult::Error(m) if m.contains(&behind)) => {
                assert_eq!(
                    (e.kind, e.curve_format),
                    (ExitKind::Expire, Some(CurveExtrasFormat::V3)),
                    "{i}"
                );
                assert!(
                    r.errors.len() == 1 && r.errors[0].contains(&behind),
                    "{i}: {:?}",
                    r.errors
                );
                assert_eq!(r.curve_formats.len(), 1, "{i}: {:?}", r.curve_formats);
                let probe = &r.curve_formats[0];
                assert_eq!(
                    (probe.deploy_slot, probe.chosen),
                    (Some(slot1), None),
                    "{i}"
                );
                assert!(probe.refused.is_empty(), "{i}: {probe:?}");
                assert_eq!(
                    h.keeper.curve_format(),
                    Some((Some(slot0), CurveExtrasFormat::V2)),
                    "{i}: nothing learnt"
                );
                held += 1;
            }
            [e] if e.result.landed().is_some() => {
                landed = Some(((*e).clone(), r.clone()));
                break;
            }
            other => panic!("{i}: {other:?}"),
        }
    }
    assert_eq!(held, 2, "two exit simulations behind the deploy slot read");

    // The node has caught up: V3 parses and the exit lands.
    let (e, r) = landed.expect("the expiry once the node caught up");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(
        (e.kind, e.curve_format),
        (ExitKind::Expire, Some(CurveExtrasFormat::V3))
    );
    assert_eq!(r.curve_formats.len(), 1, "{:?}", r.curve_formats);
    assert!(r.curve_formats[0].refused.is_empty());
    assert_eq!(r.curve_formats[0].chosen, Some(CurveExtrasFormat::V3));
    assert_eq!(
        h.keeper.curve_format(),
        Some((Some(slot1), CurveExtrasFormat::V3))
    );
    assert!(read::borsh::<shortwin::state::ShortPosition>(&h.env.borrow().svm, &pb).is_none());
    assert!(
        h.reports
            .iter()
            .all(|r| format_unknown(&r.alerts).is_empty()),
        "no format alert"
    );
    let sent = h.chain().sent();
    let failed: Vec<_> = sent.iter().filter(|s| !s.ok).collect();
    assert!(failed.is_empty(), "failed sends: {failed:?}");
    eprintln!(
        "a node behind the upgrade at slot {slot1} (its bank at slot {stale}): 2 exit simulations \
         not sent, then V3 at once; {} sends, none failed",
        sent.len()
    );
}

/// Neither list parsing (module docs; pump-final §7's alert), on the build under test.
#[test]
fn neither_list_parsing_is_alerted_once_and_a_format_found_on_the_deploy_slot_stays() {
    let (mut env, c) = Env::fork_pump_coin(PumpCoin::Near166);
    ok(env.list_pump(&c));
    let q = c.state(&env.svm).virtual_token_reserves / 200;
    let pm = env.pump_market_listed(PumpSpec::new(PumpCoin::Near166, q * 40), c);
    env.crank(&pm);
    env.warp(1);
    let mut a = env.borrower();
    let (res, na) = env.open(&pm, &mut a, q);
    ok(res);
    env.warp(30);
    env.crank(&pm);
    env.warp(1);
    let mut b = env.borrower();
    let (res, nb) = env.open(&pm, &mut b, q);
    ok(res);
    let deadline_a = env.position(&pm, &a.key(), na).deadline;
    let deadline_b = env.position(&pm, &b.key(), nb).deadline;
    assert!(deadline_b > deadline_a + 20);
    to_deadline(&mut env, &pm, deadline_a);
    let pa = pda::position(&pm.market(), &a.key(), na).0;
    let pb = pda::position(&pm.market(), &b.key(), nb).0;
    let mut h = Harness::new(env, covering_all(KeeperConfig::default()));
    h.chain().set_priority_fees(vec![5_000]);
    let slot = deploy_slot(&h);
    let bra = u32::from(ShortwinError::BadRemainingAccounts);
    let both = [
        (CurveExtrasFormat::V3, Some(bra)),
        (CurveExtrasFormat::V2, Some(bra)),
    ];
    let exit_refused = |r: &TickReport, p: Pubkey| {
        r.alerts
            .iter()
            .filter(|x| {
                matches!(x, Alert::ExitRefused { position, code: Some(c), .. }
                    if *position == p && *c == bra)
            })
            .count()
    };

    // No format known (start-up): both lists refused at every attempt; one alert.
    h.chain().refuse_exit_simulations(Some(bra));
    let mut attempts = 0u64;
    let mut paged = 0;
    let mut unknown = Vec::new();
    for _ in 0..6 {
        let r = h.second().clone();
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        for f in &r.curve_formats {
            assert_eq!(
                (f.position, f.deploy_slot, f.chosen),
                (pa, Some(slot), None)
            );
            assert_eq!(f.refused, both);
            attempts += 1;
        }
        paged += exit_refused(&r, pa);
        unknown.extend(format_unknown(&r.alerts));
        assert_eq!(h.keeper.curve_format(), None);
    }
    assert!(attempts >= 4, "{attempts} attempts");
    assert_eq!(h.keeper.metrics.curve_format_failures, attempts);
    assert_eq!(h.keeper.metrics.curve_format_probes, attempts);
    assert_eq!(
        u64::try_from(paged).unwrap(),
        attempts,
        "each refusal paged"
    );
    assert_eq!(
        unknown.len(),
        1,
        "once over {attempts} attempts: {unknown:?}"
    );
    let Alert::CurveExtrasFormatUnknown {
        market,
        position,
        deploy_slot: at,
        detail,
    } = &unknown[0]
    else {
        unreachable!()
    };
    assert_eq!((*market, *position, *at), (pm.market(), pa, Some(slot)));
    assert!(
        detail.starts_with("V3: ")
            && detail.contains("; V2: ")
            && detail.matches("Error Code: Injected").count() == 2,
        "{detail}"
    );

    // The refusals stop: the next exit finds V3 (none known: V3 first) and expires the position.
    h.chain().refuse_exit_simulations(None);
    let mut expired = None;
    for _ in 0..3 {
        let r = h.second().clone();
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        if let Some(e) = r
            .exits
            .iter()
            .find(|e| e.position == pa && e.result.landed().is_some())
        {
            expired = Some((e.clone(), r.clone()));
            break;
        }
    }
    let (e, r) = expired.expect("the expiry once the refusals stop");
    assert_eq!(
        (e.kind, e.curve_format),
        (ExitKind::Expire, Some(CurveExtrasFormat::V3))
    );
    assert_eq!(r.curve_formats.len(), 1, "{:?}", r.curve_formats);
    assert!(r.curve_formats[0].refused.is_empty());
    assert_eq!(r.curve_formats[0].chosen, Some(CurveExtrasFormat::V3));
    assert_eq!(
        h.keeper.curve_format(),
        Some((Some(slot), CurveExtrasFormat::V3))
    );

    // The format found on this deploy slot stays: both lists refused for the second position are
    // its own refusal. Its VenueExtras fail after the TradeExtras parser: V3 is refused with that
    // error, V2 by the parser at T2. The known V3's refusal is booked and paged, with the
    // program's error line.
    h.chain().refuse_parsed_exit_simulations(Some(bra));
    let vam = u32::from(ShortwinError::VenueAccountMismatch);
    let failures = h.keeper.metrics.curve_format_failures;
    let mut attempts_b = 0u64;
    for _ in 0..120 {
        let r = h.second().clone();
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert!(format_unknown(&r.alerts).is_empty(), "{:?}", r.alerts);
        for f in &r.curve_formats {
            assert_eq!(
                (f.position, f.deploy_slot, f.chosen),
                (pb, Some(slot), None)
            );
            assert_eq!(
                f.refused,
                [
                    (CurveExtrasFormat::V3, Some(bra)),
                    (CurveExtrasFormat::V2, Some(vam))
                ]
            );
            let e = r
                .exits
                .iter()
                .find(|e| e.position == pb && e.kind != ExitKind::SolSettle)
                .unwrap_or_else(|| panic!("{:?}", r.exits));
            assert_eq!(e.curve_format, Some(CurveExtrasFormat::V3), "the known one");
            assert!(
                matches!(&e.result, ExitResult::Refused { code: Some(c), .. } if *c == bra),
                "{:?}",
                e.result
            );
            assert_eq!(exit_refused(&r, pb), 1, "{:?}", r.alerts);
            let detail = r
                .alerts
                .iter()
                .find_map(|x| match x {
                    Alert::ExitRefused {
                        position, detail, ..
                    } if *position == pb => Some(detail.clone()),
                    _ => None,
                })
                .expect("paged");
            assert!(detail.contains("Error Code: Injected"), "{detail}");
            attempts_b += 1;
        }
        assert_eq!(
            h.keeper.curve_format(),
            Some((Some(slot), CurveExtrasFormat::V3)),
            "the format found stays"
        );
        if attempts_b >= 3 {
            break;
        }
    }
    assert!(h.now() > deadline_b);
    assert_eq!(attempts_b, 3);
    assert_eq!(
        h.keeper.metrics.curve_format_failures,
        failures + attempts_b
    );

    // The refusals stop: the known V3 at once, no probe.
    h.chain().refuse_exit_simulations(None);
    let r = h.second().clone();
    let e = r
        .exits
        .iter()
        .find(|e| e.position == pb && e.result.landed().is_some())
        .unwrap_or_else(|| panic!("{:?}", r.exits));
    assert_eq!(e.curve_format, Some(CurveExtrasFormat::V3));
    assert!(r.curve_formats.is_empty(), "{:?}", r.curve_formats);
    let sent = h.chain().sent();
    assert!(sent.iter().all(|s| s.ok), "no failed send");
    eprintln!(
        "neither list: {attempts} attempts, 1 CurveExtrasFormatUnknown; then V3 at slot {slot}, \
         and {attempts_b} attempts of the second position with the format kept, unalerted, V3's \
         refusal paged"
    );
}
