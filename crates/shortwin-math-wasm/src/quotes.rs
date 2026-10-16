//! Quotes, fees and the venue a market trades on (`shortwin_math::{quote, fees}`;
//! ECON §2, §3.1; ARCH §4.2, §4.3), plus the liquidation price (`risk::liquidation_price`).

use shortwin_math::MathError;
use shortwin_math::fees::{
    self, CpmmConfig, CpmmPoolFees, CurveFeeSchedule, Direction, FeeBps, FeeTier, PammFeeSchedule,
};
use shortwin_math::quote::{self, CurveOffsets, CurvePricing, MigrationParams, Pricing};
use shortwin_math::risk as mrisk;
use wasm_bindgen::JsError;
use wasm_bindgen::prelude::wasm_bindgen;

use crate::{bps_number, or_throw};

/// Fee components of a pump venue in bps (`fees::FeeBps`): charged on the SOL leg, each
/// rounded up separately.
#[wasm_bindgen]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fees(pub(crate) FeeBps);

#[wasm_bindgen]
impl Fees {
    /// `(lp, protocol, creator)` in bps.
    #[wasm_bindgen(constructor)]
    pub fn new(lp: u32, protocol: u32, creator: u32) -> Fees {
        Fees(FeeBps::new(lp.into(), protocol.into(), creator.into()))
    }

    /// LP fee, bps.
    #[wasm_bindgen(getter)]
    pub fn lp(&self) -> u32 {
        bps_number(self.0.lp)
    }

    /// Protocol fee, bps.
    #[wasm_bindgen(getter)]
    pub fn protocol(&self) -> u32 {
        bps_number(self.0.protocol)
    }

    /// Creator fee, bps.
    #[wasm_bindgen(getter)]
    pub fn creator(&self) -> u32 {
        bps_number(self.0.creator)
    }

    /// One-way fee φ in bps, rounded up (display only; gates compare ppm).
    #[wasm_bindgen(js_name = onewayBps)]
    pub fn oneway_bps(&self) -> u32 {
        bps_number(self.0.oneway_bps())
    }

    /// One-way fee φ in ppm, the value every fee gate compares (ECON §2.1; ARCH §4.2).
    #[wasm_bindgen(js_name = onewayPpm)]
    pub fn oneway_ppm(&self) -> u32 {
        self.0.oneway_ppm()
    }

    /// The risk view `min(φ, capBps)` (ARCH §4.3, V2-1).
    pub fn capped(&self, cap_bps: u32) -> Fees {
        Fees(self.0.capped(cap_bps.into()))
    }
}

/// pump-fees tiers in account order, as `FeeTier`s.
#[derive(Clone, Debug, Default)]
struct Tiers(Vec<FeeTier>);

impl Tiers {
    fn add(&mut self, threshold: u128, lp: u32, protocol: u32, creator: u32) {
        self.0.push(FeeTier {
            threshold,
            fees: FeeBps::new(lp.into(), protocol.into(), creator.into()),
        });
    }
}

/// Fee inputs of a pump-curve coin (`fees::CurveFeeSchedule`; ARCH §4.1, §4.2).
#[wasm_bindgen]
#[derive(Clone, Debug)]
pub struct CurveSchedule {
    tiers: Tiers,
    creator_fee_configurable: bool,
    curve_creator_fee_bps: u64,
    creator_is_default: bool,
}

#[wasm_bindgen]
impl CurveSchedule {
    /// `Global.creator_fee_configurable`, `BondingCurve.creator_fee_bps`, and whether the
    /// curve's creator is the default key. Add the `FeeConfig` tiers with `addTier`.
    #[wasm_bindgen(constructor)]
    pub fn new(
        creator_fee_configurable: bool,
        curve_creator_fee_bps: u32,
        creator_is_default: bool,
    ) -> CurveSchedule {
        CurveSchedule {
            tiers: Tiers::default(),
            creator_fee_configurable,
            curve_creator_fee_bps: curve_creator_fee_bps.into(),
            creator_is_default,
        }
    }

    /// Appends a `FeeConfig` tier (account order).
    #[wasm_bindgen(js_name = addTier)]
    pub fn add_tier(&mut self, threshold: u128, lp: u32, protocol: u32, creator: u32) {
        self.tiers.add(threshold, lp, protocol, creator);
    }

    /// The live fees of a trade on the curve state `(vT, vQ)` (`fees::pump_curve`: the market
    /// cap on the fixed 10¹⁵ supply).
    pub fn resolve(&self, vt: u128, vq: u128) -> Result<Fees, JsError> {
        Ok(Fees(fees::pump_curve(vt, vq, &self.schedule())?))
    }

    /// The fees pump charges on both parts of a buy past `rT` by synthetic migration
    /// (`fees::pump_curve_at_supply`): the tier at the curve's market cap on the mint's live
    /// `supply` (pump-final §5.2). Pass them to `Venue.withSynthetic`.
    #[wasm_bindgen(js_name = resolveAtSupply)]
    pub fn resolve_at_supply(&self, vt: u128, vq: u128, supply: u64) -> Result<Fees, JsError> {
        Ok(Fees(fees::pump_curve_at_supply(
            vt,
            vq,
            supply,
            &self.schedule(),
        )?))
    }
}

