//! T-SQ (M10 WP-7; ARCH §1.1 "Admin", §3.1, §8.5; owner decisions 2026-10-02, Q4): short.win's
//! governance through **the mainnet Squads v4 ELF** (`SQDS4ep6…`, immutable since slot
//! 302,582,236; group `squads_programs`), in the owner's layout ([`Env::squads_governed`]):
//! Governance autonomous 3-of-5 with a 172,800 s time lock, its vault 0 = `governance` and the
//! upgrade authority, vault 1 = `fee_recipient` (pre-funded); Emergency autonomous 2-of-4, time
//! lock 0, vault 0 = `pause_authority`; `coverage_authority` a separate hot key; all four
//! pairwise distinct. Every transaction goes through `Env::send` (the hooks run on what a vault
//! executes; I4 counts the vault path as admin-signed) and is ≤ 1,232 bytes (`SquadsMs` refuses
//! to send more), each member paying its own fees.
//!
//! | Test | What |
//! |---|---|
//! | T-SQ-1 | the autonomous multisigs as created (decoded), `initialize_config` with the vault PDAs, the four authorities distinct, the pre-funded fee vault; Squads' own 90-day time-lock cap |
//! | T-SQ-2 | the double timelock: Squads refuses at 172,799 s after the approval and executes at 172,800 s; the program then refuses `apply_config` 1 s before `not_before` and applies at it (96 h end to end); a change lapses 1 s after the 14-day window and applies at its last second |
//! | T-SQ-3 | every Emergency power in ≤ 2 transactions (`set_pause` global and market, the coverage clear, `disable_venue`, `revalidate_venue`, `set_route_broken`, `cancel_config` of a parameter change, `set_venue_extras` — the worst case, 4 templates × 4 32-byte seeds, through the transaction buffer, whose direct staging would exceed 1,232 bytes) |
//! | T-SQ-4 | the Emergency's refusals: no `propose_config` of any kind (`Unauthorized`), no cancel of an `Authorities` or `Timelock` change (`EmergencyCannotCancel`, M1-SEC-1), no coverage set; a refused vault transaction stays `Approved` until its members cancel it |
//! | T-SQ-5 | the upgrade authority: `SetAuthorityChecked` refuses a new authority that does not sign; the checked transfer to the Governance vault inside a Governance vault transaction (the deployer co-signs the execution); the deployer and the Emergency vault lose every loader power; the unchecked `SetAuthority`'s hazard (a mistyped key takes the authority) |
//! | T-SQ-6 | an upgrade through the Governance vault after its time lock (slot, authority kept, buffer closed, the program runs); the Emergency vault cannot upgrade; a buffer whose authority is not the vault (`IncorrectAuthority`), a larger ELF (`AccountDataTooSmall` until a permissionless `ExtendProgram`), Governance's `proposal_cancel_v2` (3 votes) of an approved upgrade; Emergency members cannot vote on a Governance proposal |
//! | T-SQ-7 | what ARCH §8.5 discloses: an `Approved` vault transaction survives a member rotation (its unapproved sibling goes stale, as does an approved config transaction) and a threshold increase (3 approvals, executed under 4), and never expires (a year later it executes and replaces the pending change); a time-lock increase delays it (Squads checks the current time lock, from the approval); Governance cannot cancel reactively inside the program window (its cancel lands at `not_before` at the earliest, when anyone can apply) |
//! | T-SQ-8 | the size and CU table of every governance and Emergency flow; all ≤ 1,232 bytes or on the buffer path; executions < 200,000 CU (no compute-budget instruction needed) |
//! | T-SQ-9 | I4 on vault-executed instructions: a top-level `vault_transaction_execute` carrying an admin vault is admin-signed (a non-admin multisig's is not; the signer-only rule saw neither), and an Emergency-vault execution that moves pool tokens is caught (it passes under the signer-only rule); I4's coverage clause on the vault path: the Emergency vault's executed clear is judged as `set_coverage_pause` signed by the vault (the real one passes; a wrong `coverage_cleared_at`, the flag left set, another byte changed or another signer fail; the top-level-only reading checked none of them) |

#![allow(clippy::result_large_err)]

use std::panic::AssertUnwindSafe;

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::Instruction;
use shortwin::PauseScope;
use shortwin::constants::{CONFIG_APPLY_WINDOW_S, VENUE_BIT_RAYDIUM_V4};
use shortwin::errors::ShortwinError;
use shortwin::events::COVERAGE_REASON_KEEPER;
use shortwin::instructions::set_venue_extras::{ExtraEntryArg, SeedPartArg};
use shortwin::state::{
    ConfigChange, MARKET_FLAG_PAUSED_COVERAGE, MARKET_FLAG_PAUSED_OPENS, VenueExtras, VenueKind,
};
use shortwin_litesvm_tests::errors::{assert_err, ok};
use shortwin_litesvm_tests::squads::{
    Governance, MultisigError, SquadsMs, Staged, TX_MAX, assert_squads_err, loader, sq, squads_pda,
    wire_size,
};
use shortwin_litesvm_tests::tokens::{self, TokenProgram};
use shortwin_litesvm_tests::{Env, TxResult, engine, fork, invariants, ix, pda, read};
use solana_instruction::error::InstructionError;
use solana_keypair::Keypair;
use solana_signer::Signer;
use solana_transaction_error::TransactionError;

const GOV_TL: i64 = Governance::GOV_TIME_LOCK as i64;

/// A mutation of an account's data (T-SQ-9's coverage-clause mutants).
type DataMutant = Box<dyn Fn(&mut Vec<u8>)>;

/// The CPI's loader error the failing top-level instruction returns.
#[track_caller]
fn assert_ix_err(res: &TxResult, want: InstructionError) {
    match res {
        Err(f) => match &f.err {
            TransactionError::InstructionError(_, e) => {
                assert_eq!(*e, want, "{}", f.meta.pretty_logs())
            }
            other => panic!("expected {want:?}, got {other:?}"),
        },
        Ok(m) => panic!("expected {want:?}, succeeded:\n{}", m.pretty_logs()),
    }
}

/// The compute units of a successful transaction.
fn cu(res: &TxResult) -> u64 {
    res.as_ref()
        .map(|m| m.compute_units_consumed)
        .unwrap_or_else(|f| {
            panic!(
                "transaction failed with {:?}:\n{}",
                f.err,
                f.meta.pretty_logs()
            )
        })
}

/// The proposal status of `index`.
fn status(ms: &SquadsMs, env: &Env, index: u64) -> sq::state::ProposalStatus {
    ms.proposal(env, index).expect("no proposal").status
}

/// `ConfigChange::Kink` with the current kink (a valid change that alters nothing).
fn kink_change(env: &Env) -> ConfigChange {
    ConfigChange::Kink {
        util_kink_bps: env.config().kink.util_kink_bps,
    }
}

/// Governance's `propose_config(kink_change)` (built before the `&mut Env` call that sends it).
fn propose_kink(env: &Env, governance: &Pubkey) -> Instruction {
    ix::propose_config(governance, kink_change(env))
}

/// `Authorities` keeping every authority but `coverage_authority`.
fn rotate_coverage(env: &Env, coverage: &Pubkey) -> ConfigChange {
    let c = env.config();
    ConfigChange::Authorities {
        governance: c.governance,
        pause_authority: c.pause_authority,
        coverage_authority: *coverage,
        fee_recipient: c.fee_recipient,
    }
}

