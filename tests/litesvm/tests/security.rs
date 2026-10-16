//! M1 security review regressions (`M1-SEC-n`): each finding's reproducer, rewritten to assert
//! the fixed behaviour (for M1-SEC-1, the owner's decision: the Emergency vault cancels
//! parameter changes only),
//! and the review's refutation tests for attacks the program withstands (`ok_*`). The lender
//! side of M1-SEC-2 / M1-SEC-3 is `tests/lender_it/security.rs`.
//!
//! Every test sends through `Env::send`, so the invariant hooks run, except where a test plants
//! an account that cannot exist on mainnet (said so where it happens).

#![allow(clippy::result_large_err)]

use anchor_lang::error::ErrorCode as AnchorErrorCode;
use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::bpf_loader_upgradeable;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use anchor_lang::{Discriminator, InstructionData};
use shortwin::PauseScope;
use shortwin::constants::{
    CONFIG_APPLY_WINDOW_S, DEFAULT_TIMELOCK_S, MAX_TIMELOCK_S, MEMO_PROGRAM_ID, PYTH_RECEIVER_ID,
    PYTH_SOL_USD_ACCOUNT, PYTH_SOL_USD_FEED_ID, TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID,
};
use shortwin::errors::ShortwinError;
use shortwin::state::{ConfigChange, Params, TierParams};
use shortwin_litesvm_tests::errors::{
    assert_anchor_err, assert_err, assert_failed, custom_code, ok,
};
use shortwin_litesvm_tests::fixtures::MarketKeys;
use shortwin_litesvm_tests::ix::{self, LenderKeys};
use shortwin_litesvm_tests::tokens::{self, Holder, TokenProgram};
use shortwin_litesvm_tests::{Env, TxResult, events, fork, pda, read};
use shortwin_venue_state::pyth::{self, PriceUpdate};
use solana_account::Account;
use solana_keypair::Keypair;
use solana_signer::Signer;

// ---- helpers ------------------------------------------------------------------------------

fn propose(env: &mut Env, change: ConfigChange) -> TxResult {
    let gov = env.governance.insecure_clone();
    env.send(&[ix::propose_config(&gov.pubkey(), change)], &[&gov])
}

/// `apply_config` with the optional accounts the pending change needs.
fn apply(env: &mut Env) -> TxResult {
    let apply = env.apply_config_ix();
    env.send(&[apply], &[])
}

fn propose_and_apply(env: &mut Env, change: ConfigChange) {
    ok(propose(env, change));
    let t = env.config().timelock_s;
    env.warp(i64::from(t));
    ok(apply(env));
}

fn authorities(env: &Env, pause: Pubkey, fee_recipient: Pubkey) -> ConfigChange {
    ConfigChange::Authorities {
        governance: env.governance.pubkey(),
        pause_authority: pause,
        coverage_authority: Pubkey::default(),
        fee_recipient,
    }
}

struct Listed {
    k: MarketKeys,
    authority: Keypair,
}

fn market_with_holder(env: &mut Env, program: TokenProgram, amount: u64) -> (Listed, Holder) {
    let (k, authority) = env.install_market(program, 6);
    let payer = env.payer.insecure_clone();
    let h = tokens::holder(&mut env.svm, &payer, &k.mint, &authority, amount, program);
    (Listed { k, authority }, h)
}

fn lender_keys(k: &MarketKeys, h: &Holder) -> LenderKeys {
    LenderKeys {
        owner: h.owner.pubkey(),
        mint: k.mint,
        token_program: k.token_program,
        owner_token_account: h.token_account,
    }
}

/// A new mint of `program` (6 decimals) and a creator for it.
fn new_mint(env: &mut Env, program: TokenProgram) -> (Pubkey, Keypair) {
    let creator = env.funded_keypair();
    let authority = Keypair::new();
    let payer = env.payer.insecure_clone();
    let mint = tokens::create_mint(&mut env.svm, &payer, &authority.pubkey(), 6, program);
    (mint, creator)
}

/// Lists a mint through the real `create_market` (the creation fee lands in `protocol_fees`).
fn create_market_for(env: &mut Env, program: TokenProgram) -> Pubkey {
    let (mint, creator) = new_mint(env, program);
    ok(env.send(
        &[ix::create_market(
            &creator.pubkey(),
            &mint,
            &program.id(),
            0,
            &pda::canonical_pool(&mint),
        )],
        &[&creator],
    ));
    pda::market(&mint).0
}

// ---- M1-SEC-1: the Emergency vault's cancel power (owner decision: params, not keys) -------

/// The Emergency vault may cancel a pending parameter change, but not an `Authorities` or
/// `Timelock` change, so Governance can always rotate a compromised Emergency key once the
/// timelock has run (owner decision on M1-SEC-1, SPEC §11). Governance may cancel anything.
#[test]
fn sec_1_emergency_cancels_params_but_not_keys_or_timelock() {
    let mut env = Env::initialized();
    let pause = env.pause_authority.insecure_clone();
    let gov = env.governance.insecure_clone();
    let fee = env.fee_recipient.pubkey();

    // Parameter changes: the Emergency's veto still works.
    let mut params = Params::defaults();
    params.open_fee_bps = 30;
    ok(propose(
        &mut env,
        ConfigChange::Params {
            params,
            tiers: TierParams::defaults(),
        },
    ));
    ok(env.send(&[ix::cancel_config(&pause.pubkey())], &[&pause]));
    env.warp(i64::from(DEFAULT_TIMELOCK_S));
    assert_err(&apply(&mut env), ShortwinError::NoPendingChange);
    assert_eq!(
        env.config().params.open_fee_bps,
        Params::defaults().open_fee_bps
    );

    // A Timelock change: the Emergency cannot cancel it; Governance can.
    ok(propose(
        &mut env,
        ConfigChange::Timelock {
            timelock_s: DEFAULT_TIMELOCK_S + 3_600,
        },
    ));
    assert_err(
        &env.send(&[ix::cancel_config(&pause.pubkey())], &[&pause]),
        ShortwinError::EmergencyCannotCancel,
    );
    ok(env.send(&[ix::cancel_config(&gov.pubkey())], &[&gov]));
    assert_err(&apply(&mut env), ShortwinError::NoPendingChange);

    // An Authorities change rotating the Emergency out: its cancel fails at the last second of
    // the timelock, and the rotation applies.
    let new_pause = env.funded_keypair();
    let change = authorities(&env, new_pause.pubkey(), fee);
    ok(propose(&mut env, change));
    env.warp(i64::from(DEFAULT_TIMELOCK_S) - 1);
    assert_err(
        &env.send(&[ix::cancel_config(&pause.pubkey())], &[&pause]),
        ShortwinError::EmergencyCannotCancel,
    );
    env.warp(1);
    ok(apply(&mut env));
    assert_eq!(env.config().pause_authority, new_pause.pubkey(), "rotated");
}

