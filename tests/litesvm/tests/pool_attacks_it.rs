//! Pool-capture attacks on Raydium pool selection (M4; ARCH §2.6 "Attack | Outcome" table,
//! rules 1–5, §3.9; ECON §6.2, §10.1, §10.3; R-RAY §6, §12.2–§12.3; review findings V2-7, V2-8,
//! V2-11, R3B-7 a, R4B-10), on the mainnet fork: the release short.win against the dumped AMM v4
//! and CPMM programs, dumped pools and **planted** candidate pools
//! (`shortwin_litesvm_tests::pool_select::{plant_v4, plant_cpmm}`), with the attacker's
//! liquidity moves made by the **real CPMM `deposit` / `withdraw`** instructions of the dumped
//! ELF wherever the attack needs them (module `lp`). Every instruction goes through `Env::send`,
//! so the invariant hooks — I15 (pool selection) and I17 included — run after each one.
//!
//! Each test asserts the outcome the design states for the attack; where the program follows
//! the design but the design leaves a gap, the test pins the current behaviour and says so
//! (a tripwire, to be inverted when the design changes).
//!
//! | Module | Attack | Stated outcome asserted |
//! |---|---|---|
//! | `capture` | a fake deep pool of self-supplied, unlocked liquidity, adopted and then pulled | unlocked deposits and withdrawals move `√(X·Y)` and `λ` inversely and leave `D` unchanged (the real ELF); on a list-only market the attacker keeps the reference against the genuine pool only with `D_F > D_G/1.05`, i.e. by leaving at least that much burned liquidity in its pool for good, which the pull cannot touch (`pool_d` unchanged) |
//! | `lambda_games` | LP burns, "unburns" and locker games on `λ` and `D` | lockers, burn-looking addresses and transfers never count; a real burn is the only way up; re-minting LP (a deposit) lowers `λ_now` but not `D`, the incumbent stays and opens are sized on `D` (V2-8). The implementer's two **tripwires, inverted** by the M4 security review (M4-SEC-1, -3, -5): an atomic `[deposit, execute_pool_switch, withdraw]` — or, on a small candidate, `[sell, execute_pool_switch, buy back]` under `pool_min_real_sol` — no longer cancels the window (eligibility tests the locked depth at `P_ema`), and the adoption writes the depth the window recorded, not the execution's sample |
//! | `hysteresis` | flip-flopping between two near-equal pools | neither direction clears the 5 % (window) / 25 % (after) margin; proposal spam never moves the pool or extends the listing window; flipping back costs wash-trading fees ≈ `2·Y·ln(1 + x)` on the real AMM v4 ELF (V2-7) |
//! | `front_run` | front-running the fast path with a thin or partially-unlocked pool | the fast path is not armed before 120 s; the collection window keeps the deepest candidate; an own pool wins only if it is the deepest, and its locked half cannot be withdrawn: `D`, `pool_d` and the positions' sizing survive the pull (rule 5) |
//! | `griefing` | a standing minimum position pinning the pool (R4B-10, disclosed) | `PositionsOpen` across an atomic expire-and-reopen at the deadline; the Emergency pause of opens is the remedy: the market drains and the dominant pool is adopted at `open_positions == 0` |
//! | `sides` | base/quote-swapped and non-WSOL candidates | the SOL side is derived from the verified mint fields (all four orders adopted, `P_ema` = every pool's spot, a round trip on the last); look-alike quotes (a fake WSOL, a stablecoin, the Token-2022 native mint, the token itself, WSOL twice), WSOL under Token-2022, vault fields that disagree with the mint fields and a WSOL vault the pool authority does not own are refused |
//! | `latched` | candidates while a Raydium upgrade is flagged | `VenueUpgraded` for every adoption and window candidate on the flagged program, none on the other (M4-SEC-8), `[flag, propose]` refused whole and `[propose, flag]` not; a cancellation and an exit are not latched, and a window whose candidate's program is flagged is cancelled with the chain still armed (M4V-4); adoption after `revalidate_venue` |
//! | `mid_position` | switching under open positions | `venue_switch.py`'s fast-path rows (RA, RB, RC: halt → fast path to a 25 % pool → a 2× pump over 120 s) on pools of ≈ 38,000, 3,800 and 380 SOL: liquidation, lenders whole (switch only if all fit: on RC the default size is above the 25 % pool's per-position cap, so the row carries 90 % of that cap); the settlement box ≤ 1 % wide in `L` in every one of the 60 s after the change, against 5 s for the v0.3 carry-over replayed on the same ring; no atomic liquidation or settlement through a mispriced adopted pool, and none before `liq_persist_n` / `settle_persist_n` of a held price; locked terms and EMA continuity across two fast paths |
//!
//! `-- --nocapture` prints the measured figures (the capture's donation, the tripwires, the
//! wash-trading cost, the box widths and exits of the three rows, the held-price timeline).

// `TxResult` is LiteSVM's own `TransactionResult` (as in the harness library).
#![allow(clippy::result_large_err)]

#[allow(unused_imports)]
#[path = "ray_it/common.rs"]
mod common;

#[path = "pool_select_it/support.rs"]
mod support;

/// The attacker's liquidity moves on a CPMM pool through the dumped program's own `deposit`
/// and `withdraw` (Anchor; discriminators `sha256("global:<name>")[..8]`; accounts in the
/// program's order), and the LP holdings a planted pool's circulating supply stands for.
mod lp {
    use anchor_lang::prelude::pubkey;
    use shortwin::constants::{RAYDIUM_CPMM_AUTHORITY, TOKEN_2022_PROGRAM_ID, TOKEN_PROGRAM_ID};
    use shortwin_litesvm_tests::fork::CpmmPool;
    use shortwin_litesvm_tests::tokens::TokenProgram;
    use solana_keypair::Keypair;

    use crate::common::*;

    /// SPL Memo v2 (`withdraw` names it).
    pub const MEMO: Pubkey = pubkey!("MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr");

    fn disc(name: &str) -> [u8; 8] {
        let h = fork::sha256(format!("global:{name}").as_bytes());
        let mut d = [0u8; 8];
        d.copy_from_slice(&h[..8]);
        d
    }

    /// A liquidity provider of a CPMM pool: its token 0 / token 1 accounts and its LP account.
    pub struct Provider {
        pub owner: Keypair,
        pub t0: Pubkey,
        pub t1: Pubkey,
        pub lp: Pubkey,
    }

    impl Provider {
        pub fn key(&self) -> Pubkey {
            self.owner.pubkey()
        }
    }

    /// A provider of `p` holding `sol` wrapped lamports, `tokens` of the pool's token and
    /// `lp` LP tokens. The LP balance is planted in the account (a planted pool's LP mint
    /// supply already counts every circulating LP token; `lp` must be part of it).
    pub fn provider(env: &mut Env, p: &CpmmPool, sol: u64, tokens: u64, lp: u64) -> Provider {
        let owner = env.funded_keypair();
        let w = fork::fund_wsol(env, &owner.pubkey(), sol);
        let t = fork::fund_token(
            env,
            &owner.pubkey(),
            &p.token_mint(),
            p.token_program(),
            tokens,
        );
        let l = fork::fund_token(env, &owner.pubkey(), &p.lp_mint, TokenProgram::Spl, lp);
        let (t0, t1) = if p.sol_is_token0 { (w, t) } else { (t, w) };
        Provider {
            owner,
            t0,
            t1,
            lp: l,
        }
    }

    fn metas(p: &CpmmPool, who: &Provider) -> Vec<AccountMeta> {
        vec![
            AccountMeta::new_readonly(who.key(), true),
            AccountMeta::new_readonly(RAYDIUM_CPMM_AUTHORITY, false),
            AccountMeta::new(p.pool, false),
            AccountMeta::new(who.lp, false),
            AccountMeta::new(who.t0, false),
            AccountMeta::new(who.t1, false),
            AccountMeta::new(p.token_0_vault, false),
            AccountMeta::new(p.token_1_vault, false),
            AccountMeta::new_readonly(TOKEN_PROGRAM_ID, false),
            AccountMeta::new_readonly(TOKEN_2022_PROGRAM_ID, false),
            AccountMeta::new_readonly(p.token_0_mint, false),
            AccountMeta::new_readonly(p.token_1_mint, false),
            AccountMeta::new(p.lp_mint, false),
        ]
    }

    fn data(name: &str, a: u64, b: u64, c: u64) -> Vec<u8> {
        let mut d = disc(name).to_vec();
        for v in [a, b, c] {
            d.extend_from_slice(&v.to_le_bytes());
        }
        d
    }

    /// CPMM `deposit(lp_token_amount, maximum_token_0_amount, maximum_token_1_amount)`: mints
    /// `lp` new LP tokens to the provider against both sides pro rata (rounded up).
    pub fn deposit_ix(p: &CpmmPool, who: &Provider, lp: u64) -> Instruction {
        Instruction {
            program_id: fork::RaydiumProgram::Cpmm.id(),
            accounts: metas(p, who),
            data: data("deposit", lp, u64::MAX, u64::MAX),
        }
    }

    /// CPMM `withdraw(lp_token_amount, minimum_token_0_amount, minimum_token_1_amount)`: burns
    /// `lp` of the provider's LP tokens for both sides pro rata (rounded down).
    pub fn withdraw_ix(p: &CpmmPool, who: &Provider, lp: u64) -> Instruction {
        let mut accounts = metas(p, who);
        accounts.push(AccountMeta::new_readonly(MEMO, false));
        Instruction {
            program_id: fork::RaydiumProgram::Cpmm.id(),
            accounts,
            data: data("withdraw", lp, 0, 0),
        }
    }

    /// The provider's `(SOL, token)` balances.
    pub fn needed(env: &Env, p: &CpmmPool, lp: u64) -> (u64, u64) {
        let counter = p.state(&env.svm).lp_supply;
        let (x, y) = AnyPool::Cpmm(*p)
            .io_reserves(&env.svm, shortwin_math::fees::Direction::Sell)
            .expect("reserves");
        let part =
            |r: u64| (u128::from(r) * u128::from(lp)).div_ceil(u128::from(counter)) as u64 + 1;
        (part(y), part(x))
    }
}

/// A fake deep pool of self-supplied liquidity (ARCH §2.6 table row 1; R-RAY §6.3 "Why `L`
/// resists manipulation").
mod capture {
    use shortwin::events::{POOL_KIND_LISTING, POOL_KIND_LISTING_WINDOW};
    use shortwin_litesvm_tests::raydium::{fartcoin_curve_bytes, plant_curve};
    use shortwin_math::fees::Direction;

    use crate::common::*;
    use crate::lp;
    use crate::support::*;

    /// `(λ_bps, √(X·Y), D)` of a pool now.
    fn metrics(env: &Env, pool: &AnyPool) -> (u64, u128, u128) {
        let (x, y) = pool.io_reserves(&env.svm, Direction::Sell).unwrap();
        (
            lambda_of(env, pool),
            (u128::from(x) * u128::from(y)).isqrt(),
            d_of(env, pool),
        )
    }

