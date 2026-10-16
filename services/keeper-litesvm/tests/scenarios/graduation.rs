//! Keeper duty 6 through the graduation (M5, M6; ARCH §2.5 "On → PumpSwap", §5.1, §7.1 duty 6;
//! ADOPTED Q14, Q16; pump-final §7): the **release** short.win and the **dumped** pump (2026-10-13),
//! pump-fees and PumpSwap ELFs on the mainnet fork of the program tests
//! (`shortwin_litesvm_tests::pump`, `::pamm`), driven by the keeper with its **default**
//! configuration unless a test says otherwise. Every keeper transaction runs the invariant hooks
//! after every instruction.
//!
//! - **`migrate_v2` and the transition as soon as a curve completes** (M6 default): an outside buy
//!   completes the curve of a covered market; at its next tick the keeper migrates it (its account
//!   list is the harness's own derivation's), then — in the same second — sends the transition
//!   (the pool accounts the trades require, `crank_price` onto the pool, the pool's first
//!   observation): the market is `PumpSwap`, its pAMM accounts exist, the warm-up is marked, and
//!   the keeper observes it every second until the warm-up is over (no positions: M6's keeper
//!   warm-up), once. With `migrate` off, someone else's migration is still moved onto the pool.
//! - **A graduation with positions**: a curve position survives an outside completion; the keeper
//!   migrates and transitions; the pool is pumped; the keeper sends no exit during the warm-up and
//!   liquidates the position **on the pool** in the first second the program accepts it (an outside
//!   key's simulation refused every second before), with the PumpSwap TradeExtras.
//! - **A completing liquidation** (pump-final §5.2, §14 "Keeper"; T-SYNTH-2 at the keeper): on a
//!   D@80 % curve the crowd buys `rT` below two positions' `Q`; the keeper's `liquidate` of the
//!   first buys past `rT` by pump's synthetic migration (`buy_v3`, its v3 TradeExtras, priced
//!   `B̂_v3`), completing the curve in the liquidation (`PumpMigrating`, pump's
//!   `PostCompleteBuyEvent`, `pcB = Q − rT`); in the same tick the keeper migrates and transitions
//!   the market, and the second position is liquidated **on the pool** at the warm-up's gate.
//!   No bundle, no `complete_curve`.
//! - **A synthetic expiry** for a straddle made by an outsider and for M5-SEC-4's self-made one
//!   (the borrower buys `rT − q + 1`): the keeper's `expire` completes the curve, the market
//!   graduates in the same tick, and the other expired position is expired on the pool at
//!   **`T + 8`** (an outside key's `expire` refused `EmaWarmingUp` at every second before).
//! - **σ above `σ_max`** (C5, T-SIGMA at the keeper): a straddle-size position whose locked
//!   `σ_max` is below σ(`B̂_v3`) plans `SolSettle`; the keeper never sends a buyback for it and its
//!   `sol_settle` settles S2.
//! - **A race with a third-party synthetic completion** (T-SYNTH-4 at the keeper): a third party's
//!   `buy_v3(rT + δ)` lands between the keeper's simulation and its expiry: the expiry fails on
//!   chain (`VenueMigrating`, nothing written); the keeper migrates and transitions at its next
//!   tick and expires the position on the pool at the gate.
//! - **Below `rT`** (`Q ≤ rT`): the keeper expires a curve position on the curve with the v3 list.
//! - **Graduated elsewhere** (duty 5 on a list-only market, M2-SEC-5): a curve migrated elsewhere
//!   with an expired position open goes `→ None` at the keeper's first crank; the keeper keeps
//!   the S3 chain on the no-route cadence with the detection set as ReadSet and settles the
//!   position in S3 once the chain has run `s3_wait_s`.
//! - **`None → PumpCurve` re-detection**: a list-only market whose detection set shows a live
//!   curve is cranked onto it, and the keeper then creates the trader's base ATA and pump UVA the
//!   re-detection does not create, once; an open lands on the curve.
//! - **The synthetic fee's supply** (pump-final §5.2, P1 correction (a)): the keeper's view of a
//!   curve market carries its mint's live supply and the program's state on it, so the fee of a
//!   buy past `rT` is the tier pump charges, two curve markets read in one batch.
//! - **Duty 10 on pump's post-complete fields** (pump-final §7): a live curve carrying `pcB` /
//!   `pcQ` (G-3) or breaking `vQ − rQ = ivqr`, and a migrating curve whose post-complete fields are
//!   not known, are alerted once each; a migrating curve's `ρ − 1` and `curve_base_ata` donation
//!   are its gauges.

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::Instruction;
use anchor_lang::{AccountDeserialize, AccountSerialize};
use shortwin::errors::ShortwinError;
use shortwin::state::{ShortPosition, VenueKind};
use shortwin_keeper::config::GraduationConfig;
use shortwin_keeper::decide::Plan;
use shortwin_keeper::graduation;
use shortwin_keeper::venue::CurveExtrasFormat;
use shortwin_litesvm_tests::engine::{Borrower, VenueMarket};
use shortwin_litesvm_tests::fork::{self, PumpCoin, PumpCurve};
use shortwin_litesvm_tests::pamm::{PammMarket, PammUser};
use shortwin_litesvm_tests::pump::{self, CurveUser, PumpMarket, PumpSpec};
use shortwin_litesvm_tests::{pda, read};
use shortwin_services_common::{AccountData, TxOutcome};
use solana_signer::Signer;
use solana_transaction::Transaction;

use crate::common::*;

const SOL: u64 = 1_000_000_000;

/// pump's tokens sold on ECON's D@80 % curve (`tests/litesvm` `synthetic_exit_it`): the crowd's
/// buy that puts the fresh fixture curve at 80 % progress.
const D80_SOLD: u64 = 634_480_000_000_000;

/// The fork on `coin`, a listed market with `40·q` lent (`q = vT/200`), a funded crowd, and a
/// keeper with `cfg` covering every market with positions.
fn pump_setup_on(coin: PumpCoin, cfg: KeeperConfig) -> (Harness, PumpMarket, u64) {
    let (mut env, c) = Env::fork_pump_coin(coin);
    ok(env.list_pump(&c));
    let q = c.state(&env.svm).virtual_token_reserves / 200;
    let pm = env.pump_market_listed(PumpSpec::new(coin, q * 40), c);
    // Positions of `q` are below the 1-SOL value-at-risk floor: covered all the same (the duties of
    // a covered market are what is tested; `common::covering_all`).
    let h = Harness::new(env, covering_all(cfg));
    h.chain().set_priority_fees(vec![5_000]);
    (h, pm, q)
}

/// [`pump_setup_on`] the near-complete 151-byte curve.
fn pump_setup(cfg: KeeperConfig) -> (Harness, PumpMarket, u64) {
    pump_setup_on(PumpCoin::Near, cfg)
}

/// ECON's D@80 % market under the harness's default config: the fresh fixture curve bought to 80 %
/// progress by the crowd (a real `buy_v2`), then listed, `40·q` lent (`q = vT/200`), and a keeper
/// with `cfg` covering every market with positions.
fn d80_setup(cfg: KeeperConfig) -> (Harness, PumpMarket, u64) {
    let (mut env, c) = Env::fork_pump_coin(PumpCoin::Fresh);
    let crowd = CurveUser::new(&mut env, &c);
    env.fund(&crowd.kp.pubkey(), 2_000 * SOL);
    ok(crowd.buy(&mut env, &c, D80_SOLD, u64::MAX));
    ok(env.list_pump(&c));
    let q = c.state(&env.svm).virtual_token_reserves / 200;
    let pm = env.pump_market_listed(PumpSpec::new(PumpCoin::Fresh, q * 40), c);
    let h = Harness::new(env, covering_all(cfg));
    h.chain().set_priority_fees(vec![5_000]);
    (h, pm, q)
}