// ---- M1-SEC-4: `timelock_s` has a compiled ceiling ------------------------------------------

/// The review's unit slip (48 h in milliseconds) and `u32::MAX` are refused at proposal and at
/// initialisation (`TimelockTooLong`); the ceiling itself (30 days) is accepted, and the fix
/// after it takes at most 30 days.
#[test]
fn sec_4_timelock_has_a_compiled_ceiling() {
    let mut env = Env::initialized();
    for too_long in [172_800_000, u32::MAX, MAX_TIMELOCK_S + 1] {
        let res = propose(
            &mut env,
            ConfigChange::Timelock {
                timelock_s: too_long,
            },
        );
        assert_err(&res, ShortwinError::TimelockTooLong);
    }
    propose_and_apply(
        &mut env,
        ConfigChange::Timelock {
            timelock_s: MAX_TIMELOCK_S,
        },
    );
    assert_eq!(env.config().timelock_s, MAX_TIMELOCK_S);
    ok(propose(
        &mut env,
        ConfigChange::Timelock {
            timelock_s: DEFAULT_TIMELOCK_S,
        },
    ));
    env.warp(i64::from(MAX_TIMELOCK_S));
    ok(apply(&mut env));
    assert_eq!(env.config().timelock_s, DEFAULT_TIMELOCK_S);

    let mut env = Env::new();
    let mut args = env.default_init_args();
    args.timelock_s = 172_800_000;
    assert_err(&env.initialize_config(args), ShortwinError::TimelockTooLong);
    let mut args = env.default_init_args();
    args.timelock_s = MAX_TIMELOCK_S;
    ok(env.initialize_config(args));
}

// ---- M1-SEC-5: the fee recipient is validated; the sweep never bricks ----------------------

/// (a) A sysvar or a program as recipient: `apply_config` takes the recipient as a `mut`
/// account, so the runtime's write-lock demotion refuses a sysvar (`ConstraintMut`), and
/// `check_fee_recipient` refuses anything not System-owned (a program, another market's
/// `MarketVault`) with `InvalidParams`. The account must be passed and match. The old
/// recipient keeps working throughout.
#[test]
fn sec_5_a_unusable_fee_recipients_are_refused_at_apply() {
    let mut env = Env::initialized();
    let pause = env.pause_authority.pubkey();
    let other_market = create_market_for(&mut env, TokenProgram::Spl);
    let other_vault = pda::market_vault(&other_market).0;
    let other_pool_vault = pda::pool_vault(&other_market).0;
    let clock_sysvar = anchor_lang::prelude::pubkey!("SysvarC1ock11111111111111111111111111111111");
    // `Some(code)`: that error; `None`: the runtime refuses the write lock (`ConstraintMut`);
    // a program id may also be refused by the runtime before the program runs.
    for (recipient, want) in [
        (
            clock_sysvar,
            Some(u32::from(AnchorErrorCode::ConstraintMut)),
        ),
        (other_vault, Some(u32::from(ShortwinError::InvalidParams))),
        (
            other_pool_vault,
            Some(u32::from(ShortwinError::InvalidParams)),
        ),
        (TOKEN_PROGRAM_ID, None),
    ] {
        let change = authorities(&env, pause, recipient);
        ok(propose(&mut env, change));
        env.warp(i64::from(DEFAULT_TIMELOCK_S));
        let res = apply(&mut env);
        match want {
            Some(code) => assert_eq!(
                shortwin_litesvm_tests::errors::custom_code(&res),
                Some(code),
                "{recipient}"
            ),
            None => assert_failed(&res),
        }
        // Without the account, or with another one: refused as well.
        let res = env.send(&[ix::apply_config()], &[]);
        assert_err(&res, ShortwinError::AccountMismatch);
        let res = env.send(
            &[ix::apply_config_with(
                Some(env.fee_recipient.pubkey()),
                false,
            )],
            &[],
        );
        assert_err(&res, ShortwinError::AccountMismatch);
        assert_eq!(env.config().fee_recipient, env.fee_recipient.pubkey());
    }
    // The configured recipient still receives the other market's creation fee.
    let recipient = env.fee_recipient.pubkey();
    let before = read::lamports(&env.svm, &recipient);
    ok(env.send(&[ix::sweep_protocol_fees(&other_market, &recipient)], &[]));
    assert_eq!(
        read::lamports(&env.svm, &recipient),
        before + Params::defaults().market_creation_fee
    );
}

