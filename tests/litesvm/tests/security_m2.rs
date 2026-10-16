//! M2 security review regressions (`M2-SEC-n`): each finding's reproducer from the audit,
//! rewritten to assert the fixed behaviour, plus the review's check that the release build
//! cannot reach the mock venue (A49).
//!
//! - M2-SEC-1: the settlement box collapsed to the EMA point after `RING_LEN` seconds without
//!   an integration, also on an executable venue, where a lender instruction's roll (A16)
//!   integrates a pushed bucket without recording. Fixed: only a venue that cannot execute
//!   collapses the box (`shortwin_math::settle::box_of(.., executable)`).
//! - M2-SEC-2: `previous` survived one `set_venue_extras` write. Fixed: a `stable` set per
//!   instruction kind — the newest set that stood `EXTRAS_STABLE_S` as `current`, or
//!   Governance's last publication — which quick writes cannot displace.
//! - M2-SEC-3: extras templates accepted any off-curve bump. Fixed: the canonical bump only.
//! - M2-SEC-4: after a full wipe no instruction set `wipe_trader_swept`, so no deposit could
//!   ever land. Fixed: `sweep_trader` (ARCH §3.9) is built in M2.
//! - M2-SEC-5 (latent, deferred to M5 by the finding itself): `sol_settle` had no path for a
//!   `None` / `PumpMigrating` market. Fixed in M5: their ReadSets (the detection set, the
//!   curve's) are accepted and S3 settles after the chain; M2's tripwire is inverted here.
//!
//! Every instruction goes through `Env::send`, so the invariant hooks run after each one,
//! except in the M2-SEC-5 test, which plants a state the mock venue cannot reach (said so there).

#![allow(clippy::result_large_err, dead_code)]

#[path = "settle_support.rs"]
mod support;

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::AccountMeta;
use shortwin::errors::ShortwinError as E;
use shortwin_litesvm_tests::ix::{self, LenderKeys};
use solana_signer::Signer;
use support::*;

/// Extra borrowers opened in the same second as the scenario's own position (same deadline).
struct Book {
    others: Vec<(Borrower, u64)>,
}

/// Every open position of the scenario: `(borrower key, nonce)`.
fn positions(sc: &Sc, book: &Book) -> Vec<(Pubkey, u64)> {
    let mut v = vec![(sc.b.key(), sc.nonce)];
    v.extend(book.others.iter().map(|(b, n)| (b.key(), *n)));
    v
}

/// `sol_settle` of every position of `book` in the current second, one transaction each, by
/// the scenario's keeper. Returns the settlements that executed.
fn settle_all(sc: &mut Sc, book: &Book) -> Vec<shortwin::events::SolSettle> {
    let mut out = Vec::new();
    for (b, n) in positions(sc, book) {
        if read::borsh::<shortwin::state::ShortPosition>(
            &sc.env.svm,
            &pda::position(&sc.mm.market(), &b, n).0,
        )
        .is_none()
        {
            continue;
        }
        let ix = shortwin_litesvm_tests::engine::sol_settle_ix(
            &sc.env,
            &sc.mm,
            &sc.keeper.pubkey(),
            &b,
            n,
        );
        let kp = sc.keeper.insecure_clone();
        if let Some(ev) = Outcome::of(&ok(sc.env.send(&[ix], &[&kp]))).settled {
            out.push(ev);
        }
    }
    out
}

fn sum(
    evs: &[shortwin::events::SolSettle],
    f: impl Fn(&shortwin::events::SolSettle) -> u64,
) -> i128 {
    evs.iter().map(|e| i128::from(f(e))).sum()
}

// ---- M2-SEC-1: the settlement box on an executable venue ----------------------------------

mod box_collapse {
    use super::*;
    use shortwin_math::settle::box_of;

    /// Expired, healthy positions (the scenario's own plus up to `extra` more, opened in the
    /// same second by other wallets) on a market whose route the Emergency multisig flagged
    /// 700 s before the deadline (the O6 "venue interface break" response), with the in-house
    /// keeper's documented cadence on a no-route market (duty 5: every 30 s). Returns the
    /// scenario at `deadline − 10`, just after a keeper crank and `sol_settle` (both no-ops).
    fn flagged_route_near_deadline(extra: usize) -> (Sc, Book) {
        let mut sc = Sc::default_market();
        let mut others = Vec::new();
        for _ in 0..extra {
            let mut b = sc.env.borrower();
            let (res, n) = sc.env.open(&sc.mm, &mut b, Q);
            if res.is_err() {
                break;
            }
            others.push((b, n));
        }
        let book = Book { others };
        sc.env.tick(&sc.mm, 5);
        sc.warp_to_deadline(-700);
        sc.set_route_broken(true);
        for _ in 0..23 {
            sc.env.warp(30);
            sc.crank();
            assert!(settle_all(&mut sc, &book).is_empty());
        }
        assert_eq!(sc.env.now(), sc.pos().deadline - 10);
        (sc, book)
    }

