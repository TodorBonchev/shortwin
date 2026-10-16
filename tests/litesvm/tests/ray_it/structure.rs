//! I14 on Raydium (ARCH §3.0, §3.12, §8.3; C0-2): every ReadSet and TradeExtras slot of both
//! adapters is mandatory and key-checked on `open_short`, `close_short`, `liquidate`, `expire`,
//! `crank_price` and `sol_settle` — each slot omitted, replaced by a foreign key, the first two
//! swapped, or a trailing account appended makes the instruction fail with
//! `BadRemainingAccounts` / `VenueAccountMismatch`, never "no route" or a silent default.
//! Also: the venue ignores trailing extras (R-RAY §9.6), so a selected extras set with a fixed
//! read-only entry trades normally on both programs. M10 on both programs: a thin pool in place
//! of the adopted one on every instruction (G-A6: the reversed Fartcoin pool on AMM v4, a planted
//! same-mint CPMM pool at a tenth of the depth on USELESS and the Token-2022 pool), and a
//! denylisted key in an extras set refused and bypassed with `previous` (G-A5's fork run); M10
//! batch 3 on both programs: the adopted pool and its vaults on every extras site and the
//! position's borrower on the keeper exits are `DeniedAccount` extras, with a fresh-key control.

use shortwin::state::EXTRAS_IX_SELL;
use shortwin::venues::EXTRAS_SET_CURRENT;
use shortwin::{ExtraEntryArg, SeedPartArg};
use shortwin_litesvm_tests::engine::{expire_ix, liquidate_ix, set_venue_extras_ix, sol_settle_ix};

use crate::common::*;

fn variants(ix: &Instruction, remaining: usize) -> Vec<(String, Instruction)> {
    let start = ix.accounts.len() - remaining;
    let mut out = Vec::new();
    for i in 0..remaining {
        let mut omit = ix.clone();
        omit.accounts.remove(start + i);
        out.push((format!("omit slot {i}"), omit));
        let mut swap = ix.clone();
        let m = &mut swap.accounts[start + i];
        *m = AccountMeta {
            pubkey: Pubkey::new_unique(),
            is_signer: false,
            is_writable: m.is_writable,
        };
        out.push((format!("replace slot {i}"), swap));
    }
    let mut extra = ix.clone();
    extra
        .accounts
        .push(AccountMeta::new_readonly(Pubkey::new_unique(), false));
    out.push(("extra trailing account".to_string(), extra));
    let mut order = ix.clone();
    order.accounts.swap(start, start + 1);
    out.push(("slots 0 and 1 swapped".to_string(), order));
    let mut order = ix.clone();
    order.accounts.swap(start + 1, start + 2);
    out.push(("slots 1 and 2 swapped".to_string(), order));
    out
}

fn assert_all_fail(
    env: &mut Env,
    ix: &Instruction,
    remaining: usize,
    signers: &[&solana_keypair::Keypair],
) {
    for (what, bad) in variants(ix, remaining) {
        let res = env.send(&[bad], signers);
        let c = custom_code(&res);
        assert!(
            c == Some(code(E::BadRemainingAccounts)) || c == Some(code(E::VenueAccountMismatch)),
            "{what}: expected BadRemainingAccounts / VenueAccountMismatch, got {c:?}"
        );
    }
}

#[test]
fn every_venue_slot_is_mandatory_and_key_checked() {
    for fixture in [FART, USELESS, T22] {
        let (mut env, rm, q) = setup(fixture);
        let (read_len, trade_len) = (rm.read_set(false).len(), rm.trade_extras().len());
        let all = read_len + trade_len;
        let b = env.borrower();
        let open = open_short_ix(
            &rm,
            &b.key(),
            0,
            q,
            0,
            u64::MAX,
            u64::MAX,
            ExtrasSel::NONE,
            &[],
        );
        assert_all_fail(&mut env, &open, all, &[&b.kp]);
        ok(env.send(&[open], &[&b.kp]));
        env.tick(&rm, 1);
        let crank = crank_price_ix(&rm);
        assert_all_fail(&mut env, &crank, read_len, &[]);
        let keeper = env.funded_keypair();
        let settle = sol_settle_ix(&env, &rm, &keeper.pubkey(), &b.key(), 0);
        assert_all_fail(&mut env, &settle, read_len, &[&keeper]);
        let liq = liquidate_ix(&rm, &keeper.pubkey(), &b.key(), 0);
        assert_all_fail(&mut env, &liq, all, &[&keeper]);
        let exp = expire_ix(&rm, &keeper.pubkey(), &b.key(), 0);
        assert_all_fail(&mut env, &exp, all, &[&keeper]);
        let close = close_short_ix(&rm, &b.key(), 0, u64::MAX, ExtrasSel::NONE, &[]);
        assert_all_fail(&mut env, &close, all, &[&b.kp]);
        ok(env.send(&[crank], &[]));
        ok(env.send(&[close], &[&b.kp]));
    }
}

