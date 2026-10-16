//! `sol_settle` on the mock venue (ARCH §3.8; ECON §5.6, §8.6, §10.3; M2 track settle-o6):
//! the no-op outcomes, S1 / S2 / S3 with their amounts and waterfalls, the arming clock and
//! its disarms, the share-epoch records a settlement rolls (full wipe and rebase, grief-proof
//! creation), the account checks, T-TRACE / CU, and T-BOX (the settlement box against pushed,
//! frozen and contrary seconds). Every instruction runs through `Env::send`: the invariant
//! hooks (I2, I3, I6, I9, I12, …) check each one.

#![allow(clippy::result_large_err)]

#[path = "settle_support.rs"]
mod support;

use support::*;

mod basics {
    use super::*;

    /// ARCH §3.8 step 5: a healthy position's `sol_settle` is an `Ok` no-op that keeps the
    /// prologue (its observation counts for the second, ECON §5.8 duty 7).
    #[test]
    fn a_healthy_position_is_an_ok_no_op() {
        let mut sc = Sc::default_market();
        sc.env.warp(1);
        let before = sc.pos();
        let lamports_before = lamports(&sc.env, &sc.position_key());
        let out = sc.settle_ok();
        assert!(out.settled.is_none() && out.armed.is_none() && out.disarmed.is_none());
        assert_eq!(sc.pos(), before, "nothing on the position changes");
        assert_eq!(lamports(&sc.env, &sc.position_key()), lamports_before);
        let m = sc.market();
        assert_eq!(
            m.b_sec,
            sc.env.now(),
            "the call recorded this second's observation"
        );
        assert_eq!(m.open_positions, 1);
    }

    /// S1 (SPEC §5.3): a genuine gap beyond the S1 boundary arms the clock; after
    /// `settle_persist_n` EMA updates the keeper's call settles with lenders taking `V − K_s`,
    /// never capped; the borrower gets only its account's rent back (A18). The take is below
    /// `⌊Q·P_ema/WAD⌋ + R_due` (a genuine loss), so nothing vests (AIR-21).
    #[test]
    fn s1_after_a_gap_pays_v_minus_k_s() {
        let mut sc = Sc::default_market();
        sc.env.tick(&sc.mm, 5);
        let kb = sc.s1_boundary();
        let pos = sc.pos();
        let take = sc.take();
        sc.move_by(kb * 1.05);
        let mut armed_at = None;
        let mut settled = None;
        for s in 1..=120u32 {
            let out = sc.second();
            if out.armed.is_some() {
                armed_at = Some((s, sc.arm()));
            }
            if let Some(ev) = out.settled {
                settled = Some((s, ev));
                break;
            }
        }
        let (armed_s, arm) = armed_at.expect("armed");
        let (settled_s, ev) = settled.expect("settled");
        assert_eq!(ev.condition, 1, "S1");
        assert_eq!(ev.lender_amount, take, "S1 is never capped");
        assert_eq!((ev.borrower_amount, ev.lump, ev.fv), (0, 0, 0));
        assert_eq!(ev.k_s, k_s(&pos));
        // One clock of settle_persist_n (40) EMA updates, one per keeper second.
        assert!(arm.armed == 1 && settled_s >= armed_s + u32::from(pos.settle_persist_n));
        assert!(sc.pos_opt().is_none(), "the position is closed");
        let m = sc.market();
        assert_eq!((m.open_positions, m.borrowed_q), (0, 0));
    }
}

mod more {
    use super::*;
    use anchor_lang::solana_program::instruction::AccountMeta;
    use shortwin::errors::ShortwinError as E;
    use shortwin::events::{SETTLE_REASON_NOT_LIQUIDATABLE, SETTLE_REASON_REFUTED};
    use solana_signer::Signer;

    fn gap_and_arm(sc: &mut Sc, beyond: f64) -> u32 {
        sc.gap_and_arm(beyond)
    }

    /// The balances an S1 moves: `K_s` to the keeper, `V − K_s` to `market_vault` (lender
    /// SOL), the position's account (rent + nothing else) and the exposure record to the
    /// borrower (A18); the market loses the position's aggregates (I6).
    #[test]
    fn s1_moves_exactly_the_waterfall() {
        let mut sc = Sc::default_market();
        sc.env.tick(&sc.mm, 5);
        gap_and_arm(&mut sc, 1.05);
        let pos = sc.pos();
        let keeper0 = lamports(&sc.env, &sc.keeper.pubkey());
        let borrower0 = lamports(&sc.env, &sc.b.key());
        let pos_lamports = lamports(&sc.env, &sc.position_key());
        let exposure = pda::exposure(&sc.mm.market(), &sc.b.key()).0;
        let exp_lamports = lamports(&sc.env, &exposure);
        let vault0 = lamports(&sc.env, &sc.mm.keys.market_vault);
        let lender_sol0 = sc.env.market_vault(&sc.mm.market()).lender_sol;
        let (_s, ev) = sc.run_until_settled(80).expect("S1");
        assert_eq!(ev.condition, 1);
        let k = k_s(&pos);
        assert_eq!(lamports(&sc.env, &sc.keeper.pubkey()) - keeper0, k);
        assert_eq!(
            lamports(&sc.env, &sc.b.key()) - borrower0,
            pos_lamports - pos.v + exp_lamports,
            "the borrower recovers the rent of both accounts, nothing more"
        );
        assert_eq!(
            lamports(&sc.env, &sc.mm.keys.market_vault) - vault0,
            pos.v - k
        );
        assert!(sc.env.market_vault(&sc.mm.market()).lender_sol >= lender_sol0 + pos.v - k);
        assert!(
            read::account(&sc.env.svm, &exposure).is_none(),
            "exposure closed at 0"
        );
        let m = sc.market();
        assert_eq!(m.borrowed_q_by_tier, [0; 4]);
        assert_eq!(m.total_notional_open, 0);
        assert_eq!(m.aggr().a, [0; 3]);
        // This S1 has no lump (a gap far beyond the boundary: its take is below `⌊Q·P_ema/WAD⌋ +
        // R_due`, a genuine loss; AIR-21 vests only a take above it); its SOL is immediate but
        // for the rent top-up (M10-8C-1): `R_due` above the rent credited while the position was
        // open (here `R_min`, the gap a minute after the open) vests like a lump.
        let r_due = shortwin_math::rent::due(&pos.pos_rent(), &m.basis()).unwrap();
        let credited = shortwin_math::rent::credited_for(&pos.pos_rent(), &m.basis()).unwrap();
        let top = shortwin_math::pool::rent_top_up(pos.v - k, r_due, credited);
        assert!(top > 0 && r_due == pos.r_min);
        assert_eq!(m.vest_bal, top, "S1 vests only its rent top-up");
    }