    /// A dust deposit by the whale: `deposit` runs the prologue's roll (ARCH §3.0 A16) but
    /// records no observation, so it integrates the pending bucket without opening a new one.
    fn roll_only(sc: &mut Sc) {
        let k = LenderKeys {
            owner: sc.whale.key(),
            mint: sc.mm.mint(),
            token_program: sc.mm.keys.token_program,
            owner_token_account: sc.whale.base,
        };
        let kp = sc.whale.kp.insecure_clone();
        ok(sc.env.send(&[ix::deposit(&k, 1_000_000, 0)], &[&kp]));
    }

    /// The keeper observes and calls `sol_settle` for every position every second until all
    /// settled (at most `max_s` seconds).
    fn keeper_until_settled(
        sc: &mut Sc,
        book: &Book,
        max_s: u32,
    ) -> Vec<shortwin::events::SolSettle> {
        let n = positions(sc, book).len();
        let mut all = Vec::new();
        for _ in 0..max_s {
            sc.env.warp(1);
            sc.crank();
            all.extend(settle_all(sc, book));
            if all.len() == n {
                return all;
            }
        }
        panic!("S3 incomplete after {max_s} s: {} of {n}", all.len());
    }

    /// The attack of the audit: one attacker-only pushed second at `deadline − 9`, a dust
    /// deposit at `deadline − 7` that integrates it (dt = 2), silence until `deadline + 2`.
    /// Returns the push's cost.
    fn push_roll_and_wait(sc: &mut Sc, p0: u128, mult: f64) -> i128 {
        sc.env.warp(1);
        let cost = sc.attacker_bundle(scale(p0, mult));
        sc.env.warp(2);
        roll_only(sc);
        sc.env.warp(9);
        cost
    }

    /// M2-SEC-1. On the route-flagged (executable) venue the attacker's pushed bucket, rolled
    /// by a dust deposit and left alone for 9 s, no longer decides the S3 amounts: the box
    /// keeps the ring's honest medians (the old rule would have collapsed it onto the pushed
    /// EMA — asserted on the same state), every `sol_settle` at `deadline + 2` waits, and the
    /// keeper's per-second S3 afterwards pays within 2 % of the honest run (before the fix:
    /// +28.2 % to lenders for a push up, +7.2 % of the lenders' take to borrowers for a push
    /// down).
    #[test]
    fn m2_sec_1_a_rolled_push_does_not_decide_s3_amounts() {
        let (base, book) = flagged_route_near_deadline(7);
        let n = positions(&base, &book).len();
        let saved = base.env.svm.clone();
        let mut sc = base;
        let p0 = sc.spot();

        let honest = keeper_until_settled(&mut sc, &book, 30);
        let honest_l = sum(&honest, |e| e.lender_amount);
        let honest_b = sum(&honest, |e| e.borrower_amount);
        assert_eq!(honest.len(), n);

        for (mult, name) in [(2.0, "up"), (0.5, "down")] {
            sc.env.svm = saved.clone();
            let cost = push_roll_and_wait(&mut sc, p0, mult);
            let evs = settle_all(&mut sc, &book);
            assert!(
                evs.is_empty(),
                "{name}: the pushed EMA alone settles nothing ({} settled)",
                evs.len()
            );
            // The state every one of those calls evaluated on: stale by more than RING_LEN.
            let m = sc.market();
            let o = m.observation();
            let now = sc.env.now();
            assert!(
                now - o.ema.ts > 8,
                "{name}: the EMA is {} s old",
                now - o.ema.ts
            );
            let old_rule = box_of(&o.ema, &o.ring, now, false).unwrap();
            assert!(
                old_rule.is_point(),
                "{name}: the pre-fix rule collapsed here"
            );
            let bx = box_of(&o.ema, &o.ring, now, true).unwrap();
            assert!(
                !bx.is_point(),
                "{name}: the executable venue keeps the medians"
            );
            assert!(bx.contains(o.ema.price, o.ema.liq));

            let after = keeper_until_settled(&mut sc, &book, 60);
            for ev in &after {
                assert_eq!(ev.condition, 3);
                assert!(ev.ema_age_s <= 1, "{name}: settled on a fresh EMA");
            }
            let l = sum(&after, |e| e.lender_amount);
            let b = sum(&after, |e| e.borrower_amount);
            println!(
                "M2-SEC-1 {name} ({n} positions): honest lenders {honest_l} borrowers {honest_b}; \
                 after the attack lenders {l} ({:+.2} %) borrowers {b}; push cost {cost}",
                (l - honest_l) as f64 * 100.0 / honest_l as f64
            );
            assert!(
                (l - honest_l).abs() * 50 <= honest_l,
                "{name}: lenders {l} vs honest {honest_l}"
            );
        }
    }
}

// ---- M2-SEC-2: the `stable` extras set -----------------------------------------------------

mod extras_stable {
    use super::*;
    use shortwin::constants::EXTRAS_STABLE_S;
    use shortwin::events::VenueExtrasSet;
    use shortwin::state::{PLACEHOLDER_MINT, SEED_CONST, SEED_PLACEHOLDER};
    use shortwin::venues::{EXTRAS_SET_CURRENT, EXTRAS_SET_PREVIOUS, EXTRAS_SET_STABLE, ExtrasSel};
    use shortwin::{ExtraEntryArg, SeedPartArg};
    use shortwin_litesvm_tests::engine::{VENUE_HINT_MOCK, mock_extras_key, set_venue_extras_ix};
    use solana_keypair::Keypair;

