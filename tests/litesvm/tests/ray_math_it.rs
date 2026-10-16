//! T-RAY-MATH, execution half (ARCH §8.2, §9 M3; R-RAY §3.3–§3.4, §9.6 item 8; U11, U12):
//! direct user swaps on the **real** Raydium programs dumped from mainnet (AMM v4
//! `675kPX9…`, deploy slot 445,763,204; CPMM `CPMMoo8…` as redeployed at 452,376,460 (M10 WP-2;
//! the 445,763,504 build is T-LATCH's "before"); `tests/fixtures/`, loaded by
//! `shortwin_litesvm_tests::fork`) execute exactly what `shortwin-math`'s `quote::ray_v4` /
//! `quote::cpmm` compute on the `shortwin-venue-state` readers' reserves:
//!
//! - both directions (WSOL in / token in), exact in (tag 16, `swap_base_input`) and exact out
//!   (tag 17, `swap_base_output`), over amount grids from 1 raw unit to beyond the reserves;
//! - every fee variant the fixtures hold: AMM v4 25/10,000 on both side orders (WSOL coin and
//!   WSOL pc, with a non-zero `need_take_pnl`); CPMM config 0 without a creator fee, the creator
//!   fee on WSOL as token 0 (on the input of buys, on the **output** of sells — the exact-out
//!   sale is U11's formula), on WSOL as token 1 (config 10), on both tokens, and a Token-2022
//!   pool; then every one of the 21 live `AmmConfig`s × creator fee off / on each side
//!   (including the fee on the token side, which no mainnet pool has), admin-style AMM v4
//!   fee and `need_take_pnl` changes, and CPMM zero-fee configs (M3-SEC-1: `t + c_eff = 0`
//!   refuses the direction whose creator fee is on its input with `ZeroTradingTokens`, which
//!   has no quote, and the reader's route predicate is "a buy executes");
//! - the bound passed **exactly** (`min_out = quote`, `max_in = quote`) succeeds and one unit
//!   tighter fails with `ExceededSlippage`; the user's and the vaults' deltas equal the quote;
//!   the reserves after the swap (reader) equal the quote's post-trade reserves; the venue's own
//!   record of the reserves it priced on (`ray_log`, `SwapEvent`) equals the reader's; an
//!   undefined quote (`None`) is a failed swap and a failed swap has no quote;
//! - seeded random trade sequences on the evolving state.
//!
//! Also here, because they validate the fork and the readers against the ELFs: the fixture
//! integrity (ProgramData rebuilt to the mainnet sha256, R-RAY §1.3's ELF hashes and the
//! redeploy's), the reader
//! constants against the program's, every PDA the venues derive, the route predicates of both
//! readers against the ELFs (status values and bits, open times, counters: the venue half of
//! T-RAY-DISABLE), and the byte-level admin builders against the admins' real instructions.
//!
//! Every transaction goes through `Env::send` (or `fork::admin_send`, which runs the same
//! hooks). `RAY_MATH_SEQ` sets the random sequence length per pool (default 40) and
//! `RAY_MATH_SEED` its seed.

// `TxResult` is LiteSVM's own result type (see the harness's lib.rs).
#![allow(clippy::result_large_err)]

use anchor_lang::prelude::Pubkey;
use litesvm::LiteSVM;
use shortwin::constants::{
    GUARD_RAYDIUM_CPMM, GUARD_RAYDIUM_V4, RAYDIUM_CPMM_AUTHORITY, RAYDIUM_CPMM_PROGRAM_ID,
    RAYDIUM_V4_AUTHORITY, RAYDIUM_V4_PROGRAM_ID, T_CLAMP, TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID,
    VENUE_GUARD_SLOTS_2026_09_27, VENUE_PROGRAM_DATA, VENUE_PROGRAM_IDS, WSOL_MINT,
};
use shortwin_litesvm_tests::errors::custom_code;
use shortwin_litesvm_tests::fork::{
    self, AnyPool, CpmmFixture, RaydiumProgram, Swap, Trader, V4Fixture,
};
use shortwin_litesvm_tests::{Env, TxResult};
use shortwin_math::fees::Direction;
use shortwin_venue_state::{program_data, raydium_cpmm, raydium_v4};

const DIRS: [Direction; 2] = [Direction::Buy, Direction::Sell];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    ExactIn,
    ExactOut,
}

const MODES: [Mode; 2] = [Mode::ExactIn, Mode::ExactOut];

fn env_with(pool: &str) -> (Env, AnyPool) {
    let mut env = Env::initialized();
    env.load_raydium_programs();
    let p = match pool {
        "v4" => AnyPool::V4(env.load_ray_v4(V4Fixture::Fartcoin)),
        "v4rev" => AnyPool::V4(env.load_ray_v4(V4Fixture::FartcoinReversed)),
        other => {
            let which = CpmmFixture::ALL
                .into_iter()
                .find(|f| f.prefix() == other)
                .unwrap_or_else(|| panic!("no pool {other}"));
            AnyPool::Cpmm(env.load_cpmm(which))
        }
    };
    env.set_fork_clock();
    (env, p)
}

fn all_pools() -> Vec<&'static str> {
    let mut v = vec!["v4", "v4rev"];
    v.extend(CpmmFixture::ALL.iter().map(|f| f.prefix()));
    v
}

fn new_trader(env: &mut Env, pool: &AnyPool) -> Trader {
    let (mint, program) = pool.token();
    fork::trader(env, &mint, program, 0, 0)
}

/// Tops the trader's source for `dir` up to `need`.
fn fund_source(env: &mut Env, t: &Trader, dir: Direction, need: u64) {
    let (src, _) = t.accounts(dir);
    let have = fork::vault_amount(&env.svm, &src);
    if have >= need {
        return;
    }
    match dir {
        Direction::Buy => fork::wrap(env, &src, need - have),
        Direction::Sell => fork::set_token_amount(&mut env.svm, &src, need),
    }
}

fn slippage_code(pool: &AnyPool) -> u32 {
    match pool {
        AnyPool::V4(_) => fork::V4_ERR_EXCEEDED_SLIPPAGE,
        AnyPool::Cpmm(_) => fork::CPMM_ERR_EXCEEDED_SLIPPAGE,
    }
}

fn describe(res: &TxResult) -> String {
    match res {
        Ok(m) => format!("succeeded\n{}", m.pretty_logs()),
        Err(f) => format!("failed with {:?}\n{}", f.err, f.meta.pretty_logs()),
    }
}

/// The vaults of a pool, input then output, for `dir`.
fn io_vaults(pool: &AnyPool, dir: Direction) -> (Pubkey, Pubkey) {
    match pool {
        AnyPool::V4(p) => p.io_vaults(dir),
        AnyPool::Cpmm(p) => {
            let (iv, ov, ..) = p.io(dir);
            (iv, ov)
        }
    }
}

/// What a quote says a swap moves: `(amount in, amount out, input reserve after, output
/// reserve after)`.
fn quoted(
    svm: &LiteSVM,
    pool: &AnyPool,
    dir: Direction,
    mode: Mode,
    amount: u64,
) -> Option<(u64, u64, u128, u128)> {
    match mode {
        Mode::ExactIn => pool
            .quote_exact_in(svm, dir, amount)
            .map(|q| (amount, q.proceeds, q.token_after, q.sol_after)),
        Mode::ExactOut => pool
            .quote_exact_out(svm, dir, amount)
            .map(|q| (q.cost, amount, q.sol_after, q.token_after)),
    }
}

