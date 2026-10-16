//! KI-66 full / KI-67 security review: adversarial tests of the forward chain (a third party's
//! `sync_lender` of a lender behind a rebase record walks it) and of `claim_sol`'s close. Every
//! lender instruction runs through the lender lab (`lender_it/common.rs`): checked against the
//! math crate on its pre-state, with the hooks (I8's stored chain state and close rule among
//! them) after every transaction and every prefix of a bundle.
//!
//! - Random griefing walks: seeded third-party syncs of two absent lenders (a holder whose
//!   residue makes the owner's later conversion carry a unit, so `fwd_lag` is 1 at times, and a
//!   dust holder whose chain floors to 0 shares) with random record slices (before, at and after
//!   `fwd_epoch`, writable) and `max_epochs` 0–8, interleaved with the owner's partial signed
//!   conversions, other lenders' claims, rent and three `1/1,000` rebases: no third-party sync
//!   changes a record byte or the lender's shares, epoch or lock fields, and the wind-down pays
//!   every lender what the market without the third parties pays, to the lamport.
//! - The owner stopping at its chain's own epoch below the current one (`FWD_OPEN` with
//!   `epoch == fwd_epoch < share_epoch`, the judge's attack 1) while a third party walks past it,
//!   with a lag of 0 and of 1.
//! - The claim-time auto-touch's shape abused: one transaction repeating the same unsigned sync
//!   with read-only, writable and wider record slices before the claim pays exactly what one
//!   sync and the claim pay.
//! - KI-67: a chained dust position closes by its claim and is re-created in the same
//!   transaction (`[claim_sol, deposit]`) with no chain left; a third party's sync of the new
//!   position is a plain touch.
//!
//! Requires the SBF build: `anchor build --tools-version v1.57 --arch v3`.

#![allow(clippy::result_large_err)]

#[allow(dead_code)]
#[path = "lender_it/common.rs"]
mod common;

use anchor_lang::solana_program::instruction::AccountMeta;
use common::Lab;
use shortwin_litesvm_tests::ix::{self, LenderKeys};
use shortwin_litesvm_tests::tokens::TokenProgram;
use shortwin_litesvm_tests::{events, pda, read};
use shortwin_math::pool::{self, Standing, VIRTUAL_ASSETS, VIRTUAL_SHARES};
use solana_signer::Signer;

const U: u64 = 1_000_000_000;

// ---- helpers (as `ki66_sec_it`) -------------------------------------------------------------

fn exit_all(lab: &mut Lab, name: &str) {
    loop {
        let Some(l) = lab.touch(name) else { return };
        let p = lab.market().pool();
        if l.shares == 0 || l.epoch != p.share_epoch {
            return;
        }
        let allow = u128::from(lab.free_allowance(name));
        let c = u128::from(p.tokens_free) + u128::from(p.borrowed_q) + VIRTUAL_ASSETS;
        let svs = p.total_shares + VIRTUAL_SHARES;
        let zero = svs.div_ceil(c) - 1;
        let fit = allow * svs / c;
        let s = l.shares.min(fit.max(zero).max(1));
        lab.withdraw(name, s).unwrap();
    }
}

/// Every lender exits whole and claims, then two more claim passes. Returns each lender's
/// lamports (plus its position account's) gained since `base`.
fn wind_down(lab: &mut Lab, names: &[&str], base: &[u64]) -> Vec<u64> {
    for n in names {
        exit_all(lab, n);
        lab.claim(n).unwrap();
    }
    for _ in 0..2 {
        for n in names {
            lab.claim(n).unwrap();
        }
    }
    names
        .iter()
        .zip(base)
        .map(|(n, b)| i128::from(wealth(lab, n)) - i128::from(*b))
        .map(|g| u64::try_from(g.max(0)).expect("gain"))
        .collect()
}

fn wealth(lab: &mut Lab, name: &str) -> u64 {
    let owner = lab.key(name);
    let pos = pda::lender(&lab.k.market, &owner).0;
    lab.lamports_of(name) + read::lamports(&lab.env.svm, &pos)
}