    const BUY: u8 = 1;
    const MOCK_EXTRAS_MISMATCH: u32 = 6_011;

    /// A venue that starts requiring a per-mint PDA `["extra", mint]` of some program: the
    /// account, its canonical bump and the template that derives it.
    struct PerMint {
        extra: Pubkey,
        bump: u8,
        good: Vec<ExtraEntryArg>,
    }

    fn per_mint(sc: &Sc) -> PerMint {
        let prog = Pubkey::new_unique();
        let (extra, bump) = Pubkey::find_program_address(&[b"extra", sc.mm.mint().as_ref()], &prog);
        let mut c = [0u8; 32];
        c[..5].copy_from_slice(b"extra");
        let good = vec![ExtraEntryArg {
            kind: 2,
            writable: false,
            key: prog,
            seeds: vec![
                SeedPartArg {
                    kind: SEED_CONST,
                    len: 5,
                    bytes: c,
                },
                SeedPartArg {
                    kind: SEED_PLACEHOLDER,
                    len: PLACEHOLDER_MINT,
                    bytes: [0; 32],
                },
            ],
        }];
        PerMint { extra, bump, good }
    }

    fn junk() -> Vec<ExtraEntryArg> {
        vec![ExtraEntryArg {
            kind: 1,
            writable: false,
            key: Pubkey::new_unique(),
            seeds: vec![],
        }]
    }

    /// `set_venue_extras(mock, buy, entries)` signed by `by`; returns its event.
    fn write(env: &mut Env, by: &Keypair, entries: Vec<ExtraEntryArg>) -> VenueExtrasSet {
        let ix = set_venue_extras_ix(&by.pubkey(), VENUE_HINT_MOCK, BUY, entries);
        let meta = ok(env.send(&[ix], &[by]));
        events::of::<VenueExtrasSet>(&meta)
            .pop()
            .expect("VenueExtrasSet")
    }

    fn emergency(env: &mut Env, entries: Vec<ExtraEntryArg>) -> VenueExtrasSet {
        let ea = env.pause_authority.insecure_clone();
        write(env, &ea, entries)
    }

    fn governance(env: &mut Env, entries: Vec<ExtraEntryArg>) -> VenueExtrasSet {
        let gov = env.governance.insecure_clone();
        write(env, &gov, entries)
    }

    /// `close_short` of `(b, nonce)` selecting `sel` with the trailing `extras`.
    fn close_raw(
        sc: &mut Sc,
        b: &Keypair,
        nonce: u64,
        sel: ExtrasSel,
        extras: &[AccountMeta],
    ) -> TxResult {
        let ix = close_short_ix(&sc.mm, &b.pubkey(), nonce, u64::MAX, sel, extras);
        sc.env.send(&[ix], &[b])
    }

    /// `close_short` of `(b, nonce)` selecting `set`, passing the `VenueExtras` account and the
    /// per-mint PDA (or nothing for `none`).
    fn close_with(sc: &mut Sc, b: &Keypair, nonce: u64, set: u8, v: &PerMint) -> TxResult {
        if set == 0 {
            return close_raw(sc, b, nonce, ExtrasSel::NONE, &[]);
        }
        let sel = ExtrasSel {
            set,
            bumps: [v.bump, 0, 0, 0],
        };
        let extras = [
            AccountMeta::new_readonly(mock_extras_key(), false),
            AccountMeta::new_readonly(v.extra, false),
        ];
        close_raw(sc, b, nonce, sel, &extras)
    }

    /// Current and previous hold junk and the venue requires the per-mint PDA: `current`,
    /// `previous` and `none` all fail.
    fn only_stable_is_left(sc: &mut Sc, b: &Keypair, nonce: u64, v: &PerMint) {
        for set in [EXTRAS_SET_CURRENT, EXTRAS_SET_PREVIOUS] {
            assert_err(&close_with(sc, b, nonce, set, v), E::BadExtras);
        }
        assert_eq!(
            custom_code(&close_with(sc, b, nonce, 0, v)),
            Some(MOCK_EXTRAS_MISMATCH)
        );
    }

