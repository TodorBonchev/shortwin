//! T-CAP: the open caps at their exact boundaries (SPEC §4 "Size caps"; ARCH §3.4 step 6, §4.6;
//! ECON §6.1; N2; ADOPTED Q10; M5-SEC-3; M6R-1 / M6D-1): for each cap, on the mock and on the
//! real venues' forks, an open of `q` = the bound is accepted and one of bound + 1 is refused with
//! that cap's exact error.
//!
//! **The bounds are recomputed here from `shortwin_math::caps` on the pre-state**, independently
//! of the program's glue (`engine::caps`): `X₀ = caps::conservative_x0(X_now, L_slow, P_ema, (D,
//! λ_min))` — `λ_min` Raydium's `pool_lock_min_bps`, PumpSwap's `PUMPSWAP_LOCK_MIN_BPS` (100 %:
//! the burned LP only, M6D-1) — or on the curve the reserves at the EMA price (`X₀ = min(vT,
//! x_ema)`, `rT_guard = min(rT, x_ema − (vT − rT))` with `x_ema = reprice_token(√(vT·vQ),
//! P_ema)` whenever `P_ema > P_spot`, M5-SEC-3); then, in the program's order, the largest `q`
//! passing each rule: `max_q_position(X₀, ι_pos)` and `fee_size_cap(X₀, φ)`
//! (`CapPositionImpact`), `ΣB_Q + q ≤ oi_cap(X₀ + q, ι_mkt, φ)` (`CapMarketOi`), `util_ok`
//! (`CapUtilization`), `curve_gamma_ok(ΣB_Q, q, rT_guard, γ)` (`CapCurveGamma`), `wallet_ok(q_open,
//! q, oi_cap(X₀ + q))` (`CapWallet`). The binding cap is the smallest bound (ties: the first in
//! the program's order). The pre-state is fixed by a `crank_price` in the same clock second: a
//! second's observation bucket is integrated only in the next second, so `L_slow` and `P_ema`
//! are what the open's own prologue reads; `X_now`, `φ`, `D` and the curve are the venue's live
//! state (the program's reader, compiled for the host).
//!
//! Parameters are moved only within their compiled bounds, through `initialize_config` (which
//! runs `params::validate` and `validate_econ`, the checks `propose_config` runs): with the
//! default `wallet_cap_bps` (25 % of the per-market cap) the wallet cap binds before either
//! per-position cap for a fresh wallet (0.25·7.5·φ < 3·φ; 0.25·8.713 % < 2.470 %), so the
//! per-position and per-market boundaries are shown with `wallet_cap_bps` = 10,000; the notional
//! limits (`BelowMinNotional`, `MarketNotionalCap`) are `launch_guard_it`'s.
//!
//! | Test | What |
//! |---|---|
//! | `mock_fee_scaled_size` | 25 bps: `q ≤ 3·φ·X₀` |
//! | `mock_spec_position_impact` | 1 %: `q ≤ X₀·(√1.05 − 1)` |
//! | `mock_fee_scaled_oi_and_after_closes` | 25 bps: positions at their caps until `ΣB_Q + q ≤ 7.5·φ·X₁` binds; a close frees exactly its room |
//! | `mock_spec_market_impact` | 1.5 %: `ΣB_Q + q ≤ X₁·(1 − 1/√1.2)` |
//! | `mock_utilisation` | 90 % of the lent tokens |
//! | `mock_wallet_single_and_multi` | 25 % of the per-market cap: one wallet, the same wallet's second position, other wallets until the market fills |
//! | `m7_sec_9_one_actor_with_four_keys_fills_the_oi_cap` | M7-SEC-9: the wallet cap is per key — one actor funds four keys, each opens exactly its wallet cap, and the market is full (a fifth key `CapMarketOi`) |
//! | `curve_gamma_and_wallet` | the near-complete curve: a wallet at its cap, then Tier D's graduation guard `ΣB_Q + q ≤ γ·(rT_guard + q)` on the EMA-price reserves |
//! | `curve_position_impact_then_market` | a fresh curve: Tier D's `ι_pos` (3 %), then its `ι_mkt` (10 %) on the EMA-price reserves |
//! | `pumpswap_position_and_market` | the mature pool (`X₀` bounded by the burned LP): the per-position cap, then the per-market cap, and the wallet cap at the defaults |
//! | `raydium_position_market_and_wallet` | Fartcoin (AMM v4) and USELESS (CPMM), `X₀` bounded by `D/λ_min`: the fee-scaled size, the per-market cap, and the wallet cap at the defaults |

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