fn lamports_of(lab: &mut Lab, names: &[&str]) -> Vec<u64> {
    names.iter().map(|n| wealth(lab, n)).collect()
}

/// `name` deposits `amount` tokens, then burns (for 0 tokens) what leaves its shares at `rem`
/// modulo 1,000.
fn dust(lab: &mut Lab, name: &str, amount: u64, rem: u128) -> u128 {
    lab.deposit(name, amount).unwrap();
    let s = lab.position(name).unwrap().shares;
    let extra = (s % 1_000 + 1_000 - rem) % 1_000;
    if extra > 0 {
        assert_eq!(
            lab.withdraw(name, extra).map(|(t, _)| t),
            Ok(0),
            "a 0-token burn"
        );
    }
    lab.position(name).unwrap().shares
}

/// A settlement that leaves about `S / (2·R_MAX)` raw tokens: a rebase by `1/1,000` (k = 1).
fn rebase_k1(lab: &mut Lab, pid: &str, sol: u64) {
    let e = lab.market().share_epoch;
    let p = lab.market().pool();
    let keep = u64::try_from((p.total_shares / (2 * pool::R_MAX)).max(1)).unwrap();
    lab.borrow30(pid, p.tokens_free - keep).unwrap();
    lab.settle(pid, sol, 0);
    assert_eq!(lab.market().share_epoch, e + 1, "{pid}: the epoch rolled");
    let r = *lab.state().records.last().unwrap();
    assert_eq!((r.num, r.den_log1000), (1, 1), "{pid}: a 1/1,000 rebase");
}

/// Rent in the current epoch with an early leaver (`ki66_sec_it::rent_with_leaver`).
fn rent_with_leaver(lab: &mut Lab, pid: &str, leaver: &str, days: i64, d_index: u128) {
    let t = lab.market().tokens_free;
    lab.borrow30(pid, t / 2).unwrap();
    lab.advance(days * common::D, d_index);
    let a = lab.free_allowance(leaver);
    let s = lab.shares_for(leaver, a);
    if s > 0 {
        lab.withdraw(leaver, s).unwrap();
    }
    let owed = lab.owed(pid);
    lab.repay(pid, owed, 0);
}

/// Every `ShareEpoch` account's bytes.
fn record_bytes(lab: &Lab) -> Vec<Vec<u8>> {
    let m = lab.k.market;
    (0..lab.market().share_epoch)
        .map(|e| {
            lab.env
                .svm
                .get_account(&pda::share_epoch(&m, e).0)
                .unwrap()
                .data
        })
        .collect()
}

/// What a third party must never move: the lender's conversion state.
fn order_fields(p: &shortwin::state::LenderPosition) -> (u128, u32, u128, u128, i64, u32) {
    (
        p.shares,
        p.epoch,
        p.lock,
        p.lock_term,
        p.lock_until,
        p.lock_epoch,
    )
}

/// xorshift64.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

// ---- random griefing walks ----------------------------------------------------------------

