//! Duty 10 on Raydium (ARCH §7.1, §5.5; §9 M3 "admin-action monitoring in keeper v1"): the real
//! admin instructions of the dumped programs, signed as the pool admin `GThUX1…` (signature
//! checks off for that one transaction, `fork::raydium_admin_send`), and upgrades of the dumped
//! ProgramData.
//!
//! - AMM v4 `SetParams` status 2 halts a pool: the keeper reports it at once, starts the S3 chain
//!   the same second, keeps it continuous, settles the expired position in S3, leaves the
//!   healthy one frozen, and reports the resume.
//! - Fees: AMM v4 `SetParams` fees and CPMM `update_amm_config` between and above the caps,
//!   reported against the opens cap and the hard cap; above the hard cap the market runs its S3
//!   chain (`s3_no_route`) while still executable.
//! - CPMM `update_pool_status`: the swap bit is a lost route, the deposit / withdraw bits are
//!   only a status change.
//! - CPMM `update_amm_config(0, 0)` (M3-SEC-1): a zero trade fee on a pool whose creator fee is
//!   on the buy's input is a lost route (every buy reverts with `ZeroTradingTokens`), reported
//!   with why; the S3 chain runs and the healthy position stays open; the fee restored, it
//!   closes.
//! - Upgrades: a changed deploy slot of either Raydium program is flagged with
//!   `flag_venue_upgraded` in the slot the keeper sees it (opens on every Raydium market then
//!   refused), once; a second upgrade during review refreshes the flag; after
//!   `revalidate_venue` nothing is sent; a flag someone else sent first is not repeated;
//!   Token-2022's upgrade is alerted only.
//! - AIR-01: once the Emergency revalidated AMM v4 naming its trimmed ELF's sha256, a stranger's
//!   `ExtendProgram` (the loader's own instruction) is neither flagged nor paged — the flag's
//!   simulation answers `VenueNotUpgraded`, once per deploy slot — and Raydium opens keep
//!   landing; an upgrade after the extensions is flagged in the tick it is seen, as that tick's
//!   first transaction, before its crank; an extension of that deploy during its review is
//!   neither refreshed nor paged; the revalidation of the new ELF ends the review. A keeper
//!   restarted during a review records the deploy under review, so an extension of it is not
//!   paged either. When the flag's simulation fails, the account read whole after the exits
//!   decides (an extension is not paged, an upgrade is, with `VenueFlagFailed`), and when that
//!   read fails too the moved slot is paged; Token-2022's extension is not paged, its upgrade is.
//!   A `VenueNotUpgraded` answered by a node behind the header read (a context slot before the
//!   deploy) is not booked as an extension: the whole account pages the upgrade and the next
//!   slot flags it. Two venues extended in one slot are simulated together: one priority-fee
//!   read, one blockhash, two simulations.

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::Instruction;
use shortwin::constants::{GUARD_RAYDIUM_CPMM, GUARD_RAYDIUM_V4, TOKEN_2022_PROGRAM_ID};
use shortwin::errors::ShortwinError;
use shortwin::events::SolSettle;
use shortwin_keeper::events;
use shortwin_keeper::monitor::{self, AdminChange};
use shortwin_litesvm_tests::engine::VenueMarket;
use shortwin_litesvm_tests::errors::custom_code;
use shortwin_litesvm_tests::fork::{self, FIXTURE_GUARD_SLOTS, RaydiumProgram, V4Fees};
use shortwin_litesvm_tests::{fixtures, ix, read};
use shortwin_venue_state::program_data;
use solana_signer::Signer;

use crate::common::*;
use crate::raydium::*;