#[allow(unused_imports)]
#[path = "pump_it/common.rs"]
mod pumpc;

/// The independent bounds and the exact-boundary check.
mod b {
    use anchor_lang::prelude::{AccountInfo, Pubkey};
    use shortwin::constants::{PUMPSWAP_LOCK_MIN_BPS, VENUE_BIT_MOCK};
    use shortwin::errors::ShortwinError as E;
    use shortwin::events::Open;
    use shortwin::state::{Params, TierParams, VenueKind, WalletExposure};
    use shortwin::venues::VenueState;
    use shortwin_litesvm_tests::engine::{Borrower, VenueMarket, crank_price_ix};
    use shortwin_litesvm_tests::errors::{assert_err, ok};
    use shortwin_litesvm_tests::{Env, events, pda, raydium, read};
    use shortwin_math::caps;
    use shortwin_math::quote::{liquidity, reprice_token};

    /// The test-venue environment with `f` applied to the parameters (validated by
    /// `initialize_config`).
    pub fn mock_env(f: impl FnOnce(&mut Params)) -> Env {
        let mut env = Env::new_test_venue_program();
        env.load_mock_cpamm();
        let mut args = env.default_init_args();
        args.venue_enabled |= VENUE_BIT_MOCK;
        f(&mut args.params);
        ok(env.initialize_config(args));
        env.install_mock_extras();
        env
    }

    /// `wallet_cap_bps` = 10,000 (the per-market cap per wallet; within [500, 10,000]).
    pub fn wide_wallet(p: &mut Params) {
        p.wallet_cap_bps = 10_000;
    }

    /// The program's own reader on the ReadSet's accounts as the VM holds them.
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

