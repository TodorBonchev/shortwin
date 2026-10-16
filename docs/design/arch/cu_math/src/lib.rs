//! CU micro-benchmarks of shortwin-math primitives (scratch; ARCH v0.4 §6.1).
//! instruction data: [bench id u8][iterations u32 LE]
#![no_std]
use core::hint::black_box;
use shortwin_math::fees::{CpmmFees, FeeBps};
use shortwin_math::num::{mul_div, mul_div_u64, sqrt_u128, Rounding, U256};
use shortwin_math::observe::{obs_median, sustained_growth};
use shortwin_math::observe::{integrate, record, Bucket, Ema, EmaParams, LastBucket, Ring};
use shortwin_math::quote::{self, cpmm, ray_v4, sol_leg};
use shortwin_math::rent::{self, Basis, PosRent, RentTerms};
use shortwin_math::risk;

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! { loop {} }

const WAD: u128 = 1_000_000_000_000_000_000;

#[no_mangle]
pub unsafe extern "C" fn entrypoint(input: *mut u8) -> u64 {
    // input: u64 n_accounts (0), u64 data_len, data...
    let data = input.add(16);
    let id = core::ptr::read_volatile(data);
    let n = u32::from_le_bytes([
        core::ptr::read_volatile(data.add(1)),
        core::ptr::read_volatile(data.add(2)),
        core::ptr::read_volatile(data.add(3)),
        core::ptr::read_volatile(data.add(4)),
    ]);
    run(id, n);
    0
}

#[inline(never)]
fn run(id: u8, n: u32) {
    // representative Tier A PumpSwap state: X = 150 M tokens (6 dp), Y = 2,500 SOL
    let x: u128 = black_box(150_000_000_000_000);
    let y: u128 = black_box(2_500_000_000_000);
    let q: u64 = black_box(3_704_000_000_000);
    let p: u128 = black_box(y * WAD / x);
    let l: u128 = black_box(19_364_916_731_037);
    let fees = FeeBps { lp: 20, protocol: 5, creator: 95 };
    let cf = CpmmFees { trade: 2_500, creator: 0, creator_on_input: true, protocol: 120_000, fund: 40_000 };
    let pos = PosRent { q, j_open: [0; 3], terms: RentTerms { base_bps: 1_000, slope1_bps: 5_000, slope2_bps: 100_000 }, r_min: 0 };
    let mut basis = Basis { j: [black_box(p * 86_400), black_box(p * 69_120), black_box(0)] };
    let params = EmaParams::DEFAULT;
    let mut ema = Ema::new(1, p, l).unwrap_or_default();
    let mut ring = Ring::new(1, p, l).unwrap_or_default();
    let mut bucket = Bucket::EMPTY;
    let mut last = LastBucket::NONE;
    let mut acc: u128 = 0;
    let mut t: i64 = 2;
    let mut full = Ring::new(1, p, l).unwrap_or_default();
    {
        let (mut e2, mut b2, mut l2) = (Ema::new(1, p, l).unwrap_or_default(), Bucket::EMPTY, LastBucket::NONE);
        for k in 0..9i64 {
            let _ = record(&mut b2, 2 + k, p + (k as u128) * 7_000_000_000_000 % 50_000_000_000_000, l + k as u128);
            let _ = integrate(&mut e2, &mut full, &mut b2, &mut l2, 3 + k, &params);
        }
    }
    for i in 0..n {
        let j = black_box(i as u128);
        match id {
            0 => { acc = acc.wrapping_add(black_box(j)); }
            1 => { acc = acc.wrapping_add(mul_div(y + j, q as u128, x - q as u128, Rounding::Ceil).unwrap_or(0)); }
            2 => { acc = acc.wrapping_add(mul_div(p * 1_000 + j, WAD, 7_000_000_000_000_000_000_000, Rounding::Floor).unwrap_or(0)); }
            3 => { acc = acc.wrapping_add(sol_leg::buy_exact_out(q, x + j, y, false, &fees).map(|b| b.cost as u128).unwrap_or(0)); }
            4 => { acc = acc.wrapping_add(cpmm::buy_exact_out(q, y + j, x, &cf).map(|b| b.cost as u128).unwrap_or(0)); }
            5 => { acc = acc.wrapping_add(ray_v4::buy_exact_out(q, y + j, x, 25, 10_000).map(|b| b.cost as u128).unwrap_or(0)); }
            6 => { acc = acc.wrapping_add(quote::reprice(l + j, p).map(|(a, b)| a ^ b).unwrap_or(0)); }
            7 => { acc = acc.wrapping_add(quote::liquidity(x + j, y)); }
            8 => { acc = acc.wrapping_add(sqrt_u128((x + j) * y, Rounding::Floor)); }
            9 => { basis.j[2] = j; acc = acc.wrapping_add(rent::due(&pos, &basis).unwrap_or(0) as u128); }
            10 => { acc = acc.wrapping_add(rent::accrue(&mut basis, p + j, 8_500, 8_000, 1).map(|d| d[0]).unwrap_or(0)); }
            11 => {
                let _ = record(&mut bucket, t, p + j * 1_000_000, l);
                t += 1;
                let _ = integrate(&mut ema, &mut ring, &mut bucket, &mut last, t, &params);
                acc = acc.wrapping_add(ema.price);
            }
            12 => { acc = acc.wrapping_add(shortwin_math::observe::obs_median(black_box(&ring)).ok().flatten().map(|(a, _)| a).unwrap_or(0)); }
            13 => { acc = acc.wrapping_add(risk::sigma_xy(Some(q / 40 + j as u64), q, x, y).unwrap_or(0) as u128); }
            14 => { // v0.3 per-entry EMA leg: reprice + quote + CR compare
                let r = quote::reprice(l + j, p).and_then(|(xx, yy)| sol_leg::buy_exact_out(q, xx, yy, false, &fees));
                acc = acc.wrapping_add(risk::cr_below(120_000_000_000, r.map(|b| b.cost), 12_500).unwrap_or(false) as u128);
            }
            15 => { // v0.4 cached entry: quote + CR compare
                let r = sol_leg::buy_exact_out(q, x + j, y, false, &fees);
                acc = acc.wrapping_add(risk::cr_below(120_000_000_000, r.map(|b| b.cost), 12_500).unwrap_or(false) as u128);
            }
            16 => { acc = acc.wrapping_add(U256::from_product(p + j, 10_000 + 2_500).div_rem_u128(10_000).map(|(a, _)| a.low_u128()).unwrap_or(0)); }
            17 => { acc = acc.wrapping_add(mul_div_u64(q + j as u64, 95, 10_000, Rounding::Ceil).unwrap_or(0) as u128); }
            18 => { let a = black_box(q + j as u64); acc = acc.wrapping_add(((a as u128 * 95 + 9_999) / 10_000) as u128); }
            19 => { let a = black_box(q + j as u64); let b: u64 = black_box(95); acc = acc.wrapping_add((a.saturating_mul(b).div_ceil(black_box(10_000u64))) as u128); }
            20 => { acc = acc.wrapping_add(obs_median(black_box(&full)).ok().flatten().map(|(a, _)| a).unwrap_or(0) ^ j); }
            21 => { acc = acc.wrapping_add(sustained_growth(black_box(&full), 3).unwrap_or(0) as u128 ^ j); }
            22 => { basis.j[2] = j; acc = acc.wrapping_add(rent::credited_for(&pos, &basis, 1_500).unwrap_or(0) as u128); }
            _ => {}
        }
    }
    black_box(acc);
}