/// (b) A fresh System key (0 lamports) is a valid recipient, but a sweep below its rent
/// minimum would fail the whole transaction: the sweep then moves nothing and keeps
/// `protocol_fees`, and pays once the fees cover the minimum.
#[test]
fn sec_5_b_a_sweep_below_the_recipients_rent_minimum_waits() {
    let mut env = Env::initialized();
    let mut params = Params::defaults();
    params.market_creation_fee = 100_000; // bounds [0, 10⁸]
    propose_and_apply(
        &mut env,
        ConfigChange::Params {
            params,
            tiers: TierParams::defaults(),
        },
    );
    let fresh = Pubkey::new_unique();
    let pause = env.pause_authority.pubkey();
    let change = authorities(&env, pause, fresh);
    propose_and_apply(&mut env, change);
    assert_eq!(env.config().fee_recipient, fresh);
    let market = create_market_for(&mut env, TokenProgram::Token2022);

    let meta = ok(env.send(&[ix::sweep_protocol_fees(&market, &fresh)], &[]));
    let swept = events::one::<shortwin::events::ProtocolFeesSwept>(&meta);
    assert_eq!(swept.amount, 0);
    assert_eq!(env.market_vault(&market).protocol_fees, 100_000);
    assert_eq!(read::lamports(&env.svm, &fresh), 0);

    // Once the recipient exists (anyone can fund it), the sweep pays.
    env.fund(&fresh, 1_000_000);
    let meta = ok(env.send(&[ix::sweep_protocol_fees(&market, &fresh)], &[]));
    assert_eq!(
        events::one::<shortwin::events::ProtocolFeesSwept>(&meta).amount,
        100_000
    );
    assert_eq!(env.market_vault(&market).protocol_fees, 0);
    assert_eq!(read::lamports(&env.svm, &fresh), 1_100_000);
}

/// (b′) Fees that alone cover the rent minimum create the recipient in one sweep.
#[test]
fn sec_5_c_a_sweep_that_covers_the_rent_minimum_creates_the_recipient() {
    let mut env = Env::initialized();
    let fresh = Pubkey::new_unique();
    let pause = env.pause_authority.pubkey();
    let change = authorities(&env, pause, fresh);
    propose_and_apply(&mut env, change);
    let market = create_market_for(&mut env, TokenProgram::Spl);
    let fee = Params::defaults().market_creation_fee;
    assert!(fee >= env.svm.minimum_balance_for_rent_exemption(0));
    ok(env.send(&[ix::sweep_protocol_fees(&market, &fresh)], &[]));
    assert_eq!(read::lamports(&env.svm, &fresh), fee);
    assert_eq!(env.market_vault(&market).protocol_fees, 0);
}

// ---- M1-SEC-6 / M1-FID-6: every `Market` account is bound to its PDA -----------------------

/// `ConstraintSeeds`, raised on the `market` account.
#[track_caller]
fn refused_by_market_seeds(res: &TxResult) {
    assert_anchor_err(res, AnchorErrorCode::ConstraintSeeds);
    let logs = res.as_ref().err().unwrap().meta.logs.join("\n");
    assert!(
        logs.contains("caused by account: market."),
        "not the market's seeds:\n{logs}"
    );
}

/// A copy of a `Market` planted at a non-PDA address (the harness can, mainnet cannot) is
/// refused with `ConstraintSeeds` by all five instructions that took `market` with only
/// `AccountLoader`'s owner and discriminator checks.
#[test]
fn sec_6_market_accounts_are_seed_checked() {
    let mut env = Env::initialized();
    let (k, h) = {
        let (l, h) = market_with_holder(&mut env, TokenProgram::Spl, 1_000_000);
        (l.k, h)
    };
    let owner = h.owner.insecure_clone();
    ok(env.send(
        &[ix::deposit(&lender_keys(&k, &h), 1_000_000, 0)],
        &[&owner],
    ));
    let fake = Pubkey::new_unique();
    let acc = env.svm.get_account(&k.market).unwrap();
    env.svm.set_account(fake, acc).unwrap();
    // The planted account is not a real market (its vault seeds do not match): no hooks.
    env.check_invariants = false;

    let pause = env.pause_authority.insecure_clone();
    let res = env.send(
        &[ix::set_pause(
            &pause.pubkey(),
            PauseScope::Market,
            true,
            Some(fake),
        )],
        &[&pause],
    );
    refused_by_market_seeds(&res);

    // claim_sol, cover_deficit, sync_lender, sweep_protocol_fees with the fake in the market
    // slot (the other accounts are the real market's, so only the market's seeds can fail).
    let with_fake = |mut ix: Instruction| {
        for m in ix.accounts.iter_mut() {
            if m.pubkey == k.market {
                m.pubkey = fake;
            }
        }
        ix
    };
    let recipient = env.fee_recipient.pubkey();
    let cases = [
        (with_fake(ix::claim_sol(&owner.pubkey(), &k.market)), true),
        (
            with_fake(ix::cover_deficit(&owner.pubkey(), &k.market, 1)),
            true,
        ),
        (
            with_fake(ix::sync_lender(&owner.pubkey(), &k.market, 0)),
            false,
        ),
        (
            with_fake(ix::sweep_protocol_fees(&k.market, &recipient)),
            false,
        ),
    ];
    for (bad, signed) in cases {
        let signers: &[&Keypair] = if signed { &[&owner] } else { &[] };
        let res = env.send(&[bad], signers);
        // The `market` field's own seeds refuse it (other accounts' seeds derived from the fake
        // key would fail too, so the log names the account).
        refused_by_market_seeds(&res);
    }
    // The real market passes the same instructions.
    env.check_invariants = true;
    ok(env.send(
        &[ix::set_pause(
            &pause.pubkey(),
            PauseScope::Market,
            true,
            Some(k.market),
        )],
        &[&pause],
    ));
    ok(env.send(&[ix::claim_sol(&owner.pubkey(), &k.market)], &[&owner]));
    ok(env.send(&[ix::sync_lender(&owner.pubkey(), &k.market, 0)], &[]));
    ok(env.send(&[ix::sweep_protocol_fees(&k.market, &recipient)], &[]));
}

// ---- M1-SEC-7: withdraw serves memo-required destinations (ARCH §3.3) ----------------------