/// The `VenueAdmin` changes of `market` in `r`.
fn changes(r: &TickReport, market: &Pubkey) -> Vec<AdminChange> {
    r.alerts
        .iter()
        .filter_map(|a| match a {
            Alert::VenueAdmin {
                market: m, change, ..
            } if m == market => Some(change.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn an_amm_v4_halt_is_reported_the_chain_runs_and_the_expired_position_settles_in_s3() {
    let (mut h, ms) = ray_setup(&[FART], KeeperConfig::default());
    let (rm, q) = &ms[0];
    let key = rm.market();
    let (b1, n1) = ray_open(&h, rm, *q);
    let pos1 = position_of(rm, &b1.key(), n1);
    let deadline = h.with_env(|env| env.position(rm, &b1.key(), n1).deadline);
    h.seconds(2);
    // A second position, opened an hour later: healthy and before its deadline at the end.
    h.idle(3_600);
    h.seconds(2);
    let (b2, n2) = ray_open(&h, rm, q / 3);
    h.seconds(2);
    let wait = deadline - 700 - h.now();
    h.idle(wait);
    h.seconds(5);
    // Fartcoin's pool runs as 6 (SwapOnly), as dumped.
    let status0 = h.with_env(|env| rm.v4().info(&env.svm).status);
    assert_eq!(status0, 6);
    // The pool admin disables swaps: AMM v4 SetParams (param 0) status 2.
    h.with_env(|env| {
        ok(fork::raydium_admin_send(
            env,
            fork::v4_set_params_ix(rm.v4(), 0, 2),
        ))
    });
    let halted_at = h.now();

    let mut since = None;
    let mut last_crank = halted_at;
    let mut settled = None;
    let mut reported = false;
    for s in 0..1_000 {
        let r = h.second().clone();
        let now = h.now();
        assert!(r.errors.is_empty(), "second {s}: {:?}", r.errors);
        let c = changes(&r, &key);
        if s == 0 {
            // Reported at the first tick after the halt: the route and the status.
            assert!(
                matches!(&c[0], AdminChange::RouteLost { detail } if detail.contains("2 Disabled")),
                "{c:?}"
            );
            assert!(
                matches!(
                    c[1],
                    AdminChange::StatusChanged {
                        from: Some(6),
                        to: Some(2),
                        ..
                    }
                ),
                "{c:?}"
            );
            assert_eq!(c.len(), 2, "{c:?}");
            assert!(r.needs[&key].no_route && r.needs[&key].start_chain);
            reported = true;
        } else {
            assert!(c.is_empty(), "second {s}: reported again: {c:?}");
        }
        if r.cranked.contains(&key) {
            assert!(now - last_crank <= 30, "crank gap {} s", now - last_crank);
            last_crank = now;
        }
        let m = h.with_env(|env| env.market(&key));
        if m.no_route_since != 0 {
            assert_eq!(*since.get_or_insert(m.no_route_since), m.no_route_since);
        }
        if let Some(out) = landed_for(&r, ExitKind::SolSettle, &pos1).next() {
            let ev: Vec<SolSettle> = events::of(out);
            if let Some(e) = ev.first() {
                assert_eq!(e.condition, 3, "S3");
                settled = Some(now);
                break;
            }
        }
    }
    assert!(reported);
    let since = since.expect("the S3 chain never started");
    assert_eq!(
        since,
        halted_at + 1,
        "the chain starts at the keeper's next tick"
    );
    let settled = settled.expect("no S3 settlement");
    println!(
        "AMM v4 halted at {halted_at}, chain since {since}, deadline {deadline}, S3 at {settled}"
    );
    assert!(settled >= deadline && settled >= since + 600, "{settled}");
    assert!(settled <= deadline.max(since + 600) + 1, "late: {settled}");
    assert!(!is_open(&h, rm, &b1.key(), n1));
    assert!(
        is_open(&h, rm, &b2.key(), n2),
        "the healthy position is frozen"
    );

    // The admin resumes swaps: reported, the market executes again, the position trades.
    h.with_env(|env| {
        ok(fork::raydium_admin_send(
            env,
            fork::v4_set_params_ix(rm.v4(), 0, 6),
        ))
    });
    let r = h.second().clone();
    let c = changes(&r, &key);
    assert!(
        matches!(&c[0], AdminChange::RouteRestored { detail } if detail.contains("6 SwapOnly")),
        "{c:?}"
    );
    assert!(matches!(
        c[1],
        AdminChange::StatusChanged {
            from: Some(2),
            to: Some(6),
            ..
        }
    ));
    assert!(r.needs.get(&key).is_some_and(|n| !n.no_route));
    h.seconds(2);
    ok(h.with_env(|env| env.close(rm, &b2, n2)));
}

#[test]
fn fee_changes_are_reported_against_the_caps_on_both_programs() {
    let (mut h, ms) = ray_setup(&[FART, USELESS], KeeperConfig::default());
    for (rm, q) in &ms {
        let _ = ray_open(&h, rm, *q);
    }
    let (fart, useless) = (&ms[0].0, &ms[1].0);
    let r = h.second().clone();
    assert!(venue_alerts(&r).is_empty(), "{:?}", r.alerts);
    let caps = monitor::FeeCaps::of(&h.with_env(|env| env.config()));
    assert_eq!((caps.open_ppm, caps.hard_ppm), (15_000, 50_000));
    let v4_fee = h.with_env(|env| fart.v4().info(&env.svm).fee_ppm().unwrap());
    let cpmm_fee = h.with_env(|env| {
        let p = useless.cpmm();
        p.state(&env.svm).fee_ppm(&p.config(&env.svm)).unwrap()
    });
    let set_v4 = |h: &Harness, num: u64| {
        h.with_env(|env| {
            let mut f = V4Fees::of(&env.svm, fart.v4());
            f.swap_fee = (num, 100);
            ok(fork::raydium_admin_send(
                env,
                fork::v4_set_fees_ix(fart.v4(), &f),
            ));
        })
    };
    let config = h.with_env(|env| useless.cpmm().config_key(&env.svm));
    let trade0 = h.with_env(|env| useless.cpmm().config(&env.svm).trade_fee_rate);
    let set_cpmm = |h: &Harness, trade_ppm: u64| {
        h.with_env(|env| {
            ok(fork::raydium_admin_send(
                env,
                fork::cpmm_update_amm_config_ix(&config, 0, trade_ppm),
            ));
        })
    };

    // AMM v4 SetParams fees: 3 % (between the caps): opens refused, no S3 evidence.
    set_v4(&h, 3);
    let r = h.second().clone();
    assert_eq!(
        changes(&r, &fart.market()),
        vec![AdminChange::FeeChanged {
            from_ppm: Some(v4_fee),
            to_ppm: 30_000,
            above_open_cap: true,
            above_hard_cap: false,
            via: "AMM v4 SetParams (param 2, fees)",
        }]
    );
    assert!(!r.needs[&fart.market()].no_route);
    let mut b = h.with_env(|env| env.borrower());
    let res = h.with_env(|env| env.open(fart, &mut b, ms[0].1 / 2).0);
    assert_eq!(
        custom_code(&res),
        Some(u32::from(ShortwinError::VenueFeeTooHigh))
    );

    // CPMM update_amm_config trade fee 6 % (above the hard cap), for every pool on the config:
    // s3_no_route while the venue still executes; the keeper starts the S3 chain at once.
    set_cpmm(&h, 60_000);
    let r = h.second().clone();
    let c = changes(&r, &useless.market());
    let AdminChange::FeeChanged {
        from_ppm,
        to_ppm,
        above_open_cap,
        above_hard_cap,
        via,
    } = &c[0]
    else {
        panic!("{c:?}");
    };
    assert_eq!(*from_ppm, Some(cpmm_fee));
    assert!(
        *to_ppm >= 60_000 && *above_open_cap && *above_hard_cap,
        "{c:?}"
    );
    assert!(via.contains("update_amm_config"));
    let needs = r.needs[&useless.market()];
    assert!(needs.no_route && needs.start_chain, "{needs:?}");
    let m = h.with_env(|env| env.market(&useless.market()));
    assert_eq!(m.no_route_since, h.now(), "observed the same second");

    // Both fees restored: reported, below both caps, the route back.
    h.with_env(|env| {
        let mut f = V4Fees::of(&env.svm, fart.v4());
        f.swap_fee = (25, 10_000);
        ok(fork::raydium_admin_send(
            env,
            fork::v4_set_fees_ix(fart.v4(), &f),
        ));
    });
    assert_eq!(v4_fee, 2_500, "the dumped pool's 25/10,000");
    set_cpmm(&h, trade0);
    let r = h.second().clone();
    for (key, want) in [(fart.market(), v4_fee), (useless.market(), cpmm_fee)] {
        let c = changes(&r, &key);
        assert!(
            matches!(
                c.as_slice(),
                [AdminChange::FeeChanged {
                    to_ppm,
                    above_open_cap: false,
                    above_hard_cap: false,
                    ..
                }] if *to_ppm == want
            ),
            "{c:?}"
        );
        assert!(!r.needs[&key].no_route);
    }
}

/// M3-SEC-1: CPMM `update_amm_config(0, 0)` sets config 0's trade fee to 0. USELESS has no
/// creator fee enabled and `creator_fee_on = 0`, so every buy reverts in the venue
/// (`ZeroTradingTokens`) although the status is clear: the keeper reports a lost route (with
/// why) besides the fee change, the S3 chain starts at its next tick and stays continuous, and
/// the healthy position stays open past `s3_wait_s` (no S1 at an unchanged price, no S3 while
/// healthy). With the fee back, the route is restored and the position closes.
#[test]
fn a_zero_cpmm_trade_fee_is_a_lost_route_and_healthy_positions_stay_open() {
    let (mut h, ms) = ray_setup(&[USELESS], KeeperConfig::default());
    let (rm, q) = &ms[0];
    let key = rm.market();
    let (b, n) = ray_open(&h, rm, *q);
    let pos = position_of(rm, &b.key(), n);
    h.seconds(3);
    let config = h.with_env(|env| rm.cpmm().config_key(&env.svm));
    let trade0 = h.with_env(|env| rm.cpmm().config(&env.svm).trade_fee_rate);
    let fee0 = h.with_env(|env| {
        let p = rm.cpmm();
        p.state(&env.svm).fee_ppm(&p.config(&env.svm)).unwrap()
    });
    h.with_env(|env| {
        ok(fork::raydium_admin_send(
            env,
            fork::cpmm_update_amm_config_ix(&config, 0, 0),
        ))
    });
    let at = h.now();
    let wait = h.with_env(|env| env.position(rm, &b.key(), n).s3_wait_s);
    let mut last_crank = at;
    for s in 0..(i64::from(wait) + 120) {
        let r = h.second().clone();
        let now = h.now();
        assert!(r.errors.is_empty(), "second {s}: {:?}", r.errors);
        let c = changes(&r, &key);
        if s == 0 {
            assert!(
                matches!(&c[0], AdminChange::RouteLost { detail }
                    if detail.contains("ZeroTradingTokens") && detail.contains("swap bit clear")),
                "{c:?}"
            );
            assert_eq!(
                c[1],
                AdminChange::FeeChanged {
                    from_ppm: Some(fee0),
                    to_ppm: 0,
                    above_open_cap: false,
                    above_hard_cap: false,
                    via: "CPMM update_amm_config (every pool on the config)",
                }
            );
            assert_eq!(c.len(), 2, "{c:?}");
            assert!(c[0].is_severe());
            assert!(r.needs[&key].no_route && r.needs[&key].start_chain);
            assert_eq!(
                h.with_env(|env| env.market(&key)).no_route_since,
                now,
                "the chain starts at the keeper's first tick"
            );
        } else {
            assert!(c.is_empty(), "second {s}: reported again: {c:?}");
        }
        if r.cranked.contains(&key) {
            assert!(now - last_crank <= 30, "crank gap {} s", now - last_crank);
            last_crank = now;
        }
        let m = h.with_env(|env| env.market(&key));
        assert_eq!(m.no_route_since, at + 1, "second {s}: one continuous chain");
        for out in landed_for(&r, ExitKind::SolSettle, &pos) {
            let ev: Vec<SolSettle> = events::of(out);
            assert!(
                ev.is_empty(),
                "second {s}: a healthy position settled (S{:?})",
                ev.first().map(|e| e.condition)
            );
        }
        assert!(is_open(&h, rm, &b.key(), n), "second {s}");
    }
    assert!(h.now() >= at + i64::from(wait) + 60);
    // The fee restored: the route is back, the position closes.
    h.with_env(|env| {
        ok(fork::raydium_admin_send(
            env,
            fork::cpmm_update_amm_config_ix(&config, 0, trade0),
        ))
    });
    let r = h.second().clone();
    let c = changes(&r, &key);
    assert!(matches!(c[0], AdminChange::RouteRestored { .. }), "{c:?}");
    assert!(r.needs.get(&key).is_some_and(|n| !n.no_route));
    h.seconds(2);
    ok(h.with_env(|env| env.close(rm, &b, n)));
}

#[test]
fn cpmm_status_bits_swap_is_a_lost_route_deposit_is_only_a_status_change() {
    let (mut h, ms) = ray_setup(&[USELESS], KeeperConfig::default());
    let (rm, q) = &ms[0];
    let key = rm.market();
    let _ = ray_open(&h, rm, *q);
    h.seconds(2);
    let status = |h: &Harness, s: u8| {
        h.with_env(|env| {
            ok(fork::raydium_admin_send(
                env,
                fork::cpmm_update_pool_status_ix(rm.cpmm(), s),
            ))
        })
    };
    status(&h, 4);
    let r = h.second().clone();
    let c = changes(&r, &key);
    assert!(
        matches!(&c[0], AdminChange::RouteLost { detail } if detail.contains("swaps disabled")),
        "{c:?}"
    );
    assert!(matches!(
        c[1],
        AdminChange::StatusChanged {
            from: Some(0),
            to: Some(4),
            via: "CPMM update_pool_status",
        }
    ));
    assert!(r.observed(&key), "the chain starts at once");
    assert_eq!(h.with_env(|env| env.market(&key)).no_route_since, h.now());
    // Deposit disabled (bit 0) only: swaps work again.
    status(&h, 1);
    let r = h.second().clone();
    let c = changes(&r, &key);
    assert!(matches!(c[0], AdminChange::RouteRestored { .. }), "{c:?}");
    assert!(matches!(
        c[1],
        AdminChange::StatusChanged {
            from: Some(4),
            to: Some(1),
            ..
        }
    ));
    status(&h, 0);
    let r = h.second().clone();
    assert_eq!(
        changes(&r, &key),
        vec![AdminChange::StatusChanged {
            from: Some(1),
            to: Some(0),
            via: "CPMM update_pool_status",
        }]
    );
}

#[test]
fn upgrades_are_flagged_in_the_slot_they_are_seen_once_and_refreshed_during_review() {
    let (mut h, ms) = ray_setup(&[FART, USELESS], KeeperConfig::default());
    let v4_slot = FIXTURE_GUARD_SLOTS[usize::from(GUARD_RAYDIUM_V4)];
    let cpmm_slot = FIXTURE_GUARD_SLOTS[usize::from(GUARD_RAYDIUM_CPMM)];
    // Token-2022's ProgramData (LiteSVM loads Token-2022 without one): watched, alert only.
    let t22_pd = monitor::program_data_address(&TOKEN_2022_PROGRAM_ID);
    h.with_env(|env| fixtures::set_program_data(&mut env.svm, &t22_pd, 400_000_000, None, 64));
    let r = h.slot().clone();
    assert!(r.flagged.is_empty() && venue_alerts(&r).is_empty(), "{r:?}");

    // An AMM v4 upgrade: flagged in the keeper's next tick (the next slot).
    h.with_env(|env| fork::set_deploy_slot(&mut env.svm, RaydiumProgram::AmmV4, v4_slot + 777));
    let r = h.slot().clone();
    assert_eq!(
        r.flagged,
        vec![(GUARD_RAYDIUM_V4, v4_slot + 777)],
        "{:?} {:?}",
        r.alerts,
        r.errors
    );
    let a = venue_alerts(&r);
    assert!(
        a.contains(&Alert::VenueUpgraded {
            venue: GUARD_RAYDIUM_V4,
            program: shortwin::constants::RAYDIUM_V4_PROGRAM_ID,
            known_slot: v4_slot,
            deploy_slot: v4_slot + 777,
        }),
        "{a:?}"
    );
    assert!(a.contains(&Alert::VenueUpgradeFlagged {
        venue: GUARD_RAYDIUM_V4,
        deploy_slot: v4_slot + 777
    }));
    let g = h.with_env(|env| env.venue_guard());
    assert!(g.is_flagged(GUARD_RAYDIUM_V4));
    assert_eq!(g.flagged_slot[usize::from(GUARD_RAYDIUM_V4)], v4_slot + 777);
    assert_eq!(h.keeper.metrics.venue_flags, 1);
    // Opens on both Raydium venues are refused (one mask); exits are not.
    for (rm, q) in &ms {
        let mut b = h.with_env(|env| env.borrower());
        let res = h.with_env(|env| env.open(rm, &mut b, *q).0);
        assert_eq!(
            custom_code(&res),
            Some(u32::from(ShortwinError::VenueUpgraded)),
            "{:?}",
            rm.fixture
        );
    }
    // Nothing more while the slot stands: no second flag, no second alert.
    let sent = h.chain().sent().len();
    for _ in 0..3 {
        let r = h.slot().clone();
        assert!(r.flagged.is_empty() && venue_alerts(&r).is_empty(), "{r:?}");
    }
    assert_eq!(h.chain().sent().len(), sent, "no transaction");

    // A second upgrade during review refreshes the flag.
    h.with_env(|env| fork::set_deploy_slot(&mut env.svm, RaydiumProgram::AmmV4, v4_slot + 999));
    let r = h.slot().clone();
    assert_eq!(r.flagged, vec![(GUARD_RAYDIUM_V4, v4_slot + 999)]);
    assert!(venue_alerts(&r).contains(&Alert::VenueUpgraded {
        venue: GUARD_RAYDIUM_V4,
        program: shortwin::constants::RAYDIUM_V4_PROGRAM_ID,
        known_slot: v4_slot + 777,
        deploy_slot: v4_slot + 999,
    }));
    let g = h.with_env(|env| env.venue_guard());
    assert_eq!(g.flagged_slot[usize::from(GUARD_RAYDIUM_V4)], v4_slot + 999);

    // The Emergency multisig revalidates the reviewed slot: nothing to flag afterwards.
    h.with_env(|env| {
        let ea = env.pause_authority.insecure_clone();
        ok(env.send(
            &[ix::revalidate_venue(
                &ea.pubkey(),
                GUARD_RAYDIUM_V4,
                v4_slot + 999,
            )],
            &[&ea],
        ))
    });
    let r = h.slot().clone();
    assert!(r.flagged.is_empty() && venue_alerts(&r).is_empty(), "{r:?}");
    assert!(
        !h.with_env(|env| env.venue_guard())
            .is_flagged(GUARD_RAYDIUM_V4)
    );
    let (rm, q) = &ms[1];
    let _ = ray_open(&h, rm, *q);

    // A CPMM upgrade someone else flags first: the keeper sees the latch already set.
    h.with_env(|env| {
        fork::set_deploy_slot(&mut env.svm, RaydiumProgram::Cpmm, cpmm_slot + 5);
        ok(env.send(&[ix::flag_venue_upgraded(GUARD_RAYDIUM_CPMM)], &[]));
    });
    let r = h.slot().clone();
    assert!(r.flagged.is_empty(), "{r:?}");
    // The latch already holds this slot: no upgrade to report, no attempt, no failure.
    assert!(venue_alerts(&r).is_empty(), "{:?}", r.alerts);
    assert_eq!(h.keeper.metrics.venue_flags, 2);

    // Token-2022 upgraded: an alert, nothing sent for it.
    h.with_env(|env| fixtures::set_program_data(&mut env.svm, &t22_pd, 400_000_001, None, 64));
    let r = h.slot().clone();
    assert!(
        venue_alerts(&r).contains(&Alert::ProgramUpgraded {
            program: TOKEN_2022_PROGRAM_ID,
            previous_slot: 400_000_000,
            deploy_slot: 400_000_001,
        }),
        "{:?}",
        r.alerts
    );
    assert!(r.flagged.is_empty());
}

/// `SetComputeUnitLimit(1.4 M)`, then `ix`: once a fingerprinted venue's ProgramData grew, the
/// latch instructions hash its trimmed ELF (AIR-01; half a CU per byte).
fn with_hash_budget(ix: Instruction) -> Vec<Instruction> {
    vec![shortwin_litesvm_tests::pump::cu_limit_ix(1_400_000), ix]
}

/// The sha256 of `program`'s trimmed ELF as its ProgramData holds it now: what
/// `solana-verify get-program-hash` prints and the Emergency names in `revalidate_venue`.
fn reviewed_hash(env: &Env, program: RaydiumProgram) -> [u8; 32] {
    let a = read::account(&env.svm, &program.program_data().address).expect("ProgramData");
    shortwin::instructions::flag_venue_upgraded::trimmed_elf_hash_of(&a.owner.to_bytes(), &a.data)
        .expect("a ProgramData")
        .expect("small enough to hash")
}

/// A stranger's `ExtendProgram` of `program`'s ProgramData by 10 KiB (the loader's own
/// permissionless instruction, `fork::extend_program_ix`): zeros appended, the ELF kept, the
/// current slot in the header. Returns that slot.
pub fn stranger_extends(env: &mut Env, program: &Pubkey) -> u64 {
    let key = monitor::program_data_address(program);
    let before = read::account(&env.svm, &key).unwrap().data;
    let slot = env.clock().slot;
    let stranger = env.funded_keypair();
    let ix = fork::extend_program_ix(program, &stranger.pubkey(), 10_240);
    ok(env.send(&[ix], &[&stranger]));
    let after = read::account(&env.svm, &key).unwrap().data;
    assert_eq!(after.len(), before.len() + 10_240);
    assert_eq!(
        &after[program_data::HEADER_LEN..before.len()],
        &before[program_data::HEADER_LEN..]
    );
    slot
}

/// What the loader's `Upgrade` of `program` to `elf` leaves in its ProgramData (T-LATCH's
/// `fork::set_deploy_slot` with the new ELF): the current slot in the header, `elf` after it, the
/// rest of the account zeroed, its length kept. Returns that slot.
pub fn upgrade_in_place(env: &mut Env, program: &Pubkey, elf: &[u8]) -> u64 {
    let key = monitor::program_data_address(program);
    let slot = env.clock().slot;
    let mut acc = env.svm.get_account(&key).unwrap();
    let start = program_data::HEADER_LEN;
    acc.data[4..12].copy_from_slice(&slot.to_le_bytes());
    acc.data[start..].fill(0);
    acc.data[start..start + elf.len()].copy_from_slice(elf);
    env.svm.set_account(key, acc).unwrap();
    slot
}

/// Whether backend call `c` reads ProgramData `key` whole: a `getMultipleAccounts` naming it
/// without the `VenueGuard` (the latch's header read names both; the keeper reads the accounts
/// it inspects after its exits in one call).
fn reads_whole(c: &shortwin_keeper_litesvm::Rpc, key: &Pubkey) -> bool {
    let guard = shortwin_keeper::pda::venue_guard();
    matches!(c, shortwin_keeper_litesvm::Rpc::Accounts(k) if k.contains(key) && !k.contains(&guard))
}

/// The whole-account reads of ProgramData `key` the keeper made so far ([`reads_whole`]).
pub fn whole_reads(h: &Harness, key: Pubkey) -> usize {
    h.chain()
        .rpc_log()
        .iter()
        .filter(|c| reads_whole(c, &key))
        .count()
}

/// The latch transactions (`flag_venue_upgraded`: they write the `VenueGuard`) the keeper sent.
pub fn flags_sent(h: &Harness) -> usize {
    let guard = shortwin_keeper::pda::venue_guard();
    h.chain()
        .sent()
        .iter()
        .filter(|t| t.writable.contains(&guard))
        .count()
}

/// AMM v4's dumped ELF with one byte of a panic-location string in `.rodata` changed
/// (`program/src/math.rs` → `…/meth.rs`, as `latch_raydium_it`): another build of the same size
/// the loader would deploy.
pub fn v4_elf_one_byte_changed() -> Vec<u8> {
    let mut elf = RaydiumProgram::AmmV4.elf().to_vec();
    let at = elf
        .windows(19)
        .position(|w| w == b"program/src/math.rs")
        .expect("the string")
        + 13;
    assert_eq!(elf[at], b'a');
    elf[at] = b'e';
    elf
}

/// AIR-01 (the TODO.md keeper item on `latch_action` and a pure `ExtendProgram`). The ops
/// runbook's revalidation records AMM v4's fingerprint (its dumped deploy, naming the trimmed
/// ELF's sha256). A stranger's `ExtendProgram` then moves the deploy slot: the keeper lets the
/// flag's simulation judge it — the program's rule answers `VenueNotUpgraded`, once for that
/// slot, no whole read — and neither flags nor pages it, and Raydium opens keep landing. A second
/// extension likewise. An upgrade in the grown account (one ELF byte changed) is flagged in the
/// tick it is seen and paged once, and its deploy read whole once (after the exits) for the
/// review; a stranger's extension of that unreviewed deploy during the review — which the program
/// would still refresh — is neither refreshed nor paged; the Emergency's revalidation of the new
/// ELF, in a later slot, ends the review.
#[test]
fn air01_extensions_are_neither_flagged_nor_paged_and_an_upgrade_after_them_is() {
    let (mut h, ms) = ray_setup(&[FART], KeeperConfig::default());
    let (rm, q) = &ms[0];
    let venue = GUARD_RAYDIUM_V4;
    let v4 = RaydiumProgram::AmmV4;
    let id = v4.id();
    let pd = v4.program_data().address;
    let slot0 = FIXTURE_GUARD_SLOTS[usize::from(venue)];
    let reviewed = h.with_env(|env| reviewed_hash(env, v4));
    h.with_env(|env| {
        let ea = env.pause_authority.insecure_clone();
        let ix = ix::revalidate_venue_reviewed(&ea.pubkey(), venue, slot0, reviewed);
        ok(env.send(&[ix], &[&ea]));
    });
    assert!(
        h.with_env(|env| env.venue_guard())
            .elf_fingerprint(venue)
            .is_some()
    );
    let r = h.slot().clone();
    assert!(r.flagged.is_empty() && r.extended.is_empty() && venue_alerts(&r).is_empty());

    // A stranger extends AMM v4: seen and counted in the next slot, nothing sent, nothing paged.
    let ext = h.with_env(|env| stranger_extends(env, &id));
    let sent = h.chain().sent().len();
    let refused = h.keeper.metrics.simulation_failures;
    let r = h.slot().clone();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.extended, vec![(id, ext)]);
    assert!(
        r.flagged.is_empty() && venue_alerts(&r).is_empty(),
        "{:?}",
        r.alerts
    );
    for _ in 0..3 {
        let r = h.slot().clone();
        assert!(r.flagged.is_empty() && r.extended.is_empty() && venue_alerts(&r).is_empty());
    }
    assert_eq!(h.chain().sent().len(), sent, "nothing sent");
    assert_eq!(
        h.keeper.metrics.simulation_failures,
        refused + 1,
        "the flag simulated once for that deploy slot (VenueNotUpgraded)"
    );
    assert_eq!(whole_reads(&h, pd), 0, "no whole read");
    assert_eq!(h.keeper.metrics.venue_extensions, 1);
    // The program's own answer to the flag the keeper simulated and did not send.
    h.with_env(|env| {
        let res = env.send(&with_hash_budget(ix::flag_venue_upgraded(venue)), &[]);
        assert_eq!(
            custom_code(&res),
            Some(u32::from(ShortwinError::VenueNotUpgraded))
        );
    });
    let _ = ray_open(&h, rm, *q);
    let ext2 = h.with_env(|env| stranger_extends(env, &id));
    let r = h.slot().clone();
    assert_eq!(r.extended, vec![(id, ext2)]);
    assert!(
        r.flagged.is_empty() && venue_alerts(&r).is_empty(),
        "{:?}",
        r.alerts
    );

    // An upgrade in the grown account: flagged in the next slot, paged once; its deploy read
    // whole once, after the exits, for the review.
    h.slot();
    let up = h.with_env(|env| upgrade_in_place(env, &id, &v4_elf_one_byte_changed()));
    let r = h.slot().clone();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.flagged, vec![(venue, up)], "{:?}", r.alerts);
    assert!(venue_alerts(&r).contains(&Alert::VenueUpgraded {
        venue,
        program: shortwin::constants::RAYDIUM_V4_PROGRAM_ID,
        known_slot: slot0,
        deploy_slot: up,
    }));
    assert_eq!(whole_reads(&h, pd), 1, "the deploy flagged, recorded");
    let mut b = h.with_env(|env| env.borrower());
    let res = h.with_env(|env| env.open(rm, &mut b, *q).0);
    assert_eq!(
        custom_code(&res),
        Some(u32::from(ShortwinError::VenueUpgraded))
    );

    // During the review a stranger extends the unreviewed deploy: no refresh, no page.
    h.slot();
    let ext3 = h.with_env(|env| stranger_extends(env, &id));
    let sent = h.chain().sent().len();
    let r = h.slot().clone();
    assert_eq!(r.extended, vec![(id, ext3)]);
    assert!(
        r.flagged.is_empty() && venue_alerts(&r).is_empty(),
        "{:?}",
        r.alerts
    );
    assert_eq!(h.chain().sent().len(), sent, "no refresh");
    assert_eq!(
        whole_reads(&h, pd),
        2,
        "the moved slot read whole, after the exits"
    );
    let g = h.with_env(|env| env.venue_guard());
    assert!(g.is_flagged(venue));
    assert_eq!(g.flagged_slot[usize::from(venue)], up);

    // The Emergency revalidates the new ELF after the extension: the review ends.
    h.with_env(|env| {
        let ea = env.pause_authority.insecure_clone();
        let new = reviewed_hash(env, v4);
        assert_ne!(new, reviewed);
        let ix = ix::revalidate_venue_reviewed(&ea.pubkey(), venue, up, new);
        ok(env.send(&with_hash_budget(ix), &[&ea]));
    });
    let r = h.slot().clone();
    assert!(r.flagged.is_empty() && r.extended.is_empty() && venue_alerts(&r).is_empty());
    assert!(!h.with_env(|env| env.venue_guard()).is_flagged(venue));
    let _ = ray_open(&h, rm, *q);
    assert_eq!(h.keeper.metrics.venue_extensions, 3);
    assert_eq!(h.keeper.metrics.venue_flags, 1);
}

/// AIR-01, a keeper restarted during a review: AMM v4 is upgraded (no fingerprint: the header
/// decides) and flagged; the keeper restarts with no record of that deploy. Its first tick finds
/// the venue flagged at the deploy slot the account holds, reads it whole once and records it —
/// nothing sent, nothing paged. A stranger's extension of the deploy under review is then neither
/// refreshed nor paged (before the fix the restarted keeper refreshed and paged it as a new
/// upgrade); a second upgrade during the review (the old ELF back) is.
#[test]
fn air01_a_keeper_restarted_during_a_review_does_not_page_an_extension_of_its_deploy() {
    let (mut h, _ms) = ray_setup(&[FART], KeeperConfig::default());
    let venue = GUARD_RAYDIUM_V4;
    let id = RaydiumProgram::AmmV4.id();
    let pd = RaydiumProgram::AmmV4.program_data().address;
    h.slot();
    let up = h.with_env(|env| upgrade_in_place(env, &id, &v4_elf_one_byte_changed()));
    let r = h.slot().clone();
    assert_eq!(r.flagged, vec![(venue, up)], "{:?}", r.alerts);

    let cfg = h.keeper.config().clone();
    h.restart(cfg);
    let r = h.slot().clone();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert!(
        r.flagged.is_empty() && r.extended.is_empty() && venue_alerts(&r).is_empty(),
        "{:?}",
        r.alerts
    );
    assert_eq!(whole_reads(&h, pd), 1, "the deploy under review, read once");
    assert_eq!(flags_sent(&h), 0, "nothing flagged");

    h.slot();
    let ext = h.with_env(|env| stranger_extends(env, &id));
    let r = h.slot().clone();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.extended, vec![(id, ext)]);
    assert!(
        r.flagged.is_empty() && venue_alerts(&r).is_empty(),
        "{:?}",
        r.alerts
    );
    assert_eq!(flags_sent(&h), 0, "no refresh");
    let g = h.with_env(|env| env.venue_guard());
    assert!(g.is_flagged(venue));
    assert_eq!(g.flagged_slot[usize::from(venue)], up);

    h.slot();
    let up2 = h.with_env(|env| upgrade_in_place(env, &id, RaydiumProgram::AmmV4.elf()));
    let r = h.slot().clone();
    assert_eq!(r.flagged, vec![(venue, up2)], "{:?}", r.alerts);
    assert!(venue_alerts(&r).contains(&Alert::VenueUpgraded {
        venue,
        program: id,
        known_slot: up,
        deploy_slot: up2,
    }));
}

/// Revalidates AMM v4 at its fixture deploy with its trimmed ELF's hash (the ops runbook): the
/// latch then holds its fingerprint. Returns that deploy slot.
pub fn revalidate_v4_with_hash(h: &Harness) -> u64 {
    revalidate_with_hash(h, RaydiumProgram::AmmV4, GUARD_RAYDIUM_V4)
}

/// Revalidates Raydium `program` (latch venue `venue`) at its fixture deploy with its trimmed
/// ELF's hash: the latch then holds its fingerprint. Returns that deploy slot.
fn revalidate_with_hash(h: &Harness, program: RaydiumProgram, venue: u8) -> u64 {
    let slot0 = FIXTURE_GUARD_SLOTS[usize::from(venue)];
    h.with_env(|env| {
        let reviewed = reviewed_hash(env, program);
        let ea = env.pause_authority.insecure_clone();
        let ix = ix::revalidate_venue_reviewed(&ea.pubkey(), venue, slot0, reviewed);
        ok(env.send(&[ix], &[&ea]));
    });
    assert!(
        h.with_env(|env| env.venue_guard())
            .elf_fingerprint(venue)
            .is_some()
    );
    slot0
}

/// AIR-01 keeps the latch first (ARCH §7.1 duty 10: the flag in the slot the upgrade is seen,
/// at the exit priority ceiling): AMM v4 revalidated with its fingerprint and a position open on
/// its market; an upgrade lands before the keeper's tick. The flag's simulation judges the moved
/// slot and the flag is the tick's first transaction, before its crank; the whole read of the
/// ProgramData — the record of the flagged deploy for its review — comes after them. As first
/// built the keeper read the account and flagged only after the tick's exits and cranks.
#[test]
fn air01_an_upgrade_is_flagged_before_the_ticks_exits_and_cranks() {
    let (mut h, ms) = ray_setup(&[FART], KeeperConfig::default());
    let (rm, q) = &ms[0];
    let venue = GUARD_RAYDIUM_V4;
    let id = RaydiumProgram::AmmV4.id();
    let pd = RaydiumProgram::AmmV4.program_data().address;
    let slot0 = revalidate_v4_with_hash(&h);
    let _ = ray_open(&h, rm, *q);
    let up = h.with_env(|env| upgrade_in_place(env, &id, &v4_elf_one_byte_changed()));
    let (rpc0, sent0) = (h.chain().rpc_log().len(), h.chain().sent().len());
    let r = h.second().clone();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.flagged, vec![(venue, up)], "{:?}", r.alerts);
    assert!(venue_alerts(&r).contains(&Alert::VenueUpgraded {
        venue,
        program: id,
        known_slot: slot0,
        deploy_slot: up,
    }));
    assert!(r.cranked.contains(&rm.market()), "{:?}", r.crank_failed);
    let guard = shortwin_keeper::pda::venue_guard();
    let sent = &h.chain().sent()[sent0..];
    assert!(sent.len() >= 2, "{sent:?}");
    assert!(
        sent[0].writable.contains(&guard),
        "the flag first: {sent:?}"
    );
    assert!(sent[1..].iter().all(|t| !t.writable.contains(&guard)));
    let log = &h.chain().rpc_log()[rpc0..];
    let sends: Vec<usize> = log
        .iter()
        .enumerate()
        .filter(|(_, c)| matches!(c, shortwin_keeper_litesvm::Rpc::Send { .. }))
        .map(|(i, _)| i)
        .collect();
    let whole = log
        .iter()
        .position(|c| reads_whole(c, &pd))
        .expect("the record's whole read");
    assert!(sends.len() >= 2 && sends[1] < whole, "{log:?}");
    let mut b = h.with_env(|env| env.borrower());
    let res = h.with_env(|env| env.open(rm, &mut b, *q).0);
    assert_eq!(
        custom_code(&res),
        Some(u32::from(ShortwinError::VenueUpgraded))
    );
}