    /// ARCH §3.8 step 3: a stale bucket (no crank for a few seconds) keeps a live clock
    /// running; an evaluation with the legs holding more than `settle_arm_gap_s` after the
    /// last one re-arms it (a new `s_count`, so persistence restarts).
    ///
    /// AIR-16 (ECON §5.6): an evaluation that lands in step 2 (not CR-liquidatable, e.g. a
    /// stale bucket fails the spot leg) while the box does not say "not liquidatable" keeps a
    /// live clock running, but leaves a lapsed one as it is: it is not revived with its old
    /// count, so the next evaluation with the legs holding re-arms it.
    #[test]
    fn a_stale_bucket_keeps_the_clock_and_a_long_gap_re_arms() {
        let mut sc = Sc::default_market();
        sc.env.tick(&sc.mm, 5);
        gap_and_arm(&mut sc, 1.05);
        let a0 = sc.arm();
        // 5 s without a keeper: the call's bucket is stale (ref_max_age_s = 2), the clock runs.
        sc.env.warp(5);
        let out = sc.settle_ok();
        assert!(out.armed.is_none() && out.disarmed.is_none() && out.settled.is_none());
        let a1 = sc.arm();
        assert_eq!((a1.armed, a1.count, a1.since), (1, a0.count, a0.since));
        assert_eq!(a1.last, sc.env.now(), "kept: s_last = now");
        // 12 s without an evaluation (> settle_arm_gap_s = 10), then a keeper second with a
        // fresh bucket: the legs hold and the lapsed clock re-arms at the current ema_count.
        sc.env.warp(11);
        sc.crank();
        let out = sc.second();
        let a = out.armed.expect("re-armed");
        assert!(a.s_count > a0.count && a.s_since > a0.since);
        assert_eq!(sc.arm().count, a.s_count);
        // AIR-16: 12 s later (> settle_arm_gap_s), a lone call with a stale bucket lands in
        // step 2 and leaves the lapsed clock as it is (no revive with the old count); the next
        // keeper second with a fresh bucket re-arms it, so persistence restarts.
        let a2 = sc.arm();
        sc.env.warp(12);
        let out = sc.settle_ok();
        assert!(out.armed.is_none() && out.disarmed.is_none() && out.settled.is_none());
        assert_eq!(sc.arm(), a2, "step 2 does not revive a lapsed clock");
        sc.crank();
        let out = sc.second();
        let r = out.armed.expect("the lapsed clock re-arms");
        assert!(out.settled.is_none() && r.s_count > a.s_count && r.s_since > a.s_since);
        assert_eq!(sc.arm().count, r.s_count);
    }

    /// Only box-robust evidence disarms (ARCH §3.8 steps 2 and 3): a genuine partial reversal
    /// (still CR-liquidatable, no longer beyond the S1 boundary) is refuted once the upper
    /// median of the ring's bucket highs follows it; a full reversal (no longer liquidatable)
    /// disarms as "not liquidatable" once the box says so. Neither happens in the first
    /// seconds after the reversal: the median needs 5 of the 8 entries.
    #[test]
    fn reversals_disarm_on_the_box_only() {
        for (to, reason) in [
            (0.80, SETTLE_REASON_REFUTED),
            (0.20, SETTLE_REASON_NOT_LIQUIDATABLE),
        ] {
            let mut sc = Sc::default_market();
            sc.env.tick(&sc.mm, 5);
            let p0 = sc.spot();
            let kb = sc.s1_boundary();
            gap_and_arm(&mut sc, 1.05);
            sc.env.tick(&sc.mm, 3);
            sc.move_to(scale(p0, kb * to));
            let mut disarmed = None;
            for s in 1..=40u32 {
                let out = sc.second();
                assert!(out.settled.is_none());
                if let Some(d) = out.disarmed {
                    disarmed = Some((s, d));
                    break;
                }
            }
            let (s, d) = disarmed.expect("disarmed");
            assert_eq!(d.reason, reason, "reversal to {to} of the S1 boundary");
            assert!(
                s >= 4,
                "one to four seconds cannot move the box: disarmed at {s}"
            );
            assert_eq!(sc.arm().armed, 0);
        }
    }