/// A Token-2022 account with `MemoTransfer` enabled, owned by `owner`.
fn memo_account(env: &mut Env, mint: &Pubkey, owner: &Keypair) -> Pubkey {
    use spl_token_2022_interface as t22;
    use t22::extension::ExtensionType;
    let acct = Keypair::new();
    let space = ExtensionType::try_calculate_account_len::<t22::state::Account>(&[
        ExtensionType::MemoTransfer,
    ])
    .unwrap();
    let lamports = env.svm.minimum_balance_for_rent_exemption(space);
    let payer = env.payer.pubkey();
    let ixs = vec![
        anchor_lang::solana_program::system_instruction::create_account(
            &payer,
            &acct.pubkey(),
            lamports,
            space as u64,
            &TOKEN_2022_PROGRAM_ID,
        ),
        t22::instruction::initialize_account3(
            &TOKEN_2022_PROGRAM_ID,
            &acct.pubkey(),
            mint,
            &owner.pubkey(),
        )
        .unwrap(),
        t22::extension::memo_transfer::instruction::enable_required_transfer_memos(
            &TOKEN_2022_PROGRAM_ID,
            &acct.pubkey(),
            &owner.pubkey(),
            &[],
        )
        .unwrap(),
    ];
    ok(env.send(&ixs, &[&acct, owner]));
    acct.pubkey()
}

/// The environment runs mainnet's Token-2022 (trimmed ELF `7ea94a02…`, the verified build of
/// `solana-program/token-2022@9bc02757`, from its real ProgramData) and spl-memo v3 (BPFLoader2,
/// `f520eaf0…`), not LiteSVM's bundled builds (M10 T-MEMO). Pinned here independently of the
/// harness's own check. Skipped (with a note) under `SHORTWIN_BUNDLED_SPL=1`.
fn assert_mainnet_spl(env: &Env) {
    if fork::bundled_spl() {
        eprintln!("[t-memo] SHORTWIN_BUNDLED_SPL=1: LiteSVM's bundled Token-2022 / spl-memo v3");
        return;
    }
    let program = env.svm.get_account(&TOKEN_2022_PROGRAM_ID).unwrap();
    assert_eq!(program.owner, bpf_loader_upgradeable::ID);
    let pd_key = Pubkey::find_program_address(
        &[TOKEN_2022_PROGRAM_ID.as_ref()],
        &bpf_loader_upgradeable::ID,
    )
    .0;
    assert_eq!(
        pd_key.to_string(),
        "DoU57AYuPFu2QU514RktNPG22QhApEjnKxnBcu4BHDTY"
    );
    let pd = env.svm.get_account(&pd_key).unwrap();
    assert_eq!(pd.data.len(), 1_382_061, "the mainnet ProgramData's space");
    assert_eq!(&pd.data[..4], &3u32.to_le_bytes(), "ProgramData tag");
    assert_eq!(
        u64::from_le_bytes(pd.data[4..12].try_into().unwrap()),
        427_147_035,
        "deploy slot"
    );
    assert_eq!(pd.data[12], 1, "an upgrade authority");
    assert_eq!(
        Pubkey::new_from_array(pd.data[13..45].try_into().unwrap()).to_string(),
        "AeLmXCbPaQHGWRLr2saFsEVfmMNuKnxRAbWCT9P5twgz"
    );
    assert_eq!(
        fork::trimmed_elf_sha256(&pd.data),
        "7ea94a027005b39196fa08e6d0ddcd55ec32eb43266179921a834a1ba2eb1947",
        "Token-2022: not the mainnet build"
    );
    let memo = env.svm.get_account(&MEMO_PROGRAM_ID).unwrap();
    assert_eq!(
        memo.owner.to_string(),
        "BPFLoader2111111111111111111111111111111111"
    );
    assert!(memo.executable);
    assert_eq!(
        fork::sha256_hex(&memo.data),
        "f520eaf096361abbb9639ea4dc3e5388a87b9330e121f476607b87c46ef67954",
        "spl-memo v3: not the mainnet build"
    );
}

/// The `consumed` CU of each invocation of `program` at stack height `depth` in `logs`, in order.
fn consumed_at(logs: &[String], program: &Pubkey, depth: usize) -> Vec<u64> {
    let invoke = format!("Program {program} invoke [{depth}]");
    let consumed = format!("Program {program} consumed ");
    // One entry per open invocation: is it `program` at `depth`?
    let mut frames: Vec<bool> = Vec::new();
    let mut out = Vec::new();
    for l in logs {
        if let Some(rest) = l.strip_prefix(&consumed) {
            if frames.last() == Some(&true) {
                out.push(rest.split(' ').next().unwrap().parse().unwrap());
            }
        } else if l.starts_with("Program ") && l.contains(" invoke [") {
            frames.push(*l == invoke);
        } else if l.starts_with("Program ") && (l.ends_with(" success") || l.contains(" failed")) {
            frames.pop();
        }
    }
    out
}

