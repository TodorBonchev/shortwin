//! T-COVERAGE's `set_coverage_pause` part (ARCH §3.1, §8.2 T-COVERAGE; A69; **O17**, recorded by
//! the owner in SPEC §14 on 2026-09-30): the keeper's pause-opens-only `coverage_authority` for
//! degraded mode, and — since the owner's decision of 2026-10-01 (★OWNER-M7-3 (b), SPEC §14
//! M7-3) — the Emergency vault's clear of it. (The emission part, `CoverageGap` at 41 s and not at
//! 40 s since `COVERAGE_GAP_S` became 40 (OWNER-M7-12), is `crank_price_it`'s.)
//!
//! | Module | What |
//! |---|---|
//! | `role` | inert while `coverage_authority` is unset (`CoverageAuthorityDisabled`, whoever signs: Governance, the upgrade authority, any key — and the Emergency vault for a set; its clear is `emergency`'s); installed by a timelocked `Authorities` change, then only that key sets (`Unauthorized`) and only it or the Emergency clears; a rotation revokes the old key and keeps the flag; setting the role back to the default key makes it inert again; the argument and account checks (reason, market PDA and version, config, signer, the event accounts); idempotence; independence from `set_pause`; the event; CU and trace entries |
//! | `emergency` | ★OWNER-M7-3 (b) (owner decision of 2026-10-01): the Emergency vault (`pause_authority`) clears a set flag — with the role installed, unset and rotated — records `coverage_cleared_at`, after which opens wait `COVERAGE_CLEAR_DELAY_S`; it never sets one (`Unauthorized` with the role installed, `CoverageAuthorityDisabled` without); its clear is idempotent, emits `CoveragePauseSet{by: pause_authority}` and passes the reason check; Governance still cannot sign either way; on the whole VM it changes the flag and the clock word only; `[Emergency clear, open_short]` is refused by the delay; and the hijacked-coverage-key drill: a stolen key re-pauses markets until Governance's timelocked rotation lands, and each of its sets is undone by one Emergency clear while exits land throughout |
//! | `moves_nothing` | I4's coverage clause, checked here on the whole VM: the instruction changes nothing but bit 6 of one market's `flags` (every short.win, token, venue and system account byte for byte, lamports included, the fee payer aside) — with positions, lenders and protocol fees on the market |
//! | `exits` | pause-opens-only on the mock, **differentially**: every exit and lender instruction — `close_short`, `add_margin`, `crank_price`, `deposit`, `withdraw`, `claim_sol`, `sync_lender`, `sweep_trader`, `liquidate`, `expire`, `sol_settle` S1, S2 and S3 — runs from the same state with and without the pause and leaves the same events and the same VM (the flag aside), while `open_short` is refused `CoveragePaused`; clearing the flag restores opens |
//! | `venues` | the same on the real venues' forks: the pump curve, PumpSwap, Raydium AMM v4 and CPMM (open refused, crank and close identical, cleared → an open lands), and Raydium pool selection (`propose_pool`'s zero-position adoption, the fast path's `propose_pool` / `execute_pool_switch` with positions) under the pause |
//!
//! The harness's `Env` has no coverage-authority helper and its I4 hook still carries the M7
//! TRACK note (shared files, owned elsewhere): this binary installs the key itself — through the
//! timelocked `Authorities` change where the role is under test, by writing `GlobalConfig.
//! coverage_authority` (@96) where a scenario needs the key without a 48 h warp — and checks
//! I4's coverage clause itself. `Env::send` counts the installed key as an admin key, so the
//! harness's I4 (no admin-signed decrease of a vault or position) runs on every pause too.

#![allow(clippy::result_large_err)]

#[allow(unused_imports)]
#[path = "settle_support.rs"]
mod support;

#[allow(unused_imports)]
#[path = "ray_it/common.rs"]
mod common;

#[allow(unused_imports)]
#[path = "pool_select_it/support.rs"]
mod ps;

#[allow(unused_imports)]
#[path = "pump_it/common.rs"]
mod pumpc;

#[allow(unused_imports)]
#[path = "pamm_it/common.rs"]
mod pammc;

/// The instruction, the role's installation, and the whole-VM snapshots.
mod cov {
    use std::collections::BTreeMap;

    use anchor_lang::prelude::Pubkey;
    use anchor_lang::solana_program::instruction::Instruction;
    use anchor_lang::{InstructionData, ToAccountMetas};
    use litesvm::LiteSVM;
    use litesvm::types::TransactionMetadata;
    use shortwin::constants::{
        MOCK_CPAMM_PROGRAM_ID, PUMP_AMM_PROGRAM_ID, PUMP_FEES_PROGRAM_ID, PUMP_PROGRAM_ID,
        RAYDIUM_CPMM_PROGRAM_ID, RAYDIUM_V4_PROGRAM_ID, TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID,
    };
    use shortwin::events::{COVERAGE_REASON_KEEPER, CoveragePauseSet};
    use shortwin::state::{ConfigChange, MARKET_FLAG_PAUSED_COVERAGE};
    use shortwin_litesvm_tests::errors::ok;
    use shortwin_litesvm_tests::{Env, TxResult, events, ix, pda};
    use solana_keypair::Keypair;
    use solana_signer::Signer;

    /// `GlobalConfig.version` (discriminator included; `state::config` layout).
    pub const CONFIG_VERSION_OFFSET: usize = 8;
    /// `GlobalConfig.coverage_authority`.
    pub const CONFIG_COVERAGE_AUTHORITY_OFFSET: usize = 96;
    /// `Market.version`.
    pub const MARKET_VERSION_OFFSET: usize = 8;
    /// `Market.flags` (u16; bit 6 `paused_coverage` is in its low byte).
    pub const MARKET_FLAGS_OFFSET: usize = 10;
    /// `Market.coverage_cleared_at` (i64; M7-SEC-3).
    pub const MARKET_COVERAGE_CLEARED_AT_OFFSET: usize = 2_928;

    /// Warps past the clear delay (M7-SEC-3: opens refuse `CoveragePaused` for
    /// `COVERAGE_CLEAR_DELAY_S` after a clear of a set flag).
    pub fn wait_clear_delay(env: &mut Env) {
        env.warp(shortwin::constants::COVERAGE_CLEAR_DELAY_S);
    }

    /// The market's `coverage_cleared_at`.
    pub fn cleared_at(env: &Env, market: &Pubkey) -> i64 {
        env.market(market).coverage_cleared_at
    }

    /// `set_coverage_pause(paused, reason)` signed by `authority` on `market`.
    pub fn ix_with(authority: &Pubkey, market: &Pubkey, paused: bool, reason: u8) -> Instruction {
        Instruction {
            program_id: shortwin::ID,
            accounts: shortwin::accounts::SetCoveragePause {
                coverage_authority: *authority,
                config: pda::config(),
                market: *market,
                event_authority: pda::event_authority(),
                program: shortwin::ID,
            }
            .to_account_metas(None),
            data: shortwin::instruction::SetCoveragePause { paused, reason }.data(),
        }
    }

    /// `set_coverage_pause(paused, KeeperCoverage)`.
    pub fn ix(authority: &Pubkey, market: &Pubkey, paused: bool) -> Instruction {
        ix_with(authority, market, paused, COVERAGE_REASON_KEEPER)
    }

