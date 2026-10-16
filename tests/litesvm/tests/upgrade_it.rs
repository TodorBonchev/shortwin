//! T-SELF-UPGRADE (pump-final §14, §15 "Release coordination"; review R-8, R-9 (d)): short.win's
//! own Governance upgrade, from release 1.0.0 to the build under test, under positions the 1.0.0
//! curve adapter opened, on pump's 2026-10-13 build (`PumpDeploy::CURRENT`, the re-pin).
//!
//! **The 1.0.0 program** is `tests/fixtures/shortwin/shortwin_1_0_0.so`: the `v1.0.0` tag
//! (`e4895c2`) built with the pinned toolchain, whose sha256 and trimmed-ELF sha256 must equal
//! AUDIT.md §0's verified 1.0.0 hashes (the fixtures README says how it was built and what was
//! and was not verified). It trades the curve with `sell_v2` / `buy_v2` and the 20-key v2-era
//! TradeExtras (`pump::v2_trade_extras`); the build under test with `sell_v3` / `buy_v3` and 10.
//!
//! **The upgrade is the loader's own** (`Env::upgrade_shortwin`, shared with the keeper-litesvm
//! `upgrade_window` scenario): `ExtendProgram` of short.win's ProgramData to the new ELF's
//! size (the build under test is 25,808 bytes longer), one slot later a buffer holding the new ELF
//! under the upgrade authority, then `Upgrade` signed by that authority (`Env::upgrade_authority`,
//! a real signature) — what Governance's vault transaction executes after its time lock (§15: at
//! a slot nobody chooses). Only the buffer is planted (the loader's `Write` chunks would carry the
//! same bytes).
//!
//! What it settles, in one scenario ([`v1_0_0_positions_close_and_liquidate_after_the_v3_upgrade`]):
//! - on 1.0.0, a 10-key (v3) list is refused `BadRemainingAccounts` and writes nothing; the v2
//!   list opens positions on a 115-byte Token-2022 curve and on `near166` (near completion);
//! - the upgrade touches no short.win account: every `Market`, `ShortPosition` and `MarketVault`
//!   is byte-identical across it, and the new build reads and exits them;
//! - after it, a client close with the 20-key list fails `BadRemainingAccounts` or
//!   `VenueAccountMismatch` with nothing written, and the same close with the 10-key list lands;
//! - the keeper's `[crank, liquidate]` after a real pump and its `[crank, expire]` land with the
//!   v3 list; the expiry is a synthetic `q > rT` exit (pump's `PostCompleteBuyEvent`, `pcB =
//!   q − rT`, the market `PumpMigrating` in the same instruction);
//! - a pre-upgrade position on the now migrating market settles S3 at §5.4's ρ-scaled FV on the
//!   live `(X₁, Y₁)` once the pause and `s3_wait_s` have passed.
//!
//! The keeper's own format choice across the same upgrade (§7 `CurveExtrasFormat`) is the
//! keeper-litesvm `upgrade_window` scenario (P4). Every transaction goes through `Env::send`: the
//! invariant hooks run on the 1.0.0 program's transactions too.

#![allow(clippy::result_large_err)]

#[path = "pump_curve_support.rs"]
mod support;

use shortwin_litesvm_tests::env::{
    SHORTWIN_1_0_0_EXECUTABLE_HASH, SHORTWIN_1_0_0_LEN, shortwin_1_0_0_so, trimmed_elf,
};
use shortwin_litesvm_tests::fixtures::MarketKeys;
use solana_account::Account;
use support::*;

/// A curve market as shortwin 1.0.0's clients and keeper address it: the same ReadSet, the
/// v2-era TradeExtras T0–T19.
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

/// short.win 1.0.0 on the pump fork: `initialize_config` (the harness defaults, the re-pin's
/// guard slots), the current pump, PumpSwap and pump-fees builds and statics, the coins, mainnet's
/// Rent and `near166`'s snapshot clock.
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

/// Lists `c` (`create_market(Auto)` with the pump init set), lends `40·q`, waits out the warm-up
/// and cranks; `q = vT/200`.
fn add_market(env: &mut Env, c: PumpCurve) -> (PumpMarket, u64) {
    ok(env.list_pump(&c));
    let q = c.state(&env.svm).virtual_token_reserves / 200;
    let pm = env.pump_market_listed(PumpSpec::new(c.coin, q * 40), c);
    env.crank(&pm);
    (pm, q)
}

/// `simulate_transaction` of `ixs` (nothing is committed): whether it would succeed.
fn would_land(env: &Env, ixs: &[Instruction], signers: &[&Keypair]) -> bool {
    let mut all: Vec<&Keypair> = vec![&env.payer];
    all.extend(signers.iter().copied());
    let tx = solana_transaction::Transaction::new_signed_with_payer(
        ixs,
        Some(&env.payer.pubkey()),
        &all,
        env.svm.latest_blockhash(),
    );
    env.svm.simulate_transaction(tx).is_ok()
}

