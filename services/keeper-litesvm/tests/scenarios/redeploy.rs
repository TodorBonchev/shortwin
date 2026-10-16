//! AIR-18 (leads.md L-2; the owner's decision of 2026-10-12: the bounded griefing is accepted, the
//! keeper retries in the next slot). The loader's `ExtendProgram` is permissionless and redeploys
//! the program at the current slot; Agave makes the new deployment visible only from the next slot,
//! so for the rest of that slot every invoke or CPI of the program fails "Program is not deployed"
//! (`InstructionError::UnsupportedProgramId`). Anyone can do it to short.win itself (every short.win
//! instruction fails) or to a venue program (every exit that buys back on it fails). The loader
//! answers the same for a program that stays undeployable (closed, failing verification — an ELF
//! a feature gate retired); the keeper tells the one-slot refusal by the program's ProgramData,
//! whose deploy slot is then the refused slot (`events::deployed_in_refused_slot`), read as of
//! the refused slot or later.
//!
//! LiteSVM reloads a program at once after an extension, so these scenarios install the refusal
//! the cluster gives for the rest of the slot ([`not_deployed_this_slot`]: the deploy slot an
//! extension writes, and the program refused) and lift it at the next slot; a program refused for
//! good ([`refused_for_good`]) keeps its old deploy slot. The keeper classifies the one-slot
//! refusal as transient: no `ExitRefused`, `VenueFlagFailed`, `SweepFailed` or other alert, no
//! back-off — the exit, the crank, the latch flag, the S3 probe and the sweep go out again in its
//! next tick, the next slot of the same second.
//!
//! - short.win not deployed in the second a liquidation first becomes possible: the liquidation
//!   and the crank are refused (`ExitResult::NotDeployed` naming short.win, `crank_failed`),
//!   nothing is alerted, and the liquidation lands in the next slot of the same second.
//! - The mock venue program not deployed: the liquidation's buyback CPI is refused (the venue
//!   named), once — no other extras set is tried —, the crank (no venue CPI) lands, and the
//!   liquidation lands in the next slot.
//! - The same, judged through an RPC pool whose header reads without a context slot come from a
//!   node behind short.win's last deploy: the keeper reads as of the refused slot, so the refusal
//!   is still transient and the liquidation lands in the next slot.
//! - short.win not deployed in the slot the keeper first flags a Raydium upgrade: the upgrade is
//!   paged, the flag is refused without `VenueFlagFailed`, and it lands in the next slot.
//! - short.win not deployed in the slot the keeper first sees an upgrade of a fingerprinted
//!   venue (AIR-01: the flag's simulation judges it), and in the next three (a stranger extending
//!   it every slot): the refused simulation judges nothing, so the account read whole after the
//!   exits does — the upgrade is paged in the first slot, once, without `VenueFlagFailed` (an
//!   open ordered before a slot's extension still runs, and the latch is clear) and read whole
//!   once; the flag goes out in the first slot short.win runs again.
//! - The same for a fingerprinted venue's extension: booked from the whole account in the
//!   refused slot, nothing paged, nothing flagged later.
//! - short.win not deployed in the slot the keeper first probes a position's S3 (its pool halted,
//!   the chain run `s3_wait_s`): the probe is refused, and sent again in the next slot instead of
//!   `s3_probe_interval_s` later.
//! - short.win not deployed in the slot the keeper's periodic sweep is first due: the sweep is
//!   refused, and lands in the next slot instead of `sweep.check_interval_s` later.
//! - The mock venue deployed in every slot for 30 s (a stranger extending it every slot): each
//!   second the liquidation is refused and sent again, nothing else is alerted, and one
//!   `ProgramNotDeployed` pages the run once it has lasted `NOT_DEPLOYED_ALERT_S`; once it stops,
//!   the liquidation lands.
//! - The mock venue undeployable for good (its deploy slot an old one — as a venue failing
//!   verification once a feature gate retires its ELF, which the deploy slot does not show): the
//!   liquidation is refused and alerted (`ExitRefused`), every tick, never counted as transient.

