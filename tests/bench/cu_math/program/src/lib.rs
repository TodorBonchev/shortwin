//! T-CU-MATH bench program (ARCHITECTURE §6.1, §9 M0b): runs one `shortwin-math`
//! primitive `n` times on representative Tier A PumpSwap magnitudes (the inputs of
//! `docs/design/arch/cu_math`, V18). Instruction data: `[bench id u8][n u32 LE]`, no
//! accounts. The runner measures `(CU(n) − CU(0))/n` and subtracts the loop overhead
//! (bench 0).
//!
//! Every bench is its own `#[inline(never)]` function so that no SBF stack frame holds more
//! than one ring or vault. On the host the crate is a plain library (no entrypoint), so the
//! workspace builds and lints there too.
#![cfg_attr(target_os = "solana", no_std)]

use core::hint::black_box;

use shortwin_math::fees::{CpmmFees, FeeBps};
use shortwin_math::num::{Rounding, U256, WAD, mul_div, mul_div_u64, sqrt_u256};
use shortwin_math::observe::{
    Bucket, Ema, EmaParams, LastBucket, Ring, integrate, record, ring_medians, sustained_growth,
};
use shortwin_math::persist::liq_ema_leg;
use shortwin_math::pool::{self, LOCK_BUCKET_S, Vault};
use shortwin_math::quote::{self, cpmm, ray_v4, sol_leg};
use shortwin_math::rent::{self, Basis, PosRent, RentTerms};
use shortwin_math::risk;
use shortwin_math::settle::box_of;

/// Tier A PumpSwap state: X = 150 M tokens (6 dp), Y = 2,500 SOL.
const X: u128 = 150_000_000_000_000;
const Y: u128 = 2_500_000_000_000;
/// A position of 3.7 M tokens (2.5 % of X).
const Q: u64 = 3_704_000_000_000;
/// `L = ⌊√(X·Y)⌋`.
const L: u128 = 19_364_916_731_037;
/// Vault `V_eff` of the CR comparisons (≈ 1.6 × the buyback).
const V_EFF: u64 = 120_000_000_000;
/// A vault below `L` on every ring entry: the EMA leg's worst case (all `n` entries read).
const V_EFF_BELOW: u64 = 10_000_000_000;
const FEES: FeeBps = FeeBps {
    lp: 20,
    protocol: 5,
    creator: 95,
};
const CPMM: CpmmFees = CpmmFees {
    trade: 2_500,
    creator: 0,
    creator_on_input: true,
    protocol: 120_000,
    fund: 40_000,
};
const TERMS_A: RentTerms = RentTerms {
    base_bps: 1_000,
    slope1_bps: 5_000,
    slope2_bps: 100_000,
};

/// Bench ids (the runner's table names each).
pub mod id {
    pub const LOOP: u8 = 0;
    pub const MUL_DIV: u8 = 1;
    pub const MUL_DIV_U256: u8 = 2;
    pub const MUL_DIV_U64: u8 = 3;
    pub const NATIVE_U64: u8 = 4;
    pub const QUOTE_PUMPSWAP: u8 = 5;
    pub const QUOTE_CPMM: u8 = 6;
    pub const QUOTE_RAY_V4: u8 = 7;
    pub const REPRICE: u8 = 8;
    pub const LIQUIDITY: u8 = 9;
    pub const SQRT_U256: u8 = 10;
    pub const RENT_DUE: u8 = 11;
    pub const RENT_CREDITED_FOR: u8 = 12;
    pub const RENT_ACCRUE: u8 = 13;
    pub const EMA_ROLL: u8 = 14;
    pub const RING_MEDIANS: u8 = 15;
    pub const SIGMA_XY: u8 = 16;
    pub const LEG_CACHED: u8 = 17;
    pub const LEG_REPRICED: u8 = 18;
    pub const BOX_OF: u8 = 19;
    pub const LIQ_EMA_LEG: u8 = 20;
    pub const SUSTAINED_GROWTH: u8 = 21;
    pub const CAL_ADVANCE: u8 = 22;
    pub const CURVE_SYNTHETIC: u8 = 23;
    pub const MIGRATING_FV: u8 = 24;
    pub const LAST: u8 = MIGRATING_FV;
}