/// Without the memo program the withdrawal names the problem (`MemoProgramRequired`); with it
/// the program CPIs the memo right before `transfer_checked`, and Token-2022 accepts the CPI'd
/// memo as the transfer's previous sibling (T-MEMO, U17). Since M10 this runs on the programs
/// mainnet runs ([`assert_mainnet_spl`]): the logs show spl-memo v3's `invoke [2]` before
/// Token-2022's `invoke [2]`, the memo at 7,928 CU (asserted above 5,000), `transfer_checked` at
/// exactly 2,487 and the whole withdrawal at about 42,700 CU (printed; it varies by a few tens of
/// CU with the random keys). On LiteSVM's bundled builds (`SHORTWIN_BUNDLED_SPL=1`, the CU
/// assertions skipped) the same figures are 287, 2,470 and about 35,000. A plain destination
/// needs no memo program, and a zero-token withdrawal moves nothing, so it needs none either.
#[test]
fn sec_7_memo_required_destination_is_served() {
    let mut env = Env::initialized();
    assert_mainnet_spl(&env);
    let (l, h) = market_with_holder(&mut env, TokenProgram::Token2022, 1_000_000);
    let owner = h.owner.insecure_clone();
    let lk = lender_keys(&l.k, &h);
    ok(env.send(&[ix::deposit(&lk, 1_000_000, 0)], &[&owner]));
    let memo_acct = memo_account(&mut env, &l.k.mint, &owner);
    let mut to_memo = lk;
    to_memo.owner_token_account = memo_acct;

    let shares = env.lender(&l.k.market, &owner.pubkey()).unwrap().shares;
    let res = env.send(&[ix::withdraw(&to_memo, shares / 2, 0)], &[&owner]);
    assert_err(&res, ShortwinError::MemoProgramRequired);

    // 1 share pays 0 tokens: no transfer, no memo needed.
    ok(env.send(&[ix::withdraw(&to_memo, 1, 0)], &[&owner]));

    let meta = ok(env.send(
        &[ix::withdraw_with(&to_memo, shares / 2, 1, 0, true)],
        &[&owner],
    ));
    let got = read::token_amount(&env.svm, &memo_acct).unwrap();
    assert_eq!(got, events::one::<shortwin::events::Withdraw>(&meta).amount);
    assert!(got > 0);
    let logs = meta.logs.join("\n");
    assert!(logs.contains("short.win withdraw"), "{logs}");
    assert_eq!(events::raw(&meta).len(), 1, "one event CPI");
    // The memo CPI, then the transfer CPI, both at stack height 2 (siblings).
    let at = |p: &str| {
        meta.logs
            .iter()
            .position(|l| l == &format!("Program {p} invoke [2]"))
    };
    let memo_at = at(&MEMO_PROGRAM_ID.to_string()).expect("memo invoke [2]");
    let xfer_at = at(&TOKEN_2022_PROGRAM_ID.to_string()).expect("Token-2022 invoke [2]");
    assert!(memo_at < xfer_at, "{logs}");
    let memo_cu = consumed_at(&meta.logs, &MEMO_PROGRAM_ID, 2);
    let xfer_cu = consumed_at(&meta.logs, &TOKEN_2022_PROGRAM_ID, 2);
    eprintln!(
        "[t-memo] memo-required withdraw: {} CU in all; spl-memo v3 {memo_cu:?} CU, Token-2022 \
         transfer_checked {xfer_cu:?} CU",
        meta.compute_units_consumed
    );
    if !fork::bundled_spl() {
        // What the VM executes, not just the account bytes `assert_mainnet_spl` hashes (a program
        // cache that kept the bundled ELF would pass that check). Mainnet's spl-memo v3 is a
        // 74,800-byte ELF; LiteSVM bundles spl-memo 4.0.0 (2,304 bytes, a few hundred CU) at the
        // same address. Mainnet's Token-2022 spends 2,487 CU on this `transfer_checked`, LiteSVM's
        // bundled v11 build 2,470.
        assert_eq!(memo_cu.len(), 1);
        assert!(memo_cu[0] > 5_000, "{memo_cu:?}");
        assert_eq!(
            xfer_cu,
            [2_487],
            "Token-2022 transfer_checked CU: not mainnet's ELF?"
        );
    }

    // A plain destination works without the memo program (and with it, the memo is skipped).
    let rest = env.lender(&l.k.market, &owner.pubkey()).unwrap().shares;
    ok(env.send(&[ix::withdraw_with(&lk, rest / 2, 1, 0, true)], &[&owner]));
    let rest = env.lender(&l.k.market, &owner.pubkey()).unwrap().shares;
    ok(env.send(&[ix::withdraw(&lk, rest, 1)], &[&owner]));

    // A wrong account in the memo slot is refused.
    let (l2, h2) = market_with_holder(&mut env, TokenProgram::Token2022, 1_000);
    let o2 = h2.owner.insecure_clone();
    let lk2 = lender_keys(&l2.k, &h2);
    ok(env.send(&[ix::deposit(&lk2, 1_000, 0)], &[&o2]));
    let mut bad = ix::withdraw_with(&lk2, 1_000, 0, 0, true);
    let slot = bad
        .accounts
        .iter()
        .position(|m| m.pubkey == MEMO_PROGRAM_ID)
        .unwrap();
    bad.accounts[slot].pubkey = TOKEN_PROGRAM_ID;
    assert_err(&env.send(&[bad], &[&o2]), ShortwinError::AccountMismatch);
    let _ = l.authority;
}

/// T-MEMO control (M10, TMEMO-3): Token-2022's sibling check is live in the mainnet ELF the
/// environment runs, so sec_7 passing means the CPI'd memo satisfies it (not that the check is
/// missing). A top-level `transfer_checked` into a memo-required account fails `NoMemo` (36)
/// alone, passes right after a memo (spl-memo v3 or v1), and fails again when another
/// instruction sits between the memo and the transfer.
#[test]
fn t_memo_control_top_level_sibling_check() {
    use spl_token_2022_interface as t22;
    let mut env = Env::initialized();
    assert_mainnet_spl(&env);
    let (l, h) = market_with_holder(&mut env, TokenProgram::Token2022, 1_000_000);
    let owner = h.owner.insecure_clone();
    let dest = memo_account(&mut env, &l.k.mint, &owner);
    let xfer = |amount: u64| {
        t22::instruction::transfer_checked(
            &TOKEN_2022_PROGRAM_ID,
            &h.token_account,
            &l.k.mint,
            &dest,
            &owner.pubkey(),
            &[],
            amount,
            6,
        )
        .unwrap()
    };
    let memo = |program: Pubkey, text: &[u8]| Instruction {
        program_id: program,
        accounts: vec![],
        data: text.to_vec(),
    };
    let v1: Pubkey = "Memo1UhkJRfHyvLMcVucJwxXeuD728EqVDDwQDxFMNo"
        .parse()
        .unwrap();

    let res = env.send(&[xfer(1)], &[&owner]);
    assert_eq!(custom_code(&res), Some(36), "no memo: NoMemo");
    let meta = ok(env.send(&[memo(MEMO_PROGRAM_ID, b"t-memo v3"), xfer(2)], &[&owner]));
    let memo_cu = consumed_at(&meta.logs, &MEMO_PROGRAM_ID, 1);
    eprintln!("[t-memo] top-level spl-memo v3: {memo_cu:?} CU");
    ok(env.send(&[memo(v1, b"t-memo v1"), xfer(3)], &[&owner]));
    let gap = anchor_lang::solana_program::system_instruction::transfer(
        &env.payer.pubkey(),
        &owner.pubkey(),
        1,
    );
    let res = env.send(
        &[memo(MEMO_PROGRAM_ID, b"t-memo gap"), gap, xfer(4)],
        &[&owner],
    );
    assert_eq!(
        custom_code(&res),
        Some(36),
        "memo not the previous sibling: NoMemo"
    );
    assert_eq!(read::token_amount(&env.svm, &dest).unwrap(), 5);
    let _ = l.authority;
}

