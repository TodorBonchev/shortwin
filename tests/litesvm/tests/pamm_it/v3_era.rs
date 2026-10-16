//! The PumpSwap side of the 2026-10-13 re-pin (PumpSwap of 2026-10-08, pump-fees of 2026-10-12,
//! as `fork::PumpDeploy::Oct2026Synth` and `Oct2026Chain` both hold them; pump's 2026-10-13
//! redeploy changed neither).
//!
//! **T-PFEE-CPI** (PUMP-2026-10-13-5): short.win's curve legs will stop CPIing pump-fees, but its
//! PumpSwap legs still pass pump-fees as `fee_program`, and PumpSwap invokes it there — so the
//! unexamined 2026-10-12 pump-fees build (its new `withdraw` is in no IDL) runs inside every
//! PumpSwap leg. Read off the transactions' own inner instructions: on that build, every
//! pump-fees instruction of a short.win PumpSwap sale (open) and buy (close) is `get_fees` /
//! `get_fees_with_quote_mint`, invoked by PumpSwap at height 3 with exactly `[fee_config,
//! config_program_id]` = `[PumpSwap's FeeConfig, PumpSwap]` — none of the trader PDA, its ATAs,
//! the pool vaults or any short.win account — and never `withdraw`.

use shortwin::constants::{
    PUMP_AMM_FEE_CONFIG, PUMP_AMM_PROGRAM_ID, PUMP_FEES_PROGRAM_ID, PUMP_PROGRAM_ID,
};
use shortwin_litesvm_tests::fork::{PumpDeploy, PumpProgram};
use solana_message::Message;

use crate::common::*;

/// pump-fees `get_fees` (`e7257e55…`, pump-public-docs `2293f9a6`'s IDL).
const GET_FEES: [u8; 8] = [0xe7, 0x25, 0x7e, 0x55, 0xcf, 0x5b, 0x3f, 0x34];
/// pump-fees `get_fees_with_quote_mint` (`9aed8a5c…`).
const GET_FEES_WITH_QUOTE_MINT: [u8; 8] = [0x9a, 0xed, 0x8a, 0x5c, 0xa2, 0x02, 0xa2, 0xbb];