/// pump's curve fee (one tier: lp 0, protocol 95, creator 30 bps).
const CURVE_FEES: FeeBps = FeeBps {
    lp: 0,
    protocol: 95,
    creator: 30,
};
/// pump's constant product `vT·vQ` at creation (1.073e15 tokens × 30 SOL) and the offsets
/// `vT − rT`, `vQ − rQ` (pump-final §5.2).
const CURVE_K: u128 = 1_073_000_000_000_000 * 30_000_000_000;
const CURVE_T_OFFSET: u128 = 279_900_000_000_000;
const CURVE_Q_OFFSET: u128 = 30_000_000_000;
/// A near-complete curve: 2 M tokens left (`rT`), and a position of 4 M past it.
const CURVE_RT: u128 = 2_000_000_000_000;
const Q_PAST_RT: u64 = 4_000_000_000_000;
/// `Global.pool_migration_fee` and the standard pool tokens (206.9e12).
const MIGRATION_FEE: u64 = 15_000_001;
const STANDARD_TOKENS: u128 = 206_900_000_000_000;

/// The curve with `rt` tokens left (`vT·vQ = K`, the offsets constant), live.
fn curve_at_rt(rt: u128) -> quote::CurveState {
    let vt = CURVE_T_OFFSET + rt;
    let vq = CURVE_K / vt;
    quote::CurveState {
        vt,
        vq,
        rt,
        rq: vq - CURVE_Q_OFFSET,
        complete: false,
    }
}

fn p_spot() -> u128 {
    black_box(Y * WAD / X)
}

/// A full ring of distinct entries (a pump of 5 % per second, each bucket 2 % wide), with
/// the EMA after it.
#[inline(never)]
fn full_ring() -> (Ema, Ring) {
    let p = p_spot();
    let mut ema = Ema::new(1, p, L).unwrap_or_default();
    let mut ring = Ring::new(1, p, L).unwrap_or_default();
    let (mut bucket, mut last) = (Bucket::EMPTY, LastBucket::NONE);
    let params = EmaParams::DEFAULT;
    for k in 0..9u32 {
        let t = 2 + i64::from(k);
        let pk = p * (100 + 5 * u128::from(k)) / 100;
        let _ = record(&mut bucket, t, pk, L + u128::from(k));
        let _ = record(&mut bucket, t, pk + pk / 50, L + 2 * u128::from(k));
        let _ = integrate(&mut ema, &mut ring, &mut bucket, &mut last, t + 1, &params);
    }
    (ema, ring)
}

#[inline(never)]
fn loop_only(n: u32) -> u128 {
    let mut acc = 0u128;
    for i in 0..n {
        acc = acc.wrapping_add(black_box(u128::from(i)));
    }
    acc
}

#[inline(never)]
fn arith(id: u8, n: u32) -> u128 {
    let p = p_spot();
    let mut acc = 0u128;
    for i in 0..n {
        let j = black_box(u128::from(i));
        let j64 = black_box(u64::from(i));
        let v = match id {
            id::MUL_DIV => {
                mul_div(Y + j, u128::from(Q), X - u128::from(Q), Rounding::Ceil).unwrap_or(0)
            }
            id::MUL_DIV_U256 => mul_div(
                p * 1_000 + j,
                WAD,
                7_000_000_000_000_000_000_000,
                Rounding::Floor,
            )
            .unwrap_or(0),
            id::MUL_DIV_U64 => {
                u128::from(mul_div_u64(Q + j64, 95, 10_000, Rounding::Ceil).unwrap_or(0))
            }
            id::NATIVE_U64 => {
                let a = black_box(Q + j64);
                let b: u64 = black_box(95);
                u128::from(a.saturating_mul(b).div_ceil(black_box(10_000u64)))
            }
            id::LIQUIDITY => quote::liquidity(X + j, Y).unwrap_or(0),
            id::SQRT_U256 => sqrt_u256(U256::from_product(X + j, Y), Rounding::Floor).unwrap_or(0),
            _ => 0,
        };
        acc = acc.wrapping_add(v);
    }
    acc
}