    /// M2-SEC-2, the audit's case with the set published a week earlier: two quick
    /// Emergency writes (a compromised or twice-mistaken key) push the working template out of
    /// `current` and `previous`, but not out of `stable`, and the borrower closes with it.
    /// Before the fix every selection failed and only a Governance re-publication (48 h
    /// Squads time lock) unlocked the position.
    #[test]
    fn m2_sec_2_a_week_old_set_survives_two_quick_writes() {
        let mut sc = Sc::default_market();
        sc.env.tick(&sc.mm, 5);
        let v = per_mint(&sc);
        let published = emergency(&mut sc.env, v.good.clone());
        sc.env.warp(EXTRAS_STABLE_S + 1);
        sc.env.tick(&sc.mm, 5);
        let mut b2 = sc.env.borrower();
        let (res, n2) = sc.env.open(&sc.mm, &mut b2, Q);
        ok(res);
        sc.knobs(|k| k.required_extras = vec![v.extra]);

        let first = emergency(&mut sc.env, junk());
        assert_eq!(
            first.stable_version, published.version,
            "the week-old set became stable when the first junk write displaced it"
        );
        let second = emergency(&mut sc.env, junk());
        assert_eq!(
            second.stable_version, published.version,
            "quick writes keep it"
        );
        let kp = b2.kp.insecure_clone();
        only_stable_is_left(&mut sc, &kp, n2, &v);
        // Healthy and unflagged: no settlement path either, so before the fix it was stuck.
        let keeper = sc.keeper.insecure_clone();
        for _ in 0..3 {
            sc.env.warp(1);
            sc.crank();
            let ix = shortwin_litesvm_tests::engine::sol_settle_ix(
                &sc.env,
                &sc.mm,
                &keeper.pubkey(),
                &b2.key(),
                n2,
            );
            assert!(
                Outcome::of(&ok(sc.env.send(&[ix], &[&keeper])))
                    .settled
                    .is_none()
            );
        }
        ok(close_with(&mut sc, &kp, n2, EXTRAS_SET_STABLE, &v));
    }

    /// M2-SEC-2: a Governance publication (the slow, reviewed path ratifying the Emergency's
    /// template) is `stable` at once, so a young set is protected too once ratified.
    #[test]
    fn m2_sec_2_a_governance_publication_is_pinned_at_once() {
        let mut sc = Sc::default_market();
        sc.env.tick(&sc.mm, 5);
        let v = per_mint(&sc);
        emergency(&mut sc.env, v.good.clone());
        let ratified = governance(&mut sc.env, v.good.clone());
        assert_eq!(ratified.stable_version, ratified.version);
        sc.knobs(|k| k.required_extras = vec![v.extra]);
        emergency(&mut sc.env, junk());
        let last = emergency(&mut sc.env, junk());
        assert_eq!(last.stable_version, ratified.version);
        sc.env.warp(1);
        let kp = sc.b.kp.insecure_clone();
        let nonce = sc.nonce;
        only_stable_is_left(&mut sc, &kp, nonce, &v);
        ok(close_with(&mut sc, &kp, nonce, EXTRAS_SET_STABLE, &v));
    }

    /// The limits the fix documents (design notes): a set younger than `EXTRAS_STABLE_S` that
    /// Governance has not ratified keeps the one-write guarantee of `previous` only; and an
    /// attacker that keeps a set of its own `current`, unchallenged, for `EXTRAS_STABLE_S`
    /// makes it the next `stable` — the window in which Governance must re-publish or rotate
    /// the Emergency key.
    #[test]
    fn m2_sec_2_what_stable_does_not_cover() {
        let mut sc = Sc::default_market();
        sc.env.tick(&sc.mm, 5);
        let v = per_mint(&sc);
        let young = emergency(&mut sc.env, v.good.clone());
        sc.knobs(|k| k.required_extras = vec![v.extra]);
        let saved = sc.env.svm.clone();
        let kp = sc.b.kp.insecure_clone();
        let nonce = sc.nonce;

        // One write: `previous` still closes (SPEC §11's guarantee, unchanged).
        emergency(&mut sc.env, junk());
        sc.env.warp(1);
        let one = sc.env.svm.clone();
        ok(close_with(&mut sc, &kp, nonce, EXTRAS_SET_PREVIOUS, &v));

        // Two writes against the young set: `stable` holds the set before it (empty here).
        sc.env.svm = one;
        let last = emergency(&mut sc.env, junk());
        assert!(last.stable_version < young.version);
        only_stable_is_left(&mut sc, &kp, nonce, &v);
        let empty_stable = ExtrasSel {
            set: EXTRAS_SET_STABLE,
            bumps: [0; 4],
        };
        let only_the_account = [AccountMeta::new_readonly(mock_extras_key(), false)];
        assert_eq!(
            custom_code(&close_raw(
                &mut sc,
                &kp,
                nonce,
                empty_stable,
                &only_the_account
            )),
            Some(MOCK_EXTRAS_MISMATCH)
        );

        // A junk set left current for a week becomes stable at the next write.
        sc.env.svm = saved;
        emergency(&mut sc.env, junk()); // good -> previous; young, so not stable
        let aged = emergency(&mut sc.env, junk());
        sc.env.warp(EXTRAS_STABLE_S);
        let next = emergency(&mut sc.env, junk());
        assert_eq!(next.stable_version, aged.version);
    }
}

// ---- M2-SEC-3: extras templates take the canonical bump only -------------------------------

mod template_bump {
    use super::*;
    use shortwin::state::{PLACEHOLDER_MINT, SEED_CONST, SEED_PLACEHOLDER};
    use shortwin::venues::{EXTRAS_SET_CURRENT, ExtrasSel};
    use shortwin::{ExtraEntryArg, SeedPartArg};
    use shortwin_litesvm_tests::engine::{VENUE_HINT_MOCK, mock_extras_key, set_venue_extras_ix};

    const BUY: u8 = 1;