    /// Sends `set_coverage_pause(paused)` signed by `kp`.
    pub fn send(env: &mut Env, kp: &Keypair, market: &Pubkey, paused: bool) -> TxResult {
        env.send(&[ix(&kp.pubkey(), market, paused)], &[kp])
    }

    /// `set_coverage_pause(paused)` that must succeed: checks the flag and the event.
    pub fn set(env: &mut Env, kp: &Keypair, market: &Pubkey, paused: bool) -> TransactionMetadata {
        let meta = ok(send(env, kp, market, paused));
        let ev: CoveragePauseSet = events::one(&meta);
        assert_eq!(
            (ev.market, ev.paused, ev.reason, ev.by),
            (*market, paused, COVERAGE_REASON_KEEPER, kp.pubkey())
        );
        assert_eq!((ev.slot, ev.ts), (env.clock().slot, env.now()));
        assert_eq!(is_paused(env, market), paused);
        meta
    }

    /// The market's `paused_coverage`.
    pub fn is_paused(env: &Env, market: &Pubkey) -> bool {
        env.market(market).has_flag(MARKET_FLAG_PAUSED_COVERAGE)
    }

    /// The `Authorities` change that installs `key` as `coverage_authority`, the rest as it is.
    pub fn authorities(env: &Env, key: Pubkey) -> ConfigChange {
        let c = env.config();
        ConfigChange::Authorities {
            governance: c.governance,
            pause_authority: c.pause_authority,
            coverage_authority: key,
            fee_recipient: c.fee_recipient,
        }
    }

    /// Governance installs `key` as `coverage_authority` through the timelock: `propose_config`,
    /// `timelock_s` later `apply_config` (ARCH §3.1; the path O17's role takes in production).
    pub fn install_by_governance(env: &mut Env, key: Pubkey) {
        let change = authorities(env, key);
        let g = env.governance.insecure_clone();
        ok(env.send(&[ix::propose_config(&g.pubkey(), change.clone())], &[&g]));
        env.warp(i64::from(env.config().timelock_s));
        ok(env.send(&[ix::apply_config_for(&change)], &[]));
        assert_eq!(env.config().coverage_authority, key);
    }

    /// **TEST-ONLY**: writes `key` into `GlobalConfig.coverage_authority` (@96) directly — the
    /// state an applied `Authorities` change leaves, without the 48 h warp a running scenario
    /// cannot take ([`install_by_governance`] is the real path).
    pub fn plant(env: &mut Env, key: Pubkey) {
        let cfg = pda::config();
        let mut acc = env.svm.get_account(&cfg).expect("config");
        let at = CONFIG_COVERAGE_AUTHORITY_OFFSET;
        acc.data[at..at + 32].copy_from_slice(key.as_ref());
        env.svm.set_account(cfg, acc).expect("set config");
        assert_eq!(env.config().coverage_authority, key);
    }

    /// A new coverage key, planted ([`plant`]). It never pays: the harness's payer does.
    pub fn planted(env: &mut Env) -> Keypair {
        let kp = Keypair::new();
        plant(env, kp.pubkey());
        kp
    }

    /// Every account of the VM that a protocol, venue or token program, or the System program,
    /// owns — keyed, with lamports, owner and data.
    pub type World = BTreeMap<Pubkey, (u64, Pubkey, Vec<u8>)>;

    /// The owners [`world`] snapshots.
    pub const OWNERS: [Pubkey; 10] = [
        shortwin::ID,
        TOKEN_PROGRAM_ID,
        TOKEN_2022_PROGRAM_ID,
        MOCK_CPAMM_PROGRAM_ID,
        PUMP_PROGRAM_ID,
        PUMP_AMM_PROGRAM_ID,
        PUMP_FEES_PROGRAM_ID,
        RAYDIUM_V4_PROGRAM_ID,
        RAYDIUM_CPMM_PROGRAM_ID,
        anchor_lang::system_program::ID,
    ];

    /// The whole state [`OWNERS`] hold (the fee payer's lamports excluded: it pays every
    /// transaction's fee).
    pub fn world(env: &Env) -> World {
        world_of(&env.svm, &env.payer.pubkey())
    }

    /// [`world`] on `svm`, excluding `payer`.
    pub fn world_of(svm: &LiteSVM, payer: &Pubkey) -> World {
        let mut w = World::new();
        for owner in OWNERS {
            for (k, a) in svm.get_program_accounts(&owner) {
                if k != *payer {
                    w.insert(k, (a.lamports, a.owner, a.data));
                }
            }
        }
        w
    }

    /// `a` and `b` are the same state except for `market`'s `paused_coverage` bit, which is
    /// `flag_a` in `a` and `flag_b` in `b` (I4's coverage clause on the whole VM).
    #[track_caller]
    pub fn assert_same_but_flag(a: &World, b: &World, market: &Pubkey, flag_a: bool, flag_b: bool) {
        assert_same_but(a, b, market, flag_a, flag_b, None);
    }

    /// [`assert_same_but_flag`], and `market`'s `coverage_cleared_at` is `cleared_at` in `b`
    /// (M7-SEC-3: a clear of a set flag records the clock there; nothing else changes).
    #[track_caller]
    pub fn assert_same_but(
        a: &World,
        b: &World,
        market: &Pubkey,
        flag_a: bool,
        flag_b: bool,
        cleared_at: Option<i64>,
    ) {
        let cat = MARKET_COVERAGE_CLEARED_AT_OFFSET..MARKET_COVERAGE_CLEARED_AT_OFFSET + 8;
        let keys_a: Vec<_> = a.keys().collect();
        let keys_b: Vec<_> = b.keys().collect();
        assert_eq!(keys_a, keys_b, "the same accounts exist");
        let bit = u8::try_from(MARKET_FLAG_PAUSED_COVERAGE).expect("low byte");
        for (k, (la, oa, da)) in a {
            let (lb, ob, db) = &b[k];
            assert_eq!((la, oa), (lb, ob), "lamports / owner of {k}");
            if k == market {
                assert_eq!(da.len(), db.len());
                let at = MARKET_FLAGS_OFFSET;
                assert_eq!(da[at] & bit != 0, flag_a, "flag before");
                assert_eq!(db[at] & bit != 0, flag_b, "flag after");
                if let Some(t) = cleared_at {
                    assert_eq!(
                        db.get(cat.clone())
                            .map(|s| i64::from_le_bytes(s.try_into().expect("8"))),
                        Some(t),
                        "coverage_cleared_at of {k}"
                    );
                }
                for (i, (x, y)) in da.iter().zip(db).enumerate() {
                    if i == at {
                        assert_eq!(x & !bit, y & !bit, "no other flag of {k} changes");
                    } else if cleared_at.is_some() && cat.contains(&i) {
                    } else {
                        assert_eq!(x, y, "byte {i} of market {k}");
                    }
                }
            } else {
                assert!(da == db, "account {k} changed");
            }
        }
    }
}

/// The role: who may sign, what is checked, the event and its budget.
mod role {
    use anchor_lang::error::ErrorCode as AnchorErrorCode;
    use anchor_lang::prelude::Pubkey;
    use anchor_lang::solana_program::instruction::AccountMeta;
    use shortwin::PauseScope;
    use shortwin::errors::ShortwinError as E;
    use shortwin::events::CoveragePauseSet;
    use shortwin_litesvm_tests::errors::{assert_anchor_err, assert_err, assert_failed};
    use shortwin_litesvm_tests::ix;
    use solana_keypair::Keypair;
    use solana_signer::Signer;

