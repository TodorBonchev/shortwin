//! T-TIER: the tier-transition matrix (ARCH §4.7, §3.4 step 5, §3.9 `crank_price`, §8.2; ECON
//! §6.2–§6.3; SPEC §4; ADOPTED Q8, Q18, Q19; O13; A26; M6R-1 / M6D-1), per venue, through real
//! instructions: the SOL/USD price moves through **real `crank_sol_usd`** calls on a planted Pyth
//! `PriceUpdateV2` (the mainnet dump re-published with another price and publish time), every
//! step within `oracle_max_step_bps`, and each tier is the one `crank_price` (or `open_short`)
//! caches on the market.
//!
//! Every threshold is recomputed here, independently of the program's glue, from the market's
//! stored observation state and the venue's live state: `Λ = 2·R_eff` with
//! `shortwin_math::tier::liquidity_lamports` on `R_now`, `Y(reprice(L_slow, P_ema))`, `max(VQR +
//! buckets, 0)` (PumpSwap's boost; PUMP-2026-10-1) and the venue's locked-depth bound (Raydium
//! `D/λ_min`, PumpSwap the burned LP at 100 %), and the exact crossing price: the least published
//! `p*` whose conservative `p* − conf` (the low end of Pyth's confidence interval, M7-SEC-7; the
//! fixture publishes 1 bp) reaches `⌈θ_usd_e6·10¹¹/Λ⌉` (Pyth `expo = −8`: `Λ·(p − conf)·10^(expo+6)
//! ≥ θ·10⁹`). Within one clock second the observation state does not move (a second's bucket is
//! integrated only in the next second), so `Λ` is fixed while the oracle crosses `p* − 1 → p* → p*
//! − 1 → p*`.
//!
//! | Test | What |
//! |---|---|
//! | `mock_usd_thresholds` | θ_A and θ_B crossed both ways at exactly `p*` (a walk from $121.20 in ≤ 19 % steps); `tier_computed_at` and `PriceCranked.tier` follow; the 24 h and 7 d age steps at a price above θ_A |
//! | `mock_staleness_boundary` | the cache at exactly `oracle_stale_s` (600 s since the owner's decision of 2026-10-01, OWNER-M7-15; was 3,600) still prices; one second later the market is Tier C, and an open there locks Tier C terms; a fresh update restores the tier |
//! | `mock_oracle_feed_change_empties_the_cache` | an applied `OracleFeed` change empties the cache: Tier C until the first `crank_sol_usd`, which the step rule does not hold back |
//! | `mock_non_positive_or_empty_price` | an empty or non-positive cached price is Tier C (a state `crank_sol_usd` itself refuses to write: `OracleConfidence`) |
//! | `mock_liquidity_crossings` | Λ crossings through the pool: an LP deposit lifts the tier only as `L_slow` follows it (the lag, exact to the second), a withdrawal drops it at once (`R_now`) |
//! | `mock_open_positions_keep_their_terms` | A26: positions opened in B, then A, then C (stale) keep their tier's terms while the market's tier moves |
//! | `amm_v4_age_and_thresholds` | Fartcoin: age from the listing (O13) — C to 24 h, B to 7 d, then A; θ_A and θ_B crossed exactly at `p*` of the `D/λ_min`-bounded Λ |
//! | `cpmm_age_from_listing` | a CPMM pool whose `open_time` precedes `T_CLAMP`: age from the listing (O13) — C to 24 h, B to 7 d, then A |
//! | `cpmm_thresholds` | USELESS (age from `open_time`, O13b: A at once): θ_A and θ_B crossed exactly |
//! | `pumpswap_thresholds_bounded_by_the_burned_lp` | the mature pool (7 % third-party LP): Λ bounded by the burned LP's real SOL (M6R-1 / M6D-1) strictly below the unbounded Λ, and the crossing at `p*` of the bounded Λ — one lamport of price below it the tier stays down although the unbounded Λ would lift it; age from the listing |
//! | `pumpswap_thresholds_on_the_payable_sol_under_fee_buckets` | PUMP-2026-10-1: a pool carrying the 2026-10-02 redeploy's fee buckets (in the vault, off the VQR) crosses θ_B at `p*` of the payable Λ (`R_now = min(vault − buckets, vault + VQR)`, the boost `max(VQR + buckets, 0)` off `R_slow` and the burned LP's SOL), recomputed from the raw fields; at `p* − 1` and at the bucket-blind Λ's lower `p*` it stays C; on the mature pool the burned-LP bound binds, on the fresh graduate the payable SOL; an open at `p*` locks B |
//! | `pumpswap_open_tiers_before_its_own_sale` | FUZZ-M7-24H-1: a Tier B market whose `R_eff` is the live WSOL vault (a third party's sale below `P_ema`, within the open guard), the oracle at exactly `p*_B`: the open locks Tier B — the venue before its own sale (ARCH §3.4 step 5) — although the vault its sale leaves would tier C |
//! | `curve_then_graduate_ages_c_b_a` | a curve market is Tier D at any price; after its graduation onto PumpSwap it is C for 24 h, B until day 7, then A (the oracle above θ_A); the curve position keeps its Tier D terms throughout (A26) |

#![allow(clippy::result_large_err)]

#[allow(unused_imports)]
#[path = "engine_it/common.rs"]
mod ecommon;

#[allow(unused_imports)]
#[path = "ray_it/common.rs"]
mod rcommon;

#[allow(unused_imports)]
#[path = "pamm_it/common.rs"]
mod pcommon;

/// The oracle driver and the independent tier arithmetic.
mod t {
    use anchor_lang::prelude::{AccountInfo, Pubkey};
    use shortwin::constants::{PUMPSWAP_LOCK_MIN_BPS, PYTH_RECEIVER_ID, PYTH_SOL_USD_ACCOUNT};
    use shortwin::events::PriceCranked;
    use shortwin::state::{Market, VenueKind};
    use shortwin::venues::VenueState;
    use shortwin_litesvm_tests::engine::{VenueMarket, crank_price_ix};
    use shortwin_litesvm_tests::errors::ok;
    use shortwin_litesvm_tests::{Env, events, ix, raydium};
    use shortwin_math::quote::reprice;
    use shortwin_math::tier::{self, OraclePrice, Tier, TierInputs, TierVenue};
    use shortwin_venue_state::pyth::{self, PriceUpdate};
    use solana_account::Account;