/// AIR-01, when the flag's simulation cannot judge (a node that could not simulate), the keeper
/// judges by the program's rule on the account read whole after its exits. AMM v4 revalidated
/// with its fingerprint. A stranger extends it and the simulation fails: the whole account shows
/// an extension — counted, nothing paged. A second extension whose simulation and whole read both
/// fail counts as an upgrade (fail-safe): paged once with the flag's failure (`VenueFlagFailed`);
/// the next second the flag's simulation answers `VenueNotUpgraded` — counted as an extension,
/// nothing flagged, nothing more paged. An upgrade whose simulation fails is paged by the whole
/// read with the flag's failure, and flagged the next second.
#[test]
fn air01_a_slot_the_flags_simulation_cannot_judge_is_judged_on_the_whole_account() {
    let (mut h, _ms) = ray_setup(&[FART], KeeperConfig::default());
    let venue = GUARD_RAYDIUM_V4;
    let id = RaydiumProgram::AmmV4.id();
    let pd = RaydiumProgram::AmmV4.program_data().address;
    let slot0 = revalidate_v4_with_hash(&h);
    let flag_failed = |r: &TickReport| {
        r.alerts
            .iter()
            .any(|a| matches!(a, Alert::VenueFlagFailed { venue: v, .. } if *v == venue))
    };
    h.slot();

    // The simulation fails: the whole account shows an extension.
    let ext = h.with_env(|env| stranger_extends(env, &id));
    h.chain().fail_simulations(1);
    let r = h.slot().clone();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.extended, vec![(id, ext)]);
    assert!(
        r.flagged.is_empty() && venue_alerts(&r).is_empty(),
        "{:?}",
        r.alerts
    );
    assert_eq!(whole_reads(&h, pd), 1);

    // The simulation and the whole read fail: paged (fail-safe), with the flag's failure.
    h.slot();
    let ext2 = h.with_env(|env| stranger_extends(env, &id));
    h.chain().fail_simulations(1);
    h.chain().fail_reads_of(pd, 1);
    let r = h.slot().clone();
    assert!(
        r.errors.iter().any(|e| e.contains("(whole)")),
        "{:?}",
        r.errors
    );
    assert!(r.flagged.is_empty() && r.extended.is_empty());
    let a = venue_alerts(&r);
    assert!(
        a.contains(&Alert::VenueUpgraded {
            venue,
            program: id,
            known_slot: slot0,
            deploy_slot: ext2,
        }),
        "{a:?}"
    );
    assert!(flag_failed(&r), "{a:?}");
    // The next second the program judges it: an extension.
    let r = h.second().clone();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.extended, vec![(id, ext2)]);
    assert!(
        r.flagged.is_empty() && venue_alerts(&r).is_empty(),
        "{:?}",
        r.alerts
    );
    assert_eq!(flags_sent(&h), 0, "nothing flagged");

    // An upgrade whose simulation fails: paged on the whole account, flagged the next second.
    h.slot();
    let up = h.with_env(|env| upgrade_in_place(env, &id, &v4_elf_one_byte_changed()));
    h.chain().fail_simulations(1);
    let r = h.slot().clone();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert!(r.flagged.is_empty() && r.extended.is_empty());
    assert!(
        venue_alerts(&r).contains(&Alert::VenueUpgraded {
            venue,
            program: id,
            known_slot: slot0,
            deploy_slot: up,
        }),
        "{:?}",
        r.alerts
    );
    assert!(flag_failed(&r), "{:?}", r.alerts);
    let r = h.second().clone();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.flagged, vec![(venue, up)], "{:?}", r.alerts);
    assert!(
        !venue_alerts(&r)
            .iter()
            .any(|a| matches!(a, Alert::VenueUpgraded { .. })),
        "paged once: {:?}",
        r.alerts
    );
    assert!(h.with_env(|env| env.venue_guard()).is_flagged(venue));
    assert_eq!(h.keeper.metrics.venue_extensions, 2);
}