    /// The real CPMM program: an attacker's unlocked deposit of nine times a pool's liquidity
    /// multiplies `√(X·Y)` by ten and divides `λ` by ten — `D` stays within rounding (the
    /// `λ` floor to whole bps), and the pool, now far below `λ_min`, stays eligible (M4-SEC-1:
    /// eligibility tests `D`, which the deposit cannot hold down): a list-only market adopts it
    /// on its locked depth; the withdrawal of that LP restores `λ` and `√(X·Y)`, `D` (and the
    /// market's `pool_d`) again unchanged. Unlocked liquidity adds nothing to `D`, in either
    /// direction.
    #[test]
    fn unlocked_liquidity_moves_lambda_not_d() {
        let mut env = Env::ray_fork();
        let base = env.load_ray_pool(FART);
        list_auto(&mut env, &base);
        let pool = plant(&mut env, &base, Plant::Cpmm, true, (1, 10), 6_000);
        let AnyPool::Cpmm(p) = pool else {
            unreachable!()
        };
        let (lam0, k0, d0) = metrics(&env, &pool);
        assert_eq!(lam0, 6_000);
        let counter = p.state(&env.svm).lp_supply;
        let add = 9 * counter;
        let (sol, tok) = lp::needed(&env, &p, add);
        let who = lp::provider(&mut env, &p, sol, tok, 0);
        let signer = who.owner.insecure_clone();
        ok(env.send(&[lp::deposit_ix(&p, &who, add)], &[&signer]));
        assert_eq!(tokens(&env, &who.lp), add, "the ELF minted the LP");
        let (lam1, k1, d1) = metrics(&env, &pool);
        assert_eq!(lam1, 600, "λ divided by ten");
        assert!(
            k1 >= 10 * k0 - 10 && k1 <= 10 * k0 + 10,
            "√(X·Y) ×10: {k0} -> {k1}"
        );
        assert!(d1.abs_diff(d0) * 10_000 <= d0, "D {d0} -> {d1}");
        let e = adopt_ok(&mut env, None, &pool);
        assert_eq!((e.kind, e.d_cand), (POOL_KIND_LISTING, d1));
        ok(env.send(&[lp::withdraw_ix(&p, &who, add)], &[&signer]));
        let (lam2, k2, d2) = metrics(&env, &pool);
        assert_eq!(lam2, 6_000);
        assert!(
            k2 >= k0 && k2 - k0 <= 10,
            "√(X·Y) restored (rounding stays in the pool)"
        );
        assert!(d2 >= d0 && (d2 - d0) * 10_000 <= d0, "D {d0} -> {d2}");
        ok(env.send(&[crank_price_ix(&market_view(&env, &pool))], &[]));
        let pd = market_of(&env, &pool).pool_d.get();
        assert_eq!(
            pd, d2,
            "pool_d follows D through the deposit and the withdrawal"
        );
    }

    /// A list-only market adopts any eligible candidate (ARCH §2.5 `None`), so an attacker can
    /// front-run the keeper with an own pool `F` (CPMM, WSOL as token 1, half its LP burned,
    /// the rest held unlocked). The genuine pool `G` (Fartcoin's main AMM v4 pool, λ 99.78 %)
    /// then replaces it inside the listing window iff `D_G ≥ 1.05·D_F` (rule 1):
    ///
    /// - `D_F = 94 % of D_G`: `G` is adopted 30 s later; the window keeps its start (the
    ///   front-run bought no extra delay).
    /// - `D_F = 96 % of D_G`: `G` is refused, inside the window (5 %) and after it (25 %). The
    ///   attacker then withdraws every unlocked LP of `F` with the real ELF: `D_F` and the
    ///   market's `pool_d` are unchanged, and the liquidity left in `F` — locked for good —
    ///   holds more than `λ_G·Y_G/1.05` SOL (and the matching tokens): keeping the reference
    ///   cost a permanent donation of at least the genuine pool's locked depth over 1.05, the
    ///   "honest outcome" of the design's table.
    #[test]
    fn a_fake_pool_keeps_a_list_only_market_only_by_burning() {
        for (what, num, keeps) in [("short", 1_876u64, false), ("keeps", 1_916, true)] {
            let mut env = Env::ray_fork();
            let g = env.load_ray_pool(FART);
            let (mint, _) = g.token();
            plant_curve(&mut env.svm, &mint, fartcoin_curve_bytes());
            list_auto(&mut env, &g);
            // F: G's reserves ×1.876 (1.916), λ 50 %; the attacker holds the unlocked half.
            let f = plant(&mut env, &g, Plant::Cpmm, false, (num, 1_000), 5_000);
            let AnyPool::Cpmm(fp) = f else { unreachable!() };
            let (d_g, d_f) = (d_of(&env, &g), d_of(&env, &f));
            let dominated = d_g * 10_000 >= d_f * 10_500;
            assert_eq!(dominated, !keeps, "{what}: D_G {d_g}, D_F {d_f}");
            let e = adopt_ok(&mut env, None, &f);
            assert_eq!(e.kind, POOL_KIND_LISTING, "{what}");
            let seen = market_of(&env, &g).pool_seen_at;
            env.warp(30);
            if !keeps {
                let e = adopt_ok(&mut env, Some(&f), &g);
                assert_eq!((e.kind, e.d_inc), (POOL_KIND_LISTING_WINDOW, d_f), "{what}");
                let m = market_of(&env, &g);
                assert_eq!(m.pool_seen_at, seen, "{what}: the window keeps its start");
                assert_eq!(pool_now_of(&env, &g), pool_key(&g));
                continue;
            }
            assert_err(&env.propose(Some(&f), &g), E::PoolNotDominant);
            let slot = env.clock().slot;
            let window = i64::from(env.config().params.pool_list_window_s);
            env.set_clock(seen + window, slot + 20_000);
            assert_err(&env.propose(Some(&f), &g), E::PoolNotDominant);
            // The pull: every unlocked LP token, burned by the real withdraw.
            let counter = fp.state(&env.svm).lp_supply;
            let circulating = raydium::mint_supply(&env.svm, &fp.lp_mint);
            assert_eq!(circulating, counter / 2);
            let who = lp::provider(&mut env, &fp, 0, 0, circulating);
            let signer = who.owner.insecure_clone();
            ok(env.send(&[lp::withdraw_ix(&fp, &who, circulating)], &[&signer]));
            assert_eq!(lambda_of(&env, &f), 10_000, "only burned LP is left");
            let d_after = d_of(&env, &f);
            assert!(
                d_after >= d_f && (d_after - d_f) * 10_000 <= d_f,
                "D_F {d_f} -> {d_after}"
            );
            ok(env.send(&[crank_price_ix(&market_view(&env, &f))], &[]));
            let m = market_of(&env, &f);
            assert_eq!(m.pool_d.get(), d_after, "pool_d follows D, not the pull");
            // The donation: F's SOL reserve now (all locked) against the genuine locked depth.
            let (_, y_f) = f.io_reserves(&env.svm, Direction::Sell).unwrap();
            let (_, y_g) = g.io_reserves(&env.svm, Direction::Sell).unwrap();
            let lam_g = u128::from(lambda_of(&env, &g));
            assert!(
                u128::from(y_f) * 10_500 > lam_g * u128::from(y_g),
                "locked SOL left in F {y_f} vs λ_G·Y_G/1.05 ({lam_g} bps of {y_g})"
            );
            println!(
                "capture: D_F/D_G = {:.4}; after the pull F holds {:.1} SOL locked (G: {:.1} SOL, λ {:.2} %)",
                d_f as f64 / d_g as f64,
                y_f as f64 / 1e9,
                y_g as f64 / 1e9,
                lam_g as f64 / 100.0
            );
        }
    }

    /// The pool a market's cache names now.
    fn pool_now_of(env: &Env, pool: &AnyPool) -> Pubkey {
        shortwin_litesvm_tests::pool_select::cached_pool(&env.svm, &pda::market(&pool.token().0).0)
    }

    fn pool_key(pool: &AnyPool) -> Pubkey {
        shortwin_litesvm_tests::pool_select::pool_key(pool)
    }

    /// A `RayMarket` view of `pool`'s market for its builders (no lender).
    pub fn market_view(env: &Env, pool: &AnyPool) -> RayMarket {
        let (mint, program) = pool.token();
        let market = pda::market(&mint).0;
        RayMarket {
            fixture: shortwin_litesvm_tests::pool_select::fixture_of(pool),
            pool: *pool,
            keys: shortwin_litesvm_tests::fixtures::MarketKeys {
                mint,
                token_program: program.id(),
                market,
                market_vault: pda::market_vault(&market).0,
                pool_vault: pda::pool_vault(&market).0,
                trader: pda::trader(&market).0,
            },
            trader_wsol: pda::trader_wsol(&market).0,
            lender: solana_keypair::Keypair::new(),
            lender_account: Pubkey::default(),
            amm_config: match pool {
                AnyPool::Cpmm(p) => Some(raydium::cpmm_config_key(&env.svm, p)),
                AnyPool::V4(_) => None,
            },
        }
    }
}

/// LP burns, "unburns" and locker games on `λ` (ARCH §2.6 "Eligibility" and rule 5; R-RAY
/// §6.3, §12.3; V2-8, V2-11), and the fast path's execution-time eligibility.
mod lambda_games {
    use anchor_lang::prelude::pubkey;
    use shortwin::constants::TOKEN_PROGRAM_ID;
    use shortwin::events::{POOL_KIND_FAST, POOL_KIND_LISTING};
    use shortwin_litesvm_tests::fork::Swap;
    use shortwin_litesvm_tests::pool_select::{execute_pool_switch_ix, pool_key, propose_pool_ix};
    use shortwin_litesvm_tests::raydium::candidate_read_set;
    use shortwin_litesvm_tests::tokens::TokenProgram;
    use shortwin_math::fees::Direction;

    use crate::common::*;
    use crate::lp;
    use crate::scenario::row_market;
    use crate::support::*;

    /// The Solana incinerator (tokens sent to its accounts are commonly called "burned").
    const INCINERATOR: Pubkey = pubkey!("1nc1nerator11111111111111111111111111111111");

    fn transfer(env: &mut Env, who: &lp::Provider, dst: &Pubkey, amount: u64) {
        let ix = spl_token_interface::instruction::transfer(
            &TOKEN_PROGRAM_ID,
            &who.lp,
            dst,
            &who.key(),
            &[],
            amount,
        )
        .unwrap();
        let signer = who.owner.insecure_clone();
        ok(env.send(&[ix], &[&signer]));
    }

