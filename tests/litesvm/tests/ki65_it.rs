//! KI-65 (ECON §9.5, §9.4 R3B-2 / R4B-4; ARCH §2.3.6, §3.3): no rebase orphans, through the
//! program. A lender's conversion through a rebase record (`1/1,000^k`) mints
//! `⌊(conv_rem + s)/den⌋` and leaves the remainder in the record's `conv_rem`, which every lender
//! instruction that converts writes back (`engine::lender::store_records`), so once every lender
//! has converted they hold exactly the new `S`. Before, each lender was floored alone and up to
//! `n − 1` shares of `S` had no holder: once every real lender left, `S` stayed above 0, the
//! R3B-2 flush never fired and the vest (and every later credit) went to shares nobody could
//! claim (the 24 h fuzz run B: 19,131,900,419 lamports on the mock; the reproducers replay to
//! dust in `tests/fuzz/tests/smoke.rs`).
//!
//! Every lender instruction runs through the lender lab (`lender_it/common.rs`): checked
//! against the math crate on its pre-state — the stored records included — with the hooks
//! after every transaction (I1 is now `Σ shares = S` once every lender is touched on copies).
//! The tests follow the KI-65 spec: the last lender after a rebase is paid the vest and `S`
//! reaches 0; R4B-4's whales (W1–W3, `pool_model.whale_order_demo`); multi-record lazy
//! conversions in one `claim_sol` and in `sync_lender(1)` steps; dust conversions and the
//! V2-12 jump; the writable-record rule (`EpochRecordReadOnly`); a deficit lender converting
//! through `cover_deficit`; re-entry after the flush; `claim_sol ×3`; and the I1 hook against a
//! conversion the program did not store. Since the owner's decision of 2026-10-07
//! (KI65-REV-SEC1) only a position's owner converts it through a rebase record; anyone else's
//! `sync_lender` stops there with the pot-only touch (KI-66, seventh round; plain catch-up, the
//! V2-12 jump and the parking stay permissionless): the `order_*` tests pin that a third party
//! can no longer order honest lenders' conversions (the security review's attacks convert
//! nobody, and lazy honest lenders keep their units and the vest), and what remains: a party orders its own conversions (Sybil positions placed before
//! the roll, a tuned residue) and converts after honest lenders' own conversions — at any later
//! time, no atomicity needed — collecting up to one unit per position it holds; where that
//! leaves every honest holder at 0 shares it is paid the whole vest — a JIT depositor too, with
//! one Sybil placed before the roll. The figures are pinned to the lamport (`assert_eq!`; the
//! audit docs quote them). And the rule's price for the pot, paid back by the pot-only touch
//! (KI-66): a surplus held by a lender behind a rebase record reaches the pot (R3A-2) by anyone's
//! sync, without its owner.
//!
//! Requires the SBF build: `anchor build --tools-version v1.57 --arch v3`.

#![allow(clippy::result_large_err)]

#[allow(dead_code)]
#[path = "lender_it/common.rs"]
mod common;

use anchor_lang::solana_program::instruction::AccountMeta;
use common::{Lab, code};
use shortwin::errors::ShortwinError;
use shortwin::events::ClaimSol;
use shortwin::state::LenderPosition;
use shortwin_litesvm_tests::errors::custom_code;
use shortwin_litesvm_tests::invariants::check_all;
use shortwin_litesvm_tests::invariants::lender::PoolState;
use shortwin_litesvm_tests::ix::LenderKeys;
use shortwin_litesvm_tests::tokens::TokenProgram;
use shortwin_litesvm_tests::{events, ix, pda};
use shortwin_math::Rounding;
use shortwin_math::num::{WAD, mul_div_i128};
use shortwin_math::pool::{self, Standing, VIRTUAL_ASSETS, VIRTUAL_SHARES};

/// One SOL.
const U: u64 = 1_000_000_000;
/// The lumps of these tests: 3 SOL.
const LUMP: u64 = 3 * U;
/// The rebase unit of the tests' rolls (`1/1,000`).
const DEN: u128 = 1_000;

/// `name` deposits `amount` tokens, then burns (for 0 tokens) what leaves its shares at `rem`
/// modulo 1,000: a dust residue below the next rebase's unit.
fn dust(lab: &mut Lab, name: &str, amount: u64, rem: u128) -> u128 {
    lab.deposit(name, amount).unwrap();
    let s = lab.position(name).unwrap().shares;
    let extra = (s % DEN + DEN - rem) % DEN;
    if extra > 0 {
        assert_eq!(
            lab.withdraw(name, extra).map(|(t, _)| t),
            Ok(0),
            "a 0-token burn"
        );
    }
    lab.position(name).unwrap().shares
}

/// A settlement of everything but one raw token that rebases the shares (the share price falls
/// below `R_MAX`'s bound), then a `lump` that vests in the rebased pool: `sol` reaches the
/// lenders in all. The lump is swept in the same second after the roll: since AIR-07 a lump
/// that arrives with a settlement leaving at most 1/1,000 of the claims is flushed to the old
/// shares at once, and these tests need a vest that runs in the rebased pool.
fn rebase(lab: &mut Lab, pid: &str, sol: u64, lump: u64) {
    let e = lab.market().share_epoch;
    let t = lab.market().tokens_free;
    lab.borrow30(pid, t - 1).unwrap();
    lab.settle(pid, sol - lump, 0);
    assert_eq!(lab.market().share_epoch, e + 1, "{pid}: the epoch rolled");
    let r = *lab.state().records.last().unwrap();
    assert_eq!(r.num, 1, "{pid}: a rebase, not a wipe");
    if lump > 0 {
        lab.sweep_sol(lump);
    }
}

/// The shares every lender holds once touched on copies, through one shared copy of the
/// records (as the program converts them, one transaction after another), and `S`.
fn held_and_s(lab: &Lab) -> (u128, u128) {
    let st = lab.state();
    let mut recs = st.records.clone();
    let mut v = st.vault.clone();
    let mut held = 0;
    for (_, lp) in &st.lenders {
        let mut l = lp.lender();
        let from = PoolState::records_from_mut(&mut recs, lp.epoch);
        pool::touch(&st.pool, &mut v, &mut l, from, st.now).unwrap();
        if l.epoch == st.pool.share_epoch {
            held += l.shares;
        }
    }
    (held, st.pool.total_shares)
}

fn assert_every_share_held(lab: &Lab, ctx: &str) {
    let (held, s) = held_and_s(lab);
    assert_eq!(held, s, "{ctx}: Σ shares once every lender is touched");
}

/// `pool_model.exit_all`: `name` withdraws every share in the largest burns it may (its
/// allowance or, while that is 0, the largest burn that pays 0 tokens). Returns `(tokens, SOL)`.
fn exit_all(lab: &mut Lab, name: &str) -> (u64, u64) {
    let (mut tokens, mut sol) = (0, 0);
    loop {
        let Some(l) = lab.touch(name) else {
            return (tokens, sol);
        };
        let p = lab.market().pool();
        if l.shares == 0 || l.epoch != p.share_epoch {
            return (tokens, sol);
        }
        let allow = u128::from(lab.free_allowance(name));
        let c = u128::from(p.tokens_free) + u128::from(p.borrowed_q) + VIRTUAL_ASSETS;
        let svs = p.total_shares + VIRTUAL_SHARES;
        let zero = svs.div_ceil(c) - 1;
        let fit = allow * svs / c;
        let s = l.shares.min(fit.max(zero).max(1));
        let (t, x) = lab.withdraw(name, s).unwrap();
        tokens += t;
        sol += x;
    }
}

fn conv_rem(lab: &Lab, epoch: u32) -> u128 {
    lab.state().records[epoch as usize].conv_rem
}

/// Two lenders with 600 old shares above a multiple of 1,000 each, H (1 token) and W (9,000
/// tokens), and a rebase that vests a 3 SOL lump: the record's rounding unit (1,200 / 1,000)
/// goes to whoever converts second.
fn rebased_pair(program: TokenProgram) -> Lab {
    let mut lab = Lab::with_cap(program, 10_000);
    assert_eq!(dust(&mut lab, "H", 1, 600), 600);
    assert_eq!(dust(&mut lab, "W", 9_000, 600), 7_199_600);
    rebase(&mut lab, "p", U + LUMP, LUMP);
    assert_eq!(lab.market().total_shares.get(), 7_200);
    assert_eq!(lab.market().vest_bal, LUMP);
    lab
}

