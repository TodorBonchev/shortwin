//! M6's test gates on the dumped PumpSwap ELF (ARCH §8.2; R-VEN Update 2026-09-30 H4, H6, H7):
//!
//! - **T-DISABLE** (the hard gate, C0-10): the ELF halts buys under `disable_flags` bit 3 and sales
//!   under bit 4 only; short.win's route predicate reads the buy bit (`PAMM_BUY_DISABLED_BIT`):
//!   under it exits have no route and the S3 chain runs (the halt is S3 evidence), while any
//!   other non-zero value only refuses opens (A24: not S3 evidence);
//! - **T-ATA**: every PumpSwap leg names the ephemeral, non-ATA `trader_wsol` as `user`'s WSOL
//!   account, and PumpSwap takes it;
//! - **T-PDA-USER (pAMM buy path)**: PumpSwap's `buy` with the trader PDA as `user`, signed by
//!   short.win's `invoke_signed` at height 2, nothing deeper than height 3;
//! - **T-RENT (pAMM)**: with the existence requirement met the venue takes no rent on a 301-byte
//!   pool (`Open.venue_rent = 0`, `ert` refunded to the lamport); a 211-byte pool held at its
//!   minimum grows to 287 bytes on the sale (271 before pump's 2026-10-02 redeploy) and takes
//!   `minimum_balance(287) − minimum_balance(211)` from the trader, netted out of `S`, refunded
//!   to the borrower and reported in `Open.venue_rent`;
//! - **T-CASHBACK (pAMM)**: on a cashback coin short.win's trades accrue WSOL cashback in
//!   `ATA(pAMM UVA(trader), WSOL)`; after a full wipe `sweep_trader` leaves `wipe_rewards_claimed`
//!   clear while it holds any, and `[create_idempotent ATA(trader, WSOL), claim_cashback(user =
//!   trader), sweep_trader]` — signed by the fee payer alone, on a venue halted for buys —
//!   credits it to the wiped epoch's record and sets both bits (M5-SEC-1's remainder);
//! - **`sweep_trader`'s token leg** (M10 G-A1c): its six accounts strict (omitted, foreign, another
//!   holder's, swapped, trailing, the curve's five-account leg), and the cashback slot failing
//!   closed: a non-WSOL account there keeps `wipe_rewards_claimed` clear.

use shortwin::constants::{PUMP_AMM_PROGRAM_ID, TOKEN_PROGRAM_ID, WSOL_MINT};
use shortwin::events::TraderSwept;
use shortwin::state::{MARKET_FLAG_WIPE_LIVE, MARKET_FLAG_WIPE_REWARDS_CLAIMED};
use shortwin_litesvm_tests::ix;
use shortwin_math::pool::{WIPE_REWARDS_CLAIMED, WIPE_TRADER_SWEPT};
use solana_message::Message;

use crate::common::*;

// ---- T-DISABLE -----------------------------------------------------------------------------------

/// The ELF itself (a normal trader, no short.win): `buy` fails 6020 exactly under bit 3 and
/// `sell` 6021 exactly under bit 4 — the constants the adapter compiles in.
#[test]
fn t_disable_the_elf_halts_buys_on_bit_3_and_sales_on_bit_4() {
    let (mut env, p) = Env::fork_pamm_coin(PammCoin::Mature);
    let u = PammUser::new(&mut env, &p, 10 * SOL);
    let q = q_for(&env, &p) / 10;
    for bit in 0..8u8 {
        pamm::set_disable_flags(&mut env.svm, 1 << bit);
        env.warp(1);
        let buy = u.buy(&mut env, &p, q, u64::MAX);
        let sell = u.sell(&mut env, &p, q, 0);
        match bit {
            3 => assert_eq!(custom_code(&buy), Some(pamm::PAMM_ERR_DISABLED_BUY)),
            _ => {
                ok(buy);
            }
        }
        match bit {
            4 => assert_eq!(custom_code(&sell), Some(pamm::PAMM_ERR_DISABLED_SELL)),
            _ => {
                ok(sell);
            }
        }
    }
    assert_eq!(
        shortwin::venues::pump_amm::PAMM_BUY_DISABLED_BIT,
        1 << 3,
        "the compiled buy bit is the ELF's"
    );
}

