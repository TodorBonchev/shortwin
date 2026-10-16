//! CU baseline (M1 exit criterion; ARCH §6.1 T-CU starts here). Measured under LiteSVM's
//! mainnet feature set; the budgets are ARCH §6.1's transaction limits for the admin class
//! (the table's `flag_venue_upgraded` row stays 400 k; U5's 10 MiB ProgramData copy is VERIFIED
//! for CU since M3 — not metered, 6,032 / 8,440 CU — and only its latency and fee are unmeasured).
//! `cargo test -p shortwin-litesvm-tests cu_baseline -- --nocapture` prints the table.

use shortwin::PauseScope;
use shortwin::constants::GUARD_PUMP;
use shortwin::state::ConfigChange;
use shortwin_litesvm_tests::errors::ok;
use shortwin_litesvm_tests::fork;
use shortwin_litesvm_tests::{Env, ix};
use solana_signer::Signer;

#[test]
fn cu_baseline() {
    let mut rows: Vec<(&str, u64, u64)> = Vec::new();
    let mut env = Env::new();
    let args = env.default_init_args();
    let meta = ok(env.initialize_config(args));
    rows.push(("initialize_config", meta.compute_units_consumed, 200_000));

    let gov = env.governance.insecure_clone();
    let pause = env.pause_authority.insecure_clone();
    let change = ConfigChange::Params {
        params: env.config().params,
        tiers: env.config().tiers,
    };
    let meta = ok(env.send(&[ix::propose_config(&gov.pubkey(), change)], &[&gov]));
    rows.push((
        "propose_config(Params)",
        meta.compute_units_consumed,
        200_000,
    ));
    env.warp(i64::from(env.config().timelock_s));
    let meta = ok(env.send(&[ix::apply_config()], &[]));
    rows.push(("apply_config(Params)", meta.compute_units_consumed, 200_000));

    // `set_coverage_pause` (O17) on a list-only market, the role installed by the timelocked
    // `Authorities` change.
    let authority = solana_keypair::Keypair::new();
    let payer = env.payer.insecure_clone();
    let mint = shortwin_litesvm_tests::tokens::create_mint(
        &mut env.svm,
        &payer,
        &authority.pubkey(),
        6,
        shortwin_litesvm_tests::tokens::TokenProgram::Spl,
    );
    let creator = env.funded_keypair();
    ok(env.send(
        &[ix::create_market(
            &creator.pubkey(),
            &mint,
            &shortwin_litesvm_tests::tokens::TokenProgram::Spl.id(),
            0,
            &shortwin_litesvm_tests::pda::canonical_pool(&mint),
        )],
        &[&creator],
    ));
    env.enable_coverage_authority();
    let market = shortwin_litesvm_tests::pda::market(&mint).0;
    let meta = ok(env.set_coverage_pause(&market, true));
    rows.push(("set_coverage_pause", meta.compute_units_consumed, 60_000));

    let meta = ok(env.send(
        &[ix::set_pause(
            &pause.pubkey(),
            PauseScope::Global,
            true,
            None,
        )],
        &[&pause],
    ));
    rows.push(("set_pause(global)", meta.compute_units_consumed, 60_000));
    let meta = ok(env.send(&[ix::disable_venue(&pause.pubkey(), 0)], &[&pause]));
    rows.push(("disable_venue", meta.compute_units_consumed, 60_000));

    env.set_venue_program_data(GUARD_PUMP, fork::FIXTURE_GUARD_SLOTS[0] + 1);
    let meta = ok(env.send(&[ix::flag_venue_upgraded(GUARD_PUMP)], &[]));
    rows.push((
        "flag_venue_upgraded (mock ProgramData)",
        meta.compute_units_consumed,
        400_000,
    ));
    let meta = ok(env.send(
        &[ix::revalidate_venue(
            &pause.pubkey(),
            GUARD_PUMP,
            fork::FIXTURE_GUARD_SLOTS[0] + 1,
        )],
        &[&pause],
    ));
    rows.push((
        "revalidate_venue (mock ProgramData)",
        meta.compute_units_consumed,
        400_000,
    ));

    println!("| instruction | CU | budget |\n|---|---|---|");
    for (name, cu, budget) in &rows {
        println!("| {name} | {cu} | {budget} |");
        assert!(cu <= budget, "{name}: {cu} CU > {budget}");
    }
}