/// Spec (2): the last real lender after a rebase, with a 6 h vest running, is paid the vest:
/// its burn takes `S` to 0 (R3B-2). Before KI-65 H's conversion floored to 0 and W's to 7,199,
/// leaving one orphan share in `S`; W's exit was never "the last", and the 3 SOL vest released
/// to the orphan.
#[test]
fn the_last_lender_after_a_rebase_is_paid_the_vest() {
    let mut lab = rebased_pair(TokenProgram::Spl);
    let lam = wealth(&mut lab, "W");
    // H converts first (claim_sol writes conv_rem 600) and holds nothing of the new epoch.
    lab.claim("H").unwrap();
    assert_eq!(conv_rem(&lab, 0), 600);
    assert_eq!(
        lab.position("H"),
        None,
        "dust converts to 0 and jumps (V2-12); empty and paid, the claim closes it (KI-67)"
    );
    assert_every_share_held(&lab, "after H");
    // W converts second: 600 + 7,199,600 = 7,200,200 → all 7,200 shares; its exit is the last.
    let (tokens, _) = exit_all(&mut lab, "W");
    assert_eq!(tokens, 1, "the one raw token left");
    let m = lab.market();
    assert_eq!(
        (m.total_shares.get(), m.vest_bal),
        (0, 0),
        "S = 0, the vest flushed"
    );
    assert_eq!(conv_rem(&lab, 0), 200);
    // The 3 SOL vest, W's part of the 1 SOL settled (7,199,600 / 7,200,200 of it) and the
    // position's rent back (the harness payer pays the fees).
    let paid = wealth(&mut lab, "W") - lam;
    assert!(paid >= LUMP + 999_916_667, "W paid {paid}");
    lab.claim("H").unwrap();
    assert!(lab.vault().lender_sol <= 10, "{}", lab.vault().lender_sol);
}

/// Spec (3) W1: R4B-4's 199× JIT whale around a lump while an honest lender holds a share it
/// has not converted yet: `S` counts H's latent share, so the whale's exit never takes all of
/// `S`, never triggers the flush, and it realises nothing of the lump. Mirrors
/// `pool_model.whale_order_demo` (W1).
#[test]
fn w1_a_jit_whale_takes_nothing_while_a_share_is_latent() {
    let mut lab = Lab::with_cap(TokenProgram::Token2022, 10_000);
    assert_eq!(dust(&mut lab, "H", 1, 0), 1_000);
    lab.deposit("W", 1_000_000).unwrap();
    rebase(&mut lab, "p", U + LUMP, LUMP);
    exit_all(&mut lab, "W");
    lab.claim("W").unwrap();
    assert_eq!(lab.position("H").unwrap().epoch, 0, "H has not converted");
    assert_eq!(lab.market().total_shares.get(), 1, "H's latent share");
    let lam = wealth(&mut lab, "J");
    lab.deposit("J", 199 * U).unwrap();
    let t = lab.market().tokens_free;
    lab.borrow30("q", t / 2).unwrap();
    lab.repay("q", 0, LUMP);
    let (_, sol) = exit_all(&mut lab, "J");
    lab.claim("J").unwrap();
    assert_eq!(sol, 0);
    assert_eq!(
        wealth(&mut lab, "J"),
        lam,
        "the whale realises 0 (rent of its position back)"
    );
    assert!(lab.market().total_shares.get() >= 1 && lab.market().vest_bal > 0);
    lab.advance(i64::from(lab.lump_vest_s) + 1, 0);
    let h = lab.claim("H").unwrap();
    assert!(h >= 2 * LUMP, "H is paid both vests: {h}");
    assert_every_share_held(&lab, "end");
}

/// Spec (3) W2: the whale bundles [settlement (vest + rebase) → `sync_lender` of both lenders
/// in either order → exit]. A third party's `sync_lender` of H converts nothing (the pot-only
/// touch, KI65-REV-SEC1 / KI-66): each lender converts itself (signed). The record's rounding unit goes to the
/// second converter; nobody gets less than its floor (H's is 0 shares). Order H then W (W
/// converts after H's own conversion): W holds all of `S` and its exit is paid the 3 SOL vest that
/// was stranded before KI-65; order W then H: H holds the unit and is paid the vest after W
/// leaves.
#[test]
fn w2_the_sync_order_moves_only_the_rounding_unit() {
    for (first, second) in [("H", "W"), ("W", "H")] {
        let mut lab = rebased_pair(TokenProgram::BOTH[usize::from(first == "W")]);
        assert_eq!(lab.sync("H", 1), Ok(Standing::Behind), "a third party");
        assert_eq!(conv_rem(&lab, 0), 0, "H not converted");
        assert_eq!(lab.sync_signed(first, 1), Ok(Standing::Current));
        assert_every_share_held(&lab, first);
        assert_eq!(lab.sync_signed(second, 1), Ok(Standing::Current));
        let (h, w) = (
            lab.position("H").map_or(0, |l| l.shares),
            lab.position("W").unwrap().shares,
        );
        // Nobody below its floor (H's, ⌊600/1,000⌋, is 0), nobody above it by more than one.
        assert!(
            h <= 1 && (7_199_600 / DEN..=7_199_600 / DEN + 1).contains(&w),
            "H {h}, W {w}"
        );
        assert_eq!(h + w, 7_200);
        assert_eq!(conv_rem(&lab, 0), 200);
        let lam_w = wealth(&mut lab, "W");
        exit_all(&mut lab, "W");
        lab.claim("W").unwrap();
        let w_sol = wealth(&mut lab, "W") - lam_w;
        lab.advance(i64::from(lab.lump_vest_s) + 1, 0);
        let h_sol = lab.claim("H").unwrap();
        if first == "H" {
            assert_eq!((h, w), (0, 7_200));
            assert!(w_sol >= LUMP, "W's exit is the last: {w_sol}");
        } else {
            assert_eq!((h, w), (1, 7_199));
            // W: its part of the 1 SOL settled and its position's rent back, not the vest.
            assert!(w_sol < LUMP && h_sol >= LUMP, "W {w_sol}, H {h_sol}");
        }
        assert!(lab.vault().lender_sol <= 10, "{first} first: stranded");
    }
}

/// Spec (3) W3: after the roll the other holder G leaves, so `S` is H's unconverted share; a
/// whale deposits, a 3 SOL sweep vests, the whale leaves: no flush (`S` > the whale's shares),
/// `S` stays at 1, and H, converting later, is paid the vest.
#[test]
fn w3_a_deposit_while_a_share_is_latent_gets_no_flush() {
    let mut lab = rebased_pair(TokenProgram::Spl);
    exit_all(&mut lab, "W");
    lab.claim("W").unwrap();
    assert_eq!(lab.market().total_shares.get(), 1, "H's latent share");
    let lam = wealth(&mut lab, "J");
    lab.deposit("J", 1_000_000).unwrap();
    lab.sweep_sol(LUMP);
    exit_all(&mut lab, "J");
    lab.claim("J").unwrap();
    assert_eq!(wealth(&mut lab, "J"), lam, "the whale realises 0");
    assert_eq!(lab.market().total_shares.get(), 1);
    lab.advance(i64::from(lab.lump_vest_s) + 1, 0);
    let h = lab.claim("H").unwrap();
    assert!(h >= 2 * LUMP, "H is paid both vests: {h}");
    assert_eq!(lab.position("H").unwrap().shares, 1);
    assert_every_share_held(&lab, "end");
}