/// A keeper configuration with the `migrate_v2` duty off (the transition stays on).
fn without_migrate() -> KeeperConfig {
    KeeperConfig {
        graduation: GraduationConfig { migrate: false },
        ..KeeperConfig::default()
    }
}

/// An outside user buys until `rt_left` real tokens remain (a real `buy_v2`, never past `rT`).
fn buy_down_to(env: &mut Env, c: &PumpCurve, rt_left: u64) {
    let u = CurveUser::new(env, c);
    env.fund(&u.kp.pubkey(), 2_000 * SOL);
    let rt = c.state(&env.svm).real_token_reserves;
    ok(u.buy(env, c, rt - rt_left, u64::MAX));
    assert_eq!(c.state(&env.svm).real_token_reserves, rt_left);
}

/// Who makes the positions straddle their curve.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Straddler {
    /// An outside buyer takes the curve to `rT = q/2`.
    Outsider,
    /// The first borrower's own wallet buys `rT − q + 1` (M5-SEC-4's self-made straddle).
    Borrower,
}

/// Opens `q` for each of two new borrowers (a second apart), makes both straddle (`q > rT`) as
/// `who` says, and warps past the second position's deadline (both expired: liquidatable).
fn two_straddled(h: &Harness, pm: &PumpMarket, q: u64, who: Straddler) -> [(Borrower, u64); 2] {
    h.with_env(|env| {
        env.crank(pm);
        env.warp(1);
        let mut a = env.borrower();
        let (res, na) = env.open(pm, &mut a, q);
        ok(res);
        env.warp(1);
        env.crank(pm);
        env.warp(1);
        let mut b = env.borrower();
        let (res, nb) = env.open(pm, &mut b, q);
        ok(res);
        env.warp(2);
        match who {
            Straddler::Outsider => buy_down_to(env, &pm.curve, q / 2),
            Straddler::Borrower => {
                // The first borrower's own wallet (its base ATA and pump UVA first).
                let c = pm.curve;
                let payer = env.payer.pubkey();
                let kp = a.kp.insecure_clone();
                let program = c.token_program.id();
                ok(env.send(
                    &[
                        fork::create_ata_idempotent_ix(&payer, &kp.pubkey(), &c.mint, &program),
                        fork::pump_init_uva_ix(&payer, &kp.pubkey()),
                    ],
                    &[],
                ));
                let rt = c.state(&env.svm).real_token_reserves;
                assert!(rt > q, "the positions fit the curve");
                let ata = pda::ata(&kp.pubkey(), &c.mint, &program);
                let fr = fork::pump_fee_recipient(&env.svm, 0);
                let bb = fork::pump_buyback_fee_recipient(&env.svm, 0);
                let ix = c.v2_ix(
                    &env.svm,
                    fork::PumpV2::Buy,
                    &kp.pubkey(),
                    &ata,
                    &fr,
                    &bb,
                    rt - q + 1,
                    u64::MAX,
                );
                ok(env.send(&[pump::cu_limit_ix(400_000), ix], &[&kp]));
                assert_eq!(c.state(&env.svm).real_token_reserves, q - 1);
            }
        }
        to_deadline(env, pm, env.position(pm, &b.key(), nb).deadline);
        [(a, na), (b, nb)]
    })
}

/// Warps past `deadline` with a crank at most every 10 minutes (the observation chain kept, as a
/// keeper's idle cadence would), the last one a second after it: the exits' reference is fresh.
fn to_deadline(env: &mut Env, pm: &PumpMarket, deadline: i64) {
    while env.now() < deadline + 1 {
        let step = (deadline + 1 - env.now()).min(600);
        env.warp(step);
        env.crank(pm);
    }
}

fn market_kind(h: &Harness, pm: &PumpMarket) -> Option<VenueKind> {
    h.with_env(|env| env.market(&pm.market()).venue_kind())
}

fn curve(h: &Harness, pm: &PumpMarket) -> shortwin_venue_state::pump::BondingCurve {
    h.with_env(|env| pm.curve.state(&env.svm))
}

fn tokens(h: &Harness, key: &Pubkey) -> u64 {
    h.with_env(|env| read::token_amount(&env.svm, key).unwrap_or(0))
}

/// The ReadSet accounts of `pm` as the VM holds them (what the keeper reads).
fn read_accounts(h: &Harness, pm: &PumpMarket) -> Vec<Option<AccountData>> {
    h.with_env(|env| {
        pm.read_set(false)
            .iter()
            .map(|a| {
                env.svm.get_account(&a.pubkey).map(|acc| AccountData {
                    lamports: acc.lamports,
                    owner: acc.owner,
                    executable: acc.executable,
                    data: acc.data,
                })
            })
            .collect()
    })
}

/// Whether `ixs` (signed by a funded outside key, with a compute budget) would be accepted by the
/// program now: `Ok`, or its custom error. The "first second the program accepts it" oracle.
fn accepts(h: &Harness, kp: &solana_keypair::Keypair, ix: Instruction) -> Result<(), Option<u32>> {
    h.with_env(|env| {
        let tx = Transaction::new_signed_with_payer(
            &[pump::cu_limit_ix(600_000), ix],
            Some(&kp.pubkey()),
            &[kp],
            env.svm.latest_blockhash(),
        );
        match env.svm.simulate_transaction(tx) {
            Ok(_) => Ok(()),
            Err(f) => Err(shortwin_litesvm_tests::errors::custom_code(&Err(f))),
        }
    })
}

/// The market as the VM holds it.
fn stored(h: &Harness, pm: &PumpMarket) -> shortwin::state::Market {
    h.with_env(|env| env.market(&pm.market()))
}

/// Every pool account a PumpSwap trade of `pam` requires exists (ARCH §3.12's existence
/// requirement: T14 the trader's pAMM UVA, T11 the coin-creator vault's WSOL ATA; the recipients'
/// are the fork's statics).
fn pool_accounts_exist(h: &Harness, pam: &PammMarket) -> bool {
    let (_, auth_ata) = pam.creator_vault();
    h.with_env(|env| {
        [pam.trader_uva(), auth_ata]
            .iter()
            .all(|k| env.svm.get_account(k).is_some_and(|a| !a.data.is_empty()))
    })
}

/// Whether the position is still open.
fn open(h: &Harness, position: &Pubkey) -> bool {
    h.with_env(|env| read::borsh::<ShortPosition>(&env.svm, position).is_some())
}

/// pump's `PostCompleteBuyEvent` in a keeper transaction's outcome (pump's own event self-CPI:
/// the 8-byte event tag, the discriminator, then the IDL fields): its `base_out`, the tokens bought
/// from the would-be pool (`post_complete_base_out`).
fn post_complete_base_out(out: &TxOutcome) -> Option<u64> {
    // tag 8 ‖ discriminator 8 ‖ user, mint, bonding_curve, quote_mint (4 × 32) ‖ timestamp 8.
    const BASE_OUT: usize = 8 + 8 + 4 * 32 + 8;
    out.inner.iter().flatten().find_map(|ix| {
        if ix.program_id != shortwin::constants::PUMP_PROGRAM_ID
            || ix.data.get(8..16) != Some(&fork::PUMP_POST_COMPLETE_BUY_EVENT[..])
        {
            return None;
        }
        ix.data
            .get(BASE_OUT..BASE_OUT + 8)
            .and_then(|b| b.try_into().ok())
            .map(u64::from_le_bytes)
    })
}

/// The `VenueStateChanged` events of a keeper transaction: `(from, to)`.
fn venue_changes(out: &TxOutcome) -> Vec<(u8, u8)> {
    shortwin_keeper::events::of::<shortwin::events::VenueStateChanged>(out)
        .iter()
        .map(|e| (e.from, e.to))
        .collect()
}