    use crate::cov;
    use crate::support::common::*;

    /// A test-venue environment with one default mock market.
    fn mock() -> (Env, MockMarket) {
        setup()
    }

    /// The Emergency vault's `set_pause(Market, paused)`.
    fn set_pause(env: &mut Env, market: &Pubkey, paused: bool) {
        let ea = env.pause_authority.insecure_clone();
        ok(env.send(
            &[ix::set_pause(
                &ea.pubkey(),
                PauseScope::Market,
                paused,
                Some(*market),
            )],
            &[&ea],
        ));
    }

    fn try_open(env: &mut Env, mm: &MockMarket) -> TxResult {
        let mut b = env.borrower();
        env.open(mm, &mut b, Q).0
    }

    /// ARCH §3.1: inert while `coverage_authority == Pubkey::default()` (the `initialize_config`
    /// default until governance installs a key): whoever signs — Governance, the upgrade
    /// authority, any key, and the Emergency vault for a **set** — the instruction fails
    /// `CoverageAuthorityDisabled` before its signer check, and nothing changes. The Emergency's
    /// clear is accepted with the role unset (★OWNER-M7-3 (b); here a no-op on a clear flag,
    /// which records nothing).
    #[test]
    fn inert_while_unset() {
        let (mut env, mm) = mock();
        assert_eq!(env.config().coverage_authority, Pubkey::default());
        let before = cov::world(&env);
        let signers = [
            env.governance.insecure_clone(),
            env.upgrade_authority.insecure_clone(),
            Keypair::new(),
        ];
        for kp in &signers {
            for paused in [true, false] {
                assert_err(
                    &cov::send(&mut env, kp, &mm.market(), paused),
                    E::CoverageAuthorityDisabled,
                );
            }
        }
        let ea = env.pause_authority.insecure_clone();
        assert_err(
            &cov::send(&mut env, &ea, &mm.market(), true),
            E::CoverageAuthorityDisabled,
        );
        cov::set(&mut env, &ea, &mm.market(), false);
        assert_eq!(cov::cleared_at(&env, &mm.market()), 0, "a no-op clear");
        cov::assert_same_but_flag(&before, &cov::world(&env), &mm.market(), false, false);
        ok(try_open(&mut env, &mm));
    }

    /// Installed by a timelocked `Authorities` change, only that key sets the flag: Governance,
    /// the Emergency vault and any other key fail `Unauthorized`; only it — and the Emergency
    /// vault (★OWNER-M7-3 (b), the `emergency` module) — clears it. The key sets the flag (opens
    /// refused `CoveragePaused`), sets it again (idempotent: the keeper retries until the flag
    /// matches), and clears it (opens resume).
    #[test]
    fn only_the_installed_key_signs() {
        let mut env = Env::test_venue();
        let keeper = Keypair::new();
        cov::install_by_governance(&mut env, keeper.pubkey());
        let mm = env.mock_market(MarketSpec::default());
        let others = [
            env.governance.insecure_clone(),
            env.pause_authority.insecure_clone(),
            env.upgrade_authority.insecure_clone(),
            Keypair::new(),
        ];
        for kp in &others {
            assert_err(
                &cov::send(&mut env, kp, &mm.market(), true),
                E::Unauthorized,
            );
        }
        assert!(!cov::is_paused(&env, &mm.market()));
        cov::set(&mut env, &keeper, &mm.market(), true);
        assert_err(&try_open(&mut env, &mm), E::CoveragePaused);
        cov::set(&mut env, &keeper, &mm.market(), true);
        assert!(cov::is_paused(&env, &mm.market()));
        assert_err(&try_open(&mut env, &mm), E::CoveragePaused);
        let ea = env.pause_authority.pubkey();
        for kp in others.iter().filter(|k| k.pubkey() != ea) {
            assert_err(
                &cov::send(&mut env, kp, &mm.market(), false),
                E::Unauthorized,
            );
        }
        assert!(cov::is_paused(&env, &mm.market()));
        cov::set(&mut env, &keeper, &mm.market(), false);
        let at = cov::cleared_at(&env, &mm.market());
        assert_eq!(at, env.now(), "the clear records its second");
        // A no-op clear records nothing (the flag was already clear).
        env.warp(1);
        cov::set(&mut env, &keeper, &mm.market(), false);
        assert_eq!(cov::cleared_at(&env, &mm.market()), at);
        // M7-SEC-3: opens wait out the clear delay.
        assert_err(&try_open(&mut env, &mm), E::CoveragePaused);
        cov::wait_clear_delay(&mut env);
        env.crank(&mm);
        ok(try_open(&mut env, &mm));
    }

    /// A rotation (`Authorities` through the timelock) revokes the old key at once and keeps the
    /// flag as it is (the new key clears it — or the Emergency, ★OWNER-M7-3 (b), the `emergency`
    /// module); setting the role back to the default key makes the instruction inert again for
    /// the keys, with the flag left where it was.
    #[test]
    fn rotation_revokes_and_the_default_key_disables() {
        let mut env = Env::test_venue();
        let k1 = Keypair::new();
        cov::install_by_governance(&mut env, k1.pubkey());
        let mm = env.mock_market(MarketSpec::default());
        cov::set(&mut env, &k1, &mm.market(), true);

        let k2 = Keypair::new();
        cov::install_by_governance(&mut env, k2.pubkey());
        assert!(
            cov::is_paused(&env, &mm.market()),
            "a rotation clears nothing"
        );
        assert_err(
            &cov::send(&mut env, &k1, &mm.market(), false),
            E::Unauthorized,
        );
        env.crank(&mm);
        assert_err(&try_open(&mut env, &mm), E::CoveragePaused);
        cov::set(&mut env, &k2, &mm.market(), false);
        cov::set(&mut env, &k2, &mm.market(), true);

        cov::install_by_governance(&mut env, Pubkey::default());
        for kp in [&k1, &k2] {
            assert_err(
                &cov::send(&mut env, kp, &mm.market(), false),
                E::CoverageAuthorityDisabled,
            );
        }
        assert!(
            cov::is_paused(&env, &mm.market()),
            "disabling the role clears nothing"
        );
        env.crank(&mm);
        assert_err(&try_open(&mut env, &mm), E::CoveragePaused);
        // Re-enabled, the new key clears it.
        let k3 = Keypair::new();
        cov::install_by_governance(&mut env, k3.pubkey());
        cov::set(&mut env, &k3, &mm.market(), false);
        cov::wait_clear_delay(&mut env);
        env.crank(&mm);
        ok(try_open(&mut env, &mm));
    }

