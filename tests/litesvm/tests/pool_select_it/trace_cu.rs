//! T-TRACE / T-CU for pool selection (ARCH §6.1 "`propose_pool` / `execute_pool_switch`: 25–35 k
//! (est.), tx limit 100 k"; §6.2: 2 trace entries): every path of both instructions — a
//! list-only market's adoption, a listing-window and a zero-position switch across the programs,
//! a window opened and improved, an execution that adopts and one that cancels — makes exactly
//! one CPI (its `emit_cpi!` event, at stack height 2) and stays within the transaction limit.
//! `-- --nocapture` prints the figures.

use litesvm::types::TransactionMetadata;

use crate::common::*;
use crate::support::*;

/// The CU limit ARCH §6.1 proposes for these transactions.
const TX_LIMIT: u64 = 100_000;

fn check(what: &str, meta: &TransactionMetadata) -> u64 {
    let inner = meta.inner_instructions.first().map_or(0, Vec::len);
    let heights: Vec<u8> = meta
        .inner_instructions
        .iter()
        .flatten()
        .map(|i| i.stack_height)
        .collect();
    println!(
        "{what}: {} CU, {} trace entries",
        meta.compute_units_consumed,
        1 + inner
    );
    assert_eq!((inner, heights), (1, vec![2]), "{what}: the event CPI only");
    assert!(meta.compute_units_consumed <= TX_LIMIT, "{what}");
    meta.compute_units_consumed
}

#[test]
fn pool_selection_trace_and_cu() {
    // A list-only market adopts AMM v4 and CPMM pools.
    for fixture in [FART, USELESS] {
        let mut env = Env::ray_fork();
        let pool = env.load_ray_pool(fixture);
        list_auto(&mut env, &pool);
        let meta = ok(env.propose(None, &pool));
        check(
            &format!("propose_pool {fixture:?}: list-only adoption"),
            &meta,
        );
    }
    // Switches inside and after the listing window, across the programs.
    let (mut env, rm, q) = setup(FART);
    let lam = lambda_of(&env, &rm.pool);
    let cpmm = plant(&mut env, &rm.pool, Plant::Cpmm, true, (2, 1), lam);
    let meta = ok(env.propose(Some(&rm.pool), &cpmm));
    check("propose_pool: zero-position AMM v4 -> CPMM", &meta);
    env.warp(10);
    let v4 = plant(&mut env, &cpmm, Plant::V4, false, (2, 1), lam);
    let meta = ok(env.propose(Some(&cpmm), &v4));
    check("propose_pool: listing window CPMM -> AMM v4", &meta);
    // The fast path.
    let rm = rm.on_pool(&env.svm, v4);
    env.warp(3_601 - raydium::PRICE_WARMUP_CRANKS);
    env.warm_up_price(&rm);
    let mut b = env.borrower();
    open_ok(&mut env, &rm, &mut b, q / 4);
    halt(&mut env, &rm.pool);
    let a = plant(&mut env, &rm.pool, Plant::Cpmm, true, (40, 100), lam);
    let c = plant(&mut env, &rm.pool, Plant::V4, true, (60, 100), lam);
    crank_every(&mut env, &rm, 30, 5);
    let meta = ok(env.propose(Some(&rm.pool), &a));
    check("propose_pool: fast-path window opened", &meta);
    env.warp(5);
    let meta = ok(env.propose(Some(&rm.pool), &c));
    check("propose_pool: fast-path window improved", &meta);
    env.warp(60);
    let meta = ok(env.execute_switch(&rm.pool, &c));
    check("execute_pool_switch: fast adoption (ring rewrite)", &meta);
    // A cancellation.
    let rm = rm.on_pool(&env.svm, c);
    halt(&mut env, &rm.pool);
    let d = plant(&mut env, &rm.pool, Plant::Cpmm, true, (50, 100), lam);
    crank_every(&mut env, &rm, 30, 5);
    ok(env.propose(Some(&rm.pool), &d));
    env.warp(60);
    resume(&mut env, &rm.pool);
    let meta = ok(env.execute_switch(&rm.pool, &d));
    check("execute_pool_switch: cancelled", &meta);
}