/// The worst-case `set_venue_extras` (ARCH §3.0 "Bounded extras"): 4 template entries, each 4
/// constant seeds of 32 bytes, on PumpSwap's sell set.
fn worst_extras(authority: &Pubkey) -> Instruction {
    let seeds = vec![
        SeedPartArg {
            kind: 1,
            len: 32,
            bytes: [7; 32],
        };
        4
    ];
    let entries = (0..4)
        .map(|_| ExtraEntryArg {
            kind: 2,
            writable: false,
            key: Pubkey::new_unique(),
            seeds: seeds.clone(),
        })
        .collect();
    engine::set_venue_extras_ix(authority, VenueKind::PumpSwap as u8, 0, entries)
}

/// One fixed-key `set_venue_extras` entry (the common case).
fn small_extras(authority: &Pubkey) -> Instruction {
    let entries = vec![ExtraEntryArg {
        kind: 1,
        writable: false,
        key: Pubkey::new_unique(),
        seeds: vec![],
    }];
    engine::set_venue_extras_ix(authority, VenueKind::PumpSwap as u8, 0, entries)
}

fn pumpswap_extras(env: &Env) -> VenueExtras {
    read::zc(&env.svm, &pda::venue_extras(VenueKind::PumpSwap)).expect("VenueExtras")
}

/// Governance stages `ixs`, reaches its threshold and returns the staging; the time lock runs
/// from now.
fn gov_approved(env: &mut Env, g: &Governance, ixs: &[Instruction]) -> Staged {
    let st = g.gov.stage(env, ixs);
    g.gov.approve_to_threshold(env, st.index);
    st
}

/// The release build's ELF (the upgrade's buffer).
fn release_elf() -> Vec<u8> {
    std::fs::read(shortwin_litesvm_tests::env::program_so_path())
        .expect("target/deploy/shortwin.so")
}

// ---- T-SQ-1 ---------------------------------------------------------------------------------

/// T-SQ-1: both multisigs autonomous with the owner's parameters, the config initialized with
/// the vault PDAs and the hot coverage key, all four authorities pairwise distinct (the
/// program batch's SQ-G8 check), the fee vault a pre-funded System account, Squads as mainnet
/// deployed it.
#[test]
fn t_sq_1_autonomous_setup_and_init_with_the_vault_pdas() {
    let (env, g) = Env::squads_governed();

    // Squads v4 as mainnet runs it.
    let prog = env.svm.get_account(&sq::ID).expect("Squads program");
    assert!(prog.executable);
    let pd = fork::squads_fixture("squads_v4_program_data");
    assert_eq!(
        env.svm.get_account(&pd.address).expect("ProgramData").data,
        pd.account.data
    );

    for (ms, n, threshold, time_lock) in [
        (
            &g.gov,
            Governance::GOV_MEMBERS,
            Governance::GOV_THRESHOLD,
            Governance::GOV_TIME_LOCK,
        ),
        (
            &g.emg,
            Governance::EMG_MEMBERS,
            Governance::EMG_THRESHOLD,
            Governance::EMG_TIME_LOCK,
        ),
    ] {
        let s = ms.state(&env);
        assert_eq!(s.config_authority, Pubkey::default(), "autonomous");
        assert_eq!((s.threshold, s.time_lock), (threshold, time_lock));
        assert_eq!(s.members.len(), n);
        assert!(
            s.members
                .iter()
                .all(|m| m.permissions == sq::permission::ALL)
        );
        // Squads keeps its members sorted by key.
        let mut keys: Vec<Pubkey> = ms.members.iter().map(|k| k.pubkey()).collect();
        keys.sort();
        assert_eq!(s.members.iter().map(|m| m.key).collect::<Vec<_>>(), keys);
        assert_eq!(s.rent_collector, Some(ms.vault(0)));
        assert_eq!((s.transaction_index, s.stale_transaction_index), (0, 0));
        assert_eq!(squads_pda::multisig(&s.create_key), ms.key);
    }

    let c = env.config();
    assert_eq!(c.governance, g.governance());
    assert_eq!(c.pause_authority, g.pause_authority());
    assert_eq!(c.fee_recipient, g.fee_recipient());
    assert_eq!(c.coverage_authority, env.coverage_authority.pubkey());
    assert_eq!(c.governance, squads_pda::vault(&g.gov.key, 0));
    assert_eq!(c.fee_recipient, squads_pda::vault(&g.gov.key, 1));
    assert_eq!(c.pause_authority, squads_pda::vault(&g.emg.key, 0));
    let auth = Governance::authorities(&env);
    for i in 0..4 {
        assert_ne!(auth[i], Pubkey::default());
        for j in i + 1..4 {
            assert_ne!(auth[i], auth[j], "authorities {i} and {j}");
        }
        assert_ne!(auth[i], env.upgrade_authority.pubkey());
    }
    // The fee vault: System-owned, empty, holding exactly its rent-exempt minimum.
    let fee = env.svm.get_account(&g.fee_recipient()).expect("fee vault");
    assert_eq!(fee.owner, anchor_lang::system_program::ID);
    assert!(fee.data.is_empty());
    assert_eq!(fee.lamports, Governance::FEE_VAULT_PREFUND);
    // The upgrade authority stays the deployer's until T-SQ-5's transfer.
    assert_eq!(
        loader::program_data(&env).1,
        Some(env.upgrade_authority.pubkey())
    );

    // Squads caps a time lock at 90 days (`TimeLockExceedsMaxAllowed` one second above).
    let mut env = env;
    let creator = env.funded_keypair();
    let create_key = Keypair::new();
    let args = |time_lock| sq::MultisigCreateArgs {
        config_authority: None,
        threshold: 1,
        members: vec![sq::Member::full(creator.pubkey())],
        time_lock,
        rent_collector: None,
        memo: None,
    };
    let ix = |t| {
        sq::ix::multisig_create_v2(
            &sq::MAINNET_TREASURY,
            &create_key.pubkey(),
            &creator.pubkey(),
            &args(t),
        )
    };
    let res = env.send(&[ix(sq::MAX_TIME_LOCK + 1)], &[&creator, &create_key]);
    assert_squads_err(&res, MultisigError::TimeLockExceedsMaxAllowed);
    ok(env.send(&[ix(sq::MAX_TIME_LOCK)], &[&creator, &create_key]));
}

// ---- T-SQ-2 ---------------------------------------------------------------------------------