/// A selected extras set (A41) is appended to the venue CPI; AMM v4 v2 and CPMM ignore
/// trailing accounts (R-RAY §9.6), so a harmless fixed entry changes nothing.
#[test]
fn extras_ride_along_on_both_programs() {
    for fixture in [FART, USELESS] {
        let (mut env, rm, q) = setup(fixture);
        let venue = rm.kind() as u8;
        let extra = env.funded_keypair().pubkey();
        let ea = env.pause_authority.insecure_clone();
        let entry = ExtraEntryArg {
            kind: shortwin::state::ENTRY_FIXED,
            writable: false,
            key: extra,
            seeds: Vec::<SeedPartArg>::new(),
        };
        ok(env.send(
            &[set_venue_extras_ix(
                &ea.pubkey(),
                venue,
                EXTRAS_IX_SELL as u8,
                vec![entry],
            )],
            &[&ea],
        ));
        let b = env.borrower();
        let sel = ExtrasSel {
            set: EXTRAS_SET_CURRENT,
            bumps: [0; 4],
        };
        let extras = [
            AccountMeta::new_readonly(pda::venue_extras_seed(venue), false),
            AccountMeta::new_readonly(extra, false),
        ];
        let s_exp = rm.quote_sell(&env.svm, q).unwrap().proceeds;
        let open = open_short_ix(&rm, &b.key(), 0, q, 0, u64::MAX, u64::MAX, sel, &extras);
        let meta = ok(env.send(&[open], &[&b.kp]));
        let ev: Open = events::one(&meta);
        assert_eq!(ev.s, s_exp, "{fixture:?}");
    }
}

/// The invariant hooks see Raydium markets (`invariants::venue_view`, computed from the readers
/// and the math crate, not from the adapter): the same executability, reserves and quotes as
/// the fork's pool helpers — so I13's re-quotes and I16's route rules run on every Raydium
/// instruction of this binary — and a halted pool is not executable to them either.
#[test]
fn the_hooks_see_raydium_markets() {
    use shortwin_litesvm_tests::invariants::venue_view;
    for fixture in ELIGIBLE {
        let (mut env, rm, q) = setup(fixture);
        let m = env.market(&rm.market());
        let v = venue_view::view(&env.svm, &m).expect("a Raydium view");
        assert!(v.executable, "{fixture:?}");
        assert_eq!(v.reserves, rm.reserves(&env.svm));
        assert_eq!(v.buy(q), rm.quote_buy(&env.svm, q).map(|b| b.cost));
        assert_eq!(v.sell(q), rm.quote_sell(&env.svm, q).map(|s| s.proceeds));
        assert_eq!(
            u64::from(v.fee_ppm),
            match &rm.pool {
                AnyPool::V4(p) => u64::from(p.info(&env.svm).fee_ppm().unwrap()),
                AnyPool::Cpmm(p) =>
                    u64::from(p.state(&env.svm).fee_ppm(&p.config(&env.svm)).unwrap()),
            }
        );
        match &rm.pool {
            AnyPool::V4(p) => fork::set_v4_status(&mut env.svm, p, 2),
            AnyPool::Cpmm(p) => fork::set_cpmm_status(&mut env.svm, p, 4),
        }
        let v = venue_view::view(&env.svm, &m).unwrap();
        assert_eq!((v.executable, v.reserves, v.fee_ppm), (false, None, 0));
    }
}

/// `ix` with `subs` applied: each `(i, key)` puts `key` in the `i`-th venue slot (of the last
/// `remaining` metas, ReadSet first), where `ix` has that slot (a TradeExtras slot only on the
/// trading instructions).
fn thin(ix: &Instruction, remaining: usize, subs: &[(usize, Pubkey)]) -> Instruction {
    let mut ix = ix.clone();
    let start = ix.accounts.len() - remaining;
    for &(i, key) in subs.iter().filter(|(i, _)| *i < remaining) {
        ix.accounts[start + i].pubkey = key;
    }
    ix
}