    /// The argument and account checks (I14 for this instruction): the reason must be
    /// `KeeperCoverage` (`InvalidParams`); the market must be its `["market", mint]` PDA with the
    /// stored bump (`ConstraintSeeds` for a copy at another key) and of `MARKET_VERSION`
    /// (`ConfigVersion`); another short.win account in its place fails the discriminator; the
    /// config must be the PDA (`ConstraintSeeds`) of `CONFIG_VERSION` (`ConfigVersion`); the
    /// authority must sign (`AccountNotSigner`); the event accounts are Anchor's. Nothing
    /// changes on any refusal.
    #[test]
    fn argument_and_account_checks() {
        let (mut env, mm) = mock();
        let keeper = cov::planted(&mut env);
        let k = keeper.pubkey();
        let market = mm.market();
        let before = cov::world(&env);

        for reason in [0u8, 2, 255] {
            assert_err(
                &env.send(&[cov::ix_with(&k, &market, true, reason)], &[&keeper]),
                E::InvalidParams,
            );
        }

        // A byte-for-byte copy of the market at another key (not a market the hooks can check:
        // its PDAs are the original's).
        let copy = Pubkey::new_unique();
        let acc = env.svm.get_account(&market).unwrap();
        env.svm.set_account(copy, acc.clone()).unwrap();
        env.check_invariants = false;
        assert_anchor_err(
            &env.send(&[cov::ix(&k, &copy, true)], &[&keeper]),
            AnchorErrorCode::ConstraintSeeds,
        );
        env.check_invariants = true;
        env.svm
            .set_account(copy, solana_account::Account::default())
            .unwrap();

        // Another short.win account in the market's place.
        assert_anchor_err(
            &env.send(&[cov::ix(&k, &mm.keys.market_vault, true)], &[&keeper]),
            AnchorErrorCode::AccountDiscriminatorMismatch,
        );
        assert_anchor_err(
            &env.send(&[cov::ix(&k, &pda::config(), true)], &[&keeper]),
            AnchorErrorCode::AccountDiscriminatorMismatch,
        );

        // Another market version.
        let mut bad = acc.clone();
        bad.data[cov::MARKET_VERSION_OFFSET] ^= 0x40;
        env.svm.set_account(market, bad).unwrap();
        assert_err(
            &env.send(&[cov::ix(&k, &market, true)], &[&keeper]),
            E::ConfigVersion,
        );
        env.svm.set_account(market, acc).unwrap();

        // The config: another account in its place, another version.
        let mut wrong_config = cov::ix(&k, &market, true);
        wrong_config.accounts[1] = AccountMeta::new_readonly(pda::venue_guard(), false);
        assert_failed(&env.send(&[wrong_config], &[&keeper]));
        let cfg = pda::config();
        let cfg_acc = env.svm.get_account(&cfg).unwrap();
        let mut bad = cfg_acc.clone();
        bad.data[cov::CONFIG_VERSION_OFFSET] ^= 0x40;
        env.svm.set_account(cfg, bad).unwrap();
        assert_err(
            &env.send(&[cov::ix(&k, &market, true)], &[&keeper]),
            E::ConfigVersion,
        );
        env.svm.set_account(cfg, cfg_acc).unwrap();

        // The authority's meta without the signer bit (the key named, not signing).
        let mut unsigned = cov::ix(&k, &market, true);
        unsigned.accounts[0].is_signer = false;
        assert_anchor_err(
            &env.send(&[unsigned], &[]),
            AnchorErrorCode::AccountNotSigner,
        );

        // The event accounts: a wrong event authority, a missing program.
        let mut no_event = cov::ix(&k, &market, true);
        no_event.accounts[3] = AccountMeta::new_readonly(Pubkey::new_unique(), false);
        assert_failed(&env.send(&[no_event], &[&keeper]));
        let mut short = cov::ix(&k, &market, true);
        short.accounts.truncate(4);
        assert_failed(&env.send(&[short], &[&keeper]));

        cov::assert_same_but_flag(&before, &cov::world(&env), &market, false, false);
        // The well-formed call.
        cov::set(&mut env, &keeper, &market, true);
    }

    /// The flag is independent of `set_pause`'s `paused_opens` (SPEC §11): each pause refuses
    /// opens with its own code, neither clears the other, and opens resume only when both are
    /// clear; a global `set_pause` likewise.
    #[test]
    fn independent_of_set_pause() {
        let (mut env, mm) = mock();
        let keeper = cov::planted(&mut env);
        let market = mm.market();
        set_pause(&mut env, &market, true);
        cov::set(&mut env, &keeper, &market, true);
        assert_err(&try_open(&mut env, &mm), E::OpensPaused);
        set_pause(&mut env, &market, false);
        assert!(cov::is_paused(&env, &market));
        assert_err(&try_open(&mut env, &mm), E::CoveragePaused);
        set_pause(&mut env, &market, true);
        cov::set(&mut env, &keeper, &market, false);
        assert!(
            env.market(&market)
                .has_flag(shortwin::state::MARKET_FLAG_PAUSED_OPENS)
        );
        assert_err(&try_open(&mut env, &mm), E::OpensPaused);
        set_pause(&mut env, &market, false);
        // M7-SEC-3: the coverage clear's delay still runs.
        assert_err(&try_open(&mut env, &mm), E::CoveragePaused);
        cov::wait_clear_delay(&mut env);
        env.crank(&mm);
        ok(try_open(&mut env, &mm));
        // The global pause.
        let ea = env.pause_authority.insecure_clone();
        ok(env.send(
            &[ix::set_pause(&ea.pubkey(), PauseScope::Global, true, None)],
            &[&ea],
        ));
        cov::set(&mut env, &keeper, &market, true);
        ok(env.send(
            &[ix::set_pause(&ea.pubkey(), PauseScope::Global, false, None)],
            &[&ea],
        ));
        assert_err(&try_open(&mut env, &mm), E::CoveragePaused);
        cov::set(&mut env, &keeper, &market, false);
        assert_err(&try_open(&mut env, &mm), E::CoveragePaused);
        cov::wait_clear_delay(&mut env);
        env.crank(&mm);
        ok(try_open(&mut env, &mm));
    }

    /// Pausing one market leaves every other market's opens alone.
    #[test]
    fn one_market_only() {
        let (mut env, mm) = mock();
        let other = env.mock_market(MarketSpec::default());
        let keeper = cov::planted(&mut env);
        cov::set(&mut env, &keeper, &mm.market(), true);
        assert!(!cov::is_paused(&env, &other.market()));
        assert_err(&try_open(&mut env, &mm), E::CoveragePaused);
        ok(try_open(&mut env, &other));
    }

    /// T-CU / T-TRACE for the instruction: one event CPI (2 trace entries), within the admin-flag
    /// budget of 60,000 CU (ARCH §6.1's admin row; `-- --nocapture` prints the figure).
    #[test]
    fn compute_units_and_trace() {
        let (mut env, mm) = mock();
        let keeper = cov::planted(&mut env);
        for paused in [true, true, false] {
            let meta = cov::set(&mut env, &keeper, &mm.market(), paused);
            let trace = trace_entries(&meta, 0);
            eprintln!(
                "set_coverage_pause({paused}): {} CU, {trace} trace entries",
                meta.compute_units_consumed
            );
            assert_eq!(trace, 2, "the instruction and its event CPI");
            assert!(meta.compute_units_consumed <= 60_000);
            assert_eq!(events::of::<CoveragePauseSet>(&meta).len(), 1);
        }
    }
}

/// I4's coverage clause on the whole VM.
mod moves_nothing {
    use shortwin_litesvm_tests::ix;
    use solana_signer::Signer;

    use crate::cov;
    use crate::support::common::*;

