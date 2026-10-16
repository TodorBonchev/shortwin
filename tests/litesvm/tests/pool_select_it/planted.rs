//! The planted candidate pools (`pool_select::{plant_v4, plant_cpmm}`) are real pools to the
//! dumped ELFs: exact-in and exact-out swaps in both directions move exactly what
//! `shortwin-math`'s quotes say on the readers' reserves, on either SOL side and both programs,
//! and short.win's own reader sees the planted `λ` and `D`.

use shortwin_litesvm_tests::fork::Swap;
use shortwin_math::fees::Direction;
use shortwin_math::pool_select::locked_depth as depth;

use crate::common::*;
use crate::support::*;

/// A swap of `amount` in `dir` / `swap` on `pool` by `t` moves the quoted amounts.
fn swap_as_quoted(env: &mut Env, pool: &AnyPool, t: &fork::Trader, dir: Direction, exact_in: bool) {
    let (x_in, x_out) = pool.io_reserves(&env.svm, dir).unwrap();
    let (src, dst) = t.accounts(dir);
    let (src0, dst0) = (
        fork::vault_amount(&env.svm, &src),
        fork::vault_amount(&env.svm, &dst),
    );
    let (paid, got) = if exact_in {
        let a = x_in / 500;
        let q = pool.quote_exact_in(&env.svm, dir, a).unwrap();
        ok(fork::swap(env, pool, t, dir, Swap::ExactIn(a, q.proceeds)));
        (a, q.proceeds)
    } else {
        let out = x_out / 700;
        let q = pool.quote_exact_out(&env.svm, dir, out).unwrap();
        ok(fork::swap(env, pool, t, dir, Swap::ExactOut(q.cost, out)));
        (q.cost, out)
    };
    assert_eq!(
        src0 - fork::vault_amount(&env.svm, &src),
        paid,
        "{dir:?} paid"
    );
    assert_eq!(
        fork::vault_amount(&env.svm, &dst) - dst0,
        got,
        "{dir:?} received"
    );
}

#[test]
fn planted_pools_trade_like_the_dumped_ones() {
    let mut env = Env::ray_fork();
    let fart = env.load_ray_pool(FART);
    let useless = env.load_ray_pool(USELESS);
    for (base, kind) in [
        (&fart, Plant::V4),
        (&fart, Plant::Cpmm),
        (&useless, Plant::V4),
        (&useless, Plant::Cpmm),
    ] {
        if kind == Plant::V4 && base.token().1 != TokenProgram::Spl {
            continue;
        }
        for sol_first in [true, false] {
            let pool = plant(&mut env, base, kind, sol_first, (1, 3), 9_000);
            assert_eq!(pool.token(), base.token());
            // short.win's view: the planted λ and D.
            assert_eq!(lambda_of(&env, &pool), 9_000);
            let (x, y) = pool.io_reserves(&env.svm, Direction::Sell).unwrap();
            let (bx, by) = base.io_reserves(&env.svm, Direction::Sell).unwrap();
            assert_eq!((x, y), (bx / 3, by / 3), "the base's reserves scaled");
            assert_eq!(
                d_of(&env, &pool),
                depth(
                    raydium::lp_counter(&env.svm, &pool),
                    raydium::mint_supply(&env.svm, &raydium::lp_mint(&pool)),
                    x,
                    y
                )
                .unwrap()
            );
            let (mint, program) = pool.token();
            let t = fork::trader(&mut env, &mint, program, 1_000_000_000_000, x / 10);
            for dir in [Direction::Sell, Direction::Buy] {
                for exact_in in [true, false] {
                    swap_as_quoted(&mut env, &pool, &t, dir, exact_in);
                }
            }
        }
    }
}