impl CurveSchedule {
    fn schedule(&self) -> CurveFeeSchedule<'_> {
        CurveFeeSchedule {
            tiers: &self.tiers.0,
            creator_fee_configurable: self.creator_fee_configurable,
            curve_creator_fee_bps: self.curve_creator_fee_bps,
            creator_is_default: self.creator_is_default,
            cap_bps: None,
        }
    }
}

/// Fee inputs of a canonical PumpSwap pool (`fees::PammFeeSchedule`; ARCH §4.1, §4.2).
#[wasm_bindgen]
#[derive(Clone, Debug)]
pub struct PammSchedule {
    tiers: Tiers,
    base_supply: u64,
    creator_fee_configurable: bool,
    pool_creator_fee_bps: u64,
    coin_creator_is_default: bool,
    cap_bps: Option<u64>,
}

impl PammSchedule {
    fn schedule(&self) -> PammFeeSchedule<'_> {
        PammFeeSchedule {
            tiers: &self.tiers.0,
            base_supply: self.base_supply,
            creator_fee_configurable: self.creator_fee_configurable,
            pool_creator_fee_bps: self.pool_creator_fee_bps,
            coin_creator_is_default: self.coin_creator_is_default,
            cap_bps: self.cap_bps,
        }
    }
}

#[wasm_bindgen]
impl PammSchedule {
    /// Base-mint supply (10¹⁵ before graduation), `GlobalConfig.creator_fee_configurable`,
    /// `Pool.creator_fee_bps`, and whether the coin creator is the default key. Add the
    /// `FeeConfig` tiers with `addTier`.
    #[wasm_bindgen(constructor)]
    pub fn new(
        base_supply: u64,
        creator_fee_configurable: bool,
        pool_creator_fee_bps: u32,
        coin_creator_is_default: bool,
    ) -> PammSchedule {
        PammSchedule {
            tiers: Tiers::default(),
            base_supply,
            creator_fee_configurable,
            pool_creator_fee_bps: pool_creator_fee_bps.into(),
            coin_creator_is_default,
            cap_bps: None,
        }
    }

    /// Appends a `FeeConfig` tier (account order).
    #[wasm_bindgen(js_name = addTier)]
    pub fn add_tier(&mut self, threshold: u128, lp: u32, protocol: u32, creator: u32) {
        self.tiers.add(threshold, lp, protocol, creator);
    }

    /// The live fees of a trade on `(effective quote, base reserve)` (`fees::pump_amm`).
    pub fn resolve(&self, effective_quote: u128, base_reserve: u128) -> Result<Fees, JsError> {
        let mut s = self.schedule();
        s.cap_bps = None;
        Ok(Fees(fees::pump_amm(effective_quote, base_reserve, &s)?))
    }
}

/// An exact-out buy (`quote::BuyQuote`).
#[wasm_bindgen]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BuyQuote(pub(crate) quote::BuyQuote);

#[wasm_bindgen]
impl BuyQuote {
    /// `B̂`: lamports the venue debits, fees included.
    #[wasm_bindgen(getter)]
    pub fn cost(&self) -> u64 {
        self.0.cost
    }

    /// Pre-fee SOL input `n` (σ uses it).
    #[wasm_bindgen(getter)]
    pub fn net(&self) -> u64 {
        self.0.net
    }

    /// Token pricing reserve after the trade.
    #[wasm_bindgen(getter, js_name = tokenAfter)]
    pub fn token_after(&self) -> u128 {
        self.0.token_after
    }

    /// SOL pricing reserve after the trade.
    #[wasm_bindgen(getter, js_name = solAfter)]
    pub fn sol_after(&self) -> u128 {
        self.0.sol_after
    }
}

/// An exact-in sale (`quote::SellQuote`).
#[wasm_bindgen]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SellQuote(pub(crate) quote::SellQuote);

#[wasm_bindgen]
impl SellQuote {
    /// `S`: lamports the seller receives, fees deducted.
    #[wasm_bindgen(getter)]
    pub fn proceeds(&self) -> u64 {
        self.0.proceeds
    }

    /// Token pricing reserve after the trade.
    #[wasm_bindgen(getter, js_name = tokenAfter)]
    pub fn token_after(&self) -> u128 {
        self.0.token_after
    }

    /// SOL pricing reserve after the trade.
    #[wasm_bindgen(getter, js_name = solAfter)]
    pub fn sol_after(&self) -> u128 {
        self.0.sol_after
    }
}

/// A constant-product state `(x, y)` = (token, SOL).
#[wasm_bindgen]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Reserves {
    x: u128,
    y: u128,
}

#[wasm_bindgen]
impl Reserves {
    /// Token reserve.
    #[wasm_bindgen(getter)]
    pub fn x(&self) -> u128 {
        self.x
    }