/// The thin pool on every instruction of a market on `fixture`, `subs` (from [`thin`]) naming
/// the other pool's keys: refused (`VenueAccountMismatch`) on `open_short`, `crank_price`,
/// `sol_settle`, `liquidate`, `close_short` and `expire`, the position unchanged; each honest
/// instruction (the control) is accepted — the open, the crank, the `sol_settle` no-op, the
/// close, the healthy position's liquidation refused by its body (`NotLiquidatable`), the expiry
/// after the deadline.
fn thin_pool_on_every_instruction(
    fixture: RayFixture,
    subs: impl FnOnce(&mut Env, &RayMarket) -> Vec<(usize, Pubkey)>,
) {
    let (mut env, rm, q) = setup(fixture);
    let subs = subs(&mut env, &rm);
    let (read_len, all) = (
        rm.read_set(false).len(),
        rm.read_set(false).len() + rm.trade_extras().len(),
    );
    let mut b = env.borrower();
    let open = open_short_ix(
        &rm,
        &b.key(),
        0,
        q,
        0,
        u64::MAX,
        u64::MAX,
        ExtrasSel::NONE,
        &[],
    );
    assert_err(
        &env.send(&[thin(&open, all, &subs)], &[&b.kp]),
        E::VenueAccountMismatch,
    );
    let (_, n0, _) = open_ok(&mut env, &rm, &mut b, q);
    assert_eq!(n0, 0, "{fixture:?}: the refused open created no position");
    let mut b2 = env.borrower();
    let (_, n1, _) = open_ok(&mut env, &rm, &mut b2, q);
    env.tick(&rm, 1);
    let keeper = env.funded_keypair();
    let pos1 = pda::position(&rm.market(), &b2.key(), n1).0;
    let pos_bytes = |env: &Env| env.svm.get_account(&pos1).map(|a| a.data);
    let before = pos_bytes(&env);

    let crank = crank_price_ix(&rm);
    assert_err(
        &env.send(&[thin(&crank, read_len, &subs)], &[]),
        E::VenueAccountMismatch,
    );
    ok(env.send(&[crank], &[]));
    let settle = sol_settle_ix(&env, &rm, &keeper.pubkey(), &b2.key(), n1);
    assert_err(
        &env.send(&[thin(&settle, read_len, &subs)], &[&keeper]),
        E::VenueAccountMismatch,
    );
    let liq = liquidate_ix(&rm, &keeper.pubkey(), &b2.key(), n1);
    assert_err(
        &env.send(&[thin(&liq, all, &subs)], &[&keeper]),
        E::VenueAccountMismatch,
    );
    assert_eq!(
        pos_bytes(&env),
        before,
        "{fixture:?}: the position unchanged"
    );
    ok(env.send(&[settle], &[&keeper]));
    assert_err(&env.send(&[liq], &[&keeper]), E::NotLiquidatable);

    let close = close_short_ix(&rm, &b.key(), n0, u64::MAX, ExtrasSel::NONE, &[]);
    assert_err(
        &env.send(&[thin(&close, all, &subs)], &[&b.kp]),
        E::VenueAccountMismatch,
    );
    ok(env.send(&[close], &[&b.kp]));

    let deadline = env.position(&rm, &b2.key(), n1).deadline;
    let now = env.now();
    env.warp(deadline - now + 1);
    env.tick(&rm, 3);
    let exp = expire_ix(&rm, &keeper.pubkey(), &b2.key(), n1);
    let before = pos_bytes(&env);
    assert_err(
        &env.send(&[thin(&exp, all, &subs)], &[&keeper]),
        E::VenueAccountMismatch,
    );
    assert_eq!(
        pos_bytes(&env),
        before,
        "{fixture:?}: the position unchanged"
    );
    let meta = ok(env.send(&[exp], &[&keeper]));
    let _: shortwin::events::Expire = events::one(&meta);
}

/// The ReadSet and TradeExtras are the ones the market recorded at listing (M10 G-A6 on AMM v4):
/// another pool of the same mint — the reversed Fartcoin pool's keys (pool, coin vault, pc
/// vault, LP mint) — in place of the adopted pool's ReadSet is refused on every instruction,
/// the open included ([`thin_pool_on_every_instruction`]).
#[test]
fn a_thin_pool_in_place_of_the_adopted_one_on_every_instruction() {
    thin_pool_on_every_instruction(FART, |env, _| {
        let rev = env.load_ray_v4(V4Fixture::FartcoinReversed);
        vec![
            (0, rev.pool),
            (1, rev.coin_vault),
            (2, rev.pc_vault),
            (3, rev.lp_mint),
        ]
    });
}