#[inline(never)]
fn quotes(id: u8, n: u32) -> u128 {
    let p = p_spot();
    let mut acc = 0u128;
    for i in 0..n {
        let j = black_box(u128::from(i));
        let v = match id {
            id::QUOTE_PUMPSWAP => {
                sol_leg::buy_exact_out(Q, X + j, Y, false, &FEES).map_or(0, |b| u128::from(b.cost))
            }
            id::QUOTE_CPMM => {
                cpmm::buy_exact_out(Q, Y + j, X, &CPMM).map_or(0, |b| u128::from(b.cost))
            }
            id::QUOTE_RAY_V4 => {
                ray_v4::buy_exact_out(Q, Y + j, X, 25, 10_000).map_or(0, |b| u128::from(b.cost))
            }
            id::REPRICE => quote::reprice(L + j, p).map_or(0, |(a, b)| a ^ b),
            id::SIGMA_XY => {
                u128::from(risk::sigma_xy(Some(Q / 40 + u64::from(i)), Q, X, Y).unwrap_or(0))
            }
            // v0.4: the EMA leg quotes on an entry's cached (x, y).
            id::LEG_CACHED => {
                let c = sol_leg::buy_exact_out(Q, X + j, Y, false, &FEES).map(|b| b.cost);
                let below = risk::cr_below(V_EFF, c, 12_500).unwrap_or(false);
                u128::from(c.unwrap_or(0)) ^ u128::from(below)
            }
            // A quote on reprice(L, P) + CR: a settlement-box corner (and v0.3's EMA leg).
            id::LEG_REPRICED => {
                let c = quote::reprice(L + j, p)
                    .and_then(|(xx, yy)| sol_leg::buy_exact_out(Q, xx, yy, false, &FEES))
                    .map(|b| b.cost);
                let below = risk::cr_below(V_EFF, c, 12_500).unwrap_or(false);
                u128::from(c.unwrap_or(0)) ^ u128::from(below)
            }
            _ => 0,
        };
        acc = acc.wrapping_add(v);
    }
    acc
}

#[inline(never)]
fn rents(id: u8, n: u32) -> u128 {
    let p = p_spot();
    let pos = PosRent {
        q: Q,
        j_open: [0; 3],
        terms: TERMS_A,
        rent_protocol_bps: 1_500,
        r_min: 0,
    };
    let mut basis = Basis {
        j: [black_box(p * 86_400), black_box(p * 69_120), black_box(0)],
    };
    let mut acc = 0u128;
    for i in 0..n {
        let j = black_box(u128::from(i));
        let v = match id {
            id::RENT_DUE => {
                basis.j[2] = j;
                u128::from(rent::due(&pos, &basis).unwrap_or(0))
            }
            id::RENT_CREDITED_FOR => {
                basis.j[2] = j;
                u128::from(rent::credited_for(&pos, &basis).unwrap_or(0))
            }
            id::RENT_ACCRUE => rent::accrue(&mut basis, p + j, 8_500, 8_000, 1).map_or(0, |d| d[0]),
            _ => 0,
        };
        acc = acc.wrapping_add(v);
    }
    acc
}

/// One EMA roll per iteration: a recorded second, then `integrate` (EMA step, cached
/// reprojection, ring push).
#[inline(never)]
fn ema_roll(n: u32) -> u128 {
    let p = p_spot();
    let params = EmaParams::DEFAULT;
    let mut ema = Ema::new(1, p, L).unwrap_or_default();
    let mut ring = Ring::new(1, p, L).unwrap_or_default();
    let (mut bucket, mut last) = (Bucket::EMPTY, LastBucket::NONE);
    let mut t: i64 = 2;
    let mut acc = 0u128;
    for i in 0..n {
        let j = black_box(u128::from(i));
        let _ = record(&mut bucket, t, p + j * 1_000_000, L);
        t += 1;
        let _ = integrate(&mut ema, &mut ring, &mut bucket, &mut last, t, &params);
        acc = acc.wrapping_add(ema.price);
    }
    acc
}