    /// SOL reserve.
    #[wasm_bindgen(getter)]
    pub fn y(&self) -> u128 {
        self.y
    }
}

/// `reprice(L, P)`: the state with liquidity `L` at price `P` (WAD) (ECON §2).
#[wasm_bindgen]
pub fn reprice(l: u128, p: u128) -> Option<Reserves> {
    quote::reprice(l, p).map(|(x, y)| Reserves { x, y })
}

/// The fee gate `φ > cap` compared exactly in ppm (`fees::exceeds_cap`; ECON §2.1): opens
/// above `venue_fee_cap_bps`, S3 evidence above `venue_fee_hard_cap_bps`.
#[wasm_bindgen(js_name = exceedsCap)]
pub fn exceeds_cap(phi_ppm: u32, cap_bps: u32) -> bool {
    fees::exceeds_cap(phi_ppm, cap_bps.into())
}

/// Liquidity `L = isqrt(X·Y)` (ECON §2.1); throws `overflow` if `X·Y ≥ 2¹²⁸`.
#[wasm_bindgen]
pub fn liquidity(x: u128, y: u128) -> u128 {
    crate::or_throw(quote::liquidity(x, y))
}

/// Marginal price `P = ⌊Y·WAD/X⌋` (ECON §2).
#[wasm_bindgen(js_name = priceWad)]
pub fn price_wad(x: u128, y: u128) -> Result<u128, JsError> {
    Ok(mrisk::price_wad(x, y)?)
}

/// A pump bonding-curve state (`quote::CurveState`).
#[wasm_bindgen]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CurveState(pub(crate) quote::CurveState);

#[wasm_bindgen]
impl CurveState {
    /// `(vT, vQ, rT, rQ, complete)` from the `BondingCurve` account.
    #[wasm_bindgen(constructor)]
    pub fn new(vt: u128, vq: u128, rt: u128, rq: u128, complete: bool) -> CurveState {
        CurveState(quote::CurveState {
            vt,
            vq,
            rt,
            rq,
            complete,
        })
    }

    /// `curve_reprice(L, P, offsets)` (ARCH §4.3 EMA leg): the curve at price `P` with
    /// liquidity `L` and the offsets `vT − rT`, `vQ − rQ`.
    pub fn reprice(l: u128, p: u128, offset_token: u128, offset_quote: u128) -> Option<CurveState> {
        let offset = CurveOffsets {
            token: offset_token,
            quote: offset_quote,
        };
        quote::curve_reprice(l, p, offset).map(CurveState)
    }

    /// `virtual_token_reserves`.
    #[wasm_bindgen(getter)]
    pub fn vt(&self) -> u128 {
        self.0.vt
    }

    /// `virtual_quote_reserves`.
    #[wasm_bindgen(getter)]
    pub fn vq(&self) -> u128 {
        self.0.vq
    }

    /// `real_token_reserves`.
    #[wasm_bindgen(getter)]
    pub fn rt(&self) -> u128 {
        self.0.rt
    }

    /// `real_quote_reserves`.
    #[wasm_bindgen(getter)]
    pub fn rq(&self) -> u128 {
        self.0.rq
    }

    /// `complete`.
    #[wasm_bindgen(getter)]
    pub fn complete(&self) -> bool {
        self.0.complete
    }

    /// The canonical pool right after migration, `(X_p, Y_p)` (`quote::migrated_pool`):
    /// `poolTokens` the tokens it is seeded with (206.9e12, or the live `curve_base_ata` after a
    /// synthetic pool part), `postCompleteQuoteIn` the curve's `pcQ` (0 without a pool part).
    #[wasm_bindgen(js_name = migratedPool)]
    pub fn migrated_pool(
        &self,
        migration_fee: u64,
        pool_tokens: u128,
        post_complete_quote_in: u64,
    ) -> Option<Reserves> {
        let m = MigrationParams {
            migration_fee,
            pool_tokens,
            post_complete_quote_in,
        };
        quote::migrated_pool(&self.0, &m).map(|(x, y)| Reserves { x, y })
    }
}

/// FV's price on a migrating curve after a synthetic pool part, `⌈⌈p·Y₁/Y₀⌉·X₀/X₁⌉`
/// (`quote::migrating_fv_price`, pump-final §5.4): `(X₀, Y₀)` the pool without the pool part
/// (`migratedPool(fee, 206.9e12, 0)`), `(X₁, Y₁)` the pool `migrate_v2` creates. `undefined` on
/// `X₁ = 0` or `Y₀ = 0`. Return no FV at all when the program would not define one
/// (`postCompleteKnown` false, pump-final §6.1.5).
#[wasm_bindgen(js_name = migratingFvPrice)]
pub fn migrating_fv_price(p: u128, x0: u128, y0: u128, x1: u128, y1: u128) -> Option<u128> {
    quote::migrating_fv_price(p, x0, y0, x1, y1)
}