/// M10 G-A6 on CPMM (USELESS, and the Token-2022 pool): a **same-mint** pool at a tenth of the
/// adopted pool's depth, a real CPMM pool on the same config (`pool_select::plant_cpmm`: its own
/// vaults, LP mint and observation; it trades on the 2026-10 ELF), in place of the adopted one —
/// its pool, vaults and LP mint in the ReadSet (the config slot is the same key) and its
/// observation in the TradeExtras — on every instruction ([`thin_pool_on_every_instruction`]).
#[test]
fn cpmm_a_thin_same_mint_pool_in_place_of_the_adopted_one_on_every_instruction() {
    use shortwin_litesvm_tests::pool_select::{plant_cpmm, spec_like};
    for fixture in [USELESS, T22] {
        thin_pool_on_every_instruction(fixture, |env, rm| {
            let RayFixture::Cpmm(template) = fixture else {
                unreachable!()
            };
            let p = rm.cpmm();
            let spec = spec_like(&env.svm, &rm.pool, p.sol_is_token0, 10_000).scaled(1, 10);
            let t = plant_cpmm(env, template, spec);
            assert_eq!(
                (t.token_0_mint, t.token_1_mint, t.amm_config),
                (p.token_0_mint, p.token_1_mint, p.amm_config),
                "same mints, same config"
            );
            let thin = AnyPool::Cpmm(t);
            assert!(
                t.venue_executable(&env.svm, env.now()),
                "an executable pool"
            );
            let (x, _) = thin
                .io_reserves(&env.svm, shortwin_math::fees::Direction::Sell)
                .unwrap();
            let (x0, _) = rm.reserves(&env.svm).unwrap();
            assert_eq!(x, x0 / 10, "a tenth of the depth");
            let read_len = rm.read_set(false).len();
            vec![
                (0, t.pool),
                (2, t.token_0_vault),
                (3, t.token_1_vault),
                (4, t.lp_mint),
                (read_len + 2, t.observation),
            ]
        });
    }
}

/// A denylisted key in a Raydium extras set (M10 G-A5's fork run; ARCH §2.4 rule 4, A41): the
/// Emergency publishes a harmless fixed extra, then a hostile set naming the market's pool vault
/// (the venue's source / destination of the leg) — on the sell leg and on the buy leg. Selecting
/// the hostile `current` set fails `DeniedAccount` on the open and on the close; selecting
/// `previous` (the harmless set) bypasses it and trades exactly as with no extras.
fn a_denylisted_extra_is_bypassed_with_previous(fixture: RayFixture) {
    use shortwin::state::EXTRAS_IX_BUY;
    use shortwin::venues::EXTRAS_SET_PREVIOUS;
    let (mut env, rm, q) = setup(fixture);
    let venue = rm.kind() as u8;
    let ea = env.pause_authority.insecure_clone();
    let harmless = env.funded_keypair().pubkey();
    let entry = |key: Pubkey| ExtraEntryArg {
        kind: shortwin::state::ENTRY_FIXED,
        writable: false,
        key,
        seeds: Vec::<SeedPartArg>::new(),
    };
    for ix_kind in [EXTRAS_IX_SELL as u8, EXTRAS_IX_BUY as u8] {
        for key in [harmless, rm.keys.pool_vault] {
            ok(env.send(
                &[set_venue_extras_ix(
                    &ea.pubkey(),
                    venue,
                    ix_kind,
                    vec![entry(key)],
                )],
                &[&ea],
            ));
        }
    }
    let ve = AccountMeta::new_readonly(pda::venue_extras_seed(venue), false);
    let hostile = [
        ve.clone(),
        AccountMeta::new_readonly(rm.keys.pool_vault, false),
    ];
    let good = [ve, AccountMeta::new_readonly(harmless, false)];
    let current = ExtrasSel {
        set: EXTRAS_SET_CURRENT,
        bumps: [0; 4],
    };
    let previous = ExtrasSel {
        set: EXTRAS_SET_PREVIOUS,
        bumps: [0; 4],
    };
    let b = env.borrower();
    let open = |sel, extras: &[AccountMeta]| {
        open_short_ix(&rm, &b.key(), 0, q, 0, u64::MAX, u64::MAX, sel, extras)
    };
    assert_err(
        &env.send(&[open(current, &hostile)], &[&b.kp]),
        E::DeniedAccount,
    );
    let s_exp = rm.quote_sell(&env.svm, q).unwrap().proceeds;
    let meta = ok(env.send(&[open(previous, &good)], &[&b.kp]));
    let ev: Open = events::one(&meta);
    assert_eq!(ev.s, s_exp, "{fixture:?}: as with no extras");
    env.tick(&rm, 1);
    let close =
        |sel, extras: &[AccountMeta]| close_short_ix(&rm, &b.key(), 0, u64::MAX, sel, extras);
    assert_err(
        &env.send(&[close(current, &hostile)], &[&b.kp]),
        E::DeniedAccount,
    );
    let b_exp = rm.quote_buy(&env.svm, q).unwrap().cost;
    let meta = ok(env.send(&[close(previous, &good)], &[&b.kp]));
    let ev: Close = events::one(&meta);
    assert_eq!(ev.buyback_cost, b_exp, "{fixture:?}: as with no extras");
}