    /// The mainnet `PriceUpdateV2` dump (`oracle_crank`'s fixture).
    const DUMP: &[u8] = include_bytes!("../../fixtures/accounts/pyth_sol_usd_shard0.bin");

    /// $121.20 in Pyth units (`expo = −8`).
    pub const USD_121: i64 = 12_120_000_000;
    /// $10⁷ per SOL: any pool of more than 0.05 SOL of Λ is above θ_A (a price chosen to make the
    /// tier the age's alone).
    pub const USD_HIGH: i64 = 1_000_000_000_000_000;

    /// The dump, re-published at `price` and `publish_time` (confidence 1 bp).
    pub fn update(price: i64, publish_time: i64, slot: u64) -> PriceUpdate {
        let u = pyth::read(&pyth::RECEIVER, DUMP).expect("dump");
        PriceUpdate {
            price,
            conf: u64::try_from(price / 10_000).unwrap_or(0).max(1),
            publish_time,
            prev_publish_time: publish_time - 1,
            posted_slot: slot,
            ..u
        }
    }

    /// Writes `u` at the configured Pyth account (owner: the receiver).
    pub fn put(env: &mut Env, u: &PriceUpdate) {
        let data = pyth::encode(u).to_vec();
        let lamports = env.svm.minimum_balance_for_rent_exemption(data.len());
        env.svm
            .set_account(
                PYTH_SOL_USD_ACCOUNT,
                Account {
                    lamports,
                    data,
                    owner: PYTH_RECEIVER_ID,
                    executable: false,
                    rent_epoch: 0,
                },
            )
            .expect("set price update");
    }

    /// The SOL/USD oracle as the tests drive it: the last price and publish time sent through
    /// `crank_sol_usd`.
    pub struct Usd {
        pub price: i64,
        pub publish: i64,
    }

    impl Usd {
        /// Publishes `price` at `publish` and cranks it into the cache (must succeed).
        fn crank(env: &mut Env, price: i64, publish: i64) {
            let slot = env.clock().slot;
            put(env, &update(price, publish, slot));
            ok(env.send(&[ix::crank_sol_usd(&PYTH_SOL_USD_ACCOUNT)], &[]));
            let c = env.oracle_cache();
            assert_eq!((c.price, c.expo, c.publish_time), (price, -8, publish));
        }

        /// A price published now into an empty or stale cache (no step rule then).
        pub fn fresh(env: &mut Env, price: i64) -> Self {
            let publish = env.now();
            Self::crank(env, price, publish);
            Self { price, publish }
        }

        /// Publishes `price` now; the cache must be older than `oracle_stale_s` or empty, or the
        /// move within the step rule.
        pub fn refresh(&mut self, env: &mut Env, price: i64) {
            self.publish = env.now().max(self.publish + 1);
            Self::crank(env, price, self.publish);
            self.price = price;
        }

        /// One update to `to`, published one second after the last one (possibly ahead of the
        /// clock, within `oracle_max_age_s`): the step rule `|to/price − 1| ≤ 20 %` must hold.
        pub fn step(&mut self, env: &mut Env, to: i64) {
            let max_step = i128::from(env.config().params.oracle_max_step_bps);
            let (p, q) = (i128::from(self.price), i128::from(to));
            assert!((q - p).abs() * 10_000 <= p * max_step, "step {p} → {q}");
            self.publish += 1;
            assert!(
                self.publish <= env.now() + i64::from(env.config().params.oracle_max_age_s),
                "the publish times ran ahead of the clock"
            );
            Self::crank(env, to, self.publish);
            self.price = to;
        }

        /// Steps of at most 19 % to `to`.
        pub fn walk(&mut self, env: &mut Env, to: i64) {
            while (i128::from(to) - i128::from(self.price)).abs() * 100
                > i128::from(self.price) * 19
            {
                let next = if to > self.price {
                    self.price / 100 * 119
                } else {
                    self.price / 100 * 81
                };
                self.step(env, next);
            }
            if to != self.price {
                self.step(env, to);
            }
        }
    }

    /// The program's own reader of the venue state on the ReadSet's accounts as the VM holds
    /// them (the inputs `R_now`, `VQR`; the program's reader, compiled for the host).
    pub fn host_state(env: &Env, mm: &impl VenueMarket) -> VenueState {
        let m = env.market(&mm.market());
        let kind = m.venue_kind().expect("venue");
        let mut accs: Vec<(Pubkey, u64, Vec<u8>, Pubkey)> = mm
            .read_set(false)
            .iter()
            .map(|a| {
                let acc = env.svm.get_account(&a.pubkey).unwrap_or_default();
                (a.pubkey, acc.lamports, acc.data, acc.owner)
            })
            .collect();
        let infos: Vec<AccountInfo> = accs
            .iter_mut()
            .map(|(k, l, d, o)| AccountInfo::new(k, false, false, l, d, o, false))
            .collect();
        let supply = raydium::mint_supply(&env.svm, &mm.mint());
        shortwin::venues::read_state_with(&m, kind, &infos, env.now(), Some(supply))
    }

    /// ARCH §4.7's tier venue of `m` (Raydium `D/λ_min`; PumpSwap the burned LP at
    /// `PUMPSWAP_LOCK_MIN_BPS`; the mock unlocked); `bounded = false` drops the locked-depth
    /// bound (the comparison of the PumpSwap test).
    fn venue(env: &Env, m: &Market, bounded: bool) -> Option<TierVenue> {
        let lock = env.config().params.pool_lock_min_bps;
        Some(match m.venue_kind()? {
            _ if !bounded => TierVenue::Unlocked,
            VenueKind::PumpSwap => TierVenue::PumpSwap {
                locked_depth: m.pool_d.get(),
                lambda_min_bps: PUMPSWAP_LOCK_MIN_BPS,
            },
            VenueKind::RaydiumV4 | VenueKind::RaydiumCpmm => TierVenue::Raydium {
                locked_depth: m.pool_d.get(),
                lambda_min_bps: lock,
            },
            VenueKind::Mock => TierVenue::Unlocked,
            _ => return None,
        })
    }