    /// On a market with two open positions, a lender with realised SOL and protocol fees in its
    /// vault: setting and clearing the flag change nothing anywhere but bit 6 of that market's
    /// `flags` — no lamports, no token balance, no other byte of any short.win, venue, token or
    /// system account (the fee payer aside); clearing it restores the VM exactly.
    #[test]
    fn the_pause_changes_one_bit_and_nothing_else() {
        let (mut env, mm) = setup();
        let keeper = cov::planted(&mut env);
        let mut b = env.borrower();
        let (_o1, _n1, _) = open_ok(&mut env, &mm, &mut b, Q);
        let mut b2 = env.borrower();
        let (_o2, _n2, _) = open_ok(&mut env, &mm, &mut b2, Q / 2);
        env.tick(&mm, 30);
        let lender = mm.lender.insecure_clone();
        ok(env.send(&[ix::claim_sol(&lender.pubkey(), &mm.market())], &[&lender]));
        env.tick(&mm, 5);
        let m = env.market(&mm.market());
        assert!(m.open_positions == 2 && env.market_vault(&mm.market()).protocol_fees > 0);

        let w0 = cov::world(&env);
        cov::set(&mut env, &keeper, &mm.market(), true);
        let w1 = cov::world(&env);
        cov::assert_same_but_flag(&w0, &w1, &mm.market(), false, true);
        // Idempotent: the same state again.
        cov::set(&mut env, &keeper, &mm.market(), true);
        cov::assert_same_but_flag(&w1, &cov::world(&env), &mm.market(), true, true);
        cov::set(&mut env, &keeper, &mm.market(), false);
        let w2 = cov::world(&env);
        // Clearing restores the VM exactly but for `coverage_cleared_at` (M7-SEC-3), which is
        // the clock's second.
        let now = env.now();
        cov::assert_same_but(&w0, &w2, &mm.market(), false, false, Some(now));
    }
}

/// ★OWNER-M7-3 (b) (owner decision of 2026-10-01; ARCH §3.1, T-COVERAGE): the Emergency vault
/// (`pause_authority`) clears — never sets — a market's `paused_coverage`, with the role
/// installed, unset or rotated; the clear delay applies after its clear too.
mod emergency {
    use anchor_lang::prelude::Pubkey;
    use shortwin::errors::ShortwinError as E;
    use shortwin_litesvm_tests::ix;
    use solana_keypair::Keypair;
    use solana_signer::Signer;

    use crate::cov;
    use crate::support::common::*;

    fn try_open(env: &mut Env, mm: &MockMarket) -> TxResult {
        let mut b = env.borrower();
        env.open(mm, &mut b, Q).0
    }

    /// Opens on `mm` refuse `CoveragePaused` in every second of the clear delay after the clear
    /// at `t` (the current second), and land after it.
    fn opens_wait_the_delay(env: &mut Env, mm: &MockMarket, t: i64) {
        assert_eq!(env.now(), t);
        for s in 0..shortwin::constants::COVERAGE_CLEAR_DELAY_S {
            assert_err(&try_open(env, mm), E::CoveragePaused);
            assert_eq!(env.now(), t + s);
            env.warp(1);
            env.crank(mm);
        }
        ok(try_open(env, mm));
    }

    /// With the role installed the Emergency's set fails `Unauthorized`, its clear of a clear flag
    /// is an idempotent no-op that records nothing, and its clear of a flag the key set lands —
    /// `CoveragePauseSet{by: pause_authority}`, `coverage_cleared_at` at its second, opens refused
    /// for `COVERAGE_CLEAR_DELAY_S` — after the same reason check as the key's (`InvalidParams`).
    /// Governance still cannot sign either way. After a rotation (the old key revoked) and with
    /// the role set back to the default key (`CoverageAuthorityDisabled` for every key's set and
    /// for the Emergency's), the Emergency still clears the flag the old key left set.
    #[test]
    fn the_emergency_clears_with_the_role_installed_rotated_or_unset() {
        let mut env = Env::test_venue();
        let k1 = Keypair::new();
        cov::install_by_governance(&mut env, k1.pubkey());
        let mm = env.mock_market(MarketSpec::default());
        let market = mm.market();
        let ea = env.pause_authority.insecure_clone();
        let gov = env.governance.insecure_clone();

        // Installed: the Emergency never sets.
        assert_err(&cov::send(&mut env, &ea, &market, true), E::Unauthorized);
        assert!(!cov::is_paused(&env, &market));
        // A clear of a clear flag: accepted (the event), nothing recorded.
        cov::set(&mut env, &ea, &market, false);
        assert_eq!(cov::cleared_at(&env, &market), 0);
        // The key sets; Governance can neither clear nor set.
        cov::set(&mut env, &k1, &market, true);
        for paused in [true, false] {
            assert_err(&cov::send(&mut env, &gov, &market, paused), E::Unauthorized);
        }
        // The reason check applies to the Emergency's clear too.
        for reason in [0u8, 2, 255] {
            assert_err(
                &env.send(
                    &[cov::ix_with(&ea.pubkey(), &market, false, reason)],
                    &[&ea],
                ),
                E::InvalidParams,
            );
        }
        assert!(cov::is_paused(&env, &market));
        env.warp(1);
        env.crank(&mm);
        cov::set(&mut env, &ea, &market, false);
        let t = env.now();
        assert_eq!(cov::cleared_at(&env, &market), t);
        // Idempotent: again in the same second, nothing more recorded.
        cov::set(&mut env, &ea, &market, false);
        assert_eq!(cov::cleared_at(&env, &market), t);
        opens_wait_the_delay(&mut env, &mm, t);

        // Rotated: the old key's flag stays set; the old key is revoked; the Emergency clears.
        cov::set(&mut env, &k1, &market, true);
        let k2 = Keypair::new();
        cov::install_by_governance(&mut env, k2.pubkey());
        assert!(cov::is_paused(&env, &market), "a rotation clears nothing");
        assert_err(&cov::send(&mut env, &k1, &market, false), E::Unauthorized);
        assert_err(&cov::send(&mut env, &ea, &market, true), E::Unauthorized);
        env.crank(&mm);
        cov::set(&mut env, &ea, &market, false);
        let t = env.now();
        assert_eq!(cov::cleared_at(&env, &market), t);
        opens_wait_the_delay(&mut env, &mm, t);

        // Unset: every set refused `CoverageAuthorityDisabled` (the Emergency's included), every
        // key's clear too — but the Emergency's clear lands.
        cov::set(&mut env, &k2, &market, true);
        cov::install_by_governance(&mut env, Pubkey::default());
        for kp in [&k1, &k2, &gov] {
            for paused in [true, false] {
                assert_err(
                    &cov::send(&mut env, kp, &market, paused),
                    E::CoverageAuthorityDisabled,
                );
            }
        }
        assert_err(
            &cov::send(&mut env, &ea, &market, true),
            E::CoverageAuthorityDisabled,
        );
        assert!(cov::is_paused(&env, &market));
        env.crank(&mm);
        assert_err(&try_open(&mut env, &mm), E::CoveragePaused);
        cov::set(&mut env, &ea, &market, false);
        let t = env.now();
        assert_eq!(cov::cleared_at(&env, &market), t);
        opens_wait_the_delay(&mut env, &mm, t);
    }