    /// S2 (SPEC §5.3, D3): an LP drain leaves the buyback's slippage above `σ_max` while the
    /// vault still covers it; the position is liquidatable. After `settle_persist_n` updates
    /// the keeper settles S2 with the lenders' take capped at `FV(P_ema, L_ema)` (box-
    /// consistent within `fv_dev_bps`), the excess to the borrower, and the part above
    /// `⌊Q·P_ema/WAD⌋ + R_due` vesting (R4B-6).
    #[test]
    fn s2_is_capped_at_fair_value() {
        let mut sc = Sc::default_market();
        sc.env.tick(&sc.mm, 5);
        sc.drain(9_850);
        sc.env.tick(&sc.mm, 90);
        sc.move_by(1.8);
        let pos = sc.pos();
        let vest0 = sc.market().vest_bal;
        let (_s, out) = sc.run_until_settled_out(120).expect("S2");
        eprintln!("sol_settle S2: {} trace entries, {} CU", out.trace, out.cu);
        assert!(out.trace == 2 && out.cu <= 150_000);
        let ev = out.settled.unwrap();
        assert_eq!(ev.condition, 2, "S2");
        let take = pos.v - k_s(&pos);
        assert!(ev.fv > 0 && ev.lender_amount == ev.fv.min(take));
        assert!(ev.lender_amount < take, "the cap binds");
        assert_eq!(ev.borrower_amount, take - ev.lender_amount);
        let (lo, hi) = (ev.x_lo.min(ev.x_hi), ev.x_lo.max(ev.x_hi));
        assert!(u128::from(hi - lo) * 10_000 <= u128::from(pos.fv_dev_bps) * u128::from(lo));
        assert!(ev.lender_amount + 2 >= lo && ev.lender_amount <= hi + 2);
        // Not expired: p' is the penalty at both corners.
        assert_eq!(
            (ev.pprime_lo, ev.pprime_hi),
            (pos.penalty_bps, pos.penalty_bps)
        );
        let m = sc.market();
        let v_ema = u128::from(pos.q) * m.ema_price.get() / shortwin_math::num::WAD;
        assert!(ev.lump > 0 && u128::from(ev.lender_amount - ev.lump) <= v_ema + u128::from(ev.fv));
        assert!(m.vest_bal >= vest0 + ev.lump - 1, "the lump vests");
    }

    /// The account list (I14 for `sol_settle`): the ReadSet is mandatory and key-checked, no
    /// trailing account is accepted, the borrower and exposure must be the position's.
    #[test]
    fn accounts_are_checked() {
        let mut sc = Sc::default_market();
        sc.env.warp(1);
        let good = sc.settle_ix();
        let kp = sc.keeper.insecure_clone();
        let n = good.accounts.len();
        // Missing ReadSet slot.
        let mut ix = good.clone();
        ix.accounts.truncate(n - 1);
        assert_err(&sc.env.send(&[ix], &[&kp]), E::BadRemainingAccounts);
        // A trailing account.
        let mut ix = good.clone();
        ix.accounts.push(AccountMeta::new_readonly(
            anchor_lang::prelude::Pubkey::new_unique(),
            false,
        ));
        assert_err(&sc.env.send(&[ix], &[&kp]), E::BadRemainingAccounts);
        // A wrong ReadSet key (the vaults swapped).
        let mut ix = good.clone();
        ix.accounts.swap(n - 1, n - 2);
        assert_err(&sc.env.send(&[ix], &[&kp]), E::VenueAccountMismatch);
        // Another borrower.
        let other = sc.env.borrower();
        let mut ix = good.clone();
        ix.accounts[4].pubkey = other.key();
        assert!(sc.env.send(&[ix], &[&kp]).is_err());
        ok(sc.env.send(&[good], &[&kp]));
    }
}

mod epochs {
    use super::*;
    use shortwin::errors::ShortwinError as E;
    use shortwin::state::ShareEpoch;
    use shortwin_litesvm_tests::ix;
    use shortwin_math::pool::{R_MAX, VEST_FLUSH_RATIO, VIRTUAL_ASSETS, VIRTUAL_SHARES};
    use solana_signer::Signer;

    /// The only lender withdraws free tokens down to `keep` (the largest share amount whose
    /// tokens fit, `pool::tokens_for_shares`).
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

    fn record(sc: &Sc, epoch: u32) -> ShareEpoch {
        read::borsh(&sc.env.svm, &pda::share_epoch(&sc.mm.market(), epoch).0).expect("record")
    }