/// Spec (4): a lender three rebase records behind converts through all three in one
/// `claim_sol` (each record's `conv_rem` written, as the math crate computes), another in
/// `sync_lender(1)` steps; `Σ shares = S` after every step.
#[test]
fn multi_record_lazy_conversions() {
    let mut lab = Lab::with_cap(TokenProgram::Token2022, 10_000);
    dust(&mut lab, "A", 1_000_000_000_000, 123);
    dust(&mut lab, "B", 777_777_777_777, 456);
    for i in 0..3 {
        if i > 0 {
            dust(&mut lab, "K", 2_000, 789);
        }
        rebase(&mut lab, &format!("p{i}"), U, 0);
        assert_every_share_held(&lab, &format!("roll {i}"));
    }
    assert_eq!(lab.market().share_epoch, 3);
    let rems =
        |lab: &Lab| -> Vec<u128> { lab.state().records.iter().map(|r| r.conv_rem).collect() };
    let before = rems(&lab);
    lab.claim("A").unwrap();
    let a = lab.position("A").unwrap();
    assert_eq!(a.epoch, 3);
    assert!(
        a.shares > 0,
        "A converted through all three records (no V2-12 jump)"
    );
    assert_ne!(rems(&lab), before, "A's conversion wrote the records");
    assert_every_share_held(&lab, "A converted");
    for step in 1..=3u32 {
        let want = if step == 3 {
            Standing::Current
        } else {
            Standing::Behind
        };
        assert_eq!(lab.sync_signed("B", 1), Ok(want), "step {step}");
        assert_eq!(lab.position("B").unwrap().epoch, step);
        assert_every_share_held(&lab, &format!("B step {step}"));
    }
    lab.claim("K").unwrap();
    let st = lab.state();
    let held: u128 = st.lenders.iter().map(|(_, l)| l.shares).sum();
    assert_eq!(
        held, st.pool.total_shares,
        "everyone converted: no orphan share"
    );
}

/// Spec (5): a dust lender (below one new share) converts to 0 shares and still advances
/// `conv_rem`; the V2-12 jump follows; a later converter collects the unit.
#[test]
fn a_dust_conversion_advances_conv_rem_and_jumps() {
    let mut lab = rebased_pair(TokenProgram::Spl);
    assert_eq!(lab.sync_signed("H", 1), Ok(Standing::Current));
    let h = lab.position("H").unwrap();
    assert_eq!((h.shares, h.epoch), (0, 1));
    assert_eq!(conv_rem(&lab, 0), 600);
    lab.sync_signed("W", 1).unwrap();
    assert_eq!(
        lab.position("W").unwrap().shares,
        7_200,
        "W collects the unit"
    );
    assert_eq!(conv_rem(&lab, 0), 200);
    assert_every_share_held(&lab, "end");
}

/// Spec (6): a record the conversion changes must be writable (`EpochRecordReadOnly`), on every
/// lender instruction that converts (`claim_sol`, `cover_deficit`, `sync_lender`, `deposit`,
/// `withdraw`), and a refusal writes nothing; a read-only record the conversion leaves unchanged (a
/// holding that is a multiple of the unit, with `conv_rem` 0) is accepted.
#[test]
fn a_record_that_needs_writing_must_be_writable() {
    let mut lab = rebased_pair(TokenProgram::Token2022);
    let m = lab.k.market;
    let ro = vec![AccountMeta::new_readonly(pda::share_epoch(&m, 0).0, false)];
    let refused = code(ShortwinError::EpochRecordReadOnly);
    let h = lab.key("H");
    let kp = lab.users["H"].kp.insecure_clone();
    // W (7,199,600 old shares) deposits and withdraws: its conversion changes `conv_rem` too.
    let wk = LenderKeys {
        owner: lab.key("W"),
        mint: lab.k.mint,
        token_program: lab.k.token_program,
        owner_token_account: lab.users["W"].ata,
    };
    let wkp = lab.users["W"].kp.insecure_clone();
    let (records, w) = (lab.state().records, lab.position("W").unwrap());
    for (what, ixn, signer) in [
        ("claim_sol", ix::claim_sol(&h, &m), &kp),
        ("cover_deficit", ix::cover_deficit(&h, &m, 1), &kp),
        ("sync_lender", ix::sync_lender_signed(&h, &m, 1), &kp),
        ("deposit", ix::deposit(&wk, 1_000, 0), &wkp),
        ("withdraw", ix::withdraw(&wk, 1, 0), &wkp),
    ] {
        let res = lab.env.send(&[ix::with_remaining(ixn, &ro)], &[signer]);
        assert_eq!(custom_code(&res), Some(refused), "{what}");
    }
    assert_eq!(lab.state().records, records, "no record written");
    assert_eq!(conv_rem(&lab, 0), 0);
    assert_eq!(lab.position("H").unwrap().epoch, 0, "nothing written");
    assert_eq!(lab.position("W").unwrap(), w, "nothing written");
    // Writable: accepted.
    lab.claim("H").unwrap();
    assert_eq!(conv_rem(&lab, 0), 600);
    // A lender whose conversion leaves `conv_rem` alone (1,000 old shares → 1 new share,
    // remainder 0 → 0) may pass its record read-only.
    let mut lab = Lab::with_cap(TokenProgram::Spl, 10_000);
    assert_eq!(dust(&mut lab, "A", 1, 0), 1_000);
    lab.deposit("B", 1_000_000).unwrap();
    rebase(&mut lab, "p", U, 0);
    let m = lab.k.market;
    let a = lab.key("A");
    let kp = lab.users["A"].kp.insecure_clone();
    let ro = vec![AccountMeta::new_readonly(pda::share_epoch(&m, 0).0, false)];
    let res = lab
        .env
        .send(&[ix::with_remaining(ix::claim_sol(&a, &m), &ro)], &[&kp]);
    assert!(res.is_ok(), "an unchanged record may stay read-only");
    let l = lab.position("A").unwrap();
    assert_eq!((l.shares, l.epoch), (1, 1));
    assert_eq!(conv_rem(&lab, 0), 0);
    assert_every_share_held(&lab, "end");
}

/// Spec (7): a lender with a negative `pending` behind a rebase record: `withdraw` is refused
/// (`NegativePending`, C0-13) and writes nothing; the lender still counts in `S`, so another
/// lender's whole exit is not the last and does not take the vest; `cover_deficit` converts it
/// through the record (writing `conv_rem`), and its own exit is then the last and is paid the
/// vest.
#[test]
fn a_deficit_lender_converts_through_cover_and_blocks_the_flush() {
    let mut lab = Lab::with_cap(TokenProgram::Token2022, 10_000);
    lab.deposit("B", 1_000_000_000_000).unwrap();
    // 90 % lent, not all of it: since AIR-07's residual fix a drained pool takes no deposit,
    // and A must join late, after the rent it then shares the shortfall of.
    lab.borrow30("1", 900_000_000_000).unwrap();
    lab.advance(20 * common::D, 10u128.pow(17));
    dust(&mut lab, "A", 6_000_000_000_000, 321);
    lab.repay("1", 0, 0); // the whole credited rent is debited pro-rata, A included
    rebase(&mut lab, "2", LUMP, LUMP);
    let a = lab.position("A").unwrap();
    assert_eq!(a.epoch, 0);
    let rem0 = conv_rem(&lab, 0);
    let neg = code(ShortwinError::NegativePending);
    assert_eq!(lab.withdraw("A", 1), Err(neg));
    assert_eq!(conv_rem(&lab, 0), rem0, "a refusal writes no record");
    // B leaves whole: A's (latent) shares keep S above B's, so B is not paid the vest.
    let vest = lab.market().vest_bal;
    exit_all(&mut lab, "B");
    assert!(lab.market().total_shares.get() > 0 && lab.market().vest_bal == vest);
    // A covers its deficit (converting through the record), then leaves last with the vest.
    let st = lab.state();
    let mut l = a.lender();
    let mut v = st.vault.clone();
    let mut recs = st.records.clone();
    pool::touch(&st.pool, &mut v, &mut l, &mut recs, st.now).unwrap();
    assert!(l.pending < 0, "A's pending {}", l.pending);
    lab.cover("A", l.pending.unsigned_abs()).unwrap();
    assert_eq!(lab.position("A").unwrap().epoch, 1);
    assert_ne!(conv_rem(&lab, 0), rem0, "cover_deficit wrote the record");
    let lam = wealth(&mut lab, "A");
    exit_all(&mut lab, "A");
    let m = lab.market();
    assert_eq!((m.total_shares.get(), m.vest_bal), (0, 0));
    assert!(wealth(&mut lab, "A") - lam >= vest, "A is paid the vest");
}