/// One swap of `amount` checked against its quote end to end (see the module doc). Returns
/// whether it executed.
#[track_caller]
fn exact_swap(
    env: &mut Env,
    pool: &AnyPool,
    t: &Trader,
    dir: Direction,
    mode: Mode,
    amount: u64,
) -> bool {
    let ctx = format!("{pool:?} {dir:?} {mode:?} amount {amount}");
    let pre_res = pool.io_reserves(&env.svm, dir);
    let Some((x_in, y_out)) = pre_res else {
        panic!("{ctx}: the reader finds no reserves on a pool the test expects to price");
    };
    let Some((a_in, a_out, in_after, out_after)) = quoted(&env.svm, pool, dir, mode, amount) else {
        // No quote: the venue must refuse whatever bound the user passes.
        fund_source(env, t, dir, amount.max(1));
        let s = match mode {
            Mode::ExactIn => Swap::ExactIn(amount, 0),
            Mode::ExactOut => Swap::ExactOut(u64::MAX, amount),
        };
        let saved = env.svm.clone();
        let res = fork::swap(env, pool, t, dir, s);
        assert!(
            res.is_err(),
            "{ctx}: no quote, but the venue {}",
            describe(&res)
        );
        env.svm = saved;
        return false;
    };
    fund_source(env, t, dir, a_in);
    let (src, dst) = t.accounts(dir);
    let (iv, ov) = io_vaults(pool, dir);
    let bal = |svm: &LiteSVM, k: &Pubkey| fork::vault_amount(svm, k);
    let before = [src, dst, iv, ov].map(|k| bal(&env.svm, &k));

    // One unit tighter than the quote: ExceededSlippage, nothing moves.
    let tight = match mode {
        Mode::ExactIn => Swap::ExactIn(amount, a_out + 1),
        Mode::ExactOut => Swap::ExactOut(a_in - 1, amount),
    };
    let res = fork::swap(env, pool, t, dir, tight);
    assert_eq!(
        custom_code(&res),
        Some(slippage_code(pool)),
        "{ctx}: one unit tighter than the quote must be ExceededSlippage; {}",
        describe(&res)
    );

    // The exact bound.
    let exact = match mode {
        Mode::ExactIn => Swap::ExactIn(amount, a_out),
        Mode::ExactOut => Swap::ExactOut(a_in, amount),
    };
    let res = fork::swap(env, pool, t, dir, exact);
    let meta = match res {
        Ok(m) => m,
        Err(f) => panic!(
            "{ctx}: the exact bound failed with {:?}\n{}",
            f.err,
            f.meta.pretty_logs()
        ),
    };
    let after = [src, dst, iv, ov].map(|k| bal(&env.svm, &k));
    assert_eq!(before[0] - after[0], a_in, "{ctx}: user paid");
    assert_eq!(after[1] - before[1], a_out, "{ctx}: user received");
    assert_eq!(after[2] - before[2], a_in, "{ctx}: input vault");
    assert_eq!(before[3] - after[3], a_out, "{ctx}: output vault");
    let (x2, y2) = pool.io_reserves(&env.svm, dir).expect("reserves after");
    assert_eq!(
        (u128::from(x2), u128::from(y2)),
        (in_after, out_after),
        "{ctx}: reserves after the swap"
    );

    // The venue's own record of the reserves it priced on equals the reader's.
    match pool {
        AnyPool::V4(p) => {
            let logs = fork::ray_logs(&meta.logs);
            assert_eq!(logs.len(), 1, "{ctx}: one ray_log");
            let l = logs[0];
            let (sol, tok) = match dir {
                Direction::Buy => (x_in, y_out),
                Direction::Sell => (y_out, x_in),
            };
            let (coin, pc) = if p.sol_is_coin {
                (sol, tok)
            } else {
                (tok, sol)
            };
            assert_eq!(
                (l.pool_coin, l.pool_pc),
                (coin, pc),
                "{ctx}: ray_log reserves"
            );
            let coin_in = (dir == Direction::Buy) == p.sol_is_coin;
            assert_eq!(l.direction, if coin_in { 2 } else { 1 }, "{ctx}: direction");
            match mode {
                Mode::ExactIn => {
                    assert_eq!((l.log_type, l.result), (3, a_out), "{ctx}: out_amount")
                }
                Mode::ExactOut => assert_eq!((l.log_type, l.result), (4, a_in), "{ctx}: deduct_in"),
            }
        }
        AnyPool::Cpmm(p) => {
            let ev = fork::cpmm_swap_events(&meta.logs);
            assert_eq!(ev.len(), 1, "{ctx}: one SwapEvent");
            let e = ev[0];
            assert_eq!(e.pool_id, p.pool);
            assert_eq!(
                (e.input_vault_before, e.output_vault_before),
                (x_in, y_out),
                "{ctx}: SwapEvent reserves"
            );
            assert_eq!(
                (e.input_amount, e.output_amount),
                (a_in, a_out),
                "{ctx}: SwapEvent amounts"
            );
            assert_eq!(e.base_input, mode == Mode::ExactIn);
            assert_eq!((e.input_transfer_fee, e.output_transfer_fee), (0, 0));
            let on_input = p
                .state(&env.svm)
                .creator_fee_on_input(p.input_is_token0(dir))
                .unwrap();
            assert_eq!(e.creator_fee_on_input, on_input, "{ctx}: creator fee side");
        }
    }
    // Venue CU as measured on mainnet (R-RAY §4.3): AMM v4 v2 ≈ 15.2–15.5k, CPMM ≈ 24–34k.
    let cu = meta.compute_units_consumed;
    match pool {
        AnyPool::V4(_) => assert!((14_000..17_000).contains(&cu), "{ctx}: {cu} CU"),
        AnyPool::Cpmm(_) => assert!((22_000..40_000).contains(&cu), "{ctx}: {cu} CU"),
    }
    true
}

/// Amounts from 1 raw unit to past `reserve`.
fn grid(reserve: u64, mode: Mode) -> Vec<u64> {
    let r = reserve;
    let mut v = vec![1, 2, 3, 7, 100, 10_000, 1_000_000];
    v.extend([r / 1_000_000, r / 10_000, r / 100, r / 10, r / 2]);
    match mode {
        Mode::ExactIn => v.extend([r, r.saturating_mul(3)]),
        Mode::ExactOut => v.extend([r - r / 100, r - 1, r, r + 1]),
    }
    v.retain(|&a| a > 0);
    v.sort_unstable();
    v.dedup();
    v
}

/// Every grid amount, both directions, both modes, each on the pool's state as loaded (the VM
/// is restored after each swap). Returns (executed, refused).
fn run_grid(env: &mut Env, pool: &AnyPool, t: &Trader) -> (usize, usize) {
    let (mut ok, mut refused) = (0, 0);
    for dir in DIRS {
        for mode in MODES {
            let (x, y) = pool.io_reserves(&env.svm, dir).expect("reserves");
            let reserve = match mode {
                Mode::ExactIn => x,
                Mode::ExactOut => y,
            };
            for amount in grid(reserve, mode) {
                let saved = env.svm.clone();
                if exact_swap(env, pool, t, dir, mode, amount) {
                    ok += 1;
                } else {
                    refused += 1;
                }
                env.svm = saved;
            }
        }
    }
    (ok, refused)
}

