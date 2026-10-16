//! Stack depth on the pump curve (ARCHITECTURE §6.3; C1-8; the curve part of T-DEPTH): every
//! curve-path instruction of the release short.win, called directly and through one, two and
//! three levels of the TEST-ONLY wrapper program (`wrapper/`: it forwards one instruction by CPI,
//! as a Squads vault transaction or an integrator's program would call short.win), on LiteSVM's
//! **mainnet** feature set (`MAX_INSTRUCTION_STACK_DEPTH = 5`; SIMD-0268 inactive). Mollusk is
//! not used here: it enables every feature, SIMD-0268's limit of 9 included.
//!
//! Every curve leg peaks at 3 (short.win 1 → pump 2 → pump-fees / Token / System / pump's event
//! 3), so two wrapper levels fit (peak 5) and a third fails with `CallDepth`; the instructions
//! without a venue CPI peak at 2 and fit three. Every run goes through `Env::send`: the invariant
//! hooks see the wrapped instruction's events and accounts.
//!
//! The second half of T-DEPTH on the curve: the accounts pump would otherwise create under
//! short.win's CPI one level deeper — the trader's pump UVA (T16) and `trader_base_ata` (T18) —
//! must exist; a leg without them fails cleanly before any venue CPI, direct and wrapped.
//!
//! ```sh
//! cargo-build-sbf --tools-version v1.57 --arch v3 \
//!   --manifest-path tests/bench/m5_cu/wrapper/Cargo.toml --sbf-out-dir tests/bench/m5_cu/target/deploy
//! cargo test --manifest-path tests/bench/m5_cu/Cargo.toml --test t_depth -- --nocapture
//! ```
//!
//! `M5_WRAPPER_SO` overrides the wrapper's ELF; `M5_DEPTH_WRITE=1` rewrites `depth.md`.

use std::fmt::Write as _;

use m5_cu_bench::{
    BORROWER, T22, borrower, close_ix, depth_cases, load_wrapper, open_ix, repo_path, run_wrapped,
    setup, with_position, wrap,
};
use shortwin::errors::ShortwinError as E;
use shortwin_litesvm_tests::errors::custom_code;
use shortwin_litesvm_tests::fork;
use shortwin_litesvm_tests::pump;
use solana_account::Account;

/// `MAX_INSTRUCTION_STACK_DEPTH` on mainnet (ARCH §6.3).
const MAX_DEPTH: u8 = 5;

const HEADER: &str = "# M5 stack depth (pump curve)

Peak stack height (1 = top level) of every curve-path instruction of the release short.win on the \
dumped pump, pump-fees and PumpSwap programs, called directly and through 1–3 levels of the \
TEST-ONLY CPI wrapper (`wrapper/`), on LiteSVM 0.16.0 with the mainnet feature set \
(`MAX_INSTRUCTION_STACK_DEPTH = 5`, ARCH §6.3). Written by `M5_DEPTH_WRITE=1 cargo test \
--manifest-path tests/bench/m5_cu/Cargo.toml --test t_depth` (README.md); the test fails unless \
each level reaches exactly `direct + levels`, and a level above 5 fails with `CallDepth`.

| Instruction | Case | Direct | 1 wrapper | 2 wrappers | 3 wrappers |
|---|---|---|---|---|---|
";