/// `migrate_v2`'s boost into the new pool's `virtual_quote_reserves`, `⌊X₁·Y₁/10¹⁵⌋`
/// (`quote::migration_boost`, P1 correction (b)).
#[wasm_bindgen(js_name = migrationBoost)]
pub fn migration_boost(x1: u128, y1: u128) -> Option<u128> {
    quote::migration_boost(x1, y1)
}

/// The pump curve as a venue: its own fees, the offsets for repricing, the post-migration
/// pool of the straddle (ARCH §4.3, V1-9), and the synthetic exit's fee when it trades v3
/// (pump-final §5.3); an owned `quote::CurvePricing`.
#[derive(Clone, Debug)]
struct CurveVenue {
    fees: FeeBps,
    offsets: CurveOffsets,
    migration: MigrationParams,
    pool: PammSchedule,
    synthetic: Option<FeeBps>,
}

impl CurveVenue {
    fn pricing(&self) -> CurvePricing<'_> {
        CurvePricing {
            fees: self.fees,
            offsets: self.offsets,
            migration: self.migration,
            pool: self.pool.schedule(),
            synthetic: self.synthetic,
        }
    }
}

#[derive(Clone, Debug)]
enum Kind {
    Cp(Pricing),
    Curve(Box<CurveVenue>),
}

/// The venue a market trades on, with resolved fees: PumpSwap, Raydium AMM v4 or CPMM
/// (`quote::Pricing`), or the pump curve (straddle-aware). Build the risk view with
/// `capped`.
#[wasm_bindgen]
#[derive(Clone, Debug)]
pub struct Venue(Kind);

#[wasm_bindgen]
impl Venue {
    /// A PumpSwap pool with resolved fees (`PammSchedule.resolve`).
    #[wasm_bindgen(js_name = pumpSwap)]
    pub fn pump_swap(fees: &Fees) -> Venue {
        Venue(Kind::Cp(Pricing::PumpSwap(fees.0)))
    }

    /// A Raydium AMM v4 pool (`swap_fee_numerator`, `swap_fee_denominator`).
    #[wasm_bindgen(js_name = rayV4)]
    pub fn ray_v4(num: u64, den: u64) -> Result<Venue, JsError> {
        Ok(Venue(Kind::Cp(Pricing::RayV4(fees::ray_v4(num, den)?))))
    }

    /// A Raydium CPMM pool: `AmmConfig` rates (ppm) and the pool's creator-fee fields.
    #[allow(clippy::too_many_arguments)]
    pub fn cpmm(
        trade_fee_rate: u32,
        protocol_fee_rate: u32,
        fund_fee_rate: u32,
        creator_fee_rate: u32,
        enable_creator_fee: bool,
        creator_fee_on: u8,
        sol_is_token0: bool,
    ) -> Result<Venue, JsError> {
        let config = CpmmConfig {
            trade_fee_rate: trade_fee_rate.into(),
            protocol_fee_rate: protocol_fee_rate.into(),
            fund_fee_rate: fund_fee_rate.into(),
            creator_fee_rate: creator_fee_rate.into(),
        };
        let pool = CpmmPoolFees {
            enable_creator_fee,
            creator_fee_on,
            sol_is_token0,
        };
        Ok(Venue(Kind::Cp(Pricing::Cpmm {
            buy: fees::cpmm(&config, &pool, Direction::Buy)?,
            sell: fees::cpmm(&config, &pool, Direction::Sell)?,
        })))
    }

    /// The pump curve: its resolved fees, the offsets `vT − rT` and `vQ − rQ`, the
    /// migration (`pool_migration_fee`, tokens seeded into the pool, the curve's
    /// `post_complete_quote_in`) and the PumpSwap fee schedule of the post-migration pool.
    /// Above `rT` it prices `B̂_str` on that pool; `withSynthetic` switches a live curve to
    /// pump's synthetic migration (`B̂_v3`).
    #[wasm_bindgen(js_name = pumpCurve)]
    pub fn pump_curve(
        fees: &Fees,
        offset_token: u128,
        offset_quote: u128,
        migration_fee: u64,
        pool_tokens: u128,
        post_complete_quote_in: u64,
        pool: &PammSchedule,
    ) -> Venue {
        Venue(Kind::Curve(Box::new(CurveVenue {
            fees: fees.0,
            offsets: CurveOffsets {
                token: offset_token,
                quote: offset_quote,
            },
            migration: MigrationParams {
                migration_fee,
                pool_tokens,
                post_complete_quote_in,
            },
            pool: pool.clone(),
            synthetic: None,
        })))
    }