    /// `D` — and so eligibility (M4-SEC-1: the floor on locked depth) — counts only LP the
    /// venue's counter holds and no one can redeem:
    ///
    /// 1. A planted CPMM pool of 25.1 SOL at λ = 49 % (the attacker holds every circulating LP
    ///    token): its locked SOL over `λ_min` is ≈ 24.6 SOL, under the 25-SOL floor
    ///    (`PoolTooThin`).
    /// 2. Locker games: 40 % of the attacker's LP moved into a locker program's vault (a PDA's
    ///    token account) and 10 % sent to the incinerator's account leave the mint's supply,
    ///    `λ` and `D` unchanged — still refused (lockers count as circulating, V2-11).
    /// 3. A real SPL `burn` of exactly the LP that reaches λ = 50 % lifts `D` over the floor:
    ///    adopted by the list-only market, `D` up by exactly the burn.
    /// 4. The only "unburn" is new LP from the pool's own `deposit` (the real ELF): `λ_now`
    ///    falls to 41.6 %, below `λ_min`, while `D` and the market's `pool_d` stay (the counter
    ///    grows with the supply); nothing re-validates the incumbent, and nothing needs to.
    /// 5. V2-8: after the listing window, an open on that pool succeeds (opens are sized on
    ///    locked depth, never refused on `λ_now`; the I15 hook re-checks the sizing on `D`).
    #[test]
    fn locker_burn_and_unburn_games() {
        let mut env = Env::ray_fork();
        let base = env.load_ray_pool(FART);
        list_auto(&mut env, &base);
        let spec = shortwin_litesvm_tests::pool_select::spec_like(&env.svm, &base, true, 4_900);
        let sol = 25_100_000_000u64;
        let pool = plant_spec(
            &mut env,
            Plant::Cpmm,
            shortwin_litesvm_tests::pool_select::PlantSpec {
                sol,
                tokens: (u128::from(spec.tokens) * u128::from(sol) / u128::from(spec.sol)) as u64,
                ..spec
            },
        );
        let AnyPool::Cpmm(p) = pool else {
            unreachable!()
        };
        let counter = p.state(&env.svm).lp_supply;
        let supply = raydium::mint_supply(&env.svm, &p.lp_mint);
        let who = lp::provider(&mut env, &p, 0, 0, supply);
        let d0 = d_of(&env, &pool);
        assert_eq!(lambda_of(&env, &pool), 4_900);
        assert_err(&env.propose(None, &pool), E::PoolTooThin);
        // 2. The locker and the incinerator.
        let locker_program = pubkey!("LockrWmn6K5twhz3y9w1dQERbmgSaRkfnTeTKbpofwE");
        let locker = Pubkey::find_program_address(&[b"locker", p.pool.as_ref()], &locker_program).0;
        let locker_vault = fork::fund_token(&mut env, &locker, &p.lp_mint, TokenProgram::Spl, 0);
        let ash = fork::fund_token(&mut env, &INCINERATOR, &p.lp_mint, TokenProgram::Spl, 0);
        transfer(&mut env, &who, &locker_vault, supply / 10 * 4);
        transfer(&mut env, &who, &ash, supply / 10);
        assert_eq!(raydium::mint_supply(&env.svm, &p.lp_mint), supply);
        assert_eq!((lambda_of(&env, &pool), d_of(&env, &pool)), (4_900, d0));
        assert_err(&env.propose(None, &pool), E::PoolTooThin);
        // 3. A real burn to λ = 50 %.
        let burn = supply - raydium::supply_for_lambda(counter, 5_000);
        assert!(burn <= tokens(&env, &who.lp));
        let ix = spl_token_interface::instruction::burn(
            &TOKEN_PROGRAM_ID,
            &who.lp,
            &p.lp_mint,
            &who.key(),
            &[],
            burn,
        )
        .unwrap();
        let signer = who.owner.insecure_clone();
        ok(env.send(&[ix], &[&signer]));
        assert_eq!(lambda_of(&env, &pool), 5_000);
        let (x, y) = pool.io_reserves(&env.svm, Direction::Sell).unwrap();
        let k = (u128::from(x) * u128::from(y)).isqrt();
        assert_eq!(
            (d0, d_of(&env, &pool)),
            (k * 4_900 / 10_000, k * 5_000 / 10_000)
        );
        let e = adopt_ok(&mut env, None, &pool);
        assert_eq!(e.kind, POOL_KIND_LISTING);
        let d1 = env.market(&pda::market(&p.token_mint()).0).pool_d.get();
        // 4. "Unburn": new LP only through the pool's deposit.
        let add = counter / 5;
        let (sol, tok) = lp::needed(&env, &p, add);
        let minter = lp::provider(&mut env, &p, sol, tok, 0);
        let signer = minter.owner.insecure_clone();
        ok(env.send(&[lp::deposit_ix(&p, &minter, add)], &[&signer]));
        let lam = lambda_of(&env, &pool);
        assert!(lam < 5_000 && lam > 4_100, "λ_now {lam}");
        // `D` moves only by `λ`'s floor to whole bps (≤ 1/4,166 here).
        let d2 = d_of(&env, &pool);
        assert!(d2.abs_diff(d1) * 10_000 <= d1 * 3, "D {d1} -> {d2}");
        // 5. The market after its window: an open on λ_now < λ_min.
        let (rm, _) = adopted_market(&mut env, pool);
        let m = env.market(&rm.market());
        assert!(
            m.pool_d.get().abs_diff(d1) * 10_000 <= d1 * 3,
            "pool_d follows D"
        );
        assert_eq!(
            pool_now(&env, &rm),
            p.pool,
            "the incumbent is not re-validated"
        );
        // AIR-13: the list-only adoption started the price state with `L_slow` at the pool's
        // locked depth `D` — half its √(X·Y) at λ = 50 %, so `R_eff = min(R_now, R_slow, …)` sat
        // under `pool_min_real_sol` (`PoolTooThin`) on this pool at the floor; three hours of the
        // keeper's 60-s observations bring `L_slow` to the pool's depth (τ_slow 1 h).
        crank_every(&mut env, &rm, 60, 180);
        // A position of 0.43 % of the conservative `X₀` — which the locked depth bounds and the
        // deposit did not raise (rule 5) — ≈ 0.11 SOL of notional: over `min_notional`, under
        // the fee-scaled caps (3·φ = 0.75 % of `X₀`, and the wallet's share of 7.5·φ). The
        // default `X/400` of so small a pool is under `min_notional`.
        let m = env.market(&rm.market());
        let (x, _) = rm.reserves(&env.svm).unwrap();
        let x0 = shortwin_math::caps::conservative_x0(
            x,
            m.liq_slow.get(),
            m.ema_price.get(),
            Some((m.pool_d.get(), env.config().params.pool_lock_min_bps)),
        )
        .unwrap();
        assert!(x0 < x, "sized on the locked depth, not the deposit");
        let mut b = env.borrower();
        let (_, nonce, _) = open_ok(&mut env, &rm, &mut b, x0 / 10_000 * 43);
        env.warp(2);
        env.crank(&rm);
        close_ok(&mut env, &rm, &b, nonce);
    }

    /// **M4-SEC-1 / M4-SEC-5 / M4R, inverted tripwire** (was
    /// `tripwire_lp_dilution_cancels_the_fast_path`, the implementer's report of the gap).
    /// `eligible()` no longer reads `λ_now` (anyone could fail it for as long as an unlocked
    /// deposit stayed), and an adoption reads no sample of the pool (M4R). With the fast path
    /// armed on a halted Fartcoin pool and an honest candidate in the window (a CPMM pool at 50 %
    /// of `pool_d`, λ 99.78 %), the attacker's transaction `[deposit as much LP as the pool holds
    /// (λ → 49.89 %), execute_pool_switch, withdraw it]` on the real ELF **adopts** the candidate
    /// (`PoolSwitched`, kind fast), with `L_ema` and the ring at the anchor `⌊D·10⁴/λ_min⌋` — not
    /// the doubled depth the execution saw — `L_slow` at most its locked depth and the state
    /// marked; the warm-up then seeds the pool's own depth. The candidate may not re-propose
    /// itself (`PoolNotDominant`). The same deposit left in place (the persistent form,
    /// `m4_sec_it::m4_sec_1_*`) cannot keep a window from opening either.
    #[test]
    fn lp_dilution_no_longer_cancels_the_fast_path() {
        use shortwin::events::PoolSwitched;
        let (mut env, rm, q) = setup(FART);
        let mut b = env.borrower();
        let (_, nonce, _) = open_ok(&mut env, &rm, &mut b, q);
        crank_every(&mut env, &rm, 1, 3);
        halt(&mut env, &rm.pool);
        let lam = lambda_of(&env, &rm.pool);
        let cand = plant(&mut env, &rm.pool, Plant::Cpmm, true, (50, 100), lam);
        let AnyPool::Cpmm(cp) = cand else {
            unreachable!()
        };
        crank_every(&mut env, &rm, 30, 5);
        let since = env.market(&rm.market()).pool_fail_since;
        assert!(since > 0);
        window_ok(&mut env, &rm.pool, &cand);
        let add = cp.state(&env.svm).lp_supply;
        let (sol, tok) = lp::needed(&env, &cp, add);
        let attacker = lp::provider(&mut env, &cp, sol + 1_000, tok + 1_000, 0);
        let signer = attacker.owner.insecure_clone();
        let (mint, _) = rm.pool.token();
        let inc_read = candidate_read_set(&env.svm, &rm.pool, false);
        // Diluted, the window's candidate cannot re-record itself.
        let again = [
            lp::deposit_ix(&cp, &attacker, add),
            propose_pool_ix(&env.svm, &mint, &inc_read, &cand),
        ];
        assert_err(&env.send(&again, &[&signer]), E::PoolNotDominant);
        crank_every(&mut env, &rm, 30, 2);
        let d = d_of(&env, &cand);
        let live = {
            let (x, y) = cand.io_reserves(&env.svm, Direction::Sell).unwrap();
            (u128::from(x) * u128::from(y)).isqrt()
        };
        let before = env.market(&rm.market());
        let ixs = [
            lp::deposit_ix(&cp, &attacker, add),
            execute_pool_switch_ix(&env.svm, &mint, &rm.pool, &cand),
            lp::withdraw_ix(&cp, &attacker, add),
        ];
        let meta = ok(env.send(&ixs, &[&signer]));
        let e: PoolSwitched = events::one(&meta);
        assert_eq!((e.kind, e.candidate), (POOL_KIND_FAST, cp.pool));
        assert_eq!(lambda_of(&env, &cand), lam, "λ restored");
        let m = env.market(&rm.market());
        let l = m.ema_liq.get();
        let lambda_min = u64::from(env.config().params.pool_lock_min_bps);
        assert_eq!(
            l,
            shortwin_math::pool_select::depth_at_lambda(d, lambda_min),
            "the anchor"
        );
        assert!(l < 2 * live, "L_ema {l} vs √(X·Y) {live}: not doubled");
        assert_eq!((m.ema_warm, m.ema_guard), (8, 1), "marked");
        assert_eq!(m.liq_slow.get(), d.min(before.liq_slow.get()));
        println!(
            "LP dilution: adopted at {} s of pool_fail with L_ema {l} (√(X·Y) {live}, D {d})",
            env.now() - since
        );
        let rm2 = rm.on_pool(&env.svm, cand);
        crank_every(&mut env, &rm2, 1, 9);
        let m = env.market(&rm.market());
        let l = m.ema_liq.get();
        assert_eq!((m.ema_warm, m.ema_guard), (0, 0), "confirmed at the gate");
        assert!(
            l <= live + 2 && l + 2 >= live,
            "L_ema {l} vs √(X·Y) {live}: the pool's own depth after the warm-up"
        );
        close_ok(&mut env, &rm2, &b, nonce);
    }