/// Under the buy bit the venue cannot execute: the crank starts the S3 chain, the borrower's
/// close and a keeper's liquidation refuse `NoRoute`, and an expired position settles S3 once
/// the chain has run `s3_wait_s` (the halt is S3 evidence, ARCH §5.3). Clearing the bit clears
/// the chain.
#[test]
fn t_disable_the_buy_bit_is_no_route_and_s3_evidence() {
    let (mut env, pm, q) = setup(PammCoin::Legacy261);
    let mut b = env.borrower();
    let (_, nonce, _) = open_ok(&mut env, &pm, &mut b, q);
    pamm::set_disable_flags(&mut env.svm, 1 << 3);
    env.warp(1);
    crank(&mut env, &pm);
    let m = market(&env, &pm);
    assert_eq!(m.no_route_since, env.now(), "the S3 chain starts");
    assert_err(&env.close(&pm, &b, nonce), E::NoRoute);
    let kp = keeper(&mut env);
    assert_err(&liquidate(&mut env, &pm, &kp, &b.key(), nonce), E::NoRoute);
    // Expired, the chain unbroken for s3_wait_s: S3.
    warp_to_deadline(&mut env, &pm, &b.key(), nonce, 1);
    let wait = env.position(&pm, &b.key(), nonce).s3_wait_s;
    let mut settled = None;
    for _ in 0..(wait / 30 + 3) {
        env.warp(30);
        let meta = ok(settle(&mut env, &pm, &kp, &b.key(), nonce));
        if let Some(ev) = events::of::<SolSettle>(&meta).pop() {
            settled = Some(ev);
            break;
        }
    }
    let ev = settled.expect("S3 under the buy halt");
    assert_eq!(ev.condition, 3, "S3");
    // The halt lifted: the chain clears at the next observation.
    pamm::set_disable_flags(&mut env.svm, 0);
    env.warp(1);
    crank(&mut env, &pm);
    assert_eq!(market(&env, &pm).no_route_since, 0);
}

/// Any other non-zero value — the sell bit, `create_pool` / `deposit` / `withdraw`, bits 5–7 —
/// refuses opens (`VenueFlagged`) and is **not** S3 evidence: the venue executes, the crank keeps
/// the chain clear, and the borrower's close (a buy) executes.
#[test]
fn t_disable_other_bits_block_opens_only() {
    for bit in [0u8, 1, 2, 4, 5, 6, 7] {
        let (mut env, pm, q) = setup(PammCoin::Legacy261);
        let mut b = env.borrower();
        let (_, nonce, _) = open_ok(&mut env, &pm, &mut b, q / 2);
        pamm::set_disable_flags(&mut env.svm, 1 << bit);
        env.warp(1);
        crank(&mut env, &pm);
        assert_eq!(
            market(&env, &pm).no_route_since,
            0,
            "bit {bit}: no S3 chain"
        );
        let mut b2 = env.borrower();
        let (res, _) = env.open(&pm, &mut b2, q / 2);
        assert_err(&res, E::VenueFlagged);
        env.warp(1);
        close_ok(&mut env, &pm, &b, nonce);
    }
}

// ---- T-ATA and T-PDA-USER --------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct Inner {
    program: Pubkey,
    accounts: Vec<Pubkey>,
    height: u8,
}