    /// A cap of ARCH §3.4 step 6, in the program's order.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum Cap {
        PositionImpact,
        FeeSize,
        MarketOi,
        Utilisation,
        CurveGamma,
        Wallet,
        Inventory,
    }

    impl Cap {
        /// The error the program refuses it with.
        pub fn error(self) -> E {
            match self {
                Self::PositionImpact | Self::FeeSize => E::CapPositionImpact,
                Self::MarketOi => E::CapMarketOi,
                Self::Utilisation => E::CapUtilization,
                Self::CurveGamma => E::CapCurveGamma,
                Self::Wallet => E::CapWallet,
                Self::Inventory => E::InsufficientInventory,
            }
        }
    }

    /// The pre-state the open's caps read.
    #[derive(Clone, Copy)]
    pub struct Pre {
        pub x0: u64,
        /// Tier D's `rT_guard` (the curve), else `None`.
        pub rt_guard: Option<u64>,
        pub phi: u32,
        pub t: u64,
        pub b: u64,
        /// The borrower's `WalletExposure.q_open`.
        pub wq: u64,
        pub tier: TierParams,
        pub params: Params,
    }

    impl core::fmt::Debug for Pre {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            write!(
                f,
                "Pre {{ x0: {}, rt_guard: {:?}, phi: {}, t: {}, b: {}, wq: {}, ι_pos: {}, ι_mkt: {} }}",
                self.x0,
                self.rt_guard,
                self.phi,
                self.t,
                self.b,
                self.wq,
                self.tier.impact_pos_bps,
                self.tier.impact_mkt_bps
            )
        }
    }

    /// The pre-state of an open on `mm` by `borrower` now (after a `crank_price` this second).
    pub fn pre(env: &Env, mm: &impl VenueMarket, borrower: &Pubkey) -> Pre {
        let m = env.market(&mm.market());
        assert_eq!(
            m.b_sec,
            env.now(),
            "the second's state is fixed by an instruction in it"
        );
        let st = host_state(env, mm);
        assert!(st.executable);
        let config = env.config();
        let p_ema = m.ema_price.get();
        let kind = m.venue_kind().expect("venue");
        let (x0, rt_guard) = if kind == VenueKind::PumpCurve {
            let c = st.curve.as_ref().expect("curve").live;
            let (vt, vq, rt) = (c.vt, c.vq, c.rt);
            let p_spot = st.price().expect("spot");
            if p_ema <= p_spot {
                (vt, Some(rt))
            } else {
                let x_ema = reprice_token(liquidity(vt, vq).unwrap(), p_ema).unwrap();
                let x0 = vt.min(x_ema);
                let rt_g = rt.min(x_ema.saturating_sub(vt - rt));
                (x0, Some(rt_g))
            }
        } else {
            // `D`: the live pool's locked depth, which the open's own prologue writes into
            // `pool_d` before its caps read it (an earlier trade this second moved it by the fee
            // the pool kept).
            let lock = match kind {
                VenueKind::PumpSwap => Some((st.locked_depth, PUMPSWAP_LOCK_MIN_BPS)),
                VenueKind::RaydiumV4 | VenueKind::RaydiumCpmm => {
                    Some((st.locked_depth, config.params.pool_lock_min_bps))
                }
                _ => None,
            };
            let x0 = caps::conservative_x0(st.x, m.liq_slow.get(), p_ema, lock).unwrap();
            (u128::from(x0), None)
        };
        if std::env::var_os("CAPS_DEBUG").is_some() {
            eprintln!(
                "pre: kind {kind:?} x_now {} l_slow {} p_ema {p_ema} pool_d {} D {} phi {} x0 {x0} \
                 tier {} max_notional {} total {}",
                st.x,
                m.liq_slow.get(),
                m.pool_d.get(),
                st.locked_depth,
                st.fee_ppm,
                m.tier,
                config.params.market_max_notional,
                m.total_notional_open
            );
        }
        let wq = read::borsh::<WalletExposure>(&env.svm, &pda::exposure(&mm.market(), borrower).0)
            .map_or(0, |e| e.q_open);
        Pre {
            x0: u64::try_from(x0).unwrap(),
            rt_guard: rt_guard.map(|r| u64::try_from(r).unwrap()),
            phi: st.fee_ppm,
            t: m.tokens_free,
            b: m.borrowed_q,
            wq,
            tier: config.tiers[usize::from(m.tier)],
            params: config.params,
        }
    }

    /// The largest `q ∈ [0, hi]` with `pred(q)` (`pred` monotone, `pred(0)`).
    fn max_q(pred: impl Fn(u64) -> bool, hi: u64) -> u64 {
        assert!(pred(0));
        if pred(hi) {
            return hi;
        }
        let (mut lo, mut hi) = (0u64, hi);
        while hi - lo > 1 {
            let mid = lo + (hi - lo) / 2;
            if pred(mid) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        lo
    }

    /// `oi_cap(X₀ + q)` of `p`.
    fn oi_cap(p: &Pre, q: u64) -> u64 {
        caps::oi_cap(
            p.x0 + q,
            p.tier.impact_mkt_bps,
            p.phi,
            p.params.fee_oi_cap_x100,
        )
        .unwrap()
    }

    /// Every cap's largest passing `q`, in the program's order.
    pub fn bounds(p: &Pre) -> Vec<(Cap, u64)> {
        let hi = p.x0;
        let mut v = vec![
            (
                Cap::PositionImpact,
                caps::max_q_position(p.x0, p.tier.impact_pos_bps).unwrap(),
            ),
            (
                Cap::FeeSize,
                caps::fee_size_cap(p.x0, p.phi, p.params.fee_size_cap_x100)
                    .unwrap()
                    .unwrap_or(u64::MAX),
            ),
        ];
        let oi = if p.b <= oi_cap(p, 0) {
            max_q(|q| p.b + q <= oi_cap(p, q), hi)
        } else {
            0
        };
        v.push((Cap::MarketOi, oi));
        v.push((
            Cap::Utilisation,
            max_q(
                |q| caps::util_ok(p.t, p.b, q, p.params.util_cap_bps).unwrap(),
                p.t,
            ),
        ));
        if let Some(rt) = p.rt_guard {
            let ok0 = caps::curve_gamma_ok(p.b, 0, rt, p.params.curve_oi_gamma_bps).unwrap();
            let g = if ok0 {
                max_q(
                    |q| caps::curve_gamma_ok(p.b, q, rt, p.params.curve_oi_gamma_bps).unwrap(),
                    hi,
                )
            } else {
                0
            };
            v.push((Cap::CurveGamma, g));
        }
        let w_ok =
            |q: u64| caps::wallet_ok(p.wq, q, oi_cap(p, q), p.params.wallet_cap_bps).unwrap();
        v.push((Cap::Wallet, if w_ok(0) { max_q(w_ok, hi) } else { 0 }));
        v.push((Cap::Inventory, p.t));
        v
    }

    /// The binding cap: the smallest bound, the first in the program's order on a tie.
    pub fn binding(p: &Pre) -> (Cap, u64) {
        bounds(p)
            .into_iter()
            .fold(None, |acc: Option<(Cap, u64)>, (c, q)| match acc {
                Some((_, best)) if best <= q => acc,
                _ => Some((c, q)),
            })
            .expect("bounds")
    }

    /// `crank_price` one second later: the second's pre-state fixed.
    pub fn next_second(env: &mut Env, mm: &impl VenueMarket) {
        env.warp(1);
        ok(env.send(&[crank_price_ix(mm)], &[]));
    }

    /// In the current second: the binding cap of `b`'s next open on `mm` must be `expect`; an
    /// open of bound + 1 is refused with its error, then an open of the bound lands. Returns
    /// the bound and the `Open` event.
    #[track_caller]
    pub fn exact(
        env: &mut Env,
        mm: &impl VenueMarket,
        b: &mut Borrower,
        expect: Cap,
    ) -> (u64, Open) {
        let p = pre(env, mm, &b.key());
        let (cap, q) = binding(&p);
        assert_eq!(
            cap,
            expect,
            "binding cap; bounds {:?}; pre {p:?}",
            bounds(&p)
        );
        assert!(q > 0, "room for an open: {:?}", bounds(&p));
        let (res, _) = env.open(mm, b, q + 1);
        assert_err(&res, cap.error());
        let (res, _) = env.open(mm, b, q);
        let open: Open = events::one(&ok(res));
        assert_eq!(open.q, q);
        // The program's own reports agree with the recomputation.
        let after = env.market(&mm.market());
        assert_eq!(after.borrowed_q, p.b + q);
        (q, open)
    }
}