/// The stored position with `f` applied (a forged field; the account keeps its size).
fn forge_position(h: &Harness, key: &Pubkey, f: impl FnOnce(&mut ShortPosition)) {
    h.with_env(|env| {
        let mut acc = env.svm.get_account(key).expect("the position");
        let mut pos = ShortPosition::try_deserialize(&mut &acc.data[..]).expect("a position");
        f(&mut pos);
        let mut data = Vec::new();
        pos.try_serialize(&mut data).expect("serialize");
        acc.data[..data.len()].copy_from_slice(&data);
        env.svm.set_account(*key, acc).expect("set the position");
    });
}

/// The checks of a keeper exit that completed the curve by pump's synthetic migration (pump-final
/// §5.2, §6.1.7): the v3 list (found by simulation at the first curve exit), pump's pool part of
/// `q − rT` tokens, the market `PumpCurve → PumpMigrating` in the same instruction, `q` into the
/// lending vault; then, in the same tick, the keeper's `migrate_v2` and transition.
#[allow(clippy::too_many_arguments)]
fn check_completing_exit(
    h: &Harness,
    pm: &PumpMarket,
    r: &TickReport,
    position: Pubkey,
    kind: ExitKind,
    q: u64,
    rt: u64,
    vault0: u64,
) {
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    let exits: Vec<_> = r
        .exits
        .iter()
        .filter(|e| e.result.landed().is_some() && e.kind != ExitKind::SolSettle)
        .collect();
    assert_eq!(exits.len(), 1, "one buyback: {:?}", r.exits);
    let e = exits[0];
    assert_eq!((e.position, e.kind), (position, kind));
    assert_eq!(e.curve_format, Some(CurveExtrasFormat::V3), "the v3 list");
    let out = e.result.landed().expect("landed");
    assert_eq!(
        venue_changes(out),
        [(VenueKind::PumpCurve as u8, VenueKind::PumpMigrating as u8)],
        "the exit completed the curve"
    );
    assert_eq!(
        post_complete_base_out(out),
        Some(q - rt),
        "pump's pool part: pcB = q − rT"
    );
    match kind {
        ExitKind::Liquidate => {
            let ev: Vec<shortwin::events::Liquidate> = shortwin_keeper::events::of(out);
            assert!(ev[0].max_in >= ev[0].b_live && !ev[0].expired);
            assert!(ev[0].buyback_cost > 0);
        }
        _ => {
            let ev: Vec<shortwin::events::Expire> = shortwin_keeper::events::of(out);
            assert!(ev[0].max_in >= ev[0].b_live && ev[0].buyback_cost > 0);
        }
    }
    assert!(
        tokens(h, &pm.keys.pool_vault) >= vault0 + q,
        "q back to the lenders"
    );
    // pump-final §7: the first curve exit found its TradeExtras format by simulation (V3 first).
    assert_eq!(r.curve_formats.len(), 1, "{:?}", r.curve_formats);
    assert_eq!(r.curve_formats[0].chosen, Some(CurveExtrasFormat::V3));
    assert!(r.curve_formats[0].refused.is_empty());
    // The keeper graduated the market in the same tick.
    assert_eq!(r.migrations.len(), 1, "{:?}", r.migrations);
    assert!(r.migrations[0].result.landed().is_some());
    assert_eq!(r.transitions.len(), 1, "{:?}", r.transitions);
    let tr = &r.transitions[0];
    assert!(tr.result.landed().is_some());
    assert_eq!(
        tr.venue_changes,
        [(VenueKind::PumpMigrating as u8, VenueKind::PumpSwap as u8)]
    );
    assert!(r.observed(&pm.market()));
    assert_eq!(market_kind(h, pm), Some(VenueKind::PumpSwap));
    let m = stored(h, pm);
    assert_eq!((m.ema_warm, m.ema_guard), (8, 1));
    assert!(m.b_lo.get() != 0, "the transition's own bucket");
    assert_eq!(curve(h, pm).virtual_token_reserves, 0, "migrated");
    let metrics = &h.keeper.metrics;
    assert_eq!(
        metrics.markets.get(&pm.market()).map(|m| m.synthetic_exits),
        Some(1)
    );
    assert_eq!(metrics.synthetic_exits, 1);
}

/// Duty 6 (ADOPTED Q16) with the **default** configuration (M6: `migrate` on again): an outside buy
/// completes a covered market's curve; at its next tick the keeper sends `migrate_v2` — top-level,
/// alone, with exactly the accounts the harness derives (`pump::migrate_v2_ix`: the 27 IDL accounts
/// and the two boost accounts) — then, in the same second, the transition: the pool accounts the
/// trades require (created: they did not exist), `crank_price` on the curve's ReadSet
/// (`VenueStateChanged PumpCurve → PumpSwap`) and on the pool (its first bucket). The market is on
/// its pool with the warm-up marked; the keeper observes it every second until the warm-up is over
/// (no positions: the first open must find a confirmed `P_ema`), then leaves it alone; once.
#[test]
fn the_keeper_migrates_and_moves_a_completed_curve_onto_its_pool() {
    let (mut h, pm, _q) = pump_setup(KeeperConfig::default());
    assert!(KeeperConfig::default().graduation.migrate, "M6's default");
    h.second();
    assert_eq!(market_kind(&h, &pm), Some(VenueKind::PumpCurve));
    h.with_env(|env| buy_down_to(env, &pm.curve, 0));
    let c = curve(&h, &pm);
    assert!(c.complete && c.virtual_token_reserves > 0, "migrating");

    // The keeper's instruction is the harness's own derivation.
    let payer = h.keeper.payer();
    let market = h.with_env(|env| env.market(&pm.market()));
    let mine = graduation::migrate_v2_ix(&payer, &market, &read_accounts(&h, &pm)).unwrap();
    let theirs = h.with_env(|env| pump::migrate_v2_ix(&env.svm, &payer, &pm.curve));
    assert_eq!(mine.program_id, theirs.program_id);
    assert_eq!(mine.data, theirs.data);
    assert_eq!(mine.accounts, theirs.accounts);

    let r = h.second().clone();
    let t = h.now();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.migrations.len(), 1, "{:?}", r.migrations);
    let out = r.migrations[0].result.landed().expect("migrate_v2 landed");
    eprintln!("migrate_v2: {} CU", out.units_consumed);
    assert_eq!(curve(&h, &pm).virtual_token_reserves, 0, "migrated");
    // The transition, in the same second.
    assert_eq!(r.transitions.len(), 1, "{:?}", r.transitions);
    let tr = &r.transitions[0];
    assert!(tr.created >= 1, "the trader's pAMM UVA at least");
    let out = tr.result.landed().expect("the transition landed");
    assert_eq!(
        tr.venue_changes,
        [(VenueKind::PumpCurve as u8, VenueKind::PumpSwap as u8)]
    );
    eprintln!(
        "transition: {} accounts created, {} CU",
        tr.created, out.units_consumed
    );
    assert!(
        r.observed(&pm.market()),
        "the market's observation this second"
    );
    assert!(!r.cranked.contains(&pm.market()), "no second crank");
    assert_eq!(market_kind(&h, &pm), Some(VenueKind::PumpSwap));
    let m = stored(&h, &pm);
    assert_eq!((m.ema_warm, m.ema_guard), (8, 1));
    assert_eq!(m.graduated_at, t);
    assert!(m.b_lo.get() != 0, "the pool's first bucket");
    let pam = h.with_env(|env| PammMarket::graduated(&env.svm, &pm));
    assert!(pool_accounts_exist(&h, &pam));

    // The warm-up, observed every second (no positions: M6's keeper warm-up) while the next
    // instruction would still find it running: buckets T … T + 7, the eighth integrated by
    // whatever instruction comes next (an open's own prologue); then nothing.
    for sec in 1..=10 {
        let r = h.second().clone();
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert!(r.migrations.is_empty() && r.transitions.is_empty(), "once");
        assert!(r.pamm_accounts.is_empty(), "nothing left to create");
        assert_eq!(
            r.observed(&pm.market()),
            sec <= 7,
            "+{sec} s: warm {} needs {:?}",
            stored(&h, &pm).ema_warm,
            r.needs.get(&pm.market())
        );
    }
    assert_eq!(stored(&h, &pm).ema_warm, 1, "the eighth bucket is pending");
    h.with_env(|env| env.crank(&pam));
    assert_eq!(
        stored(&h, &pm).ema_warm,
        0,
        "the next instruction ends the warm-up"
    );
}