/// Spec (8): after the last exit flushed the vest (`S = 0`), a new depositor is a first
/// depositor (virtual shares only, `vest_bal = 0`), with nothing of the old epoch's SOL. Since
/// AIR-08 it bundles the sweep a deposit after the last exit waits for.
#[test]
fn re_entry_after_the_flush_is_a_first_deposit() {
    let mut lab = rebased_pair(TokenProgram::Spl);
    lab.claim("H").unwrap();
    exit_all(&mut lab, "W");
    lab.claim("H").unwrap();
    let m = lab.market();
    assert_eq!((m.total_shares.get(), m.vest_bal), (0, 0));
    let p = m.pool();
    let want = pool::shares_for_tokens(&p, 1_000_000).unwrap();
    assert_eq!(
        lab.deposit("N", 1_000_000),
        Err(code(ShortwinError::WipeSweepPending))
    );
    lab.sweep_sol(0);
    assert_eq!(lab.deposit("N", 1_000_000), Ok(want));
    assert_eq!(lab.market().vest_bal, 0);
    lab.advance(i64::from(lab.lump_vest_s) + 1, 0);
    assert_eq!(lab.claim("N"), Ok(0));
    assert_every_share_held(&lab, "end");
}

/// Spec (9): `claim_sol ×3` in one transaction: the first converts the lender and writes the
/// record; the others write nothing and pay 0 (V2-2). KI-67: H's claim converts its dust to 0
/// shares and, paid in full, closes the position, so a second `claim_sol` of it in the same
/// transaction fails with `AccountNotInitialized` (3012) and the whole transaction with it
/// (nothing converted); W keeps shares, so its `claim_sol ×3` converts once and pays once.
/// The same transaction for H after W's conversion would succeed: H then receives the record's
/// rounding unit and keeps its position.
#[test]
fn claim_sol_three_times_converts_once() {
    let mut lab = rebased_pair(TokenProgram::Token2022);
    lab.advance(i64::from(lab.lump_vest_s) + 1, 0);
    let m = lab.k.market;
    let metas = ix::epoch_records(&m, 0, 1);
    let three = |lab: &mut Lab, name: &str| {
        let owner = lab.key(name);
        let kp = lab.users[name].kp.insecure_clone();
        let one = ix::with_remaining(ix::claim_sol(&owner, &m), &metas);
        lab.env.send(&[one.clone(), one.clone(), one], &[&kp])
    };
    let before = lab.state().records;
    let res = three(&mut lab, "H");
    assert_eq!(
        custom_code(&res),
        Some(u32::from(
            anchor_lang::error::ErrorCode::AccountNotInitialized
        )),
        "the second claim of a closed position"
    );
    assert_eq!(
        lab.state().records,
        before,
        "the failed transaction converted nobody"
    );
    assert!(lab.position("H").is_some());
    let meta = three(&mut lab, "W").expect("claim_sol ×3");
    let paid: Vec<u64> = events::of::<ClaimSol>(&meta)
        .iter()
        .map(|e| e.sol_paid)
        .collect();
    assert_eq!(paid.len(), 3);
    assert!(paid[0] > 0 && paid[1] == 0 && paid[2] == 0, "{paid:?}");
    assert_eq!(conv_rem(&lab, 0), 600, "converted once");
    let before = lab.state().records;
    assert_eq!(lab.claim("W"), Ok(0));
    assert_eq!(
        lab.state().records,
        before,
        "a current lender writes no record"
    );
    // W converted first, so H's conversion now carries the record's unit (600 + 600): it keeps
    // a share and its position.
    assert!(lab.claim("H").unwrap() > 0);
    assert_eq!(lab.position("H").map(|h| h.shares), Some(1));
}

/// Spec (10): the hook against the `store_records` mutant. A lender instruction that converted
/// a lender through a rebase record but did not store the record's `conv_rem` (simulated by
/// restoring the record after the conversion) leaves a share without a holder (or one too
/// many): I1 (`Σ shares = S` once every lender is touched) catches it.
#[test]
fn a_conversion_not_stored_is_caught_by_i1() {
    let mut lab = rebased_pair(TokenProgram::Spl);
    let unconverted = lab.state().records[0];
    lab.claim("H").unwrap();
    assert!(check_all(&lab.env.svm, &lab.k.market).is_ok());
    lab.write_record(&unconverted);
    let v = check_all(&lab.env.svm, &lab.k.market).unwrap_err();
    assert_eq!(v.invariant, "I1", "{v}");
    assert!(v.detail.contains("!= S"), "{v}");
    let (held, s) = held_and_s(&lab);
    assert_eq!((held, s), (7_199, 7_200));
}

// ---- Owner-signed rebase conversions (owner decision 2026-10-07, KI65-REV-SEC1) --------------

/// Only a position's owner converts it through a rebase record: a third party's `sync_lender`
/// that would read one is the pot-only touch (KI-66: the lender accrued to the record's ends,
/// its shares, epoch and the record unchanged; idempotent), an owner that is not the
/// position's is refused by the lender's seeds, and the owner's own signature converts. Without
/// the owner the instruction stays permissionless where no rebase record is read: the parking on
/// a live full-wipe record, the plain catch-up through a wipe record and the V2-12 jump over a
/// later rebase record, and `sync_lender(0)` (a behind lender stays behind; a current one is
/// touched).
#[test]
fn only_the_owner_converts_through_a_rebase_record() {
    let mut lab = Lab::with_cap(TokenProgram::Spl, 10_000);
    lab.deposit("A", 1_000_000_000).unwrap();
    lab.borrow30("p", 1_000_000_000).unwrap();
    lab.settle("p", 5_000_000_000, 0);
    assert_eq!(lab.state().records[0].num, 0, "a full wipe");
    // A parks on the live wipe record, without its owner.
    assert_eq!(lab.sync("A", 1), Ok(Standing::Parked));
    lab.sweep_sol(0);
    assert_eq!(dust(&mut lab, "B", 9_000, 600) % DEN, 600);
    rebase(&mut lab, "q", U + LUMP, LUMP);
    assert_eq!(lab.market().share_epoch, 2);
    // B is behind the rebase record: sync_lender(0) leaves it there, by anyone.
    assert_eq!(lab.sync("B", 0), Ok(Standing::Behind));
    let (records, b) = (lab.state().records, lab.position("B").unwrap());
    assert_eq!(lab.sync("B", 1), Ok(Standing::Behind));
    let held = lab.position("B").unwrap();
    assert_eq!(lab.state().records, records, "no record written");
    assert_eq!(
        (held.shares, held.epoch, held.lock, held.lock_term),
        (b.shares, b.epoch, b.lock, b.lock_term),
        "not converted"
    );
    // KI-66 full: held at record 1 (accrued to its ends) and its forward chain walked to the
    // current epoch on the floor of its shares, `⌊s/den⌋`.
    let den = records[1].den().unwrap();
    let q = b.shares / den;
    assert_eq!(
        (held.fwd, held.fwd_epoch, held.fwd_shares, held.fwd_lag),
        (pool::FWD_HOLD, 2, q, 0),
        "the forward chain"
    );
    let p = lab.market().pool();
    let fl = |x: i128, n: u128| mul_div_i128(x, n, WAD, Rounding::Floor).unwrap();
    let pending = i128::from(b.pending) + fl(records[1].acc_end - b.snap, b.shares) + fl(p.acc, q);
    assert_eq!(
        i128::from(held.pending),
        pending,
        "pending: the hold, then the chain"
    );
    assert_eq!(
        (held.snap, held.snap_r),
        (p.acc, p.acc_r),
        "the chain's snapshots"
    );
    assert_eq!(lab.sync("B", 1), Ok(Standing::Behind));
    assert_eq!(lab.position("B").unwrap(), held, "idempotent");
    // Another signer in the owner's place: the lender's seeds refuse it.
    let m = lab.k.market;
    let (bk, ak) = (lab.key("B"), lab.key("A"));
    let akp = lab.users["A"].kp.insecure_clone();
    let mut forged = ix::sync_lender_signed(&bk, &m, 1);
    for meta in forged.accounts.iter_mut().filter(|x| x.pubkey == bk) {
        meta.pubkey = ak;
    }
    let res = lab.env.send(
        &[ix::with_remaining(forged, &ix::epoch_records(&m, 1, 2))],
        &[&akp],
    );
    assert_eq!(
        custom_code(&res),
        Some(u32::from(anchor_lang::error::ErrorCode::ConstraintSeeds))
    );
    // A, empty after the wipe, converts through it and jumps over the rebase record: anyone.
    assert_eq!(lab.sync("A", 2), Ok(Standing::Current));
    assert_eq!(lab.position("A").unwrap().epoch, 2);
    assert_eq!(conv_rem(&lab, 1), 0, "A read no rebase record");
    // B's owner converts it.
    assert_eq!(lab.sync_signed("B", 1), Ok(Standing::Current));
    assert_eq!(lab.position("B").unwrap().epoch, 2);
    assert_eq!(conv_rem(&lab, 1), 600);
    // A current lender's plain touch, by anyone.
    assert_eq!(lab.sync("B", 0), Ok(Standing::Current));
    assert_every_share_held(&lab, "end");
}