// ---- the fork itself ---------------------------------------------------------------------------

/// The dumped programs and their ProgramData are what mainnet holds: the rebuilt ProgramData
/// (header ‖ ELF ‖ zeros) hashes to the fetched account (checked on load), the ELF with its
/// trailing zeros trimmed hashes to R-RAY §1.3's value (the redeploy: the manifest's pin, which is
/// OtterSec's on-chain hash), the header carries the deploy slot and the Squads v4 upgrade
/// authority, at the latch's addresses. AMM v4 and the CPMM build the fork loads carry
/// [`fork::FIXTURE_GUARD_SLOTS`]; the CPMM build `VENUE_GUARD_SLOTS_2026_09_27` names (445,763,504)
/// is kept for T-LATCH.
#[test]
fn fixtures_are_mainnet() {
    use fork::CpmmDeploy;
    let v4 = RaydiumProgram::AmmV4;
    let cases = [
        (
            "AMM v4",
            GUARD_RAYDIUM_V4,
            v4.program_data(),
            v4.program(),
            v4.elf(),
            "raydium_amm_v4",
            445_763_204,
            "d4cee0cd4f78f69d05f5d645fc6d120dff830f231162d35cf2b2dae1df7b2c9f",
        ),
        (
            "CPMM 2026-09",
            GUARD_RAYDIUM_CPMM,
            CpmmDeploy::Sep2026.program_data(),
            CpmmDeploy::Sep2026.program(),
            CpmmDeploy::Sep2026.elf(),
            "raydium_cpmm",
            445_763_504,
            "bdaff73e7ed9fc75f091d732987d36476d7378d32c47495fe9eef0a930dbf8cc",
        ),
        (
            "CPMM 2026-10",
            GUARD_RAYDIUM_CPMM,
            CpmmDeploy::Oct2026.program_data(),
            CpmmDeploy::Oct2026.program(),
            CpmmDeploy::Oct2026.elf(),
            "raydium_cpmm_2026_10",
            452_376_460,
            "93cad2458ac72435c94b8ac89faa686987f4e29d2f0e862d546fbc5aa814a0d2",
        ),
    ];
    let authority = Pubkey::from_str_const("FytDrVzDybM1TwFQPGb8qaxZR7dBCzNeqT3vtQsceZQK");
    for (label, guard, pd, prog, elf, file, slot, trimmed) in cases {
        let g = usize::from(guard);
        assert_eq!(pd.address, VENUE_PROGRAM_DATA[g], "{label}");
        let h = program_data::read(&pd.account.owner.to_bytes(), &pd.account.data).unwrap();
        assert_eq!(h.slot, slot, "{label}");
        assert_eq!(h.upgrade_authority, Some(authority.to_bytes()), "{label}");
        assert_eq!(&elf[..4], b"\x7fELF");
        // SBPF v0 (`e_flags` 0): the loader runs mainnet's own binary, not a rebuild.
        assert_eq!(elf[48..52], [0, 0, 0, 0], "{label}");
        let file = format!("programs/{file}.so");
        assert_eq!(std::fs::read(fork::fixtures_dir().join(file)).unwrap(), elf);
        assert!(
            pd.account.data[program_data::HEADER_LEN + elf.len()..]
                .iter()
                .all(|&b| b == 0)
        );
        let end = elf.iter().rposition(|&b| b != 0).unwrap() + 1;
        assert_eq!(fork::sha256_hex(&elf[..end]), trimmed, "{label}");
        assert_eq!(prog.address, VENUE_PROGRAM_IDS[g], "{label}");
        assert!(prog.account.executable);
        assert_eq!(prog.account.data[4..36], pd.address.to_bytes());
    }
    // The fork loads the redeploy; the program constant still names the 2026-09 build.
    for p in RaydiumProgram::ALL {
        let g = usize::from(match p {
            RaydiumProgram::AmmV4 => GUARD_RAYDIUM_V4,
            RaydiumProgram::Cpmm => GUARD_RAYDIUM_CPMM,
        });
        let h = program_data::read(
            &p.program_data().account.owner.to_bytes(),
            &p.program_data().account.data,
        )
        .unwrap();
        assert_eq!(h.slot, fork::FIXTURE_GUARD_SLOTS[g], "{p:?}");
        assert_eq!(p.program().address, p.id());
    }
    assert_eq!(RaydiumProgram::Cpmm.elf(), CpmmDeploy::CURRENT.elf());
    assert_eq!(CpmmDeploy::CURRENT, CpmmDeploy::Oct2026);
    assert_eq!(
        CpmmDeploy::Sep2026.deploy_slot(),
        VENUE_GUARD_SLOTS_2026_09_27[usize::from(GUARD_RAYDIUM_CPMM)]
    );
    assert_eq!(
        CpmmDeploy::Oct2026.deploy_slot(),
        fork::CPMM_DEPLOY_SLOT_2026_10
    );
    assert_eq!(
        fork::fixture_at(&VENUE_PROGRAM_DATA[usize::from(GUARD_RAYDIUM_CPMM)])
            .unwrap()
            .name,
        "raydium_cpmm_2026_10_program_data",
        "an address in both CPMM groups resolves to the redeploy"
    );
    // Every state fixture is one snapshot.
    let s = fork::state_snapshot();
    for f in fork::all_fixtures().filter(|f| f.group == "raydium_state") {
        assert_eq!(f.slot, s.slot, "{}", f.name);
    }
    assert!(s.block_time > 1_790_000_000);
}

/// The readers' byte constants equal the program's compile-time keys.
#[test]
fn reader_constants_match_the_program() {
    assert_eq!(raydium_v4::PROGRAM, RAYDIUM_V4_PROGRAM_ID.to_bytes());
    assert_eq!(raydium_v4::AUTHORITY, RAYDIUM_V4_AUTHORITY.to_bytes());
    assert_eq!(
        raydium_v4::AUTHORITY_NONCE,
        u64::from(shortwin::constants::RAYDIUM_V4_AUTHORITY_NONCE)
    );
    assert_eq!(raydium_cpmm::PROGRAM, RAYDIUM_CPMM_PROGRAM_ID.to_bytes());
    assert_eq!(raydium_cpmm::AUTHORITY, RAYDIUM_CPMM_AUTHORITY.to_bytes());
    assert_eq!(raydium_cpmm::T_CLAMP, T_CLAMP);
    assert_eq!(
        shortwin_venue_state::mint::NATIVE_MINT,
        WSOL_MINT.to_bytes()
    );
    assert_eq!(
        raydium_v4::POOL_ADMIN,
        Pubkey::from_str_const("GThUX1Atko4tqhN2NaiTazWSeFWMuiUvfFnyJyUghFMJ").to_bytes()
    );
}