/// T-SQ-2: a Governance change waits for both timelocks — Squads' 172,800 s from the approval
/// that reaches 3-of-5 (refused at 172,799 s: `TimeLockNotReleased`), then the program's
/// `timelock_s` from the proposal (refused 1 s before `not_before`: `TimelockNotElapsed`) — 96 h
/// end to end; the pending change then lapses `CONFIG_APPLY_WINDOW_S` (14 d) after
/// `not_before` (`PendingChangeExpired` 1 s later, applied at the window's last second).
#[test]
fn t_sq_2_double_timelock_then_the_14_day_window() {
    let (mut env, g) = Env::squads_governed();
    let new_coverage = Keypair::new();
    let change = rotate_coverage(&env, &new_coverage.pubkey());
    let st = g.gov.stage(
        &mut env,
        &[ix::propose_config(&g.governance(), change.clone())],
    );
    let last = g.gov.approve_to_one_short(&mut env, st.index);
    // Two approvals of three: Active, nothing executes.
    assert!(matches!(
        status(&g.gov, &env, st.index),
        sq::state::ProposalStatus::Active(_)
    ));
    assert_squads_err(
        &g.gov.execute(&mut env, &st, 0, &[]),
        MultisigError::InvalidProposalStatus,
    );
    ok(g.gov.approve(&mut env, st.index, last));
    let approved_at = env.now();
    assert_eq!(
        status(&g.gov, &env, st.index),
        sq::state::ProposalStatus::Approved(approved_at)
    );

    // Squads: 172,799 s after the approval, refused; at 172,800 s, executed.
    env.warp(GOV_TL - 1);
    assert_squads_err(
        &g.gov.execute(&mut env, &st, 3, &[]),
        MultisigError::TimeLockNotReleased,
    );
    assert_eq!(env.config().pending.kind, 0, "nothing proposed yet");
    env.warp(1);
    let res = g.gov.execute(&mut env, &st, 3, &[]);
    println!(
        "T-SQ-2 Governance execute propose_config(Authorities): {} B, {} CU",
        g.gov.last_size(),
        cu(&res)
    );
    ok(res);
    assert!(matches!(
        status(&g.gov, &env, st.index),
        sq::state::ProposalStatus::Executed(_)
    ));
    let c = env.config();
    assert_eq!(c.pending.kind, ConfigChange::KIND_AUTHORITIES);
    assert_eq!(c.pending.not_before, env.now() + i64::from(c.timelock_s));
    assert_eq!(c.timelock_s, shortwin::constants::DEFAULT_TIMELOCK_S);

    // The program: 1 s before `not_before`, refused; at it, applied — by anyone.
    env.warp(i64::from(c.timelock_s) - 1);
    assert_err(
        &env.send(&[ix::apply_config_for(&change)], &[]),
        ShortwinError::TimelockNotElapsed,
    );
    env.warp(1);
    ok(env.send(&[ix::apply_config_for(&change)], &[]));
    assert_eq!(env.config().coverage_authority, new_coverage.pubkey());
    assert_eq!(
        env.config().fee_recipient,
        g.fee_recipient(),
        "vault 1 passed check_fee_recipient"
    );
    assert_eq!(
        env.now() - approved_at,
        2 * GOV_TL,
        "96 h from the last approval"
    );

    // The window: a change applied 1 s after `not_before + 14 d` lapses ...
    let kink = kink_change(&env);
    ok(g.gov.run(
        &mut env,
        &[ix::propose_config(&g.governance(), kink.clone())],
        &[],
    ));
    let not_before = env.config().pending.not_before;
    env.set_clock(
        not_before + CONFIG_APPLY_WINDOW_S + 1,
        env.clock().slot + 10,
    );
    assert_err(
        &env.send(&[ix::apply_config_for(&kink)], &[]),
        ShortwinError::PendingChangeExpired,
    );
    // ... and one applied at the window's last second lands (Governance proposes it again).
    ok(g.gov.run(
        &mut env,
        &[ix::propose_config(&g.governance(), kink.clone())],
        &[],
    ));
    let not_before = env.config().pending.not_before;
    env.set_clock(not_before + CONFIG_APPLY_WINDOW_S, env.clock().slot + 10);
    let version = env.config().kink.version;
    ok(env.send(&[ix::apply_config_for(&kink)], &[]));
    assert_eq!(env.config().kink.version, version + 1);
    assert_eq!(env.config().pending.kind, 0);
}

// ---- T-SQ-3 ---------------------------------------------------------------------------------

/// One Emergency flow's figures.
struct Row {
    what: &'static str,
    txs: usize,
    sizes: Vec<usize>,
    cu: u64,
}

/// Runs `ixs` through the Emergency (stage by member 0; member 1's approval and the execution
/// in one transaction) and returns its figures.
fn emergency(env: &mut Env, g: &Governance, what: &'static str, ixs: &[Instruction]) -> Row {
    let before = g.emg.sent();
    let res = g.emg.run(env, ixs, &[]);
    let cu = cu(&res);
    let sizes = g.emg.sizes()[before..].to_vec();
    Row {
        what,
        txs: sizes.len(),
        sizes,
        cu,
    }
}

/// T-SQ-3: each Emergency power lands in at most two transactions of at most 1,232 bytes —
/// the worst-case `set_venue_extras` too, through the transaction buffer (its direct staging
/// would be over the limit): the buffer in the first, `[create_from_buffer, proposal_create,
/// approve, approve, execute]` co-signed by two members in the second.
#[test]
fn t_sq_3_emergency_powers_each_in_at_most_two_transactions() {
    let (mut env, g) = Env::squads_governed();
    let emg = g.pause_authority();
    let (market, _) = env.install_market(TokenProgram::Spl, 6);
    let market = market.market;
    // A coverage pause by the hot key, a flagged venue upgrade, a pending parameter change.
    ok(env.set_coverage_pause(&market, true));
    env.set_all_venue_program_data();
    let venue = 0u8;
    let new_slot = fork::FIXTURE_GUARD_SLOTS[usize::from(venue)] + 1;
    env.set_venue_program_data(venue, new_slot);
    ok(env.send(&[ix::flag_venue_upgraded(venue)], &[]));
    assert_ne!(env.venue_guard().upgraded & (1 << venue), 0);
    let kink = propose_kink(&env, &g.governance());
    ok(g.gov.run(&mut env, &[kink], &[]));
    assert_eq!(env.config().pending.kind, 3);

    let mut rows = Vec::new();
    rows.push(emergency(
        &mut env,
        &g,
        "set_pause(Global, true)",
        &[ix::set_pause(&emg, PauseScope::Global, true, None)],
    ));
    assert!(env.config().paused_opens());
    rows.push(emergency(
        &mut env,
        &g,
        "set_pause(Global, false)",
        &[ix::set_pause(&emg, PauseScope::Global, false, None)],
    ));
    assert!(!env.config().paused_opens());
    rows.push(emergency(
        &mut env,
        &g,
        "set_pause(Market, true)",
        &[ix::set_pause(&emg, PauseScope::Market, true, Some(market))],
    ));
    assert!(env.market(&market).has_flag(MARKET_FLAG_PAUSED_OPENS));
    rows.push(emergency(
        &mut env,
        &g,
        "set_coverage_pause(false) (clear)",
        &[ix::set_coverage_pause(
            &emg,
            &market,
            false,
            COVERAGE_REASON_KEEPER,
        )],
    ));
    assert!(!env.market(&market).has_flag(MARKET_FLAG_PAUSED_COVERAGE));
    // M7-SEC-3: the clear records the clock (I4's coverage clause judged it on the vault path).
    assert_eq!(env.market(&market).coverage_cleared_at, env.now());
    rows.push(emergency(
        &mut env,
        &g,
        "disable_venue(Raydium AMM v4)",
        &[ix::disable_venue(&emg, 2)],
    ));
    assert_eq!(env.config().venue_enabled & VENUE_BIT_RAYDIUM_V4, 0);
    rows.push(emergency(
        &mut env,
        &g,
        "revalidate_venue(0, slot)",
        &[ix::revalidate_venue(&emg, venue, new_slot)],
    ));
    assert_eq!(env.venue_guard().upgraded & (1 << venue), 0);
    assert_eq!(env.venue_guard().guard_slot[usize::from(venue)], new_slot);
    rows.push(emergency(
        &mut env,
        &g,
        "set_route_broken(Global, AMM v4)",
        &[engine::set_route_broken_ix(
            &emg,
            PauseScope::Global,
            2,
            true,
            None,
        )],
    ));
    assert_eq!(env.config().venue_route_broken, VENUE_BIT_RAYDIUM_V4);
    rows.push(emergency(
        &mut env,
        &g,
        "cancel_config (Kink pending)",
        &[ix::cancel_config(&emg)],
    ));
    assert_eq!(env.config().pending.kind, 0);
    rows.push(emergency(
        &mut env,
        &g,
        "set_venue_extras (1 fixed key)",
        &[small_extras(&emg)],
    ));

    // The worst case: staging it directly would be over the packet limit ...
    let worst = worst_extras(&emg);
    let msg = g.emg.compile(0, std::slice::from_ref(&worst), &[]);
    let direct = wire_size(
        &g.emg.members[0].pubkey(),
        &g.emg.stage_ixs(g.emg.next_index(&env), &msg, 0, 0),
    );
    assert!(direct > TX_MAX, "direct staging {direct} B");
    // ... so it goes through the buffer: two transactions.
    let version = pumpswap_extras(&env).sets[0].current.version;
    let before = g.emg.sent();
    let (st, res) = g
        .emg
        .stage_buffered(&mut env, 0, &[worst], 0, &[1], Some(1));
    let worst_cu = cu(&res);
    let sizes = g.emg.sizes()[before..].to_vec();
    assert!(matches!(
        status(&g.emg, &env, st.index),
        sq::state::ProposalStatus::Executed(_)
    ));
    assert!(
        g.emg.buffer(&env, 0, 0).is_none(),
        "the buffer closed into the transaction"
    );
    let set = pumpswap_extras(&env).sets[0].current;
    assert_eq!((set.version, set.count), (version + 1, 4));
    assert!(
        set.entries
            .iter()
            .all(|e| e.n_seeds == 4 && e.seeds.iter().all(|s| s.len == 32))
    );
    println!(
        "T-SQ-3 worst-case set_venue_extras: message {} B; direct staging {direct} B (> {TX_MAX})",
        msg.bytes.len()
    );
    rows.push(Row {
        what: "set_venue_extras worst (buffer)",
        txs: sizes.len(),
        sizes,
        cu: worst_cu,
    });

    println!("T-SQ-3 Emergency powers (2-of-4, time lock 0; each member pays its own fees):");
    for r in &rows {
        println!(
            "  {:<36} {} tx {:?} B; execute {} CU",
            r.what, r.txs, r.sizes, r.cu
        );
        assert!(r.txs <= 2, "{}: {} transactions", r.what, r.txs);
        assert!(r.sizes.iter().all(|s| *s <= TX_MAX));
        assert!(r.cu < 200_000);
    }
}