/// One run of the griefing scenario (module docs). The random draws are the same with and
/// without third parties (drawn, then applied only with them), so the owners' actions match.
/// Returns the wind-down's gains, `lender_sol`, the pot, and how many third-party syncs ran
/// and how many of them found the target chained (`FWD_HOLD` / `FWD_OPEN`).
fn griefed(seed: u64, third_party: bool) -> (Vec<u64>, u64, u64, (u32, u32, u32)) {
    let names = ["B", "C", "D", "H", "L1", "L2", "L3"];
    let mut rng = Rng(0x9e37_79b9_7f4a_7c15 ^ seed.wrapping_mul(0x2545_f491_4f6c_dd1d));
    let mut lab = Lab::with_cap(TokenProgram::Spl, 10_000);
    let base = lamports_of(&mut lab, &names);
    dust(
        &mut lab,
        "H",
        U,
        if seed.is_multiple_of(2) { 999 } else { 0 },
    );
    assert_eq!(dust(&mut lab, "C", 1_000 * U, 600) % 1_000, 600);
    dust(&mut lab, "D", 1, 600);
    dust(&mut lab, "B", 1_000 * U, 0);
    let mut stats = (0u32, 0u32, 0u32);
    // One third-party sync of C or D with a random slice, or nothing.
    let grief = |lab: &mut Lab, rng: &mut Rng, stats: &mut (u32, u32, u32)| {
        let (who, from_draw, n) = (rng.next() % 3, rng.next() % 5, rng.next() % 9);
        let target = match who {
            0 => "C",
            1 => "D",
            _ => return,
        };
        if !third_party {
            return;
        }
        let Some(p0) = lab.position(target) else {
            return;
        };
        let start = if p0.fwd == pool::FWD_NONE {
            p0.epoch
        } else {
            p0.fwd_epoch
        };
        // Before (−2, −1), at (0) or after (+1, +2) where the walk starts.
        let from = (i64::from(start) + i64::try_from(from_draw).unwrap() - 2).max(0);
        let from = u32::try_from(from).unwrap();
        let bytes = record_bytes(lab);
        let pot = lab.vault().unmatched;
        let n = u8::try_from(n).unwrap();
        let st = lab
            .sync_from(target, from, n)
            .expect("a third party's sync");
        let p1 = lab.position(target).unwrap();
        stats.0 += 1;
        if p1.fwd == pool::FWD_HOLD {
            stats.1 += 1;
        }
        if p1.fwd == pool::FWD_OPEN {
            stats.2 += 1;
        }
        assert_eq!(record_bytes(lab), bytes, "{target}: no record written");
        assert_eq!(
            order_fields(&p1),
            order_fields(&p0),
            "{target}: no conversion"
        );
        assert!(lab.vault().unmatched >= pot, "the pot only grows");
        if p1.epoch < lab.market().share_epoch && (p1.shares, p1.lock) != (0, 0) {
            assert_eq!(st, Standing::Behind);
        }
        // A second identical sync moves nothing.
        assert_eq!(lab.sync_from(target, from, n), Ok(st));
        assert_eq!(lab.position(target).unwrap(), p1, "{target}: idempotent");
    };
    for (k, (leaver, pid)) in [("L1", "r1"), ("L2", "r2"), ("L3", "r3")]
        .into_iter()
        .enumerate()
    {
        lab.deposit(leaver, 1_000 * U).unwrap();
        grief(&mut lab, &mut rng, &mut stats);
        rebase_k1(&mut lab, &format!("p{k}"), U);
        grief(&mut lab, &mut rng, &mut stats);
        if rng.next().is_multiple_of(2) {
            lab.claim("H").unwrap();
        }
        grief(&mut lab, &mut rng, &mut stats);
        rent_with_leaver(&mut lab, pid, leaver, 2, 10u128.pow(19));
        grief(&mut lab, &mut rng, &mut stats);
        // The owner of C converts one record now and then (FWD_OPEN when chained).
        if !rng.next().is_multiple_of(3) {
            lab.sync_signed("C", 1).unwrap();
        }
        grief(&mut lab, &mut rng, &mut stats);
        if rng.next().is_multiple_of(2) {
            lab.claim("B").unwrap();
        }
        grief(&mut lab, &mut rng, &mut stats);
    }
    // Rent in the last epoch, then the owners come back one at a time.
    let t = lab.market().tokens_free;
    lab.borrow30("r4", t / 2).unwrap();
    lab.advance(2 * common::D, 10u128.pow(19));
    grief(&mut lab, &mut rng, &mut stats);
    let owed = lab.owed("r4");
    lab.repay("r4", owed, 0);
    grief(&mut lab, &mut rng, &mut stats);
    lab.claim("B").unwrap();
    lab.claim("H").unwrap();
    let g = wind_down(&mut lab, &names, &base);
    let v = lab.vault();
    (g, v.lender_sol, v.unmatched, stats)
}