/// The readers' fee resolution in ppm equals `shortwin-math`'s one-way fee (`fees::ray_v4` /
/// `fees::cpmm` `oneway_ppm`, ARCH §4.2) on every fixture config and creator-fee setting and on
/// AMM v4 fractions around the edges, and both say "no fee" (no quote) on the same inputs.
#[test]
fn reader_fees_match_shortwin_math() {
    use shortwin_math::fees::{self, CpmmConfig, CpmmPoolFees};
    let v4 = raydium_v4::read(
        &raydium_v4::PROGRAM,
        &fork::fixture("ray_v4_fartcoin_pool").account.data,
    )
    .unwrap();
    for (num, den) in [
        (25, 10_000),
        (0, 1),
        (1, 3),
        (2, 3),
        (7, 1_000_000_007),
        (u64::MAX - 1, u64::MAX),
        (1, u64::MAX),
        (3, 3),
        (4, 3),
        (0, 0),
    ] {
        let info = raydium_v4::AmmInfo {
            swap_fee_numerator: num,
            swap_fee_denominator: den,
            ..v4
        };
        let math = fees::ray_v4(num, den).ok().map(|f| f.oneway_ppm());
        assert_eq!(info.fee_ppm(), math, "{num}/{den}");
    }
    let pool = raydium_cpmm::read_pool(
        &raydium_cpmm::PROGRAM,
        &fork::fixture("cpmm_useless_pool").account.data,
    )
    .unwrap();
    for index in 0..fork::CPMM_CONFIGS {
        let f = fork::fixture(&fork::cpmm_config_name(index));
        let c = raydium_cpmm::read_config(&f.account.owner.to_bytes(), &f.account.data).unwrap();
        for enable in [false, true] {
            for on in 0..=3u8 {
                let p = raydium_cpmm::PoolState {
                    enable_creator_fee: enable,
                    creator_fee_on: on,
                    ..pool
                };
                for dir in DIRS {
                    let m = fees::cpmm(
                        &CpmmConfig {
                            trade_fee_rate: c.trade_fee_rate,
                            protocol_fee_rate: c.protocol_fee_rate,
                            fund_fee_rate: c.fund_fee_rate,
                            creator_fee_rate: c.creator_fee_rate,
                        },
                        &CpmmPoolFees {
                            enable_creator_fee: enable,
                            creator_fee_on: on,
                            sol_is_token0: true,
                        },
                        dir,
                    );
                    let input_is_token0 = dir == Direction::Buy;
                    match m {
                        Ok(m) => {
                            assert_eq!(
                                p.fee_ppm(&c),
                                Some(m.oneway_ppm()),
                                "{index} {enable} {on}"
                            );
                            assert_eq!(
                                p.creator_fee_on_input(input_is_token0),
                                Some(m.creator_on_input)
                            );
                            assert_eq!(p.creator_fee_rate(&c), m.creator);
                        }
                        Err(_) => assert_eq!(p.creator_fee_on_input(input_is_token0), None),
                    }
                }
            }
        }
    }
}

/// Every address the venues derive (R-RAY §1.1, §2.4, §2.5), re-derived on the dumped keys:
/// the two authorities, every config `["amm_config", index BE]`, and for every CPMM pool its
/// vaults `["pool_vault", pool, mint]`, LP mint and observation.
#[test]
fn venue_pdas() {
    let v4 = Pubkey::create_program_address(&[b"amm authority", &[254]], &RAYDIUM_V4_PROGRAM_ID)
        .unwrap();
    assert_eq!(v4, RAYDIUM_V4_AUTHORITY);
    let (cp, bump) =
        Pubkey::find_program_address(&[b"vault_and_lp_mint_auth_seed"], &RAYDIUM_CPMM_PROGRAM_ID);
    assert_eq!((cp, bump), (RAYDIUM_CPMM_AUTHORITY, 253));
    for index in 0..fork::CPMM_CONFIGS {
        let (k, _) = Pubkey::find_program_address(
            &[b"amm_config", &index.to_be_bytes()],
            &RAYDIUM_CPMM_PROGRAM_ID,
        );
        assert_eq!(k, fork::cpmm_config(index), "config {index}");
        let f = fork::fixture(&fork::cpmm_config_name(index));
        let c = raydium_cpmm::read_config(&f.account.owner.to_bytes(), &f.account.data).unwrap();
        assert_eq!(c.index, index);
    }
    let mut env = Env::new();
    for which in CpmmFixture::ALL {
        let p = env.load_cpmm(which);
        let s = p.state(&env.svm);
        let pda = |seeds: &[&[u8]]| Pubkey::find_program_address(seeds, &RAYDIUM_CPMM_PROGRAM_ID).0;
        for (vault, mint) in [
            (p.token_0_vault, p.token_0_mint),
            (p.token_1_vault, p.token_1_mint),
        ] {
            assert_eq!(
                vault,
                pda(&[b"pool_vault", p.pool.as_ref(), mint.as_ref()]),
                "{which:?}"
            );
        }
        assert_eq!(
            p.lp_mint,
            pda(&[b"pool_lp_mint", p.pool.as_ref()]),
            "{which:?}"
        );
        assert_eq!(
            p.observation,
            pda(&[b"observation", p.pool.as_ref()]),
            "{which:?}"
        );
        assert_eq!(s.auth_bump, 253);
        // WSOL sits under SPL Token; the token side under its mint's program.
        let wsol_program = if p.sol_is_token0 {
            p.token_0_program
        } else {
            p.token_1_program
        };
        assert_eq!(wsol_program, TOKEN_PROGRAM_ID);
        let mint_owner = env.svm.get_account(&p.token_mint()).unwrap().owner;
        assert_eq!(p.token_program().id(), mint_owner);
    }
    let t22 = env.load_cpmm(CpmmFixture::Token2022);
    assert_eq!(t22.token_program().id(), TOKEN_2022_PROGRAM_ID);
    for which in V4Fixture::ALL {
        let p = env.load_ray_v4(which);
        assert_eq!(p.info(&env.svm).nonce, 254);
    }
}

// ---- T-RAY-MATH: the dumped state ----------------------------------------------------------------

/// Every pool as dumped: the full grid, both directions, both modes.
#[test]
fn every_pool_as_dumped() {
    for name in all_pools() {
        let (mut env, pool) = env_with(name);
        let t = new_trader(&mut env, &pool);
        let (ok, refused) = run_grid(&mut env, &pool, &t);
        println!("{name}: {ok} swaps executed exactly, {refused} refused with no quote");
        // The grids reach past the reserves, so some amounts have no quote; most execute.
        assert!(
            ok >= 40 && refused >= 2,
            "{name}: {ok} executed, {refused} refused"
        );
    }
}

/// R-RAY §3.5's worked examples replayed on the fork's reserves: the reader's reserves of the
/// Fartcoin pool feed `quote::ray_v4` exactly as the venue's `ray_log` does (a 100,000,000,000
/// raw FART exact-out buy, the (F) SIM's size).
#[test]
fn fartcoin_sim_size_buy() {
    let (mut env, pool) = env_with("v4");
    let t = new_trader(&mut env, &pool);
    assert!(exact_swap(
        &mut env,
        &pool,
        &t,
        Direction::Buy,
        Mode::ExactOut,
        100_000_000_000
    ));
}