    /// This curve with buys past `rT` priced as pump executes them by synthetic migration
    /// (`CurvePricing::synthetic`, `quote::curve_synthetic`, pump-final §5.2–§5.3): `rT` on
    /// the curve plus the rest on the would-be pool, both at `syntheticFees` — the tier at the
    /// curve's market cap on the mint's live supply (`CurveSchedule.resolveAtSupply`). For a live
    /// (not complete), non-cashback, non-mayhem curve only; a migrating market keeps `B̂_str`.
    /// Throws `invalid input` on a pool venue.
    #[wasm_bindgen(js_name = withSynthetic)]
    pub fn with_synthetic(&self, synthetic_fees: &Fees) -> Result<Venue, JsError> {
        let mut c = self.curve()?.clone();
        c.synthetic = Some(synthetic_fees.0);
        Ok(Venue(Kind::Curve(Box::new(c))))
    }

    /// The risk view: every fee at `min(φ_live, capBps)` (ARCH §4.3, V2-1), for CR, the
    /// liquidation price and FV.
    pub fn capped(&self, cap_bps: u32) -> Venue {
        let cap = u64::from(cap_bps);
        Venue(match &self.0 {
            Kind::Cp(p) => Kind::Cp(p.capped(cap)),
            Kind::Curve(c) => {
                let capped = c.pricing().capped(cap);
                let mut c = c.clone();
                c.fees = capped.fees;
                c.synthetic = capped.synthetic;
                c.pool.cap_bps = capped.pool.cap_bps;
                Kind::Curve(c)
            }
        })
    }

    /// One-way fee φ of a buy in bps, rounded up (the curve's own fee on a curve; display
    /// only).
    #[wasm_bindgen(js_name = onewayBps)]
    pub fn oneway_bps(&self) -> u32 {
        bps_number(match &self.0 {
            Kind::Cp(p) => p.oneway_bps(),
            Kind::Curve(c) => c.fees.oneway_bps(),
        })
    }

    /// One-way fee φ of a buy in ppm (ECON §2.1): what `venue_fee_cap_bps` and
    /// `venue_fee_hard_cap_bps` are compared with, as `cap·100` (`exceedsCap`).
    #[wasm_bindgen(js_name = onewayPpm)]
    pub fn oneway_ppm(&self) -> u32 {
        match &self.0 {
            Kind::Cp(p) => p.oneway_ppm(),
            Kind::Curve(c) => c.fees.oneway_ppm(),
        }
    }

    /// Exact-out buy of `q` on the pool reserves `(x, y)` = (token, SOL). A curve is
    /// quoted with `buyCurve`.
    pub fn buy(&self, q: u64, x: u128, y: u128) -> Result<Option<BuyQuote>, JsError> {
        Ok(self.pricing()?.buy(q, x, y).map(BuyQuote))
    }

    /// Exact-in sale of `q` on the pool reserves `(x, y)`; `realQuoteCap` is PumpSwap's
    /// quote vault (Raydium ignores it). A curve is quoted with `sellCurve`.
    pub fn sell(
        &self,
        q: u64,
        x: u128,
        y: u128,
        real_quote_cap: u64,
    ) -> Result<Option<SellQuote>, JsError> {
        Ok(self.pricing()?.sell(q, x, y, real_quote_cap).map(SellQuote))
    }

    /// Buyback of `q` on a curve state along the executed path (`CurvePricing::buy`): the
    /// curve for `q ≤ rT`; above it `B̂_v3` after `withSynthetic` (`quote::curve_synthetic`),
    /// else all of `q` on the post-migration pool (`quote::curve_straddle`).
    #[wasm_bindgen(js_name = buyCurve)]
    pub fn buy_curve(&self, q: u64, state: &CurveState) -> Result<Option<BuyQuote>, JsError> {
        Ok(self.curve()?.pricing().buy(q, &state.0).map(BuyQuote))
    }

    /// Sale of `q` on a curve state (`CurvePricing::sell`: `sol_leg::sell_exact_in` on
    /// `(vT, vQ)`, paid out of `rQ`; `undefined` on a complete curve).
    #[wasm_bindgen(js_name = sellCurve)]
    pub fn sell_curve(&self, q: u64, state: &CurveState) -> Result<Option<SellQuote>, JsError> {
        Ok(self.curve()?.pricing().sell(q, &state.0).map(SellQuote))
    }

    /// `B̂(q; reprice(L, P))`: the buy on the state repriced to `(L, P)` (on a curve,
    /// the straddle on `curve_reprice`) — the EMA leg, FV and the reference quote.
    #[wasm_bindgen(js_name = buyRepriced)]
    pub fn buy_repriced(&self, q: u64, l: u128, p: u128) -> Option<BuyQuote> {
        self.repriced(q, l, p).map(BuyQuote)
    }

    /// `B̂_FV(q)` of a stuck migration (`Quoter::fv_repriced` on a `PumpMigrating` market,
    /// `quote::migrating_fv`, pump-final §5.4): the pool `migrate_v2` will create from the
    /// complete curve `state` with this curve's migration (206.9e12 tokens, or the live
    /// `curve_base_ata` and `pcQ` after a synthetic pool part), at that pool's own PumpSwap tier
    /// (capped in the risk view), repriced to `p` — ρ-scaled, `⌈⌈p·Y₁/Y₀⌉·X₀/X₁⌉`, after a pool
    /// part. Return no FV at all when the program defines none (`postCompleteKnown` false,
    /// pump-final §6.1.5). Throws `invalid input` on a pool venue.
    #[wasm_bindgen(js_name = migratingFv)]
    pub fn migrating_fv(
        &self,
        q: u64,
        p: u128,
        state: &CurveState,
    ) -> Result<Option<BuyQuote>, JsError> {
        let c = self.curve()?;
        Ok(quote::migrating_fv(q, p, &state.0, &c.migration, &c.pool.schedule()).map(BuyQuote))
    }