/// With `graduation.migrate` off the keeper sends no `migrate_v2`, but once someone else migrates
/// the curve the transition is still the keeper's (ARCH §7.1 duty 6 "on the canonical pool's
/// appearance"): the market moves onto its pool and its pool accounts are created.
#[test]
fn with_migrate_off_a_third_party_migration_is_still_moved_onto_the_pool() {
    let (mut h, pm, _q) = pump_setup(without_migrate());
    h.second();
    h.with_env(|env| buy_down_to(env, &pm.curve, 0));
    for _ in 0..2 {
        let r = h.second().clone();
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert!(r.migrations.is_empty(), "migrate off");
        assert!(r.transitions.is_empty());
    }
    h.with_env(|env| {
        let m = env.funded_keypair();
        let ix = pump::migrate_v2_ix(&env.svm, &m.pubkey(), &pm.curve);
        ok(env.send(&[pump::cu_limit_ix(600_000), ix], &[&m]));
    });
    let r = h.second().clone();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.transitions.len(), 1);
    assert!(r.transitions[0].result.landed().is_some());
    assert_eq!(market_kind(&h, &pm), Some(VenueKind::PumpSwap));
    let pam = h.with_env(|env| PammMarket::graduated(&env.svm, &pm));
    assert!(pool_accounts_exist(&h, &pam));
}

/// Who moves the market onto its pool in [`a_graduation_with_a_position_is_liquidated_on_the_pool_after_the_warm_up`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Transition {
    /// The keeper: its `migrate_v2`, then its transition with the pool's first bucket (`T`).
    Keeper,
    /// A third party migrates and cranks the market onto the pool (the transition records no
    /// bucket) before the keeper's tick: the warm-up's buckets are the keeper's from `T + 1`.
    ThirdParty,
}

/// A graduation with a position open (ARCH §2.5, §5.3; ECON §10): a curve position (`Q ≤ rT`)
/// survives an outside buy that completes the curve; the market is moved onto its pool — by the
/// keeper (`migrate_v2` and its transition with the pool's first bucket, in the same second), or by
/// a third party's migration and crank (no bucket; the keeper then creates the pool accounts) —; a
/// whale pumps the pool ×3 right after. During the warm-up the keeper observes every second and
/// sends **no exit** (an outside key's `liquidate` is refused `EmaWarmingUp`, then
/// `NotLiquidatable` while the EMA catches up); the keeper's `liquidate` — with the PumpSwap
/// ReadSet and TradeExtras — lands in the **first** second the outside key's simulation passes,
/// buying `Q` on the pool. No exit of the keeper is refused on the way. The two variants measure
/// what the keeper's own first bucket (the graduation price, before the pump) costs a genuine
/// pump: the warm-up seeds `P_ema` at the lowest bucket low (M4R), so with it the EMA starts at
/// the graduation price and climbs at the steady step after the gate.
#[test]
fn a_graduation_with_a_position_is_liquidated_on_the_pool_after_the_warm_up() {
    let mut measured = Vec::new();
    for variant in [Transition::Keeper, Transition::ThirdParty] {
        let (mut h, pm, q) = pump_setup(KeeperConfig::default());
        let (b, nonce) = h.with_env(|env| {
            env.crank(&pm);
            env.warp(1);
            let mut b = env.borrower();
            let (res, nonce) = env.open(&pm, &mut b, q);
            ok(res);
            (b, nonce)
        });
        let position = pda::position(&pm.market(), &b.key(), nonce).0;
        h.second();
        h.idle(1);
        h.with_env(|env| buy_down_to(env, &pm.curve, 0));
        let t = h.now();
        match variant {
            Transition::Keeper => {
                let r = h.tick().clone();
                assert!(r.errors.is_empty(), "{:?}", r.errors);
                assert!(r.migrations[0].result.landed().is_some());
                assert!(r.transitions[0].result.landed().is_some());
            }
            Transition::ThirdParty => h.with_env(|env| {
                let m = env.funded_keypair();
                let ix = pump::migrate_v2_ix(&env.svm, &m.pubkey(), &pm.curve);
                ok(env.send(&[pump::cu_limit_ix(600_000), ix], &[&m]));
                env.crank(&pm);
                assert_eq!(
                    env.market(&pm.market()).venue_kind(),
                    Some(VenueKind::PumpSwap)
                );
                assert_eq!(env.market(&pm.market()).b_lo.get(), 0, "no bucket yet");
            }),
        }
        let pam = h.with_env(|env| PammMarket::graduated(&env.svm, &pm));
        // The pool pumped ×3 right after the transition.
        h.with_env(|env| {
            let whale = PammUser::new(env, &pam.pool, 20_000 * SOL);
            ok(whale.push_price(env, &pam.pool, 3, 1));
        });
        let outsider = h.with_env(|env| env.funded_keypair());
        let liq = |h: &Harness| {
            let ix = shortwin_litesvm_tests::engine::liquidate_ix(
                &pam,
                &outsider.pubkey(),
                &b.key(),
                nonce,
            );
            accepts(h, &outsider, ix)
        };
        let mut liquidated_at = None;
        for _ in 0..300 {
            h.idle(1);
            let before = liq(&h);
            let warm = stored(&h, &pm).ema_warm;
            let r = h.tick().clone();
            assert!(r.errors.is_empty(), "{:?}", r.errors);
            assert!(
                !r.alerts
                    .iter()
                    .any(|a| matches!(a, Alert::ExitRefused { .. })),
                "{:?}",
                r.alerts
            );
            let landed = r.exits.iter().any(|e| {
                e.position == position
                    && e.kind == ExitKind::Liquidate
                    && e.result.landed().is_some()
            });
            if landed {
                assert_eq!(before, Ok(()), "the program accepted it this second");
                liquidated_at = Some(h.now());
                break;
            }
            // The keeper sent nothing the program would have taken.
            assert_ne!(before, Ok(()), "+{} s: the keeper missed it", h.now() - t);
            if warm > 0 {
                assert!(
                    r.exits.iter().all(|e| e.kind == ExitKind::SolSettle),
                    "no liquidation during the warm-up"
                );
            }
            assert!(r.observed(&pm.market()), "every second");
        }
        assert!(pool_accounts_exist(&h, &pam), "the keeper's duty");
        let at = liquidated_at.expect("liquidated on the pool");
        eprintln!(
            "graduation with a position ({variant:?}): transition at {t}, liquidated on the pool \
             at +{} s",
            at - t
        );
        assert!(at - t >= 8, "not before the warm-up's gate");
        assert!(!open(&h, &position));
        assert_eq!(market_kind(&h, &pm), Some(VenueKind::PumpSwap));
        measured.push(at - t);
    }
    eprintln!(
        "×3 pump right after the graduation: liquidated at +{} s with the keeper's first bucket, \
         +{} s without",
        measured[0], measured[1]
    );
    assert!(measured[0] >= measured[1]);
}