/// Random griefing walks change no order and pay nobody twice (module docs).
#[test]
fn random_third_party_walks_order_nothing_and_pay_nobody_twice() {
    let mut total = (0u32, 0u32, 0u32);
    for seed in 0..12u64 {
        let (gx, lx, px, _) = griefed(seed, false);
        let (gy, ly, py, s) = griefed(seed, true);
        eprintln!(
            "seed {seed}: owners alone {gx:?} lender_sol {lx} pot {px}; griefed {gy:?} \
             lender_sol {ly} pot {py}; syncs {s:?}"
        );
        assert_eq!((gx, lx, px), (gy, ly, py), "seed {seed}");
        total = (total.0 + s.0, total.1 + s.1, total.2 + s.2);
    }
    eprintln!("third-party syncs {total:?} (all, FWD_HOLD after, FWD_OPEN after)");
    assert!(
        total.1 > 10 && total.2 > 0,
        "the walks met chains: {total:?}"
    );
}

// ---- the owner at its chain's epoch below the current one ----------------------------------

/// `FWD_OPEN` with `epoch == fwd_epoch < share_epoch`: the third party holds C while epoch 1 is
/// current (the chain at `fwd_epoch` 1 with the then-current `acc` as its snapshots) and walks
/// it again during epoch 1's rent; a second rebase rolls; C's owner converts one record and
/// lands exactly on the chain's epoch, below the current one; the third party then walks past
/// it (the chain accrues the rest of epoch 1 to the record's ends, then epoch 2); the owner
/// finishes. With a lag of 0 and of 1, the wind-down equals the run without the third party.
#[test]
fn an_owner_at_its_chains_epoch_below_the_current_one() {
    for lag in [0u8, 1] {
        let names = ["B", "C", "H"];
        let run = |third_party: bool| -> (Vec<u64>, u64, u64) {
            let mut lab = Lab::with_cap(TokenProgram::Spl, 10_000);
            let base = lamports_of(&mut lab, &names);
            dust(&mut lab, "H", U, if lag == 1 { 999 } else { 0 });
            dust(&mut lab, "C", 1_000 * U, 600);
            dust(&mut lab, "B", 1_000 * U, 0);
            rebase_k1(&mut lab, "p1", U);
            lab.claim("H").unwrap();
            let t = lab.market().tokens_free;
            lab.borrow30("r1", t / 2).unwrap();
            lab.advance(common::D, 10u128.pow(19));
            if third_party {
                assert_eq!(lab.sync("C", 1), Ok(Standing::Behind));
                let c = lab.position("C").unwrap();
                assert_eq!((c.fwd, c.fwd_epoch, c.epoch), (pool::FWD_HOLD, 1, 0));
            }
            lab.advance(common::D, 10u128.pow(19));
            let a = lab.free_allowance("B");
            let s = lab.shares_for("B", a);
            lab.withdraw("B", s).unwrap();
            let owed = lab.owed("r1");
            lab.repay("r1", owed, 0);
            if third_party {
                assert_eq!(lab.sync("C", 8), Ok(Standing::Behind));
                assert_eq!(lab.position("C").unwrap().fwd_epoch, 1);
            }
            rebase_k1(&mut lab, "p2", U);
            assert_eq!(lab.sync_signed("C", 1), Ok(Standing::Behind));
            if third_party {
                let c = lab.position("C").unwrap();
                assert_eq!(
                    (c.fwd, c.epoch, c.fwd_epoch, c.fwd_lag),
                    (pool::FWD_OPEN, 1, 1, lag),
                    "the owner stopped at the chain's epoch, below the current one"
                );
                assert!(lab.market().share_epoch > c.fwd_epoch);
            }
            rent_with_leaver(&mut lab, "r2", "H", 2, 10u128.pow(19));
            if third_party {
                assert_eq!(lab.sync("C", 8), Ok(Standing::Behind));
                let c = lab.position("C").unwrap();
                assert_eq!(
                    (c.fwd, c.epoch, c.fwd_epoch),
                    (pool::FWD_OPEN, 1, 2),
                    "walked past the owner's epoch"
                );
                lab.claim("B").unwrap();
                lab.claim("H").unwrap();
            }
            assert_eq!(lab.sync_signed("C", 1), Ok(Standing::Current));
            assert_eq!(lab.position("C").unwrap().fwd, pool::FWD_NONE);
            let g = wind_down(&mut lab, &names, &base);
            let v = lab.vault();
            (g, v.lender_sol, v.unmatched)
        };
        let x = run(false);
        let y = run(true);
        eprintln!("lag {lag}: owners alone {x:?}; with the forward chain {y:?}");
        assert_eq!(x, y, "lag {lag}");
    }
}