    /// `Λ = 2·R_eff` of `mm` now (ARCH §4.7), from the stored observation state and the venue's
    /// live state; `bounded` as [`venue`].
    pub fn lambda_with(env: &Env, mm: &impl VenueMarket, bounded: bool) -> u128 {
        let m = env.market(&mm.market());
        let st = host_state(env, mm);
        assert!(st.executable, "the venue executes");
        let p_ema = m.ema_price.get();
        let y_slow = reprice(m.liq_slow.get(), p_ema).map_or(0, |(_, y)| y);
        let vqr = st.pool.map_or(0, |p| p.virtual_part);
        let params = env.config().params;
        let inp = TierInputs {
            venue: venue(env, &m, bounded).expect("a tiered venue"),
            r_now: st.r,
            y_slow,
            vqr,
            p_ema,
            now: env.now(),
            graduated_at: m.graduated_at,
            oracle: OraclePrice::default(),
            oracle_stale_s: params.oracle_stale_s,
            theta_a_usd_e6: params.theta_a_usd_e6,
            theta_b_usd_e6: params.theta_b_usd_e6,
        };
        tier::liquidity_lamports(&inp)
            .expect("Λ")
            .expect("a pool venue")
    }

    /// [`lambda_with`] with the venue's bound.
    pub fn lambda(env: &Env, mm: &impl VenueMarket) -> u128 {
        lambda_with(env, mm, true)
    }

    /// The confidence [`update`] publishes with `price` (1 bp, at least 1).
    pub fn conf_of(price: i64) -> i64 {
        (price / 10_000).max(1)
    }

    /// Whether the published `price` (with [`conf_of`]) reaches `θ` (USD·10⁶) at `Λ`: the tier
    /// converts at the conservative `price − conf` (M7-SEC-7), `Λ·(p − conf) ≥ θ·10¹¹`.
    pub fn reaches(theta_usd_e6: u64, lambda: u128, price: i64) -> bool {
        let p = u128::try_from(price - conf_of(price)).unwrap_or(0);
        lambda * p >= u128::from(theta_usd_e6) * 100_000_000_000
    }

    /// The least published Pyth price (`expo = −8`, confidence [`conf_of`]) at which `Λ` reaches
    /// `θ` (USD·10⁶): the least `p` with `Λ·(p − conf(p)) ≥ θ·10¹¹` (M7-SEC-7: the tier reads the
    /// low end of the confidence interval).
    pub fn p_star(theta_usd_e6: u64, lambda: u128) -> i64 {
        let num = u128::from(theta_usd_e6) * 100_000_000_000;
        let q = i64::try_from(num.div_ceil(lambda)).expect("price");
        let mut p = q + conf_of(q);
        while reaches(theta_usd_e6, lambda, p - 1) {
            p -= 1;
        }
        while !reaches(theta_usd_e6, lambda, p) {
            p += 1;
        }
        p
    }

    /// `p*` of θ_A and θ_B for `mm` now.
    pub fn thresholds(env: &Env, mm: &impl VenueMarket) -> (i64, i64) {
        let p = env.config().params;
        let l = lambda(env, mm);
        (p_star(p.theta_a_usd_e6, l), p_star(p.theta_b_usd_e6, l))
    }

    /// `crank_price` now: the tier it cached (checked against `tier_computed_at` and the
    /// `PriceCranked` event).
    pub fn crank_tier(env: &mut Env, mm: &impl VenueMarket) -> Tier {
        let meta = ok(env.send(&[crank_price_ix(mm)], &[]));
        let ev: PriceCranked = events::one(&meta);
        let m = env.market(&mm.market());
        assert_eq!(m.tier_computed_at, env.now(), "tier_computed_at");
        assert_eq!(ev.tier, m.tier, "PriceCranked.tier");
        Tier::from_u8(m.tier).expect("tier")
    }

    /// The four crossings of `θ` at fixed `Λ` within the current second: `p* − 1` below, `p*`
    /// at, back and again, each through `crank_sol_usd` then `crank_price`; `Λ` checked
    /// unchanged. Returns `p*`.
    pub fn cross(
        env: &mut Env,
        mm: &impl VenueMarket,
        usd: &mut Usd,
        theta_usd_e6: u64,
        below: Tier,
        at: Tier,
    ) -> i64 {
        let l = lambda(env, mm);
        let p = p_star(theta_usd_e6, l);
        assert!(!reaches(theta_usd_e6, l, p - 1) && reaches(theta_usd_e6, l, p));
        usd.walk(env, p - 1);
        assert_eq!(crank_tier(env, mm), below, "at p* − 1 = {}", p - 1);
        for (price, want) in [(p, at), (p - 1, below), (p, at)] {
            usd.step(env, price);
            assert_eq!(crank_tier(env, mm), want, "at {price} (p* = {p})");
            assert_eq!(lambda(env, mm), l, "Λ is fixed within the second");
        }
        p
    }

    /// Warps to `t`, cranks, publishes a fresh price `mult`·`p*_A` (Λ ≥ θ_A with room) and
    /// returns the tier `crank_price` caches at `t`.
    pub fn tier_at_age(env: &mut Env, mm: &impl VenueMarket, usd: &mut Usd, t: i64) -> Tier {
        let now = env.now();
        assert!(t > now, "{t} > {now}");
        env.warp(t - now);
        crank_tier(env, mm);
        let (pa, _) = thresholds(env, mm);
        usd.refresh(env, pa.saturating_mul(2));
        crank_tier(env, mm)
    }
}

/// The mock venue.
mod mock {
    use shortwin::errors::ShortwinError as E;
    use shortwin::events::Open;
    use shortwin::state::{ConfigChange, OracleCache, TierParams};
    use shortwin_litesvm_tests::fixtures;
    use shortwin_math::tier::{AGE_A_S, AGE_C_S, Tier};

    use solana_signer::Signer;

    use crate::ecommon::*;
    use crate::t::{self, Usd, crank_tier};