#[derive(Clone, Debug)]
struct Inner {
    program: Pubkey,
    accounts: Vec<Pubkey>,
    data: Vec<u8>,
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
                    data: i.instruction.data.clone(),
                    height: i.stack_height,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Every pump-fees call of a leg (instruction 1 of `ixs`) is a read-only fee query of
/// PumpSwap's `FeeConfig`, made by PumpSwap at height 3; returns how many there were.
#[track_caller]
fn fee_queries_only(
    env: &Env,
    pm: &PammMarket,
    b: &Pubkey,
    ixs: &[Instruction],
    meta: &TransactionMetadata,
    label: &str,
) -> usize {
    let calls = inner(env, ixs, 1, meta);
    let market = pm.market();
    let shortwin_side = [
        pm.trader(),
        pda::trader_wsol(&market).0,
        pm.trader_base_ata,
        wsol_ata(&pm.trader()),
        pm.trader_uva(),
        pm.pool.base_vault,
        pm.pool.quote_vault,
        pm.pool.pool,
        market,
        pm.keys.market_vault,
        pm.keys.pool_vault,
        pda::position(&market, b, 0).0,
        *b,
        shortwin::ID,
    ];
    let mut n = 0;
    for (k, c) in calls.iter().enumerate() {
        if c.program != PUMP_FEES_PROGRAM_ID {
            continue;
        }
        n += 1;
        let disc: [u8; 8] = c.data[..8].try_into().unwrap();
        assert!(
            disc == GET_FEES || disc == GET_FEES_WITH_QUOTE_MINT,
            "{label}: pump-fees call {k} is {}, not a fee query",
            hex(&disc)
        );
        assert_ne!(disc, fork::PUMP_FEES_WITHDRAW, "{label}: never withdraw");
        assert_eq!(
            c.accounts,
            [PUMP_AMM_FEE_CONFIG, PUMP_AMM_PROGRAM_ID],
            "{label}: exactly fee_config and config_program_id"
        );
        assert!(
            c.accounts.iter().all(|a| !shortwin_side.contains(a)),
            "{label}: no short.win account"
        );
        assert_eq!(c.height, 3, "{label}: invoked by PumpSwap");
        let parent = calls[..k]
            .iter()
            .rev()
            .find(|p| p.height < c.height)
            .expect("a caller");
        assert_eq!(
            (parent.program, parent.height),
            (PUMP_AMM_PROGRAM_ID, 2),
            "{label}: the caller is PumpSwap under short.win's CPI"
        );
    }
    assert!(
        calls.iter().all(|c| c.program != PUMP_PROGRAM_ID),
        "{label}: no pump curve call"
    );
    n
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// R-PFEE (read-only; the result is recorded for the addendum, not asserted): walks pump-fees'
/// undocumented `withdraw` on the dumped 2026-10-12 ELF. Anchor reads the accounts in field order
/// and names the first one it lacks (`AccountNotEnoughKeys`), so the fields are learnt one at a
/// time — each given an account its name suggests until Anchor moves on — and then every
/// constraint error is resolved from what it prints (a signer, writable, the expected PDA or
/// address). Prints the trail and the account list. Run with `--ignored --nocapture`.
#[test]
#[ignore = "R-PFEE: a read-only probe of pump-fees' withdraw; run with --ignored --nocapture"]
fn r_pfee_withdraw_probe() {
    use solana_keypair::Keypair;
    let (mut env, _) = Env::fork_pump_coin(fork::PumpCoin::Fresh166);
    assert!(
        env.pump_build_running(PumpProgram::Fees)
            .same_build(PumpDeploy::Oct2026Synth, PumpProgram::Fees)
    );
    let mut data = fork::PUMP_FEES_WITHDRAW.to_vec();
    data.extend_from_slice(&[0; 16]);
    let mut kps: Vec<Keypair> = Vec::new();
    let send =
        |env: &mut Env, metas: &[AccountMeta], kps: &[Keypair]| -> (Option<u32>, Vec<String>) {
            let ix = Instruction {
                program_id: PUMP_FEES_PROGRAM_ID,
                accounts: metas.to_vec(),
                data: data.clone(),
            };
            let signers: Vec<&Keypair> = kps
                .iter()
                .filter(|k| metas.iter().any(|m| m.is_signer && m.pubkey == k.pubkey()))
                .collect();
            let res = env.send(&[ix], &signers);
            let logs = match &res {
                Ok(m) => m.logs.clone(),
                Err(f) => f.meta.logs.clone(),
            };
            (custom_code(&res), logs)
        };
    let name_of = |logs: &[String]| -> Option<String> {
        logs.iter()
            .find_map(|l| l.split("caused by account: ").nth(1))
            .and_then(|r| r.split('.').next())
            .map(str::to_owned)
    };
    let key_after = |logs: &[String], tag: &str| -> Option<Pubkey> {
        let i = logs.iter().position(|l| l.ends_with(tag))?;
        logs.get(i + 1)?.split_whitespace().last()?.parse().ok()
    };
    let token_programs = [
        shortwin::constants::TOKEN_PROGRAM_ID,
        shortwin::constants::TOKEN_2022_PROGRAM_ID,
    ];
    let mints = [
        shortwin::constants::WSOL_MINT,
        fork::PumpCoin::Fresh166.mint(),
    ];
    let ro = |k: Pubkey| AccountMeta::new_readonly(k, false);
    // ---- 1. the fields, in order ----
    let mut metas: Vec<AccountMeta> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    for _ in 0..30 {
        let (code, logs) = send(&mut env, &metas, &kps);
        if code != Some(fork::ANCHOR_ACCOUNT_NOT_ENOUGH_KEYS) {
            println!(
                "R-PFEE: {} fields; then {code:?} {:?}",
                metas.len(),
                name_of(&logs)
            );
            break;
        }
        let field = name_of(&logs).unwrap_or_else(|| "?".to_owned());
        let user = metas.first().map(|m| m.pubkey);
        let mut candidates: Vec<AccountMeta> = Vec::new();
        if field == "user"
            || (field.contains("authority") && field != "event_authority")
            || field == "payer"
        {
            let kp = Keypair::new();
            env.fund(&kp.pubkey(), 10_000_000_000);
            candidates.push(AccountMeta::new(kp.pubkey(), true));
            kps.push(kp);
        }
        if field.contains("mint") {
            candidates.extend(mints.iter().map(|m| ro(*m)));
        }
        if field == "program" {
            // emit_cpi!'s self-invocation: the program itself.
            candidates.push(ro(PUMP_FEES_PROGRAM_ID));
        }
        if field.contains("token_program") || field.contains("program") {
            candidates.extend(token_programs.iter().map(|p| ro(*p)));
            candidates.push(ro(shortwin::constants::ASSOCIATED_TOKEN_PROGRAM_ID));
            candidates.push(ro(anchor_lang::system_program::ID));
            candidates.push(ro(PUMP_FEES_PROGRAM_ID));
        }
        if (field.contains("ata") || field.contains("token_account"))
            && let Some(u) = user
        {
            for mint in mints {
                for program in token_programs {
                    let payer = env.payer.pubkey();
                    let ix = fork::create_ata_idempotent_ix(&payer, &u, &mint, &program);
                    let _ = env.send(&[ix], &[]);
                    candidates.push(AccountMeta::new(pda::ata(&u, &mint, &program), false));
                }
            }
        }
        if field == "event_authority" {
            candidates.push(ro(Pubkey::find_program_address(
                &[b"__event_authority"],
                &PUMP_FEES_PROGRAM_ID,
            )
            .0));
        }
        candidates.push(AccountMeta::new(Pubkey::new_unique(), false));
        let mut chosen = None;
        for cand in candidates {
            metas.push(cand.clone());
            let (c2, l2) = send(&mut env, &metas, &kps);
            let moved = c2 != Some(fork::ANCHOR_ACCOUNT_NOT_ENOUGH_KEYS)
                || name_of(&l2).as_deref() != Some(field.as_str());
            let blames = c2 != Some(fork::ANCHOR_ACCOUNT_NOT_ENOUGH_KEYS)
                && name_of(&l2).as_deref() == Some(field.as_str())
                && !matches!(c2, Some(2000..=2999));
            if moved && !blames {
                chosen = Some(cand);
                break;
            }
            metas.pop();
        }
        let Some(cand) = chosen else {
            println!(
                "R-PFEE: no candidate passes field {} `{field}`",
                metas.len()
            );
            break;
        };
        println!(
            "R-PFEE field {} `{field}`: {} (signer {}, writable {})",
            metas.len() - 1,
            cand.pubkey,
            cand.is_signer,
            cand.is_writable
        );
        names.push(field);
    }
    // ---- 2. the constraints ----
    for round in 0..40 {
        let (code, logs) = send(&mut env, &metas, &kps);
        let lines: Vec<&String> = logs
            .iter()
            .filter(|l| l.contains("AnchorError") || l.contains("Program log:"))
            .collect();
        println!("R-PFEE constraint round {round}: {code:?} {lines:?}");
        let Some(code) = code else {
            println!("R-PFEE: withdraw executed");
            break;
        };
        let field = name_of(&logs);
        let at = field
            .as_ref()
            .and_then(|f| names.iter().position(|n| n == f));
        match (code, at) {
            (3010 | 2002, Some(i)) => {
                let kp = Keypair::new();
                env.fund(&kp.pubkey(), 10_000_000_000);
                metas[i] = AccountMeta::new(kp.pubkey(), true);
                kps.push(kp);
                println!("R-PFEE: `{}` must sign", names[i]);
            }
            (2000, Some(i)) => {
                metas[i].is_writable = true;
                println!("R-PFEE: `{}` must be writable", names[i]);
            }
            // ConstraintTokenOwner / ConstraintTokenMint name no account: try each token-account
            // field as the ATA of each passed account for the passed mint and token program.
            (2014 | 2015, _) => {
                let mint = names
                    .iter()
                    .position(|n| n.contains("mint"))
                    .map(|i| metas[i].pubkey);
                let program = names
                    .iter()
                    .position(|n| n == "token_program")
                    .map(|i| metas[i].pubkey);
                let (Some(mint), Some(program)) = (mint, program) else {
                    break;
                };
                let owners: Vec<Pubkey> = metas.iter().map(|m| m.pubkey).collect();
                let mut fixed = false;
                'try_ata: for i in (0..names.len()).filter(|&i| names[i].contains("ata")) {
                    for owner in &owners {
                        let a = pda::ata(owner, &mint, &program);
                        if a == metas[i].pubkey {
                            continue;
                        }
                        let payer = env.payer.pubkey();
                        let ix = fork::create_ata_idempotent_ix(&payer, owner, &mint, &program);
                        let _ = env.send(&[ix], &[]);
                        let saved = metas[i].clone();
                        metas[i].pubkey = a;
                        let (c2, _) = send(&mut env, &metas, &kps);
                        if c2 != Some(code) {
                            println!(
                                "R-PFEE: `{}` ({code}) must be ATA({owner}, {mint}) = {a}",
                                names[i]
                            );
                            fixed = true;
                            break 'try_ata;
                        }
                        metas[i] = saved;
                    }
                }
                if !fixed {
                    println!("R-PFEE: stops at {code}");
                    break;
                }
            }
            _ => {
                let (Some(left), Some(right)) =
                    (key_after(&logs, "Left:"), key_after(&logs, "Right:"))
                else {
                    println!("R-PFEE: stops at {code} {field:?}");
                    for l in &logs {
                        println!("   {l}");
                    }
                    break;
                };
                let (i, to) = match (
                    metas.iter().position(|m| m.pubkey == left),
                    metas.iter().position(|m| m.pubkey == right),
                ) {
                    (Some(i), _) => (i, right),
                    (None, Some(i)) => (i, left),
                    _ => {
                        println!("R-PFEE: {field:?}: neither {left} nor {right} is passed");
                        break;
                    }
                };
                println!("R-PFEE: `{}` ({code}) must be {to}", names[i]);
                metas[i].pubkey = to;
                if env.svm.get_account(&to).is_none() {
                    // An ATA of a passed account for a known mint, or a PDA to leave empty.
                    'ata: for owner in metas.clone().iter().map(|m| m.pubkey) {
                        for mint in mints {
                            for program in token_programs {
                                if pda::ata(&owner, &mint, &program) == to {
                                    let payer = env.payer.pubkey();
                                    let ix = fork::create_ata_idempotent_ix(
                                        &payer, &owner, &mint, &program,
                                    );
                                    let _ = env.send(&[ix], &[]);
                                    println!("R-PFEE: created ATA({owner}, {mint})");
                                    break 'ata;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    let at = |n: &str| names.iter().position(|x| x == n);
    // What it moves: the vault's ATA funded (and the vault PDA's lamports), then withdraw.
    if let (Some(u), Some(v), Some(va), Some(ua)) = (
        at("user"),
        at("referral_vault"),
        at("vault_ata"),
        at("user_ata"),
    ) {
        let amount = 1_000_000u64;
        let mut acc = env.svm.get_account(&metas[va].pubkey).unwrap();
        acc.lamports += amount;
        env.svm.set_account(metas[va].pubkey, acc).unwrap();
        fork::set_token_amount(&mut env.svm, &metas[va].pubkey, amount);
        env.fund(&metas[v].pubkey, 3_000_000_000);
        let keys = [
            metas[u].pubkey,
            metas[v].pubkey,
            metas[va].pubkey,
            metas[ua].pubkey,
        ];
        let before: Vec<(u64, u64)> = keys
            .iter()
            .map(|k| {
                (
                    read::lamports(&env.svm, k),
                    read::token_amount(&env.svm, k).unwrap_or(0),
                )
            })
            .collect();
        let (c, logs) = send(&mut env, &metas, &kps);
        let after: Vec<(u64, u64)> = keys
            .iter()
            .map(|k| {
                (
                    read::lamports(&env.svm, k),
                    read::token_amount(&env.svm, k).unwrap_or(0),
                )
            })
            .collect();
        println!("R-PFEE: a funded vault → {c:?}");
        for l in &logs {
            println!("   {l}");
        }
        for (k, (b, a)) in ["user", "referral_vault", "vault_ata", "user_ata"]
            .iter()
            .zip(before.iter().zip(&after))
        {
            println!(
                "   {k:<15} lamports {} → {}, tokens {} → {}",
                b.0, a.0, b.1, a.1
            );
        }
    }
    if let (Some(u), Some(v)) = (at("user"), at("referral_vault")) {
        let pda = Pubkey::find_program_address(
            &[b"referral-vault", metas[u].pubkey.as_ref()],
            &PUMP_FEES_PROGRAM_ID,
        )
        .0;
        println!(
            "R-PFEE: referral_vault == PDA(pump-fees, [\"referral-vault\", user]): {}",
            metas[v].pubkey == pda
        );
    }
    // Whether `user_ata` is tied to the user: another owner's ATA in its place.
    if let (Some(i), Some(m), Some(p)) = (at("user_ata"), at("quote_mint"), at("token_program")) {
        let other = Pubkey::new_unique();
        let (mint, program) = (metas[m].pubkey, metas[p].pubkey);
        let payer = env.payer.pubkey();
        let _ = env.send(
            &[fork::create_ata_idempotent_ix(
                &payer, &other, &mint, &program,
            )],
            &[],
        );
        let saved = metas[i].clone();
        metas[i].pubkey = pda::ata(&other, &mint, &program);
        let (c, logs) = send(&mut env, &metas, &kps);
        println!(
            "R-PFEE: user_ata = another owner's ATA → {c:?} {:?}",
            name_of(&logs)
        );
        metas[i] = saved;
    }
    println!("R-PFEE withdraw accounts:");
    for (i, (n, m)) in names.iter().zip(&metas).enumerate() {
        println!(
            "   {i:2} {n:<28} {} signer {} writable {}",
            m.pubkey, m.is_signer, m.is_writable
        );
    }
}

/// T-PFEE-CPI on a Token-2022 and an SPL-Token pool: the 2026-10-12 pump-fees build executes
/// (its `withdraw` dispatches), and inside short.win's PumpSwap sale and buy it only answers
/// fee queries about PumpSwap's `FeeConfig`, seeing no short.win account.
#[test]
fn pump_fees_runs_read_only_inside_a_pumpswap_leg() {
    for coin in [PammCoin::Mature, PammCoin::Legacy261] {
        let (mut env, pm, q) = setup(coin);
        assert!(
            env.pump_build_running(PumpProgram::Fees)
                .same_build(PumpDeploy::Oct2026Synth, PumpProgram::Fees),
            "{coin:?}: the 2026-10-12 pump-fees runs"
        );
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
        let sells = fee_queries_only(&env, &pm, &b.key(), &ixs, &meta, "open (sell)");
        assert!(sells >= 1, "{coin:?}: the sale queries pump-fees");
        env.warp(1);
        crank(&mut env, &pm);
        let close = close_short_ix(&pm, &b.key(), 0, u64::MAX, ExtrasSel::NONE, &[]);
        let ixs = [cu_limit_ix(pamm::PAMM_TRADE_CU), close];
        let meta = ok(env.send(&ixs, &[&kp]));
        let buys = fee_queries_only(&env, &pm, &b.key(), &ixs, &meta, "close (buy)");
        assert!(buys >= 1, "{coin:?}: the buy queries pump-fees");
        eprintln!("T-PFEE-CPI {coin:?}: {sells} fee queries in the sale, {buys} in the buy");
    }
}