use anchor_lang::prelude::Pubkey;
use shortwin::constants::{GUARD_RAYDIUM_V4, RAYDIUM_V4_PROGRAM_ID};
use shortwin::events::TraderSwept;
use shortwin_keeper::config::SweepConfig;
use shortwin_keeper::decide::Plan;
use shortwin_keeper::keeper::NOT_DEPLOYED_ALERT_S;
use shortwin_keeper::monitor;
use shortwin_keeper::sweep::SweepKind;
use shortwin_litesvm_tests::engine::{VenueMarket, liquidate_ix};
use shortwin_litesvm_tests::fork::{self, FIXTURE_GUARD_SLOTS, RaydiumProgram};
use solana_signer::Signer;

use crate::common::*;
use crate::raydium::{FART, ray_setup};
use crate::raydium_admin::{
    flags_sent, revalidate_v4_with_hash, stranger_extends, upgrade_in_place,
    v4_elf_one_byte_changed, whole_reads,
};

/// Writes `slot` into `program`'s ProgramData header: what a deploy, an upgrade or the loader's
/// `ExtendProgram` in that slot leaves.
fn set_deploy_slot(env: &mut Env, program: &Pubkey, slot: u64) {
    let key = monitor::program_data_address(program);
    let mut acc = env.svm.get_account(&key).expect("the ProgramData");
    acc.data[4..12].copy_from_slice(&slot.to_le_bytes());
    env.svm.set_account(key, acc).expect("the ProgramData");
}

/// `program` refused by the loader: its program account pointed at a missing ProgramData, a cache
/// entry the loader refuses on the same path as a deployment not yet visible and a program failing
/// verification (`ProgramCacheEntryType::Closed`, `DelayVisibility` and `FailedVerification`
/// share it: "Program is not deployed", `UnsupportedProgramId`). Returns the account [`lift`]
/// restores.
fn refuse(h: &Harness, program: &Pubkey) -> solana_account::Account {
    h.with_env(|env| {
        let saved = env.svm.get_account(program).expect("the program account");
        let mut acc = saved.clone();
        // `UpgradeableLoaderState::Program { programdata_address }` of a missing account.
        let mut data = 2u32.to_le_bytes().to_vec();
        data.extend_from_slice(Pubkey::new_unique().as_ref());
        acc.data = data;
        env.svm.set_account(*program, acc).expect("program account");
        saved
    })
}

/// What a stranger's `ExtendProgram` of `program` leaves for the rest of its slot on a cluster
/// (module docs): the current slot as its deploy slot, and the program refused.
fn not_deployed_this_slot(h: &Harness, program: &Pubkey) -> solana_account::Account {
    h.with_env(|env| {
        let slot = env.clock().slot;
        set_deploy_slot(env, program, slot);
    });
    refuse(h, program)
}

/// A program that stays undeployable (module docs): refused, its deploy slot the old one.
fn refused_for_good(h: &Harness, program: &Pubkey) -> solana_account::Account {
    refuse(h, program)
}

/// `program` deployed again: its program account restored.
fn lift(h: &Harness, program: &Pubkey, saved: solana_account::Account) {
    h.with_env(|env| {
        env.svm
            .set_account(*program, saved)
            .expect("program account")
    });
}

/// The next slot of the same second, `program` deployed again: one keeper tick.
fn next_slot(h: &mut Harness, program: &Pubkey, saved: solana_account::Account) -> TickReport {
    lift(h, program, saved);
    h.slot().clone()
}

/// The alerts a refused exit, crank, flag, probe or sweep could raise.
fn failure_alerts(r: &TickReport) -> Vec<&Alert> {
    r.alerts
        .iter()
        .filter(|a| {
            matches!(
                a,
                Alert::ExitRefused { .. }
                    | Alert::VenueFlagFailed { .. }
                    | Alert::SweepFailed { .. }
                    | Alert::Degraded { .. }
                    | Alert::ProgramNotDeployed { .. }
            )
        })
        .collect()
}