    /// M2-SEC-3. A template re-derived from the caller's bump used to accept any off-curve
    /// bump, so a caller — the keeper on `liquidate` / `expire`, whose venue CPI spends the
    /// borrower's SOL — could pass a second, non-canonical PDA of the template's program. The
    /// audit's case reached the venue CPI (the mock's own `ExtrasMismatch`); now short.win's
    /// parser refuses it (`BadExtras`) and the canonical bump still closes.
    #[test]
    fn m2_sec_3_a_template_refuses_a_non_canonical_bump() {
        let mut sc = Sc::default_market();
        sc.env.tick(&sc.mm, 5);
        // A program id for which the per-mint PDA has a second valid (non-canonical) bump.
        let (prog, canonical, other) = loop {
            let prog = Pubkey::new_unique();
            let (_, bump) = Pubkey::find_program_address(&[b"extra", sc.mm.mint().as_ref()], &prog);
            if let Some(b2) = (0..bump).rev().find(|b| {
                Pubkey::create_program_address(&[b"extra", sc.mm.mint().as_ref(), &[*b]], &prog)
                    .is_ok()
            }) {
                break (prog, bump, b2);
            }
        };
        let pda_of = |b: u8| {
            Pubkey::create_program_address(&[b"extra", sc.mm.mint().as_ref(), &[b]], &prog).unwrap()
        };
        let (canon, alt) = (pda_of(canonical), pda_of(other));
        assert_ne!(canon, alt);
        sc.knobs(|k| k.required_extras = vec![canon]);
        let mut c = [0u8; 32];
        c[..5].copy_from_slice(b"extra");
        let entry = ExtraEntryArg {
            kind: 2,
            writable: false,
            key: prog,
            seeds: vec![
                SeedPartArg {
                    kind: SEED_CONST,
                    len: 5,
                    bytes: c,
                },
                SeedPartArg {
                    kind: SEED_PLACEHOLDER,
                    len: PLACEHOLDER_MINT,
                    bytes: [0; 32],
                },
            ],
        };
        let ea = sc.env.pause_authority.insecure_clone();
        let ix = set_venue_extras_ix(&ea.pubkey(), VENUE_HINT_MOCK, BUY, vec![entry]);
        ok(sc.env.send(&[ix], &[&ea]));
        sc.env.warp(1);
        let close = |bump: u8, key: Pubkey| {
            close_short_ix(
                &sc.mm,
                &sc.b.key(),
                sc.nonce,
                u64::MAX,
                ExtrasSel {
                    set: EXTRAS_SET_CURRENT,
                    bumps: [bump, 0, 0, 0],
                },
                &[
                    AccountMeta::new_readonly(mock_extras_key(), false),
                    AccountMeta::new_readonly(key, false),
                ],
            )
        };
        let kp = sc.b.kp.insecure_clone();
        let (bad, good) = (close(other, alt), close(canonical, canon));
        assert_err(&sc.env.send(&[bad], &[&kp]), E::BadExtras);
        ok(sc.env.send(&[good], &[&kp]));
    }
}

// ---- A49: the release build cannot reach the mock venue -----------------------------------

mod release_build {
    use super::*;
    use shortwin_litesvm_tests::engine::create_mock_market_ix;

    /// The release `shortwin.so` (`$SHORTWIN_SO`) refuses the mock hint (`NoSupportedVenue`),
    /// and the config refuses the mock's venue bit and its `VenueExtras` seed.
    #[test]
    fn the_release_build_refuses_the_mock_venue() {
        let mut env = Env::initialized();
        env.load_mock_cpamm();
        let pool = mock::new_pool(&mut env, TokenProgram::Spl, 6, 2_500);
        mock::seed(&mut env, &pool, 1_000_000_000_000_000, 1_000_000_000_000);
        let creator = env.funded_keypair();
        let res = env.send(
            &[create_mock_market_ix(&creator.pubkey(), &pool.keys)],
            &[&creator],
        );
        assert_err(&res, E::NoSupportedVenue);
        let ea = env.pause_authority.insecure_clone();
        let ix = shortwin_litesvm_tests::engine::set_venue_extras_ix(&ea.pubkey(), 6, 1, vec![]);
        assert!(env.send(&[ix], &[&ea]).is_err());
        let ix = shortwin_litesvm_tests::engine::set_route_broken_ix(
            &ea.pubkey(),
            shortwin::PauseScope::Global,
            5,
            true,
            None,
        );
        assert_err(&env.send(&[ix], &[&ea]), E::InvalidParams);
    }
}

// ---- M2-SEC-4: `sweep_trader` reopens lending after a full wipe ----------------------------

mod full_wipe {
    use super::*;
    use shortwin::constants::WSOL_MINT;
    use shortwin::events::{
        Deposit as DepositEvent, SWEEP_TO_VEST, SWEEP_TO_WIPE_RECORD, TraderSwept,
    };
    use shortwin::instructions::sweep_trader::WIPE_BITS_LIVE;
    use shortwin::state::{MARKET_FLAG_WIPE_LIVE, MARKET_FLAG_WIPE_TRADER_SWEPT, ShareEpoch};
    use shortwin_litesvm_tests::engine::{sweep_trader_ix, sweep_trader_ix_for, trader_wsol_ata};
    use shortwin_litesvm_tests::tokens;
    use shortwin_math::pool::WIPE_TRADER_SWEPT;