// ---- the auto-touch's shape abused ----------------------------------------------------------

/// `lender_it`'s pot scenario rolled by two rebases, C behind both with post-record surplus.
fn chained_pot_market() -> Lab {
    let mut lab = Lab::with_cap(TokenProgram::Spl, 10_000);
    lab.deposit("A", 1_000 * U).unwrap();
    lab.deposit("B", 1_000 * U).unwrap();
    lab.borrow30("p", 900 * U).unwrap();
    lab.advance(10 * common::D, 10u128.pow(16));
    let a = lab.free_allowance("A");
    let s = lab.shares_for("A", a);
    lab.withdraw("A", s).unwrap();
    lab.deposit("C", 2_000 * U).unwrap();
    let owed = lab.owed("p");
    lab.repay("p", owed, 0);
    lab.claim("A").unwrap();
    rebase_k1(&mut lab, "q", U);
    lab.deposit("D", 1_000 * U).unwrap();
    rent_with_leaver(&mut lab, "r1", "D", 3, 10u128.pow(15));
    rebase_k1(&mut lab, "q2", U);
    rent_with_leaver(&mut lab, "r2", "B", 3, 10u128.pow(15));
    assert_eq!(lab.position("C").unwrap().epoch, 0);
    lab
}

/// One claim transaction (signed by A only) that repeats C's unsigned sync — read-only records
/// from C's epoch, the same records writable, a wider slice from before, `max_epochs` 0 —
/// before A's `claim_sol` pays exactly what one sync and the claim pay, leaves C and the vault
/// as they leave them, and writes no record.
#[test]
fn a_claim_bundle_repeating_one_sync_pays_what_one_sync_pays() {
    // One sync, then the claim.
    let mut x = chained_pot_market();
    assert_eq!(x.sync("C", 8), Ok(Standing::Behind));
    let lam = x.lamports_of("A");
    x.claim("A").unwrap();
    let paid_x = x.lamports_of("A") - lam;
    let (cx, vx) = (x.position("C").unwrap(), x.vault());
    // The bundle.
    let mut y = chained_pot_market();
    let m = y.k.market;
    let c = y.key("C");
    let a = y.key("A");
    let kp = y.users["A"].kp.insecure_clone();
    let se = y.market().share_epoch;
    let metas = |from: u32, writable: bool| -> Vec<AccountMeta> {
        (from..se)
            .map(|e| {
                let k = pda::share_epoch(&m, e).0;
                if writable {
                    AccountMeta::new(k, false)
                } else {
                    AccountMeta::new_readonly(k, false)
                }
            })
            .collect()
    };
    let a_epoch = y.position("A").unwrap().epoch;
    let n = u8::try_from(se).unwrap();
    let lam = y.lamports_of("A");
    let meta = y
        .env
        .send(
            &[
                ix::with_remaining(ix::sync_lender(&c, &m, n), &metas(0, false)),
                ix::with_remaining(ix::sync_lender(&c, &m, n), &metas(0, true)),
                ix::with_remaining(ix::sync_lender(&c, &m, 1), &metas(1, true)),
                ix::sync_lender(&c, &m, 0),
                ix::with_remaining(ix::claim_sol(&a, &m), &metas(a_epoch, true)),
            ],
            &[&kp],
        )
        .expect("the bundle");
    let ev = events::of::<shortwin::events::ClaimSol>(&meta);
    let paid_y = y.lamports_of("A") - lam;
    assert_eq!(ev[0].sol_paid, paid_y);
    assert_eq!(paid_y, paid_x, "A is paid what one sync gives");
    assert_eq!(
        y.position("C").unwrap().lender(),
        cx.lender(),
        "C as after one sync"
    );
    let vy = y.vault();
    assert_eq!((vy.unmatched, vy.lender_sol), (vx.unmatched, vx.lender_sol));
    assert_eq!(
        y.state().records,
        x.state().records,
        "the records as A's own conversion leaves them (the syncs wrote none)"
    );
    eprintln!(
        "bundle: A paid {paid_y} (one sync: {paid_x}), CU {}",
        meta.compute_units_consumed
    );
}