/// A mock market whose price a whale pushes up 4 % a second (the `liquidate` scenarios' trend),
/// the keeper ticking every second, until an outside simulation first accepts the liquidation of
/// its position; the keeper has not ticked in that second yet. Returns the harness, the market
/// and the position's key.
fn liquidation_possible() -> (Harness, MockMarket, Pubkey) {
    let (mut h, mms) = setup(1, KeeperConfig::default());
    let mm = mms.into_iter().next().expect("a market");
    let (b, nonce) = open(&h, &mm, Q);
    let pos = position_key(&mm, &b.key(), nonce);
    let oracle = Oracle::new(&h);
    let whale = h.with_env(|env| env.actor(&mm, 0, 5_000_000_000_000));
    h.seconds(5);
    for s in 0..120 {
        h.with_env(|env| {
            env.warp(1);
            let target = env.spot(&mm) * 104 / 100;
            env.push_price(&mm, &whale, target);
        });
        let ix = liquidate_ix(&mm, &oracle.kp.pubkey(), &b.key(), nonce);
        if oracle.accepts(&h, ix).is_ok() {
            return (h, mm, pos);
        }
        let r = h.tick().clone();
        assert!(r.errors.is_empty(), "second {s}: {:?}", r.errors);
        assert!(landed_for(&r, ExitKind::Liquidate, &pos).next().is_none());
    }
    panic!("the liquidation never became possible");
}

/// The liquidation attempts of `position` in `r`.
fn liquidations<'a>(r: &'a TickReport, position: &Pubkey) -> Vec<&'a ExitResult> {
    r.exits
        .iter()
        .filter(|e| e.position == *position && e.kind == ExitKind::Liquidate)
        .map(|e| &e.result)
        .collect()
}

/// short.win not deployed in the second a liquidation first becomes possible (module docs).
#[test]
fn a_liquidation_refused_while_short_win_is_not_deployed_lands_the_next_slot() {
    let (mut h, mm, pos) = liquidation_possible();
    let key = mm.market();
    let now = h.now();
    let saved = not_deployed_this_slot(&h, &shortwin::ID);
    let r = h.tick().clone();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert!(
        matches!(
            liquidations(&r, &pos).as_slice(),
            [ExitResult::NotDeployed { program, .. }] if *program == shortwin::ID
        ),
        "{:?}",
        r.exits
    );
    assert!(
        r.crank_failed
            .iter()
            .any(|(k, why)| *k == key && why.contains("not deployed")),
        "{:?}",
        r.crank_failed
    );
    assert!(!r.observed(&key));
    assert!(failure_alerts(&r).is_empty(), "{:?}", r.alerts);
    let refusals = h.keeper.metrics.not_deployed;
    assert!(refusals >= 2, "the liquidation and the crank: {refusals}");

    let r = next_slot(&mut h, &shortwin::ID, saved);
    assert_eq!(h.now(), now, "the next slot of the same second");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert!(
        landed_for(&r, ExitKind::Liquidate, &pos).next().is_some(),
        "{:?}",
        r.exits
    );
    assert!(
        r.observed(&key),
        "the liquidation is the second's observation"
    );
    assert!(failure_alerts(&r).is_empty(), "{:?}", r.alerts);
    assert_eq!(h.keeper.metrics.not_deployed, refusals);
    assert_eq!(h.keeper.metrics.markets[&key].liquidations, 1);
}