#[test]
fn curve_paths_keep_two_spare_levels() {
    let mut table = String::from(HEADER);
    let mut failures = Vec::new();
    for case in depth_cases() {
        let mut cells = Vec::new();
        for levels in 0..=3usize {
            let want = case.direct_peak + levels as u8;
            let got = run_wrapped(&case, levels);
            let cell = match (&got, want <= MAX_DEPTH) {
                (Ok(peak), true) if *peak == want => peak.to_string(),
                (Err(e), false) if e.contains("CallDepth") => "refused (`CallDepth`)".to_owned(),
                _ => {
                    failures.push(format!(
                        "{} ({}), {levels} wrapper level(s): expected {}, got {got:?}",
                        case.handler,
                        case.case,
                        if want <= MAX_DEPTH {
                            format!("peak {want}")
                        } else {
                            "CallDepth".to_owned()
                        }
                    ));
                    "*unexpected*".to_owned()
                }
            };
            cells.push(cell);
        }
        writeln!(
            table,
            "| `{}` | {} | {} |",
            case.handler,
            case.case,
            cells.join(" | ")
        )
        .unwrap();
    }
    println!("{table}");
    if std::env::var_os("M5_DEPTH_WRITE").is_some() {
        let path = repo_path("tests/bench/m5_cu/depth.md");
        std::fs::write(&path, &table).unwrap_or_else(|e| panic!("write {}: {e}", path.display()));
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// ARCH §6.3 / §3.12 on the curve: pump would create the trader's pump UVA and base ATA itself
/// (System and ATA CPIs one level below pump) if they were missing; short.win requires both, so
/// an open or a close without the UVA fails cleanly with `VenueAccountMismatch` and an open
/// without `trader_base_ata` with `TraderStateInvalid`, before pump runs — never a deeper
/// creation — directly and through one wrapper level.
#[test]
fn a_missing_venue_account_fails_cleanly() {
    for levels in 0..=1usize {
        // The trader's pump UVA removed: the open is refused before the sale.
        let (mut env, pm, q) = setup(T22);
        load_wrapper(&mut env);
        let uva = fork::pump_user_volume_accumulator(&pm.trader());
        env.svm.set_account(uva, Account::default()).unwrap();
        env.tick(&pm, 1);
        let b = borrower(&mut env, BORROWER);
        let ix = wrap(&open_ix(&pm, &b.key(), 0, q), levels);
        let res = env.send(&[pump::cu_limit_ix(1_400_000), ix], &[&b.kp]);
        let err = res.as_ref().expect_err("no UVA");
        assert_eq!(
            custom_code(&res),
            Some(u32::from(E::VenueAccountMismatch)),
            "{levels} level(s): {:?}",
            err.err
        );
        assert!(
            !err.meta
                .logs
                .iter()
                .any(|l| l.contains(&format!("{} invoke", shortwin::constants::PUMP_PROGRAM_ID))),
            "{levels} level(s): pump never ran"
        );

        // The UVA removed under an open position: the close is refused the same way.
        let mut p = with_position(T22);
        load_wrapper(&mut p.env);
        let uva = fork::pump_user_volume_accumulator(&p.pm.trader());
        p.env.svm.set_account(uva, Account::default()).unwrap();
        let ix = wrap(&close_ix(&p.pm, &p.b.key(), p.nonce), levels);
        let res = p.env.send(&[pump::cu_limit_ix(1_400_000), ix], &[&p.b.kp]);
        assert_eq!(
            custom_code(&res),
            Some(u32::from(E::VenueAccountMismatch)),
            "{levels} level(s): close without the UVA"
        );

        // `trader_base_ata` removed: the open fails on the trader-state check before pump runs.
        let (mut env, pm, q) = setup(T22);
        load_wrapper(&mut env);
        env.svm
            .set_account(pm.trader_base_ata, Account::default())
            .unwrap();
        env.tick(&pm, 1);
        let b = borrower(&mut env, BORROWER);
        let ix = wrap(&open_ix(&pm, &b.key(), 0, q), levels);
        let res = env.send(&[pump::cu_limit_ix(1_400_000), ix], &[&b.kp]);
        let err = res.as_ref().expect_err("no trader_base_ata");
        assert_eq!(
            custom_code(&res),
            Some(u32::from(E::TraderStateInvalid)),
            "{levels} level(s): {:?}",
            err.err
        );
        assert!(
            !err.meta
                .logs
                .iter()
                .any(|l| l.contains(&format!("{} invoke", shortwin::constants::PUMP_PROGRAM_ID))),
            "{levels} level(s): pump never ran"
        );
    }
}