    /// The only lender withdraws free tokens down to `keep` (settle_it.rs's helper).
    fn withdraw_down_to(sc: &mut Sc, keep: u64) {
        let m = sc.market();
        let pool = m.pool();
        let want = m.tokens_free - keep;
        let lender = sc
            .env
            .lender(&sc.mm.market(), &sc.mm.lender.pubkey())
            .expect("lender");
        let (mut lo, mut hi) = (0u128, lender.shares);
        while lo < hi {
            let mid = lo + (hi - lo).div_ceil(2);
            if shortwin_math::pool::tokens_for_shares(&pool, mid).unwrap() <= u128::from(want) {
                lo = mid;
            } else {
                hi = mid - 1;
            }
        }
        let kp = sc.mm.lender.insecure_clone();
        ok(sc
            .env
            .send(&[ix::withdraw(&sc.mm.lender_keys(), lo, want)], &[&kp]));
        assert_eq!(sc.market().tokens_free, keep);
    }

    /// An organic full wipe (the audit's path): the lender withdraws every free token, then a
    /// gap settles the last borrowed tokens S1, which rolls a record with `num = 0`.
    fn wiped() -> Sc {
        let mut sc = Sc::default_market();
        sc.env.tick(&sc.mm, 5);
        withdraw_down_to(&mut sc, 0);
        sc.gap_and_arm(1.05);
        let (_s, out) = sc.run_until_settled_out(80).expect("settled");
        assert!(out.rolled.is_some_and(|r| r.num == 0), "a full wipe");
        let m = sc.market();
        assert!(m.has_flag(MARKET_FLAG_WIPE_LIVE));
        assert!(!m.has_flag(MARKET_FLAG_WIPE_TRADER_SWEPT));
        sc
    }

    /// The whale's deposit into the wiped market, with the records it needs.
    fn whale_deposit(sc: &Sc) -> anchor_lang::solana_program::instruction::Instruction {
        let k = LenderKeys {
            owner: sc.whale.key(),
            mint: sc.mm.mint(),
            token_program: sc.mm.keys.token_program,
            owner_token_account: sc.whale.base,
        };
        let recs = ix::epoch_records(&sc.mm.market(), 0, sc.market().share_epoch);
        ix::with_remaining(ix::deposit(&k, 1_000_000_000_000, 0), &recs)
    }

    fn sweep(sc: &mut Sc) -> TraderSwept {
        let ix = sweep_trader_ix_for(&sc.env, &sc.mm.market());
        let meta = ok(sc.env.send(&[ix], &[]));
        events::of::<TraderSwept>(&meta).pop().expect("TraderSwept")
    }

    fn wipe_record(sc: &Sc) -> (Pubkey, ShareEpoch) {
        let key = pda::share_epoch(&sc.mm.market(), sc.market().share_epoch - 1).0;
        (
            key,
            read::borsh::<ShareEpoch>(&sc.env.svm, &key).expect("record"),
        )
    }

    /// M2-SEC-4. The audit's reproducer — after an organic full wipe a deposit fails with
    /// `WipeSweepPending` whatever else runs (a donation to the trader, an open, cranks, 30
    /// days) — and the fix: the permissionless `sweep_trader` closes `ATA(trader, WSOL)`,
    /// sweeps the trader PDA, credits the live wipe record (the wiped lender's), sets
    /// `wipe_trader_swept`, and the deposit then lands.
    #[test]
    fn m2_sec_4_sweep_trader_reopens_lending_after_a_full_wipe() {
        let mut sc = wiped();
        let market = sc.mm.market();
        let dep = whale_deposit(&sc);
        let kp = sc.whale.kp.insecure_clone();
        assert_err(
            &sc.env.send(std::slice::from_ref(&dep), &[&kp]),
            E::WipeSweepPending,
        );
        // Nothing else sets the bit (the audit's finding).
        let trader = sc.mm.keys.trader;
        sc.env.fund(&trader, 5_000_000);
        let mut b = sc.env.borrower();
        let (res, _) = sc.env.open(&sc.mm, &mut b, Q);
        assert_err(&res, E::InsufficientInventory);
        sc.env.tick(&sc.mm, 30);
        sc.env.warp(30 * 86_400);
        sc.crank();
        assert_err(
            &sc.env.send(std::slice::from_ref(&dep), &[&kp]),
            E::WipeSweepPending,
        );

        // Someone also created and funded ATA(trader, WSOL).
        let payer = sc.env.payer.insecure_clone();
        let ata = tokens::create_ata(
            &mut sc.env.svm,
            &payer,
            &trader,
            &WSOL_MINT,
            TokenProgram::Spl,
        );
        assert_eq!(ata, trader_wsol_ata(&market));
        mock::wrap(&mut sc.env, &ata, 3_000_000);
        let ata_lamports = read::lamports(&sc.env.svm, &ata);
        let (_, rec0) = wipe_record(&sc);
        let sol0 = sc.env.market_vault(&market).lender_sol;

        let ev = sweep(&mut sc);
        assert_eq!(ev.lamports, 5_000_000 + ata_lamports);
        assert_eq!(ev.to, SWEEP_TO_WIPE_RECORD);
        assert_eq!(ev.wipe_bits, WIPE_TRADER_SWEPT | WIPE_BITS_LIVE);
        assert_eq!(read::lamports(&sc.env.svm, &trader), 0);
        assert_eq!(
            read::lamports(&sc.env.svm, &ata),
            0,
            "ATA(trader, WSOL) closed"
        );
        let m = sc.market();
        assert!(m.has_flag(MARKET_FLAG_WIPE_LIVE) && m.has_flag(MARKET_FLAG_WIPE_TRADER_SWEPT));
        assert_eq!(sc.env.market_vault(&market).lender_sol, sol0 + ev.lamports);
        let (_, rec1) = wipe_record(&sc);
        assert!(rec1.acc_end > rec0.acc_end && rec1.acc_r_end > rec0.acc_r_end);

        // The wiped lender is paid the sweep with its settlement SOL.
        let lender = sc.mm.lender.insecure_clone();
        let recs = ix::epoch_records(&market, 0, m.share_epoch);
        let claim = ix::with_remaining(ix::claim_sol(&lender.pubkey(), &market), &recs);
        let before = read::lamports(&sc.env.svm, &lender.pubkey());
        ok(sc.env.send(&[claim], &[&lender]));
        let paid = read::lamports(&sc.env.svm, &lender.pubkey()) - before;
        assert!(paid + 1 >= ev.lamports, "claimed {paid}");

        // Lending reopens.
        let meta = ok(sc.env.send(&[dep], &[&kp]));
        assert_eq!(events::of::<DepositEvent>(&meta).len(), 1);
        assert!(!sc.market().has_flag(MARKET_FLAG_WIPE_LIVE));
    }