/// The mock venue.
mod mock {
    use shortwin::errors::ShortwinError as E;

    use crate::b::{self, Cap};
    use crate::ecommon::*;

    /// The default pool (10¹⁵ raw tokens against 1,000 SOL) at fee `fee_ppm`, `lend` lent.
    fn spec(fee_ppm: u32, base: u64, quote: u64, lend: u64) -> MarketSpec {
        MarketSpec {
            fee_ppm,
            base,
            quote,
            lend,
            ..MarketSpec::default()
        }
    }

    #[test]
    fn mock_fee_scaled_size() {
        let mut env = b::mock_env(b::wide_wallet);
        let mm = env.mock_market(MarketSpec::default());
        b::next_second(&mut env, &mm);
        let mut b1 = env.borrower();
        let (q, _) = b::exact(&mut env, &mm, &mut b1, Cap::FeeSize);
        eprintln!("mock 25 bps: fee-scaled size cap {q} (0.75 % of X₀)");
    }

    #[test]
    fn mock_spec_position_impact() {
        let mut env = b::mock_env(b::wide_wallet);
        let mm = env.mock_market(spec(
            10_000,
            400_000_000_000_000,
            400_000_000_000,
            100_000_000_000_000,
        ));
        b::next_second(&mut env, &mm);
        let mut b1 = env.borrower();
        b::exact(&mut env, &mm, &mut b1, Cap::PositionImpact);
    }