    /// ECON §9.5 (V2-12): the settlement of the last borrowed tokens with no free tokens left
    /// wipes the pool: `pool::maybe_roll` closes the epoch with `num = 0`, the keeper creates
    /// the `ShareEpoch` record (it pays its rent), the unreleased vest is flushed into it, and
    /// the market waits for its post-wipe sweeps (`wipe_live`, shares 0).
    #[test]
    fn a_full_wipe_rolls_the_epoch_into_a_live_record() {
        let mut sc = Sc::default_market();
        sc.env.tick(&sc.mm, 5);
        withdraw_down_to(&mut sc, 0);
        let epoch = sc.market().share_epoch;
        let s_old = sc.market().total_shares.get();
        let keeper0 = lamports(&sc.env, &sc.keeper.pubkey());
        let k = k_s(&sc.pos());
        sc.gap_and_arm(1.05);
        let (_s, out) = sc.run_until_settled_out(80).expect("settled");
        // T-TRACE (ARCH §6.2: "+3 when it creates a ShareEpoch record"): the instruction, the
        // record's funding transfer, `allocate`, `assign`, and two events (`SolSettle`,
        // `EpochRolled`).
        assert_eq!(out.trace, 6);
        assert!(out.cu <= 150_000, "{} CU", out.cu);
        eprintln!(
            "sol_settle with an epoch roll: {} trace entries, {} CU",
            out.trace, out.cu
        );
        let ev = out.settled.unwrap();
        assert_eq!(ev.condition, 1);
        let rolled = out.rolled.expect("EpochRolled");
        assert_eq!((rolled.epoch, rolled.num, rolled.den), (epoch, 0, 1));
        assert_eq!(
            ev.vest_flushed, rolled.vest_flushed,
            "both events report the flush"
        );
        let rec = record(&sc, epoch);
        assert_eq!(
            (rec.epoch, rec.num, rec.den_log1000, rec.s_old),
            (epoch, 0, 0, s_old)
        );
        assert_eq!(rec.market, sc.mm.market());
        assert_eq!(rec.acc_end, rolled.acc_end);
        let rent = shortwin_litesvm_tests::read::account(
            &sc.env.svm,
            &pda::share_epoch(&sc.mm.market(), epoch).0,
        )
        .unwrap();
        assert_eq!(rent.owner, shortwin::ID);
        assert_eq!(
            lamports(&sc.env, &sc.keeper.pubkey()),
            keeper0 + k - rent.lamports,
            "the keeper pays the record's rent out of K_s"
        );
        let m = sc.market();
        assert_eq!(
            (m.share_epoch, m.total_shares.get(), m.vest_bal),
            (epoch + 1, 0, 0)
        );
        assert!(m.has_flag(shortwin::state::MARKET_FLAG_WIPE_LIVE));
    }

    /// ECON §9.5 (C0-6): a settlement that leaves `S + VS > R_MAX·(T + ΣB_Q + VA)` rebases
    /// the shares by `1/1,000^k` (a record with `num = 1`). AIR-07: it leaves 10⁹ of the
    /// 3.001·10¹² token claims it found (at most 1/1,000), so the old shares also keep the
    /// unreleased vest — here the S1's rent top-up — at once, as at a full wipe
    /// (`vest_flushed`), instead of the next depositor taking it.
    #[test]
    fn a_rebase_rolls_the_epoch() {
        let mut sc = Sc::default_market();
        sc.env.tick(&sc.mm, 5);
        withdraw_down_to(&mut sc, 1_000_000_000);
        let epoch = sc.market().share_epoch;
        let s_old = sc.market().total_shares.get();
        let (pos, vest0) = (sc.pos(), sc.market().vest_bal);
        sc.gap_and_arm(1.05);
        let (_s, out) = sc.run_until_settled_out(80).expect("settled");
        let ev = out.settled.expect("SolSettle");
        let rolled = out.rolled.expect("EpochRolled");
        let basis = sc.market().basis();
        let r_due = shortwin_math::rent::due(&pos.pos_rent(), &basis).unwrap();
        let credited = shortwin_math::rent::credited_for(&pos.pos_rent(), &basis).unwrap();
        let top = shortwin_math::pool::rent_top_up(ev.lender_amount - ev.lump, r_due, credited);
        assert!(top > 0);
        assert_eq!(
            (rolled.epoch, rolled.num, rolled.vest_flushed),
            (epoch, 1, vest0 + ev.lump + top)
        );
        assert_eq!(
            ev.vest_flushed, rolled.vest_flushed,
            "both events report the flush"
        );
        assert_eq!(sc.market().vest_bal, 0, "AIR-07: flushed to the old shares");
        let rec = record(&sc, epoch);
        assert!(rec.den_log1000 >= 1 && rec.s_old == s_old);
        assert_eq!(rolled.den, 1_000u128.pow(u32::from(rec.den_log1000)));
        let m = sc.market();
        assert_eq!(m.share_epoch, epoch + 1);
        assert_eq!(m.total_shares.get(), s_old / rolled.den);
        let claims = u128::from(m.tokens_free) + u128::from(m.borrowed_q);
        assert!(m.total_shares.get() + VIRTUAL_SHARES <= R_MAX * (claims + VIRTUAL_ASSETS));
    }