/// The refusal judged through a lagging RPC pool (module docs): the pool's header reads that name
/// no context slot see short.win at its deploy before this slot's. The keeper reads short.win's
/// ProgramData as of the refused slot, so the refusal stays transient; read without a context
/// slot, the old deploy slot made it a refusal that stays (`ExitRefused`).
#[test]
fn a_refusal_judged_through_a_lagging_rpc_pool_is_still_transient() {
    let (mut h, _mm, pos) = liquidation_possible();
    let pd = monitor::program_data_address(&shortwin::ID);
    let before = h.with_env(|env| {
        let acc = env.svm.get_account(&pd).expect("short.win's ProgramData");
        u64::from_le_bytes(acc.data[4..12].try_into().expect("a deploy slot"))
    });
    let saved = not_deployed_this_slot(&h, &shortwin::ID);
    h.chain().lag_headers_of(pd, before, 100);
    let r = h.tick().clone();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert!(
        matches!(
            liquidations(&r, &pos).as_slice(),
            [ExitResult::NotDeployed { program, .. }] if *program == shortwin::ID
        ),
        "{:?}",
        r.exits
    );
    assert!(failure_alerts(&r).is_empty(), "{:?}", r.alerts);

    let r = next_slot(&mut h, &shortwin::ID, saved);
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert!(
        landed_for(&r, ExitKind::Liquidate, &pos).next().is_some(),
        "{:?}",
        r.exits
    );
    assert!(failure_alerts(&r).is_empty(), "{:?}", r.alerts);
}

/// The mock venue program not deployed in the second a liquidation first becomes possible
/// (module docs).
#[test]
fn a_liquidation_refused_while_the_venue_is_not_deployed_lands_the_next_slot() {
    let (mut h, mm, pos) = liquidation_possible();
    let key = mm.market();
    let now = h.now();
    let venue = mock::PROGRAM_ID;
    let saved = not_deployed_this_slot(&h, &venue);
    let r = h.tick().clone();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert!(
        matches!(
            liquidations(&r, &pos).as_slice(),
            [ExitResult::NotDeployed { program, .. }] if *program == venue
        ),
        "one attempt, the venue named: {:?}",
        r.exits
    );
    assert!(
        r.cranked.contains(&key),
        "crank_price makes no venue CPI: {:?}",
        r.crank_failed
    );
    assert!(failure_alerts(&r).is_empty(), "{:?}", r.alerts);

    let r = next_slot(&mut h, &venue, saved);
    assert_eq!(h.now(), now, "the next slot of the same second");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert!(
        landed_for(&r, ExitKind::Liquidate, &pos).next().is_some(),
        "{:?}",
        r.exits
    );
    assert!(failure_alerts(&r).is_empty(), "{:?}", r.alerts);
}

/// short.win not deployed in the slot the keeper first flags a Raydium upgrade (module docs).
#[test]
fn a_venue_flag_refused_while_short_win_is_not_deployed_lands_the_next_slot() {
    let (mut h, _ms) = ray_setup(&[FART], KeeperConfig::default());
    let venue = GUARD_RAYDIUM_V4;
    let slot0 = FIXTURE_GUARD_SLOTS[usize::from(venue)];
    let r = h.slot().clone();
    assert!(r.flagged.is_empty() && r.errors.is_empty());
    h.with_env(|env| fork::set_deploy_slot(&mut env.svm, RaydiumProgram::AmmV4, slot0 + 777));
    let saved = not_deployed_this_slot(&h, &shortwin::ID);
    let r = h.slot().clone();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert!(r.flagged.is_empty());
    assert!(r.alerts.contains(&Alert::VenueUpgraded {
        venue,
        program: RAYDIUM_V4_PROGRAM_ID,
        known_slot: slot0,
        deploy_slot: slot0 + 777,
    }));
    assert!(failure_alerts(&r).is_empty(), "{:?}", r.alerts);

    let r = next_slot(&mut h, &shortwin::ID, saved);
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.flagged, vec![(venue, slot0 + 777)], "{:?}", r.alerts);
    assert!(failure_alerts(&r).is_empty(), "{:?}", r.alerts);
    assert!(h.with_env(|env| env.venue_guard()).is_flagged(venue));
}