    /// Tier `tier`'s terms of a position: `(cr_liq_bps, penalty_bps, max duration)`.
    fn terms_of(pos: &shortwin::state::ShortPosition) -> (u32, u32, i64) {
        (
            pos.cr_liq_bps,
            u32::from(pos.penalty_bps),
            pos.deadline - pos.t_open,
        )
    }

    /// The default terms of `tier`.
    fn terms(tier: Tier) -> (u32, u32, i64) {
        let tp = TierParams::defaults()[tier.index()];
        (tp.cr_liq_bps, tp.penalty_bps, i64::from(tp.max_duration_s))
    }

    /// A default mock market (Λ = 2,000 SOL: Tier B at $121.20), listed at `listed`.
    fn market(env: &mut Env) -> (MockMarket, i64) {
        let mm = env.mock_market(MarketSpec::default());
        let listed = env.market(&mm.market()).graduated_at;
        (mm, listed)
    }

    /// An open of `q` by a new borrower that must succeed: its event, the borrower and nonce.
    fn open(env: &mut Env, mm: &MockMarket, q: u64) -> (Open, Borrower, u64) {
        let mut b = env.borrower();
        let (ev, nonce, _) = open_ok(env, mm, &mut b, q);
        (ev, b, nonce)
    }

    #[test]
    fn mock_usd_thresholds() {
        let mut env = Env::test_venue();
        let (mm, listed) = market(&mut env);
        env.crank(&mm);
        let mut usd = Usd::fresh(&mut env, t::USD_121);
        // The age steps at a price above θ_A: C to 24 h, B to 7 d, then A.
        for (t, want) in [
            (listed + AGE_C_S - 1, Tier::C),
            (listed + AGE_C_S, Tier::B),
            (listed + AGE_A_S - 1, Tier::B),
            (listed + AGE_A_S, Tier::A),
        ] {
            assert_eq!(
                t::tier_at_age(&mut env, &mm, &mut usd, t),
                want,
                "age {}",
                t - listed
            );
        }
        // A new second, its observation state fixed by one crank; the price walked back to
        // $121.20 within the step rule (the cache is fresh).
        env.warp(1);
        env.crank(&mm);
        usd.walk(&mut env, t::USD_121);
        assert_eq!(crank_tier(&mut env, &mm), Tier::B, "Λ 2,000 SOL at $121.20");
        let p = env.config().params;
        let pa = t::cross(&mut env, &mm, &mut usd, p.theta_a_usd_e6, Tier::B, Tier::A);
        let pb = t::cross(&mut env, &mm, &mut usd, p.theta_b_usd_e6, Tier::C, Tier::B);
        assert!((pa - 5 * pb).abs() <= 5, "θ_A/θ_B = 5: {pa} / {pb}");
        // The mock's R_now is its SOL pricing reserve.
        let st = t::host_state(&env, &mm);
        assert_eq!(st.r, mock::reserves(&env.svm, mm.mock()).quote);
    }

    #[test]
    fn mock_staleness_boundary() {
        let mut env = Env::test_venue();
        let (mm, _) = market(&mut env);
        env.warp(AGE_A_S);
        env.crank(&mm);
        let (pa, pb) = t::thresholds(&env, &mm);
        // Mid-band (Tier B) whatever small drift Λ takes over the hour.
        let mid = pb * 2;
        assert!(mid < pa);
        let mut usd = Usd::fresh(&mut env, mid);
        let published = usd.publish;
        let stale_s = i64::from(env.config().params.oracle_stale_s);
        env.warp(stale_s - 1);
        env.crank(&mm);
        env.warp(1);
        assert_eq!(env.now() - published, stale_s);
        assert_eq!(
            crank_tier(&mut env, &mm),
            Tier::B,
            "exactly oracle_stale_s old: priced"
        );
        let (o, _, _) = open(&mut env, &mm, Q);
        assert_eq!(o.tier, Tier::B as u8);
        env.warp(1);
        assert_eq!(
            crank_tier(&mut env, &mm),
            Tier::C,
            "one second older: Tier C"
        );
        let (o, b, n) = open(&mut env, &mm, Q);
        assert_eq!(
            o.tier,
            Tier::C as u8,
            "the open locks Tier C terms (fail safe, Q8)"
        );
        let pos = env.position(&mm, &b.key(), n);
        assert_eq!(terms_of(&pos), terms(Tier::C));
        // A fresh update (the stale cache takes any price) restores the tier.
        usd.refresh(&mut env, mid);
        assert_eq!(crank_tier(&mut env, &mm), Tier::B);
    }

    #[test]
    fn mock_oracle_feed_change_empties_the_cache() {
        let mut env = Env::test_venue();
        let (mm, _) = market(&mut env);
        let change = ConfigChange::OracleFeed {
            account: shortwin::constants::PYTH_SOL_USD_ACCOUNT,
            feed_id: shortwin::constants::PYTH_SOL_USD_FEED_ID,
        };
        let g = env.governance.insecure_clone();
        ok(env.send(
            &[shortwin_litesvm_tests::ix::propose_config(
                &g.pubkey(),
                change.clone(),
            )],
            &[&g],
        ));
        env.warp(i64::from(env.config().timelock_s).max(AGE_A_S));
        env.crank(&mm);
        let (pa, pb) = t::thresholds(&env, &mm);
        let mid = pb * 2;
        let mut usd = Usd::fresh(&mut env, mid);
        assert_eq!(crank_tier(&mut env, &mm), Tier::B);
        ok(env.send(
            &[shortwin_litesvm_tests::ix::apply_config_for(&change)],
            &[],
        ));
        let c = env.oracle_cache();
        assert_eq!((c.price, c.conf, c.expo, c.publish_time), (0, 0, 0, 0));
        assert_eq!(
            crank_tier(&mut env, &mm),
            Tier::C,
            "the emptied cache is Tier C"
        );
        // The first update after the change is not held to the step rule (empty cache): 3×.
        usd.refresh(&mut env, pa * 3 / 2);
        assert_eq!(crank_tier(&mut env, &mm), Tier::A);
    }