// ---- T-RAY-MATH: fee variants ---------------------------------------------------------------------

/// AMM v4: the swap fee the admin can set per pool (`SetParams` param 2; any `num < den`),
/// and fees outside that (no quote, and the venue refuses), on both side orders.
#[test]
fn v4_fee_variants() {
    let fees: [(u64, u64); 9] = [
        (25, 10_000),
        (0, 10_000),
        (1, 3),
        (30, 10_000),
        (100, 10_000),
        (9_999, 10_000),
        (7, 1_000_000_007),
        (u64::MAX / 3, u64::MAX / 2),
        (10_000, 10_000),
    ];
    for name in ["v4", "v4rev"] {
        let (mut env, pool) = env_with(name);
        let AnyPool::V4(p) = pool else { unreachable!() };
        let t = new_trader(&mut env, &pool);
        let base = env.svm.clone();
        for (num, den) in fees {
            env.svm = base.clone();
            fork::set_v4_swap_fee(&mut env.svm, &p, num, den);
            assert_eq!(p.info(&env.svm).fee_ppm().is_some(), num < den);
            for dir in DIRS {
                for mode in MODES {
                    let (x, y) = pool.io_reserves(&env.svm, dir).unwrap();
                    let r = if mode == Mode::ExactIn { x } else { y };
                    for amount in [1_000_000, r / 1_000, r / 10] {
                        let saved = env.svm.clone();
                        let executed = exact_swap(&mut env, &pool, &t, dir, mode, amount);
                        assert!(
                            executed || num >= den || amount == 1_000_000,
                            "{name} {num}/{den} {dir:?} {mode:?} {amount}"
                        );
                        if num >= den {
                            assert!(!executed);
                        }
                        env.svm = saved;
                    }
                }
            }
        }
    }
}

/// AMM v4: `need_take_pnl` (the protocol's unrealised PnL) is excluded from the reserves the
/// venue prices on, exactly as the reader excludes it.
#[test]
fn v4_need_take_pnl() {
    let (mut env, pool) = env_with("v4");
    let AnyPool::V4(p) = pool else { unreachable!() };
    let t = new_trader(&mut env, &pool);
    let (cv, pv) = p.vault_amounts(&env.svm);
    for (coin, pc) in [
        (cv / 3, pv / 5),
        (cv - 1_000_000_000, 0),
        (0, pv - 1_000_000_000),
    ] {
        let saved = env.svm.clone();
        fork::set_v4_need_take_pnl(&mut env.svm, &p, coin, pc);
        assert_eq!(p.reserves(&env.svm), Some((cv - coin, pv - pc)));
        let (ok, _) = run_grid(&mut env, &pool, &t);
        assert!(ok > 20);
        env.svm = saved;
    }
}

/// CPMM: all 21 live configs × creator fee off / on each side (0 = both tokens, 1 = token 0,
/// 2 = token 1) on a pool with WSOL as token 0 and one with WSOL as token 1, so every
/// combination of fee side, direction and mode runs: fee on the input of buys and of sells,
/// and on the output of either (the exact-out buy with the creator fee on the token side is
/// U11's case, which no mainnet pool has).
#[test]
fn cpmm_every_config_and_creator_side() {
    for name in ["cpmm_useless", "cpmm_wsol_token1"] {
        let (mut env, pool) = env_with(name);
        let AnyPool::Cpmm(p) = pool else {
            unreachable!()
        };
        let t = new_trader(&mut env, &pool);
        let base = env.svm.clone();
        let mut runs = 0;
        for index in 0..fork::CPMM_CONFIGS {
            for (enable, on) in [(false, 0u8), (true, 0), (true, 1), (true, 2)] {
                env.svm = base.clone();
                fork::set_cpmm_pool_config(&mut env.svm, &p, &fork::cpmm_config(index));
                fork::set_cpmm_creator_fee(&mut env.svm, &p, enable, on);
                for dir in DIRS {
                    let fees = p.fees(&env.svm, dir).unwrap();
                    let token_side = on != 0 && ((on == 1) != p.sol_is_token0);
                    let c_on_input = on == 0 || ((dir == Direction::Sell) == token_side);
                    assert_eq!(fees.creator_on_input, c_on_input, "{index} {on} {dir:?}");
                    for mode in MODES {
                        let (x, y) = pool.io_reserves(&env.svm, dir).unwrap();
                        let r = if mode == Mode::ExactIn { x } else { y };
                        for amount in [r / 100_000, r / 50] {
                            let saved = env.svm.clone();
                            assert!(exact_swap(&mut env, &pool, &t, dir, mode, amount));
                            env.svm = saved;
                            runs += 1;
                        }
                    }
                }
            }
        }
        assert_eq!(runs, 21 * 4 * 2 * 2 * 2);
    }
}

/// CPMM: admin-style changes of the config rates (`update_amm_config`: trade and creator fee
/// up to `t + c < 10⁶`, the protocol / fund split) and large uncollected counters (excluded
/// from the reserves the venue prices on).
#[test]
fn cpmm_rates_and_counters() {
    let (mut env, pool) = env_with("cpmm_creator_fee");
    let AnyPool::Cpmm(p) = pool else {
        unreachable!()
    };
    let t = new_trader(&mut env, &pool);
    let cfg = p.config_key(&env.svm);
    let base = env.svm.clone();
    let rates: [(u64, u64, u64, u64); 6] = [
        (2_500, 120_000, 40_000, 500),
        (0, 0, 0, 1),
        (1, 0, 0, 0),
        (50_000, 500_000, 500_000, 49_999),
        (400_000, 1_000_000, 0, 599_999),
        (999_998, 0, 1_000_000, 1),
    ];
    for (trade, protocol, fund, creator) in rates {
        env.svm = base.clone();
        fork::set_cpmm_config_rates(&mut env.svm, &cfg, trade, protocol, fund, creator);
        let (ok, _) = run_grid(&mut env, &pool, &t);
        assert!(ok > 20, "{trade}/{protocol}/{fund}/{creator}: {ok}");
    }
    let (v0, v1) = p.vault_amounts(&base);
    for (c0, c1) in [
        ((0, 0, 0), (0, 0, 0)),
        ((v0 / 4, v0 / 8, v0 / 16), (v1 / 3, 1, v1 / 3)),
    ] {
        env.svm = base.clone();
        fork::set_cpmm_counters(&mut env.svm, &p, c0, c1);
        let (r0, r1) = p.reserves(&env.svm).unwrap();
        assert_eq!((r0, r1), (v0 - c0.0 - c0.1 - c0.2, v1 - c1.0 - c1.1 - c1.2));
        let (ok, _) = run_grid(&mut env, &pool, &t);
        assert!(ok > 20);
    }
}