/// A completing liquidation (module docs; pump-final §14 "Keeper", T-SYNTH-2 at the keeper): on
/// ECON's D@80 % curve two borrowers each short `q`; the crowd buys the curve down to `rT = q/2`
/// (the price ×2.4: both positions liquidatable once the EMA follows). The keeper observes every
/// second; its evaluation plans `Liquidate` for a position past `rT` (`synthetic`, priced
/// `B̂_v3`) — never a bundle — and its `liquidate` of the first lands on the curve in the first
/// second the program takes it, completing the curve by pump's synthetic migration; in the same
/// tick it migrates and transitions the market. The second position, frozen by the completion
/// (`VenueMigrating` until the transition), is liquidated **on the pool** after the warm-up's gate.
#[test]
fn a_completing_liquidation_graduates_the_market_and_the_rest_exits_on_the_pool() {
    let (mut h, pm, q) = d80_setup(KeeperConfig::default());
    let [(a, na), (b, nb)] = h.with_env(|env| {
        env.crank(&pm);
        env.warp(1);
        let mut a = env.borrower();
        let (res, na) = env.open(&pm, &mut a, q);
        ok(res);
        let mut b = env.borrower();
        let (res, nb) = env.open(&pm, &mut b, q);
        ok(res);
        [(a, na), (b, nb)]
    });
    let pa = pda::position(&pm.market(), &a.key(), na).0;
    let pb = pda::position(&pm.market(), &b.key(), nb).0;
    for _ in 0..30 {
        h.second();
    }
    h.with_env(|env| buy_down_to(env, &pm.curve, q / 2));
    let rt = q / 2;
    let mut done = None;
    for _ in 0..900 {
        h.idle(1);
        // The keeper's own view of the next tick: a position past `rT` plans as any other.
        for p in [pa, pb] {
            if let Some((ev, _, _)) = keeper_eval(&h, pm.market(), p) {
                assert!(ev.synthetic, "Q > rT on a live curve");
                assert!(!matches!(ev.plan, Plan::SolSettle | Plan::S3Probe) || ev.armed);
                if ev.plan == Plan::Liquidate {
                    assert!(ev.buyback_allowed && ev.b_live.is_some());
                    assert!(ev.max_in >= ev.b_live, "the bound covers B̂_v3");
                }
            }
        }
        let vault0 = tokens(&h, &pm.keys.pool_vault);
        let r = h.tick().clone();
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        if r.exits
            .iter()
            .any(|e| e.kind == ExitKind::Liquidate && e.result.landed().is_some())
        {
            let first = r
                .exits
                .iter()
                .find(|e| e.kind == ExitKind::Liquidate && e.result.landed().is_some())
                .map(|e| e.position)
                .expect("landed");
            check_completing_exit(&h, &pm, &r, first, ExitKind::Liquidate, q, rt, vault0);
            done = Some((h.now(), first));
            break;
        }
        assert!(
            !r.alerts
                .iter()
                .any(|a| matches!(a, Alert::ExitRefused { .. })),
            "{:?}",
            r.alerts
        );
    }
    let (t, first) = done.expect("the completing liquidation landed");
    let other = if first == pa { pb } else { pa };
    assert!(!open(&h, &first) && open(&h, &other));
    // The other position on the pool after the warm-up's gate.
    let mut at = None;
    for _ in 0..300 {
        let r = h.second().clone();
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        if r.exits.iter().any(|e| {
            e.position == other && e.kind == ExitKind::Liquidate && e.result.landed().is_some()
        }) {
            assert!(
                r.exits
                    .iter()
                    .all(|e| e.curve_format.is_none() || e.position != other),
                "a PumpSwap exit carries no curve format"
            );
            at = Some(h.now() - t);
            break;
        }
    }
    let at = at.expect("the other position liquidated on the pool");
    assert!(at >= 8, "not before the warm-up's gate: +{at} s");
    assert!(!open(&h, &other));
    eprintln!("completing liquidation at {t}; the other position on the pool at +{at} s");
}

/// The synthetic expiry (module docs) for a straddle made by an outside buyer and for M5-SEC-4's
/// **self-made** straddle (the first borrower buys `rT − q + 1` itself): two expired positions of
/// `q` past `rT`; the keeper's first tick expires one on the curve by pump's synthetic migration,
/// which completes it, and graduates the market in the same tick (`migrate_v2`, the transition
/// with its own bucket); the other is expired **on the pool** at `T + 8`, the warm-up's gate (an
/// outside key's `expire` refused `EmaWarmingUp` in every second before).
#[test]
fn a_synthetic_expiry_graduates_the_market_and_the_rest_expires_at_the_gate() {
    for who in [Straddler::Outsider, Straddler::Borrower] {
        let (mut h, pm, q) = pump_setup_on(PumpCoin::Near166, KeeperConfig::default());
        let positions = two_straddled(&h, &pm, q, who);
        let rt = curve(&h, &pm).real_token_reserves;
        assert!(rt < q, "{who:?}: both straddle");
        let keys: Vec<Pubkey> = positions
            .iter()
            .map(|(b, n)| pda::position(&pm.market(), &b.key(), *n).0)
            .collect();
        for p in &keys {
            let (ev, _, _) = keeper_eval(&h, pm.market(), *p).expect("open");
            assert!(ev.synthetic && ev.expired, "{who:?}");
            assert!(
                matches!(ev.plan, Plan::Expire | Plan::Observe),
                "{who:?}: {:?}",
                ev.plan
            );
        }
        let vault0 = tokens(&h, &pm.keys.pool_vault);
        // The first tick may only observe (a fresh reference); the expiry follows.
        let mut r = h.second().clone();
        if !r
            .exits
            .iter()
            .any(|e| e.result.landed().is_some() && e.kind == ExitKind::Expire)
        {
            r = h.second().clone();
        }
        let t = h.now();
        let first = r
            .exits
            .iter()
            .find(|e| e.kind == ExitKind::Expire && e.result.landed().is_some())
            .map(|e| e.position)
            .unwrap_or_else(|| panic!("{who:?}: {:?}", r.exits));
        check_completing_exit(&h, &pm, &r, first, ExitKind::Expire, q, rt, vault0);
        let other = keys.iter().copied().find(|k| *k != first).unwrap();
        let (b, n) = positions
            .iter()
            .find(|(b, n)| pda::position(&pm.market(), &b.key(), *n).0 == other)
            .unwrap();
        let pam = h.with_env(|env| PammMarket::graduated(&env.svm, &pm));
        assert!(pool_accounts_exist(&h, &pam));
        let outsider = h.with_env(|env| env.funded_keypair());
        let mut expired_at = None;
        for _ in 0..20 {
            h.idle(1);
            let ix =
                shortwin_litesvm_tests::engine::expire_ix(&pam, &outsider.pubkey(), &b.key(), *n);
            let before = accepts(&h, &outsider, ix);
            let r = h.tick().clone();
            assert!(r.errors.is_empty(), "{:?}", r.errors);
            if r.exits.iter().any(|e| {
                e.position == other && e.kind == ExitKind::Expire && e.result.landed().is_some()
            }) {
                assert_eq!(before, Ok(()));
                expired_at = Some(h.now() - t);
                break;
            }
            assert_eq!(
                before,
                Err(Some(u32::from(ShortwinError::EmaWarmingUp))),
                "{who:?}: +{} s",
                h.now() - t
            );
            assert!(r.observed(&pm.market()), "every second of the warm-up");
        }
        assert_eq!(expired_at, Some(8), "{who:?}: at the warm-up's gate, T + 8");
        assert!(!open(&h, &other));
    }
}