    #[test]
    fn mock_non_positive_or_empty_price() {
        let mut env = Env::test_venue();
        let (mm, _) = market(&mut env);
        env.warp(AGE_A_S);
        env.crank(&mm);
        assert_eq!(env.oracle_cache().price, 0);
        assert_eq!(crank_tier(&mut env, &mm), Tier::C, "an empty cache");
        let (pa, _) = t::thresholds(&env, &mm);
        let _usd = Usd::fresh(&mut env, pa);
        assert_eq!(crank_tier(&mut env, &mm), Tier::A);
        // crank_sol_usd refuses a non-positive price.
        for price in [0, -1] {
            let u = t::update(price, env.now() + 1, env.clock().slot);
            t::put(&mut env, &u);
            assert_err(
                &env.send(
                    &[shortwin_litesvm_tests::ix::crank_sol_usd(
                        &shortwin::constants::PYTH_SOL_USD_ACCOUNT,
                    )],
                    &[],
                ),
                E::OracleConfidence,
            );
        }
        // A non-positive cached price (never written by the program) fails safe to C.
        for price in [0, -1, i64::MIN] {
            let mut c: OracleCache = env.oracle_cache();
            c.price = price;
            c.publish_time = env.now();
            fixtures::set_borsh(
                &mut env.svm,
                &pda::oracle_cache(),
                &c,
                OracleCache::SPACE,
                0,
            );
            assert_eq!(crank_tier(&mut env, &mm), Tier::C, "price {price}");
        }
    }

    /// An LP deposit doubling the pool lifts `R_now` at once but `R_slow` only as `L_slow`
    /// follows (τ = 1 h): the tier crosses θ_A exactly when the recomputed Λ does — seconds
    /// later, not at the deposit — and every second between, the cached tier is the recomputed
    /// one. A withdrawal of the deposit drops `R_now` and the tier at the next crank.
    #[test]
    fn mock_liquidity_crossings() {
        let mut env = Env::test_venue();
        let (mm, _) = market(&mut env);
        env.warp(AGE_A_S);
        env.tick(&mm, 5);
        let l0 = t::lambda(&env, &mm);
        let st0 = t::host_state(&env, &mm);
        assert_eq!(l0, u128::from(st0.r) * 2, "L_slow at the pool: R_now binds");
        let (pa, _) = t::thresholds(&env, &mm);
        // 1 % below θ_A at this Λ.
        let price = pa / 100 * 99;
        let usd = Usd::fresh(&mut env, price);
        assert_eq!(crank_tier(&mut env, &mm), Tier::B);
        let r = mock::reserves(&env.svm, mm.mock());
        ok(mock::add_liquidity(
            &mut env,
            mm.mock(),
            &mm.pool.mint_authority,
            r.base,
            r.quote,
        ));
        let theta_a = u128::from(env.config().params.theta_a_usd_e6) * 100_000_000_000;
        let mut flipped = None;
        for s in 1..=600u32 {
            env.warp(1);
            let tier = crank_tier(&mut env, &mm);
            let l = t::lambda(&env, &mm);
            let want = if l * u128::try_from(usd.price).unwrap() >= theta_a {
                Tier::A
            } else {
                Tier::B
            };
            assert_eq!(tier, want, "second {s}: Λ {l}");
            if s == 1 {
                assert!(l < l0 * 102 / 100, "R_slow lags: Λ {l} against {l0} before");
                assert_eq!(tier, Tier::B, "not at the deposit");
            }
            if tier == Tier::A {
                flipped = Some(s);
                break;
            }
        }
        let s = flipped.expect("L_slow reaches the deposit");
        eprintln!("mock: the deposit lifts the tier to A after {s} s (L_slow's lag)");
        assert!(s > 5, "the lag: A after {s} s");
        // The withdrawal of the deposit: R_now drops back below R_slow at once; the next crank
        // tiers B.
        let r2 = mock::reserves(&env.svm, mm.mock());
        ok(mock::remove_liquidity(
            &mut env,
            &mm.pool,
            r2.base - r.base,
            r2.quote - r.quote,
        ));
        env.warp(1);
        let tier = crank_tier(&mut env, &mm);
        let st = t::host_state(&env, &mm);
        let l = t::lambda(&env, &mm);
        assert_eq!(u128::from(st.r) * 2, l, "R_now binds");
        assert!(l <= l0, "Λ {l} back at most at {l0}");
        assert_eq!(tier, Tier::B);
    }

    /// A26: a position locks its tier's terms at open; later tier moves change the market's
    /// tier and new positions only.
    #[test]
    fn mock_open_positions_keep_their_terms() {
        let mut env = Env::test_venue();
        let (mm, _) = market(&mut env);
        env.warp(AGE_A_S);
        env.tick(&mm, 3);
        let (pa, pb) = t::thresholds(&env, &mm);
        let mut usd = Usd::fresh(&mut env, pb * 2);
        let (ob, bb, nb) = open(&mut env, &mm, Q);
        assert_eq!(ob.tier, Tier::B as u8);
        let pos_b = env.position(&mm, &bb.key(), nb);
        env.warp(1);
        env.crank(&mm);
        let (pa2, _) = t::thresholds(&env, &mm);
        usd.walk(&mut env, pa.max(pa2) * 11 / 10);
        assert_eq!(crank_tier(&mut env, &mm), Tier::A);
        let (oa, ba, na) = open(&mut env, &mm, Q);
        assert_eq!(oa.tier, Tier::A as u8);
        let pos_a = env.position(&mm, &ba.key(), na);
        let stale_s = i64::from(env.config().params.oracle_stale_s);
        env.warp(usd.publish + stale_s + 1 - env.now());
        assert_eq!(crank_tier(&mut env, &mm), Tier::C);
        let (oc, bc, nc) = open(&mut env, &mm, Q);
        assert_eq!(oc.tier, Tier::C as u8);
        let pos_c = env.position(&mm, &bc.key(), nc);
        for (pos, tier) in [(&pos_b, Tier::B), (&pos_a, Tier::A), (&pos_c, Tier::C)] {
            assert_eq!(pos.tier, tier as u8);
            assert_eq!(terms_of(pos), terms(tier));
        }
        assert_eq!(
            env.position(&mm, &bb.key(), nb),
            pos_b,
            "B's terms unchanged"
        );
        assert_eq!(
            env.position(&mm, &ba.key(), na),
            pos_a,
            "A's terms unchanged"
        );
    }
}