    /// Positions at their fee-scaled size caps until the fee-scaled OI cap binds; each exact.
    /// Then a close frees its room: the next open's OI bound is exact again. All in one second
    /// except the close's (a new second, recomputed).
    #[test]
    fn mock_fee_scaled_oi_and_after_closes() {
        let mut env = b::mock_env(b::wide_wallet);
        let mm = env.mock_market(MarketSpec::default());
        b::next_second(&mut env, &mm);
        let mut filled = Vec::new();
        loop {
            let mut bw = env.borrower();
            let p = b::pre(&env, &mm, &bw.key());
            let (cap, _) = b::binding(&p);
            if cap == Cap::MarketOi {
                b::exact(&mut env, &mm, &mut bw, Cap::MarketOi);
                filled.push((bw, 0));
                break;
            }
            b::exact(&mut env, &mm, &mut bw, Cap::FeeSize);
            filled.push((bw, 0));
            assert!(filled.len() < 6);
        }
        // Full: a new wallet has no room left.
        let mut late = env.borrower();
        let p = b::pre(&env, &mm, &late.key());
        assert_eq!(b::binding(&p), (Cap::MarketOi, 0));
        let (res, _) = env.open(&mm, &mut late, 1);
        assert_err(&res, E::CapMarketOi);
        // A close frees its room: the last (smallest) position closes, and the next open is
        // bound by the per-market cap again, at exactly the freed room plus what the close's
        // buyback moved.
        b::next_second(&mut env, &mm);
        let (bw, nonce) = filled.last().expect("positions");
        close_ok(&mut env, &mm, bw, *nonce);
        let (q, _) = b::exact(&mut env, &mm, &mut late, Cap::MarketOi);
        eprintln!(
            "mock 25 bps: {} positions filled the OI cap; after a close {q}",
            filled.len()
        );
    }

    #[test]
    fn mock_spec_market_impact() {
        let mut env = b::mock_env(|p| {
            b::wide_wallet(p);
            p.market_max_notional = 1_000_000_000_000;
            p.open_ema_dev_bps = 5_000;
        });
        let mm = env.mock_market(spec(
            15_000,
            400_000_000_000_000,
            400_000_000_000,
            100_000_000_000_000,
        ));
        b::next_second(&mut env, &mm);
        let mut n = 0;
        loop {
            let mut bw = env.borrower();
            let p = b::pre(&env, &mm, &bw.key());
            if b::binding(&p).0 == Cap::MarketOi {
                b::exact(&mut env, &mm, &mut bw, Cap::MarketOi);
                break;
            }
            b::exact(&mut env, &mm, &mut bw, Cap::PositionImpact);
            n += 1;
            assert!(n < 6);
        }
        assert!(
            n >= 3,
            "{n} positions at the SPEC impact cap before the market cap"
        );
    }

    #[test]
    fn mock_utilisation() {
        let mut env = Env::test_venue();
        let mm = env.mock_market(MarketSpec {
            lend: 4_000_000_000_000,
            ..MarketSpec::default()
        });
        b::next_second(&mut env, &mm);
        let mut b1 = env.borrower();
        b::exact(&mut env, &mm, &mut b1, Cap::Utilisation);
    }