/// The inner instructions of instruction `at` of `ixs`, sent by the harness payer.
fn inner(env: &Env, ixs: &[Instruction], at: usize, meta: &TransactionMetadata) -> Vec<Inner> {
    let msg = Message::new(ixs, Some(&env.payer.pubkey()));
    let keys = msg.account_keys;
    meta.inner_instructions
        .get(at)
        .map(|list| {
            list.iter()
                .map(|i| Inner {
                    program: keys[usize::from(i.instruction.program_id_index)],
                    accounts: i
                        .instruction
                        .accounts
                        .iter()
                        .map(|a| keys[usize::from(*a)])
                        .collect(),
                    height: i.stack_height,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// T-ATA and T-PDA-USER (pAMM): the open's `sell` and the close's `buy` invoke PumpSwap at height 2
/// with the trader PDA as `user` (slot 1), `trader_base_ata` (slot 5) and the ephemeral
/// `trader_wsol` — a PDA of short.win, not `ATA(trader, WSOL)` — as `user`'s WSOL account (slot
/// 6); PumpSwap takes both (the legs execute to the lamport), nothing runs deeper than height 3.
#[test]
fn t_ata_and_t_pda_user_the_trader_pda_trades_with_a_non_ata_wsol_account() {
    let (mut env, pm, q) = setup(PammCoin::Mature);
    let trader = pm.trader();
    let trader_wsol = pda::trader_wsol(&pm.market()).0;
    assert_ne!(trader_wsol, wsol_ata(&trader), "not an ATA");
    let mut b = env.borrower();
    let open = open_short_ix(
        &pm,
        &b.key(),
        0,
        q,
        0,
        u64::MAX,
        u64::MAX,
        ExtrasSel::NONE,
        &[],
    );
    let ixs = [cu_limit_ix(pamm::PAMM_TRADE_CU), open];
    let kp = b.kp.insecure_clone();
    let meta = ok(env.send(&ixs, &[&kp]));
    b.next_nonce = 1;
    {
        let label = "sell";
        let calls = inner(&env, &ixs, 1, &meta);
        let pamm_calls: Vec<&Inner> = calls
            .iter()
            .filter(|i| i.program == PUMP_AMM_PROGRAM_ID && i.height == 2)
            .collect();
        assert_eq!(pamm_calls.len(), 1, "{label}: one PumpSwap call");
        assert!(
            calls
                .iter()
                .filter(|i| i.program == PUMP_AMM_PROGRAM_ID && i.height != 2)
                .all(|i| i.height == 3),
            "{label}: PumpSwap's own event self-CPI one level down"
        );
        let c = pamm_calls[0];
        assert_eq!(c.height, 2);
        assert_eq!(c.accounts[1], trader, "{label}: user = the trader PDA");
        assert_eq!(c.accounts[5], pm.trader_base_ata);
        assert_eq!(
            c.accounts[6], trader_wsol,
            "{label}: the non-ATA trader_wsol"
        );
        assert!(calls.iter().all(|i| i.height <= 3), "{label}: height ≤ 3");
    }
    env.warp(1);
    crank(&mut env, &pm);
    let close = close_short_ix(&pm, &b.key(), 0, u64::MAX, ExtrasSel::NONE, &[]);
    let ixs = [cu_limit_ix(pamm::PAMM_TRADE_CU), close];
    let lamports0 = lamports(&env, &trader);
    assert_eq!(
        lamports0, 0,
        "the trader PDA holds nothing between instructions"
    );
    let meta = ok(env.send(&ixs, &[&kp]));
    let calls = inner(&env, &ixs, 1, &meta);
    let c = calls
        .iter()
        .find(|i| i.program == PUMP_AMM_PROGRAM_ID && i.height == 2)
        .expect("PumpSwap buy");
    assert_eq!(
        (c.height, c.accounts[1], c.accounts[6]),
        (2, trader, trader_wsol)
    );
    assert_eq!(
        c.accounts[20],
        pm.trader_uva(),
        "buy: the trader's pAMM UVA"
    );
    assert!(calls.iter().all(|i| i.height <= 3), "buy: height ≤ 3");
    assert_eq!(lamports(&env, &trader), 0);
}

// ---- T-RENT ----------------------------------------------------------------------------------------

/// T-RENT (pAMM) on a 301-byte pool: the borrower pays exactly the margin and the open fee (its
/// `ert` and the WSOL account's rent come back to the lamport), `Open.venue_rent` is 0, and the
/// close's charge is PumpSwap's quote exactly.
#[test]
fn t_rent_no_venue_rent_on_a_full_size_pool() {
    let (mut env, pm, q) = setup(PammCoin::Mature);
    let mut b = env.borrower();
    let before = lamports(&env, &b.key());
    let (open, _, _) = open_ok(&mut env, &pm, &mut b, q);
    let pos = env.position(&pm, &b.key(), open.nonce);
    let rent_pos = env
        .svm
        .minimum_balance_for_rent_exemption(ShortPosition::SPACE);
    let exposure = env
        .svm
        .get_account(&pda::exposure(&pm.market(), &b.key()).0)
        .unwrap()
        .lamports;
    let paid = before - lamports(&env, &b.key());
    assert_eq!(open.venue_rent, 0);
    assert_eq!(
        paid,
        open.m + open.fee_open + rent_pos + exposure,
        "margin, open fee and the two records' rent: nothing else (the harness payer pays the fee)"
    );
    assert!(pos.ert > 0);
}

/// T-RENT (pAMM) on the 211-byte pool held at `minimum_balance(211)` (the fixture's is
/// over-funded at the old rent: its rent is lowered to today's minimum, a plant of the venue's
/// account): PumpSwap grows it on the sale — to **287** bytes since its 2026-10-02 redeploy (the
/// fee buckets @271/@279; 271 before) — and tops its rent up from the trader —
/// `minimum_balance(287) − minimum_balance(211)` = 386,080 (304,800 to 271 bytes before) — which
/// the leg nets out of `S` (`S = sol_leg − 386,080`), reports in `Open.venue_rent`, and refunds to
/// the borrower (the venue took it out of the borrower's `ert`). As on the curve (T-RENT's curve
/// part, `pump_it::rent::a_small_sale_on_a_short_curve_is_refused`), an open whose `S_exp`
/// tolerance (`exec_tol_bps`) cannot cover the growth fails
/// `ProceedsBelowMin`, the pool untouched; the growth is paid once. Since the redeploy no size
/// inside the default wallet cap has a tolerance that covers 386,080 on this pool (2.1 % of its
/// tokens does; 1.8 % covered the 2026-09 build's 304,800). That is checked first with the
/// defaults (PUMP-2026-10-2, disclosed): the smallest covering size is `CapWallet`, and every
/// size inside the wallet cap is `ProceedsBelowMin` — the largest, found by bisection (≈ 1.81 %,
/// `S_exp` ≈ 0.3375 SOL, a tolerance of 337,498), cannot cover the growth, and the quote rises
/// with the size; the 0.1 % steps below it are each refused — so short.win's first open on a
/// 211-byte pool held at its minimum rent is refused until any trade has grown it. The growth
/// itself is then run with the wallet cap lifted within its compiled bounds (`wallet_cap_bps`
/// 10⁴, as `lock`).
#[test]
fn t_rent_a_short_pool_grows_on_the_sale() {
    // At minimum rent: the 211-byte pool's lamports planted down to `minimum_balance(211)`.
    let at_min_rent = |env: &mut Env, p: &PammPool| {
        let mut acc = env.svm.get_account(&p.pool).unwrap();
        assert_eq!(acc.data.len(), 211);
        acc.lamports = env.svm.minimum_balance_for_rent_exemption(211);
        env.svm.set_account(p.pool, acc).unwrap();
    };
    let (mut env, p) = Env::fork_pamm_coin_with(PammCoin::Legacy211, |a| {
        a.params.wallet_cap_bps = 10_000;
    });
    let r = p.reserves(&env.svm).unwrap();
    assert!(r.effective_quote < 20 * u128::from(SOL));
    let min211 = env.svm.minimum_balance_for_rent_exemption(211);
    let min287 = env
        .svm
        .minimum_balance_for_rent_exemption(pamm::POOL_LEN_2026_10);
    let growth = min287 - min211;
    assert_eq!(growth, 386_080);
    let tol = u64::from(env.config().params.exec_tol_bps);
    // The smallest size in steps of 0.1 % of the pool's 934.9 M tokens (19.13 SOL) whose 10-bps
    // tolerance covers the growth: 2.1 %, a sale of ≈ 0.39 SOL, inside Tier C's impact cap.
    let q = (1..)
        .map(|k| r.base / 1_000 * k)
        .find(|&q| quote_sell(&env, &p, q).expect("quote") * tol / 10_000 >= growth)
        .unwrap();
    assert_eq!(q, r.base / 1_000 * 21);

    // With the defaults (the same fork, market and lending): no open qualifies.
    {
        let (mut env, p) = Env::fork_pamm_coin(PammCoin::Legacy211);
        assert_eq!(env.config().params.exec_tol_bps, tol as u16);
        let pm = list_on(&mut env, &p, q * 4);
        at_min_rent(&mut env, &p);
        let try_open = |env: &mut Env, size: u64| {
            let mut b = env.borrower();
            custom_code(&env.open(&pm, &mut b, size).0)
        };
        let cap_wallet = Some(u32::from(E::CapWallet));
        let below_min = Some(u32::from(E::ProceedsBelowMin));
        assert_eq!(
            try_open(&mut env, q),
            cap_wallet,
            "the smallest covering size"
        );
        // The wallet cap's edge: `lo` passes it, `lo + 1` does not (the cap grows slower than `q`).
        let (mut lo, mut hi) = (1, q);
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            if try_open(&mut env, mid) == cap_wallet {
                hi = mid;
            } else {
                lo = mid;
            }
        }
        assert_eq!(
            try_open(&mut env, lo),
            below_min,
            "the largest size inside the cap"
        );
        let s_lo = quote_sell(&env, &p, lo).expect("quote");
        assert!(
            s_lo * tol / 10_000 < growth,
            "its tolerance {} is below the growth",
            s_lo * tol / 10_000
        );
        for k in 1.. {
            let size = r.base / 1_000 * k;
            if size > lo {
                break;
            }
            assert_eq!(try_open(&mut env, size), below_min, "{k} ‰");
        }
        eprintln!(
            "defaults: the wallet cap admits {lo} ({:.3} % of the pool, S_exp {s_lo}, tolerance \
             {}); 2.1 % is {q}",
            lo as f64 * 100.0 / r.base as f64,
            s_lo * tol / 10_000
        );
        assert_eq!(env.svm.get_account(&p.pool).unwrap().data.len(), 211);
    }

    let pm = list_on(&mut env, &p, q * 4);
    at_min_rent(&mut env, &p);
    // A sale whose tolerance is below the growth.
    let small = q / 20;
    let s_small = quote_sell(&env, &p, small).expect("quote");
    assert!(s_small * tol / 10_000 < growth);
    let mut b = env.borrower();
    let (res, _) = env.open(&pm, &mut b, small);
    assert_err(&res, E::ProceedsBelowMin);
    assert_eq!(env.svm.get_account(&p.pool).unwrap().data.len(), 211);
    env.warp(1);
    let s_exp = quote_sell(&env, &p, q).expect("quote");
    assert!(
        s_exp * tol / 10_000 >= growth,
        "the tolerance covers the growth"
    );
    let (open, _, _) = open_ok(&mut env, &pm, &mut b, q);
    assert_eq!(open.venue_rent, growth);
    assert_eq!(open.s, s_exp - growth, "S net of the venue-side rent");
    let acc = env.svm.get_account(&p.pool).unwrap();
    assert_eq!(
        (acc.data.len(), acc.lamports),
        (pamm::POOL_LEN_2026_10, min287)
    );
    assert_trader_clean(&env, &pm);
}

// ---- T-CASHBACK --------------------------------------------------------------------------------------

/// T-CASHBACK (pAMM part) and M5-SEC-1's remainder, on the dumped cashback coin: a position's
/// open and close accrue cashback in `ATA(pAMM UVA(trader), WSOL)`; a full wipe (the only lender
/// takes out every free token, a 5× gap settles the last position S1); `sweep_trader` alone sets
/// only the trader bit while the cashback is outstanding, so a deposit waits (`WipeSweepPending`);
/// with the venue halted for buys, `[create_idempotent ATA(trader, WSOL), claim_cashback(user =
/// trader), sweep_trader]` — no signature but the fee payer's — moves the cashback through
/// `ATA(trader, WSOL)` into the wiped epoch's record and sets both bits; the deposit lands.
#[test]
fn t_cashback_pumpswap_cashback_reaches_the_wiped_lenders() {
    let (mut env, pm, q) = setup(PammCoin::Cashback);
    let market_key = pm.market();
    let mut b = env.borrower();
    let (_, n0, _) = open_ok(&mut env, &pm, &mut b, q);
    env.warp(1);
    crank(&mut env, &pm);
    close_ok(&mut env, &pm, &b, n0);
    let cashback = tokens(&env, &pm.cashback_ata());
    assert!(cashback > 0, "the trader's trades accrued cashback");

    // The full wipe: a position, every free token withdrawn, a 5× gap, S1.
    let mut b2 = env.borrower();
    let (_, n2, _) = open_ok(&mut env, &pm, &mut b2, q);
    withdraw_all_free(&mut env, &pm);
    let whale = PammUser::new(&mut env, &pm.pool, 1_000 * SOL);
    ok(whale.push_price(&mut env, &pm.pool, 5, 1));
    let kp = keeper(&mut env);
    let mut s1 = None;
    for _ in 0..120 {
        env.warp(1);
        crank(&mut env, &pm);
        let meta = ok(settle(&mut env, &pm, &kp, &b2.key(), n2));
        if let Some(ev) = events::of::<SolSettle>(&meta).pop() {
            s1 = Some(ev);
            break;
        }
    }
    assert_eq!(s1.expect("settled").condition, 1, "S1");
    let m = env.market(&market_key);
    assert_eq!(m.total_shares.get(), 0);
    assert!(m.has_flag(MARKET_FLAG_WIPE_LIVE), "a full wipe");
    let cashback = tokens(&env, &pm.cashback_ata());
    assert!(cashback > 0);

    // The sweep alone: the trader bit only; a deposit waits.
    let meta = ok(env.send(&[pamm::sweep_trader_pamm_ix(&env, &pm)], &[]));
    let ev: TraderSwept = events::one(&meta);
    assert_eq!(ev.wipe_bits & WIPE_TRADER_SWEPT, WIPE_TRADER_SWEPT);
    assert_eq!(
        ev.wipe_bits & WIPE_REWARDS_CLAIMED,
        0,
        "cashback outstanding"
    );
    let amount = q;
    let lender2 = env.funded_keypair();
    let acc = fork::fund_token(
        &mut env,
        &lender2.pubkey(),
        &pm.keys.mint,
        pm.pool.token_program(),
        amount,
    );
    let k = ix::LenderKeys {
        owner: lender2.pubkey(),
        mint: pm.keys.mint,
        token_program: pm.keys.token_program,
        owner_token_account: acc,
    };
    let deposit = |env: &Env| {
        let m = env.market(&market_key);
        ix::with_remaining(
            ix::deposit(&k, amount, 0),
            &ix::epoch_records(&market_key, 0, m.share_epoch),
        )
    };
    let d0 = deposit(&env);
    assert_err(&env.send(&[d0], &[&lender2]), E::WipeSweepPending);

    // The unlocking bundle on a venue halted for buys, signed by the fee payer alone.
    pamm::set_disable_flags(&mut env.svm, 1 << 3);
    let trader = pm.trader();
    let dest = wsol_ata(&trader);
    let payer = env.payer.pubkey();
    let claim = pamm::pamm_claim_cashback_ix(&trader, &dest);
    assert!(claim.accounts.iter().all(|a| !a.is_signer));
    let lender_sol0 = env.market_vault(&market_key).lender_sol;
    let ata_rent = env.svm.minimum_balance_for_rent_exemption(165);
    let meta = ok(env.send(
        &[
            fork::create_ata_idempotent_ix(&payer, &trader, &WSOL_MINT, &TOKEN_PROGRAM_ID),
            claim,
            pamm::sweep_trader_pamm_ix(&env, &pm),
        ],
        &[],
    ));
    let ev: TraderSwept = events::one(&meta);
    assert_eq!(
        ev.lamports,
        cashback + ata_rent,
        "the cashback and the bundler's ATA rent, through the trader"
    );
    assert_eq!(
        ev.wipe_bits & (WIPE_TRADER_SWEPT | WIPE_REWARDS_CLAIMED),
        WIPE_TRADER_SWEPT | WIPE_REWARDS_CLAIMED
    );
    assert_eq!(tokens(&env, &pm.cashback_ata()), 0);
    assert!(
        env.market(&market_key)
            .has_flag(MARKET_FLAG_WIPE_REWARDS_CLAIMED)
    );
    assert_eq!(
        env.market_vault(&market_key).lender_sol - lender_sol0,
        cashback + ata_rent,
        "credited to the wiped epoch's record"
    );
    pamm::set_disable_flags(&mut env.svm, 0);
    let d1 = deposit(&env);
    ok(env.send(&[d1], &[&lender2]));
    assert!(!env.market(&market_key).has_flag(MARKET_FLAG_WIPE_LIVE));
}

/// A non-cashback market's full wipe needs no claim: the pAMM UVA's WSOL ATA does not exist (the
/// normal state), so `sweep_trader` alone sets both bits — whether or not the venue executes.
#[test]
fn t_cashback_nothing_to_claim_sets_both_bits_at_once() {
    let (mut env, pm, q) = setup(PammCoin::Legacy261);
    assert!(
        env.svm
            .get_account(&pm.cashback_ata())
            .is_none_or(|a| a.data.is_empty())
    );
    let mut b = env.borrower();
    let (_, n, _) = open_ok(&mut env, &pm, &mut b, q);
    withdraw_all_free(&mut env, &pm);
    let whale = PammUser::new(&mut env, &pm.pool, 1_000 * SOL);
    ok(whale.push_price(&mut env, &pm.pool, 5, 1));
    let kp = keeper(&mut env);
    let mut done = false;
    for _ in 0..120 {
        env.warp(1);
        crank(&mut env, &pm);
        let meta = ok(settle(&mut env, &pm, &kp, &b.key(), n));
        if !events::of::<SolSettle>(&meta).is_empty() {
            done = true;
            break;
        }
    }
    assert!(done, "settled");
    assert!(env.market(&pm.market()).has_flag(MARKET_FLAG_WIPE_LIVE));
    pamm::set_disable_flags(&mut env.svm, 1 << 3);
    let meta = ok(env.send(&[pamm::sweep_trader_pamm_ix(&env, &pm)], &[]));
    let ev: TraderSwept = events::one(&meta);
    assert_eq!(
        ev.wipe_bits & (WIPE_TRADER_SWEPT | WIPE_REWARDS_CLAIMED),
        WIPE_TRADER_SWEPT | WIPE_REWARDS_CLAIMED
    );
}

/// The seeding lender withdraws every free token (every share whose tokens `T` covers).
fn withdraw_all_free(env: &mut Env, pm: &PammMarket) {
    let m = env.market(&pm.market());
    let pool = m.pool();
    let want = m.tokens_free;
    let lender = env
        .lender(&pm.market(), &pm.lender.pubkey())
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
    let kp = pm.lender.insecure_clone();
    ok(env.send(&[ix::withdraw(&pm.lender_keys(), lo, want)], &[&kp]));
    assert_eq!(env.market(&pm.market()).tokens_free, 0);
}

// ---- sweep_trader's PumpSwap token leg (M10 G-A1c) ---------------------------------------------

/// `sweep_trader`'s PumpSwap token leg is strict (ARCH §3.9; M6): with tokens donated to
/// `trader_base_ata`, each of the leg's six accounts (`trader_base_ata`, `pool_vault`, the mint, its
/// token program, the trader's pump UVA, `ATA(pAMM UVA(trader), WSOL)`) omitted or replaced by a
/// foreign key, by another holder's real account of the same kind (the lender's token account at
/// slots 0 and 1, the WSOL mint, the other token program, another user's pump UVA, another
/// trader's cashback ATA), the first two swapped, a trailing account, the whole leg missing, and
/// the curve's five-account leg (no cashback slot) are each refused (`AccountMismatch` /
/// `BadRemainingAccounts`) with the donation where it was; the honest sweep (the control) moves it.
#[test]
fn sweep_trader_token_leg_account_rules() {
    let (mut env, pm, _) = setup(PammCoin::Cashback);
    donate_tokens(&mut env, &pm, &pm.trader_base_ata, 31_337);
    let other = PammUser::new(&mut env, &pm.pool, SOL);
    let honest = pamm::sweep_trader_pamm_ix(&env, &pm);
    let n = honest.accounts.len();
    let start = n - 6;
    assert_eq!(honest.accounts[n - 1].pubkey, pm.cashback_ata());
    let other_program = if pm.keys.token_program == TOKEN_PROGRAM_ID {
        shortwin::constants::TOKEN_2022_PROGRAM_ID
    } else {
        TOKEN_PROGRAM_ID
    };
    let others = [
        pm.lender_account,
        pm.lender_account,
        WSOL_MINT,
        other_program,
        fork::pump_user_volume_accumulator(&other.kp.pubkey()),
        wsol_ata(&pamm::pamm_uva(&other.kp.pubkey())),
    ];
    let mut cases: Vec<(String, Instruction, E)> = Vec::new();
    for (i, other_key) in others.into_iter().enumerate() {
        let mut ix = honest.clone();
        ix.accounts.remove(start + i);
        cases.push((format!("omit leg {i}"), ix, E::BadRemainingAccounts));
        let mut ix = honest.clone();
        ix.accounts[start + i].pubkey = Pubkey::new_unique();
        cases.push((format!("foreign leg {i}"), ix, E::AccountMismatch));
        let mut ix = honest.clone();
        ix.accounts[start + i].pubkey = other_key;
        cases.push((format!("another holder's leg {i}"), ix, E::AccountMismatch));
    }
    let mut ix = honest.clone();
    ix.accounts.swap(start, start + 1);
    cases.push(("leg 0 and 1 swapped".into(), ix, E::AccountMismatch));
    let mut ix = honest.clone();
    ix.accounts.swap(start + 4, start + 5);
    cases.push(("leg 4 and 5 swapped".into(), ix, E::AccountMismatch));
    let mut ix = honest.clone();
    ix.accounts
        .push(AccountMeta::new_readonly(Pubkey::new_unique(), false));
    cases.push(("a trailing account".into(), ix, E::BadRemainingAccounts));
    let mut ix = honest.clone();
    ix.accounts.truncate(start);
    cases.push(("no token leg".into(), ix, E::BadRemainingAccounts));
    let mut ix = honest.clone();
    ix.accounts.truncate(n - 1);
    cases.push((
        "the curve's five-account leg".into(),
        ix,
        E::BadRemainingAccounts,
    ));
    for (what, ix, want) in cases {
        let res = env.send(&[ix], &[]);
        assert_eq!(custom_code(&res), Some(code(want)), "{what}");
        assert_eq!(tokens(&env, &pm.trader_base_ata), 31_337, "{what}: kept");
    }
    let vault0 = tokens(&env, &pm.keys.pool_vault);
    let meta = ok(env.send(&[honest], &[]));
    let ev: TraderSwept = events::one(&meta);
    assert_eq!(ev.tokens, 31_337);
    assert_eq!(tokens(&env, &pm.keys.pool_vault) - vault0, 31_337);
    assert_trader_clean(&env, &pm);
}

/// The cashback slot fails closed (ARCH §3.9 "M6's choice"; `venues::pump_amm::cashback_claimable`;
/// M10 G-A1c): after a full wipe of a market whose pAMM UVA has no cashback ATA, an account at
/// `ATA(pAMM UVA(trader), WSOL)` that is **not** a WSOL token account under SPL Token — a token
/// account of another mint, a WSOL-layout account owned by Token-2022, an account another program
/// owns (plants: mainnet's ATA program cannot put them there) — reads as "cashback outstanding":
/// `sweep_trader` sets the trader bit only, `wipe_rewards_claimed` stays clear and a deposit waits.
/// The control: the same bytes as a WSOL account under SPL Token holding 0 set both bits (and the
/// address left empty does too: `t_cashback_nothing_to_claim_sets_both_bits_at_once`).
#[test]
fn a_non_wsol_account_at_the_cashback_slot_fails_closed() {
    let (mut env, pm, q) = setup(PammCoin::Legacy261);
    let ata = pm.cashback_ata();
    assert!(env.svm.get_account(&ata).is_none_or(|a| a.data.is_empty()));
    let mut b = env.borrower();
    let (_, n, _) = open_ok(&mut env, &pm, &mut b, q);
    withdraw_all_free(&mut env, &pm);
    let whale = PammUser::new(&mut env, &pm.pool, 1_000 * SOL);
    ok(whale.push_price(&mut env, &pm.pool, 5, 1));
    let kp = keeper(&mut env);
    let mut done = false;
    for _ in 0..120 {
        env.warp(1);
        crank(&mut env, &pm);
        let meta = ok(settle(&mut env, &pm, &kp, &b.key(), n));
        if !events::of::<SolSettle>(&meta).is_empty() {
            done = true;
            break;
        }
    }
    assert!(done, "settled");
    assert!(env.market(&pm.market()).has_flag(MARKET_FLAG_WIPE_LIVE));

    // Plants at the cashback address, each made from a real account's bytes: owner field the
    // trader's pAMM UVA, amount 0, rent-exempt for 165 bytes.
    let rent165 = env.svm.minimum_balance_for_rent_exemption(165);
    let plant = |key: &Pubkey, program: Pubkey| {
        let mut acc = env.svm.get_account(key).expect("a token account");
        acc.data[32..64].copy_from_slice(pm.trader_uva().as_ref());
        acc.data[64..72].fill(0);
        acc.lamports = rent165;
        acc.owner = program;
        acc
    };
    let other_mint = plant(&whale.base, TOKEN_PROGRAM_ID);
    assert_ne!(other_mint.data[..32], WSOL_MINT.to_bytes()[..]);
    let t22_owned = plant(&whale.wsol, shortwin::constants::TOKEN_2022_PROGRAM_ID);
    let foreign_owned = plant(&whale.wsol, PUMP_AMM_PROGRAM_ID);
    // The same bytes under SPL Token are a valid, empty WSOL account: nothing claimable.
    let real_empty = plant(&whale.wsol, TOKEN_PROGRAM_ID);
    let rewards = |env: &Env| {
        env.market(&pm.market())
            .has_flag(MARKET_FLAG_WIPE_REWARDS_CLAIMED)
    };
    for (what, acc) in [
        ("a token account of the base mint", other_mint),
        ("a WSOL account owned by Token-2022", t22_owned),
        ("a WSOL-layout account owned by pAMM", foreign_owned),
    ] {
        env.svm.set_account(ata, acc).unwrap();
        env.warp(1);
        let meta = ok(env.send(&[pamm::sweep_trader_pamm_ix(&env, &pm)], &[]));
        let ev: TraderSwept = events::one(&meta);
        assert_eq!(
            ev.wipe_bits & WIPE_TRADER_SWEPT,
            WIPE_TRADER_SWEPT,
            "{what}: the trader bit"
        );
        assert_eq!(
            ev.wipe_bits & WIPE_REWARDS_CLAIMED,
            0,
            "{what}: fails closed"
        );
        assert!(!rewards(&env), "{what}");
    }
    // The control: the same bytes under SPL Token, an empty WSOL account: nothing claimable, both
    // bits (one control per wipe: the bit stays set once set).
    env.svm.set_account(ata, real_empty).unwrap();
    env.warp(1);
    let meta = ok(env.send(&[pamm::sweep_trader_pamm_ix(&env, &pm)], &[]));
    let ev: TraderSwept = events::one(&meta);
    assert_eq!(
        ev.wipe_bits & WIPE_REWARDS_CLAIMED,
        WIPE_REWARDS_CLAIMED,
        "an empty WSOL account: both bits"
    );
    assert_eq!(ev.wipe_bits & WIPE_TRADER_SWEPT, WIPE_TRADER_SWEPT);
    assert!(rewards(&env));
}