/// Raydium AMM v4 and CPMM.
mod raydium {
    use shortwin_math::tier::{AGE_A_S, AGE_C_S, Tier};

    use crate::rcommon::*;
    use crate::t::{self, Usd, crank_tier};

    /// Fartcoin's AMM v4 pool: age from the listing (O13); Λ (≈ 76,000 SOL, under the
    /// `D/λ_min` bound) crosses θ_A near $6.5 and θ_B near $1.3.
    #[test]
    fn amm_v4_age_and_thresholds() {
        let (mut env, rm, _q) = setup(FART);
        let listed = env.market(&rm.market()).graduated_at;
        let mut usd = Usd::fresh(&mut env, t::USD_121);
        for (at, want) in [
            (listed + AGE_C_S - 1, Tier::C),
            (listed + AGE_C_S, Tier::B),
            (listed + AGE_A_S - 1, Tier::B),
            (listed + AGE_A_S, Tier::A),
        ] {
            assert_eq!(t::tier_at_age(&mut env, &rm, &mut usd, at), want);
        }
        env.warp(1);
        env.crank(&rm);
        let p = env.config().params;
        let pa = t::cross(&mut env, &rm, &mut usd, p.theta_a_usd_e6, Tier::B, Tier::A);
        let pb = t::cross(&mut env, &rm, &mut usd, p.theta_b_usd_e6, Tier::C, Tier::B);
        eprintln!(
            "Fartcoin: Λ {} lamports, p*_A {pa}, p*_B {pb}",
            t::lambda(&env, &rm)
        );
    }

    /// A CPMM pool whose `open_time` precedes `T_CLAMP` proves no creation time (O13): it ages
    /// from its listing — C for 24 h, B until day 7, then A — like an AMM v4 pool.
    #[test]
    fn cpmm_age_from_listing() {
        let mut env = Env::ray_fork();
        let pool = env.load_ray_pool(USELESS);
        let AnyPool::Cpmm(p) = &pool else {
            unreachable!()
        };
        fork::set_cpmm_open_time(&mut env.svm, p, 1_600_000_000);
        ok(env.list_ray(&pool, USELESS.hint()));
        let q = q_of(&env, &pool);
        let rm = env.ray_market_listed(RaySpec::new(USELESS, q * 40), pool);
        let listed = env.market(&rm.market()).graduated_at;
        assert!(listed > 1_600_000_000, "aged from the listing");
        let mut usd = Usd::fresh(&mut env, t::USD_121);
        for (at, want) in [
            (listed + AGE_C_S - 1, Tier::C),
            (listed + AGE_C_S, Tier::B),
            (listed + AGE_A_S - 1, Tier::B),
            (listed + AGE_A_S, Tier::A),
        ] {
            assert_eq!(t::tier_at_age(&mut env, &rm, &mut usd, at), want);
        }
    }

    /// USELESS on CPMM: `open_time` long before the listing (O13b), so Tier A at once at a
    /// price above θ_A; the thresholds crossed exactly.
    #[test]
    fn cpmm_thresholds() {
        let (mut env, rm, _q) = setup(USELESS);
        env.warp(1);
        env.crank(&rm);
        let (pa, _) = t::thresholds(&env, &rm);
        let mut usd = Usd::fresh(&mut env, pa * 2);
        assert_eq!(crank_tier(&mut env, &rm), Tier::A, "aged from open_time");
        let p = env.config().params;
        t::cross(&mut env, &rm, &mut usd, p.theta_a_usd_e6, Tier::B, Tier::A);
        t::cross(&mut env, &rm, &mut usd, p.theta_b_usd_e6, Tier::C, Tier::B);
    }
}

/// PumpSwap and the curve.
mod pump {
    use shortwin::state::TierParams;
    use shortwin_litesvm_tests::pump::{CurveUser, PumpSpec};
    use shortwin_math::tier::{AGE_A_S, AGE_C_S, Tier};

    use crate::pcommon::*;
    use crate::t::{self, Usd, crank_tier};

    /// The mature pool carries 7 % third-party LP: Λ is bounded by the burned LP's real SOL
    /// (M6R-1; at 100 % since M6D-1), strictly below the unbounded `2·min(R_now, R_slow)`; θ_A
    /// and θ_B are crossed at `p*` of the bounded Λ — at the unbounded Λ's `p*`, which is
    /// lower, the tier has not moved. Age counts from the listing (O13).
    #[test]
    fn pumpswap_thresholds_bounded_by_the_burned_lp() {
        let (mut env, pm, _q) = setup(PammCoin::Mature);
        let listed = env.market(&pm.market()).graduated_at;
        let mut usd = Usd::fresh(&mut env, t::USD_121);
        for (at, want) in [
            (listed + AGE_C_S - 1, Tier::C),
            (listed + AGE_C_S, Tier::B),
            (listed + AGE_A_S - 1, Tier::B),
            (listed + AGE_A_S, Tier::A),
        ] {
            assert_eq!(t::tier_at_age(&mut env, &pm, &mut usd, at), want);
        }
        env.warp(1);
        env.crank(&pm);
        let bounded = t::lambda(&env, &pm);
        let unbounded = t::lambda_with(&env, &pm, false);
        assert!(
            bounded < unbounded,
            "the burned LP binds: {bounded} < {unbounded}"
        );
        let p = env.config().params;
        let loose = t::p_star(p.theta_a_usd_e6, unbounded);
        let tight = t::p_star(p.theta_a_usd_e6, bounded);
        assert!(loose < tight - 1);
        usd.walk(&mut env, loose);
        assert_eq!(
            crank_tier(&mut env, &pm),
            Tier::B,
            "the unbounded Λ's threshold lifts nothing"
        );
        t::cross(&mut env, &pm, &mut usd, p.theta_a_usd_e6, Tier::B, Tier::A);
        t::cross(&mut env, &pm, &mut usd, p.theta_b_usd_e6, Tier::C, Tier::B);
        eprintln!(
            "PumpSwap mature: Λ {bounded} (unbounded {unbounded}) lamports, p*_A {tight} \
             (unbounded {loose})"
        );
    }