// ---- Conversion order (security review SEC-1): what remains, pinned --------------------------
//
// The order of the conversions through a rebase record decides who receives its rounding
// units (at most `n − 1`, one per position), never pushing a lender below its floor `⌊s/den⌋`.
// Since the owner's decision of 2026-10-07 only a position's owner converts it through a
// rebase record (KI65-REV-SEC1; anyone else's sync is the pot-only touch, KI-66, which converts
// nobody), so the security review's attacks — a third
// party's `sync_lender` of the honest lenders, interleaved with its own Sybils or around a
// tuned residue — are refused, and an honest lender that has not converted keeps its latent
// unit: it counts in `S`, blocks the flush (W3) and is paid the vest when it converts. What
// remains: a party orders its own conversions (Sybil positions placed before the roll, a tuned
// residue) and converts after an honest lender's own conversion (its claim, deposit,
// withdrawal, cover or signed sync) — at any later time while the record's remainder stands, no
// atomicity needed (the delayed test below matches the immediate one to a lamport). Each honest
// conversion that leaves the remainder just below a unit hands that unit to the party's next
// conversion: an honest sub-unit lender loses its latent unit at its first own conversion
// during a running vest, and claiming is the normal action after a settlement, so the lazy
// figures are a best case. Where the honest lenders all converted to 0 shares that way, the
// party holds all of `S` and its last burn is paid the whole unreleased vest and later lumps
// (R3B-2) — a JIT depositor's too, with one Sybil placed before the roll. Before KI-65 that SOL
// was stranded (claimable by nobody). Every figure below is pinned to the lamport on this
// program (the audit docs quote them; a change that moves one must update them).

fn shares(lab: &mut Lab, name: &str) -> u128 {
    lab.position(name).map_or(0, |l| l.shares)
}

/// Honest dust lenders H1, H2 (999 old shares each: 1.998 units together) and an attacker
/// with a whale W and two 1-share Sybils A1, A2 (0.002 units), then a rebase that vests a lump.
fn sybil_market() -> (Lab, u128) {
    let mut lab = Lab::with_cap(TokenProgram::Spl, 10_000);
    assert_eq!(dust(&mut lab, "H1", 1, 999), 999);
    assert_eq!(dust(&mut lab, "H2", 1, 999), 999);
    assert_eq!(dust(&mut lab, "A1", 1, 1), 1);
    assert_eq!(dust(&mut lab, "A2", 1, 1), 1);
    let w = dust(&mut lab, "W", 9_000, 0);
    rebase(&mut lab, "p", U + LUMP, LUMP);
    assert_eq!(
        lab.market().total_shares.get(),
        (999 + 999 + 1 + 1 + w) / DEN
    );
    assert_eq!(lab.market().vest_bal, LUMP);
    (lab, w)
}

/// The SOL `names` receive from now on, after each exits whole and claims, and how many of their
/// positions the exit's own `withdraw` closed (all shares burned, nothing owed, no lock: the
/// closes KI-65's `withdraw` made too, whose rent its pins counted, [`pinned`]).
fn exit_and_claim(lab: &mut Lab, names: &[&str]) -> (u64, u64) {
    let lam: Vec<u64> = names.iter().map(|n| wealth(lab, n)).collect();
    let mut closed = 0;
    for n in names {
        exit_all(lab, n);
        closed += u64::from(lab.position(n).is_none());
        lab.claim(n).unwrap();
    }
    let gain = names
        .iter()
        .zip(&lam)
        .map(|(n, l)| wealth(lab, n) - l)
        .sum();
    (gain, closed)
}

/// The `order_*` figures are SOL received, measured as [`wealth`] (a position's rent is the
/// owner's whether it sits in the position or came back). KI-65's pins measured the owners'
/// lamports alone, so they also counted the rent of the `closed` positions their `withdraw`
/// closed and refunded within the window; since KI-67 `claim_sol` closes the positions a
/// withdrawal left open and returns their rent too, which the lamports alone would add. The pins
/// stay KI-65's: `gain` plus the `closed` positions' rent, with `closed` counted by
/// [`exit_and_claim`] as the scenario runs (the exits whose `withdraw` burned a position's last
/// shares; `withdraw`'s close is KI-65's, `pool::closable` on a current lender), not read off
/// the result. SOL alone is then unchanged from KI-65 exactly when the pin holds.
fn pinned(lab: &Lab, got: (u64, u64), pin: (u64, u64), closed: u64) {
    let rent = lab
        .env
        .svm
        .minimum_balance_for_rent_exemption(LenderPosition::SPACE);
    assert_eq!(rent, 2_568_240);
    assert_eq!(
        (got.0 + closed * rent, got.1),
        pin,
        "pinned (KI-65's figures: SOL plus {closed} position rents; SOL alone {got:?})"
    );
}

/// What `name` owns in lamports: its own and its position account's (the rent `withdraw` and,
/// since KI-67, `claim_sol` return when they close the position, which is the owner's either
/// way; the figures below measure SOL paid, not where the rent sits).
fn wealth(lab: &mut Lab, name: &str) -> u64 {
    let owner = lab.key(name);
    let pos = pda::lender(&lab.k.market, &owner).0;
    lab.lamports_of(name) + shortwin_litesvm_tests::read::lamports(&lab.env.svm, &pos)
}

/// The lamports of `names` together ([`wealth`]).
fn lamports(lab: &mut Lab, names: &[&str]) -> u64 {
    names.iter().map(|n| wealth(lab, n)).sum()
}

/// The `held` lenders' conversions as a third party would order them: its `sync_lender`s are the
/// pot-only touch (KI-66), which converts nobody: no record written, every position at its
/// shares and epoch.
fn third_party_converts_nobody(lab: &mut Lab, held: &[&str]) {
    let records = lab.state().records;
    for n in held {
        let before = lab.position(n).unwrap();
        assert_eq!(lab.sync(n, 1), Ok(Standing::Behind), "{n}");
        let after = lab.position(n).unwrap();
        assert_eq!(
            (after.shares, after.epoch),
            (before.shares, before.epoch),
            "{n}: not converted"
        );
    }
    assert_eq!(lab.state().records, records, "no record written");
}

/// The security review's Sybil interleaving (H1, A1, H2, A2) needed a third party's
/// `sync_lender` of H1 and H2: it converts neither (the pot-only touch). With the honest lenders lazy, the attacker's own
/// conversions, in any order, collect neither of the record's two units: the honest lenders'
/// 1,998 latent old shares (with the Sybils' 2 left in `conv_rem`) keep both, the attacker's
/// last exit is not the last of `S`, and the honest lenders, converting later, hold a share
/// each and are paid the vest.
#[test]
fn order_a_third_party_cannot_interleave_its_sybils_with_honest_lenders() {
    let (mut lab, w) = sybil_market();
    third_party_converts_nobody(&mut lab, &["H1", "H2"]);
    for n in ["A1", "W", "A2"] {
        assert_eq!(lab.sync_signed(n, 1), Ok(Standing::Current), "{n}");
        assert_every_share_held(&lab, n);
    }
    let got: Vec<u128> = ["A1", "A2", "W"]
        .iter()
        .map(|n| shares(&mut lab, n))
        .collect();
    assert_eq!(got, vec![0, 0, w / DEN], "the attacker's own conversions");
    assert_eq!(conv_rem(&lab, 0), 2);
    let (gain, closed) = exit_and_claim(&mut lab, &["A1", "A2", "W"]);
    assert_eq!(
        lab.market().total_shares.get(),
        2,
        "the honest lenders' latent units"
    );
    lab.advance(i64::from(lab.lump_vest_s) + 1, 0);
    let honest = lab.claim("H1").unwrap() + lab.claim("H2").unwrap();
    assert_eq!((shares(&mut lab, "H1"), shares(&mut lab, "H2")), (1, 1));
    assert!(
        gain < LUMP && honest >= LUMP,
        "attacker {gain}, honest {honest}"
    );
    assert!(lab.vault().lender_sol <= 10, "nothing stranded");
    eprintln!("Sybils, honest lenders lazy: attacker {gain} lamports, honest {honest}");
    pinned(&lab, (gain, honest), (1_002_198_376, 3_000_369_862), closed);
}