/// C5 / T-SIGMA at the keeper (PUMP-2026-10-13-3): σ is judged on `B̂_v3` at once. On the D@80 %
/// curve a position past `rT` whose locked `σ_max` is forged to 1 bps — below σ(`B̂_v3`) ≈ 2–3 %,
/// itself below the parameter's floor — is liquidatable but its normal buyback is not allowed: the
/// keeper's evaluation plans `SolSettle` (it was `Straddle`, the bundle, before pump-final), the
/// keeper sends no `liquidate` or `expire` for it, and its `sol_settle` calls arm the S2 legs and
/// settle S2. The curve stays live (no buyback ran).
#[test]
fn above_sigma_max_a_position_past_rt_goes_to_sol_settle() {
    let (mut h, pm, q) = d80_setup(KeeperConfig::default());
    let (b, nb) = h.with_env(|env| {
        env.crank(&pm);
        env.warp(1);
        let mut b = env.borrower();
        let (res, nb) = env.open(&pm, &mut b, q);
        ok(res);
        (b, nb)
    });
    let p = pda::position(&pm.market(), &b.key(), nb).0;
    forge_position(&h, &p, |pos| pos.max_liq_slip_bps = 1);
    for _ in 0..30 {
        h.second();
    }
    h.with_env(|env| buy_down_to(env, &pm.curve, q / 2));
    let mut planned = false;
    let mut settled = None;
    for _ in 0..1_200 {
        h.idle(1);
        if let Some((ev, _, _)) = keeper_eval(&h, pm.market(), p) {
            assert!(ev.synthetic);
            if ev.spot_leg && ev.ema_leg {
                assert!(!ev.buyback_allowed, "σ(B̂_v3) above the forged σ_max");
                assert_eq!(ev.plan, Plan::SolSettle);
                planned = true;
            }
        }
        let r = h.tick().clone();
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert!(
            r.exits
                .iter()
                .all(|e| e.position != p || e.kind == ExitKind::SolSettle),
            "no buyback: {:?}",
            r.exits
        );
        if let Some(out) = r
            .exits
            .iter()
            .filter(|e| e.position == p)
            .find_map(|e| e.result.landed())
            && let Some(ev) = shortwin_keeper::events::of::<shortwin::events::SolSettle>(out).pop()
        {
            settled = Some(ev);
            break;
        }
    }
    assert!(planned, "the position became liquidatable");
    let ev = settled.expect("S2 settled");
    assert_eq!(ev.condition, 2, "S2");
    assert!(ev.lender_amount > 0 && ev.fv > 0);
    assert!(!curve(&h, &pm).complete, "no buyback ran");
    assert!(!open(&h, &p));
}

/// A race with a third-party synthetic completion (module docs; T-SYNTH-4 at the keeper): an
/// expired position past `rT` on `near166`; between the keeper's simulation of its `expire` and the
/// transaction landing, a third party's `buy_v3(rT + δ)` completes the curve by pump's synthetic
/// migration. The keeper's expiry fails on chain with `VenueMigrating` and writes nothing (the
/// position, the lending vault and the curve's completion are as the third party left them); at
/// its next tick the keeper migrates and transitions the market, and expires the position on the
/// pool at the warm-up's gate.
#[test]
fn a_third_party_synthetic_completion_races_the_keepers_expiry() {
    let (mut h, pm, q) = pump_setup_on(PumpCoin::Near166, KeeperConfig::default());
    let (b, nb) = h.with_env(|env| {
        env.crank(&pm);
        env.warp(1);
        let mut b = env.borrower();
        let (res, nb) = env.open(&pm, &mut b, q);
        ok(res);
        env.warp(2);
        buy_down_to(env, &pm.curve, q / 2);
        to_deadline(env, &pm, env.position(&pm, &b.key(), nb).deadline);
        (b, nb)
    });
    let p = pda::position(&pm.market(), &b.key(), nb).0;
    // Until the keeper's next tick would send the expiry (a fresh reference first).
    let mut ready = false;
    for _ in 0..3 {
        h.idle(1);
        let (ev, _, _) = keeper_eval(&h, pm.market(), p).expect("open");
        assert!(ev.synthetic && ev.expired, "{ev:?}");
        if ev.plan == Plan::Expire {
            ready = true;
            break;
        }
        h.tick();
    }
    assert!(ready, "the expiry is due");
    let before = h.with_env(|env| env.svm.get_account(&p));
    let vault0 = tokens(&h, &pm.keys.pool_vault);
    let c = pm.curve;
    // The third party (its accounts made first): its buy lands on the chain the keeper's
    // transaction then lands on — directly on the VM, with the blockhash the keeper's transaction
    // carries still valid (`Env::send` would expire it, a harness artefact a cluster does not
    // have).
    let third = h.with_env(|env| {
        let u = CurveUser::new(env, &c);
        env.fund(&u.kp.pubkey(), 2_000 * SOL);
        u
    });
    h.chain().before_next_send(move |env| {
        let rt = c.state(&env.svm).real_token_reserves;
        let ix = third.buy_v3_ix(&env.svm, &c, rt + rt / 4, u64::MAX);
        let tx = Transaction::new_signed_with_payer(
            &[pump::cu_limit_ix(400_000), ix],
            Some(&third.kp.pubkey()),
            &[&third.kp],
            env.svm.latest_blockhash(),
        );
        env.svm
            .send_transaction(tx)
            .expect("the third party's synthetic completion");
        assert!(
            c.state(&env.svm).complete,
            "the third party completed the curve"
        );
    });
    let r = h.tick().clone();
    let e = r
        .exits
        .iter()
        .find(|e| e.position == p && e.kind == ExitKind::Expire)
        .unwrap_or_else(|| panic!("{:?}", r.exits));
    assert_eq!(e.curve_format, Some(CurveExtrasFormat::V3));
    let ExitResult::Failed { outcome } = &e.result else {
        panic!("{:?}", e.result);
    };
    assert_eq!(
        outcome.err.as_ref().and_then(|x| x.custom_code()),
        Some(u32::from(ShortwinError::VenueMigrating))
    );
    assert_eq!(
        h.with_env(|env| env.svm.get_account(&p)),
        before,
        "nothing written"
    );
    assert_eq!(tokens(&h, &pm.keys.pool_vault), vault0);
    assert!(curve(&h, &pm).complete);
    // The next tick graduates the market; the position expires on the pool at the gate.
    let r = h.second().clone();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert!(
        r.migrations[0].result.landed().is_some(),
        "{:?}",
        r.migrations
    );
    assert!(
        r.transitions[0].result.landed().is_some(),
        "{:?}",
        r.transitions
    );
    let t = h.now();
    let mut at = None;
    for _ in 0..20 {
        let r = h.second().clone();
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        if r.exits
            .iter()
            .any(|e| e.position == p && e.kind == ExitKind::Expire && e.result.landed().is_some())
        {
            at = Some(h.now() - t);
            break;
        }
    }
    assert_eq!(at, Some(8), "on the pool at the warm-up's gate");
    assert!(!open(&h, &p));
}

#[test]
fn below_rt_the_keeper_expires_on_the_curve() {
    let (mut h, pm, q) = pump_setup(KeeperConfig::default());
    let (b, nonce) = h.with_env(|env| {
        env.crank(&pm);
        env.warp(1);
        let mut b = env.borrower();
        let (res, nonce) = env.open(&pm, &mut b, q);
        ok(res);
        let deadline = env.position(&pm, &b.key(), nonce).deadline;
        let now = env.now();
        env.warp(deadline - now + 1);
        (b, nonce)
    });
    let position = pda::position(&pm.market(), &b.key(), nonce).0;
    assert!(u128::from(q) <= u128::from(curve(&h, &pm).real_token_reserves));
    let mut expired = false;
    for _ in 0..5 {
        let r = h.second().clone();
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert!(r.migrations.is_empty());
        if let Some(e) = r.exits.iter().find(|e| {
            e.position == position && e.kind == ExitKind::Expire && e.result.landed().is_some()
        }) {
            assert_eq!(e.curve_format, Some(CurveExtrasFormat::V3), "the v3 list");
            expired = true;
            break;
        }
    }
    assert!(expired, "expired on the curve");
    assert!(!open(&h, &position));
    assert!(!curve(&h, &pm).complete);
    assert_eq!(
        h.keeper.curve_format().map(|(_, f)| f),
        Some(CurveExtrasFormat::V3)
    );
}