#[inline(never)]
fn ring_ops(id: u8, n: u32) -> u128 {
    let (ema, ring) = full_ring();
    let now = ema.ts;
    let mut acc = 0u128;
    for i in 0..n {
        let j = black_box(u128::from(i));
        let ring = black_box(&ring);
        let v = match id {
            id::RING_MEDIANS => ring_medians(ring).map_or(0, |m| m.0 ^ m.3),
            // An executable venue (M2-SEC-1): the medians path, never the collapsed point.
            id::BOX_OF => box_of(&ema, ring, now, true).map_or(0, |b| b.p_lo ^ b.l_hi),
            id::LIQ_EMA_LEG => {
                let below = |x: u128, y: u128| {
                    let r = sol_leg::buy_exact_out(Q, x, y, false, &FEES);
                    risk::cr_below(V_EFF_BELOW, r.map(|b| b.cost), 12_500)
                };
                u128::from(liq_ema_leg(ring, below, 3).unwrap_or(false))
            }
            id::SUSTAINED_GROWTH => u128::from(sustained_growth(ring, 3).unwrap_or(0)),
            _ => 0,
        };
        acc = acc.wrapping_add(v ^ j);
    }
    acc
}

/// pump-final §5.2–§5.4 on a near-complete curve: `B̂_v3` past `rT` (the curve leg `rT` plus the
/// pool part on the would-be pool, both at the curve's fee), and a migrating market's FV after a
/// synthetic pool part (the pool `migrate_v2` will create, ρ-scaled, at its PumpSwap tier).
#[inline(never)]
fn curve_quotes(id: u8, n: u32) -> u128 {
    let live = curve_at_rt(CURVE_RT);
    let live_m = quote::MigrationParams {
        migration_fee: MIGRATION_FEE,
        pool_tokens: STANDARD_TOKENS,
        post_complete_quote_in: 0,
    };
    // The same curve completed by a buy of `rT + 2 M` (pcB = 2 M, pcQ its pool part's quote).
    let pc_b: u128 = 2_000_000_000_000;
    let done = quote::CurveState {
        rt: 0,
        vt: CURVE_T_OFFSET,
        vq: CURVE_K / CURVE_T_OFFSET,
        rq: CURVE_K / CURVE_T_OFFSET - CURVE_Q_OFFSET,
        complete: true,
    };
    let done_m = quote::MigrationParams {
        migration_fee: MIGRATION_FEE,
        pool_tokens: STANDARD_TOKENS - pc_b,
        post_complete_quote_in: 1_200_000_000,
    };
    let tiers = [shortwin_math::fees::FeeTier {
        threshold: 0,
        fees: FEES,
    }];
    let schedule = shortwin_math::fees::PammFeeSchedule {
        tiers: &tiers,
        base_supply: 1_000_000_000_000_000,
        creator_fee_configurable: false,
        pool_creator_fee_bps: 0,
        coin_creator_is_default: false,
        cap_bps: Some(300),
    };
    let p = black_box(done.vq * WAD / done.vt);
    let mut acc = 0u128;
    for i in 0..n {
        let j = black_box(u64::from(i));
        let v = match id {
            id::CURVE_SYNTHETIC => {
                quote::curve_synthetic(Q_PAST_RT + j, &live, &CURVE_FEES, &CURVE_FEES, &live_m)
                    .map_or(0, |b| u128::from(b.cost))
            }
            id::MIGRATING_FV => quote::migrating_fv(Q_PAST_RT + j, p, &done, &done_m, &schedule)
                .map_or(0, |b| u128::from(b.cost)),
            _ => 0,
        };
        acc = acc.wrapping_add(v);
    }
    acc
}

/// `cal_advance` over one elapsed day with a live bucket to drop.
#[inline(never)]
fn cal_advance(n: u32) -> u128 {
    let mut vault = Vault::new();
    vault.cal_day = 10;
    let mut acc = 0u128;
    for i in 0..n {
        let day = 10 + i64::from(i);
        vault.lock_cal[usize::try_from(day).unwrap_or(0) % vault.lock_cal.len()] = 7;
        vault.lock_norm = vault.lock_norm.wrapping_add(7);
        let _ = pool::cal_advance(&mut vault, black_box((day + 1) * LOCK_BUCKET_S));
        acc = acc.wrapping_add(vault.lock_norm ^ u128::try_from(vault.cal_day).unwrap_or(0));
    }
    acc
}