    /// The wallet cap at its default (25 % of the per-market cap): a fresh wallet; the same
    /// wallet's second position (its `q_open` counted); other wallets until the market fills
    /// (the last one bound by whichever of the wallet and market caps is smaller).
    #[test]
    fn mock_wallet_single_and_multi() {
        let mut env = Env::test_venue();
        let mm = env.mock_market(MarketSpec::default());
        b::next_second(&mut env, &mm);
        let mut a = env.borrower();
        let p = b::pre(&env, &mm, &a.key());
        let (cap, q_full) = b::binding(&p);
        assert_eq!(cap, Cap::Wallet);
        // Half first, then the exact rest.
        let (res, _) = env.open(&mm, &mut a, q_full / 2);
        ok(res);
        let (q2, _) = b::exact(&mut env, &mm, &mut a, Cap::Wallet);
        let p = b::pre(&env, &mm, &a.key());
        assert_eq!(p.wq, q_full / 2 + q2);
        assert_eq!(b::binding(&p), (Cap::Wallet, 0), "the wallet is full");
        let (res, _) = env.open(&mm, &mut a, 1);
        assert_err(&res, E::CapWallet);
        // Other wallets.
        for i in 0..8 {
            let mut w = env.borrower();
            let p = b::pre(&env, &mm, &w.key());
            let (cap, q) = b::binding(&p);
            if q == 0 {
                assert_eq!(
                    cap,
                    Cap::MarketOi,
                    "the market is full after {i} more wallets"
                );
                return;
            }
            b::exact(&mut env, &mm, &mut w, cap);
        }
        panic!("the market never filled");
    }

    /// M7-SEC-9 (SPEC §12 R1; ECON §6.1 "Per wallet"): the wallet cap is per key — a UX limit,
    /// not a self-dealing bound. One actor, one funded key that funds fresh keys with plain
    /// System transfers in one transaction, fills the market's OI cap at the defaults
    /// (`wallet_cap_bps` 2,500): each key opens exactly up to its own wallet cap (one raw token
    /// more is `CapWallet`), and after the fourth the market is full (`CapMarketOi` for any key,
    /// no room), the actor holding the whole per-market cap — four times what one key may.
    /// No ECON bound relies on the wallet cap (§12.5's basket uses the full OI cap).
    #[test]
    fn m7_sec_9_one_actor_with_four_keys_fills_the_oi_cap() {
        use anchor_lang::solana_program::system_instruction;
        use solana_keypair::Keypair;
        use solana_signer::Signer;

        let mut env = Env::test_venue();
        let mm = env.mock_market(MarketSpec::default());
        assert_eq!(env.config().params.wallet_cap_bps, 2_500, "the default");
        b::next_second(&mut env, &mm);
        let actor = env.funded_keypair();
        let keys: Vec<Keypair> = (0..5).map(|_| Keypair::new()).collect();
        let fund: Vec<_> = keys
            .iter()
            .map(|k| system_instruction::transfer(&actor.pubkey(), &k.pubkey(), 100_000_000_000))
            .collect();
        ok(env.send(&fund, &[&actor]));
        let mut keys = keys.into_iter().map(|kp| Borrower { kp, next_nonce: 0 });
        // Four keys, each at its own wallet cap (in one second: `X₀` is the same for each).
        let mut held = Vec::new();
        for _ in 0..4 {
            let mut w = keys.next().expect("a key");
            let (q, _) = b::exact(&mut env, &mm, &mut w, Cap::Wallet);
            held.push(q);
        }
        assert!(held.iter().all(|&q| q == held[0]), "{held:?}");
        let held: u64 = held.iter().sum();
        // The fifth key: the market is full; only the per-market cap stops the actor.
        let mut fifth = keys.next().expect("a key");
        let p = b::pre(&env, &mm, &fifth.key());
        assert_eq!(b::binding(&p), (Cap::MarketOi, 0), "{:?}", b::bounds(&p));
        let (res, _) = env.open(&mm, &mut fifth, 1);
        assert_err(&res, E::CapMarketOi);
        let m = env.market(&mm.market());
        assert_eq!(m.borrowed_q, held, "every borrowed token is the actor's");
        eprintln!(
            "M7-SEC-9: one actor's four keys hold {held} raw tokens, the market's whole OI cap \
             (one key's cap {})",
            held / 4
        );
    }
}