/// CPMM zero-fee configs (M3-SEC-1): `update_amm_config` accepts a trade fee of 0, which on a
/// pool without an enabled creator fee (every permissionless pool: `creator_fee_on = 0`, fee
/// off), or with a creator rate of 0, leaves `t + c_eff = 0`. On both WSOL sides and every
/// creator-fee side, both directions and modes: the direction whose creator fee is on its input
/// cannot split the zero fee — the venue refuses every amount with `ZeroTradingTokens` and the
/// quote is undefined — and the other executes exactly as quoted, fee-free; the reader's
/// `venue_executable` is exactly "a buy (WSOL in) executes". With `c_eff > 0` both directions
/// execute at `t = 0`.
#[test]
fn cpmm_zero_fee_configs() {
    for name in [
        "cpmm_useless",
        "cpmm_wsol_token1",
        "cpmm_creator_fee",
        "cpmm_creator_both",
    ] {
        let (mut env, pool) = env_with(name);
        let AnyPool::Cpmm(p) = pool else {
            unreachable!()
        };
        let t = new_trader(&mut env, &pool);
        let cfg = p.config_key(&env.svm);
        let now = env.now();
        let base = env.svm.clone();
        let (mut executed, mut refused) = (0, 0);
        for (enable, on) in [(false, 0u8), (true, 0), (true, 1), (true, 2)] {
            for creator in [0u64, 500] {
                env.svm = base.clone();
                fork::set_cpmm_config_rates(&mut env.svm, &cfg, 0, 120_000, 40_000, creator);
                fork::set_cpmm_creator_fee(&mut env.svm, &p, enable, on);
                let c_eff = if enable { creator } else { 0 };
                let broken =
                    |dir: Direction| c_eff == 0 && (on == 0 || (on == 1) == p.input_is_token0(dir));
                let case = format!("{name} enable {enable} on {on} c {creator}");
                assert_eq!(
                    p.venue_executable(&env.svm, now),
                    !broken(Direction::Buy),
                    "{case}: executable = the buy splits its fee"
                );
                for dir in DIRS {
                    for mode in MODES {
                        let (x, y) = pool.io_reserves(&env.svm, dir).unwrap();
                        let r = if mode == Mode::ExactIn { x } else { y };
                        for amount in [r / 100_000, r / 50] {
                            let saved = env.svm.clone();
                            let ok = exact_swap(&mut env, &pool, &t, dir, mode, amount);
                            assert_eq!(ok, !broken(dir), "{case} {dir:?} {mode:?} {amount}");
                            if ok {
                                executed += 1;
                            } else {
                                refused += 1;
                                fund_source(&mut env, &t, dir, amount.max(x / 10));
                                let s = match mode {
                                    Mode::ExactIn => Swap::ExactIn(amount, 0),
                                    Mode::ExactOut => Swap::ExactOut(u64::MAX, amount),
                                };
                                let res = fork::swap(&mut env, &pool, &t, dir, s);
                                assert_eq!(
                                    custom_code(&res),
                                    Some(fork::CPMM_ERR_ZERO_TRADING_TOKENS),
                                    "{case} {dir:?} {mode:?}: {}",
                                    describe(&res)
                                );
                            }
                            env.svm = saved;
                        }
                    }
                }
            }
        }
        // Per pool, 8 settings × 2 directions × 2 modes × 2 amounts: of the five settings with
        // `c_eff = 0`, fee model 0 (three of them) refuses both directions and models 1 and 2
        // one direction each.
        assert_eq!((executed, refused), (32, 32), "{name}");
    }
}

// ---- T-RAY-MATH: random sequences -----------------------------------------------------------------

struct Rng(u64);

impl Rng {
    /// A generator from any seed (splitmix64 of it, never the xorshift's fixed point 0).
    fn new(seed: u64) -> Self {
        let mut z = seed.wrapping_add(0x9e37_79b9_7f4a_7c15);
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        Self((z ^ (z >> 31)).max(1))
    }

    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    /// Log-uniform in `[1, max]`.
    fn amount(&mut self, max: u64) -> u64 {
        let bits = 64 - max.max(1).leading_zeros();
        let b = 1 + self.next() % u64::from(bits);
        let v = self.next() & (u64::MAX >> (64 - b));
        v.clamp(1, max.max(1))
    }
}

/// Seeded random trades on every pool, each on the state the previous one left: the reserves
/// the venue records and the post-trade reserves chain exactly.
#[test]
fn random_sequences() {
    let steps: usize = std::env::var("RAY_MATH_SEQ")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(40);
    let seed: u64 = std::env::var("RAY_MATH_SEED")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    for (i, name) in all_pools().into_iter().enumerate() {
        let (mut env, pool) = env_with(name);
        let t = new_trader(&mut env, &pool);
        let mut rng = Rng::new(seed ^ (i as u64 + 1));
        let mut executed = 0;
        for _ in 0..steps {
            let dir = DIRS[(rng.next() % 2) as usize];
            let mode = MODES[(rng.next() % 2) as usize];
            let (x, y) = pool.io_reserves(&env.svm, dir).unwrap();
            let amount = match mode {
                Mode::ExactIn => rng.amount(x / 4),
                Mode::ExactOut => rng.amount(y / 4),
            };
            if exact_swap(&mut env, &pool, &t, dir, mode, amount) {
                executed += 1;
            }
            env.warp(1 + (rng.next() % 30) as i64);
        }
        assert!(executed * 4 >= steps * 3, "{name}: {executed} of {steps}");
    }
}

// ---- readers vs the ELFs: the route predicates ----------------------------------------------------

/// A swap that should work on a healthy pool, as a probe of the venue's gate.
fn probe(env: &mut Env, pool: &AnyPool, t: &Trader) -> TxResult {
    let (x, _) = pool
        .io_reserves(&env.svm, Direction::Buy)
        .unwrap_or((1_000_000_000, 0));
    let amount = (x / 100_000).max(1_000_000);
    fund_source(env, t, Direction::Buy, amount);
    fork::swap(env, pool, t, Direction::Buy, Swap::ExactIn(amount, 0))
}

/// AMM v4: `venue_executable` equals whether the ELF swaps, for every status value (1, 6, 7
/// swap; 0, 2–5 refuse with `InvalidStatus`; above 7 the venue panics), `WaitingTrade` around
/// its open time (the first swap writes status 6), and the vaults around `need_take_pnl`.
#[test]
fn v4_route_predicate_matches_the_elf() {
    let (mut env, pool) = env_with("v4");
    let AnyPool::V4(p) = pool else { unreachable!() };
    let t = new_trader(&mut env, &pool);
    let now = env.now();
    let base = env.svm.clone();
    for status in (0..=9u64).chain([u64::MAX]) {
        env.svm = base.clone();
        fork::set_v4_status(&mut env.svm, &p, status);
        let pred = p.venue_executable(&env.svm, now);
        let res = probe(&mut env, &pool, &t);
        assert_eq!(pred, res.is_ok(), "status {status}: {}", describe(&res));
        if (0..=5).contains(&status) && status != 1 {
            assert_eq!(
                custom_code(&res),
                Some(fork::V4_ERR_INVALID_STATUS),
                "status {status}"
            );
        }
    }
    for (open, ok) in [(now + 1, false), (now, true), (now - 1, true), (0, true)] {
        env.svm = base.clone();
        fork::set_v4_status(&mut env.svm, &p, raydium_v4::STATUS_WAITING_TRADE);
        fork::set_v4_open_time(&mut env.svm, &p, u64::try_from(open).unwrap());
        assert_eq!(p.venue_executable(&env.svm, now), ok);
        let res = probe(&mut env, &pool, &t);
        assert_eq!(res.is_ok(), ok, "open {open}: {}", describe(&res));
        let after = p.info(&env.svm).status;
        assert_eq!(
            after,
            if ok {
                raydium_v4::STATUS_SWAP_ONLY
            } else {
                raydium_v4::STATUS_WAITING_TRADE
            }
        );
    }
    let (cv, pv) = p.vault_amounts(&base);
    for (coin, pc) in [(cv, 0), (cv + 1, 0), (0, pv), (0, pv + 1)] {
        env.svm = base.clone();
        fork::set_v4_need_take_pnl(&mut env.svm, &p, coin, pc);
        let pred = p.venue_executable(&env.svm, now);
        let res = probe(&mut env, &pool, &t);
        assert!(!pred);
        assert!(
            res.is_err(),
            "need_take_pnl ({coin}, {pc}): {}",
            describe(&res)
        );
        if coin > cv || pc > pv {
            assert_eq!(custom_code(&res), Some(fork::V4_ERR_CHECKED_SUB_OVERFLOW));
        }
    }
}