/// The keeper's exit transaction `[SetComputeUnitLimit, crank_price, exit]` (ARCH §6.1's curve
/// keeper limit), retried every second — the clock advanced, the market cranked alone when the
/// simulation says not yet — until it would land; then sent. Returns its metadata.
fn keeper_exit(
    env: &mut Env,
    pm: &PumpMarket,
    keeper: &Keypair,
    exit: impl Fn(&Env) -> Instruction,
    max_s: usize,
) -> TransactionMetadata {
    for _ in 0..max_s {
        env.warp(1);
        let ixs = [
            pump::cu_limit_ix(pump::CURVE_KEEPER_EXIT_CU),
            crank_price_ix(pm),
            exit(env),
        ];
        if would_land(env, &ixs, &[keeper]) {
            return ok(env.send(&ixs, &[keeper]));
        }
        env.crank(pm);
    }
    panic!("the keeper's exit never landed within {max_s} s");
}

/// The accounts the upgrade must leave as they are (byte for byte).
fn snapshot(env: &Env, keys: &[Pubkey]) -> Vec<Option<Account>> {
    keys.iter().map(|k| env.svm.get_account(k)).collect()
}

/// T-SELF-UPGRADE (module docs).
#[test]
fn v1_0_0_positions_close_and_liquidate_after_the_v3_upgrade() {
    let (mut env, curves) = fork_on_1_0_0(&[PumpCoin::T22_115, PumpCoin::Near166]);
    let (t22, qt) = add_market(&mut env, curves[0]);
    let (near, qn) = add_market(&mut env, curves[1]);

    // 1.0.0 takes the 20-key v2 list only: a 10-key (v3) open is refused before any CPI.
    let mut a = env.borrower();
    let before = env.svm.get_account(&t22.curve.curve);
    let res = env.open(&t22, &mut a, qt).0;
    assert_err(&res, E::BadRemainingAccounts);
    assert_eq!(
        env.svm.get_account(&t22.curve.curve),
        before,
        "nothing written"
    );

    // Positions opened by 1.0.0's curve adapter (`sell_v2`, the v2 list).
    let open = |env: &mut Env, pm: &PumpMarket, b: &mut Borrower, q: u64| -> u64 {
        let view = v2(env, pm);
        let (res, nonce) = env.open(&view, b, q);
        let ev: Open = events::one(&ok(res));
        assert_eq!(ev.q, q);
        nonce
    };
    let na = open(&mut env, &t22, &mut a, qt);
    let mut b = env.borrower();
    let nb = open(&mut env, &t22, &mut b, qt);
    let mut d = env.borrower();
    let nd = open(&mut env, &near, &mut d, qn);
    let mut e = env.borrower();
    let ne = open(&mut env, &near, &mut e, qn / 2);
    env.warp(1);
    env.crank(&t22);
    env.crank(&near);
    let pos = |pm: &PumpMarket, w: &Borrower, n: u64| pda::position(&pm.market(), &w.key(), n).0;
    let kept: Vec<Pubkey> = vec![
        t22.market(),
        near.market(),
        pda::market_vault(&t22.market()).0,
        pda::market_vault(&near.market()).0,
        pos(&t22, &a, na),
        pos(&t22, &b, nb),
        pos(&near, &d, nd),
        pos(&near, &e, ne),
    ];
    let held = snapshot(&env, &kept);
    let pos_e_v1 = env.position(&near, &e.key(), ne);

    // Governance's upgrade to the build under test.
    let new_elf = std::fs::read(shortwin_litesvm_tests::env::program_so_path()).unwrap();
    assert!(
        new_elf.len() > SHORTWIN_1_0_0_LEN,
        "the ProgramData must grow first"
    );
    env.upgrade_shortwin(&new_elf);
    assert_eq!(
        env.deployed_shortwin_hash(),
        fork::sha256_hex(trimmed_elf(&new_elf)),
        "the build under test runs"
    );
    assert_eq!(snapshot(&env, &kept), held, "the upgrade wrote no account");

    // A client close with the 20-key list fails cleanly; with the 10-key list it lands.
    let watched = [
        t22.curve.curve,
        t22.keys.pool_vault,
        t22.market(),
        pos(&t22, &a, na),
    ];
    let before = snapshot(&env, &watched);
    let view = v2(&env, &t22);
    let res = env.close(&view, &a, na);
    let code = custom_code(&res);
    assert!(
        code == Some(u32::from(E::BadRemainingAccounts))
            || code == Some(u32::from(E::VenueAccountMismatch)),
        "a 20-key list: {code:?}"
    );
    assert_eq!(snapshot(&env, &watched), before, "nothing written");
    let vault0 = tokens(&env, &t22.keys.pool_vault);
    let c: Close = events::one(&ok(env.close(&t22, &a, na)));
    assert_eq!(c.q, qt);
    assert_eq!(tokens(&env, &t22.keys.pool_vault) - vault0, qt);

    // The keeper's [crank, liquidate] after a real pump, with the v3 list.
    let keeper = env.funded_keypair();
    let crowd = CurveUser::new(&mut env, &t22.curve);
    let vt = t22.curve.state(&env.svm).virtual_token_reserves;
    ok(crowd.buy(&mut env, &t22.curve, vt / 10 * 4, u64::MAX));
    let vault0 = tokens(&env, &t22.keys.pool_vault);
    let kp = keeper.pubkey();
    let meta = keeper_exit(
        &mut env,
        &t22,
        &keeper,
        |_| liquidate_ix(&t22, &kp, &b.key(), nb),
        900,
    );
    let liq: Liquidate = events::one(&meta);
    assert!(liq.buyback_cost > 0 && !liq.expired);
    assert_eq!(tokens(&env, &t22.keys.pool_vault) - vault0, qt);
    assert!(
        fork::pump_post_complete_buy_events(&meta).is_empty(),
        "a plain buy"
    );

    // The keeper's synthetic expiry on `near166`: the crowd leaves `rT = q/2`, the deadline
    // passes, `[crank, expire]` buys `rT` on the curve and `q − rT` from the would-be pool.
    let crowd = CurveUser::new(&mut env, &near.curve);
    env.fund(&crowd.kp.pubkey(), 2_000 * SOL);
    let rt = near.curve.state(&env.svm).real_token_reserves;
    ok(crowd.buy(&mut env, &near.curve, rt - qn / 2, u64::MAX));
    let rt = near.curve.state(&env.svm).real_token_reserves;
    assert_eq!(rt, qn / 2);
    let deadline = env.position(&near, &d.key(), nd).deadline;
    while env.now() < deadline {
        let step = (deadline - env.now()).min(600);
        env.warp(step);
        env.crank(&near);
    }
    let vault0 = tokens(&env, &near.keys.pool_vault);
    let meta = keeper_exit(
        &mut env,
        &near,
        &keeper,
        |_| expire_ix(&near, &kp, &d.key(), nd),
        600,
    );
    let ex: Expire = events::one(&meta);
    assert!(ex.buyback_cost > 0 && ex.max_in >= ex.b_live);
    assert_eq!(tokens(&env, &near.keys.pool_vault) - vault0, qn);
    let post = fork::pump_post_complete_buy_events(&meta);
    assert_eq!(post.len(), 1, "the pool part");
    let cs = near.curve.state(&env.svm);
    assert!(cs.complete);
    assert_eq!(cs.post_complete_base_out, qn - rt, "pcB = q − rT");
    venue_changed(&meta, VenueKind::PumpCurve, VenueKind::PumpMigrating);
    let t_mig = env.now();
    assert_eq!(market(&env, &near).migration_seen_at, t_mig);

    // The other pre-upgrade position waits (`VenueMigrating`) and settles S3 at §5.4's FV.
    assert_eq!(env.position(&near, &e.key(), ne), pos_e_v1, "untouched");
    env.warp(1);
    assert_err(
        &env.send(
            &[
                pump::cu_limit_ix(pump::CURVE_KEEPER_EXIT_CU),
                expire_ix(&near, &kp, &e.key(), ne),
            ],
            &[&keeper],
        ),
        E::VenueMigrating,
    );
    let pe = env.position(&near, &e.key(), ne);
    let due = t_mig + i64::from(pe.max_migration_pause_s) + i64::from(pe.s3_wait_s);
    let ev = loop {
        env.warp(30);
        env.crank(&near);
        let ix = sol_settle_ix(&env, &near, &kp, &e.key(), ne);
        let meta = ok(env.send(&[ix], &[&keeper]));
        if let Some(ev) = events::of::<SolSettle>(&meta).pop() {
            break ev;
        }
        assert!(env.now() < due + 60, "S3 is due at the pause's end");
    };
    assert_eq!(ev.condition, 3, "S3");
    assert!(env.now() >= due);
    let p_ema = market(&env, &near).ema_price.get();
    let b_fv = b_fv_migrating(&env, &near, &pe, p_ema);
    let fv = fair_value(&pe, env.now(), rent_due(&env, &near, &pe), b_fv);
    assert_eq!(ev.fv, fv, "FV at §5.4's ρ-scaled pool price");
    assert_eq!(ev.lender_amount, fv.min(pe.v - k_s(&pe)));
    let g = env
        .svm
        .get_account(&shortwin::constants::PUMP_GLOBAL)
        .unwrap();
    let migration_fee = shortwin_venue_state::pump::read_global(&g.owner.to_bytes(), &g.data)
        .unwrap()
        .pool_migration_fee();
    let (x1, y1) = shortwin_math::quote::migrated_pool(
        &curve(&env, &near),
        &shortwin_math::quote::MigrationParams {
            migration_fee,
            pool_tokens: u128::from(tokens(&env, &near.curve.curve_base_ata)),
            post_complete_quote_in: cs.post_complete_quote_in,
        },
    )
    .unwrap();
    assert_eq!(
        x1 + u128::from(cs.post_complete_base_out),
        shortwin::venues::STANDARD_POOL_TOKENS,
        "no donation: X₁ = 206.9e12 − pcB"
    );
    eprintln!(
        "T-SELF-UPGRADE: the 20-key close refused {code:?}; liquidation B {} (q {qt}), \
         synthetic expiry B {} (q {qn}, rT {rt}), S3 FV {} on (X1, Y1) = ({x1}, {y1})",
        liq.buyback_cost, ex.buyback_cost, ev.fv
    );
}