    /// Liquidation price (ECON §3.3; `risk::liquidation_price`, on a curve
    /// `CurvePricing::liquidation_price`): the smallest `P ≥ pFrom` at which
    /// `CR(vEff, B̂(q; reprice(L, P))) < crLiqBps`. Use the capped venue and `L_live` for
    /// the spot leg, `L_ema` for the EMA leg. `undefined` if no price is.
    #[wasm_bindgen(js_name = liquidationPrice)]
    pub fn liquidation_price(
        &self,
        q: u64,
        v_eff: u64,
        cr_liq_bps: u32,
        l: u128,
        p_from: u128,
    ) -> Option<u128> {
        or_throw(match &self.0 {
            Kind::Cp(pricing) => mrisk::liquidation_price(p_from, v_eff, cr_liq_bps, |p| {
                pricing.buy_repriced(q, l, p).map(|b| b.cost)
            }),
            Kind::Curve(c) => c
                .pricing()
                .liquidation_price(q, v_eff, cr_liq_bps, l, p_from),
        })
    }
}

impl Venue {
    fn pricing(&self) -> Result<&Pricing, MathError> {
        match &self.0 {
            Kind::Cp(p) => Ok(p),
            Kind::Curve(_) => Err(MathError::InvalidInput),
        }
    }

    fn curve(&self) -> Result<&CurveVenue, MathError> {
        match &self.0 {
            Kind::Curve(c) => Ok(c),
            Kind::Cp(_) => Err(MathError::InvalidInput),
        }
    }