/// AIR-01 through an RPC pool whose simulations come from a node behind its header reads: the
/// flag's simulation judges the deploy the header showed only on a bank that holds it. AMM v4
/// revalidated with its fingerprint; a node of the pool stays at a slot
/// (`LiteSvmChain::lag_simulations`) and AMM v4 is upgraded in the next one. The keeper's header
/// read shows the upgrade; the flag's simulation, served by the node behind, answers
/// `VenueNotUpgraded` — the validated deploy still stands there — on a context slot before the
/// deploy. That answer is not booked as an extension: the account read whole after the exits
/// judges the slot — an upgrade, paged once, with the flag's failure — and the next slot's
/// simulation, on the chain as it is, flags it; opens are then refused. As first built the
/// keeper booked the stale answer as an extension, skipped that deploy slot from then on, and
/// neither flagged nor paged the upgrade while opens on it passed the latch.
#[test]
fn air01_an_upgrade_a_lagging_node_simulates_is_still_flagged_and_paged() {
    let (mut h, ms) = ray_setup(&[FART], KeeperConfig::default());
    let (rm, q) = &ms[0];
    let venue = GUARD_RAYDIUM_V4;
    let id = RaydiumProgram::AmmV4.id();
    let pd = RaydiumProgram::AmmV4.program_data().address;
    let slot0 = revalidate_v4_with_hash(&h);
    h.slot();
    // A node of the pool stays at this slot; the upgrade lands in the next one.
    h.chain().lag_simulations(1);
    let up = h.with_env(|env| {
        let c = env.clock();
        env.set_clock(c.unix_timestamp, c.slot + 1);
        upgrade_in_place(env, &id, &v4_elf_one_byte_changed())
    });
    let r = h.slot().clone();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert!(
        r.flagged.is_empty() && r.extended.is_empty(),
        "not booked as an extension: {r:?}"
    );
    let a = venue_alerts(&r);
    assert!(
        a.contains(&Alert::VenueUpgraded {
            venue,
            program: id,
            known_slot: slot0,
            deploy_slot: up,
        }),
        "{a:?}"
    );
    assert!(
        a.iter().any(|x| matches!(
            x,
            Alert::VenueFlagFailed { venue: v, detail }
                if *v == venue && detail.contains("before deploy slot")
        )),
        "{a:?}"
    );
    assert_eq!(whole_reads(&h, pd), 1, "judged on the whole account");
    assert_eq!(h.keeper.metrics.venue_extensions, 0);

    // The next slot's simulation holds the deploy: flagged, not paged again.
    let r = h.slot().clone();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.flagged, vec![(venue, up)], "{:?}", r.alerts);
    assert!(
        !venue_alerts(&r)
            .iter()
            .any(|a| matches!(a, Alert::VenueUpgraded { .. })),
        "paged once: {:?}",
        r.alerts
    );
    assert_eq!(h.keeper.metrics.venue_extensions, 0);
    let mut b = h.with_env(|env| env.borrower());
    let res = h.with_env(|env| env.open(rm, &mut b, *q).0);
    assert_eq!(
        custom_code(&res),
        Some(u32::from(ShortwinError::VenueUpgraded))
    );
}