/// Duty 5 on a list-only market (M5, M2-SEC-5): a covered curve market's curve completes and pump
/// migrates it **elsewhere** (reserves zeroed, no canonical pool; planted on pump's account) with
/// an expired position open. The keeper covers the market with its detection set as ReadSet: its
/// first crank moves it `PumpCurve → None` (graduated elsewhere; the S3 chain starts), it keeps
/// the chain on the no-route cadence (an observation at least every `no_route_interval_s`), sends
/// no migration and no trading exit, and settles the position in S3 on the detection set as soon
/// as the chain has run `s3_wait_s`.
#[test]
fn a_curve_that_graduated_elsewhere_is_settled_in_s3_by_the_keeper() {
    let (mut h, pm, q) = pump_setup(KeeperConfig::default());
    let (b, nonce) = h.with_env(|env| {
        env.crank(&pm);
        env.warp(1);
        let mut b = env.borrower();
        let (res, nonce) = env.open(&pm, &mut b, q);
        ok(res);
        env.warp(1);
        // The curve completes and pump migrates it elsewhere before the keeper's next tick.
        buy_down_to(env, &pm.curve, 0);
        fork::set_curve_reserves(&mut env.svm, &pm.curve.curve, 0, 0, 0, 0);
        let deadline = env.position(&pm, &b.key(), nonce).deadline;
        let now = env.now();
        env.warp(deadline - now + 1);
        (b, nonce)
    });
    let position = pda::position(&pm.market(), &b.key(), nonce).0;
    let s3_wait = h.with_env(|env| env.position(&pm, &b.key(), nonce).s3_wait_s);
    let r = h.second().clone();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(market_kind(&h, &pm), Some(VenueKind::None));
    let since = h.with_env(|env| env.market(&pm.market()).no_route_since);
    assert!(since > 0, "the S3 chain runs");
    let mut last_obs = h.now();
    let mut settled_at = None;
    for _ in 0..(i64::from(s3_wait) + 120) {
        let r = h.second().clone();
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert!(r.migrations.is_empty());
        assert!(r.exits.iter().all(|e| e.kind == ExitKind::SolSettle));
        let now = h.now();
        if r.cranked.contains(&pm.market()) || r.observed(&pm.market()) {
            last_obs = now;
        }
        assert!(
            now - last_obs <= KeeperConfig::default().coverage.no_route_interval_s,
            "the no-route cadence at {now} (last observation {last_obs})"
        );
        if r.exits.iter().any(|e| {
            e.position == position && e.kind == ExitKind::SolSettle && e.result.landed().is_some()
        }) && !open(&h, &position)
        {
            settled_at = Some(now);
            break;
        }
    }
    let at = settled_at.expect("settled in S3");
    assert!(
        at >= since + i64::from(s3_wait),
        "not before the chain ran s3_wait_s"
    );
    assert!(
        at <= since + i64::from(s3_wait) + KeeperConfig::default().coverage.s3_probe_interval_s + 1,
        "at the first probe after it ({at} vs {since} + {s3_wait})"
    );
    println!(
        "graduated elsewhere: the keeper kept the S3 chain from {since} and settled at +{} s",
        at - since
    );
}

/// `crank_price`'s `None → PumpCurve` re-detection by the keeper (M5; ARCH §2.5 `None` row): a mint
/// listed before its curve was readable (the curve account removed for the listing, restored
/// after) is list-only; the keeper sees a live curve in its detection set (the program's
/// `classify_pump`) and cranks it onto the curve, then creates the trader's base ATA and pump UVA
/// the curve legs require (the crank has no payer and creates neither), once; a borrower's open
/// then lands on the curve.
#[test]
fn a_list_only_market_is_redetected_and_prepared_by_the_keeper() {
    let (mut env, c) = Env::fork_pump_coin(PumpCoin::T22_115);
    let saved = env.svm.get_account(&c.curve).unwrap();
    env.svm
        .set_account(c.curve, solana_account::Account::default())
        .unwrap();
    let creator = env.funded_keypair();
    let list = shortwin_litesvm_tests::ix::create_market(
        &solana_signer::Signer::pubkey(&creator),
        &c.mint,
        &c.token_program.id(),
        shortwin::instructions::create_market::VENUE_HINT_AUTO,
        &pda::canonical_pool(&c.mint),
    );
    ok(env.send(&[list], &[&creator]));
    env.svm.set_account(c.curve, saved).unwrap();
    let q = c.state(&env.svm).virtual_token_reserves / 200;
    let pm = env.pump_market_listed(PumpSpec::new(PumpCoin::T22_115, q * 40), c);
    assert_eq!(
        env.market(&pm.market()).venue_kind(),
        Some(VenueKind::None),
        "listed without a curve: list-only"
    );
    let mut h = Harness::new(env, KeeperConfig::default());
    let r = h.second().clone();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert!(r.cranked.contains(&pm.market()), "the keeper cranks it");
    assert_eq!(market_kind(&h, &pm), Some(VenueKind::PumpCurve));
    let [base, uva] = h.with_env(|env| graduation::trader_account_keys(&env.market(&pm.market())));
    assert_eq!(base, pm.trader_base_ata);
    let missing = |h: &Harness| {
        h.with_env(|env| {
            (
                read::account(&env.svm, &base).is_none(),
                read::account(&env.svm, &uva).is_none(),
            )
        })
    };
    assert_eq!(missing(&h), (true, true), "the crank created nothing");
    let r = h.second().clone();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.curve_accounts.len(), 1);
    assert!(
        r.curve_accounts[0].1.landed().is_some(),
        "{:?}",
        r.curve_accounts
    );
    assert_eq!(missing(&h), (false, false), "the keeper created both");
    for _ in 0..3 {
        let r = h.second().clone();
        assert!(r.curve_accounts.is_empty(), "once");
    }
    // A borrower's open lands on the curve.
    h.with_env(|env| {
        env.warp(i64::from(env.config().params.market_warmup_s));
        env.crank(&pm);
        env.warp(1);
        let mut b = env.borrower();
        let (res, _) = env.open(&pm, &mut b, q);
        ok(res);
    });
}

