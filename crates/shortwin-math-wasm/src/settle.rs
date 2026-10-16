//! The settlement box, FV's `p'` per evaluated state and the box amount check
//! (`shortwin_math::settle`; ECON §5.6, §8.6; ARCH §3.8, §4.5), and the protocol
//! parameter checks (`shortwin_math::params`; ECON §15; ARCH §2.3.3).

use shortwin_math::observe::{self, Ema, Ring, RingEntry};
use shortwin_math::params::{self as mparams, Violation};
use shortwin_math::settle::{self as msettle};
use shortwin_math::tier::Tier;
use shortwin_math::{MathError, consts};
use wasm_bindgen::JsError;
use wasm_bindgen::prelude::wasm_bindgen;

/// A box `[pLo, pHi] × [lLo, lHi]` of prices (WAD) and liquidities: the settlement box, or
/// the ring's medians `(P_mlo, P_mhi, L_mlo, L_mhi)`.
#[wasm_bindgen]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SettleBox(msettle::SettleBox);

#[wasm_bindgen]
impl SettleBox {
    /// Lowest price.
    #[wasm_bindgen(getter, js_name = pLo)]
    pub fn p_lo(&self) -> u128 {
        self.0.p_lo
    }

    /// Highest price.
    #[wasm_bindgen(getter, js_name = pHi)]
    pub fn p_hi(&self) -> u128 {
        self.0.p_hi
    }

    /// Lowest liquidity.
    #[wasm_bindgen(getter, js_name = lLo)]
    pub fn l_lo(&self) -> u128 {
        self.0.l_lo
    }

    /// Highest liquidity.
    #[wasm_bindgen(getter, js_name = lHi)]
    pub fn l_hi(&self) -> u128 {
        self.0.l_hi
    }

    /// The box is the EMA point alone (no integration for `RING_LEN` seconds).
    #[wasm_bindgen(getter, js_name = isPoint)]
    pub fn is_point(&self) -> bool {
        self.0.is_point()
    }

    /// `(p, l)` lies in the box.
    pub fn contains(&self, p: u128, l: u128) -> bool {
        self.0.contains(p, l)
    }
}

/// A market's EMA and ring as stored in `Market` (ARCH §2.3.4 observation group), for the
/// settlement box the next `sol_settle` evaluates (after its own roll, which the frontend
/// cannot anticipate: the box shown is the one of the stored state).
#[wasm_bindgen]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OracleState {
    ema: Ema,
    ring: Ring,
}

#[wasm_bindgen]
impl OracleState {
    /// `P_ema`, `L_ema` and `ema_ts` of the market, with an empty ring.
    #[wasm_bindgen(constructor)]
    pub fn new(p_ema: u128, l_ema: u128, ema_ts: i64) -> OracleState {
        OracleState {
            ema: Ema {
                price: p_ema,
                liq: l_ema,
                liq_slow: l_ema,
                ts: ema_ts,
                slow_ts: ema_ts,
                ..Ema::default()
            },
            ring: Ring::default(),
        }
    }

    /// Appends a ring entry as the newest (entries oldest first), dropping the oldest of a
    /// full ring. `(x, y)` is the cached `reprice(l, p)`.
    #[allow(clippy::too_many_arguments)]
    pub fn push(
        &mut self,
        ts: i64,
        p: u128,
        l: u128,
        x: u128,
        y: u128,
        lo: u128,
        hi: u128,
        llo: u128,
        lhi: u128,
    ) -> Result<(), JsError> {
        let e = RingEntry {
            p,
            l,
            x,
            y,
            lo,
            hi,
            llo,
            lhi,
            ts,
            _pad: [0; 8],
        };
        Ok(self.ring.push(e)?)
    }

    /// Number of ring entries (at most `RING_LEN`).
    #[wasm_bindgen(getter)]
    pub fn len(&self) -> usize {
        self.ring.len()
    }

    /// Whether the ring has no entry.
    #[wasm_bindgen(js_name = isEmpty)]
    pub fn is_empty(&self) -> bool {
        self.ring.is_empty()
    }

    /// The settlement box at `now` (`settle::box_of`): the EMA point widened to the ring's
    /// lower medians of the bucket lows and upper medians of the highs, or the EMA point
    /// alone after `RING_LEN` seconds without an integration **if the venue cannot execute**
    /// (`executable = false`; an executable venue always keeps the medians, M2-SEC-1).
    /// Throws on an empty ring.
    #[wasm_bindgen(js_name = settleBox)]
    pub fn settle_box(&self, now: i64, executable: bool) -> Result<SettleBox, JsError> {
        Ok(SettleBox(msettle::box_of(
            &self.ema, &self.ring, now, executable,
        )?))
    }