/// CPMM: `venue_executable` equals whether the ELF swaps, for every status byte (bit 2 refuses
/// with `NotApproved`), the open time, counters around the vaults and the fee model byte.
#[test]
fn cpmm_route_predicate_matches_the_elf() {
    let (mut env, pool) = env_with("cpmm_creator_fee");
    let AnyPool::Cpmm(p) = pool else {
        unreachable!()
    };
    let t = new_trader(&mut env, &pool);
    let now = env.now();
    let base = env.svm.clone();
    for status in 0..=u8::MAX {
        env.svm = base.clone();
        fork::set_cpmm_status(&mut env.svm, &p, status);
        let pred = p.venue_executable(&env.svm, now);
        let res = probe(&mut env, &pool, &t);
        assert_eq!(pred, res.is_ok(), "status {status}: {}", describe(&res));
        if !pred {
            assert_eq!(custom_code(&res), Some(fork::CPMM_ERR_NOT_APPROVED));
        }
    }
    for (open, ok) in [(now + 1, false), (now, true), (now - 1, true)] {
        env.svm = base.clone();
        fork::set_cpmm_open_time(&mut env.svm, &p, u64::try_from(open).unwrap());
        assert_eq!(p.venue_executable(&env.svm, now), ok);
        let res = probe(&mut env, &pool, &t);
        assert_eq!(res.is_ok(), ok, "open {open}: {}", describe(&res));
    }
    let (v0, v1) = p.vault_amounts(&base);
    for (c0, c1) in [(v0 + 1, 0), (0, v1 + 1), (v0, 0), (0, v1)] {
        env.svm = base.clone();
        fork::set_cpmm_counters(&mut env.svm, &p, (c0, 0, 0), (c1, 0, 0));
        assert!(!p.venue_executable(&env.svm, now));
        let res = probe(&mut env, &pool, &t);
        assert!(res.is_err(), "counters ({c0}, {c1}): {}", describe(&res));
        if c0 > v0 || c1 > v1 {
            assert_eq!(custom_code(&res), Some(fork::CPMM_ERR_INSUFFICIENT_VAULT));
        }
    }
    for on in [0u8, 1, 2, 3, 255] {
        env.svm = base.clone();
        fork::set_cpmm_creator_fee(&mut env.svm, &p, true, on);
        let pred = p.venue_executable(&env.svm, now);
        let res = probe(&mut env, &pool, &t);
        assert_eq!(pred, res.is_ok(), "creator_fee_on {on}: {}", describe(&res));
        if on > 2 {
            assert_eq!(custom_code(&res), Some(fork::CPMM_ERR_INVALID_FEE_MODEL));
        }
    }
    // M3-SEC-1: zero config fees (`update_amm_config` accepts `t = 0`). The buy (WSOL = token 0
    // in) cannot split a zero fee on its input (`ZeroTradingTokens`); a fee on its output, or
    // any non-zero `t + c_eff`, swaps.
    let cfg = p.config_key(&base);
    for (trade, creator) in [(0u64, 0u64), (0, 1), (1, 0)] {
        for (enable, on) in [(false, 0u8), (true, 0), (true, 1), (true, 2)] {
            env.svm = base.clone();
            fork::set_cpmm_config_rates(&mut env.svm, &cfg, trade, 120_000, 40_000, creator);
            fork::set_cpmm_creator_fee(&mut env.svm, &p, enable, on);
            let pred = p.venue_executable(&env.svm, now);
            let res = probe(&mut env, &pool, &t);
            let case = format!("t {trade} c {creator} enable {enable} on {on}");
            assert_eq!(pred, res.is_ok(), "{case}: {}", describe(&res));
            let c_eff = if enable { creator } else { 0 };
            assert_eq!(pred, trade + c_eff > 0 || on == 2, "{case}");
            if !pred {
                assert_eq!(
                    custom_code(&res),
                    Some(fork::CPMM_ERR_ZERO_TRADING_TOKENS),
                    "{case}"
                );
            }
        }
    }
}

// ---- byte-level admin builders vs the admins' real instructions -----------------------------------