    /// AIR-07, the flush without a roll (the indexer's `vest_bal`): a settlement that leaves at
    /// most 1/1,000 of the token claims it found flushes the vest even when the pool needs no
    /// rebase, so no `EpochRolled` is emitted; `SolSettle.vest_flushed` reports the flush (before
    /// it, no event did, and the indexer kept the last crank's `vest_bal`). A running vest — a
    /// trader sweep's 3,000,000 lamports — is there before. The window, with the only lender's
    /// shares at ≈ `VS/VA` = 1,000 a token: `T = ⌊q/999⌋` free tokens give `999·T ≤ q` (a flush)
    /// and `S + VS ≤ R_MAX·(T + VA)` (no rebase). One free token more leaves more than 1/1,000
    /// of the claims: no flush, `vest_flushed = 0`, and the vest keeps vesting. The report costs
    /// no instruction-trace entry: the instruction and its one event, as without it. Without the
    /// report the `Env::send` hook (`lender::vest_flushes_are_reported`) fails the flush.
    #[test]
    fn a_flush_without_a_roll_is_reported_in_sol_settle() {
        use shortwin_litesvm_tests::engine::sweep_trader_ix_for;
        use shortwin_litesvm_tests::invariants::IxEvents;
        use shortwin_litesvm_tests::invariants::lender::{PoolState, vest_flushes_are_reported};
        for extra in [0u64, 1] {
            let mut sc = Sc::default_market();
            sc.env.tick(&sc.mm, 5);
            let q = sc.q;
            withdraw_down_to(&mut sc, q / 999 + extra);
            sc.env.fund(&sc.mm.keys.trader, 3_000_000);
            ok(sc
                .env
                .send(&[sweep_trader_ix_for(&sc.env, &sc.mm.market())], &[]));
            let m = sc.market();
            assert_eq!(m.vest_bal, 3_000_000, "the sweep vests");
            let (epoch, s_old) = (m.share_epoch, m.total_shares.get());
            let t = u128::from(m.tokens_free);
            assert_eq!(m.borrowed_q, q, "the only position");
            let flush = VEST_FLUSH_RATIO * t <= t + u128::from(q);
            assert_eq!(flush, extra == 0, "T = {t}");
            assert!(
                s_old + VIRTUAL_SHARES <= R_MAX * (t + VIRTUAL_ASSETS),
                "the pool needs no rebase after the settlement: S {s_old}, T {t}"
            );
            let pos = sc.pos();
            sc.gap_and_arm(1.05);
            // Keeper seconds; the vest the settlement finds is the one the second's crank left
            // (its prologue released the vest at this second; the settlement's releases nothing
            // more).
            let mut settled = None;
            for _ in 0..80 {
                sc.env.warp(1);
                sc.crank();
                let pre = PoolState::load(&sc.env.svm, &sc.mm.market()).expect("pool");
                let out = sc.settle_ok();
                if out.settled.is_some() {
                    settled = Some((out, pre));
                    break;
                }
            }
            let (out, pre) = settled.expect("settled");
            let vest_before = pre.pool.vest_bal;
            assert!(vest_before > 0, "a running vest before the settlement");
            // The hook `Env::send` ran on this settlement (lender::check_ix) flags a flush that no
            // event reports — the program before `SolSettle.vest_flushed`.
            let post = PoolState::load(&sc.env.svm, &sc.mm.market()).expect("pool");
            assert_eq!(
                vest_flushes_are_reported(&pre, &post, &IxEvents::default()).is_err(),
                flush,
                "the hook flags an unreported flush"
            );
            let ev = out.settled.expect("SolSettle");
            assert_eq!(ev.condition, 1);
            assert!(out.rolled.is_none(), "no epoch roll, no EpochRolled");
            assert_eq!(out.trace, 2, "the instruction and SolSettle");
            let basis = sc.market().basis();
            let r_due = shortwin_math::rent::due(&pos.pos_rent(), &basis).unwrap();
            let credited = shortwin_math::rent::credited_for(&pos.pos_rent(), &basis).unwrap();
            let top = shortwin_math::pool::rent_top_up(ev.lender_amount - ev.lump, r_due, credited);
            let vest = vest_before + ev.lump + top;
            let m = sc.market();
            assert_eq!(
                (m.share_epoch, m.total_shares.get(), m.tokens_free),
                (epoch, s_old, q / 999 + extra),
                "no roll: the epoch and the shares stay"
            );
            if flush {
                assert_eq!(ev.vest_flushed, vest, "the flush is reported");
                assert_eq!(m.vest_bal, 0, "flushed to the shares");
            } else {
                assert_eq!(ev.vest_flushed, 0, "no flush, none reported");
                assert_eq!(m.vest_bal, vest, "the vest keeps vesting");
            }
            eprintln!(
                "T = q/999 + {extra}: vest {vest_before} before, vest_flushed {}, {} trace \
                 entries, {} CU",
                ev.vest_flushed, out.trace, out.cu
            );
        }
    }

    /// ARCH §2.4.1 grief-proof creation: a third party pre-funds the record's address (it cannot
    /// allocate or assign it); the settlement still creates the record, sweeps the donation
    /// into `market_vault` and credits it to the wiped lenders (the live wipe record's top-up).
    #[test]
    fn a_prefunded_record_address_cannot_block_the_roll() {
        let donation = 7_000_000;
        let mut acc_end = [0i128; 2];
        let mut lender_sol = [0u64; 2];
        for (i, donate) in [false, true].into_iter().enumerate() {
            let mut sc = Sc::default_market();
            sc.env.tick(&sc.mm, 5);
            withdraw_down_to(&mut sc, 0);
            let epoch = sc.market().share_epoch;
            if donate {
                let addr = pda::share_epoch(&sc.mm.market(), epoch).0;
                sc.env.fund(&addr, donation);
            }
            let vault0 = sc.env.market_vault(&sc.mm.market()).lender_sol;
            sc.gap_and_arm(1.05);
            let (_s, out) = sc.run_until_settled_out(80).expect("settled");
            // One more trace entry for the sweep of the donation.
            assert_eq!(out.trace, if donate { 7 } else { 6 });
            let ev = out.settled.unwrap();
            let rec = record(&sc, epoch);
            let rec_lamports = lamports(&sc.env, &pda::share_epoch(&sc.mm.market(), epoch).0);
            assert_eq!(
                rec_lamports,
                shortwin_litesvm_tests::read::account(
                    &sc.env.svm,
                    &pda::share_epoch(&sc.mm.market(), epoch).0,
                )
                .unwrap()
                .lamports
            );
            assert_eq!(
                rec_lamports,
                sc.env
                    .svm
                    .minimum_balance_for_rent_exemption(ShareEpoch::SPACE),
                "the donation left the record's address"
            );
            acc_end[i] = rec.acc_end;
            lender_sol[i] =
                sc.env.market_vault(&sc.mm.market()).lender_sol - vault0 - ev.lender_amount;
        }
        assert_eq!(lender_sol, [0, donation], "the donation became lender SOL");
        assert!(
            acc_end[1] > acc_end[0],
            "credited to the wiped lenders' record"
        );
    }