// ---- T-SQ-4 ---------------------------------------------------------------------------------

/// T-SQ-4: what the Emergency vault cannot do, with the program's own errors through the vault:
/// propose any change (`Unauthorized`), cancel an `Authorities` or `Timelock` change
/// (`EmergencyCannotCancel`, M1-SEC-1: Governance can always rotate a compromised Emergency),
/// set a coverage pause (`Unauthorized`). A refused vault transaction is not consumed: it stays
/// `Approved` (Squads has no expiry) until two members cancel it.
#[test]
fn t_sq_4_emergency_refusals() {
    let (mut env, g) = Env::squads_governed();
    let emg = g.pause_authority();
    let c = env.config();
    let mut refused = None;
    for change in [
        ConfigChange::Authorities {
            governance: emg,
            pause_authority: emg,
            coverage_authority: c.coverage_authority,
            fee_recipient: c.fee_recipient,
        },
        ConfigChange::Timelock { timelock_s: 86_400 },
        kink_change(&env),
        ConfigChange::Params {
            params: c.params,
            tiers: c.tiers,
        },
    ] {
        // Approved in its own transaction, so the refused execution leaves it Approved.
        let st = g.emg.stage(&mut env, &[ix::propose_config(&emg, change)]);
        ok(g.emg.approve(&mut env, st.index, 1));
        let res = g.emg.execute(&mut env, &st, 1, &[]);
        assert_err(&res, ShortwinError::Unauthorized);
        assert_eq!(env.config().pending.kind, 0);
        refused = Some(st);
    }
    // The last refused transaction is still Approved and executable later; two members cancel
    // it (`proposal_cancel_v2`), after which it cannot execute.
    let st = refused.unwrap();
    assert!(matches!(
        status(&g.emg, &env, st.index),
        sq::state::ProposalStatus::Approved(_)
    ));
    ok(g.emg.cancel(&mut env, st.index, 0));
    assert!(matches!(
        status(&g.emg, &env, st.index),
        sq::state::ProposalStatus::Approved(_)
    ));
    ok(g.emg.cancel(&mut env, st.index, 2));
    assert!(matches!(
        status(&g.emg, &env, st.index),
        sq::state::ProposalStatus::Cancelled(_)
    ));
    assert_squads_err(
        &g.emg.execute(&mut env, &st, 1, &[]),
        MultisigError::InvalidProposalStatus,
    );

    // Governance's pending Authorities and Timelock changes: the Emergency cannot cancel them.
    for change in [
        rotate_coverage(&env, &Pubkey::new_unique()),
        ConfigChange::Timelock {
            timelock_s: 259_200,
        },
    ] {
        ok(g.gov.run(
            &mut env,
            &[ix::propose_config(&g.governance(), change.clone())],
            &[],
        ));
        let kind = env.config().pending.kind;
        assert_err(
            &g.emg.run(&mut env, &[ix::cancel_config(&emg)], &[]),
            ShortwinError::EmergencyCannotCancel,
        );
        assert_eq!(env.config().pending.kind, kind);
        // Governance can (through its own 48 h).
        ok(g.gov
            .run(&mut env, &[ix::cancel_config(&g.governance())], &[]));
        assert_eq!(env.config().pending.kind, 0);
    }

    // The coverage pause: the Emergency only clears.
    let (market, _) = env.install_market(TokenProgram::Spl, 6);
    let res = g.emg.run(
        &mut env,
        &[ix::set_coverage_pause(
            &emg,
            &market.market,
            true,
            COVERAGE_REASON_KEEPER,
        )],
        &[],
    );
    assert_err(&res, ShortwinError::Unauthorized);
    assert!(
        !env.market(&market.market)
            .has_flag(MARKET_FLAG_PAUSED_COVERAGE)
    );
}

// ---- T-SQ-5 ---------------------------------------------------------------------------------

/// T-SQ-5: the upgrade authority moves from the deployer to the Governance vault with
/// `SetAuthorityChecked` — which refuses a new authority that does not sign, so a mistyped
/// vault can never receive it — inside a Governance vault transaction (the vault signs through
/// Squads, the deployer co-signs the execution after the time lock). Afterwards the deployer
/// and the Emergency vault have no loader power. The unchecked `SetAuthority` would hand the
/// authority to any key, a typo included.
#[test]
fn t_sq_5_upgrade_authority_transfer_checked_and_unchecked() {
    let (mut env, g) = Env::squads_governed();
    let deployer = env.upgrade_authority.insecure_clone();
    let vault = g.governance();

    // Checked, without the vault's signature (what a direct `solana program set-upgrade-
    // authority --new-upgrade-authority <vault>` would send): refused.
    let mut unsigned =
        loader::set_authority_checked(&pda::program_data(), &deployer.pubkey(), &vault);
    unsigned.accounts[2].is_signer = false;
    assert_ix_err(
        &env.send(&[unsigned], &[&deployer]),
        InstructionError::MissingRequiredSignature,
    );
    assert_eq!(loader::program_data(&env).1, Some(deployer.pubkey()));

    // Checked, through the Governance vault (the deployer co-signs the execution).
    let res = g.transfer_upgrade_authority(&mut env);
    println!(
        "T-SQ-5 SetAuthorityChecked through the Governance vault: execute {} B, {} CU",
        g.gov.last_size(),
        cu(&res)
    );
    ok(res);
    assert_eq!(loader::program_data(&env).1, Some(vault));
    assert!(env.admin_keys().contains(&vault));

    // The deployer has no loader power left; nor does the Emergency vault.
    let back = loader::set_authority(
        &pda::program_data(),
        &deployer.pubkey(),
        Some(&deployer.pubkey()),
    );
    assert_ix_err(
        &env.send(&[back], &[&deployer]),
        InstructionError::IncorrectAuthority,
    );
    let emg = g.pause_authority();
    let grab = loader::set_authority(&pda::program_data(), &emg, Some(&emg));
    assert_ix_err(
        &g.emg.run(&mut env, &[grab], &[]),
        InstructionError::IncorrectAuthority,
    );
    assert_eq!(loader::program_data(&env).1, Some(vault));

    // The unchecked hazard, on a fresh deployment: one flipped byte and the authority is gone.
    let (mut env, g) = Env::squads_governed();
    let deployer = env.upgrade_authority.insecure_clone();
    let mut typo = g.governance().to_bytes();
    typo[31] ^= 1;
    let typo = Pubkey::new_from_array(typo);
    let mut checked =
        loader::set_authority_checked(&pda::program_data(), &deployer.pubkey(), &typo);
    checked.accounts[2].is_signer = false;
    assert_ix_err(
        &env.send(&[checked], &[&deployer]),
        InstructionError::MissingRequiredSignature,
    );
    let unchecked = loader::set_authority(&pda::program_data(), &deployer.pubkey(), Some(&typo));
    ok(env.send(&[unchecked], &[&deployer]));
    assert_eq!(
        loader::program_data(&env).1,
        Some(typo),
        "handed to a key nobody holds"
    );
}