/// What remains: the attacker converts one of its Sybils (signed by itself) after each honest
/// lender's own conversion (here its `claim_sol`, as the web's claim converts). H1's
/// claim leaves the remainder at 999 (H1 at its floor, 0 shares) and A1's 1 share collects the
/// unit; so again for H2 and A2. The attacker holds all of `S`, and its last exit is paid the
/// whole running vest: the security review's interleaving figures, without its third-party
/// syncs. With both honest claims first and the Sybils after, H2 keeps a unit, the attacker's
/// exit is not the last, and H2 is paid the vest.
#[test]
fn order_sybils_converting_after_honest_conversions_collect_every_unit() {
    let (mut lab, w) = sybil_market();
    let honest0 = lamports(&mut lab, &["H1", "H2"]);
    lab.claim("H1").unwrap();
    assert_eq!(lab.sync_signed("A1", 1), Ok(Standing::Current));
    lab.claim("H2").unwrap();
    for n in ["A2", "W"] {
        assert_eq!(lab.sync_signed(n, 1), Ok(Standing::Current), "{n}");
    }
    assert_every_share_held(&lab, "after the honest claims");
    let got: Vec<u128> = ["H1", "H2", "A1", "A2", "W"]
        .iter()
        .map(|n| shares(&mut lab, n))
        .collect();
    assert_eq!(
        got,
        vec![0, 0, 1, 1, w / DEN],
        "Sybils after the honest claims"
    );
    let (gain, closed) = exit_and_claim(&mut lab, &["A1", "A2", "W"]);
    assert_eq!(
        lab.market().total_shares.get(),
        0,
        "the attacker's last burn took S to 0"
    );
    assert!(
        gain >= LUMP,
        "the attacker's exits are paid the vest: {gain}"
    );
    lab.advance(i64::from(lab.lump_vest_s) + 1, 0);
    lab.claim("H1").unwrap();
    lab.claim("H2").unwrap();
    let honest = lamports(&mut lab, &["H1", "H2"]) - honest0;
    assert!(
        honest < U / 1_000,
        "the honest lenders get none of it: {honest}"
    );
    assert!(lab.vault().lender_sol <= 10, "nothing stranded");
    eprintln!("Sybils converting after honest claims: attacker {gain} lamports, honest {honest}");
    pinned(&lab, (gain, honest), (4_007_334_855, 369_862), closed);

    // Both honest claims first: H2 keeps a unit and the attacker's exit is not the last.
    let (mut lab, w) = sybil_market();
    lab.claim("H1").unwrap();
    lab.claim("H2").unwrap();
    for n in ["A1", "A2", "W"] {
        assert_eq!(lab.sync_signed(n, 1), Ok(Standing::Current), "{n}");
    }
    let got: Vec<u128> = ["H1", "H2", "A1", "A2", "W"]
        .iter()
        .map(|n| shares(&mut lab, n))
        .collect();
    assert_eq!(got, vec![0, 1, 0, 1, w / DEN], "honest-first order");
    let (gain, closed) = exit_and_claim(&mut lab, &["A1", "A2", "W"]);
    assert_eq!(lab.market().total_shares.get(), 1, "H2's unit");
    lab.advance(i64::from(lab.lump_vest_s) + 1, 0);
    let h2 = lab.claim("H2").unwrap();
    assert!(gain < LUMP && h2 >= LUMP, "attacker {gain}, H2 {h2}");
    eprintln!("honest claims first: attacker {gain} lamports, H2 {h2}");
    pinned(&lab, (gain, h2), (1_004_766_616, 3_000_000_000), closed);
}

/// The later conversion needs no atomicity (the security review of the follow-up): the
/// attacker converts each Sybil an hour after the honest lender's claim — the record's remainder
/// waits at 999 in between, and nothing an honest lender does short of converting again moves it
/// — and collects both units and the vest as in the immediate order above, while the honest
/// lenders, who claimed within the vest, get none of it. An honest sub-unit lender's latent
/// unit lasts only until its own first conversion during the vest.
#[test]
fn order_the_later_conversion_needs_no_atomicity() {
    let (mut lab, w) = sybil_market();
    let honest0 = lamports(&mut lab, &["H1", "H2"]);
    lab.claim("H1").unwrap();
    assert_eq!(
        conv_rem(&lab, 0),
        999,
        "H1 at its floor, the remainder below a unit"
    );
    lab.advance(3_600, 0);
    assert_eq!(lab.sync_signed("A1", 1), Ok(Standing::Current));
    lab.claim("H2").unwrap();
    lab.advance(3_600, 0);
    for n in ["A2", "W"] {
        assert_eq!(lab.sync_signed(n, 1), Ok(Standing::Current), "{n}");
    }
    let got: Vec<u128> = ["H1", "H2", "A1", "A2", "W"]
        .iter()
        .map(|n| shares(&mut lab, n))
        .collect();
    assert_eq!(
        got,
        vec![0, 0, 1, 1, w / DEN],
        "an hour after each honest claim"
    );
    let (gain, closed) = exit_and_claim(&mut lab, &["A1", "A2", "W"]);
    assert_eq!(lab.market().total_shares.get(), 0);
    lab.advance(i64::from(lab.lump_vest_s) + 1, 0);
    lab.claim("H1").unwrap();
    lab.claim("H2").unwrap();
    let honest = lamports(&mut lab, &["H1", "H2"]) - honest0;
    assert!(
        gain >= LUMP && honest < U / 1_000,
        "attacker {gain}, honest {honest}"
    );
    assert!(lab.vault().lender_sol <= 10, "nothing stranded");
    eprintln!(
        "Sybils converting an hour after each honest claim: attacker {gain}, honest {honest}"
    );
    pinned(&lab, (gain, honest), (4_007_334_854, 369_862), closed);
}