// ---- M1-SEC-8: `crank_sol_usd` refuses future-dated updates and odd exponents --------------

const DUMP: &[u8] = include_bytes!("../../fixtures/accounts/pyth_sol_usd_shard0.bin");
const DUMP_TS: i64 = 1_790_523_064;
const DUMP_SLOT: u64 = 451_037_803;

fn put_update(env: &mut Env, key: Pubkey, u: &PriceUpdate) {
    let data = pyth::encode(u).to_vec();
    let lamports = env.svm.minimum_balance_for_rent_exemption(data.len());
    env.svm
        .set_account(
            key,
            Account {
                lamports,
                data,
                owner: PYTH_RECEIVER_ID,
                executable: false,
                rent_epoch: 0,
            },
        )
        .unwrap();
}

/// The review's update dated one hour ahead is refused (`OracleFutureDated`), so it can no
/// longer block honest ones; an update up to `oracle_max_age_s` ahead (a cluster clock that
/// lags the publishers) is still accepted, and one exponent outside `[−18, 0]` is refused.
#[test]
fn sec_8_future_dated_updates_are_refused() {
    let mut env = Env::initialized();
    env.set_clock(DUMP_TS, DUMP_SLOT);
    let max_age = i64::from(env.config().params.oracle_max_age_s);
    let u = pyth::read(&pyth::RECEIVER, DUMP).unwrap();
    let dated = |t: i64| PriceUpdate {
        publish_time: t,
        prev_publish_time: t - 1,
        ..u
    };
    put_update(&mut env, PYTH_SOL_USD_ACCOUNT, &dated(DUMP_TS + 3_600));
    let crank = ix::crank_sol_usd(&PYTH_SOL_USD_ACCOUNT);
    assert_err(
        &env.send(std::slice::from_ref(&crank), &[]),
        ShortwinError::OracleFutureDated,
    );
    put_update(
        &mut env,
        PYTH_SOL_USD_ACCOUNT,
        &dated(DUMP_TS + max_age + 1),
    );
    assert_err(
        &env.send(std::slice::from_ref(&crank), &[]),
        ShortwinError::OracleFutureDated,
    );
    assert_eq!(env.oracle_cache().publish_time, 0, "nothing cached");
    // An honest update 30 s later is accepted.
    env.warp(30);
    put_update(&mut env, PYTH_SOL_USD_ACCOUNT, &dated(DUMP_TS + 30));
    ok(env.send(std::slice::from_ref(&crank), &[]));
    assert_eq!(env.oracle_cache().publish_time, DUMP_TS + 30);
    // Up to max_age ahead of the clock is tolerated.
    env.warp(1);
    let now = env.now();
    put_update(&mut env, PYTH_SOL_USD_ACCOUNT, &dated(now + max_age));
    ok(env.send(std::slice::from_ref(&crank), &[]));
    // An exponent outside [−18, 0] (checked before the timing rules).
    env.warp(1);
    let odd = PriceUpdate {
        exponent: 3,
        ..dated(env.now())
    };
    put_update(&mut env, PYTH_SOL_USD_ACCOUNT, &odd);
    assert_err(&env.send(&[crank], &[]), ShortwinError::OracleFeed);
}

// ---- M1-SEC-9: pending changes lapse ------------------------------------------------------

/// A pending change can be applied from `not_before` for `CONFIG_APPLY_WINDOW_S` (14 days),
/// not after (`PendingChangeExpired`); an expired change can be cancelled or proposed again.
#[test]
fn sec_9_pending_changes_lapse_after_the_apply_window() {
    let kink = ConfigChange::Kink {
        util_kink_bps: 7_000,
    };
    // The review's 2 years: refused.
    let mut env = Env::initialized();
    ok(propose(&mut env, kink.clone()));
    env.warp(2 * 365 * 86_400);
    assert_err(&apply(&mut env), ShortwinError::PendingChangeExpired);
    assert_eq!(env.config().kink.util_kink_bps, 8_000);
    // Governance proposes it again; it applies after a fresh timelock.
    ok(propose(&mut env, kink.clone()));
    env.warp(i64::from(DEFAULT_TIMELOCK_S));
    ok(apply(&mut env));
    assert_eq!(env.config().kink.util_kink_bps, 7_000);

    // The window's edges: the last second applies, the next one does not.
    let mut env = Env::initialized();
    ok(propose(&mut env, kink.clone()));
    env.warp(i64::from(DEFAULT_TIMELOCK_S) + CONFIG_APPLY_WINDOW_S);
    ok(apply(&mut env));
    let mut env = Env::initialized();
    ok(propose(&mut env, kink));
    env.warp(i64::from(DEFAULT_TIMELOCK_S) + CONFIG_APPLY_WINDOW_S + 1);
    assert_err(&apply(&mut env), ShortwinError::PendingChangeExpired);
    let gov = env.governance.insecure_clone();
    ok(env.send(&[ix::cancel_config(&gov.pubkey())], &[&gov]));
    assert!(!env.config().pending.is_some());
}

// ---- M1-SEC-10: an OracleFeed change empties the cache --------------------------------------