/// short.win not deployed in the slot the keeper first sees an upgrade of a fingerprinted venue,
/// and in the next three (module docs): AMM v4 revalidated with its trimmed ELF's hash, then
/// upgraded in place. As first built the refused slots judged and paged nothing, so a stranger
/// extending short.win in every slot kept the upgrade unpaged for the whole run.
#[test]
fn a_fingerprinted_venues_upgrade_seen_while_short_win_is_not_deployed_is_paged_at_once() {
    let (mut h, _ms) = ray_setup(&[FART], KeeperConfig::default());
    let venue = GUARD_RAYDIUM_V4;
    let id = RaydiumProgram::AmmV4.id();
    let pd = RaydiumProgram::AmmV4.program_data().address;
    let slot0 = revalidate_v4_with_hash(&h);
    let r = h.slot().clone();
    assert!(r.flagged.is_empty() && r.errors.is_empty());
    let up = h.with_env(|env| upgrade_in_place(env, &id, &v4_elf_one_byte_changed()));
    let saved = not_deployed_this_slot(&h, &shortwin::ID);
    let mut paged = Vec::new();
    for s in 0..4 {
        if s > 0 {
            // The next slot of the same second, short.win extended again.
            h.with_env(|env| {
                let c = env.clock();
                env.set_clock(c.unix_timestamp, c.slot + 1);
                set_deploy_slot(env, &shortwin::ID, c.slot + 1);
            });
        }
        let r = h.tick().clone();
        assert!(r.errors.is_empty(), "slot {s}: {:?}", r.errors);
        assert!(
            r.flagged.is_empty() && r.extended.is_empty(),
            "slot {s}: {r:?}"
        );
        assert!(failure_alerts(&r).is_empty(), "slot {s}: {:?}", r.alerts);
        paged.extend(
            r.alerts
                .iter()
                .filter(|a| matches!(a, Alert::VenueUpgraded { .. }))
                .map(|a| (s, a.clone())),
        );
    }
    assert_eq!(
        paged,
        vec![(
            0,
            Alert::VenueUpgraded {
                venue,
                program: RAYDIUM_V4_PROGRAM_ID,
                known_slot: slot0,
                deploy_slot: up,
            }
        )]
    );
    assert_eq!(whole_reads(&h, pd), 1, "that deploy read whole once");
    assert!(!h.with_env(|env| env.venue_guard()).is_flagged(venue));

    let r = next_slot(&mut h, &shortwin::ID, saved);
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.flagged, vec![(venue, up)], "{:?}", r.alerts);
    assert!(failure_alerts(&r).is_empty(), "{:?}", r.alerts);
    assert!(
        !r.alerts
            .iter()
            .any(|a| matches!(a, Alert::VenueUpgraded { .. })),
        "paged once: {:?}",
        r.alerts
    );
    assert!(h.with_env(|env| env.venue_guard()).is_flagged(venue));
}

/// A fingerprinted venue's extension seen while short.win is not deployed (module docs): AMM v4
/// revalidated with its trimmed ELF's hash, then extended by a stranger in the slot short.win
/// is refused.
#[test]
fn a_fingerprinted_venues_extension_seen_while_short_win_is_not_deployed_is_booked_at_once() {
    let (mut h, _ms) = ray_setup(&[FART], KeeperConfig::default());
    let id = RaydiumProgram::AmmV4.id();
    revalidate_v4_with_hash(&h);
    let r = h.slot().clone();
    assert!(r.flagged.is_empty() && r.errors.is_empty());
    let ext = h.with_env(|env| stranger_extends(env, &id));
    let saved = not_deployed_this_slot(&h, &shortwin::ID);
    let r = h.tick().clone();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.extended, vec![(id, ext)], "{:?}", r.alerts);
    assert!(r.flagged.is_empty());
    let venue_pages = |r: &TickReport| {
        r.alerts
            .iter()
            .filter(|a| {
                matches!(
                    a,
                    Alert::VenueUpgraded { .. } | Alert::VenueFlagFailed { .. }
                )
            })
            .count()
    };
    assert_eq!(venue_pages(&r), 0, "{:?}", r.alerts);
    assert!(failure_alerts(&r).is_empty(), "{:?}", r.alerts);

    let r = next_slot(&mut h, &shortwin::ID, saved);
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert!(r.extended.is_empty() && r.flagged.is_empty(), "{r:?}");
    assert_eq!(venue_pages(&r), 0, "{:?}", r.alerts);
    assert_eq!(flags_sent(&h), 0);
    assert_eq!(h.keeper.metrics.venue_extensions, 1);
}