/// AIR-01's cost before the exits, when a stranger extends several fingerprinted venues in one
/// slot (four at most: pump-fees, AMM v4, CPMM, Jupiter): the flags of all the tick's moved slots
/// the header cannot judge are simulated together — one priority-fee read and one blockhash,
/// then the simulations (in parallel on a JSON-RPC backend). AMM v4 and CPMM revalidated with
/// their fingerprints; a stranger extends both in one slot. The keeper's next tick books both
/// extensions with one priority-fee read, one blockhash read and two simulations, and sends,
/// flags and pages nothing. As first built it simulated one venue after the other, each with
/// its own priority-fee read and blockhash: three calls per venue in a row.
#[test]
fn air01_venues_extended_in_one_slot_are_simulated_together() {
    let (mut h, _ms) = ray_setup(&[FART], KeeperConfig::default());
    let v4 = RaydiumProgram::AmmV4.id();
    let cpmm = RaydiumProgram::Cpmm.id();
    revalidate_v4_with_hash(&h);
    revalidate_with_hash(&h, RaydiumProgram::Cpmm, GUARD_RAYDIUM_CPMM);
    h.slot();
    // A tick with nothing moved calls none of them.
    let quiet = h.chain().other_calls();
    let r = h.slot().clone();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(
        h.chain().other_calls().since(quiet),
        shortwin_keeper_litesvm::OtherCalls::default(),
        "{r:?}"
    );

    let (e1, e2) = h.with_env(|env| (stranger_extends(env, &v4), stranger_extends(env, &cpmm)));
    assert_eq!(e1, e2, "one slot");
    let (calls, sent) = (h.chain().other_calls(), h.chain().sent().len());
    let r = h.slot().clone();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    let mut extended = r.extended.clone();
    extended.sort();
    let mut want = vec![(v4, e1), (cpmm, e1)];
    want.sort();
    assert_eq!(extended, want);
    assert!(
        r.flagged.is_empty() && venue_alerts(&r).is_empty(),
        "{:?}",
        r.alerts
    );
    assert_eq!(h.chain().sent().len(), sent, "nothing sent");
    assert_eq!(
        h.chain().other_calls().since(calls),
        shortwin_keeper_litesvm::OtherCalls {
            blockhashes: 1,
            priority_fees: 1,
            simulations: 2,
        }
    );
    assert_eq!(h.keeper.metrics.venue_extensions, 2);
}