    /// FUZZ-M7-24H-1 (I19): an open is tiered on the venue **before its own sale** (ARCH §3.4 step
    /// 5 precedes the sale of step 7; §4.7's `R_now` is the pool's real SOL then). A Tier B market
    /// (the fresh graduate, its boost waited out; age 1–7 d from the listing, O13) whose `R_eff`
    /// is `R_now` — a third party's sale moved the spot below `P_ema` within the open guard, so
    /// the live WSOL vault binds — at a SOL/USD price of exactly `p*_B` of that `Λ`: the open
    /// locks Tier B, although the WSOL the open's own sale leaves in the vault would tier the
    /// market C. (The harness's I19 read the vault after the instruction and failed this open;
    /// the fuzzer found it on a price-wrapped open.)
    #[test]
    fn pumpswap_open_tiers_before_its_own_sale() {
        let (mut env, pm, q) = setup(PammCoin::Fresh);
        let listed = env.market(&pm.market()).graduated_at;
        let mut usd = Usd::fresh(&mut env, t::USD_121);
        assert_eq!(
            t::tier_at_age(&mut env, &pm, &mut usd, listed + AGE_C_S + 3_600),
            Tier::B
        );
        crank_secs(&mut env, &pm, 2);
        // A third party sells into the pool in half-percent steps (within `open_ema_dev_bps`)
        // until the live WSOL vault is the binding term of `R_eff` (`Λ = 2·R_now`).
        let seller = PammUser::new(&mut env, &pm.pool, SOL);
        let base = pm.pool.reserves(&env.svm).expect("reserves").base;
        let mut binds = false;
        for _ in 0..12 {
            ok(seller.sell(&mut env, &pm.pool, base / 200, 0));
            let st = t::host_state(&env, &pm);
            if t::lambda(&env, &pm) == 2 * u128::from(st.r) {
                binds = true;
                break;
            }
        }
        assert!(binds, "R_now binds R_eff after the sale");
        // The oracle at exactly `p*_B` of `Λ` before the open (the same second: `Λ` is fixed).
        let theta_b = env.config().params.theta_b_usd_e6;
        let before = t::lambda(&env, &pm);
        let p_b = t::p_star(theta_b, before);
        usd.walk(&mut env, p_b);
        assert_eq!(t::lambda(&env, &pm), before, "Λ is fixed within the second");
        let vault_before = t::host_state(&env, &pm).r;
        let mut b = env.borrower();
        let (open, _, _) = open_ok(&mut env, &pm, &mut b, q);
        assert_eq!(
            open.tier,
            Tier::B as u8,
            "tiered on the venue before the sale"
        );
        // The vault after the open's own sale: `Λ` on it is below `θ_B` at this price.
        let after = t::lambda(&env, &pm);
        let vault_after = t::host_state(&env, &pm).r;
        assert!(vault_after < vault_before && after < before);
        assert!(
            t::reaches(theta_b, before, p_b) && !t::reaches(theta_b, after, p_b),
            "Λ {before} → {after} straddles θ_B at p*_B {p_b}"
        );
        eprintln!(
            "PumpSwap fresh: R_now {vault_before} → {vault_after} (S {}), Λ {before} → {after} \
             lamports, p*_B {p_b}",
            open.s
        );
    }

    /// `Λ = 2·R_eff` of a PumpSwap market from the pool's raw fields and the stored observation
    /// state alone (never the program's reader): `(the payable reading, the bucket-blind one)`
    /// and the binding term of the first. Payable (PUMP-2026-10-1): `R_now = min(vault − buckets,
    /// vault + VQR)`, the virtual part the boost `max(VQR + buckets, 0)`; bucket-blind (the build
    /// before it): `R_now = min(vault, vault + VQR)`, `max(VQR, 0)`. Each is `2·min(R_now,
    /// Y(reprice(L_slow, P_ema)) − virtual, ⌊D·isqrt(P_ema)/10⁹⌋ − virtual)` (λ_min 100 %).
    fn bucket_lambdas(env: &Env, pm: &PammMarket) -> (u128, u128, &'static str) {
        let m = env.market(&pm.market());
        let pool = pm.pool.state(&env.svm);
        let vault = fork::vault_amount(&env.svm, &pm.pool.quote_vault);
        let vqr = pool.virtual_quote_reserves;
        let effective = u128::try_from(i128::from(vault) + vqr).expect("vault + VQR ≥ 0");
        let buckets = pool.protocol_fees + pool.creator_fees;
        let boost = u128::try_from((vqr + i128::from(buckets)).max(0)).expect("u128");
        let p_ema = m.ema_price.get();
        let y_slow = shortwin_math::quote::reprice(m.liq_slow.get(), p_ema).map_or(0, |(_, y)| y);
        let locked = shortwin_math::tier::locked_sol(m.pool_d.get(), p_ema);
        let terms = |r_now: u128, virt: u128| {
            [
                (r_now, "R_now"),
                (y_slow.saturating_sub(virt), "R_slow"),
                (locked.saturating_sub(virt), "locked"),
            ]
        };
        let payable = terms(u128::from(vault - buckets).min(effective), boost);
        let blind = terms(
            u128::from(vault).min(effective),
            u128::try_from(vqr.max(0)).expect("u128"),
        );
        let (r, binds) = payable.into_iter().min_by_key(|(r, _)| *r).expect("terms");
        let b = blind.into_iter().map(|(r, _)| r).min().expect("terms");
        (2 * r, 2 * b, binds)
    }