/// The S3 probes of `position` in `r` (`sol_settle` simulated alone: `Plan::S3Probe`).
fn probes<'a>(r: &'a TickReport, position: &Pubkey) -> Vec<&'a ExitResult> {
    r.exits
        .iter()
        .filter(|e| {
            e.position == *position && e.kind == ExitKind::SolSettle && e.plan == Plan::S3Probe
        })
        .map(|e| &e.result)
        .collect()
}

/// short.win not deployed in the slot the keeper first probes a position's S3 (module docs).
#[test]
fn an_s3_probe_refused_while_short_win_is_not_deployed_is_sent_again_the_next_slot() {
    let (mut h, mms) = setup(1, KeeperConfig::default());
    let mm = mms.into_iter().next().expect("a market");
    let key = mm.market();
    let (b, nonce) = open(&h, &mm, Q);
    let pos = position_key(&mm, &b.key(), nonce);
    h.seconds(2);
    h.with_env(|env| mock::update_knobs(env, &mm.pool, |k| k.disabled = true));
    // S3 is due `s3_wait_s` (600 s) after the chain starts and the market's graduation grace
    // (`grad_grace_s`, 600 s after its creation) ends.
    for s in 0..1_500 {
        h.with_env(|env| env.warp(1));
        let probe = keeper_eval(&h, key, pos).is_some_and(|(e, _, _)| e.plan == Plan::S3Probe);
        if !probe {
            let r = h.tick().clone();
            assert!(r.errors.is_empty(), "second {s}: {:?}", r.errors);
            assert!(probes(&r, &pos).is_empty(), "second {s}: {:?}", r.exits);
            continue;
        }
        let saved = not_deployed_this_slot(&h, &shortwin::ID);
        let r = h.tick().clone();
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert!(
            matches!(
                probes(&r, &pos).as_slice(),
                [ExitResult::NotDeployed { program, .. }] if *program == shortwin::ID
            ),
            "{:?}",
            r.exits
        );
        assert!(failure_alerts(&r).is_empty(), "{:?}", r.alerts);
        // The refusal held no probe: the next slot probes again (the program settles nothing:
        // the position is not expired), and then the throttle holds.
        let r = next_slot(&mut h, &shortwin::ID, saved);
        assert!(r.errors.is_empty(), "{:?}", r.errors);
        assert!(
            matches!(
                probes(&r, &pos).as_slice(),
                [ExitResult::NoOp | ExitResult::Refused { .. }]
            ),
            "probed again: {:?}",
            r.exits
        );
        assert!(failure_alerts(&r).is_empty(), "{:?}", r.alerts);
        let r = h.slot().clone();
        assert!(probes(&r, &pos).is_empty(), "throttled: {:?}", r.exits);
        assert!(position_open(&h, &mm, &b.key(), nonce));
        return;
    }
    panic!("no S3 probe");
}