    /// I4's coverage clause on the whole VM for the Emergency's clear, on a market with an open
    /// position: the clear changes bit 6 of that market's `flags` and its `coverage_cleared_at`
    /// (to the clock's second) and nothing else anywhere — no lamports, no token balance, no
    /// other byte of any short.win, venue, token or system account (the fee payer aside); a
    /// refused Emergency set changes nothing.
    #[test]
    fn the_emergency_clear_changes_the_flag_and_its_clock_only() {
        let (mut env, mm) = setup();
        let keeper = cov::planted(&mut env);
        let mut b = env.borrower();
        let (_o, _n, _) = open_ok(&mut env, &mm, &mut b, Q);
        env.tick(&mm, 5);
        cov::set(&mut env, &keeper, &mm.market(), true);
        env.warp(2);
        let w0 = cov::world(&env);
        let ea = env.pause_authority.insecure_clone();
        let meta = cov::set(&mut env, &ea, &mm.market(), false);
        assert!(meta.compute_units_consumed <= 60_000);
        let now = env.now();
        cov::assert_same_but(&w0, &cov::world(&env), &mm.market(), true, false, Some(now));
        let w1 = cov::world(&env);
        assert_err(
            &cov::send(&mut env, &ea, &mm.market(), true),
            E::Unauthorized,
        );
        cov::assert_same_but_flag(&w1, &cov::world(&env), &mm.market(), false, false);
        println!(
            "OWNER-M7-3 (b): the Emergency's clear: {} CU, 2 trace entries",
            meta.compute_units_consumed
        );
    }

    /// The hijacked-coverage-key drill (★OWNER-M7-3 (b); ARCH §7.1 duty 9's runbook): a stolen
    /// `coverage_authority` key pauses every market. Governance proposes the rotation at once
    /// (it lands `timelock_s`, 48 h, later); meanwhile each of the thief's sets is undone by one
    /// Emergency clear per market (≈ 8,000 CU), opens resume `COVERAGE_CLEAR_DELAY_S` after each
    /// clear, and exits land under every pause. Once the rotation applies the stolen key is
    /// refused (`Unauthorized`) and the new key works.
    #[test]
    fn a_hijacked_coverage_key_is_neutralised_by_the_emergency() {
        let mut env = Env::test_venue();
        let stolen = Keypair::new();
        cov::install_by_governance(&mut env, stolen.pubkey());
        let markets = [
            env.mock_market(MarketSpec::default()),
            env.mock_market(MarketSpec::default()),
        ];
        for m in &markets {
            env.crank(m);
        }
        let ea = env.pause_authority.insecure_clone();
        let gov = env.governance.insecure_clone();
        // The page: Governance proposes the rotation at once.
        let fresh = Keypair::new();
        let change = cov::authorities(&env, fresh.pubkey());
        ok(env.send(
            &[ix::propose_config(&gov.pubkey(), change.clone())],
            &[&gov],
        ));
        let applies_at = env.now() + i64::from(env.config().timelock_s);
        let mut positions: Vec<(usize, Borrower, u64)> = Vec::new();
        let (mut clears, mut max_cu) = (0u32, 0u64);
        for _round in 0..3 {
            // The thief pauses every market ...
            for m in &markets {
                cov::set(&mut env, &stolen, &m.market(), true);
            }
            for m in &markets {
                assert_err(&try_open(&mut env, m), E::CoveragePaused);
            }
            // ... the exits land under its pause ...
            for (i, b, nonce) in positions.drain(..) {
                close_ok(&mut env, &markets[i], &b, nonce);
            }
            // ... and the Emergency undoes each set, one clear per market.
            for m in &markets {
                let meta = cov::set(&mut env, &ea, &m.market(), false);
                max_cu = max_cu.max(meta.compute_units_consumed);
                clears += 1;
                assert_eq!(cov::cleared_at(&env, &m.market()), env.now());
            }
            // Opens resume after the clear delay.
            cov::wait_clear_delay(&mut env);
            for (i, m) in markets.iter().enumerate() {
                env.crank(m);
                let mut b = env.borrower();
                let (_o, nonce, _) = open_ok(&mut env, m, &mut b, Q);
                positions.push((i, b, nonce));
            }
            env.warp(600);
            for m in &markets {
                env.crank(m);
            }
            assert!(env.now() < applies_at, "the drill runs inside the timelock");
        }
        for (i, b, nonce) in positions.drain(..) {
            close_ok(&mut env, &markets[i], &b, nonce);
        }
        // The rotation applies: the stolen key is revoked, the flag stays clear, the new key
        // works.
        env.warp(applies_at - env.now());
        ok(env.send(&[ix::apply_config_for(&change)], &[]));
        assert_eq!(env.config().coverage_authority, fresh.pubkey());
        for m in &markets {
            env.crank(m);
            for paused in [true, false] {
                assert_err(
                    &cov::send(&mut env, &stolen, &m.market(), paused),
                    E::Unauthorized,
                );
            }
            assert!(!cov::is_paused(&env, &m.market()));
            ok(try_open(&mut env, m));
        }
        cov::set(&mut env, &fresh, &markets[0].market(), true);
        cov::set(&mut env, &fresh, &markets[0].market(), false);
        assert!(max_cu <= 60_000);
        println!(
            "OWNER-M7-3 (b) drill: {clears} Emergency clears undid the stolen key's sets on 2 \
             markets (max {max_cu} CU each); exits landed under every pause; the key refused \
             after the rotation"
        );
    }
}

/// Pause-opens-only on the mock, differentially.
mod exits {
    use anchor_lang::AnchorSerialize;
    use shortwin::errors::ShortwinError as E;
    use shortwin::events::{
        ClaimSol, Deposit, Expire, LenderSynced, Liquidate, MarginAdded, PriceCranked, SolSettle,
        TraderSwept, Withdraw,
    };
    use shortwin_litesvm_tests::engine::{
        add_margin_ix, expire_ix, liquidate_ix, sweep_trader_ix_for,
    };
    use shortwin_litesvm_tests::{ix, tokens};
    use solana_signer::Signer;

    use crate::cov;
    use crate::support::*;

    /// Borsh bytes of an event (the comparison key).
    fn bytes<T: AnchorSerialize>(v: &T) -> Vec<u8> {
        let mut out = Vec::new();
        v.serialize(&mut out).expect("serialize");
        out
    }

    /// Runs `script` from the scenario's current state twice — as it is, and after the
    /// coverage key paused the market — and asserts that both runs return the same events and
    /// leave the same VM except for the flag (still set at the end of the paused run: no exit
    /// clears it), and that the paused market refuses an open (`CoveragePaused`) at the end.
    /// Cleared, the open is no longer refused for coverage; `reopen`: it lands (a scenario that
    /// ends drained, route-broken or past its tier's caps refuses it for that reason).
    fn differential(sc: &mut Sc, reopen: bool, script: impl Fn(&mut Sc) -> Vec<Vec<u8>>) {
        let keeper = cov::planted(&mut sc.env);
        let market = sc.mm.market();
        let mut b = sc.env.borrower();
        let saved = sc.env.svm.clone();
        let nonce0 = sc.b.next_nonce;

        let plain = script(sc);
        let w_plain = cov::world(&sc.env);

        sc.env.svm = saved;
        sc.b.next_nonce = nonce0;
        cov::set(&mut sc.env, &keeper, &market, true);
        let paused = script(sc);
        let w_paused = cov::world(&sc.env);

        assert!(!plain.is_empty());
        assert_eq!(plain, paused, "the same events with and without the pause");
        cov::assert_same_but_flag(&w_plain, &w_paused, &market, false, true);
        let (res, _) = sc.env.open(&sc.mm, &mut b, sc.q / 10);
        assert_err(&res, E::CoveragePaused);
        cov::set(&mut sc.env, &keeper, &market, false);
        // M7-SEC-3: still refused for the clear delay, then not for coverage.
        let (res, _) = sc.env.open(&sc.mm, &mut b, sc.q / 10);
        assert_err(&res, E::CoveragePaused);
        cov::wait_clear_delay(&mut sc.env);
        sc.crank();
        let (res, _) = sc.env.open(&sc.mm, &mut b, sc.q / 10);
        assert_ne!(
            custom_code(&res),
            Some(u32::from(E::CoveragePaused)),
            "cleared"
        );
        if reopen {
            ok(res);
        }
    }