    /// **M4-SEC-3, inverted tripwire** (was
    /// `tripwire_a_round_trip_under_the_sol_floor_cancels_the_fast_path`). On a small market (a
    /// planted AMM v4 incumbent of ≈ 380 SOL) whose fast path recorded a genuine CPMM candidate
    /// of ≈ 115 SOL (30 % of `pool_d`), the attacker's `[sell tokens for all but 24.99 SOL of the
    /// candidate's reserve, execute_pool_switch, buy the tokens back]` on the real ELF no longer
    /// cancels the window: eligibility's floor is on the locked depth at the market's `P_ema`,
    /// which a swap does not move (`D` grows by the fee only), so the candidate is adopted. A
    /// proposal made inside such a round trip judges the window floor at `min(P_ema, P_cand)`
    /// (M4R-2): the dump only hurts that proposal (`PoolTooThin`); the keeper's plain proposal
    /// opens the window. The position is sized to fit the candidate (switch only if all fit: the
    /// default size, 0.25 % of the incumbent's `X`, is above a 30 % pool's cap), and the
    /// execution judges the fit on the terms the round trip cannot move.
    #[test]
    fn a_round_trip_under_the_sol_floor_no_longer_cancels_the_fast_path() {
        use shortwin::events::{PoolProposed, PoolSwitched};
        let (mut env, rm, q) = row_market(100);
        let lam = lambda_of(&env, &rm.pool);
        let cand = plant(&mut env, &rm.pool, Plant::Cpmm, true, (30, 100), lam);
        let mut b = env.borrower();
        let q = fit_q(&env, &cand, q);
        open_ok(&mut env, &rm, &mut b, q);
        crank_every(&mut env, &rm, 1, 3);
        halt(&mut env, &rm.pool);
        crank_every(&mut env, &rm, 30, 5);
        let min = env.config().params.pool_min_real_sol;
        let (_, y) = cand.io_reserves(&env.svm, Direction::Sell).unwrap();
        let out = y - (min - 1);
        let tokens_in = cand
            .quote_exact_out(&env.svm, Direction::Sell, out)
            .unwrap()
            .cost;
        let (mint, program) = cand.token();
        let t = fork::trader(&mut env, &mint, program, 0, 3 * tokens_in);
        let owner = t.owner.insecure_clone();
        let round_trip = |env: &Env, mid: Instruction| {
            let (_, y) = cand.io_reserves(&env.svm, Direction::Sell).unwrap();
            let out = y - (min - 1);
            let tokens_in = cand
                .quote_exact_out(&env.svm, Direction::Sell, out)
                .unwrap()
                .cost;
            let sell = cand.swap_ix(
                &env.svm,
                &owner.pubkey(),
                &t,
                Direction::Sell,
                Swap::ExactOut(tokens_in, out),
            );
            let buy = cand.swap_ix(
                &env.svm,
                &owner.pubkey(),
                &t,
                Direction::Buy,
                Swap::ExactIn(out, 0),
            );
            vec![sell, mid, buy]
        };
        // A proposal inside the dump round trip is refused (M4R-2: the floor at the candidate's
        // own, dumped price); a plain one opens the window.
        let inc_read = candidate_read_set(&env.svm, &rm.pool, false);
        let prop = propose_pool_ix(&env.svm, &mint, &inc_read, &cand);
        assert_err(
            &env.send(&round_trip(&env, prop), &[&owner]),
            E::PoolTooThin,
        );
        let w: PoolProposed = window_ok(&mut env, &rm.pool, &cand);
        assert_eq!(w.candidate, pool_key(&cand));
        crank_every(&mut env, &rm, 30, 2);
        // The execution inside the round trip adopts.
        let exec = execute_pool_switch_ix(&env.svm, &mint, &rm.pool, &cand);
        let meta = ok(env.send(&round_trip(&env, exec), &[&owner]));
        let e: PoolSwitched = events::one(&meta);
        assert_eq!((e.kind, e.candidate), (POOL_KIND_FAST, pool_key(&cand)));
        assert_eq!(pool_now(&env, &rm), pool_key(&cand));
    }
}

/// Flip-flopping between two near-equal pools (ARCH §2.6 rules 1–2, table rows "Proposal
/// spam" and "Wash trading to raise `D`"; V2-7; R-RAY §12.2).
mod hysteresis {
    use shortwin::events::POOL_KIND_LISTING_WINDOW;
    use shortwin_litesvm_tests::fork::Swap;
    use shortwin_math::fees::Direction;
    use shortwin_math::pool_select::dominates;

    use crate::common::*;
    use crate::support::*;

    /// Fartcoin listed on its main pool `A`; `B4` (AMM v4, +4 % of `D`) and `B6` (CPMM, WSOL
    /// as token 1, +6 %). Inside the listing window `B4` is refused and `B6` adopted (5 %).
    /// From then on neither direction clears the margin: thirty seconds of proposals of `A`
    /// (`D_B6/1.06`) and `B4` (`1.04/1.06`) every second are all `PoolNotDominant`, the pool
    /// and `pool_seen_at` never move (the window is not extended); after the window both stay
    /// refused under the 25 % margin.
    #[test]
    fn near_equal_pools_do_not_flip_flop() {
        let mut env = Env::ray_fork();
        let a = env.load_ray_pool(FART);
        ok(env.list_ray(&a, VENUE_HINT_V4));
        let seen = market_of(&env, &a).pool_seen_at;
        let lam = lambda_of(&env, &a);
        let d_a = d_of(&env, &a);
        let b4 = plant(&mut env, &a, Plant::V4, false, (1_040, 1_000), lam);
        let b6 = plant(&mut env, &a, Plant::Cpmm, false, (1_060, 1_000), lam);
        let d_b6 = d_of(&env, &b6);
        assert!(!dominates(d_of(&env, &b4), d_a, 500) && dominates(d_b6, d_a, 500));
        env.warp(5);
        assert_err(&env.propose(Some(&a), &b4), E::PoolNotDominant);
        let e = adopt_ok(&mut env, Some(&a), &b6);
        assert_eq!(e.kind, POOL_KIND_LISTING_WINDOW);
        let b6_key = shortwin_litesvm_tests::pool_select::pool_key(&b6);
        for s in 0..30 {
            env.warp(1);
            for p in [&a, &b4] {
                assert_err(&env.propose(Some(&b6), p), E::PoolNotDominant);
            }
            let m = market_of(&env, &a);
            assert_eq!(
                (pool_now_of(&env, &a), m.pool_seen_at),
                (b6_key, seen),
                "second {s}"
            );
            assert_eq!(
                m.pool_d.get(),
                d_b6,
                "second {s}: no fee growth without trades"
            );
        }
        let slot = env.clock().slot;
        let window = i64::from(env.config().params.pool_list_window_s);
        env.set_clock(seen + window, slot + 20_000);
        for p in [&a, &b4] {
            assert_err(&env.propose(Some(&b6), p), E::PoolNotDominant);
        }
        assert_eq!(pool_now_of(&env, &a), b6_key);
    }

    /// The pool the market of `pool`'s mint names now.
    fn pool_now_of(env: &Env, pool: &AnyPool) -> Pubkey {
        shortwin_litesvm_tests::pool_select::cached_pool(&env.svm, &pda::market(&pool.token().0).0)
    }

    /// V2-7 on the real AMM v4 ELF: flipping back costs wash-trading fees. A market listed on a
    /// planted AMM v4 pool `A` (Fartcoin's reserves / 100, ≈ 381 SOL) adopts `B` (+6 % of `D`,
    /// WSOL as the pc) inside its listing window. To flip back, `A` needs `D_A ≥ 1.05·D_B`
    /// (+11.3 %): with no LP to burn, the attacker wash-trades `A` — round trips of `Y/2` SOL
    /// bought and every token sold back — since AMM v4 keeps the whole trade fee in the vaults
    /// and `√(X·Y)` grows by ≈ `f·A/(2Y)` per trade. The measured cost (SOL lost, tokens back
    /// to zero) is `2·Y·ln(D₁/D₀)` within [0.9, 1.25] — ARCH §2.6's "≈ `2·Y·ln(1 + x)`"; about
    /// a quarter of the pool's SOL for one flip (24 % measured, the first-order figure 21.6 %:
    /// round trips of `Y/2` pay the fee on more than their first-order volume) — of which the
    /// burned LP's share `λ` stays in the pool whoever holds the rest. `A` is then adopted, still inside the original window
    /// (`pool_seen_at` kept: flips never delay opens).
    #[test]
    fn flipping_back_costs_wash_trading_fees() {
        let mut env = Env::ray_fork();
        let base = env.load_ray_pool(FART);
        let lam = lambda_of(&env, &base);
        let a = plant(&mut env, &base, Plant::V4, true, (1, 100), lam);
        ok(env.list_ray(&a, VENUE_HINT_V4));
        let seen = market_of(&env, &a).pool_seen_at;
        let b = plant(&mut env, &base, Plant::V4, false, (106, 10_000), lam);
        env.warp(1);
        let e = adopt_ok(&mut env, Some(&a), &b);
        assert_eq!(e.kind, POOL_KIND_LISTING_WINDOW);
        let d_b = d_of(&env, &b);
        let d0 = d_of(&env, &a);
        assert_err(&env.propose(Some(&b), &a), E::PoolNotDominant);
        let (_, y0) = a.io_reserves(&env.svm, Direction::Sell).unwrap();
        let (mint, program) = a.token();
        let t = fork::trader(&mut env, &mint, program, 100 * y0, 0);
        let sol0 = tokens(&env, &t.wsol);
        let mut trips = 0u32;
        while !dominates(d_of(&env, &a), d_b, 500) {
            let held = tokens(&env, &t.token);
            ok(fork::swap(
                &mut env,
                &a,
                &t,
                Direction::Buy,
                Swap::ExactIn(y0 / 2, 0),
            ));
            let got = tokens(&env, &t.token) - held;
            ok(fork::swap(
                &mut env,
                &a,
                &t,
                Direction::Sell,
                Swap::ExactIn(got, 0),
            ));
            trips += 1;
            assert!(trips < 2_000, "no convergence");
        }
        assert_eq!(tokens(&env, &t.token), 0);
        let cost = sol0 - tokens(&env, &t.wsol);
        let d1 = d_of(&env, &a);
        let predicted = 2.0 * y0 as f64 * (d1 as f64 / d0 as f64).ln();
        let ratio = cost as f64 / predicted;
        println!(
            "wash trading: D ×{:.4} in {trips} round trips of {:.1} SOL: cost {:.2} SOL = {:.1} % of Y; 2·Y·ln(D₁/D₀) = {:.2} SOL (ratio {ratio:.3})",
            d1 as f64 / d0 as f64,
            (y0 / 2) as f64 / 1e9,
            cost as f64 / 1e9,
            cost as f64 * 100.0 / y0 as f64,
            predicted / 1e9
        );
        assert!(
            (0.9..=1.25).contains(&ratio),
            "cost / 2·Y·ln(1 + x) = {ratio}"
        );
        env.warp(1);
        let e = adopt_ok(&mut env, Some(&b), &a);
        assert_eq!(e.kind, POOL_KIND_LISTING_WINDOW);
        assert_eq!(
            market_of(&env, &a).pool_seen_at,
            seen,
            "the window is not extended"
        );
    }
}

/// Shared scenario: a Fartcoin market with an open position whose pool is then halted.
mod scenario {
    use crate::common::*;
    use crate::support::*;

    /// `(env, market, borrower, nonce)`: Fartcoin listed on its main pool, five seconds of
    /// cranks, one position of the default size, three more seconds, then the pool halted
    /// (AMM v4 status 2).
    pub fn halted_with_position() -> (Env, RayMarket, Borrower, u64) {
        let (mut env, rm, q) = setup(FART);
        crank_every(&mut env, &rm, 1, 5);
        let mut b = env.borrower();
        let (_, nonce, _) = open_ok(&mut env, &rm, &mut b, q);
        crank_every(&mut env, &rm, 1, 3);
        halt(&mut env, &rm.pool);
        (env, rm, b, nonce)
    }

    /// A market on Fartcoin's main pool (`scale` 1) or on a planted AMM v4 pool with its
    /// reserves divided by `scale` (its λ; ≈ 3,800 SOL for 10, 380 SOL for 100), after its
    /// listing window, and its default size `q`.
    pub fn row_market(scale: u64) -> (Env, RayMarket, u64) {
        if scale == 1 {
            return setup(FART);
        }
        let mut env = Env::ray_fork();
        let base = env.load_ray_pool(FART);
        let lam = lambda_of(&env, &base);
        let pool = plant(&mut env, &base, Plant::V4, true, (1, scale), lam);
        ok(env.list_ray(&pool, VENUE_HINT_V4));
        let q = q_of(&env, &pool);
        let rm = env.ray_market_listed(RaySpec::new(FART, q.saturating_mul(40)), pool);
        env.crank(&rm);
        (env, rm, q)
    }
}

/// Front-running the fast path with a thin or partially-unlocked pool (ARCH §2.6 rule 3 and
/// table row "Front-running the fast path…"; V2-7).
mod front_run {
    use shortwin::events::POOL_KIND_FAST;
    use shortwin_litesvm_tests::engine::liquidate_ix;
    use shortwin_litesvm_tests::pool_select::pool_key;
    use shortwin_math::fees::Direction;
    use shortwin_math::pool_select::fast_path_qualifies;

    use crate::common::*;
    use crate::lp;
    use crate::scenario::halted_with_position;
    use crate::support::*;