    /// AIR-07 with ARCH §2.4.1's donation, on a rebase (what the indexer's `vest_bal` rule for
    /// `EpochRolled` with `num > 0` and `vest_flushed > 0` — `vest_bal := record_donation` — rests
    /// on): a third party pre-funds the record's address of a rebase that flushes the vest. The
    /// settlement flushes the vest to the old shares first (`vest_flushed`, the same with or
    /// without the donation, in `SolSettle` and `EpochRolled` alike), then sweeps the donation
    /// into `market_vault` (one more trace entry) and vests it anew for the rebased shares: the
    /// market's vest is then exactly the donation, from the settlement's second over
    /// `lump_vest_s`, and the old shares' record does not see it. Without the donation the vest
    /// is 0.
    #[test]
    fn a_flushing_rebase_vests_a_prefunded_record_anew() {
        let donation = 7_000_000;
        let mut flushed = [0u64; 2];
        let mut acc_end = [0i128; 2];
        for (i, donate) in [false, true].into_iter().enumerate() {
            let mut sc = Sc::default_market();
            sc.env.tick(&sc.mm, 5);
            withdraw_down_to(&mut sc, 1_000_000_000);
            let epoch = sc.market().share_epoch;
            let addr = pda::share_epoch(&sc.mm.market(), epoch).0;
            if donate {
                sc.env.fund(&addr, donation);
            }
            let vault0 = sc.env.market_vault(&sc.mm.market()).lender_sol;
            sc.gap_and_arm(1.05);
            let (_s, out) = sc.run_until_settled_out(80).expect("settled");
            assert_eq!(out.trace, if donate { 7 } else { 6 });
            let ev = out.settled.expect("SolSettle");
            let rolled = out.rolled.expect("EpochRolled");
            assert_eq!((rolled.epoch, rolled.num), (epoch, 1), "a rebase");
            assert!(rolled.vest_flushed > 0, "a flushing rebase");
            assert_eq!(
                ev.vest_flushed, rolled.vest_flushed,
                "both events report the flush"
            );
            let swept = sc.env.market_vault(&sc.mm.market()).lender_sol - vault0 - ev.lender_amount;
            assert_eq!(swept, if donate { donation } else { 0 }, "the sweep");
            assert_eq!(
                lamports(&sc.env, &addr),
                sc.env
                    .svm
                    .minimum_balance_for_rent_exemption(ShareEpoch::SPACE),
                "the donation left the record's address"
            );
            let m = sc.market();
            assert_eq!(m.vest_bal, swept, "the vest after the roll is the donation");
            if donate {
                let lump_vest_s = i64::from(sc.env.config().params.lump_vest_s);
                assert_eq!(
                    (m.vest_last, m.vest_end),
                    (ev.ts, ev.ts + lump_vest_s),
                    "vested anew from the settlement's second"
                );
            }
            eprintln!(
                "a flushing rebase, record pre-funded with {}: vest_flushed {}, vest_bal {}, {} \
                 trace entries, {} CU",
                if donate { donation } else { 0 },
                ev.vest_flushed,
                m.vest_bal,
                out.trace,
                out.cu
            );
            flushed[i] = ev.vest_flushed;
            acc_end[i] = record(&sc, epoch).acc_end;
        }
        assert_eq!(
            flushed[0], flushed[1],
            "the donation is not part of the flush"
        );
        assert_eq!(acc_end[0], acc_end[1], "nor of the old shares' record");
    }

    /// The record's key is checked when it is used: a settlement that rolls the epoch fails
    /// with `EpochRecordMismatch` on any other address; calls that roll nothing ignore it.
    #[test]
    fn the_record_key_is_checked_when_used() {
        let mut sc = Sc::default_market();
        sc.env.tick(&sc.mm, 5);
        withdraw_down_to(&mut sc, 0);
        let epoch = sc.market().share_epoch;
        let wrong = pda::share_epoch(&sc.mm.market(), epoch + 1).0;
        sc.gap_and_arm(1.05);
        let kp = sc.keeper.insecure_clone();
        let mut refused = 0;
        for _ in 0..80 {
            sc.env.warp(1);
            sc.crank();
            let mut ix = sc.settle_ix();
            ix.accounts[8].pubkey = wrong;
            match sc.env.send(&[ix], &[&kp]) {
                Ok(meta) => assert!(Outcome::of(&meta).settled.is_none()),
                Err(_) => {
                    let res = {
                        let mut ix = sc.settle_ix();
                        ix.accounts[8].pubkey = wrong;
                        sc.env.send(&[ix], &[&kp])
                    };
                    assert_err(&res, E::EpochRecordMismatch);
                    refused += 1;
                    let out = sc.settle_ok();
                    assert!(out.settled.is_some() && out.rolled.is_some());
                    break;
                }
            }
        }
        assert_eq!(refused, 1);
    }
}

mod trace {
    use super::*;