/// Without Sybils: a whale that tunes its own residue (`s_W ≡ 1 mod 1,000`). The security
/// review's order (H1, W, H2 by a third party) converts neither H (the pot-only touch). With the
/// honest lenders lazy, W's
/// own conversion floors (`⌊s_W/1,000⌋`, remainder 1), the honest 1,998 old shares keep the
/// record's unit, W's exit is not the last and the honest lenders are paid the vest. Converting
/// after H1's own claim (remainder 999), W collects the unit and holds all of `S` — H2's
/// 999 latent old shares no longer make a unit — and its exit is paid the running vest.
#[test]
fn order_a_residue_tuned_whale_collects_the_unit_only_after_an_honest_conversion() {
    let tuned = || {
        let mut lab = Lab::with_cap(TokenProgram::Spl, 10_000);
        assert_eq!(dust(&mut lab, "H1", 1, 999), 999);
        assert_eq!(dust(&mut lab, "H2", 1, 999), 999);
        let w = dust(&mut lab, "W", 9_000, 1);
        rebase(&mut lab, "p", U + LUMP, LUMP);
        (lab, w)
    };
    let (mut lab, w) = tuned();
    third_party_converts_nobody(&mut lab, &["H1", "H2"]);
    assert_eq!(lab.sync_signed("W", 1), Ok(Standing::Current));
    assert_eq!(shares(&mut lab, "W"), w / DEN);
    assert_eq!(lab.market().total_shares.get(), w / DEN + 1);
    let (gain, closed) = exit_and_claim(&mut lab, &["W"]);
    lab.advance(i64::from(lab.lump_vest_s) + 1, 0);
    let honest = lab.claim("H1").unwrap() + lab.claim("H2").unwrap();
    assert!(gain < LUMP && honest >= LUMP, "W {gain}, honest {honest}");
    assert!(lab.vault().lender_sol <= 10);
    eprintln!("residue-tuned whale, honest lenders lazy: W {gain} lamports, honest {honest}");
    pinned(&lab, (gain, honest), (1_002_346_116, 3_000_222_122), closed);

    // W converts after H1's own claim.
    let (mut lab, w) = tuned();
    let honest0 = lamports(&mut lab, &["H1", "H2"]);
    lab.claim("H1").unwrap();
    assert_eq!(lab.sync_signed("W", 1), Ok(Standing::Current));
    assert_eq!(shares(&mut lab, "W"), w / DEN + 1);
    assert_eq!(shares(&mut lab, "W"), lab.market().total_shares.get());
    let (gain, closed) = exit_and_claim(&mut lab, &["W"]);
    assert!(gain >= LUMP, "W's exit is the last: {gain}");
    lab.advance(i64::from(lab.lump_vest_s) + 1, 0);
    lab.claim("H1").unwrap();
    lab.claim("H2").unwrap();
    let honest = lamports(&mut lab, &["H1", "H2"]) - honest0;
    assert_eq!(shares(&mut lab, "H2"), 0, "H2 at its floor");
    assert!(
        honest < U / 1_000,
        "the honest lenders get none of the vest: {honest}"
    );
    assert!(lab.vault().lender_sol <= 10);
    eprintln!(
        "residue-tuned whale converting after H1's claim: W {gain} lamports, honest {honest}"
    );
    pinned(&lab, (gain, honest), (4_002_346_115, 222_122), closed);
}

/// W3 with one Sybil placed before the roll: H (600 old shares), the attacker's dust position A
/// (400) and an honest whale G. A third party's `sync_lender` of H converts nothing (the pot-only
/// touch). While H stays
/// lazy, A's own conversion floors to 0 (remainder 400), H's 600 latent old shares hold the
/// record's unit, and after G leaves, a JIT depositor J around a 3 SOL sweep realises 0; H,
/// converting later, is paid both vests. Converting after H's own claim (H at 0 shares,
/// remainder 600), A collects the unit; G leaves; then J's exit takes `S` to 0 with no time passing and
/// is paid the whole lump (R3B-2), while H keeps a position with 0 shares.
#[test]
fn order_w3_with_a_sybil_the_jit_depositor_is_paid_the_lump_only_after_hs_conversion() {
    let w3 = || {
        let mut lab = Lab::with_cap(TokenProgram::Spl, 10_000);
        assert_eq!(dust(&mut lab, "H", 1, 600), 600);
        assert_eq!(dust(&mut lab, "A", 1, 400), 400);
        let g = dust(&mut lab, "G", 9_000, 0);
        rebase(&mut lab, "p", U + LUMP, LUMP);
        assert_eq!(lab.market().total_shares.get(), (600 + 400 + g) / DEN);
        lab
    };
    // H lazy.
    let mut lab = w3();
    third_party_converts_nobody(&mut lab, &["H"]);
    for n in ["A", "G"] {
        assert_eq!(lab.sync_signed(n, 1), Ok(Standing::Current), "{n}");
    }
    assert_eq!(shares(&mut lab, "A"), 0);
    let _ = exit_and_claim(&mut lab, &["G"]);
    assert_eq!(lab.market().total_shares.get(), 1, "H's latent unit");
    let lam = wealth(&mut lab, "J");
    lab.deposit("J", 1_000_000).unwrap();
    lab.sweep_sol(LUMP);
    let _ = exit_and_claim(&mut lab, &["A", "J"]);
    let gain = wealth(&mut lab, "J") - lam;
    assert_eq!(gain, 0, "J realises 0 (its position's rent back)");
    assert_eq!(lab.market().total_shares.get(), 1);
    lab.advance(i64::from(lab.lump_vest_s) + 1, 0);
    let h = lab.claim("H").unwrap();
    assert!(h >= 2 * LUMP, "H is paid both vests: {h}");
    assert_eq!(shares(&mut lab, "H"), 1);
    eprintln!("W3 with a Sybil, H lazy: J {gain} lamports, H {h}");
    assert_eq!(h, 6_000_099_983, "pinned");

    // A converts after H's own claim.
    let mut lab = w3();
    lab.claim("H").unwrap();
    for n in ["A", "G"] {
        assert_eq!(lab.sync_signed(n, 1), Ok(Standing::Current), "{n}");
    }
    assert_eq!((shares(&mut lab, "H"), shares(&mut lab, "A")), (0, 1));
    let _ = exit_and_claim(&mut lab, &["G"]);
    assert_eq!(lab.market().total_shares.get(), 1, "A's unit");
    // A collects the first vest as the only holder; then the JIT round, at one instant.
    lab.advance(i64::from(lab.lump_vest_s) + 1, 0);
    lab.claim("A").unwrap();
    let lam = wealth(&mut lab, "J");
    lab.deposit("J", 1_000_000).unwrap();
    lab.sweep_sol(LUMP);
    let _ = exit_and_claim(&mut lab, &["A"]);
    let _ = exit_and_claim(&mut lab, &["J"]);
    let gain = wealth(&mut lab, "J") - lam;
    assert_eq!(lab.market().total_shares.get(), 0);
    assert!(gain >= LUMP - 10, "J is paid the lump: {gain}");
    eprintln!("W3 with a Sybil converting after H's claim: J {gain} lamports of a {LUMP} lump");
    assert_eq!(gain, LUMP, "pinned");
    assert_eq!(lab.position("H").map_or(0, |h| h.shares), 0);
}

// ---- The pot and a surplus holder that never returns (KI-66) --------------------------------