/// pump-final §5.2, P1 correction (a), on the keeper's side: a tick reads a `PumpCurve` market's
/// mint after its ReadSet and hands the supply to the program's reader, so the keeper's view
/// carries the fee pump charges a buy past `rT` (the tier of the curve's market cap on the live
/// supply). Two curve markets in one read — `near166` (supply burned to 999,818,465,159,445)
/// first, `buckets166` second — under three tiers that split near166's two market caps: each view
/// carries its own mint's supply, its own ReadSet accounts and the program's state on them with
/// that supply (a misplaced mint slot would shift the second market's ReadSet), and near166's
/// `φ_s` is the live supply's 80/25 where its curve path keeps the 10¹⁵ tier, 70/22.
#[test]
fn the_keepers_view_resolves_a_curve_markets_synthetic_fee_on_its_mint() {
    use shortwin::constants::PUMP_FEE_CONFIG;
    use shortwin::venues::VenueState;
    use shortwin_keeper::view;
    use shortwin_math::fees::FeeBps;

    let (mut env, a) = Env::fork_pump_coin(PumpCoin::Near166);
    let b = env.load_pump_coin(PumpCoin::Buckets166);
    ok(env.list_pump(&a));
    ok(env.list_pump(&b));
    let pa = env.pump_market_listed(PumpSpec::new(PumpCoin::Near166, 0), a);
    let pb = env.pump_market_listed(PumpSpec::new(PumpCoin::Buckets166, 0), b);
    let tiers = [
        (0, [0, 90, 28]),
        (290_174_135_726, [0, 80, 25]),
        (290_226_826_789, [0, 70, 22]),
    ];
    fork::set_fee_config(
        &mut env.svm,
        &PUMP_FEE_CONFIG,
        [0, 95, 30],
        &tiers,
        &tiers,
        [0, 95, 30],
    );
    let h = Harness::new(env, KeeperConfig::default());
    let cv = shortwin_keeper_litesvm::block_on(view::load(h.chain(), &[pa.market(), pb.market()]))
        .expect("view");
    let supply = |pm: &PumpMarket| {
        h.with_env(|env| {
            let m = env.svm.get_account(&pm.keys.mint).expect("mint");
            shortwin_venue_state::mint::read_base(&m.owner.to_bytes(), &m.data)
                .expect("mint")
                .supply
        })
    };
    let (sa, sb) = (supply(&pa), supply(&pb));
    assert_eq!(sa, 999_818_465_159_445, "near166's burned supply");
    assert_ne!(sa, sb, "two supplies");
    let fees_of = |st: &VenueState| {
        let p = st
            .curve
            .as_ref()
            .and_then(|c| c.pricing.as_ref())
            .expect("the curve's pricing inputs");
        (p.fees, p.synthetic_fees())
    };
    let now = h.with_env(|env| env.now());
    let mut seen = Vec::new();
    for (pm, s) in [(&pa, sa), (&pb, sb)] {
        let mv = cv.market(&pm.market()).expect("loaded");
        assert_eq!(mv.kind, VenueKind::PumpCurve);
        assert_eq!(mv.supply, Some(s), "its own mint's supply");
        let keys: Vec<Pubkey> = pm.read_set(false).iter().map(|m| m.pubkey).collect();
        assert_eq!(mv.read_keys, keys);
        let accs = read_accounts(&h, pm);
        assert_eq!(mv.read_accounts, accs, "its own ReadSet accounts");
        // The program's reader on the same accounts with that supply.
        let market = stored(&h, pm);
        let want = view::with_account_infos(&keys, &accs, |infos| {
            shortwin::venues::read_state_with(&market, VenueKind::PumpCurve, infos, now, Some(s))
        });
        assert!(mv.state.executable && want.executable, "live curves");
        assert_eq!(
            (mv.state.x, mv.state.y, mv.state.r, mv.state.fee_ppm),
            (want.x, want.y, want.r, want.fee_ppm)
        );
        assert_eq!(fees_of(&mv.state), fees_of(&want));
        seen.push(fees_of(&mv.state));
    }
    assert_eq!(
        seen[0],
        (FeeBps::new(0, 70, 22), Some(FeeBps::new(0, 80, 25))),
        "near166: φ on 10¹⁵, φ_s on the live supply"
    );
    assert!(seen[1].1.is_some(), "buckets166's φ_s resolved too");
}

/// Duty 10 on pump's post-complete fields (pump-final §7, §6.1.5), on `near166`:
/// - a live curve carrying `pcQ` (@158; G-3) is not executable and is alerted
///   (`LiveCurvePostComplete`) once, however many ticks it lasts;
/// - a live curve breaking pump's invariant `vQ − rQ = ivqr` (@142) is alerted
///   (`CurveQuoteOffsetInconsistent`) once;
/// - with `migrate` off, a third party's synthetic completion leaves the market migrating: its
///   gauges are `ρ − 1` > 0 (the pool part raises the pool's price) and a donation of 0; with
///   `pcQ` cleared (one of the two fields written) the fields are not known and
///   `MigratingFvUndefined` is alerted, once, and the `ρ` gauge goes.
#[test]
fn duty_10_reports_pumps_post_complete_fields() {
    use shortwin_venue_state::pump::{
        OFF_INITIAL_VIRTUAL_QUOTE_RESERVES, OFF_POST_COMPLETE_QUOTE_IN,
    };
    let (mut h, pm, _q) = pump_setup_on(PumpCoin::Near166, without_migrate());
    h.second();
    let curve_key = pm.curve.curve;
    let put = |h: &Harness, off: usize, v: u64| {
        h.with_env(|env| {
            fork::patch(&mut env.svm, &curve_key, |d| {
                d[off..off + 8].copy_from_slice(&v.to_le_bytes());
            })
        })
    };
    let get = |h: &Harness, off: usize| {
        h.with_env(|env| {
            let d = env.svm.get_account(&curve_key).unwrap().data;
            u64::from_le_bytes(d[off..off + 8].try_into().unwrap())
        })
    };
    let count = |r: &TickReport, f: fn(&Alert) -> bool| r.alerts.iter().filter(|a| f(a)).count();

    // A live curve carrying pcQ.
    put(&h, OFF_POST_COMPLETE_QUOTE_IN, 7);
    let r = h.second().clone();
    assert_eq!(
        count(&r, |a| matches!(
            a,
            Alert::LiveCurvePostComplete {
                post_complete_quote_in: 7,
                ..
            }
        )),
        1,
        "{:?}",
        r.alerts
    );
    let cv =
        shortwin_keeper_litesvm::block_on(shortwin_keeper::view::load(h.chain(), &[pm.market()]))
            .expect("view");
    assert!(!cv.markets[0].state.executable, "G-3: not executable");
    let r = h.second().clone();
    assert_eq!(
        count(&r, |a| matches!(a, Alert::LiveCurvePostComplete { .. })),
        0,
        "once"
    );
    put(&h, OFF_POST_COMPLETE_QUOTE_IN, 0);
    h.second();

    // A live curve breaking vQ − rQ = ivqr.
    let ivqr = get(&h, OFF_INITIAL_VIRTUAL_QUOTE_RESERVES);
    assert!(ivqr > 0, "a create_v2-era curve");
    put(&h, OFF_INITIAL_VIRTUAL_QUOTE_RESERVES, ivqr + 1);
    let r = h.second().clone();
    assert_eq!(
        count(&r, |a| matches!(
            a,
            Alert::CurveQuoteOffsetInconsistent { .. }
        )),
        1,
        "{:?}",
        r.alerts
    );
    let r = h.second().clone();
    assert_eq!(
        count(&r, |a| matches!(
            a,
            Alert::CurveQuoteOffsetInconsistent { .. }
        )),
        0
    );
    put(&h, OFF_INITIAL_VIRTUAL_QUOTE_RESERVES, ivqr);

    // A third party's synthetic completion; migrate off: the market stays migrating.
    h.with_env(|env| {
        let u = CurveUser::new(env, &pm.curve);
        env.fund(&u.kp.pubkey(), 2_000 * SOL);
        let rt = pm.curve.state(&env.svm).real_token_reserves;
        let ix = u.buy_v3_ix(&env.svm, &pm.curve, rt + rt / 2 + 1, u64::MAX);
        ok(env.send(&[pump::cu_limit_ix(400_000), ix], &[&u.kp]));
    });
    let c = curve(&h, &pm);
    assert!(c.complete && c.post_complete_base_out > 0 && c.post_complete_quote_in > 0);
    let r = h.second().clone();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert!(r.migrations.is_empty(), "migrate off");
    let gauges = |h: &Harness| {
        h.keeper
            .metrics
            .markets
            .get(&pm.market())
            .map(|m| (m.synthetic_completion_delta_bps, m.curve_base_ata_donation))
    };
    let (rho, donation) = gauges(&h).expect("the market's metrics");
    assert!(rho.is_some_and(|b| b > 0), "ρ − 1 = {rho:?} bps");
    assert_eq!(donation, Some(0), "no donation");
    assert_eq!(
        count(&r, |a| matches!(a, Alert::MigratingFvUndefined { .. })),
        0
    );
    eprintln!(
        "near166 synthetic completion of rT·1.5: ρ − 1 = {} bps, pcB {}, pcQ {}",
        rho.unwrap(),
        c.post_complete_base_out,
        c.post_complete_quote_in
    );
    // Only pcB written: the fields are not known (FV undefined).
    put(&h, OFF_POST_COMPLETE_QUOTE_IN, 0);
    let r = h.second().clone();
    assert_eq!(
        count(&r, |a| matches!(a, Alert::MigratingFvUndefined { .. })),
        1,
        "{:?}",
        r.alerts
    );
    assert_eq!(
        gauges(&h).and_then(|g| g.0),
        None,
        "no ρ without known fields"
    );
    let r = h.second().clone();
    assert_eq!(
        count(&r, |a| matches!(a, Alert::MigratingFvUndefined { .. })),
        0,
        "once"
    );
}