    /// PUMP-2026-10-1 (pump's 2026-10-02 redeploy; `pamm_it::buckets`): a PumpSwap market whose
    /// pool carries fee buckets (planted as a third party's `buy_v2` / `sell_v2` leave them: in
    /// the vault, off the VQR) crosses θ_B at `p*` of the payable `Λ` — the program's, equal to the
    /// one recomputed here from the raw fields — one lamport of price below it the tier stays C,
    /// and at the bucket-blind `Λ`'s (lower) `p*` too: the buckets lift nothing. Two pools, two
    /// binding terms: the mature pool's burned-LP bound (`locked − boost`) and, on the fresh
    /// graduate (wholly burned, its boost waited out), the payable SOL `vault − buckets`. An open
    /// at `p*` locks Tier B, and the hooks' I19 re-derives it from the view's payable inputs.
    #[test]
    fn pumpswap_thresholds_on_the_payable_sol_under_fee_buckets() {
        for (coin, binds) in [(PammCoin::Mature, "locked"), (PammCoin::Fresh, "R_now")] {
            let (mut env, pm, q) = setup(coin);
            let listed = env.market(&pm.market()).graduated_at;
            let mut usd = Usd::fresh(&mut env, t::USD_121);
            assert_eq!(
                t::tier_at_age(&mut env, &pm, &mut usd, listed + AGE_C_S + 3_600),
                Tier::B
            );
            crank_secs(&mut env, &pm, 2);
            let vqr = pm.pool.state(&env.svm).virtual_quote_reserves;
            let boost = u64::try_from(vqr).expect("a boosted pool");
            pamm::plant_buckets(&mut env.svm, &pm.pool, boost / 4, boost / 8);
            env.warp(1);
            env.crank(&pm);
            let (lambda, blind, bound) = bucket_lambdas(&env, &pm);
            assert_eq!(bound, binds, "{coin:?}: the binding term");
            assert_eq!(t::lambda(&env, &pm), lambda, "{coin:?}: the program's Λ");
            assert!(blind > lambda, "{coin:?}: {blind} > {lambda}");
            let theta_b = env.config().params.theta_b_usd_e6;
            let (tight, loose) = (t::p_star(theta_b, lambda), t::p_star(theta_b, blind));
            assert!(loose < tight - 1, "{coin:?}: {loose} < {tight} − 1");
            usd.walk(&mut env, loose);
            assert_eq!(
                crank_tier(&mut env, &pm),
                Tier::C,
                "{coin:?}: the bucket-blind Λ's threshold lifts nothing"
            );
            t::cross(&mut env, &pm, &mut usd, theta_b, Tier::C, Tier::B);
            assert_eq!(t::lambda(&env, &pm), lambda, "Λ is fixed within the second");
            let mut b = env.borrower();
            let (open, _, _) = open_ok(&mut env, &pm, &mut b, q);
            assert_eq!(open.tier, Tier::B as u8, "{coin:?}: the open at p*_B");
            eprintln!(
                "PumpSwap {coin:?} with buckets {} + {}: Λ {lambda} ({bound} binds; bucket-blind \
                 {blind}) lamports, p*_B {tight} (bucket-blind {loose})",
                boost / 4,
                boost / 8
            );
        }
    }

    /// A curve market (the near-complete fixture) is Tier D at any SOL/USD price; its position
    /// locks Tier D terms. It graduates (a real completing buy, `migrate_v2`, the transition in
    /// the next crank, `graduated_at = now`): with the oracle above θ_A it is Tier C until 24 h,
    /// B until 7 d, then A — and the curve position keeps its Tier D terms throughout (A26).
    #[test]
    fn curve_then_graduate_ages_c_b_a() {
        let (mut env, c) = Env::fork_pump_coin(fork::PumpCoin::Near);
        ok(env.list_pump(&c));
        let q = c.state(&env.svm).virtual_token_reserves / 200;
        let pm = env.pump_market_listed(PumpSpec::new(fork::PumpCoin::Near, q * 40), c);
        let mut usd = Usd::fresh(&mut env, t::USD_HIGH);
        env.warp(1);
        assert_eq!(crank_tier(&mut env, &pm), Tier::D, "the curve at any price");
        let mut b = env.borrower();
        let (res, n) = env.open(&pm, &mut b, q);
        let open: Open = events::one(&ok(res));
        assert_eq!(open.tier, Tier::D as u8);
        let pos_d = env.position(&pm, &b.key(), n);
        let d = TierParams::defaults()[Tier::D.index()];
        assert_eq!(
            (
                pos_d.cr_liq_bps,
                u32::from(pos_d.penalty_bps),
                pos_d.deadline - pos_d.t_open
            ),
            (d.cr_liq_bps, d.penalty_bps, i64::from(d.max_duration_s))
        );
        // Graduation.
        env.warp(1);
        let u = CurveUser::new(&mut env, &pm.curve);
        let rt = pm.curve.state(&env.svm).real_token_reserves;
        ok(u.buy(&mut env, &pm.curve, rt, u64::MAX));
        env.warp(1);
        venue_changed(
            &ok(env.send(&[crank_price_ix(&pm)], &[])),
            VenueKind::PumpCurve,
            VenueKind::PumpMigrating,
        );
        let migrator = env.funded_keypair();
        let ix = pump::migrate_v2_ix(&env.svm, &migrator.pubkey(), &pm.curve);
        ok(env.send(&[cu_limit_ix(600_000), ix], &[&migrator]));
        env.warp(1);
        venue_changed(
            &ok(env.send(&[crank_price_ix(&pm)], &[])),
            VenueKind::PumpMigrating,
            VenueKind::PumpSwap,
        );
        let graduated = env.market(&pm.market()).graduated_at;
        assert_eq!(graduated, env.now());
        let pam = PammMarket::graduated(&env.svm, &pm);
        let payer = env.payer.pubkey();
        let ixs = pamm::venue_account_ixs(&env.svm, &payer, &pam);
        ok(env.send(&ixs, &[]));
        crank_secs(&mut env, &pam, 9);
        assert_eq!(env.market(&pm.market()).ema_warm, 0);
        // The oracle far above θ_A: the tier is the age's.
        usd.refresh(&mut env, t::USD_HIGH);
        assert_eq!(crank_tier(&mut env, &pam), Tier::C, "graduated seconds ago");
        for (at, want) in [
            (graduated + AGE_C_S - 1, Tier::C),
            (graduated + AGE_C_S, Tier::B),
            (graduated + AGE_A_S - 1, Tier::B),
            (graduated + AGE_A_S, Tier::A),
        ] {
            assert_eq!(t::tier_at_age(&mut env, &pam, &mut usd, at), want);
        }
        assert_eq!(
            env.position(&pam, &b.key(), n),
            pos_d,
            "the curve position keeps its Tier D terms"
        );
    }
}