    /// The attacker proposes first, with a thin pool at the `pool_fast_min_bps` floor (25 %
    /// of `pool_d`): at 119 s of `pool_fail` it is refused (`PositionsOpen`: the fast path is
    /// not armed), at 120 s it opens the collection window. The keeper's genuine pool (60 %)
    /// replaces it; the attacker's partially-unlocked pool (λ 50 %, 1.1× Fartcoin's reserves:
    /// `D` ≈ 55 %) and the thin pool again are refused (`PoolNotDominant`); the execution
    /// adopts the genuine pool, and the position closes there. Front-running bought nothing.
    #[test]
    fn a_thin_front_run_loses_to_the_deepest_candidate() {
        let (mut env, rm, b, nonce) = halted_with_position();
        let lam = lambda_of(&env, &rm.pool);
        let pool_d = env.market(&rm.market()).pool_d.get();
        let thin = plant(&mut env, &rm.pool, Plant::V4, true, (2_505, 10_000), lam);
        let half = plant(
            &mut env,
            &rm.pool,
            Plant::Cpmm,
            false,
            (1_100, 1_000),
            5_000,
        );
        let genuine = plant(&mut env, &rm.pool, Plant::Cpmm, true, (60, 100), lam);
        let (d_thin, d_half, d_gen) = (d_of(&env, &thin), d_of(&env, &half), d_of(&env, &genuine));
        assert!(fast_path_qualifies(d_thin, pool_d, 2_500));
        assert!(!fast_path_qualifies(d_thin, pool_d, 2_600), "at the floor");
        assert!(d_thin < d_half && d_half < d_gen);
        env.warp(1);
        env.crank(&rm);
        let since = env.market(&rm.market()).pool_fail_since;
        crank_every(&mut env, &rm, 30, 3);
        env.warp(29);
        assert_err(&env.propose(Some(&rm.pool), &thin), E::PositionsOpen);
        env.warp(1);
        assert_eq!(env.now() - since, 120);
        let e = window_ok(&mut env, &rm.pool, &thin);
        let t0 = env.now();
        assert_eq!(e.candidate, pool_key(&thin));
        env.warp(5);
        window_ok(&mut env, &rm.pool, &genuine);
        env.warp(5);
        assert_err(&env.propose(Some(&rm.pool), &half), E::PoolNotDominant);
        assert_err(&env.propose(Some(&rm.pool), &thin), E::PoolNotDominant);
        let m = env.market(&rm.market());
        assert_eq!(
            (m.fp_keys[0], m.fp_d.get(), m.fp_t0),
            (pool_key(&genuine), d_gen, t0)
        );
        env.warp(t0 + 60 - env.now());
        let ev = execute_ok(&mut env, &rm.pool, &genuine).unwrap_or_else(|_| panic!("cancelled"));
        assert_eq!(
            (ev.kind, ev.candidate),
            (POOL_KIND_FAST, pool_key(&genuine))
        );
        let rm2 = rm.on_pool(&env.svm, genuine);
        env.warp(1);
        env.crank(&rm2);
        close_ok(&mut env, &rm2, &b, nonce);
    }

    /// An own pool that is the deepest wins — and keeps its locked half. The attacker's CPMM
    /// pool (λ = 50 %, 1.6× Fartcoin's reserves: `D` ≈ 80 % of `pool_d`) displaces the keeper's
    /// genuine 60 % pool in the window and is adopted. After the new listing window (and the
    /// keeper's observations at its end, which finish the post-change warm-up, M4R) a second
    /// position is sized on it; then the attacker withdraws every unlocked LP with the real
    /// ELF: the pool's reserves halve, `λ` becomes 100 %, `D` (and `pool_d`) stay, and the SOL
    /// left is at least `λ_min` of the reserve the second position was sized on (rule 5). No
    /// LP is left to withdraw (the locked half has no holder), neither position is
    /// liquidatable, and both close on the pool.
    #[test]
    fn a_partially_unlocked_winner_keeps_its_locked_half() {
        let (mut env, rm, b, nonce) = halted_with_position();
        let lam = lambda_of(&env, &rm.pool);
        let genuine = plant(&mut env, &rm.pool, Plant::V4, false, (60, 100), lam);
        let own = plant(&mut env, &rm.pool, Plant::Cpmm, true, (16, 10), 5_000);
        let AnyPool::Cpmm(op) = own else {
            unreachable!()
        };
        assert!(d_of(&env, &own) > d_of(&env, &genuine));
        crank_every(&mut env, &rm, 30, 5);
        window_ok(&mut env, &rm.pool, &genuine);
        env.warp(10);
        window_ok(&mut env, &rm.pool, &own);
        crank_every(&mut env, &rm, 10, 5);
        let pos1 = env.position(&rm, &b.key(), nonce);
        let ev = execute_ok(&mut env, &rm.pool, &own).unwrap_or_else(|_| panic!("cancelled"));
        assert_eq!(ev.candidate, op.pool);
        assert_eq!(env.position(&rm, &b.key(), nonce), pos1, "locked terms");
        let rm2 = rm.on_pool(&env.svm, own);
        env.warp(3_601 - raydium::PRICE_WARMUP_CRANKS);
        env.warm_up_price(&rm2);
        let q2 = q_for(&env, &rm2);
        let mut b2 = env.borrower();
        let (_, n2, _) = open_ok(&mut env, &rm2, &mut b2, q2);
        let (_, y_open) = own.io_reserves(&env.svm, Direction::Sell).unwrap();
        let d_open = env.market(&rm.market()).pool_d.get();
        // The pull.
        let counter = op.state(&env.svm).lp_supply;
        let circulating = raydium::mint_supply(&env.svm, &op.lp_mint);
        assert_eq!(circulating, counter / 2);
        let who = lp::provider(&mut env, &op, 0, 0, circulating);
        let signer = who.owner.insecure_clone();
        ok(env.send(&[lp::withdraw_ix(&op, &who, circulating)], &[&signer]));
        assert_eq!(lambda_of(&env, &own), 10_000);
        assert_eq!(
            raydium::mint_supply(&env.svm, &op.lp_mint),
            0,
            "no LP left to withdraw"
        );
        assert!(
            env.send(&[lp::withdraw_ix(&op, &who, 1)], &[&signer])
                .is_err()
        );
        let (_, y_after) = own.io_reserves(&env.svm, Direction::Sell).unwrap();
        let lam_min = u64::from(env.config().params.pool_lock_min_bps);
        assert!(
            y_after * 10_000 + 10_000 >= lam_min * y_open,
            "SOL left {y_after} vs λ_min of {y_open}"
        );
        env.warp(1);
        env.crank(&rm2);
        let d_after = env.market(&rm.market()).pool_d.get();
        assert!(
            d_after >= d_open && (d_after - d_open) * 10_000 <= d_open,
            "D {d_open} -> {d_after}"
        );
        crank_every(&mut env, &rm2, 1, 3);
        let keeper = env.funded_keypair();
        for (who_b, n) in [(&b, nonce), (&b2, n2)] {
            let ix = liquidate_ix(&rm2, &keeper.pubkey(), &who_b.key(), n);
            assert_err(&env.send(&[ix], &[&keeper]), E::NotLiquidatable);
        }
        close_ok(&mut env, &rm2, &b, nonce);
        close_ok(&mut env, &rm2, &b2, n2);
    }
}

/// Adoption griefing with a standing minimum position (ARCH §2.6 rule 2 "Griefing path
/// (disclosed; R4B-10)"; owner item O18).
mod griefing {
    use shortwin::PauseScope;
    use shortwin::events::POOL_KIND_ZERO_POSITION;
    use shortwin_litesvm_tests::engine::expire_ix;
    use shortwin_litesvm_tests::ix;

    use crate::common::*;
    use crate::support::*;

    /// The disclosed behaviour and its remedy, end to end on Fartcoin: a pool with twice the
    /// locked depth is proposed while the attacker's one minimum-size position (`N_open` just
    /// above `min_notional`) is open — `PositionsOpen`. At the deadline the attacker expires
    /// its own position and re-opens in the same transaction: still `PositionsOpen`. The
    /// Emergency vault pauses opens on the market (the existing power, SPEC §9): the same
    /// expire-and-reopen transaction now fails (`OpensPaused`), a keeper expires the position
    /// (the market drains within the longest open duration), and the dominant pool is adopted
    /// at `open_positions == 0`. Unpaused, opens wait the new listing window. The lender's
    /// tokens are all back in the pool vault once the market has drained (no lender funds at
    /// risk).
    #[test]
    fn a_standing_minimum_position_pins_the_pool_until_opens_are_paused() {
        let (mut env, rm, _) = setup(FART);
        let lam = lambda_of(&env, &rm.pool);
        let dominant = plant(&mut env, &rm.pool, Plant::V4, false, (2, 1), lam);
        let (x, y) = rm.reserves(&env.svm).unwrap();
        let q_min = (u128::from(x) * 105_000_000 / u128::from(y)) as u64;
        let min_notional = env.config().params.min_notional;
        let lent = tokens(&env, &rm.keys.pool_vault);
        let mut att = env.borrower();
        let (open, n1, _) = open_ok(&mut env, &rm, &mut att, q_min);
        assert!(open.n_open >= min_notional && open.n_open < min_notional * 11 / 10);
        env.warp(1);
        assert_err(&env.propose(Some(&rm.pool), &dominant), E::PositionsOpen);
        // Expire and re-open at the deadline, atomically.
        let reopen = |att: &Borrower, n: u64| {
            let open = open_short_ix(
                &rm,
                &att.key(),
                att.next_nonce,
                q_min,
                0,
                u64::MAX,
                u64::MAX,
                ExtrasSel::NONE,
                &[],
            );
            [expire_ix(&rm, &att.key(), &att.key(), n), open]
        };
        let pos = env.position(&rm, &att.key(), n1);
        let slot = env.clock().slot;
        env.set_clock(pos.deadline + 1, slot + 1_600_000);
        env.tick(&rm, 3);
        let kp = att.kp.insecure_clone();
        let ixs = reopen(&att, n1);
        ok(env.send(&ixs, &[&kp]));
        let n2 = att.next_nonce;
        att.next_nonce += 1;
        env.warp(1);
        assert_eq!(env.market(&rm.market()).open_positions, 1);
        assert_err(&env.propose(Some(&rm.pool), &dominant), E::PositionsOpen);
        // The remedy: Emergency pauses opens on the market.
        let ea = env.pause_authority.insecure_clone();
        ok(env.send(
            &[ix::set_pause(
                &ea.pubkey(),
                PauseScope::Market,
                true,
                Some(rm.market()),
            )],
            &[&ea],
        ));
        let pos2 = env.position(&rm, &att.key(), n2);
        let longest = shortwin_math::tier::TierParams::DEFAULTS
            .iter()
            .map(|t| i64::from(t.max_duration_s))
            .max()
            .unwrap();
        assert!(
            pos2.deadline - env.now() <= longest,
            "drains within the longest duration"
        );
        let slot = env.clock().slot;
        env.set_clock(pos2.deadline + 1, slot + 1_600_000);
        env.tick(&rm, 3);
        let ixs = reopen(&att, n2);
        assert_err(&env.send(&ixs, &[&kp]), E::OpensPaused);
        let keeper = env.funded_keypair();
        ok(env.send(
            &[expire_ix(&rm, &keeper.pubkey(), &att.key(), n2)],
            &[&keeper],
        ));
        assert_eq!(env.market(&rm.market()).open_positions, 0);
        assert!(
            tokens(&env, &rm.keys.pool_vault) >= lent,
            "the lender's tokens are back"
        );
        env.warp(1);
        let e = adopt_ok(&mut env, Some(&rm.pool), &dominant);
        assert_eq!(e.kind, POOL_KIND_ZERO_POSITION);
        ok(env.send(
            &[ix::set_pause(
                &ea.pubkey(),
                PauseScope::Market,
                false,
                Some(rm.market()),
            )],
            &[&ea],
        ));
        let rm2 = rm.on_pool(&env.svm, dominant);
        let mut c = env.borrower();
        assert_err(&env.open(&rm2, &mut c, q_min).0, E::PoolListWindow);
    }
}