// ---- KI-67: close and re-creation of a chained position --------------------------------------

/// A dust position held by a third party (a chain of 0 shares), left behind by a later roll:
/// its owner's `[claim_sol, deposit]` closes it (the conversion drops the stale chain) and
/// re-creates it in the same transaction with no chain, at the current epoch; a third party's
/// sync of the new position is a plain touch (`Current`), and no chain field comes back.
#[test]
fn a_chained_dust_position_closes_and_is_recreated_clean_in_one_transaction() {
    let mut lab = Lab::with_cap(TokenProgram::Spl, 10_000);
    assert_eq!(dust(&mut lab, "D", 1, 600) % 1_000, 600);
    dust(&mut lab, "B", 1_000 * U, 0);
    rebase_k1(&mut lab, "p1", U);
    assert_eq!(lab.sync("D", 1), Ok(Standing::Behind));
    let d = lab.position("D").unwrap();
    assert_eq!((d.fwd, d.fwd_shares), (pool::FWD_HOLD, 0));
    lab.deposit("E", 1_000 * U).unwrap();
    rebase_k1(&mut lab, "p2", U);
    assert_eq!(lab.sync("D", 8), Ok(Standing::Behind));
    let m = lab.k.market;
    let k = lab.k;
    let u = lab.user("D");
    let keys = LenderKeys {
        owner: u.kp.pubkey(),
        mint: k.mint,
        token_program: k.token_program,
        owner_token_account: u.ata,
    };
    let kp = lab.users["D"].kp.insecure_clone();
    let se = lab.market().share_epoch;
    let recs: Vec<AccountMeta> = (d.epoch..se)
        .map(|e| AccountMeta::new(pda::share_epoch(&m, e).0, false))
        .collect();
    lab.env
        .send(
            &[
                ix::with_remaining(ix::claim_sol(&keys.owner, &m), &recs),
                ix::deposit(&keys, 1_000_000, 0),
            ],
            &[&kp],
        )
        .expect("[claim_sol, deposit]");
    let d = lab.position("D").expect("re-created");
    assert_eq!(
        (d.fwd, d.fwd_epoch, d.fwd_shares, d.fwd_lag, d.epoch),
        (pool::FWD_NONE, 0, 0, 0, se),
        "no chain survives the close"
    );
    assert_eq!((d.real, d.claimed, d.lock, d.lock_term), (0, 0, 0, 0));
    assert_eq!(lab.sync("D", 8), Ok(Standing::Current));
    assert_eq!(lab.position("D").unwrap().fwd, pool::FWD_NONE);
}

// ---- R4B-4's whales (W1, W3) with the forward chain -----------------------------------------

/// The lumps of `ki65_it`'s whale tests: 3 SOL.
const LUMP: u64 = 3 * U;

/// `ki65_it::rebase`: everything but one raw token settled (`sol − lump` of it), a rebase, then
/// a `lump` swept into the rebased pool in the same second (since AIR-07 a lump that arrives with
/// such a settlement is flushed to the old shares at once).
fn rebase(lab: &mut Lab, pid: &str, sol: u64, lump: u64) {
    let e = lab.market().share_epoch;
    let t = lab.market().tokens_free;
    lab.borrow30(pid, t - 1).unwrap();
    lab.settle(pid, sol - lump, 0);
    assert_eq!(lab.market().share_epoch, e + 1, "{pid}: the epoch rolled");
    assert_eq!(
        lab.state().records.last().unwrap().num,
        1,
        "{pid}: a rebase"
    );
    if lump > 0 {
        lab.sweep_sol(lump);
    }
}