    /// `sweep_trader` sets the bit even when nothing is left to sweep (ARCH §3.9), so a
    /// deposit never waits on a donation.
    #[test]
    fn m2_sec_4_an_empty_sweep_sets_the_bit() {
        let mut sc = wiped();
        let ev = sweep(&mut sc);
        assert_eq!((ev.lamports, ev.to), (0, SWEEP_TO_WIPE_RECORD));
        assert!(sc.market().has_flag(MARKET_FLAG_WIPE_TRADER_SWEPT));
        let dep = whale_deposit(&sc);
        let kp = sc.whale.kp.insecure_clone();
        ok(sc.env.send(&[dep], &[&kp]));
    }

    /// The account rules: while a wipe record is live, exactly the latest record, writable;
    /// otherwise no remaining account; `ATA(trader, WSOL)` at its derived key. With lenders
    /// (`S > 0`) the sweep vests like the legs' normalisation.
    #[test]
    fn m2_sec_4_sweep_trader_account_rules() {
        let mut sc = wiped();
        let market = sc.mm.market();
        let (rec, _) = wipe_record(&sc);
        assert_err(
            &sc.env.send(&[sweep_trader_ix(&market, None)], &[]),
            E::BadRemainingAccounts,
        );
        // A short.win account that is not a `ShareEpoch` (the SOL/USD cache).
        let not_a_record = pda::oracle_cache();
        assert_err(
            &sc.env
                .send(&[sweep_trader_ix(&market, Some(not_a_record))], &[]),
            E::EpochRecordMismatch,
        );
        let mut readonly = sweep_trader_ix(&market, Some(rec));
        readonly.accounts.last_mut().unwrap().is_writable = false;
        assert_err(&sc.env.send(&[readonly], &[]), E::EpochRecordMismatch);
        let mut wrong_ata = sweep_trader_ix(&market, Some(rec));
        wrong_ata.accounts[5].pubkey = Pubkey::new_unique();
        assert_err(&sc.env.send(&[wrong_ata], &[]), E::AccountMismatch);

        // A market with lenders: no record, and the donation vests.
        let mut sc = Sc::default_market();
        sc.env.tick(&sc.mm, 5);
        let market = sc.mm.market();
        assert_err(
            &sc.env.send(&[sweep_trader_ix(&market, Some(rec))], &[]),
            E::BadRemainingAccounts,
        );
        let trader = sc.mm.keys.trader;
        sc.env.fund(&trader, 7_000_000);
        let sol0 = sc.env.market_vault(&market).lender_sol;
        let ev = sweep(&mut sc);
        assert_eq!((ev.lamports, ev.to), (7_000_000, SWEEP_TO_VEST));
        assert_eq!(ev.wipe_bits & WIPE_BITS_LIVE, 0);
        assert_eq!(sc.env.market_vault(&market).lender_sol, sol0 + 7_000_000);
        assert!(sc.market().vest_bal >= 7_000_000, "the donation vests");
        assert_eq!(read::lamports(&sc.env.svm, &trader), 0);
    }
}

// ---- M2-SEC-5 (fixed in M5): `sol_settle` on a `None` market ---------------------------------

mod none_venue {
    use super::*;
    use shortwin::state::{Market, VenueKind};
    use shortwin_math::fees::FeeBps;
    use shortwin_math::quote::Pricing;
    use shortwin_math::rent;