/// Base/quote-swapped and non-WSOL candidates (ARCH §2.6 "Verification of a candidate", table
/// row "Mint on the wrong side, fake quote mint"; R-RAY §6.2).
mod sides {
    use anchor_lang::prelude::pubkey;
    use shortwin::constants::{RAYDIUM_V4_AUTHORITY, TOKEN_2022_PROGRAM_ID, WSOL_MINT};
    use shortwin::events::{POOL_KIND_LISTING, POOL_KIND_LISTING_WINDOW};
    use shortwin::state::MARKET_FLAG_SOL_IS_TOKEN0_OR_COIN;
    use shortwin_litesvm_tests::fork::{CpmmPool, V4Pool};
    use shortwin_litesvm_tests::pool_select::{PlantSpec, plant_cpmm, plant_v4, spec_like};
    use shortwin_litesvm_tests::tokens::{self as toks, TokenProgram};
    use shortwin_math::fees::Direction;
    use shortwin_math::num::WAD;
    use shortwin_venue_state::{raydium_cpmm as rc, raydium_v4 as rv};

    use crate::common::*;
    use crate::support::*;

    /// The Token-2022 native mint (never the quote: WSOL must be exactly `So111…112` under
    /// Tokenkeg).
    const NATIVE_MINT_2022: Pubkey = pubkey!("9pan9bMn5HatX4EJduexEVgjBc1BRbSd3wSsXhUKJT9U");

    fn put_key(env: &mut Env, pool: &Pubkey, off: usize, key: &Pubkey) {
        fork::patch(&mut env.svm, pool, |d| {
            d[off..off + 32].copy_from_slice(key.as_ref())
        });
    }

    /// A planted AMM v4 / CPMM pool of Fartcoin (a tenth of its reserves, its λ).
    fn v4(env: &mut Env, base: &AnyPool, sol_first: bool) -> V4Pool {
        let s = spec(env, base, sol_first);
        plant_v4(env, V4Fixture::Fartcoin, s)
    }

    fn cpmm(env: &mut Env, base: &AnyPool, sol_first: bool) -> CpmmPool {
        let s = spec(env, base, sol_first);
        plant_cpmm(env, CpmmFixture::Useless, s)
    }

    fn spec(env: &Env, base: &AnyPool, sol_first: bool) -> PlantSpec {
        let lam = lambda_of(env, base);
        spec_like(&env.svm, base, sol_first, lam).scaled(1, 10)
    }

    /// Every look-alike of a WSOL pool is refused (`PoolIneligible`) by a list-only market,
    /// which would adopt any eligible candidate: a 9-decimal fake WSOL and a 6-decimal
    /// stablecoin as the AMM v4 coin; the token on both sides; WSOL on both sides; the
    /// Token-2022 native mint as a CPMM token 0; WSOL under the Token-2022 program; AMM v4 and
    /// CPMM pools whose vault fields are swapped against their mint fields (the side a reader
    /// would take for SOL holds the token); a WSOL vault the pool authority does not own. The
    /// market stays list-only.
    #[test]
    fn look_alike_and_non_wsol_quotes_are_refused() {
        let mut env = Env::ray_fork();
        let base = env.load_ray_pool(FART);
        list_auto(&mut env, &base);
        let (mint, _) = base.token();
        let payer = env.payer.insecure_clone();
        let mut cases: Vec<(&str, AnyPool)> = Vec::new();
        for (what, decimals) in [("fake WSOL", 9u8), ("stablecoin", 6)] {
            let v = v4(&mut env, &base, true);
            let fake = toks::create_mint(
                &mut env.svm,
                &payer,
                &payer.pubkey(),
                decimals,
                TokenProgram::Spl,
            );
            let sol = fork::vault_amount(&env.svm, &v.coin_vault);
            let vault = fork::fund_token(
                &mut env,
                &RAYDIUM_V4_AUTHORITY,
                &fake,
                TokenProgram::Spl,
                sol,
            );
            put_key(&mut env, &v.pool, rv::OFF_COIN_VAULT_MINT, &fake);
            put_key(&mut env, &v.pool, rv::OFF_COIN_VAULT, &vault);
            cases.push((
                what,
                AnyPool::V4(V4Pool {
                    coin_vault: vault,
                    coin_mint: fake,
                    ..v
                }),
            ));
        }
        // The token on both sides.
        let v = v4(&mut env, &base, true);
        let tok = fork::vault_amount(&env.svm, &v.pc_vault);
        let vault = fork::fund_token(
            &mut env,
            &RAYDIUM_V4_AUTHORITY,
            &mint,
            TokenProgram::Spl,
            tok,
        );
        put_key(&mut env, &v.pool, rv::OFF_COIN_VAULT_MINT, &mint);
        put_key(&mut env, &v.pool, rv::OFF_COIN_VAULT, &vault);
        cases.push((
            "token/token",
            AnyPool::V4(V4Pool {
                coin_vault: vault,
                coin_mint: mint,
                ..v
            }),
        ));
        // WSOL on both sides.
        let v = v4(&mut env, &base, true);
        let sol = fork::vault_amount(&env.svm, &v.coin_vault);
        let vault = fork::fund_wsol(&mut env, &RAYDIUM_V4_AUTHORITY, sol);
        put_key(&mut env, &v.pool, rv::OFF_PC_VAULT_MINT, &WSOL_MINT);
        put_key(&mut env, &v.pool, rv::OFF_PC_VAULT, &vault);
        cases.push((
            "WSOL/WSOL",
            AnyPool::V4(V4Pool {
                pc_vault: vault,
                ..v
            }),
        ));
        // The Token-2022 native mint, and WSOL under Token-2022, as CPMM token 0.
        let c = cpmm(&mut env, &base, true);
        put_key(&mut env, &c.pool, rc::OFF_TOKEN_0_MINT, &NATIVE_MINT_2022);
        cases.push(("Token-2022 native mint", AnyPool::Cpmm(c)));
        let c = cpmm(&mut env, &base, true);
        put_key(
            &mut env,
            &c.pool,
            rc::OFF_TOKEN_0_PROGRAM,
            &TOKEN_2022_PROGRAM_ID,
        );
        cases.push(("WSOL under Token-2022", AnyPool::Cpmm(c)));
        // Vault fields swapped against the mint fields.
        let v = v4(&mut env, &base, true);
        put_key(&mut env, &v.pool, rv::OFF_COIN_VAULT, &v.pc_vault);
        put_key(&mut env, &v.pool, rv::OFF_PC_VAULT, &v.coin_vault);
        let swapped = V4Pool {
            coin_vault: v.pc_vault,
            pc_vault: v.coin_vault,
            ..v
        };
        cases.push(("AMM v4 vaults swapped", AnyPool::V4(swapped)));
        let c = cpmm(&mut env, &base, false);
        put_key(&mut env, &c.pool, rc::OFF_TOKEN_0_VAULT, &c.token_1_vault);
        put_key(&mut env, &c.pool, rc::OFF_TOKEN_1_VAULT, &c.token_0_vault);
        let swapped = CpmmPool {
            token_0_vault: c.token_1_vault,
            token_1_vault: c.token_0_vault,
            ..c
        };
        cases.push(("CPMM vaults swapped", AnyPool::Cpmm(swapped)));
        // A WSOL vault owned by the attacker, not the pool authority.
        let v = v4(&mut env, &base, false);
        let sol = fork::vault_amount(&env.svm, &v.pc_vault);
        let attacker = env.funded_keypair();
        let own = fork::fund_wsol(&mut env, &attacker.pubkey(), sol);
        put_key(&mut env, &v.pool, rv::OFF_PC_VAULT, &own);
        cases.push((
            "attacker's WSOL vault",
            AnyPool::V4(V4Pool { pc_vault: own, ..v }),
        ));
        let c = cpmm(&mut env, &base, true);
        let sol = fork::vault_amount(&env.svm, &c.token_0_vault);
        let own = fork::fund_wsol(&mut env, &attacker.pubkey(), sol);
        put_key(&mut env, &c.pool, rc::OFF_TOKEN_0_VAULT, &own);
        cases.push((
            "attacker's CPMM WSOL vault",
            AnyPool::Cpmm(CpmmPool {
                token_0_vault: own,
                ..c
            }),
        ));
        for (what, pool) in &cases {
            env.warp(1);
            let res = env.propose(None, pool);
            assert_eq!(custom_code(&res), Some(code(E::PoolIneligible)), "{what}");
        }
        assert_eq!(market_of(&env, &base).venue_kind(), Some(VenueKind::None));
    }

    /// Both orders of each program are adopted with the SOL side their verified mint fields
    /// give (the list-only market's first pool: AMM v4 with WSOL as the pc; then, inside the
    /// listing window at +10 % each, CPMM with WSOL as token 1, CPMM with WSOL as token 0, AMM
    /// v4 with WSOL as the coin): the SOL-side flag follows each pool, and `P_ema` — set by the
    /// first pool and carried over — equals every new pool's spot to 1 bp (a misread side would
    /// invert it). After the window a position opens and closes on the last pool.
    #[test]
    fn both_orders_are_adopted_on_their_sol_side() {
        let mut env = Env::ray_fork();
        let base = env.load_ray_pool(FART);
        list_auto(&mut env, &base);
        let lam = lambda_of(&env, &base);
        let steps = [
            (Plant::V4, false, 1_000u64, POOL_KIND_LISTING),
            (Plant::Cpmm, false, 1_100, POOL_KIND_LISTING_WINDOW),
            (Plant::Cpmm, true, 1_210, POOL_KIND_LISTING_WINDOW),
            (Plant::V4, true, 1_331, POOL_KIND_LISTING_WINDOW),
        ];
        let mut inc: Option<AnyPool> = None;
        for (kind, sol_first, scale, want) in steps {
            let p = plant(&mut env, &base, kind, sol_first, (scale, 10_000), lam);
            env.warp(10);
            let e = adopt_ok(&mut env, inc.as_ref(), &p);
            assert_eq!(e.kind, want, "{kind:?} sol_first {sol_first}");
            let m = market_of(&env, &p);
            assert_eq!(
                m.has_flag(MARKET_FLAG_SOL_IS_TOKEN0_OR_COIN),
                sol_first,
                "{kind:?}"
            );
            let (x, y) = p.io_reserves(&env.svm, Direction::Sell).unwrap();
            let spot = u128::from(y) * WAD / u128::from(x);
            let ema = m.ema_price.get();
            assert!(
                ema.abs_diff(spot) * 10_000 <= spot,
                "{kind:?}: P_ema {ema} vs spot {spot}"
            );
            inc = Some(p);
        }
        let (rm, q) = adopted_market(&mut env, inc.unwrap());
        let mut b = env.borrower();
        let (_, nonce, _) = open_ok(&mut env, &rm, &mut b, q);
        env.warp(2);
        env.crank(&rm);
        close_ok(&mut env, &rm, &b, nonce);
    }
}

/// Candidates while a Raydium program's upgrade is flagged (M3-SEC-2's latch extended to
/// adoptions and window candidates, on the candidate's own program since M4-SEC-8; the
/// engine's flagged deviation from ARCH §3.9; I17).
mod latched {
    use shortwin::constants::{GUARD_MASK_RAYDIUM, GUARD_RAYDIUM_CPMM, GUARD_RAYDIUM_V4};
    use shortwin::events::{
        POOL_KIND_FAST, POOL_KIND_LISTING_WINDOW, POOL_KIND_ZERO_POSITION, PoolSwitched,
    };
    use shortwin_litesvm_tests::fork::{FIXTURE_GUARD_SLOTS, RaydiumProgram};
    use shortwin_litesvm_tests::ix;
    use shortwin_litesvm_tests::pool_select::{pool_key, propose_pool_ix};
    use shortwin_litesvm_tests::raydium::candidate_read_set;