    /// Mints `amount` to the market's lender and deposits it.
    fn lender_deposit(sc: &mut Sc, amount: u64) -> Deposit {
        let payer = sc.env.payer.insecure_clone();
        tokens::mint_to(
            &mut sc.env.svm,
            &payer,
            &sc.mm.keys.mint,
            &sc.mm.lender_account,
            &sc.mm.pool.mint_authority,
            amount,
            sc.mm.spec.program,
        );
        let lender = sc.mm.lender.insecure_clone();
        events::one(&ok(sc
            .env
            .send(&[ix::deposit(&sc.mm.lender_keys(), amount, 0)], &[&lender])))
    }

    /// The borrower's own paths and the lender pool: `crank_price`, `add_margin`, `deposit`,
    /// `withdraw`, `claim_sol`, `sync_lender`, `sweep_trader`, `close_short`.
    #[test]
    fn borrower_lender_and_crank_paths() {
        let mut sc = Sc::default_market();
        sc.env.tick(&sc.mm, 5);
        differential(&mut sc, true, |sc| {
            let mut out = Vec::new();
            sc.env.warp(1);
            let meta = ok(sc.env.send(&[crank_price_ix(&sc.mm)], &[]));
            out.push(bytes(&events::one::<PriceCranked>(&meta)));
            let bk = sc.b.kp.insecure_clone();
            let meta = ok(sc.env.send(
                &[add_margin_ix(&sc.mm, &sc.b.key(), sc.nonce, 50_000_000)],
                &[&bk],
            ));
            out.push(bytes(&events::one::<MarginAdded>(&meta)));
            sc.env.tick(&sc.mm, 30);
            out.push(bytes(&lender_deposit(sc, 1_000_000_000_000)));
            let lender = sc.mm.lender.insecure_clone();
            let meta = ok(sc.env.send(
                &[ix::claim_sol(&lender.pubkey(), &sc.mm.market())],
                &[&lender],
            ));
            out.push(bytes(&events::one::<ClaimSol>(&meta)));
            let shares = sc
                .env
                .lender(&sc.mm.market(), &lender.pubkey())
                .expect("lender")
                .shares;
            let meta = ok(sc.env.send(
                &[ix::withdraw(&sc.mm.lender_keys(), shares / 100, 0)],
                &[&lender],
            ));
            out.push(bytes(&events::one::<Withdraw>(&meta)));
            let meta = ok(sc.env.send(
                &[ix::sync_lender(&lender.pubkey(), &sc.mm.market(), 8)],
                &[],
            ));
            out.push(bytes(&events::one::<LenderSynced>(&meta)));
            let meta = ok(sc
                .env
                .send(&[sweep_trader_ix_for(&sc.env, &sc.mm.market())], &[]));
            out.extend(events::of::<TraderSwept>(&meta).iter().map(bytes));
            sc.env.warp(1);
            sc.crank();
            let (close, _) = close_ok(&mut sc.env, &sc.mm, &sc.b, sc.nonce);
            out.push(bytes(&close));
            out
        });
    }

    /// `liquidate`: the position made CR-liquidatable by an exogenous move to 0.8 of its S1
    /// boundary, the EMA following for 20 s (below S1: a normal liquidation).
    #[test]
    fn liquidate() {
        let mut sc = Sc::default_market();
        sc.env.tick(&sc.mm, 5);
        let p0 = sc.spot();
        let kb = sc.s1_boundary();
        sc.move_to(scale(p0, kb * 0.8));
        for _ in 0..20 {
            let out = sc.second();
            assert!(out.settled.is_none() && out.armed.is_none());
        }
        differential(&mut sc, true, |sc| {
            sc.env.warp(1);
            sc.crank();
            let k = sc.keeper.insecure_clone();
            let ix = liquidate_ix(&sc.mm, &k.pubkey(), &sc.b.key(), sc.nonce);
            let meta = ok(sc.env.send(&[ix], &[&k]));
            vec![bytes(&events::one::<Liquidate>(&meta))]
        });
    }

    /// `expire`: a healthy position at its deadline.
    #[test]
    fn expire() {
        let mut sc = Sc::default_market();
        sc.env.tick(&sc.mm, 5);
        let deadline = sc.pos().deadline;
        while sc.env.now() + 86_400 < deadline - 10 {
            sc.env.warp(86_400);
            sc.crank();
        }
        let gap = deadline - 10 - sc.env.now();
        sc.env.warp(gap);
        sc.env.tick(&sc.mm, 9);
        differential(&mut sc, true, |sc| {
            sc.env.warp(1);
            sc.crank();
            assert_eq!(sc.env.now(), sc.pos().deadline);
            let k = sc.keeper.insecure_clone();
            let ix = expire_ix(&sc.mm, &k.pubkey(), &sc.b.key(), sc.nonce);
            let meta = ok(sc.env.send(&[ix], &[&k]));
            vec![bytes(&events::one::<Expire>(&meta))]
        });
    }

    /// Keeper seconds until `sol_settle` settles; the settlement's bytes.
    fn settle_run(sc: &mut Sc, max_s: u32, condition: u8) -> Vec<Vec<u8>> {
        let (_s, ev) = sc.run_until_settled(max_s).expect("settled");
        assert_eq!(ev.condition, condition);
        vec![bytes::<SolSettle>(&ev)]
    }

    /// `sol_settle` S1: a genuine gap beyond the S1 boundary, then keeper seconds (the clock
    /// arms and persists under the pause as without it).
    #[test]
    fn sol_settle_s1() {
        let mut sc = Sc::default_market();
        sc.env.tick(&sc.mm, 5);
        let kb = sc.s1_boundary();
        sc.move_by(kb * 1.05);
        differential(&mut sc, true, |sc| settle_run(sc, 120, 1));
    }

    /// `sol_settle` S2: an LP drain leaves the buyback's slippage above `σ_max`.
    #[test]
    fn sol_settle_s2() {
        let mut sc = Sc::default_market();
        sc.env.tick(&sc.mm, 5);
        sc.drain(9_850);
        sc.env.tick(&sc.mm, 90);
        sc.move_by(1.8);
        differential(&mut sc, false, |sc| settle_run(sc, 120, 2));
    }