// ---- T-SQ-6 ---------------------------------------------------------------------------------

/// Stages an `Upgrade` from `buffer` through the Governance vault and approves it to the
/// threshold (the time lock starts).
fn gov_upgrade(env: &mut Env, g: &Governance, buffer: &Pubkey) -> Staged {
    let spill = g.gov.members[0].pubkey();
    gov_approved(env, g, &[loader::upgrade(buffer, &spill, &g.governance())])
}

/// T-SQ-6: an upgrade through the Governance vault, and its failure cases.
#[test]
fn t_sq_6_upgrade_through_the_vault() {
    let (mut env, g) = Env::squads_governed();
    ok(g.transfer_upgrade_authority(&mut env));
    let vault = g.governance();
    let elf = release_elf();

    // The Emergency vault cannot upgrade (its buffer names it, so only the authority differs).
    let emg = g.pause_authority();
    let buffer = loader::plant_buffer(&mut env, &elf, &emg);
    let spill = g.emg.members[0].pubkey();
    assert_ix_err(
        &g.emg
            .run(&mut env, &[loader::upgrade(&buffer, &spill, &emg)], &[]),
        InstructionError::IncorrectAuthority,
    );

    // A buffer whose authority is not the vault (the deployer's): refused at execution.
    let deployer = env.upgrade_authority.pubkey();
    let wrong = loader::plant_buffer(&mut env, &elf, &deployer);
    let st = gov_upgrade(&mut env, &g, &wrong);
    env.warp(GOV_TL);
    assert_ix_err(
        &g.gov.execute(&mut env, &st, 2, &[]),
        InstructionError::IncorrectAuthority,
    );
    // Not consumed: it stays Approved until Governance cancels it (3 cancel votes).
    assert!(matches!(
        status(&g.gov, &env, st.index),
        sq::state::ProposalStatus::Approved(_)
    ));
    for m in 0..2 {
        ok(g.gov.cancel(&mut env, st.index, m));
        assert!(matches!(
            status(&g.gov, &env, st.index),
            sq::state::ProposalStatus::Approved(_)
        ));
    }
    // An Emergency member cannot vote on a Governance proposal (no veto over upgrades).
    let outsider = g.emg.member(0);
    let res = g.gov.send(
        &mut env,
        &outsider,
        &[sq::ix::proposal_cancel_v2(
            &g.gov.key,
            st.index,
            &outsider.pubkey(),
        )],
        &[],
    );
    assert_squads_err(&res, MultisigError::NotAMember);
    ok(g.gov.cancel(&mut env, st.index, 2));
    assert!(matches!(
        status(&g.gov, &env, st.index),
        sq::state::ProposalStatus::Cancelled(_)
    ));
    assert_squads_err(
        &g.gov.execute(&mut env, &st, 3, &[]),
        MultisigError::InvalidProposalStatus,
    );

    // The upgrade proper: the release ELF from a vault-owned buffer, after the time lock.
    let buffer = loader::plant_buffer(&mut env, &elf, &vault);
    let buffer_lamports = env.svm.get_account(&buffer).unwrap().lamports;
    let st = gov_upgrade(&mut env, &g, &buffer);
    env.warp(GOV_TL - 1);
    assert_squads_err(
        &g.gov.execute(&mut env, &st, 2, &[]),
        MultisigError::TimeLockNotReleased,
    );
    env.warp(1);
    // The spill account (member 0) is not the fee payer (member 2 executes), so its credit is
    // exact: the loader moves the buffer's lamports and any ProgramData surplus over its rent
    // exemption to it.
    let spill = g.gov.members[0].pubkey();
    let spill_before = env.svm.get_account(&spill).unwrap().lamports;
    let pd_before = env.svm.get_account(&pda::program_data()).unwrap().lamports;
    let (slot_before, _) = loader::program_data(&env);
    let res = g.gov.execute(&mut env, &st, 2, &[]);
    println!(
        "T-SQ-6 Upgrade through the Governance vault: execute {} B, {} CU",
        g.gov.last_size(),
        cu(&res)
    );
    ok(res);
    let (slot, authority) = loader::program_data(&env);
    assert_eq!((slot, authority), (env.clock().slot, Some(vault)));
    assert_ne!(slot, slot_before);
    assert!(
        env.svm.get_account(&buffer).is_none_or(|a| a.lamports == 0),
        "buffer closed"
    );
    let pd_after = env.svm.get_account(&pda::program_data()).unwrap().lamports;
    assert_eq!(
        pd_after,
        env.svm
            .minimum_balance_for_rent_exemption(loader::program_data_len(&env)),
        "ProgramData left exactly rent-exempt"
    );
    assert_eq!(
        env.svm.get_account(&spill).unwrap().lamports,
        spill_before + buffer_lamports + pd_before - pd_after,
        "spill credit: the buffer's lamports and the ProgramData surplus"
    );
    // The upgraded program runs (from the next slot).
    env.warp(1);
    let emg = g.pause_authority();
    ok(g.emg.run(
        &mut env,
        &[ix::set_pause(&emg, PauseScope::Global, true, None)],
        &[],
    ));
    assert!(env.config().paused_opens());

    // A larger ELF (the test-venue build) does not fit the ProgramData: refused at execution
    // until anyone extends the ProgramData (permissionless; a member pays), then it lands.
    let bigger =
        std::fs::read(shortwin_litesvm_tests::test_venue_so_path()).expect("test-venue build");
    let len = loader::program_data_len(&env);
    assert!(bigger.len() + 45 > len, "the test-venue ELF is larger");
    let buffer = loader::plant_buffer(&mut env, &bigger, &vault);
    let st = gov_upgrade(&mut env, &g, &buffer);
    env.warp(GOV_TL);
    assert_ix_err(
        &g.gov.execute(&mut env, &st, 2, &[]),
        InstructionError::AccountDataTooSmall,
    );
    let payer = g.gov.member(4);
    let extra = u32::try_from(bigger.len() + 45 - len).unwrap().max(10_240);
    ok(env.send(&[loader::extend_program(&payer.pubkey(), extra)], &[&payer]));
    assert_eq!(loader::program_data_len(&env), len + extra as usize);
    env.warp(1); // the extension redeployed in its slot
    ok(g.gov.execute(&mut env, &st, 2, &[]));
    assert_eq!(loader::program_data(&env).1, Some(vault));
}

// ---- T-SQ-7 ---------------------------------------------------------------------------------