    /// The ring's medians `(P_mlo, P_mhi, L_mlo, L_mhi)` as a box (`observe::ring_medians`).
    #[wasm_bindgen(js_name = ringMedians)]
    pub fn ring_medians(&self) -> Result<SettleBox, JsError> {
        let (p_lo, p_hi, l_lo, l_hi) = observe::ring_medians(&self.ring)?;
        Ok(SettleBox(msettle::SettleBox {
            p_lo,
            p_hi,
            l_lo,
            l_hi,
        }))
    }

    /// Minimum per-second EMA growth over the newest `n` steps, bps/s
    /// (`observe::sustained_growth`, the κ_eff input).
    #[wasm_bindgen(js_name = sustainedGrowth)]
    pub fn sustained_growth(&self, n: u8) -> Result<u64, JsError> {
        Ok(observe::sustained_growth(&self.ring, n)?)
    }
}

/// Entries of the EMA ring (`RING_LEN`); the box is the EMA point after this many seconds
/// without an integration.
#[wasm_bindgen(js_name = ringLen)]
pub fn ring_len() -> usize {
    consts::RING_LEN
}

/// FV's `p'` at one evaluated state (`settle::pprime`; ECON §8.6, O15, R4A-3): the expiry
/// fee if `expired` and `⌊V_eff·10⁴/B̂_FV⌋ ≥ L` there, else the penalty.
#[wasm_bindgen]
pub fn pprime(
    expired: bool,
    v_eff: u64,
    b_fv: u64,
    cr_liq_bps: u32,
    penalty_bps: u32,
    expiry_fee_bps: u32,
) -> u32 {
    msettle::pprime(
        expired,
        v_eff,
        b_fv,
        cr_liq_bps,
        penalty_bps,
        expiry_fee_bps,
    )
}

/// The box amount check of an S2/S3 settlement (`settle::amount_ok`; ECON §5.6):
/// `(x_hi − x_lo)·10⁴ ≤ fv_dev_bps·x_lo`.
#[wasm_bindgen(js_name = amountOk)]
pub fn amount_ok(x_lo: u64, x_hi: u64, fv_dev_bps: u32) -> bool {
    msettle::amount_ok(x_lo, x_hi, fv_dev_bps)
}

fn tier_of(tier: Option<u8>) -> Result<Option<Tier>, MathError> {
    tier.map(|t| Tier::from_u8(t).ok_or(MathError::InvalidInput))
        .transpose()
}

fn label(v: Violation) -> String {
    match v.tier {
        Some(t) => format!("{}:{}", ["A", "B", "C", "D"][t.index()], v.rule.name()),
        None => v.rule.name().into(),
    }
}

/// A protocol parameter set (`params::ParamSet`: `Params`, the four `TierParams` and the
/// market-level EMA parameters), starting from the ECON §15 defaults. Parameters are
/// addressed by their ECON §15 names; tier parameters take the tier (`0` = A … `3` = D).
#[wasm_bindgen]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParamSet(mparams::ParamSet);

#[wasm_bindgen]
impl ParamSet {
    /// The defaults (they validate).
    #[wasm_bindgen(constructor)]
    pub fn new() -> ParamSet {
        ParamSet(mparams::ParamSet::DEFAULT)
    }

    /// The value of `name` (for `tier`); `undefined` for an unknown name.
    pub fn get(&self, name: &str, tier: Option<u8>) -> Result<Option<u64>, JsError> {
        Ok(self.0.get(name, tier_of(tier)?))
    }

    /// Sets `name` (for `tier`) without validating; throws `"invalid input"` for an
    /// unknown name or a value beyond the field's type.
    pub fn set(&mut self, name: &str, tier: Option<u8>, value: u64) -> Result<(), JsError> {
        Ok(self.0.set(name, tier_of(tier)?, value)?)
    }

    /// Every failed check (`params::check`: the sim's rules, then the ones for parameters the
    /// sim does not model) as `"rule"` or `"T:rule"` (tier `T` in A–D); empty when the set
    /// validates. Rule names are the bounded parameter's name or the relational check's
    /// (`params::Rule::name`).
    pub fn violations(&self) -> Vec<String> {
        let mut out = Vec::new();
        mparams::check(&self.0, |v| out.push(label(v)));
        out
    }