#[test]
fn a_denylisted_extra_on_amm_v4_is_bypassed_with_previous() {
    a_denylisted_extra_is_bypassed_with_previous(FART);
}

/// G-A5 on CPMM: the hostile set names the CPMM market's pool vault (the swap's token source on
/// the sale, its destination on the buy-back), on USELESS and on the Token-2022 pool.
#[test]
fn a_denylisted_extra_on_cpmm_is_bypassed_with_previous() {
    for fixture in [USELESS, T22] {
        a_denylisted_extra_is_bypassed_with_previous(fixture);
    }
}

/// M10 batch 3 on both Raydium programs (ARCH §2.4 rule 4; `engine::pool_and_borrower_denied_extras`):
/// the adopted pool — the AMM v4 pool and its coin / pc vaults (Fartcoin), the CPMM pool and its
/// token 0 / 1 vaults (USELESS, on the 2026-10 CPMM build) — and, on the keeper exits, the
/// position's borrower, each published as a fixed extra, are `DeniedAccount` on `open_short`,
/// `close_short`, `liquidate` and `expire`, the position unchanged; the same set shape with a fresh
/// key rides along (Raydium ignores trailing accounts): the open, the close and the expiry land,
/// the healthy liquidation reaches `NotLiquidatable`.
#[test]
fn the_pool_and_the_borrower_are_denied_extras_on_both_programs() {
    for (fixture, label) in [(FART, "AMM v4"), (USELESS, "CPMM 2026-10")] {
        shortwin_litesvm_tests::engine::pool_and_borrower_denied_extras(|| setup(fixture), label);
    }
}

/// M10 batch 3 (the security review) on the CPMM 2026-10 build (USELESS): the pool's token
/// vaults are CPMM PDAs `["pool_vault", pool, mint]`, so an Emergency template on the `pool` and
/// `mint` placeholders derives them; the denylist runs on the derived key, so each is
/// `DeniedAccount` on `open_short` through `current` and through `previous`
/// (`engine::template_aliases_denied`). AMM v4's vaults cannot be derived from the placeholders.
#[test]
fn template_aliases_of_the_cpmm_vaults_are_denied_extras() {
    use shortwin::constants::WSOL_MINT;
    use shortwin::state::{PLACEHOLDER_MINT, PLACEHOLDER_POOL};
    use shortwin_litesvm_tests::engine::{PoolAlias, VenueMarket, seed_const, seed_placeholder};
    shortwin_litesvm_tests::engine::template_aliases_denied(
        || setup(USELESS),
        "CPMM 2026-10",
        |env, rm| {
            let pool = rm.read_set(false)[shortwin::venues::raydium_cpmm::R_POOL].pubkey;
            let program = env.svm.get_account(&pool).unwrap().owner;
            let vault = |what: &str, second: shortwin::SeedPartArg, key: Pubkey| PoolAlias {
                what: what.into(),
                program,
                seeds: vec![
                    seed_const(b"pool_vault"),
                    seed_placeholder(PLACEHOLDER_POOL),
                    second,
                ],
                resolved: vec![
                    b"pool_vault".to_vec(),
                    pool.to_bytes().to_vec(),
                    key.to_bytes().to_vec(),
                ],
            };
            vec![
                vault(
                    "the token vault",
                    seed_placeholder(PLACEHOLDER_MINT),
                    rm.mint(),
                ),
                vault("the WSOL vault", seed_const(WSOL_MINT.as_ref()), WSOL_MINT),
            ]
        },
    );
}