/// The pump curve.
mod curve {
    use shortwin::state::InitConfigArgs;
    use shortwin_litesvm_tests::pump::{CurveUser, PumpMarket, PumpSpec};

    use crate::b::{self, Cap};
    use crate::pumpc::*;

    /// A curve market on `coin` in an environment initialized with `f`; `lend` lent.
    fn market(
        coin: PumpCoin,
        lend_div: u64,
        f: impl FnOnce(&mut InitConfigArgs),
    ) -> (Env, PumpMarket) {
        let mut env = Env::new();
        let mut args = env.default_init_args();
        f(&mut args);
        ok(env.initialize_config(args));
        env.load_pump_programs();
        env.load_pump_statics();
        let c = env.load_pump_coin(coin);
        env.set_pump_rent();
        env.set_pump_clock();
        if coin == PumpCoin::Fresh {
            let u = CurveUser::new(&mut env, &c);
            let s = c.curve_state(&env.svm);
            let q = u64::try_from(s.vt / 7).unwrap();
            ok(u.buy(&mut env, &c, q, u64::MAX));
        }
        ok(env.list_pump(&c));
        let vt = c.state(&env.svm).virtual_token_reserves;
        let pm = env.pump_market_listed(PumpSpec::new(coin, vt / lend_div), c);
        (env, pm)
    }

    /// The near-complete curve: a wallet at its cap (25 % of the Tier D market cap), then a
    /// second wallet bound by Tier D's graduation guard on the reserves at the EMA price (the
    /// first sale lowered the spot below `P_ema`, so `rT_guard` is the EMA-price curve's, not
    /// the live `rT` the sale raised; M5-SEC-3).
    #[test]
    fn curve_gamma_and_wallet() {
        let (mut env, pm) = market(PumpCoin::Near, 5, |_| {});
        b::next_second(&mut env, &pm);
        let mut a = env.borrower();
        b::exact(&mut env, &pm, &mut a, Cap::Wallet);
        let mut w = env.borrower();
        let p = b::pre(&env, &pm, &w.key());
        let rt_live = pm.curve.state(&env.svm).real_token_reserves;
        assert!(
            p.rt_guard.expect("curve") < rt_live,
            "the guard reads the EMA-price curve"
        );
        b::exact(&mut env, &pm, &mut w, Cap::CurveGamma);
    }

    #[test]
    fn curve_position_impact_then_market() {
        // `open_ema_dev_bps` at its bound (50 %): the positions sold in one second move the
        // curve's spot ≈ 9 % below `P_ema`.
        let (mut env, pm) = market(PumpCoin::Fresh, 5, |a| {
            b::wide_wallet(&mut a.params);
            a.params.open_ema_dev_bps = 5_000;
        });
        b::next_second(&mut env, &pm);
        let mut a = env.borrower();
        b::exact(&mut env, &pm, &mut a, Cap::PositionImpact);
        // Positions at 80 % of their per-position cap until Tier D's per-market cap binds (so
        // the market's last room stays above `min_notional`); that open exact.
        let mut n = 1;
        loop {
            let mut w = env.borrower();
            let p = b::pre(&env, &pm, &w.key());
            let (cap, q) = b::binding(&p);
            if cap == Cap::MarketOi {
                b::exact(&mut env, &pm, &mut w, Cap::MarketOi);
                break;
            }
            assert_eq!(cap, Cap::PositionImpact);
            ok(env.open(&pm, &mut w, q / 10 * 8).0);
            n += 1;
            assert!(n < 8);
        }
        assert!(n >= 2, "{n} positions before Tier D's ι_mkt binds");
    }
}

/// PumpSwap.
mod pumpswap {
    use crate::b::{self, Cap};
    use crate::pcommon::*;