    /// M2-SEC-5, inverted in M5 (the finding's T-S3 test for `None`; ARCH §2.5, §3.8 step 4,
    /// §5.3): `sol_settle` takes a list-only market's detection set `[bonding_curve,
    /// canonical_pool]` as its ReadSet, and an expired position on a `None` market settles S3
    /// once the market's continuous chain has run `s3_wait_s` — never before. The box is the
    /// frozen EMA point; with no venue fee to read, FV is priced by the cap-only risk view (one
    /// combined fee at the position's `venue_fee_hard_cap_bps` on the SOL leg,
    /// `venues::cap_only`) with the expiry fee (O15).
    ///
    /// The mock market is **planted** to `None` (M2's venue never leaves `Mock`), so the
    /// invariant hooks are off from the plant on; the same S3 on a real `None` transition — a
    /// pump curve that completes and graduates elsewhere with the position open — runs with
    /// the hooks on in `pump_it::graduation`.
    #[test]
    fn m2_sec_5_sol_settle_reaches_s3_on_a_none_market() {
        let mut sc = Sc::default_market();
        sc.env.tick(&sc.mm, 5);
        sc.warp_to_deadline(1);
        let pos = sc.pos();
        sc.env.check_invariants = false;
        let key = sc.mm.market();
        let mut acc = read::account(&sc.env.svm, &key).unwrap();
        let mut m: Market = read::zc(&sc.env.svm, &key).unwrap();
        m.venue = VenueKind::None as u8;
        acc.data[8..8 + core::mem::size_of::<Market>()].copy_from_slice(bytemuck::bytes_of(&m));
        sc.env.svm.set_account(key, acc).unwrap();
        let detection = [
            AccountMeta::new_readonly(m.bonding_curve, false),
            AccountMeta::new_readonly(m.canonical_pool, false),
        ];
        let crank = {
            let mut c = crank_price_ix(&sc.mm);
            c.accounts.truncate(c.accounts.len() - 3);
            ix::with_remaining(c, &detection)
        };
        let settle = |sc: &Sc| {
            let mut ix = shortwin_litesvm_tests::engine::sol_settle_ix(
                &sc.env,
                &sc.mm,
                &sc.keeper.pubkey(),
                &sc.b.key(),
                sc.nonce,
            );
            ix.accounts.truncate(ix.accounts.len() - 3);
            ix.accounts.extend(detection.clone());
            ix
        };
        let kp = sc.keeper.insecure_clone();
        // The mock's ReadSet is refused on `None`: the detection set is the ReadSet.
        let wrong = sc.settle_ix();
        assert_err(&sc.env.send(&[wrong], &[&kp]), E::VenueAccountMismatch);
        sc.env.warp(1);
        let first = settle(&sc);
        let out = Outcome::of(&ok(sc.env.send(&[first], &[&kp])));
        assert!(out.settled.is_none(), "the chain only starts");
        let since = sc.market().no_route_since;
        assert_eq!(
            since,
            sc.env.now(),
            "sol_settle starts the S3 chain on None"
        );
        assert_eq!(sc.market().b_lo.get(), 0, "nothing is observed on None");
        let m0 = sc.market();
        // The keeper's ≤ 30 s duty keeps the chain continuous; S3 is not due before
        // `since + s3_wait_s`.
        let mut settled = None;
        for _ in 0..40 {
            sc.env.warp(30);
            ok(sc.env.send(std::slice::from_ref(&crank), &[]));
            let ix = settle(&sc);
            let out = Outcome::of(&ok(sc.env.send(&[ix], &[&kp])));
            if let Some(ev) = out.settled {
                settled = Some(ev);
                break;
            }
            assert!(
                sc.env.now() < since + i64::from(pos.s3_wait_s),
                "S3 only when due"
            );
        }
        let ev = settled.expect("S3 on None");
        assert_eq!(ev.condition, 3);
        assert!(sc.env.now() >= since + i64::from(pos.s3_wait_s));
        assert!(sc.env.now() < since + i64::from(pos.s3_wait_s) + 30);
        let m = sc.market();
        let (p, l) = (m0.ema_price.get(), m0.ema_liq.get());
        assert_eq!(
            (m.ema_price.get(), m.ema_liq.get()),
            (p, l),
            "the EMA is frozen while nothing is observed"
        );
        assert_eq!(
            (ev.p_lo, ev.p_hi, ev.l_lo, ev.l_hi),
            (p, p, l, l),
            "the EMA point"
        );
        assert_eq!(
            (ev.pprime_lo, ev.pprime_hi),
            (pos.expiry_fee_bps, pos.expiry_fee_bps),
            "an expired position pays the expiry fee (O15)"
        );
        let r_due = rent::due(&pos.pos_rent(), &m.basis()).unwrap();
        let cap = Pricing::PumpSwap(FeeBps::new(0, u64::from(pos.venue_fee_hard_cap_bps), 0));
        let b = cap.buy_repriced(pos.q, l, p).expect("FV quote").cost;
        let fv =
            b + (u128::from(b) * u128::from(pos.expiry_fee_bps)).div_ceil(10_000) as u64 + r_due;
        assert_eq!(ev.fv, fv, "FV at the cap-only risk view");
        let take = pos.v - k_s(&pos);
        assert_eq!(ev.lender_amount, fv.min(take));
        assert_eq!(ev.borrower_amount, take - ev.lender_amount);
        assert!(sc.pos_opt().is_none(), "the position is closed");
    }
}