/// T-SQ-7: the Squads semantics ARCH §8.5 discloses, on the mainnet ELF.
///
/// 1. An `Approved` vault transaction survives a member rotation (`RemoveMember` +
///    `AddMember`, a time-locked config transaction): it still executes, while a proposal that
///    was only `Active` goes stale (`StaleProposal`) and so does an approved config transaction;
///    the removed member can no longer vote.
/// 2. An `Approved` vault transaction never expires: a year after its approval it executes —
///    and its `propose_config` replaces whatever change is pending then.
/// 3. Governance cannot cancel reactively inside the program window: its `cancel_config`,
///    approved the second the change was proposed, executes 172,800 s later at the earliest,
///    which is `not_before`, when anyone can already apply the change.
/// 4. An `Approved` vault transaction survives a threshold change: approved by 3, it executes
///    after the threshold became 4.
/// 5. A time-lock increase does not leave an `Approved` vault transaction untouched: Squads
///    checks the **current** time lock at execution, so it waits for the new one, counted
///    from its approval (refused 1 s short of it, past the old one; executed at it).
#[test]
fn t_sq_7_disclosed_semantics() {
    let (mut env, mut g) = Env::squads_governed();
    let gv = g.governance();

    // (1) X approved, Y only Active, D an approved config transaction; then the rotation R.
    let kink = propose_kink(&env, &gv);
    let x = gov_approved(&mut env, &g, std::slice::from_ref(&kink));
    let y = g.gov.stage(&mut env, &[kink]);
    let d = g
        .gov
        .config(&mut env, &[sq::ConfigAction::ChangeThreshold(3)]);
    g.gov.approve_to_threshold(&mut env, d);
    let newcomer = env.funded_keypair();
    let removed = g.gov.members[4].pubkey();
    let r = g.gov.config(
        &mut env,
        &[
            sq::ConfigAction::RemoveMember(removed),
            sq::ConfigAction::AddMember(sq::Member::full(newcomer.pubkey())),
        ],
    );
    g.gov.approve_to_threshold(&mut env, r);
    env.warp(GOV_TL);
    ok(g.gov.config_execute(&mut env, r, 1));
    let s = g.gov.state(&env);
    assert_eq!(s.stale_transaction_index, r);
    assert!(!s.is_member(&removed) && s.is_member(&newcomer.pubkey()));
    assert_eq!(s.members.len(), 5);
    // X still executes ...
    ok(g.gov.execute(&mut env, &x, 0, &[]));
    assert_eq!(env.config().pending.kind, 3);
    // ... Y cannot be approved any more, D cannot execute.
    assert_squads_err(
        &g.gov.approve(&mut env, y.index, 1),
        MultisigError::StaleProposal,
    );
    assert_squads_err(
        &g.gov.config_execute(&mut env, d, 0),
        MultisigError::StaleProposal,
    );
    // The removed member has no vote; the newcomer has.
    assert_squads_err(
        &g.gov.approve(&mut env, y.index, 4),
        MultisigError::NotAMember,
    );
    g.gov.members[4] = newcomer;

    // (2) Approved now, executed a year later; it replaces the change pending then.
    let old = gov_approved(
        &mut env,
        &g,
        &[ix::propose_config(
            &gv,
            ConfigChange::Timelock {
                timelock_s: 259_200,
            },
        )],
    );
    env.warp(365 * 86_400);
    let kink = propose_kink(&env, &gv);
    ok(g.gov.run(&mut env, &[kink], &[]));
    assert_eq!(env.config().pending.kind, 3);
    ok(g.gov.execute(&mut env, &old, 1, &[]));
    assert_eq!(
        env.config().pending.kind,
        ConfigChange::KIND_TIMELOCK,
        "replaced"
    );

    // (3) The reactive cancel: proposal at t, Governance's cancel approved at t, executable at
    // t + 172,800 = not_before; an apply in that second lands first and the cancel finds
    // nothing.
    ok(g.gov.run(&mut env, &[ix::cancel_config(&gv)], &[]));
    let change = rotate_coverage(&env, &Pubkey::new_unique());
    let p = gov_approved(&mut env, &g, &[ix::propose_config(&gv, change.clone())]);
    env.warp(GOV_TL);
    ok(g.gov.execute(&mut env, &p, 0, &[]));
    let not_before = env.config().pending.not_before;
    let cancel = gov_approved(&mut env, &g, &[ix::cancel_config(&gv)]);
    env.warp(GOV_TL - 1);
    assert_squads_err(
        &g.gov.execute(&mut env, &cancel, 0, &[]),
        MultisigError::TimeLockNotReleased,
    );
    env.warp(1);
    assert_eq!(
        env.now(),
        not_before,
        "the cancel's first second is not_before"
    );
    ok(env.send(&[ix::apply_config_for(&change)], &[]));
    assert_err(
        &g.gov.execute(&mut env, &cancel, 0, &[]),
        ShortwinError::NoPendingChange,
    );
    // The Emergency, by contrast, cancels a parameter change at once (T-SQ-3).

    // (4) Approved by 3 of 5, then the threshold goes to 4 (a config transaction): it still
    // executes with its 3 approvals.
    let kink = propose_kink(&env, &gv);
    let a = gov_approved(&mut env, &g, &[kink]);
    let approvals = g
        .gov
        .proposal(&env, a.index)
        .expect("proposal")
        .approved
        .len();
    assert_eq!(approvals, 3);
    let t = g
        .gov
        .config(&mut env, &[sq::ConfigAction::ChangeThreshold(4)]);
    g.gov.approve_to_threshold(&mut env, t);
    env.warp(GOV_TL);
    ok(g.gov.config_execute(&mut env, t, 0));
    let s = g.gov.state(&env);
    assert_eq!(s.threshold, 4);
    assert!(
        s.stale_transaction_index >= a.index,
        "a is below the stale index"
    );
    ok(g.gov.execute(&mut env, &a, 0, &[]));
    assert_eq!(env.config().pending.kind, 3);

    // (5) Approved under a 172,800 s time lock, which then doubles: refused past the old lock
    // and 1 s short of the new one, counted from the approval; executed at it.
    let c = gov_approved(&mut env, &g, &[ix::cancel_config(&gv)]);
    let sq::state::ProposalStatus::Approved(approved_at) = status(&g.gov, &env, c.index) else {
        panic!("not approved");
    };
    let doubled = 2 * Governance::GOV_TIME_LOCK;
    let t = g
        .gov
        .config(&mut env, &[sq::ConfigAction::SetTimeLock(doubled)]);
    g.gov.approve_to_threshold(&mut env, t);
    env.warp(GOV_TL);
    ok(g.gov.config_execute(&mut env, t, 0));
    let s = g.gov.state(&env);
    assert_eq!(s.time_lock, doubled);
    assert!(
        s.stale_transaction_index >= c.index,
        "c is below the stale index"
    );
    assert!(env.now() >= approved_at + GOV_TL, "past the old time lock");
    env.warp(approved_at + i64::from(doubled) - 1 - env.now());
    assert_squads_err(
        &g.gov.execute(&mut env, &c, 0, &[]),
        MultisigError::TimeLockNotReleased,
    );
    env.warp(1);
    ok(g.gov.execute(&mut env, &c, 0, &[]));
    assert_eq!(env.config().pending.kind, 0, "cancelled");
}

// ---- T-SQ-8 ---------------------------------------------------------------------------------