/// `apply_config(OracleFeed)` needs `oracle_cache` and empties it. The keeper cranks the old
/// feed right up to the switch, so without the reset the new feed's first updates would be
/// refused as not newer (its publish time trails the old feed's by a heartbeat) or as a step
/// (another source's price) until `oracle_stale_s` passed, and readers would keep the old
/// feed's price as fresh meanwhile.
#[test]
fn sec_10_an_oracle_feed_change_empties_the_cache() {
    let mut env = Env::initialized();
    env.set_clock(DUMP_TS, DUMP_SLOT);
    let u = pyth::read(&pyth::RECEIVER, DUMP).unwrap();
    let other = Pubkey::new_unique();
    let feed = [7u8; 32];
    ok(propose(
        &mut env,
        ConfigChange::OracleFeed {
            account: other,
            feed_id: feed,
        },
    ));
    env.warp(i64::from(DEFAULT_TIMELOCK_S));
    // The old feed, cranked just before the switch.
    let now = env.now();
    let old = PriceUpdate {
        publish_time: now - 1,
        prev_publish_time: now - 2,
        ..u
    };
    put_update(&mut env, PYTH_SOL_USD_ACCOUNT, &old);
    ok(env.send(&[ix::crank_sol_usd(&PYTH_SOL_USD_ACCOUNT)], &[]));
    let bump = env.oracle_cache().bump;
    assert_eq!(env.oracle_cache().publish_time, now - 1);

    // Without the cache account: refused.
    assert_err(
        &env.send(&[ix::apply_config()], &[]),
        ShortwinError::AccountMismatch,
    );
    ok(apply(&mut env));
    let c = env.oracle_cache();
    assert_eq!(
        (c.price, c.conf, c.expo, c.publish_time, c.updated_slot),
        (0, 0, 0, 0, 0)
    );
    assert_eq!(c.bump, bump);

    // The new feed: published 20 s before the old cache's time, 30 % away (the step limit is
    // 20 %), at another exponent — accepted at once.
    let new = PriceUpdate {
        feed_id: feed,
        price: u.price * 13 / 100,
        conf: u.conf / 10,
        exponent: -7,
        publish_time: now - 20,
        prev_publish_time: now - 21,
        ..u
    };
    put_update(&mut env, other, &new);
    ok(env.send(&[ix::crank_sol_usd(&other)], &[]));
    let c = env.oracle_cache();
    assert_eq!((c.price, c.expo, c.publish_time), (new.price, -7, now - 20));
    let _ = PYTH_SOL_USD_FEED_ID;
}

// ---- M1-SEC-11: the creator bounds the creation fee -----------------------------------------

/// `create_market(venue_hint, max_creation_fee)`: a fee above the creator's maximum is refused
/// before any account is created (`CreationFeeAboveMax`); the exact fee is accepted.
#[test]
fn sec_11_creation_fee_is_bounded_by_the_creator() {
    let mut env = Env::initialized();
    let fee = env.config().params.market_creation_fee;
    let (mint, creator) = new_mint(&mut env, TokenProgram::Spl);
    let pool = pda::canonical_pool(&mint);
    let tp = TokenProgram::Spl.id();
    let res = env.send(
        &[ix::create_market_max_fee(
            &creator.pubkey(),
            &mint,
            &tp,
            0,
            &pool,
            fee - 1,
        )],
        &[&creator],
    );
    assert_err(&res, ShortwinError::CreationFeeAboveMax);
    assert!(read::account(&env.svm, &pda::market(&mint).0).is_none());
    let meta = ok(env.send(
        &[ix::create_market_max_fee(
            &creator.pubkey(),
            &mint,
            &tp,
            0,
            &pool,
            fee,
        )],
        &[&creator],
    ));
    assert_eq!(
        events::one::<shortwin::events::MarketCreated>(&meta).creation_fee,
        fee
    );
}

// ---- refutations: attacks the program withstands (from the review) ---------------------------

/// Event injection: the `emit_cpi!` self-CPI entry point requires the event-authority PDA as a
/// signer, which only the program can produce. A forged `Withdraw` event is refused.
#[test]
fn ok_forged_event_cpi_is_refused() {
    let mut env = Env::initialized();
    let mut data = anchor_lang::event::EVENT_IX_TAG_LE.to_vec();
    data.extend_from_slice(shortwin::events::Withdraw::DISCRIMINATOR);
    data.extend_from_slice(&[0u8; 200]);
    let ix1 = Instruction {
        program_id: shortwin::ID,
        accounts: vec![AccountMeta::new_readonly(pda::event_authority(), false)],
        data: data.clone(),
    };
    assert_anchor_err(&env.send(&[ix1], &[]), AnchorErrorCode::ConstraintSigner);
    let signer = env.funded_keypair();
    let ix2 = Instruction {
        program_id: shortwin::ID,
        accounts: vec![AccountMeta::new_readonly(signer.pubkey(), true)],
        data,
    };
    assert_anchor_err(
        &env.send(&[ix2], &[&signer]),
        AnchorErrorCode::ConstraintSeeds,
    );
}

/// Withdrawing into the pool vault itself and other account substitutions on the lender path
/// are refused by the Accounts constraints.
#[test]
fn ok_lender_account_substitutions_are_refused() {
    let mut env = Env::initialized();
    let (l, h) = market_with_holder(&mut env, TokenProgram::Token2022, 1_000_000);
    let owner = h.owner.insecure_clone();
    let lk = lender_keys(&l.k, &h);
    ok(env.send(&[ix::deposit(&lk, 1_000_000, 0)], &[&owner]));
    let shares = env.lender(&l.k.market, &owner.pubkey()).unwrap().shares;

    let mut self_dest = lk;
    self_dest.owner_token_account = l.k.pool_vault;
    assert_anchor_err(
        &env.send(&[ix::withdraw(&self_dest, shares, 0)], &[&owner]),
        AnchorErrorCode::ConstraintDuplicateMutableAccount,
    );
    let mut wrong_prog = lk;
    wrong_prog.token_program = TOKEN_PROGRAM_ID;
    assert_failed(&env.send(&[ix::withdraw(&wrong_prog, shares, 0)], &[&owner]));
    assert_failed(&env.send(&[ix::deposit(&wrong_prog, 1, 0)], &[&owner]));
    let (other, _) = env.install_market(TokenProgram::Token2022, 6);
    let mut ixw = ix::withdraw(&lk, shares, 0);
    ixw.accounts[3].pubkey = other.market_vault;
    assert_failed(&env.send(&[ixw], &[&owner]));
    ok(env.send(&[ix::withdraw(&lk, shares, 0)], &[&owner]));
    assert_eq!(read::token_amount(&env.svm, &l.k.pool_vault), Some(0));
}