/// The pool bytes the byte-level builders leave equal what the admin's own instruction leaves
/// on the dumped program (`SetParams` status / open time / fees; `update_pool_status` and
/// `update_amm_config` up to `recent_epoch`, which the real instruction also stamps), and the
/// real instructions refuse what their bounds refuse.
#[test]
fn admin_builders_match_the_real_instructions() {
    let (mut env, pool) = env_with("v4");
    let AnyPool::V4(p) = pool else { unreachable!() };
    let base = env.svm.clone();
    // Both programs stamp the clock's epoch into the pool (`recent_epoch`); the builders do not.
    let bytes_masked = |svm: &LiteSVM, k: &Pubkey, epoch: std::ops::Range<usize>| {
        let mut d = svm.get_account(k).unwrap().data;
        d[epoch].fill(0);
        d
    };
    let v4_epoch = 736..744;
    let bytes = |svm: &LiteSVM, k: &Pubkey| bytes_masked(svm, k, v4_epoch.clone());
    for status in [1u64, 2, 3, 4, 5, 6, 7] {
        env.svm = base.clone();
        shortwin_litesvm_tests::errors::ok(fork::raydium_admin_send(
            &mut env,
            fork::v4_set_params_ix(&p, 0, status),
        ));
        let real = bytes(&env.svm, &p.pool);
        let mut svm = base.clone();
        fork::set_v4_status(&mut svm, &p, status);
        assert_eq!(real, bytes(&svm, &p.pool), "status {status}");
    }
    for bad in [0u64, 8, u64::MAX] {
        env.svm = base.clone();
        let res = fork::raydium_admin_send(&mut env, fork::v4_set_params_ix(&p, 0, bad));
        assert_eq!(
            custom_code(&res),
            Some(fork::V4_ERR_INVALID_INPUT),
            "status {bad}"
        );
    }
    env.svm = base.clone();
    shortwin_litesvm_tests::errors::ok(fork::raydium_admin_send(
        &mut env,
        fork::v4_set_params_ix(&p, 3, 1_800_000_000),
    ));
    let mut svm = base.clone();
    fork::set_v4_open_time(&mut svm, &p, 1_800_000_000);
    assert_eq!(bytes(&env.svm, &p.pool), bytes(&svm, &p.pool));
    let mut fees = fork::V4Fees::of(&base, &p);
    fees.swap_fee = (9_999, 10_000);
    env.svm = base.clone();
    shortwin_litesvm_tests::errors::ok(fork::raydium_admin_send(
        &mut env,
        fork::v4_set_fees_ix(&p, &fees),
    ));
    let mut svm = base.clone();
    fork::set_v4_swap_fee(&mut svm, &p, 9_999, 10_000);
    assert_eq!(bytes(&env.svm, &p.pool), bytes(&svm, &p.pool));
    fees.swap_fee = (10_000, 10_000);
    env.svm = base.clone();
    let res = fork::raydium_admin_send(&mut env, fork::v4_set_fees_ix(&p, &fees));
    assert_eq!(custom_code(&res), Some(fork::V4_ERR_INVALID_FEE));
    // Anyone else signing is refused.
    env.svm = base.clone();
    let mut ix = fork::v4_set_params_ix(&p, 0, 2);
    let stranger = Pubkey::new_unique();
    ix.accounts[1].pubkey = stranger;
    let res = fork::admin_send(&mut env, ix, &[stranger]);
    assert_eq!(custom_code(&res), Some(fork::V4_ERR_INVALID_SIGN_ACCOUNT));

    let (mut env, pool) = env_with("cpmm_useless");
    let AnyPool::Cpmm(p) = pool else {
        unreachable!()
    };
    let base = env.svm.clone();
    let epoch = raydium_cpmm::OFF_RECENT_EPOCH..raydium_cpmm::OFF_RECENT_EPOCH + 8;
    let bytes = |svm: &LiteSVM, k: &Pubkey| bytes_masked(svm, k, epoch.clone());
    for status in [0u8, 1, 4, 7, 255] {
        env.svm = base.clone();
        shortwin_litesvm_tests::errors::ok(fork::raydium_admin_send(
            &mut env,
            fork::cpmm_update_pool_status_ix(&p, status),
        ));
        let real = bytes(&env.svm, &p.pool);
        let mut svm = base.clone();
        fork::set_cpmm_status(&mut svm, &p, status);
        assert_eq!(real, bytes(&svm, &p.pool), "status {status}");
    }
    let bytes = |svm: &LiteSVM, k: &Pubkey| svm.get_account(k).unwrap().data;
    let cfg = p.config_key(&base);
    let c = p.config(&base);
    for (param, value, rates) in [
        (
            0u8,
            40_000u64,
            (
                40_000,
                c.protocol_fee_rate,
                c.fund_fee_rate,
                c.creator_fee_rate,
            ),
        ),
        (
            7,
            959_999,
            (
                c.trade_fee_rate,
                c.protocol_fee_rate,
                c.fund_fee_rate,
                959_999,
            ),
        ),
        (
            1,
            500_000,
            (
                c.trade_fee_rate,
                500_000,
                c.fund_fee_rate,
                c.creator_fee_rate,
            ),
        ),
        (
            2,
            0,
            (c.trade_fee_rate, c.protocol_fee_rate, 0, c.creator_fee_rate),
        ),
    ] {
        env.svm = base.clone();
        shortwin_litesvm_tests::errors::ok(fork::raydium_admin_send(
            &mut env,
            fork::cpmm_update_amm_config_ix(&cfg, param, value),
        ));
        let mut svm = base.clone();
        fork::set_cpmm_config_rates(&mut svm, &cfg, rates.0, rates.1, rates.2, rates.3);
        assert_eq!(bytes(&env.svm, &cfg), bytes(&svm, &cfg), "param {param}");
    }
    // t + c must stay below 10⁶ (R-RAY §12.1): 960,000 with c = 500 hits the assert.
    env.svm = base.clone();
    assert!(
        fork::raydium_admin_send(&mut env, fork::cpmm_update_amm_config_ix(&cfg, 0, 999_500))
            .is_err()
    );
    env.svm = base.clone();
    shortwin_litesvm_tests::errors::ok(fork::raydium_admin_send(
        &mut env,
        fork::cpmm_update_amm_config_ix(&cfg, 0, 999_499),
    ));
}

// ---- negative controls ------------------------------------------------------------------------

/// The exactness above is not vacuous: on the pools whose exclusions are non-zero, a quote on
/// the raw vault balances (ignoring `need_take_pnl` / the fee counters) disagrees with what the
/// venue pays, and so does a quote with the fee one unit higher, while the reader-based quote
/// matches to the unit.
#[test]
fn negative_controls() {
    use shortwin_math::quote::{cpmm as qcpmm, ray_v4 as qv4};
    for name in ["v4rev", "cpmm_creator_fee", "cpmm_wsol_token1"] {
        let (mut env, pool) = env_with(name);
        let t = new_trader(&mut env, &pool);
        for dir in DIRS {
            let (x, _) = pool.io_reserves(&env.svm, dir).unwrap();
            let amount = x / 10;
            let good = pool.quote_exact_in(&env.svm, dir, amount).unwrap().proceeds;
            let (iv, ov) = io_vaults(&pool, dir);
            let (rx, ry) = (
                u128::from(fork::vault_amount(&env.svm, &iv)),
                u128::from(fork::vault_amount(&env.svm, &ov)),
            );
            let (raw, fee_off) = match &pool {
                AnyPool::V4(p) => {
                    let i = p.info(&env.svm);
                    let (n, d) = (i.swap_fee_numerator, i.swap_fee_denominator);
                    let (qx, qy) = p.io_reserves(&env.svm, dir).unwrap();
                    (
                        qv4::sell_exact_in(amount, rx, ry, n, d).unwrap().proceeds,
                        qv4::sell_exact_in(amount, qx.into(), qy.into(), n + 1, d)
                            .unwrap()
                            .proceeds,
                    )
                }
                AnyPool::Cpmm(p) => {
                    let f = p.fees(&env.svm, dir).unwrap();
                    let (qx, qy) = p.io_reserves(&env.svm, dir).unwrap();
                    let worse = shortwin_math::fees::CpmmFees {
                        trade: f.trade + 1,
                        ..f
                    };
                    (
                        qcpmm::sell_exact_in(amount, rx, ry, &f).unwrap().proceeds,
                        qcpmm::sell_exact_in(amount, qx.into(), qy.into(), &worse)
                            .unwrap()
                            .proceeds,
                    )
                }
            };
            fund_source(&mut env, &t, dir, amount);
            let (_, dst) = t.accounts(dir);
            let before = fork::vault_amount(&env.svm, &dst);
            let saved = env.svm.clone();
            shortwin_litesvm_tests::errors::ok(fork::swap(
                &mut env,
                &pool,
                &t,
                dir,
                Swap::ExactIn(amount, 0),
            ));
            let got = fork::vault_amount(&env.svm, &dst) - before;
            env.svm = saved;
            assert_eq!(got, good, "{name} {dir:?}");
            assert_ne!(
                got, raw,
                "{name} {dir:?}: raw vault balances would have matched"
            );
            assert_ne!(
                got, fee_off,
                "{name} {dir:?}: the fee off by one would have matched"
            );
        }
    }
}