    /// The first check the program refuses the set on (`params::validate`, then
    /// `params::validate_econ`), or `undefined` if the program accepts it.
    pub fn validate(&self) -> Option<String> {
        mparams::validate(&self.0)
            .and_then(|()| mparams::validate_econ(&self.0))
            .err()
            .map(label)
    }

    /// The first violation `shortwin_sim.validate_params` reports (`params::validate`, the
    /// T-CONFIG gate), or `undefined` if the sim accepts the set.
    #[wasm_bindgen(js_name = validateSim)]
    pub fn validate_sim(&self) -> Option<String> {
        mparams::validate(&self.0).err().map(label)
    }
}

impl Default for ParamSet {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const WAD: u128 = 1_000_000_000_000_000_000;

    #[test]
    fn box_and_medians() {
        let mut o = OracleState::new(2 * WAD, 1_000, 10);
        assert!(o.is_empty());
        for (k, (lo, hi)) in [(1, 3), (2, 5), (1, 4)].into_iter().enumerate() {
            let ts = 8 + i64::try_from(k).unwrap();
            o.push(ts, 2 * WAD, 1_000, 0, 0, lo * WAD, hi * WAD, 900, 1_100)
                .unwrap();
        }
        assert_eq!(o.len(), 3);
        let m = o.ring_medians().unwrap();
        assert_eq!((m.p_lo(), m.p_hi()), (WAD, 4 * WAD));
        let b = o.settle_box(11, true).unwrap();
        assert_eq!(
            (b.p_lo(), b.p_hi(), b.l_lo(), b.l_hi()),
            (WAD, 4 * WAD, 900, 1_100)
        );
        assert!(b.contains(2 * WAD, 1_000) && !b.is_point());
        // No integration for more than RING_LEN seconds: the EMA point on a venue that
        // cannot execute, the medians on one that can (M2-SEC-1).
        let far = 10 + i64::try_from(ring_len()).unwrap() + 1;
        assert!(o.settle_box(far, false).unwrap().is_point());
        assert!(!o.settle_box(far, true).unwrap().is_point());
    }

    #[test]
    fn pprime_and_amounts() {
        assert_eq!(pprime(true, 1_250, 1_000, 12_500, 500, 200), 200);
        assert_eq!(pprime(true, 1_249, 1_000, 12_500, 500, 200), 500);
        assert_eq!(pprime(false, 5_000, 1_000, 12_500, 500, 200), 500);
        assert!(amount_ok(10_000, 10_100, 100));
        assert!(!amount_ok(10_000, 10_101, 100));
        assert!(amount_ok(10_000, 9_000, 0));
    }

    #[test]
    fn params_by_name() {
        let mut ps = ParamSet::new();
        assert_eq!(ps.validate(), None);
        assert!(ps.violations().is_empty());
        assert_eq!(ps.get("kappa_liq_bps", None).unwrap(), Some(2_500));
        assert_eq!(ps.get("cr_liq_bps", Some(3)).unwrap(), Some(17_500));
        ps.set("kappa_liq_bps", None, 2_000).unwrap();
        ps.set("cr_liq_bps", Some(1), 13_000).unwrap();
        assert_eq!(ps.validate().as_deref(), Some("kappa_liq_bps"));
        assert_eq!(ps.validate_sim().as_deref(), Some("kappa_liq_bps"));
        assert_eq!(ps.violations(), ["kappa_liq_bps", "B:tier_spec_cr"]);
        // A per-tier bound: since the M1 doc sync the sim checks it too (ECON §15, TIER_BOUNDS).
        let mut ps = ParamSet::new();
        ps.set("cr_open_bps", Some(0), 100_001).unwrap();
        assert_eq!(ps.validate().as_deref(), Some("A:cr_open_bps"));
        assert_eq!(ps.validate_sim().as_deref(), Some("A:cr_open_bps"));
        assert_eq!(ps.violations(), ["A:cr_open_bps"]);
        // An ECON §15 bound the sim does not model (`validate_econ`): the program refuses, the
        // sim accepts.
        let mut ps = ParamSet::new();
        ps.set("min_notional", None, 9_999_999).unwrap();
        assert_eq!(ps.validate().as_deref(), Some("min_notional"));
        assert_eq!(ps.validate_sim(), None);
        assert_eq!(ps.violations(), ["min_notional"]);
    }
}