    use crate::common::*;
    use crate::scenario::halted_with_position;
    use crate::support::*;

    /// A CPMM upgrade not yet flagged: `[flag, propose a CPMM pool]` fails whole
    /// (`VenueUpgraded`), while `[propose, flag]` adopts first — the latch orders like opens
    /// (T-LATCH). Once flagged, every CPMM candidate is refused, on a Raydium market and on a
    /// list-only one, while an AMM v4 candidate is not held up (M4-SEC-8: a candidate waits for
    /// the review of its own program only — the one whose compiled reader verifies it); after
    /// `revalidate_venue` the CPMM candidates are adopted.
    #[test]
    fn candidates_wait_while_a_raydium_upgrade_is_flagged() {
        let (mut env, rm, _) = setup(FART);
        let useless = env.load_ray_pool(USELESS);
        list_auto(&mut env, &useless);
        let lam = lambda_of(&env, &rm.pool);
        let b = plant(&mut env, &rm.pool, Plant::Cpmm, false, (13, 10), lam);
        let c_v4 = plant(&mut env, &rm.pool, Plant::V4, true, (169, 100), lam);
        let c_cpmm = plant(&mut env, &rm.pool, Plant::Cpmm, true, (190, 100), lam);
        let dumped = FIXTURE_GUARD_SLOTS[usize::from(GUARD_RAYDIUM_CPMM)];
        fork::set_deploy_slot(&mut env.svm, RaydiumProgram::Cpmm, dumped + 1);
        let (mint, _) = rm.pool.token();
        let prefix = candidate_read_set(&env.svm, &rm.pool, false);
        let prop = propose_pool_ix(&env.svm, &mint, &prefix, &b);
        let flag = ix::flag_venue_upgraded(GUARD_RAYDIUM_CPMM);
        env.warp(1);
        assert_err(
            &env.send(&[flag.clone(), prop.clone()], &[]),
            E::VenueUpgraded,
        );
        assert!(
            !env.venue_guard().blocks(GUARD_MASK_RAYDIUM),
            "reverted whole"
        );
        let meta = ok(env.send(&[prop, flag], &[]));
        let e: PoolSwitched = events::one(&meta);
        assert_eq!(
            (e.kind, e.candidate),
            (POOL_KIND_ZERO_POSITION, pool_key(&b))
        );
        assert!(env.venue_guard().blocks(GUARD_MASK_RAYDIUM));
        env.warp(1);
        assert_err(&env.propose(Some(&b), &c_cpmm), E::VenueUpgraded);
        assert_err(&env.propose(None, &useless), E::VenueUpgraded);
        // The AMM v4 candidate: its program is not under review.
        let e = adopt_ok(&mut env, Some(&b), &c_v4);
        assert_eq!(e.kind, POOL_KIND_LISTING_WINDOW);
        let ea = env.pause_authority.insecure_clone();
        ok(env.send(
            &[ix::revalidate_venue(
                &ea.pubkey(),
                GUARD_RAYDIUM_CPMM,
                dumped + 1,
            )],
            &[&ea],
        ));
        env.warp(1);
        let e = adopt_ok(&mut env, Some(&c_v4), &c_cpmm);
        assert_eq!(e.kind, POOL_KIND_LISTING_WINDOW);
        adopt_ok(&mut env, None, &useless);
    }

    /// The latch holds an adoption, never a cancellation or an exit: with a window recorded
    /// and then an AMM v4 upgrade flagged, the execution after the window adopts nothing — a
    /// latched window candidate is not adoptable now, so the window is **cancelled**
    /// (`PoolSwitchCancelled`, window cleared, nothing adopted; M4V-4: M4-SEC-8's revert kept
    /// the window and froze every other proposal) while the chain stays armed; a new proposal
    /// of the flagged program's pool waits (`VenueUpgraded`), and the position closes while the
    /// flag is still up (exits never read the latch, I10).
    #[test]
    fn a_cancellation_is_not_latched() {
        let (mut env, rm, b, nonce) = halted_with_position();
        let lam = lambda_of(&env, &rm.pool);
        let cand = plant(&mut env, &rm.pool, Plant::V4, true, (50, 100), lam);
        crank_every(&mut env, &rm, 30, 5);
        window_ok(&mut env, &rm.pool, &cand);
        let dumped = FIXTURE_GUARD_SLOTS[usize::from(GUARD_RAYDIUM_V4)];
        fork::set_deploy_slot(&mut env.svm, RaydiumProgram::AmmV4, dumped + 1);
        ok(env.send(&[ix::flag_venue_upgraded(GUARD_RAYDIUM_V4)], &[]));
        crank_every(&mut env, &rm, 30, 2);
        let c = execute_ok(&mut env, &rm.pool, &cand)
            .map(|_| ())
            .expect_err("a latched window candidate cancels");
        assert_eq!((c.kind, c.candidate), (POOL_KIND_FAST, pool_key(&cand)));
        let m = env.market(&rm.market());
        assert_eq!(m.fp_t0, 0, "the window is cleared");
        assert!(m.pool_fail_since != 0, "the chain stays armed");
        assert_eq!(pool_now(&env, &rm), pool_key(&rm.pool));
        assert_err(&env.propose(Some(&rm.pool), &cand), E::VenueUpgraded);
        assert!(env.venue_guard().blocks(GUARD_MASK_RAYDIUM));
        resume(&mut env, &rm.pool);
        env.warp(1);
        env.crank(&rm);
        close_ok(&mut env, &rm, &b, nonce);
    }
}

/// Switching under open positions (ARCH §2.6 rules 3–4, §4.4 `rewrite_liquidity`; ECON §5.2,
/// §10.1 `venue_switch.py`, §10.3; A26, A72; T-VENUE-SWITCH).
mod mid_position {
    use shortwin::events::{Liquidate, POOL_KIND_FAST, PoolSwitched, SolSettle};
    use shortwin::state::ShortPosition;
    use shortwin_litesvm_tests::engine::{add_margin_ix, liquidate_ix, sol_settle_ix};
    use shortwin_litesvm_tests::fork::Swap;
    use shortwin_litesvm_tests::pool_select::{
        PlantSpec, execute_pool_switch_ix, pool_key, spec_like,
    };
    use shortwin_math::fees::Direction;
    use shortwin_math::num::WAD;
    use shortwin_math::observe::Ring;
    use shortwin_math::pool_select::fast_path_qualifies;
    use shortwin_math::settle;

    use crate::common::*;
    use crate::scenario::{halted_with_position, row_market};
    use crate::support::*;

    /// The spot price (WAD) of a pool.
    fn spot(env: &Env, pool: &AnyPool) -> u128 {
        let (x, y) = pool.io_reserves(&env.svm, Direction::Sell).unwrap();
        u128::from(y) * WAD / u128::from(x)
    }

    /// Over `secs` seconds (one crank of `rm` each), the seconds in which the settlement box
    /// is wider than `fv_dev_bps` in `L` (`settle::box_of` on the post-roll state): on the
    /// program's ring once its post-change warm-up is over (M4R: during it no decision reads the
    /// box, `sol_settle` evaluates nothing), and on `venue_switch.py`'s `CarryOver` replay — the
    /// ring `pre` held just before the change (the old pool's liquidity) with the program's new
    /// entries appended as they are integrated (their `P_ema`, `L_ema` and bucket extremes are
    /// the same in both runs: only the old entries' `l`, `llo`, `lhi` differ), every second.
    fn box_widths(
        env: &mut Env,
        rm: &RayMarket,
        pre: &Ring,
        fv_dev: u128,
        secs: u32,
    ) -> (u32, u32) {
        let mut cf = *pre;
        let mut last = pre.iter().map(|e| e.ts).max().unwrap();
        let (mut wide, mut wide_cf) = (0, 0);
        for _ in 0..secs {
            env.warp(1);
            env.crank(rm);
            let o = env.market(&rm.market()).observation();
            let mut fresh: Vec<_> = o.ring.iter().filter(|e| e.ts > last).copied().collect();
            fresh.sort_by_key(|e| e.ts);
            for e in fresh {
                last = e.ts;
                cf.push(e).unwrap();
            }
            let now = env.now();
            let too_wide = |r: &Ring| {
                let b = settle::box_of(&o.ema, r, now, true).unwrap();
                (b.l_hi - b.l_lo) * 10_000 > fv_dev * b.l_lo
            };
            if o.ema.warm == 0 {
                wide += u32::from(too_wide(&o.ring));
            }
            wide_cf += u32::from(too_wide(&cf));
        }
        (wide, wide_cf)
    }

    /// `venue_switch.py`'s fast-path rows ("halt → fast path to a 25 % pool, then pump 2×/120 s"
    /// on ECON's RA, RB and RC) on the fork, on Fartcoin's pool (≈ 38,000 SOL) and on planted
    /// AMM v4 pools of a tenth and a hundredth of it: the pool halts under an open position;
    /// after 120 s of `pool_fail` and
    /// the 60 s window the market adopts a CPMM pool at the `pool_fast_min_bps` floor (25 % of
    /// `pool_d`); over the first 60 s after the change the settlement box is never wider than
    /// `fv_dev_bps` in `L` once the post-change warm-up is over (M4R: the gate at +9 s; ECON
    /// §10.1's "0" after it), while the v0.3 carry-over
    /// replayed on the same ring is wide for 3–5 s (ECON: 4); the position is byte for byte
    /// what it was. Then real buys double the new pool's price over 120 s and a keeper tries
    /// `liquidate` every second: the outcome is a liquidation (as ECON's three rows), the
    /// lenders' tokens are all back in the pool vault, rent and the penalty share are theirs,
    /// and the waterfall conserves `V`.
    #[test]
    fn venue_switch_py_fast_path_rows_on_the_fork() {
        for (row, scale) in [("RA", 1u64), ("RB", 10), ("RC", 100)] {
            fast_path_row(row, scale);
        }
    }