/// AIR-01 for Token-2022 (alert only; the mainnet program, extendable like any upgradeable
/// program): the keeper's first tick reads its ProgramData whole once, the deploy its changes are
/// judged against. A stranger's `ExtendProgram` of it is not paged — counted as an extension;
/// before the fix every extension paged `ProgramUpgraded` — and an upgrade (one ELF byte changed,
/// in the grown account) is alerted once.
#[test]
fn air01_token_2022_is_paged_for_an_upgrade_not_for_an_extension() {
    let (mut h, _ms) = ray_setup(&[FART], KeeperConfig::default());
    let t22 = TOKEN_2022_PROGRAM_ID;
    let t22_pd = monitor::program_data_address(&t22);
    let r = h.slot().clone();
    assert!(r.errors.is_empty() && venue_alerts(&r).is_empty(), "{r:?}");
    assert_eq!(whole_reads(&h, t22_pd), 1, "the first read");

    h.slot();
    let ext = h.with_env(|env| stranger_extends(env, &t22));
    let r = h.slot().clone();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert_eq!(r.extended, vec![(t22, ext)]);
    assert!(venue_alerts(&r).is_empty(), "{:?}", r.alerts);
    for _ in 0..2 {
        let r = h.slot().clone();
        assert!(r.extended.is_empty() && venue_alerts(&r).is_empty());
    }
    assert_eq!(whole_reads(&h, t22_pd), 2, "once per deploy slot");

    h.slot();
    let up = h.with_env(|env| {
        let data = read::account(&env.svm, &t22_pd).unwrap().data;
        let elf = &data[program_data::HEADER_LEN..];
        let end = elf.iter().rposition(|&b| b != 0).expect("an ELF") + 1;
        let mut changed = elf[..end].to_vec();
        changed[end / 2] ^= 0x01;
        upgrade_in_place(env, &t22, &changed)
    });
    let r = h.slot().clone();
    assert!(r.errors.is_empty(), "{:?}", r.errors);
    assert!(r.extended.is_empty());
    assert_eq!(
        venue_alerts(&r),
        vec![Alert::ProgramUpgraded {
            program: t22,
            previous_slot: ext,
            deploy_slot: up,
        }]
    );
    let r = h.slot().clone();
    assert!(venue_alerts(&r).is_empty(), "once: {:?}", r.alerts);
    assert_eq!(h.keeper.metrics.venue_extensions, 1);
}

#[test]
fn with_the_watch_off_nothing_is_flagged_or_reported() {
    let mut cfg = KeeperConfig::default();
    cfg.venue_watch.flag_upgrades = false;
    cfg.venue_watch.pool_admin = false;
    let (mut h, ms) = ray_setup(&[FART], cfg);
    let (rm, q) = &ms[0];
    let _ = ray_open(&h, rm, *q);
    h.second();
    h.with_env(|env| {
        fork::set_deploy_slot(&mut env.svm, RaydiumProgram::AmmV4, 1);
        ok(fork::raydium_admin_send(
            env,
            fork::v4_set_params_ix(rm.v4(), 0, 2),
        ));
    });
    let r = h.second().clone();
    assert!(r.flagged.is_empty() && venue_alerts(&r).is_empty(), "{r:?}");
    assert!(
        !h.with_env(|env| env.venue_guard())
            .is_flagged(GUARD_RAYDIUM_V4)
    );
    // The duties still see the halt through the program's own route predicate.
    assert!(r.needs[&rm.market()].no_route);
}