    /// T-TRACE / T-CU for `sol_settle` (ARCH §6.1, §6.2): a no-op evaluation is one trace
    /// entry, an arming call two (its event), a settlement two (`SolSettle`), and a settlement
    /// that rolls the epoch six (+ System transfer, `allocate`, `assign` of the grief-proof
    /// record and `EpochRolled`). CU stays under ARCH §6.1's 150 k limit.
    #[test]
    fn trace_entries_and_compute_units() {
        let mut sc = Sc::default_market();
        sc.env.tick(&sc.mm, 5);
        sc.env.warp(1);
        let meta = ok(sc.settle());
        let noop = (trace_entries(&meta, 0), meta.compute_units_consumed);
        let kb = sc.s1_boundary();
        sc.move_by(kb * 1.05);
        let mut armed = None;
        let mut settled = None;
        for _ in 0..80 {
            sc.env.warp(1);
            sc.crank();
            let meta = ok(sc.settle());
            let out = Outcome::of(&meta);
            let m = (trace_entries(&meta, 0), meta.compute_units_consumed);
            if out.armed.is_some() {
                armed = Some(m);
            }
            if out.settled.is_some() {
                settled = Some(m);
                break;
            }
        }
        let (armed, settled) = (armed.unwrap(), settled.unwrap());
        eprintln!("sol_settle trace/CU: no-op {noop:?}, arming {armed:?}, S1 {settled:?}");
        assert_eq!((noop.0, armed.0, settled.0), (1, 2, 2));
        for (_, cu) in [noop, armed, settled] {
            assert!(cu <= 150_000, "{cu} CU");
        }
    }
}

/// T-BOX (ARCH §8.2; ECON §5.6, §12.10, §12.11; A65): the settlement box against pushed,
/// frozen and contrary seconds. The gate's other cases run where their scenarios live:
/// the S2 → S1 flip with 1–2 pushed seconds and with one push + 0–10 frozen seconds, the
/// 5-consecutive-seconds limit and the amount pushes (`attacks_settle_it::fv_push`); the
/// refutation push and push-then-freeze (S1 at most 8 s later) and the spot-leg veto
/// (`attacks_settle_it::refute_push`); the contrary low observation before an S3 (`p'` per
/// corner, `attacks_settle_it::fv_push::a_contrary_observation_does_not_change_pprime`); the
/// halted venue's S3 on the EMA point (`route_it::not_executable`).
mod t_box {
    use super::*;
    use anchor_lang::prelude::Pubkey;
    use anchor_lang::solana_program::instruction::Instruction;
    use shortwin_litesvm_tests::tokens;
    use shortwin_math::quote::cpmm;
    use solana_signer::Signer;

    /// The pool admin's token accounts (an LP's): the destination of its removals and the
    /// source of its re-deposits.
    fn lp_accounts(sc: &mut Sc) -> (Pubkey, Pubkey) {
        let admin = sc.mm.pool.admin.pubkey();
        let payer = sc.env.payer.insecure_clone();
        let base = tokens::create_token_account(
            &mut sc.env.svm,
            &payer,
            &sc.mm.mint(),
            &admin,
            sc.mm.spec.program,
        );
        let quote = mock::wsol_account(&mut sc.env, &admin, 0);
        (base, quote)
    }

    /// `[LP out (bps) → optional price push to mult × spot on the thinner pool → middle →
    /// trade back → LP back in]`: an LP's liquidity push around `middle` (R4B-2's `L`
    /// variant; on Raydium an LP remove / add costs no swap fee).
    fn lp_bundle(
        sc: &Sc,
        lp: (Pubkey, Pubkey),
        bps: u64,
        mult: Option<f64>,
        middle: Vec<Instruction>,
    ) -> Vec<Instruction> {
        let r = mock::reserves(&sc.env.svm, sc.mm.mock());
        let base = (u128::from(r.base) * u128::from(bps) / 10_000) as u64;
        let quote = (u128::from(r.quote) * u128::from(bps) / 10_000) as u64;
        let admin = sc.mm.pool.admin.pubkey();
        let mut ixs = vec![mock::remove_liquidity_ix(
            sc.mm.mock(),
            &admin,
            &lp.0,
            &lp.1,
            base,
            quote,
        )];
        let mut back = None;
        if let Some(mult) = mult {
            let (x, y) = ((r.base - base) as f64, (r.quote - quote) as f64);
            let fee = f64::from(FEE_PPM) / 1e6;
            let net = (x * y * (y / x) * mult).sqrt() - y;
            let gross = (net / (1.0 - fee)).ceil() as u64;
            let out = cpmm::sell_exact_in(
                gross,
                u128::from(r.quote - quote),
                u128::from(r.base - base),
                &mock::cpmm_fees(FEE_PPM),
            )
            .expect("a quote")
            .proceeds;
            ixs.push(whale_swap(sc, Dir::QuoteIn, gross));
            back = Some(whale_swap(sc, Dir::BaseIn, out));
        }
        ixs.extend(middle);
        ixs.extend(back);
        ixs.push(mock::add_liquidity_ix(
            sc.mm.mock(),
            &admin,
            &lp.0,
            &lp.1,
            base,
            quote,
        ));
        ixs
    }

    fn send_lp(sc: &mut Sc, ixs: &[Instruction]) -> Outcome {
        let (a, w, k) = (
            sc.mm.pool.admin.insecure_clone(),
            sc.whale.kp.insecure_clone(),
            sc.keeper.insecure_clone(),
        );
        Outcome::of(&ok(sc.env.send(ixs, &[&a, &w, &k])))
    }