/// T-SQ-8: every governance flow's transactions fit 1,232 bytes (the worst-case extras on the
/// buffer path: T-SQ-3) and every execution stays under 200,000 CU. Executes each Governance
/// `propose_config` variant (the largest is `Params`) and prints the table.
#[test]
fn t_sq_8_sizes_and_cu() {
    let (mut env, g) = Env::squads_governed();
    let gv = g.governance();
    let c = env.config();
    let changes = [
        (
            "propose_config(Params)",
            ConfigChange::Params {
                params: c.params,
                tiers: c.tiers,
            },
        ),
        (
            "propose_config(EmaParams)",
            ConfigChange::EmaParams {
                ema: c.ema_params.values(),
            },
        ),
        ("propose_config(Kink)", kink_change(&env)),
        (
            "propose_config(Authorities)",
            rotate_coverage(&env, &Pubkey::new_unique()),
        ),
        (
            "propose_config(Timelock)",
            ConfigChange::Timelock {
                timelock_s: 172_800,
            },
        ),
        (
            "propose_config(EnableVenues)",
            ConfigChange::EnableVenues {
                bits: VENUE_BIT_RAYDIUM_V4,
            },
        ),
        (
            "propose_config(OracleFeed)",
            ConfigChange::OracleFeed {
                account: c.pyth_sol_usd,
                feed_id: c.pyth_feed_id,
            },
        ),
    ];
    println!(
        "T-SQ-8 Governance (3-of-5, 172,800 s): stage / approve / approve / execute bytes; execute CU"
    );
    let mut flows: Vec<(String, Vec<Instruction>)> = changes
        .iter()
        .map(|(n, ch)| (n.to_string(), vec![ix::propose_config(&gv, ch.clone())]))
        .collect();
    flows.push(("cancel_config".into(), vec![ix::cancel_config(&gv)]));
    flows.push((
        "set_pause(Global)".into(),
        vec![ix::set_pause(&gv, PauseScope::Global, false, None)],
    ));
    flows.push((
        "set_venue_extras (1 fixed key)".into(),
        vec![small_extras(&gv)],
    ));
    let mut max_execute = 0;
    for (name, ixs) in &flows {
        let before = g.gov.sent();
        let res = g.gov.run(&mut env, ixs, &[]);
        let units = cu(&res);
        let sizes = g.gov.sizes()[before..].to_vec();
        println!("  {name:<32} {sizes:?} B; {units} CU");
        assert_eq!(sizes.len(), 4, "stage, two approvals, execute");
        assert!(sizes.iter().all(|s| *s <= TX_MAX));
        assert!(units < 200_000);
        max_execute = max_execute.max(*sizes.last().unwrap());
    }
    // Params is the largest message a direct staging carries.
    let params = g.gov.compile(0, &flows[0].1, &[]);
    let stage = wire_size(
        &g.gov.members[0].pubkey(),
        &g.gov.stage_ixs(1, &params, 0, 0),
    );
    println!(
        "  largest direct staging: propose_config(Params), message {} B, transaction {stage} B (margin {} B)",
        params.bytes.len(),
        TX_MAX - stage
    );
    assert!(stage <= TX_MAX);
    // The worst-case extras cannot be staged directly (T-SQ-3 stages it through the buffer).
    let worst = g.emg.compile(0, &[worst_extras(&g.pause_authority())], &[]);
    let direct = wire_size(
        &g.emg.members[0].pubkey(),
        &g.emg.stage_ixs(1, &worst, 0, 0),
    );
    println!(
        "  worst-case set_venue_extras: message {} B, direct staging {direct} B: buffer path",
        worst.bytes.len()
    );
    assert!(direct > TX_MAX);
    assert!(max_execute <= TX_MAX);

    // The same flows through an address lookup table holding the static accounts: the message
    // loads them (Squads resolves the table on execution).
    let alt = Pubkey::new_unique();
    let addresses = vec![pda::config(), pda::event_authority(), shortwin::ID];
    shortwin_litesvm_tests::raydium::install_alt(&mut env.svm, &alt, &addresses);
    let table = sq::LookupTable {
        key: alt,
        addresses,
    };
    let ixs = [ix::set_pause(&gv, PauseScope::Global, true, None)];
    let plain = g.gov.compile(0, &ixs, &[]);
    let st = g
        .gov
        .stage_with(&mut env, 0, &ixs, std::slice::from_ref(&table));
    assert_eq!(st.message.lookups.len(), 1);
    assert!(st.message.bytes.len() < plain.bytes.len());
    ok(g.gov.finish(&mut env, &st, &[]));
    assert!(env.config().paused_opens());
    println!(
        "  set_pause through an ALT: message {} B (plain {} B)",
        st.message.bytes.len(),
        plain.bytes.len()
    );
}

// ---- T-SQ-9 ---------------------------------------------------------------------------------

/// The signer-only rule I4 had before the vault path.
fn signer_only(ix: &Instruction, admins: &[Pubkey]) -> bool {
    ix.accounts
        .iter()
        .any(|m| m.is_signer && admins.contains(&m.pubkey))
}

/// T-SQ-9: I4 on vault-executed instructions. The authorities are vaults, which never sign a
/// transaction; `is_admin_signed` reads a top-level `vault_transaction_execute` carrying one
/// as admin-signed. An Emergency-vault execution that takes tokens out of a `pool_vault` (the
/// vault withdrawing its own lender position — what I4 forbids for any admin-signed
/// instruction) is caught; under the signer-only rule it passed.
#[test]
fn t_sq_9_i4_on_vault_executed_instructions() {
    let (mut env, g) = Env::squads_governed();
    let admins = env.admin_keys();
    let gv = g.governance();
    let emg = g.pause_authority();

    // A third, non-admin multisig.
    let other = SquadsMs::create(&mut env, 2, 2, 0);
    let ov = other.vault(0);
    let gov_exec = g.gov.execute_ix(
        &Staged {
            index: 1,
            vault_index: 0,
            message: g.gov.compile(0, &[ix::cancel_config(&gv)], &[]),
        },
        0,
    );
    let emg_exec = g.emg.execute_ix(
        &Staged {
            index: 1,
            vault_index: 0,
            message: g.emg.compile(0, &[ix::cancel_config(&emg)], &[]),
        },
        0,
    );
    let other_exec = other.execute_ix(
        &Staged {
            index: 1,
            vault_index: 0,
            message: other.compile(
                0,
                &[ix::set_pause(&ov, PauseScope::Global, true, None)],
                &[],
            ),
        },
        0,
    );
    for (what, e, admin) in [
        ("Governance", &gov_exec, true),
        ("Emergency", &emg_exec, true),
        ("non-admin", &other_exec, false),
    ] {
        assert_eq!(invariants::is_admin_signed(e, &admins), admin, "{what}");
        assert!(
            !signer_only(e, &admins),
            "{what}: the signer-only rule never saw a vault"
        );
    }
    // A non-admin multisig's vault cannot act (and its execution is not admin-signed).
    assert_err(
        &other.run(
            &mut env,
            &[ix::set_pause(&ov, PauseScope::Global, true, None)],
            &[],
        ),
        ShortwinError::Unauthorized,
    );

    // The Emergency vault as a lender: deposits (I4 holds: nothing decreases) ...
    let (k, authority) = env.install_market(TokenProgram::Spl, 6);
    env.fund(&emg, 1_000_000_000);
    let payer = env.payer.insecure_clone();
    let ata = tokens::create_ata(&mut env.svm, &payer, &emg, &k.mint, TokenProgram::Spl);
    tokens::mint_to(
        &mut env.svm,
        &payer,
        &k.mint,
        &ata,
        &authority,
        1_000_000,
        TokenProgram::Spl,
    );
    let lk = ix::LenderKeys {
        owner: emg,
        mint: k.mint,
        token_program: k.token_program,
        owner_token_account: ata,
    };
    ok(g.emg.run(&mut env, &[ix::deposit(&lk, 1_000_000, 0)], &[]));
    let shares = env.lender(&k.market, &emg).expect("lender").shares;
    assert!(shares > 0);

    // ... then withdraws through the vault. Executed on a copy of the VM, I4 flags it under
    // the vault-path rule and passes it under the signer-only rule.
    let st = g.emg.stage(&mut env, &[ix::withdraw(&lk, shares, 0)]);
    let exec = g.emg.execute_ix(&st, 1);
    let approve = sq::ix::proposal_approve(&g.emg.key, st.index, &g.emg.members[1].pubkey());
    let m1 = g.emg.member(1);
    let mut svm = env.svm.clone();
    let tx = solana_transaction::Transaction::new_signed_with_payer(
        std::slice::from_ref(&approve),
        Some(&m1.pubkey()),
        &[&m1],
        svm.latest_blockhash(),
    );
    svm.send_transaction(tx).expect("approve");
    let pre = invariants::snapshot(&svm, &[k.market]);
    let tx = solana_transaction::Transaction::new_signed_with_payer(
        std::slice::from_ref(&exec),
        Some(&m1.pubkey()),
        &[&m1],
        svm.latest_blockhash(),
    );
    let meta = svm.send_transaction(tx).expect("execute the withdrawal");
    assert!(read::token_amount(&svm, &k.pool_vault).unwrap_or(0) < 1_000_000);
    let ev = invariants::IxEvents::of_instruction(&meta, 0, &exec);
    let admins = env.admin_keys();
    let v = invariants::check_ix(&svm, &pre, invariants::is_admin_signed(&exec, &admins), &ev)
        .expect_err("I4 on the vault path");
    assert_eq!(v.invariant, "I4");
    assert!(invariants::check_ix(&svm, &pre, signer_only(&exec, &admins), &ev).is_ok());

    // And through Env::send the hooks stop it.
    ok(g.emg.approve(&mut env, st.index, 1));
    let caught = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let _ = g.emg.execute(&mut env, &st, 1, &[]);
    }));
    let msg = caught.expect_err("the hooks panic on I4");
    let text = msg
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| msg.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default();
    assert!(text.contains("I4"), "{text}");
}