    /// `sol_settle` S3: the Emergency route-broken flag on the market, the chain run, the
    /// position at its deadline.
    #[test]
    fn sol_settle_s3() {
        let mut sc = Sc::default_market();
        sc.env.tick(&sc.mm, 5);
        sc.warp_to_deadline(-700);
        sc.set_route_broken(true);
        differential(&mut sc, false, |sc| {
            for _ in 0..23 {
                sc.env.warp(30);
                sc.crank();
                assert!(sc.settle_ok().settled.is_none());
            }
            sc.warp_to_deadline(0);
            settle_run(sc, 10, 3)
        });
    }
}

/// The real venues.
mod venues {
    use litesvm::types::TransactionMetadata;
    use shortwin::errors::ShortwinError as E;
    use shortwin::events::{PoolProposed, PoolSwitched};
    use shortwin_litesvm_tests::engine::{Borrower, VenueMarket, crank_price_ix};
    use shortwin_litesvm_tests::errors::{assert_err, ok};
    use shortwin_litesvm_tests::{Env, events};

    use crate::cov;

    /// On `mm` with `b`'s position `nonce` open: pause, then an open is refused `CoveragePaused`;
    /// a crank and the position's close run exactly as without the pause (the same events, the
    /// same VM but the flag); cleared, an open of `q` lands.
    fn pause_opens_only(env: &mut Env, mm: &impl VenueMarket, b: &Borrower, nonce: u64, q: u64) {
        let keeper = cov::planted(env);
        let market = mm.market();
        let mut b2 = env.borrower();
        env.warp(1);
        let saved = env.svm.clone();
        let run = |env: &mut Env| -> Vec<Vec<u8>> {
            let crank: TransactionMetadata = ok(env.send(&[crank_price_ix(mm)], &[]));
            let close = ok(env.close(mm, b, nonce));
            let mut raw = events::raw(&crank);
            raw.extend(events::raw(&close));
            raw
        };
        let plain = run(env);
        let w_plain = cov::world(env);
        env.svm = saved;
        cov::set(env, &keeper, &market, true);
        assert_err(&env.open(mm, &mut b2, q).0, E::CoveragePaused);
        let paused = run(env);
        let w_paused = cov::world(env);
        assert!(!plain.is_empty());
        assert_eq!(plain, paused, "crank and close identical under the pause");
        cov::assert_same_but_flag(&w_plain, &w_paused, &market, false, true);
        assert_err(&env.open(mm, &mut b2, q).0, E::CoveragePaused);
        cov::set(env, &keeper, &market, false);
        // M7-SEC-3: the clear delay, then the open lands.
        assert_err(&env.open(mm, &mut b2, q).0, E::CoveragePaused);
        cov::wait_clear_delay(env);
        ok(env.send(&[crank_price_ix(mm)], &[]));
        ok(env.open(mm, &mut b2, q).0);
    }

    /// The pump curve (Tier D): the legacy 115-byte curve coin.
    #[test]
    fn the_pump_curve() {
        use crate::pumpc::*;
        let (mut env, pm, q) = setup(PumpCoin::Legacy115);
        let mut b = env.borrower();
        let (_o, nonce, _) = open_ok(&mut env, &pm, &mut b, q);
        pause_opens_only(&mut env, &pm, &b, nonce, q);
    }

    /// PumpSwap: the mature canonical pool (boost spent, third-party LP).
    #[test]
    fn pumpswap() {
        use crate::pammc::*;
        let (mut env, pm, q) = setup(PammCoin::Mature);
        let mut b = env.borrower();
        let (_o, nonce, _) = open_ok(&mut env, &pm, &mut b, q);
        pause_opens_only(&mut env, &pm, &b, nonce, q);
    }

    /// Raydium AMM v4 (Fartcoin) and CPMM (USELESS).
    #[test]
    fn raydium_amm_v4_and_cpmm() {
        use crate::common::*;
        for fixture in [FART, USELESS] {
            let (mut env, rm, q) = setup(fixture);
            let mut b = env.borrower();
            let (_o, nonce, _) = open_ok(&mut env, &rm, &mut b, q);
            pause_opens_only(&mut env, &rm, &b, nonce, q);
        }
    }

    /// Raydium pool selection under the pause: `propose_pool`'s zero-position adoption onto a
    /// deeper planted CPMM pool, and the fast path — the incumbent halted with two positions
    /// open, the chain run, `propose_pool` opening the collection window and
    /// `execute_pool_switch` adopting — produce the same events and the same VM (the flag aside)
    /// as without it. The pause survives the adoption (the new pool's opens stay refused).
    #[test]
    fn raydium_pool_selection() {
        use crate::common::*;
        use crate::ps::*;
        // Zero-position adoption.
        let (mut env, rm, _q) = setup(FART);
        crank_every(&mut env, &rm, 1, 5);
        let lam = lambda_of(&env, &rm.pool);
        let deep = plant(&mut env, &rm.pool, Plant::Cpmm, true, (10, 1), lam);
        let keeper = cov::planted(&mut env);
        env.warp(1);
        let saved = env.svm.clone();
        let plain = events::raw(&ok(env.propose(Some(&rm.pool), &deep)));
        let w_plain = cov::world(&env);
        env.svm = saved;
        cov::set(&mut env, &keeper, &rm.market(), true);
        let paused = events::raw(&ok(env.propose(Some(&rm.pool), &deep)));
        assert_eq!(plain, paused);
        cov::assert_same_but_flag(&w_plain, &cov::world(&env), &rm.market(), false, true);
        assert!(
            cov::is_paused(&env, &rm.market()),
            "the adoption keeps the flag"
        );

        // The fast path with open positions.
        let (mut env, rm, q) = setup(FART);
        crank_every(&mut env, &rm, 1, 5);
        let mut b = env.borrower();
        let (_, _n1, _) = open_ok(&mut env, &rm, &mut b, q);
        let (_, _n2, _) = open_ok(&mut env, &rm, &mut b, q / 3);
        crank_every(&mut env, &rm, 1, 3);
        halt(&mut env, &rm.pool);
        let lam = lambda_of(&env, &rm.pool);
        let best = plant(&mut env, &rm.pool, Plant::Cpmm, true, (60, 100), lam);
        let keeper = cov::planted(&mut env);
        let saved = env.svm.clone();
        let script = |env: &mut Env| -> Vec<Vec<u8>> {
            env.warp(1);
            env.crank(&rm);
            crank_every(env, &rm, 30, 4);
            let meta = ok(env.propose(Some(&rm.pool), &best));
            assert_eq!(
                events::of::<PoolProposed>(&meta).len(),
                1,
                "the window opens"
            );
            let mut raw = events::raw(&meta);
            let window = i64::from(env.config().params.pool_fast_window_s);
            env.warp(window / 2);
            env.crank(&rm);
            env.warp(window - window / 2);
            let meta = ok(env.execute_switch(&rm.pool, &best));
            assert_eq!(events::of::<PoolSwitched>(&meta).len(), 1, "adopted");
            raw.extend(events::raw(&meta));
            raw
        };
        let plain = script(&mut env);
        let w_plain = cov::world(&env);
        env.svm = saved;
        cov::set(&mut env, &keeper, &rm.market(), true);
        let paused = script(&mut env);
        assert_eq!(plain, paused, "the fast path is the same under the pause");
        cov::assert_same_but_flag(&w_plain, &cov::world(&env), &rm.market(), false, true);
    }
}