    fn fast_path_row(row: &str, scale: u64) {
        let (mut env, rm, q) = row_market(scale);
        crank_every(&mut env, &rm, 1, 10);
        let lam = lambda_of(&env, &rm.pool);
        let cand = plant(&mut env, &rm.pool, Plant::Cpmm, true, (2_501, 10_000), lam);
        // Switch only if all fit: the default size (min(X/400, 2 SOL)) fits a 25 % pool on RA and
        // RB; on RC (≈ 380 SOL) 0.25 % of the incumbent's X is above the candidate's cap (0.75 %
        // of its conservative X₀), so the row carries 90 % of that cap.
        let q_row = fit_q(&env, &cand, q);
        println!(
            "{row}: default q {q}, carried q {q_row} ({:.0} % of the default)",
            q_row as f64 * 100.0 / q as f64
        );
        assert_eq!(q_row < q, row == "RC", "{row}");
        let q = q_row;
        let mut b = env.borrower();
        let (open, nonce, _) = open_ok(&mut env, &rm, &mut b, q);
        crank_every(&mut env, &rm, 1, 10);
        let pool_d = env.market(&rm.market()).pool_d.get();
        assert!(fast_path_qualifies(d_of(&env, &cand), pool_d, 2_500));
        assert!(
            !fast_path_qualifies(d_of(&env, &cand), pool_d, 2_510),
            "a 25 % pool"
        );
        let t_event = env.now();
        halt(&mut env, &rm.pool);
        crank_every(&mut env, &rm, 1, 121);
        window_ok(&mut env, &rm.pool, &cand);
        crank_every(&mut env, &rm, 1, 60);
        let pre = env.market(&rm.market()).ring_math();
        let pos0 = env.position(&rm, &b.key(), nonce);
        let ev = execute_ok(&mut env, &rm.pool, &cand).unwrap_or_else(|_| panic!("cancelled"));
        assert_eq!((ev.kind, ev.candidate), (POOL_KIND_FAST, pool_key(&cand)));
        let t_change = env.now();
        let rm2 = rm.on_pool(&env.svm, cand);
        let (wide, wide_cf) = box_widths(&mut env, &rm2, &pre, u128::from(pos0.fv_dev_bps), 60);
        println!(
            "{row} on the fork: switch {} s after the halt; seconds with the box > {} bps wide in L over the next 60 s: rewritten {wide}, carried over {wide_cf}",
            t_change - t_event,
            pos0.fv_dev_bps
        );
        assert_eq!(wide, 0, "{row}: the rewrite (v0.4.1)");
        assert!(
            (3..=5).contains(&wide_cf),
            "{row}: the carry-over: {wide_cf} s"
        );
        assert_eq!(env.position(&rm, &b.key(), nonce), pos0, "locked terms");
        // The pump: 2× over 120 s by real buys on the new pool; a keeper every second.
        let (mint, program) = cand.token();
        let p0 = spot(&env, &cand);
        let (_, y_c) = cand.io_reserves(&env.svm, Direction::Sell).unwrap();
        let whale = fork::trader(&mut env, &mint, program, y_c * 2, 0);
        let keeper = env.funded_keypair();
        let vault0 = tokens(&env, &rm.keys.pool_vault);
        let mut done: Option<(i64, Liquidate)> = None;
        let mut seen = std::collections::BTreeMap::new();
        for s in 1..=600i64 {
            env.warp(1);
            if s <= 120 {
                let target = p0 as f64 * (1.0 + s as f64 / 120.0);
                let (_, y) = cand.io_reserves(&env.svm, Direction::Sell).unwrap();
                let now_p = spot(&env, &cand) as f64;
                let a = y as f64 * ((target / now_p).sqrt() - 1.0) / 0.997;
                if a >= 1.0 {
                    ok(fork::swap(
                        &mut env,
                        &cand,
                        &whale,
                        Direction::Buy,
                        Swap::ExactIn(a as u64, 0),
                    ));
                }
            }
            env.crank(&rm2);
            let res = env.send(
                &[liquidate_ix(&rm2, &keeper.pubkey(), &b.key(), nonce)],
                &[&keeper],
            );
            match custom_code(&res) {
                None => {
                    done = Some((env.now(), events::one(&ok(res))));
                    break;
                }
                Some(c) if c == code(E::NotLiquidatable) || c == code(E::ExitBoundNotMet) => {
                    let e = seen.entry(c).or_insert((s, s));
                    e.1 = s;
                }
                Some(c) => panic!("{row}, second {s}: liquidate failed with {c}"),
            }
        }
        let (t_exit, e) = done.unwrap_or_else(|| panic!("{row}: never liquidated ({seen:?})"));
        println!(
            "{row} on the fork: liquidation {} s after the halt ({} s after the switch) at {:.2}× the switch price; lenders: rent {} + penalty share {} lamports",
            t_exit - t_event,
            t_exit - t_change,
            spot(&env, &cand) as f64 / p0 as f64,
            e.rent,
            e.penalty_lender
        );
        assert!(!e.expired, "{row}");
        assert_eq!(
            tokens(&env, &rm.keys.pool_vault) - vault0,
            q,
            "{row}: the lenders' tokens are back"
        );
        assert!(e.rent > 0 && e.penalty_lender > 0, "{row}");
        assert_eq!(
            e.buyback_cost + e.rent + e.penalty + e.keeper_topup + e.borrower_payout,
            open.v,
            "{row}: the waterfall conserves V"
        );
    }

    /// No forced exit through a switch: the attacker's own pool, priced at three times the
    /// incumbent (half its SOL against a sixth of its tokens: `D` ≈ 29 % of `pool_d`), is
    /// the only fast-path candidate. `[execute_pool_switch, liquidate]` fails whole
    /// (`EmaWarmingUp`: M4R's gate — the adoption restarted the observation state);
    /// `[execute_pool_switch, sol_settle]` switches and settles nothing (a no-op: the position is
    /// byte for byte what it was, `P_ema` unchanged). While the attacker then holds the 3× price
    /// (no arbitrage in the test), nothing exits during the post-change warm-up (8 integrations,
    /// ≥ `liq_persist_n`) and no settlement executes before `settle_persist_n` (40) EMA updates of
    /// an armed clock; after the gate the warm-up has seeded `P_ema` at the pool's own (held)
    /// price and the position is liquidated there — the held-model push of O18 residual 5 on a
    /// pool the fast path may make 4× thinner than the one the position was sized on, printed.
    #[test]
    fn an_attacker_cannot_force_an_exit_through_a_switch() {
        let (mut env, rm, b, nonce) = halted_with_position();
        let lam = lambda_of(&env, &rm.pool);
        let pool_d = env.market(&rm.market()).pool_d.get();
        let s = spec_like(&env.svm, &rm.pool, true, lam);
        let bad = plant_spec(
            &mut env,
            Plant::Cpmm,
            PlantSpec {
                sol: s.sol / 2,
                tokens: s.tokens / 6,
                ..s
            },
        );
        assert!(fast_path_qualifies(d_of(&env, &bad), pool_d, 2_500));
        let p_inc = spot(&env, &rm.pool);
        assert!(spot(&env, &bad) * 10 >= p_inc * 29, "priced at 3×");
        crank_every(&mut env, &rm, 30, 5);
        window_ok(&mut env, &rm.pool, &bad);
        crank_every(&mut env, &rm, 30, 2);
        let pos0 = env.position(&rm, &b.key(), nonce);
        let ema0 = env.market(&rm.market()).ema_price.get();
        let k = env.funded_keypair();
        let rm2 = rm.on_pool(&env.svm, bad);
        let (mint, _) = rm.pool.token();
        let exec = execute_pool_switch_ix(&env.svm, &mint, &rm.pool, &bad);
        let liq = liquidate_ix(&rm2, &k.pubkey(), &b.key(), nonce);
        assert_err(&env.send(&[exec.clone(), liq], &[&k]), E::EmaWarmingUp);
        assert_eq!(pool_now(&env, &rm), pool_key(&rm.pool), "reverted whole");
        let settle = sol_settle_ix(&env, &rm2, &k.pubkey(), &b.key(), nonce);
        let meta = ok(env.send(&[exec, settle], &[&k]));
        let e: PoolSwitched = events::one(&meta);
        assert_eq!((e.kind, e.candidate), (POOL_KIND_FAST, pool_key(&bad)));
        let m = env.market(&rm.market());
        assert_eq!((m.open_positions, m.ema_price.get()), (1, ema0));
        assert_eq!(env.position(&rm, &b.key(), nonce), pos0, "nothing settled");
        // The attacker holds the 3× price (nothing arbitrages it here); a keeper calls
        // `sol_settle` (which also cranks) and tries `liquidate` every second.
        let params = env.config().params;
        let (liq_n, settle_n) = (
            i64::from(params.liq_persist_n),
            i64::from(params.settle_persist_n),
        );
        let t_switch = env.now();
        let position = pda::position(&rm.market(), &b.key(), nonce).0;
        let (mut liquidatable, mut settled) = (None, None);
        for _ in 0..(settle_n + 30) {
            env.warp(1);
            let s = env.now() - t_switch;
            let meta = ok(env.send(
                &[sol_settle_ix(&env, &rm2, &k.pubkey(), &b.key(), nonce)],
                &[&k],
            ));
            if read::account(&env.svm, &position).is_none() {
                let e: SolSettle = events::one(&meta);
                settled = Some((s, e.condition));
                break;
            }
            let res = env.send(&[liquidate_ix(&rm2, &k.pubkey(), &b.key(), nonce)], &[&k]);
            match custom_code(&res) {
                None => {
                    ok(res);
                    liquidatable.get_or_insert(s);
                    settled = Some((s, 0));
                    break;
                }
                Some(c) if c == code(E::UseSolSettle) => {
                    liquidatable.get_or_insert(s);
                }
                Some(c)
                    if c == code(E::NotLiquidatable)
                        || c == code(E::ExitBoundNotMet)
                        || c == code(E::EmaWarmingUp) => {}
                Some(c) => panic!("second {s}: liquidate failed with {c}"),
            }
        }
        println!(
            "held 3× on the attacker's adopted pool (no arbitrage): liquidatable after {liquidatable:?} s, settled (s, condition) {settled:?} (liq_persist_n {liq_n}, settle_persist_n {settle_n})"
        );
        // The switch's own `sol_settle` recorded the new pool in the switch's second, so the
        // eighth warm-up integration (the gate) is at +8 s.
        let gate = i64::from(shortwin_math::consts::EMA_WARMUP_BUCKETS);
        assert!(
            liquidatable.is_none_or(|s| s >= liq_n.max(gate)),
            "liquidatable after {liquidatable:?} s"
        );
        assert!(
            settled.is_none_or(|(s, c)| s >= settle_n || (c == 0 && s >= gate)),
            "settled after {settled:?}"
        );
    }

    /// Two fast paths in a row under open positions — Fartcoin's AMM v4 pool → a CPMM pool
    /// with WSOL as token 1 (60 %) → an AMM v4 pool with WSOL as the pc (40 % of that) — keep
    /// both positions byte for byte (A26) and `P_ema`, `ema_ts`, `ema_count` continuous at each
    /// adoption; the positions then take a top-up and close on the third pool.
    #[test]
    fn two_fast_paths_keep_locked_terms() {
        let (mut env, rm, q) = setup(FART);
        crank_every(&mut env, &rm, 1, 5);
        let mut b1 = env.borrower();
        let mut b2 = env.borrower();
        let (_, n1, _) = open_ok(&mut env, &rm, &mut b1, q);
        let (_, n2, _) = open_ok(&mut env, &rm, &mut b2, q / 2);
        crank_every(&mut env, &rm, 1, 3);
        let snap = |env: &Env| -> (ShortPosition, ShortPosition) {
            (
                env.position(&rm, &b1.key(), n1),
                env.position(&rm, &b2.key(), n2),
            )
        };
        let before = snap(&env);
        let lam = lambda_of(&env, &rm.pool);
        let pb = plant(&mut env, &rm.pool, Plant::Cpmm, false, (60, 100), lam);
        let pc = plant(&mut env, &rm.pool, Plant::V4, false, (24, 100), lam);
        let ema = |env: &Env| {
            let m = env.market(&rm.market());
            (m.ema_price.get(), m.ema_ts, m.ema_count)
        };
        let mut cur = rm.on_pool(&env.svm, rm.pool);
        for next in [pb, pc] {
            halt(&mut env, &cur.pool);
            crank_every(&mut env, &cur, 30, 5);
            window_ok(&mut env, &cur.pool, &next);
            crank_every(&mut env, &cur, 30, 2);
            let e0 = ema(&env);
            let ev = execute_ok(&mut env, &cur.pool, &next).unwrap_or_else(|_| panic!("cancelled"));
            assert_eq!(ev.candidate, pool_key(&next));
            assert_eq!(ema(&env), e0, "EMA continuity");
            assert_eq!(snap(&env), before, "locked terms");
            cur = cur.on_pool(&env.svm, next);
            crank_every(&mut env, &cur, 1, 5);
        }
        let kp = b1.kp.insecure_clone();
        ok(env.send(&[add_margin_ix(&cur, &b1.key(), n1, 10_000_000)], &[&kp]));
        close_ok(&mut env, &cur, &b1, n1);
        close_ok(&mut env, &cur, &b2, n2);
        assert_eq!(env.market(&rm.market()).open_positions, 0);
    }
}