/// First-depositor inflation: donations straight to the pool vault do not move the share
/// price (`T` is an internal counter), so a 1-unit first deposit plus a large donation takes
/// nothing from the next depositor (the before-the-first-deposit and SOL cases are
/// `lender_it::adversarial::first_depositor_inflation_by_donation`, M1-FID-14).
#[test]
fn ok_donation_inflation_does_not_dilute() {
    let mut env = Env::initialized();
    let (l, attacker) = market_with_holder(&mut env, TokenProgram::Spl, 1_000_000_000_001);
    let payer = env.payer.insecure_clone();
    let victim = tokens::holder(
        &mut env.svm,
        &payer,
        &l.k.mint,
        &l.authority,
        1_000_000_000,
        TokenProgram::Spl,
    );
    let a = attacker.owner.insecure_clone();
    ok(env.send(&[ix::deposit(&lender_keys(&l.k, &attacker), 1, 0)], &[&a]));
    let donate = spl_token_interface::instruction::transfer(
        &TOKEN_PROGRAM_ID,
        &attacker.token_account,
        &l.k.pool_vault,
        &a.pubkey(),
        &[],
        1_000_000_000_000,
    )
    .unwrap();
    ok(env.send(&[donate], &[&a]));
    let v = victim.owner.insecure_clone();
    let vk = lender_keys(&l.k, &victim);
    ok(env.send(&[ix::deposit(&vk, 1_000_000_000, 0)], &[&v]));
    let vs = env.lender(&l.k.market, &v.pubkey()).unwrap().shares;
    assert_eq!(
        vs,
        1_000_000_000 * 1_000,
        "same rate as the first depositor"
    );
    ok(env.send(&[ix::withdraw(&vk, vs, 1_000_000_000)], &[&v]));
    let ak = lender_keys(&l.k, &attacker);
    let as_ = env.lender(&l.k.market, &a.pubkey()).unwrap().shares;
    ok(env.send(&[ix::withdraw(&ak, as_, 1)], &[&a]));
    assert_eq!(
        read::token_amount(&env.svm, &l.k.pool_vault),
        Some(1_000_000_000_000)
    );
}

/// `init_if_needed`: a lender PDA pre-funded by a third party is still created by the first
/// deposit, and an existing position cannot be re-created to reset its state.
#[test]
fn ok_prefunded_lender_pda_and_no_reinit() {
    let mut env = Env::initialized();
    let (l, h) = market_with_holder(&mut env, TokenProgram::Spl, 2_000);
    let owner = h.owner.insecure_clone();
    let lender = pda::lender(&l.k.market, &owner.pubkey()).0;
    env.fund(&lender, 1_000_000);
    let lk = lender_keys(&l.k, &h);
    ok(env.send(&[ix::deposit(&lk, 1_000, 0)], &[&owner]));
    ok(env.send(&[ix::deposit(&lk, 1_000, 0)], &[&owner]));
    let lp = env.lender(&l.k.market, &owner.pubkey()).unwrap();
    assert_eq!(lp.shares, 2_000_000);
    assert_eq!(lp.owner, owner.pubkey());
}

/// Closing and re-opening a lender position in one transaction cannot duplicate SOL. The
/// owner is the only lender, so since AIR-08 the re-deposit waits for `sweep_trader` (a deposit
/// into a pool every lender left), which the transaction carries.
#[test]
fn ok_close_and_reopen_in_one_transaction() {
    let mut env = Env::initialized();
    let (l, h) = market_with_holder(&mut env, TokenProgram::Spl, 2_000);
    let owner = h.owner.insecure_clone();
    let lk = lender_keys(&l.k, &h);
    ok(env.send(&[ix::deposit(&lk, 1_000, 0)], &[&owner]));
    ok(env.send(
        &[ix::cover_deficit(&owner.pubkey(), &l.k.market, 5_000)],
        &[&owner],
    ));
    let shares = env.lender(&l.k.market, &owner.pubkey()).unwrap().shares;
    let before = read::lamports(&env.svm, &owner.pubkey());
    let sweep = shortwin_litesvm_tests::engine::sweep_trader_ix(&l.k.market, None);
    assert_err(
        &env.send(
            &[ix::withdraw(&lk, shares, 0), ix::deposit(&lk, 1_000, 0)],
            &[&owner],
        ),
        ShortwinError::WipeSweepPending,
    );
    ok(env.send(
        &[
            ix::withdraw(&lk, shares, 0),
            sweep,
            ix::deposit(&lk, 1_000, 0),
            ix::claim_sol(&owner.pubkey(), &l.k.market),
        ],
        &[&owner],
    ));
    let after = read::lamports(&env.svm, &owner.pubkey());
    assert_eq!(after, before + 5_000);
    assert_eq!(env.market_vault(&l.k.market).lender_sol, 0);
}

/// `create_market` refuses a token program that does not own the mint.
#[test]
fn ok_create_market_token_program_confusion() {
    let mut env = Env::initialized();
    let (mint, creator) = new_mint(&mut env, TokenProgram::Token2022);
    let res = env.send(
        &[ix::create_market(
            &creator.pubkey(),
            &mint,
            &TOKEN_PROGRAM_ID,
            0,
            &pda::canonical_pool(&mint),
        )],
        &[&creator],
    );
    assert_failed(&res);
}

/// Only the Emergency vault clears the venue latch, and only at the reviewed slot.
#[test]
fn ok_latch_cannot_be_cleared_by_others() {
    let mut env = Env::initialized();
    env.set_all_venue_program_data();
    env.set_venue_program_data(0, 1);
    ok(env.send(&[ix::flag_venue_upgraded(0)], &[]));
    let gov = env.governance.insecure_clone();
    let res = env.send(&[ix::revalidate_venue(&gov.pubkey(), 0, 1)], &[&gov]);
    assert_err(&res, ShortwinError::Unauthorized);
    assert_eq!(env.venue_guard().upgraded, 1);
    let data = shortwin::instruction::SweepProtocolFees {}.data();
    assert_eq!(data.len(), 8);
}