    /// T-BOX, the liquidity variant (R4B-2): an LP takes half the liquidity out in the pushed
    /// second (with a price push), and around the keeper's observation in each frozen second,
    /// then flips with the liquidity out inside the bundle too. `L_hi` is the upper median of
    /// the buckets' liquidity highs, which the frozen seconds keep honest: no S1.
    #[test]
    fn the_liquidity_variant_cannot_flip_s2_into_s1() {
        let (mut sc, honest) = fv_model_ready(9_700, 1.8);
        assert_eq!(honest.condition, 2);
        let lp = lp_accounts(&mut sc);
        let saved = sc.env.svm.clone();
        for k in [0u32, 2, 4, 6] {
            sc.env.svm = saved.clone();
            sc.env.warp(1);
            let crank = crank_price_ix(&sc.mm);
            let l_honest = sc.market().ema_liq.get();
            let ixs = lp_bundle(&sc, lp, 5_000, Some(3.0), vec![crank]);
            send_lp(&mut sc, &ixs);
            let m = sc.market();
            assert!(
                m.b_llo.get() * 10 < l_honest * 6 && m.b_lo.get() > m.ema_price.get(),
                "the pushed second observed a thinner, pricier pool"
            );
            for _ in 0..k {
                sc.env.warp(1);
                sc.crank();
                let crank = crank_price_ix(&sc.mm);
                let ixs = lp_bundle(&sc, lp, 5_000, None, vec![crank]);
                send_lp(&mut sc, &ixs);
            }
            sc.env.warp(1);
            let settle = sc.settle_ix();
            let ixs = lp_bundle(&sc, lp, 5_000, Some(3.0), vec![settle]);
            let out = send_lp(&mut sc, &ixs);
            assert!(
                out.settled.as_ref().is_none_or(|e| e.condition != 1),
                "no S1 after an L push and {k} frozen seconds"
            );
        }
    }

    /// T-BOX, on-chain and seeded ("one pushed or frozen second cannot change a box
    /// decision"): from the S2 model case one second before the honest settlement, one
    /// attacker-only second at a random price (0.3×–3×), or one frozen second (the keeper's
    /// observation plus one just beyond `P_ema`), then the honest keeper: the outcome is never
    /// S1, and the S2 amount stays within `fv_dev_bps` of the honest one (twice, for the
    /// pushed EMA still in the point `x(P_ema, L_ema)`).
    #[test]
    fn one_pushed_or_frozen_second_cannot_change_a_box_decision() {
        let (mut sc, honest) = fv_model_ready(9_700, 1.8);
        let dev = u64::from(sc.pos().fv_dev_bps);
        let saved = sc.env.svm.clone();
        let mut seed: u64 = 0x5eed_0b0c_5e77_1e00;
        let mut next = || {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            seed >> 33
        };
        for case in 0..12 {
            sc.env.svm = saved.clone();
            let r = next();
            if case % 3 == 2 {
                // A frozen second, above or below P_ema.
                sc.env.warp(1);
                sc.crank();
                let ema = sc.market().ema_price.get();
                let bps = if r % 2 == 0 { 10_005 } else { 9_995 };
                sc.attacker_bundle(ema * bps / 10_000);
            } else {
                let mult = 0.3 + (r % 2_700) as f64 / 1_000.0;
                push(&mut sc, mult);
            }
            let (_s, ev) = sc.run_until_settled(120).expect("a settlement");
            assert_eq!(ev.condition, 2, "case {case}: never S1");
            assert!(
                ev.lender_amount.abs_diff(honest.lender_amount) * 10_000
                    <= 2 * dev * honest.lender_amount,
                "case {case}: {} against the honest {}",
                ev.lender_amount,
                honest.lender_amount
            );
        }
    }

    /// T-BOX (R3A-6 a): one attacker-only second at a random dump (0.3×–0.95×) at a random
    /// moment of an armed S1 clock never disarms it — the refutation corner is the upper
    /// median of the bucket highs — and S1 still lands, at most 8 s later.
    #[test]
    fn one_attacker_second_never_disarms_an_s1_clock() {
        let mut sc = Sc::rc();
        sc.env.tick(&sc.mm, 120);
        let armed_after = sc.gap_and_arm(1.05);
        let saved = sc.env.svm.clone();
        let (honest_s, _) = sc.run_until_settled(80).expect("honest S1");
        let mut seed: u64 = 0x0b0c_a11e_d15a_4a3d;
        for case in 0..8 {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let at = (seed >> 33) % u64::from(honest_s - 1);
            let mult = 0.3 + ((seed >> 20) % 650) as f64 / 1_000.0;
            sc.env.svm = saved.clone();
            let mut settled = None;
            let mut s = 0u32;
            while s < 120 {
                s += 1;
                if u64::from(s) == at + 1 {
                    push(&mut sc, mult);
                    continue;
                }
                let out = sc.second();
                assert!(
                    out.disarmed.is_none(),
                    "case {case}: disarmed by one second"
                );
                if let Some(ev) = out.settled {
                    settled = Some((s, ev));
                    break;
                }
            }
            let (s, ev) = settled.expect("S1");
            assert_eq!(ev.condition, 1, "case {case}");
            assert!(
                s <= honest_s + 8,
                "case {case} (push ×{mult:.2} at +{at} s): S1 at {s}, honest {honest_s}, armed after {armed_after}"
            );
        }
    }
}