/// T-SQ-9 (coverage clause): the Emergency's coverage clear in production is a Squads
/// `vault_transaction_execute` whose message runs `set_coverage_pause(false)` signed by the
/// Emergency vault. I4's coverage clause reads it through the `VaultTransaction`: the snapshot is
/// taken, the events carry the market and the vault as the signer, and the clause holds the
/// executed clear to ★OWNER-M7-3 (b) — the flag cleared, `coverage_cleared_at` at the clock's
/// second, nothing else changed. Mutated post-states fail it; under the top-level-only reading
/// (no coverage snapshot, `coverage_pause` false) every one of them passed.
#[test]
fn t_sq_9_i4_coverage_clause_on_the_vault_path() {
    let (mut env, g) = Env::squads_governed();
    let emg = g.pause_authority();
    let (k, _) = env.install_market(TokenProgram::Spl, 6);
    let market = k.market;
    ok(env.set_coverage_pause(&market, true));
    assert!(env.market(&market).has_flag(MARKET_FLAG_PAUSED_COVERAGE));
    env.warp(5);

    let clear = ix::set_coverage_pause(&emg, &market, false, COVERAGE_REASON_KEEPER);
    let st = g.emg.stage(&mut env, std::slice::from_ref(&clear));
    ok(g.emg.approve(&mut env, st.index, 1));
    let exec = g.emg.execute_ix(&st, 1);
    // Read through the VaultTransaction: the market, and the Emergency vault as the signer
    // (not member 1, who sends the transaction).
    assert_eq!(
        invariants::vault_coverage_pause(&env.svm, &exec),
        Some((vec![market], vec![emg]))
    );
    assert!(!invariants::is_coverage_pause(&exec));
    let pre = invariants::snapshot_for(&env.svm, &[market], std::slice::from_ref(&exec));
    assert!(
        pre.coverage.is_some(),
        "the vault path takes the coverage snapshot"
    );
    // What the top-level-only reading took: no coverage snapshot.
    let pre_top = invariants::snapshot(&env.svm, &[market]);
    let pre_top = invariants::TxSnapshot {
        coverage: None,
        ..pre_top
    };

    // Executed on a copy of the VM.
    let m1 = g.emg.member(1);
    let mut svm = env.svm.clone();
    let tx = solana_transaction::Transaction::new_signed_with_payer(
        std::slice::from_ref(&exec),
        Some(&m1.pubkey()),
        &[&m1],
        svm.latest_blockhash(),
    );
    let meta = svm.send_transaction(tx).expect("execute the clear");
    let now = svm
        .get_sysvar::<anchor_lang::solana_program::clock::Clock>()
        .unix_timestamp;
    let admins = env.admin_keys();
    let admin = invariants::is_admin_signed(&exec, &admins);
    assert!(admin);
    let ev = invariants::IxEvents::of_instruction_in(&svm, &meta, 0, &exec);
    assert!(ev.coverage_pause);
    assert_eq!(
        (ev.coverage_markets.clone(), ev.signers.clone()),
        (vec![market], vec![emg])
    );
    let ev_top = invariants::IxEvents::of_instruction(&meta, 0, &exec);
    assert!(!ev_top.coverage_pause);
    let after = svm.clone();
    let m = read::zc::<shortwin::state::Market>(&after, &market).expect("market");
    assert!(!m.has_flag(MARKET_FLAG_PAUSED_COVERAGE));
    assert_eq!(m.coverage_cleared_at, now);
    assert_eq!(
        invariants::check_ix(&after, &pre, admin, &ev),
        Ok(()),
        "the real Emergency-vault clear passes"
    );

    // Mutated post-states of the same execution.
    let raw = |svm: &mut litesvm::LiteSVM, f: &dyn Fn(&mut Vec<u8>)| {
        let mut a = svm.get_account(&market).unwrap();
        f(&mut a.data);
        svm.set_account(market, a).unwrap();
    };
    let at = invariants::MARKET_COVERAGE_CLEARED_AT_OFFSET;
    let flags = invariants::MARKET_FLAGS_OFFSET;
    let cov_bit = u8::try_from(MARKET_FLAG_PAUSED_COVERAGE).unwrap();
    let opens_bit = u8::try_from(MARKET_FLAG_PAUSED_OPENS).unwrap();
    let mutants: [(&str, DataMutant); 4] = [
        (
            "coverage_cleared_at one second early",
            Box::new(move |d: &mut Vec<u8>| {
                d[at..at + 8].copy_from_slice(&(now - 1).to_le_bytes())
            }),
        ),
        (
            "coverage_cleared_at not recorded",
            Box::new(move |d: &mut Vec<u8>| d[at..at + 8].copy_from_slice(&0i64.to_le_bytes())),
        ),
        (
            "the flag left set",
            Box::new(move |d: &mut Vec<u8>| {
                d[flags] |= cov_bit;
                d[at..at + 8].copy_from_slice(&0i64.to_le_bytes());
            }),
        ),
        (
            "paused_opens set too",
            Box::new(move |d: &mut Vec<u8>| d[flags] |= opens_bit),
        ),
    ];
    for (what, f) in &mutants {
        let mut svm = after.clone();
        raw(&mut svm, f.as_ref());
        let v = invariants::check_ix(&svm, &pre, admin, &ev).expect_err(what);
        assert_eq!(v.invariant, "I4", "{what}");
        // The top-level-only reading checked none of it.
        assert_eq!(
            invariants::check_ix(&svm, &pre_top, admin, &ev_top),
            Ok(()),
            "{what}: unseen before the vault path"
        );
    }
    // Signed by a vault that is neither the Emergency nor the role (only a broken program check
    // lets it land): the clause fails it even though only the flag changed.
    let foreign = invariants::IxEvents {
        signers: vec![g.governance()],
        ..ev
    };
    assert_eq!(
        invariants::check_ix(&after, &pre, admin, &foreign).map_err(|v| v.invariant),
        Err("I4")
    );

    // Through Env::send (the hooks judge it): it lands, clear at the clock.
    ok(g.emg.execute(&mut env, &st, 1, &[]));
    assert!(!env.market(&market).has_flag(MARKET_FLAG_PAUSED_COVERAGE));
    assert_eq!(env.market(&market).coverage_cleared_at, env.now());
}