    fn repriced(&self, q: u64, l: u128, p: u128) -> Option<quote::BuyQuote> {
        match &self.0 {
            Kind::Cp(pricing) => pricing.buy_repriced(q, l, p),
            Kind::Curve(c) => c.pricing().buy_repriced(q, l, p),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const WAD: u128 = shortwin_math::num::WAD;

    #[test]
    fn pump_curve_worked_example() {
        // R-PUMP §4.2 / ECON §3.2: the program charges 485,764,634 lamports for 10¹³ raw;
        // short.win's quote rounds up to 485,764,635. The sale pays 462,410,820.
        let mut pamm = PammSchedule::new(1_000_000_000_000_000, true, 0, false);
        pamm.add_tier(0, 2, 93, 30);
        let v = Venue::pump_curve(
            &Fees::new(0, 95, 30),
            279_900_000_000_000,
            30_000_000_000,
            15_000_001,
            206_900_000_000_000,
            0,
            &pamm,
        );
        let s = CurveState::new(
            824_130_602_106_172,
            39_059_343_535,
            544_230_602_106_172,
            9_059_343_535,
            false,
        );
        let q = 10_000_000_000_000;
        assert_eq!(v.buy_curve(q, &s).unwrap().unwrap().cost(), 485_764_635);
        assert_eq!(
            v.sell_curve(q, &s).unwrap().unwrap().proceeds(),
            462_410_820
        );
        assert_eq!((v.oneway_bps(), v.oneway_ppm()), (125, 12_500));
        assert!(exceeds_cap(v.oneway_ppm(), 124) && !exceeds_cap(v.oneway_ppm(), 125));
        assert_eq!(v.capped(100).oneway_bps(), 100);
        // The repriced curve at the live (L, P) is within rounding of the live quote.
        let l = liquidity(s.vt(), s.vq());
        let p = price_wad(s.vt(), s.vq()).unwrap();
        let r = v.buy_repriced(q, l, p).unwrap().cost();
        assert!(r.abs_diff(485_764_635) < 100_000);
        let p_liq = v.liquidation_price(q, 2 * r, 12_500, l, p).unwrap();
        assert!(p_liq > p && p_liq < 2 * p);
    }

    /// pump-final §5.2–§5.5: `withSynthetic` prices a buy past `rT` as `quote::curve_synthetic`
    /// (G0's `near166` buy of `rT + 10e12`: pump charged 22,944,563,744, `B̂_v3` +2); without it
    /// the curve keeps `B̂_str`. `resolveAtSupply` takes the tier of the live supply's market cap
    /// where `resolve` takes the 10¹⁵ one (G0's tier tripwire, P1 correction (a)). The migrated
    /// pool with `pcQ`, ρ and the boost as in the crate.
    #[test]
    fn pump_curve_synthetic() {
        let mut pamm = PammSchedule::new(1_000_000_000_000_000, true, 0, false);
        pamm.add_tier(0, 2, 93, 30);
        let mut sched = CurveSchedule::new(true, 0, false);
        sched.add_tier(0, 0, 95, 30);
        let s = CurveState::new(
            333_021_296_825_365,
            96_660_487_854,
            53_121_296_825_365,
            66_660_487_854,
            false,
        );
        let f = sched.resolve(s.vt(), s.vq()).unwrap();
        let fs = sched
            .resolve_at_supply(s.vt(), s.vq(), 999_818_465_159_445)
            .unwrap();
        assert_eq!(fs, f, "one flat tier: the live supply changes nothing");
        let v = Venue::pump_curve(
            &f,
            279_900_000_000_000,
            30_000_000_000,
            15_000_001,
            206_900_000_000_000,
            0,
            &pamm,
        );
        let syn = v.with_synthetic(&fs).unwrap();
        let q = 53_121_296_825_365 + 10_000_000_000_000;
        let b = syn.buy_curve(q, &s).unwrap().unwrap();
        assert_eq!(b.cost(), 22_944_563_746);
        let mig = MigrationParams {
            migration_fee: 15_000_001,
            pool_tokens: 206_900_000_000_000,
            post_complete_quote_in: 0,
        };
        assert_eq!(
            Some(b.0),
            quote::curve_synthetic(q, &s.0, &f.0, &fs.0, &mig)
        );
        let straddle = v.buy_curve(q, &s).unwrap().unwrap();
        assert!(straddle.cost() > b.cost());
        // Below rT both price the curve; the capped view caps the synthetic legs too.
        assert_eq!(
            syn.buy_curve(1_000, &s).unwrap(),
            v.buy_curve(1_000, &s).unwrap()
        );
        assert!(syn.capped(100).buy_curve(q, &s).unwrap().unwrap().cost() < b.cost());
        // P1 correction (a) under G0's tier tripwire: on this burned supply the fixed 10¹⁵ puts
        // the market cap (290,253,172,320) in the 70/22 tier, the live supply (290,200,481,257) in
        // the 80/25 one, which pump charged for rT + 50e12 (45,906,126,201): B̂_v3 covers it at
        // resolveAtSupply's fee and falls 59,057,853 under it at resolve's.
        let mut tiers = CurveSchedule::new(true, 0, false);
        tiers.add_tier(0, 0, 90, 28);
        tiers.add_tier(290_174_135_726, 0, 80, 25);
        tiers.add_tier(290_226_826_789, 0, 70, 22);
        let f_fixed = tiers.resolve(s.vt(), s.vq()).unwrap();
        let f_live = tiers
            .resolve_at_supply(s.vt(), s.vq(), 999_818_465_159_445)
            .unwrap();
        assert_eq!((f_fixed.protocol(), f_fixed.creator()), (70, 22));
        assert_eq!((f_live.protocol(), f_live.creator()), (80, 25));
        let at_1e15 = tiers
            .resolve_at_supply(s.vt(), s.vq(), 1_000_000_000_000_000)
            .unwrap();
        assert_eq!(at_1e15, f_fixed, "resolve is the 10¹⁵ supply");
        let tv = Venue::pump_curve(
            &f_fixed,
            279_900_000_000_000,
            30_000_000_000,
            15_000_001,
            206_900_000_000_000,
            0,
            &pamm,
        );
        let q50 = 53_121_296_825_365 + 50_000_000_000_000;
        let cost = |fs: &Fees| {
            tv.with_synthetic(fs)
                .unwrap()
                .buy_curve(q50, &s)
                .unwrap()
                .unwrap()
                .cost()
        };
        assert_eq!(cost(&f_live), 45_906_126_205);
        assert_eq!(cost(&f_fixed), 45_906_126_201 - 59_057_853);
        // migratedPool with pcQ (98u8RJ), ρ and the boost.
        let done = CurveState::new(
            279_900_000_000_000,
            115_005_359_190,
            0,
            85_005_359_190,
            true,
        );
        let p = done
            .migrated_pool(15_000_001, 200_661_149_081_575, 2_642_475_552)
            .unwrap();
        assert_eq!((p.x(), p.y()), (200_661_149_081_575, 87_632_834_741));
        let p0 = done
            .migrated_pool(15_000_001, 206_900_000_000_000, 0)
            .unwrap();
        let rho = migrating_fv_price(WAD, p0.x(), p0.y(), p.x(), p.y()).unwrap();
        assert_eq!(rho / 1_000_000_000_000, 1_063_149);
        assert_eq!(migrating_fv_price(1, 1, 1, 0, 1), None);
        assert_eq!(migration_boost(p.x(), p.y()), Some(17_584_505_316));
    }

    /// `migratingFv` is the program's FV on a stuck migration (`quote::migrating_fv`, pump-final
    /// §5.4) on 98u8RJ's completion: at the frozen curve's pool price `P(X₀, Y₀)`, ρ reprices it
    /// onto the pool `migrate_v2` created, `(X₁, Y₁)`, so FV is that pool's buy (within the
    /// reprice's rounding); without the pool part it is today's FV at `p` itself. The risk view
    /// caps the pool's tier.
    #[test]
    fn migrating_fv_follows_the_post_complete_pool() {
        let mut pamm = PammSchedule::new(1_000_000_000_000_000, true, 0, false);
        pamm.add_tier(0, 2, 93, 30);
        let curve = |pool_tokens: u128, pcq: u64| {
            Venue::pump_curve(
                &Fees::new(0, 95, 30),
                279_900_000_000_000,
                30_000_000_000,
                15_000_001,
                pool_tokens,
                pcq,
                &pamm,
            )
        };
        let done = CurveState::new(
            279_900_000_000_000,
            115_005_359_190,
            0,
            85_005_359_190,
            true,
        );
        let synthetic = curve(200_661_149_081_575, 2_642_475_552);
        let plain = curve(206_900_000_000_000, 0);
        let p0 = done
            .migrated_pool(15_000_001, 206_900_000_000_000, 0)
            .unwrap();
        let p = price_wad(p0.x(), p0.y()).unwrap();
        let q = 10_000_000_000_000;
        let fv = synthetic.migrating_fv(q, p, &done).unwrap().unwrap();
        let mig = MigrationParams {
            migration_fee: 15_000_001,
            pool_tokens: 200_661_149_081_575,
            post_complete_quote_in: 2_642_475_552,
        };
        let sched = pamm.schedule();
        assert_eq!(Some(fv.0), quote::migrating_fv(q, p, &done.0, &mig, &sched));
        // The pool migrate_v2 created, at its own tier: FV is its buy within the reprice's
        // rounding.
        let created = Venue::pump_swap(&pamm.resolve(87_632_834_741, 200_661_149_081_575).unwrap());
        let on_pool = created
            .buy(q, 200_661_149_081_575, 87_632_834_741)
            .unwrap()
            .unwrap();
        assert!(fv.cost().abs_diff(on_pool.cost()) * 10_000 < on_pool.cost());
        // Without ρ the same p prices the deeper-looking pool: low by about 1 − 1/ρ.
        let unscaled = plain.migrating_fv(q, p, &done).unwrap().unwrap();
        assert!(unscaled.cost() < fv.cost());
        let plain_pool = Venue::pump_swap(&pamm.resolve(p0.y(), p0.x()).unwrap());
        let reprices = plain_pool
            .buy_repriced(q, liquidity(p0.x(), p0.y()), p)
            .unwrap();
        assert_eq!(unscaled, reprices, "no pool part: FV at p itself");
        let capped = synthetic
            .capped(20)
            .migrating_fv(q, p, &done)
            .unwrap()
            .unwrap();
        assert!(capped.cost() < fv.cost());
    }

    #[test]
    fn cp_venues() {
        let v4 = Venue::ray_v4(25, 10_000).unwrap();
        // R-RAY §3.5 (A), with (token, SOL) reserves.
        let b = v4
            .buy(235_932_179_000, 37_639_277_914_829_371, 17_595_123_332_536)
            .unwrap()
            .unwrap();
        assert_eq!((b.net(), b.cost()), (110_291_219, 110_567_639));
        let cp = Venue::cpmm(2_500, 120_000, 40_000, 0, false, 0, true).unwrap();
        assert_eq!(cp.oneway_bps(), 25);
        let ps = Venue::pump_swap(&Fees::new(2, 93, 30));
        let y = 18_917_931_618u128 + 17_584_505_333;
        let b = ps
            .buy(9_155_477_586_733, 482_553_257_944_361, y)
            .unwrap()
            .unwrap();
        assert_eq!((b.net(), b.cost()), (705_954_395, 714_778_826));
        assert!(ps.buy_repriced(1, 0, WAD).is_none());
        assert_eq!(
            reprice(1, 1),
            Some(Reserves {
                x: 1_000_000_000,
                y: 1
            })
        );
    }

    #[test]
    fn fee_schedules_resolve() {
        let mut c = CurveSchedule::new(true, 0, false);
        c.add_tier(0, 0, 95, 30);
        let f = c.resolve(824_130_602_106_172, 39_059_343_535).unwrap();
        assert_eq!((f.lp(), f.protocol(), f.creator()), (0, 95, 30));
        let mut p = PammSchedule::new(1_000_000_000_000_000, true, 0, false);
        p.add_tier(0, 2, 93, 30);
        p.add_tier(420_000_000_000, 20, 5, 95);
        // mcap ⌊Y·supply/X⌋: 410.8 SOL (tier 0), then 483.3 SOL (tier 1).
        let f = p.resolve(84_990_359_345, 206_900_000_000_000).unwrap();
        assert_eq!((f.lp(), f.protocol(), f.creator()), (2, 93, 30));
        let f = p.resolve(100_000_000_000, 206_900_000_000_000).unwrap();
        assert_eq!((f.lp(), f.protocol(), f.creator()), (20, 5, 95));
    }
}