/// Runs bench `id` `n` times.
pub fn run(id: u8, n: u32) -> u128 {
    match id {
        id::LOOP => loop_only(n),
        id::MUL_DIV
        | id::MUL_DIV_U256
        | id::MUL_DIV_U64
        | id::NATIVE_U64
        | id::LIQUIDITY
        | id::SQRT_U256 => arith(id, n),
        id::QUOTE_PUMPSWAP
        | id::QUOTE_CPMM
        | id::QUOTE_RAY_V4
        | id::REPRICE
        | id::SIGMA_XY
        | id::LEG_CACHED
        | id::LEG_REPRICED => quotes(id, n),
        id::RENT_DUE | id::RENT_CREDITED_FOR | id::RENT_ACCRUE => rents(id, n),
        id::EMA_ROLL => ema_roll(n),
        id::RING_MEDIANS | id::BOX_OF | id::LIQ_EMA_LEG | id::SUSTAINED_GROWTH => ring_ops(id, n),
        id::CAL_ADVANCE => cal_advance(n),
        id::CURVE_SYNTHETIC | id::MIGRATING_FV => curve_quotes(id, n),
        _ => 0,
    }
}

#[cfg(target_os = "solana")]
mod entry {
    #[panic_handler]
    fn panic(_: &core::panic::PanicInfo) -> ! {
        loop {}
    }

    /// Raw entrypoint: `input` = `[u64 n_accounts = 0][u64 data_len][data]…`.
    ///
    /// # Safety
    /// Called by the loader with a valid serialized input region.
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn entrypoint(input: *mut u8) -> u64 {
        // SAFETY: the loader's input region holds at least the two u64 headers and the
        // 5-byte instruction data the runner sends.
        let (id, n) = unsafe {
            let data = input.add(16);
            let id = core::ptr::read_volatile(data);
            let n = u32::from_le_bytes([
                core::ptr::read_volatile(data.add(1)),
                core::ptr::read_volatile(data.add(2)),
                core::ptr::read_volatile(data.add(3)),
                core::ptr::read_volatile(data.add(4)),
            ]);
            (id, n)
        };
        core::hint::black_box(super::run(id, n));
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every bench computes something on the host (no primitive returns an error on these
    /// inputs, so the SBF numbers measure the full path).
    #[test]
    fn benches_run_on_the_host() {
        assert_eq!(quote::liquidity(X, Y), Ok(L));
        let (ema, ring) = full_ring();
        assert_eq!(ring.len(), 8);
        assert!(!box_of(&ema, &ring, ema.ts, true).expect("box").is_point());
        assert!(sustained_growth(&ring, 3).expect("growth") > 0);
        assert!(sol_leg::buy_exact_out(Q, X, Y, false, &FEES).is_some());
        let below = |x: u128, y: u128| {
            let r = sol_leg::buy_exact_out(Q, x, y, false, &FEES);
            risk::cr_below(V_EFF_BELOW, r.map(|b| b.cost), 12_500)
        };
        assert_eq!(liq_ema_leg(&ring, below, 3), Ok(true));
        assert!(quote::reprice(L, p_spot()).is_some());
        // B̂_v3 past rT: the curve leg plus a pool part, dearer than buying rT alone.
        let live = curve_at_rt(CURVE_RT);
        let m = quote::MigrationParams {
            migration_fee: MIGRATION_FEE,
            pool_tokens: STANDARD_TOKENS,
            post_complete_quote_in: 0,
        };
        let v3 =
            quote::curve_synthetic(Q_PAST_RT, &live, &CURVE_FEES, &CURVE_FEES, &m).expect("B̂_v3");
        let curve_part = quote::curve_synthetic(
            u64::try_from(CURVE_RT).unwrap(),
            &live,
            &CURVE_FEES,
            &CURVE_FEES,
            &m,
        )
        .expect("rT");
        assert!(v3.cost > curve_part.cost);
        for id in 1..=id::LAST {
            assert_ne!(run(id, 3), 0, "bench {id}");
        }
    }
}