    /// The mature pool with `lend` = 5 % of its base reserve, in an environment initialized
    /// with `f`, warmed up and past its boost window.
    fn market(f: impl FnOnce(&mut shortwin::state::InitConfigArgs)) -> (Env, PammMarket) {
        let (mut env, p) = Env::fork_pamm_coin_with(PammCoin::Mature, f);
        let base = p.reserves(&env.svm).expect("reserves").base;
        let pm = list_on(&mut env, &p, base / 20);
        wait_out_boost(&mut env, &pm);
        (env, pm)
    }

    #[test]
    fn pumpswap_position_and_market() {
        let (mut env, pm) = market(|a| {
            b::wide_wallet(&mut a.params);
            a.params.market_max_notional = 100_000_000_000_000;
        });
        b::next_second(&mut env, &pm);
        let mut n = 0;
        loop {
            let mut w = env.borrower();
            env.fund(&w.key(), 50_000_000_000_000);
            let p = b::pre(&env, &pm, &w.key());
            let (cap, _) = b::binding(&p);
            if cap == Cap::MarketOi {
                b::exact(&mut env, &pm, &mut w, Cap::MarketOi);
                break;
            }
            assert!(matches!(cap, Cap::PositionImpact | Cap::FeeSize), "{cap:?}");
            b::exact(&mut env, &pm, &mut w, cap);
            n += 1;
            assert!(n < 8);
        }
        eprintln!("PumpSwap mature: {n} positions at the per-position cap, then the market cap");
    }

    #[test]
    fn pumpswap_wallet() {
        let (mut env, pm) = market(|_| {});
        b::next_second(&mut env, &pm);
        let mut w = env.borrower();
        b::exact(&mut env, &pm, &mut w, Cap::Wallet);
    }
}

/// Raydium AMM v4 and CPMM.
mod raydium {
    use shortwin::state::InitConfigArgs;

    use crate::b::{self, Cap};
    use crate::rcommon::*;

    /// A market on `fixture` in an environment initialized with `f`: 5 % of the pool's token
    /// reserve lent, the listing window waited out, cranked.
    fn market(fixture: RayFixture, f: impl FnOnce(&mut InitConfigArgs)) -> (Env, RayMarket) {
        let mut env = Env::new();
        let mut args = env.default_init_args();
        f(&mut args);
        ok(env.initialize_config(args));
        env.load_raydium_programs();
        env.set_fork_clock();
        let pool = env.load_ray_pool(fixture);
        ok(env.list_ray(&pool, fixture.hint()));
        let (x, _) = pool
            .io_reserves(&env.svm, shortwin_math::fees::Direction::Sell)
            .expect("reserves");
        let rm = env.ray_market_listed(RaySpec::new(fixture, x / 20), pool);
        env.crank(&rm);
        (env, rm)
    }

    #[test]
    fn raydium_position_market_and_wallet() {
        for fixture in [FART, USELESS] {
            let (mut env, rm) = market(fixture, |a| {
                b::wide_wallet(&mut a.params);
                a.params.market_max_notional = 100_000_000_000_000;
            });
            b::next_second(&mut env, &rm);
            let mut n = 0;
            loop {
                let mut w = env.borrower();
                env.fund(&w.key(), 50_000_000_000_000);
                let p = b::pre(&env, &rm, &w.key());
                if b::binding(&p).0 == Cap::MarketOi {
                    b::exact(&mut env, &rm, &mut w, Cap::MarketOi);
                    break;
                }
                b::exact(&mut env, &rm, &mut w, Cap::FeeSize);
                n += 1;
                assert!(n < 8);
            }
            // The wallet cap at its default. On pools this deep the 25 SOL launch guard binds
            // first (Fartcoin's wallet cap is ≈ 180 SOL of notional), so it is lifted here.
            let (mut env, rm) = market(fixture, |a| {
                a.params.market_max_notional = 100_000_000_000_000;
            });
            b::next_second(&mut env, &rm);
            let mut w = env.borrower();
            env.fund(&w.key(), 50_000_000_000_000);
            b::exact(&mut env, &rm, &mut w, Cap::Wallet);
        }
    }
}