/// short.win not deployed in the slot the keeper's periodic sweep is first due (module docs):
/// somebody sent lamports to the trader PDA; the sweep the first tick sends is refused, nothing
/// is alerted, and it lands in the next slot of the same second.
#[test]
fn a_sweep_refused_while_short_win_is_not_deployed_lands_the_next_slot() {
    let mut env = Env::test_venue();
    let mm = env.mock_market(MarketSpec::default());
    let market = mm.market();
    let trader = mm.keys().trader;
    let donor = env.funded_keypair();
    ok(env.send(
        &[transfer_ix(&donor.pubkey(), &trader, 1_000_000)],
        &[&donor],
    ));
    let cfg = KeeperConfig {
        sweep: SweepConfig {
            check_interval_s: 10,
            ..SweepConfig::default()
        },
        ..KeeperConfig::default()
    };
    let mut h = Harness::new(env, cfg);
    h.with_env(|env| env.warp(1));
    let now = h.now();
    let saved = not_deployed_this_slot(&h, &shortwin::ID);
    let r = h.tick().clone();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    let sweeps: Vec<_> = r.sweeps.iter().filter(|x| x.market == market).collect();
    assert!(
        matches!(
            sweeps.as_slice(),
            [rec] if rec.kind == SweepKind::Sweep
                && matches!(rec.result, ExitResult::NotDeployed { program, .. } if program == shortwin::ID)
        ),
        "{sweeps:?}"
    );
    assert!(failure_alerts(&r).is_empty(), "{:?}", r.alerts);

    let r = next_slot(&mut h, &shortwin::ID, saved);
    assert_eq!(h.now(), now, "the next slot of the same second");
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    let rec = r
        .sweeps
        .iter()
        .find(|x| x.market == market)
        .expect("swept again");
    let out = rec.result.landed().expect("landed");
    let ev = shortwin_keeper::events::of::<TraderSwept>(out)
        .pop()
        .expect("TraderSwept");
    assert!(ev.lamports >= 1_000_000);
    assert!(failure_alerts(&r).is_empty(), "{:?}", r.alerts);
}

/// The mock venue deployed in every slot for 30 s (module docs).
#[test]
fn a_venue_deployed_in_every_slot_for_30_s_is_paged_once() {
    let (mut h, _mm, pos) = liquidation_possible();
    let venue = mock::PROGRAM_ID;
    let since = h.now();
    let saved = not_deployed_this_slot(&h, &venue);
    let mut paged = Vec::new();
    for s in 0..=NOT_DEPLOYED_ALERT_S {
        if s > 0 {
            h.with_env(|env| {
                env.warp(1);
                let slot = env.clock().slot;
                set_deploy_slot(env, &venue, slot);
            });
        }
        let r = h.tick().clone();
        assert!(r.errors.is_empty(), "second {s}: {:?}", r.errors);
        assert!(
            matches!(
                liquidations(&r, &pos).as_slice(),
                [ExitResult::NotDeployed { program, .. }] if *program == venue
            ),
            "second {s}: {:?}",
            r.exits
        );
        for a in failure_alerts(&r) {
            paged.push((s, a.clone()));
        }
    }
    assert_eq!(
        paged,
        vec![(
            NOT_DEPLOYED_ALERT_S,
            Alert::ProgramNotDeployed {
                program: venue,
                since,
                refusals: u64::try_from(NOT_DEPLOYED_ALERT_S).unwrap() + 1,
            }
        )]
    );
    let r = next_slot(&mut h, &venue, saved);
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert!(
        landed_for(&r, ExitKind::Liquidate, &pos).next().is_some(),
        "{:?}",
        r.exits
    );
    assert!(failure_alerts(&r).is_empty(), "{:?}", r.alerts);
}

/// The mock venue undeployable for good (module docs).
#[test]
fn a_venue_that_stays_undeployable_is_refused_and_alerted() {
    let (mut h, mm, pos) = liquidation_possible();
    let venue = mock::PROGRAM_ID;
    let _saved = refused_for_good(&h, &venue);
    for (s, r) in [h.tick().clone(), h.second().clone()]
        .into_iter()
        .enumerate()
    {
        assert!(r.errors.is_empty(), "{s}: {:?}", r.errors);
        assert!(
            matches!(
                liquidations(&r, &pos).as_slice(),
                [ExitResult::Refused { code: None, .. }]
            ),
            "{s}: {:?}",
            r.exits
        );
        assert!(
            r.alerts.iter().any(|a| matches!(
                a,
                Alert::ExitRefused { position, code: None, detail, .. }
                    if *position == pos && detail.contains("UnsupportedProgramId")
            )),
            "{s}: {:?}",
            r.alerts
        );
        assert!(
            r.cranked.contains(&mm.market()) || r.observed(&mm.market()),
            "{s}: the crank lands"
        );
    }
    assert_eq!(h.keeper.metrics.not_deployed, 0, "never transient");
}