/// `ki65_it::w1_a_jit_whale_takes_nothing_while_a_share_is_latent` with the forward chain: H's
/// latent share (1,000 old shares, a chain of 1 share) is walked by a third party — the whale
/// itself — before the whale's deposit, after its lump, before its exit and before its claim, so
/// whatever H's share earns after the record reaches the pot while the whale is in. The whale
/// still realises 0 (its deficit, if any, is its own entitlement), and H, converting later, is
/// paid both vests.
#[test]
fn w1_with_the_chain_a_jit_whale_still_takes_nothing() {
    let mut lab = Lab::with_cap(TokenProgram::Token2022, 10_000);
    assert_eq!(dust(&mut lab, "H", 1, 0), 1_000);
    lab.deposit("W", 1_000_000).unwrap();
    rebase(&mut lab, "p", U + LUMP, LUMP);
    exit_all(&mut lab, "W");
    lab.claim("W").unwrap();
    assert_eq!(lab.market().total_shares.get(), 1, "H's latent share");
    let lam = wealth(&mut lab, "J");
    assert_eq!(lab.sync("H", 8), Ok(Standing::Behind));
    let h = lab.position("H").unwrap();
    assert_eq!((h.fwd, h.fwd_shares, h.epoch), (pool::FWD_HOLD, 1, 0));
    lab.deposit("J", 199 * U).unwrap();
    assert_eq!(lab.sync("H", 8), Ok(Standing::Behind));
    let t = lab.market().tokens_free;
    lab.borrow30("q", t / 2).unwrap();
    lab.repay("q", 0, LUMP);
    assert_eq!(lab.sync("H", 8), Ok(Standing::Behind));
    let pot = lab.vault().unmatched;
    exit_all(&mut lab, "J");
    assert_eq!(lab.sync("H", 8), Ok(Standing::Behind));
    lab.claim("J").unwrap();
    lab.claim("J").unwrap();
    assert_eq!(
        wealth(&mut lab, "J"),
        lam,
        "the whale realises 0 (pot {pot} after H's walks)"
    );
    lab.advance(i64::from(lab.lump_vest_s) + 1, 0);
    assert_eq!(lab.sync("H", 8), Ok(Standing::Behind));
    let paid = lab.claim("H").unwrap();
    assert!(paid >= 2 * LUMP, "H is paid both vests: {paid}");
    eprintln!("W1 with the chain: pot after H's walks {pot}, H paid {paid}");
}

/// `ki65_it::w3_a_deposit_while_a_share_is_latent_gets_no_flush` with third-party syncs of H
/// (600 old shares: a chain of 0 shares that jumps) around the whale's deposit, sweep, exit and
/// claim: no flush, the whale realises 0, H is paid both vests.
#[test]
fn w3_with_the_chain_a_deposit_while_a_share_is_latent_gets_no_flush() {
    let mut lab = Lab::with_cap(TokenProgram::Spl, 10_000);
    assert_eq!(dust(&mut lab, "H", 1, 600), 600);
    assert_eq!(dust(&mut lab, "W", 9_000, 600), 7_199_600);
    rebase(&mut lab, "p", U + LUMP, LUMP);
    exit_all(&mut lab, "W");
    lab.claim("W").unwrap();
    assert_eq!(lab.market().total_shares.get(), 1, "H's latent share");
    let lam = wealth(&mut lab, "J");
    assert_eq!(lab.sync("H", 8), Ok(Standing::Behind));
    lab.deposit("J", 1_000_000).unwrap();
    assert_eq!(lab.sync("H", 8), Ok(Standing::Behind));
    lab.sweep_sol(LUMP);
    assert_eq!(lab.sync("H", 8), Ok(Standing::Behind));
    exit_all(&mut lab, "J");
    lab.claim("J").unwrap();
    assert_eq!(wealth(&mut lab, "J"), lam, "the whale realises 0");
    assert_eq!(lab.market().total_shares.get(), 1);
    lab.advance(i64::from(lab.lump_vest_s) + 1, 0);
    let h = lab.claim("H").unwrap();
    assert!(h >= 2 * LUMP, "H is paid both vests: {h}");
    assert_eq!(lab.position("H").unwrap().shares, 1);
}