/// The pot (R3A-2) is filled only by a touch of a lender holding realised SOL beyond its
/// entitlement, and a third party's `sync_lender` is how a surplus holder that never returns
/// gets touched (ECON R3A-2, ARCH I-P6). Behind a rebase record that sync may not convert it
/// (KI65-REV-SEC1), so since KI-66 it is the pot-only touch: `lender_it`'s
/// `the_pot_connects_early_leavers_and_late_joiners` (A leaves early, C joins late and holds the
/// surplus) with a rebase rolled before anyone touched C. `sync_lender(0)` passes no record and
/// leaves C as it is; a third party's `sync_lender(1)` accrues C to the record's ends and moves
/// its surplus to the pot without converting C (shares, epoch, the record's `conv_rem`), a
/// second one changes nothing, and A's next claim is paid in full — C's owner never signed. When
/// C's owner returns, its conversion counts nothing twice: C is paid what it would have been had
/// it converted first (`pool_model.pot_touch_demo` pins the same figures). Before KI-66 (the
/// KI-65 follow-up) A stayed short 2,827,173,914 lamports until C's owner returned.
#[test]
fn a_surplus_behind_a_rebase_record_reaches_the_pot_without_its_owner() {
    let mut lab = Lab::with_cap(TokenProgram::Spl, 10_000);
    lab.deposit("A", 1_000_000_000_000).unwrap();
    lab.deposit("B", 1_000_000_000_000).unwrap();
    lab.borrow30("p", 900_000_000_000).unwrap();
    lab.advance(10 * common::D, 10u128.pow(16));
    let a = lab.free_allowance("A");
    let s = lab.shares_for("A", a);
    lab.withdraw("A", s).unwrap();
    lab.deposit("C", 2_000_000_000_000).unwrap();
    let owed = lab.owed("p");
    lab.repay("p", owed, 0);
    let first = lab.claim("A").unwrap();
    let short = lab.position("A").unwrap().pending;
    assert!(
        short > 0,
        "A's entitlement is not realised on its own position"
    );
    // C's surplus, had anyone touched it before the roll (on a copy).
    let st = lab.state();
    let mut v = *st.vault;
    let mut c = st.lender_of(&lab.key("C")).unwrap().lender();
    let mut recs = st.records.clone();
    let from = PoolState::records_from_mut(&mut recs, c.epoch);
    pool::touch(&st.pool, &mut v, &mut c, from, st.now).unwrap();
    let surplus = v.unmatched - st.vault.unmatched;
    assert!(surplus > 0);

    rebase(&mut lab, "q", U, 0);
    assert_eq!(lab.sync("C", 0), Ok(Standing::Behind));
    assert_eq!(lab.vault().unmatched, 0, "sync_lender(0) passes no record");
    let c0 = lab.position("C").unwrap();
    // A third party's sync of C: the pot-only touch.
    assert_eq!(lab.sync("C", 1), Ok(Standing::Behind));
    let pot = lab.vault().unmatched;
    let c1 = lab.position("C").unwrap();
    assert_eq!(
        (c1.shares, c1.epoch, c1.lock, c1.lock_term),
        (c0.shares, c0.epoch, c0.lock, c0.lock_term),
        "C is not converted"
    );
    assert_eq!(conv_rem(&lab, 0), 0, "no record written");
    assert!(
        pot >= surplus,
        "C's surplus reached the pot: {pot} vs {surplus}"
    );
    assert_eq!(lab.sync("C", 1), Ok(Standing::Behind));
    assert_eq!(
        (lab.vault().unmatched, lab.position("C").unwrap()),
        (pot, c1),
        "idempotent"
    );
    // A's next claim: its share of the rebase's settlement and its whole shortfall.
    let second = lab.claim("A").unwrap();
    let left = lab.position("A").unwrap().pending;
    assert_eq!(left, 0, "A is paid in full without C's owner");
    // C's owner returns: nothing counted twice.
    assert_eq!(lab.sync_signed("C", 1), Ok(Standing::Current));
    let c_shares = lab.position("C").unwrap().shares;
    let c_paid = lab.claim("C").unwrap();
    eprintln!(
        "pot liveness behind a rebase (KI-66): A claimed {first}, short {short}; C's surplus \
         {surplus} (before the roll), pot after a third party's pot-only touch {pot}; A's next \
         claim {second}, left {left}; C converted later: {c_shares} shares, claims {c_paid}"
    );
    assert_eq!(
        (first, short, surplus, pot, second, c_shares, c_paid),
        (
            997_826_086,
            2_827_173_914,
            4_434_782_608,
            4_434_782_609,
            2_957_608_696,
            2_000,
            579_710_144
        ),
        "pinned (pool_model.pot_touch_demo: the same figures)"
    );
    assert_every_share_held(&lab, "end");
}

/// `pool_model._chain_pair`: A and C lend 1,000 tokens each; a settlement of everything but
/// 1,000 raw tokens (1 SOL) rebases the shares before anyone touched C; A's owner converts; in
/// the new epoch 90 % of the tokens are borrowed and charged 10²⁵ of rent index over 10 days
/// (credited to A and to C's unconverted shares), A withdraws its allowance (an early leaver)
/// and the rent is paid, realised on the shares still there — C's among them, beyond C's
/// entitlement — while A is short. Returns the lab and the SOL A's withdrawal paid at once.
fn chain_pair() -> (Lab, u64) {
    let mut lab = Lab::with_cap(TokenProgram::Spl, 10_000);
    lab.deposit("A", 1_000_000_000_000).unwrap();
    lab.deposit("C", 1_000_000_000_000).unwrap();
    let t = lab.market().tokens_free;
    lab.borrow30("p", t - 1_000).unwrap();
    lab.settle("p", U, 0);
    assert_eq!(lab.market().share_epoch, 1);
    assert_eq!(lab.state().records[0].num, 1, "a rebase");
    lab.touch("A");
    let t = lab.market().tokens_free;
    lab.borrow30("q", t * 9 / 10).unwrap();
    lab.advance(10 * common::D, 10u128.pow(25));
    let a = lab.free_allowance("A");
    let s = lab.shares_for("A", a);
    let (_, paid) = lab.withdraw("A", s).unwrap();
    let owed = lab.owed("q");
    lab.repay("q", owed, 0);
    (lab, paid)
}

/// KI-66 full (owner decision of 2026-10-08, KI66-R3): what an absent lender's unconverted shares
/// earn AFTER the rebase record reaches the pot without its owner, through the forward chain
/// (`pool_model.pot_chain_demo` pins the same figures). C's surplus is all of the new epoch: the
/// seventh round's hold (accrual to the record's ends only) moved nothing here, so A stayed short
/// 201,209,837 lamports until C's owner returned — the row "owners only" below, which is what the
/// held-only touch left. A third party's `sync_lender(1)` now walks C's chain (`⌊s/den⌋` shares
/// through the record) to the current epoch and moves the surplus: A is paid in full (1 lamport
/// of rounding, as when C's owner converts first); a second sync moves nothing; C's owner's
/// later conversion counts nothing twice. Every row ends in the same state.
#[test]
fn a_surplus_after_the_rebase_record_reaches_the_pot_through_the_forward_chain() {
    // (pot after C's sync, C's fwd, A's claim, A's pending left, C's shares, C's claim,
    //  lender_sol left)
    let row = |case: &str| -> (u64, u8, u64, i64, u128, u64, u64) {
        let (mut lab, early) = chain_pair();
        let c0 = lab.position("C").unwrap();
        let rem0 = conv_rem(&lab, 0);
        match case {
            "owners only" => {}
            "owner first" => assert_eq!(lab.sync_signed("C", 1), Ok(Standing::Current)),
            _ => {
                assert_eq!(lab.sync("C", 1), Ok(Standing::Behind));
                let c1 = lab.position("C").unwrap();
                assert_eq!(
                    (
                        c1.shares,
                        c1.epoch,
                        c1.lock,
                        c1.lock_term,
                        conv_rem(&lab, 0)
                    ),
                    (c0.shares, c0.epoch, c0.lock, c0.lock_term, rem0),
                    "C is not converted, no record written"
                );
                assert_eq!(
                    (c1.fwd, c1.fwd_epoch, c1.fwd_shares),
                    (
                        pool::FWD_HOLD,
                        1,
                        c0.shares / lab.state().records[0].den().unwrap()
                    )
                );
                if case == "twice" {
                    let pot = lab.vault().unmatched;
                    assert_eq!(lab.sync("C", 1), Ok(Standing::Behind));
                    assert_eq!(
                        (lab.vault().unmatched, lab.position("C").unwrap()),
                        (pot, c1),
                        "a second pot-only touch moves nothing"
                    );
                }
            }
        }
        let (pot, fwd) = (lab.vault().unmatched, lab.position("C").unwrap().fwd);
        // The program's withdraw paid A's SOL part at once (`pool_model`'s pays it here).
        let a = early + lab.claim("A").unwrap();
        let left = lab.position("A").unwrap().pending;
        assert_eq!(
            lab.sync_signed("C", 1).map(|_| ()),
            Ok(()),
            "C's owner returns"
        );
        let c = lab.position("C").unwrap();
        assert_eq!(c.fwd, pool::FWD_NONE);
        let c_paid = lab.claim("C").unwrap();
        lab.claim("A").unwrap();
        (pot, fwd, a, left, c.shares, c_paid, lab.vault().lender_sol)
    };
    let rows: Vec<_> = ["owners only", "chain", "twice", "owner first"]
        .iter()
        .map(|c| (*c, row(c)))
        .collect();
    for (c, r) in &rows {
        eprintln!("KI-66 full, the forward chain: {c}: {r:?}");
    }
    let end = |r: &(u64, u8, u64, i64, u128, u64, u64)| (r.4, r.5, r.6);
    assert!(
        rows.iter().all(|(_, r)| end(r) == end(&rows[0].1)),
        "the same end"
    );
    assert_eq!(
        rows.iter()
            .map(|(_, r)| (r.0, r.1, r.2, r.3))
            .collect::<Vec<_>>(),
        vec![
            (0, 0, 4_123_790_163, 201_209_837),
            (201_209_836, 1, 4_324_999_999, 1),
            (201_209_836, 1, 4_324_999_999, 1),
            (201_209_836, 0, 4_324_999_999, 1),
        ],
        "pinned (pool_model.pot_chain_demo)"
    );
    assert_eq!(end(&rows[0].1), (1_000_000, 4_325_000_000, 1), "pinned");
}
