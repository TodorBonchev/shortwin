//! T-LAYOUT (ARCH §2.3 zero-copy layout rule, A67; §8.2): compile-time assertions of the size
//! and of every field offset of every zero-copy type.
//!
//! These `const` blocks are compiled on all three targets — sbpfv3 (`anchor build --arch v3`),
//! x86_64 (`cargo build` / `cargo test`) and wasm32 (`cargo check -p shortwin --target
//! wasm32-unknown-unknown`, the T-LAYOUT CI step) — so a layout that differs on any of them
//! fails that build. Offsets are written as in ARCH §2.3, i.e. **including** the 8-byte
//! account discriminator for account types (`acct` rows) and from 0 for nested types. Borsh
//! account sizes (`ShortPosition` 384, `LenderPosition` 241, `ShareEpoch` 143,
//! `WalletExposure` 81, `OracleCache` 45) are asserted here too.
//!
//! `tests::raw_128_bit_fields` is the ARCH §2.3 / §8.4 lint: no zero-copy file declares a raw
//! `u128` / `i128` field.

use core::mem::{align_of, offset_of, size_of};

use crate::state::config::{
    EmaConfig, GlobalConfig, KinkConfig, Params, PendingChange, TierParams, PENDING_PAYLOAD_LEN,
};
use crate::state::epoch::ShareEpoch;
use crate::state::exposure::WalletExposure;
use crate::state::lender::LenderPosition;
use crate::state::market::{Market, RingEntry};
use crate::state::oracle::OracleCache;
use crate::state::pod::{PodI128, PodU128};
use crate::state::position::ShortPosition;
use crate::state::vault::MarketVault;
use crate::state::venue_extras::{ExtraEntry, ExtrasSet, IxExtras, SeedPart, VenueExtras};
use crate::state::venue_guard::{VenueGuard, GUARD_ELF_FIRST, GUARD_ELF_LEN, GUARD_ELF_VENUES};

/// Asserts `size_of` (account types: + 8 for the discriminator) and each field offset.
macro_rules! layout {
    (acct $t:ty, $size:expr, { $($f:ident @ $o:expr),* $(,)? }) => {
        #[allow(clippy::arithmetic_side_effects)]
        const _: () = {
            assert!(8 + size_of::<$t>() == $size);
            assert!(align_of::<$t>() <= 8);
            $( assert!(8 + offset_of!($t, $f) == $o); )*
        };
    };
    (nested $t:ty, $size:expr, { $($f:ident @ $o:expr),* $(,)? }) => {
        const _: () = {
            assert!(size_of::<$t>() == $size);
            assert!(align_of::<$t>() <= 8);
            $( assert!(offset_of!($t, $f) == $o); )*
        };
    };
}

// ---- PodU128 / PodI128: 16 bytes, alignment 1 ----
const _: () = {
    assert!(size_of::<PodU128>() == 16 && align_of::<PodU128>() == 1);
    assert!(size_of::<PodI128>() == 16 && align_of::<PodI128>() == 1);
};

// ---- GlobalConfig (1,448 B) ----
layout!(acct GlobalConfig, 1_448, {
    version @ 8, bump @ 10, flags @ 11, venue_enabled @ 12, venue_route_broken @ 16,
    timelock_s @ 20, params_version @ 24, _pad0 @ 28,
    governance @ 32, pause_authority @ 64, coverage_authority @ 96, fee_recipient @ 128,
    pyth_sol_usd @ 160, pyth_feed_id @ 192,
    params @ 224, tiers @ 488, ema_params @ 680, kink @ 712, pending @ 720, _reserved @ 1_192,
});
layout!(nested Params, 264, {
    settle_keeper_fixed @ 0, settle_keeper_cap @ 8, expiry_keeper_fixed @ 16,
    expiry_keeper_cap @ 24, min_notional @ 32, market_max_notional @ 40,
    market_creation_fee @ 48, boost_dust_lamports @ 56, exec_rent_tol @ 64,
    theta_a_usd_e6 @ 72, theta_b_usd_e6 @ 80, pool_min_real_sol @ 88,
    open_fee_bps @ 96, rent_protocol_bps @ 100, penalty_lender_bps @ 104,
    penalty_protocol_bps @ 108, penalty_keeper_bps @ 112, expiry_fee_bps @ 116,
    settle_keeper_bps @ 120, expiry_keeper_bps @ 124, util_cap_bps @ 128, wallet_cap_bps @ 132,
    curve_oi_gamma_bps @ 136, fee_size_cap_x100 @ 140, fee_oi_cap_x100 @ 144, s3_wait_s @ 148,
    grad_grace_s @ 152, max_migration_pause_s @ 156, boost_window_s @ 160,
    open_ema_dev_bps @ 164, market_warmup_s @ 168, ema_force_delay_s @ 172,
    settle_persist_n @ 176, settle_arm_gap_s @ 180, fv_dev_bps @ 184, lump_vest_s @ 188,
    lock_grace_s @ 192, oracle_max_age_s @ 196, oracle_stale_s @ 200,
    oracle_max_conf_bps @ 204, oracle_max_step_bps @ 208, venue_fee_cap_bps @ 212,
    venue_fee_hard_cap_bps @ 216, pool_lock_min_bps @ 220, pool_switch_margin_bps @ 224,
    pool_list_window_s @ 228, pool_list_margin_bps @ 232, pool_fast_route_s @ 236,
    pool_fast_min_bps @ 240, pool_fast_window_s @ 244,
    kappa_liq_bps @ 248, kappa_base_bps @ 250, delta_liq_bps @ 252, delta_exp_bps @ 254,
    exec_tol_bps @ 256, ref_max_age_s @ 258, liq_persist_n @ 260, kappa_growth_n @ 262,
});
layout!(nested TierParams, 48, {
    m_bps @ 0, cr_open_bps @ 4, cr_liq_bps @ 8, penalty_bps @ 12, max_duration_s @ 16,
    impact_pos_bps @ 20, impact_mkt_bps @ 24, max_liq_slip_bps @ 28, rate_base_bps @ 32,
    rate_slope1_bps @ 36, rate_slope2_bps @ 40, min_rent_s @ 44,
});
layout!(nested EmaConfig, 32, {
    ema_dtmax_s @ 0, _pad0 @ 2, ema_clamp_bps @ 4, liq_slow_tau_s @ 8, liq_slow_dtmax_s @ 12,
    version @ 16, _pad1 @ 20, force_at @ 24,
});
layout!(nested KinkConfig, 8, { util_kink_bps @ 0, version @ 4 });
layout!(nested PendingChange, 472, { kind @ 0, _pad @ 1, not_before @ 8, payload @ 16 });
#[allow(clippy::arithmetic_side_effects)]
const _: () = assert!(
    PENDING_PAYLOAD_LEN == 456
        && size_of::<Params>() + 4 * size_of::<TierParams>() == PENDING_PAYLOAD_LEN
);

// ---- VenueGuard (224 B; AIR-01: `guard_len` and `guard_elf` from `_reserved`) ----
layout!(acct VenueGuard, 224, {
    version @ 8, bump @ 10, _pad0 @ 11, upgraded @ 12, guard_slot @ 16, flagged_slot @ 64,
    flagged_at @ 112, guard_len @ 160, guard_elf @ 176,
});
// The literal array lengths of `guard_len` / `guard_elf` are the module's constants.
#[allow(clippy::arithmetic_side_effects)]
const _: () = assert!(
    offset_of!(VenueGuard, guard_elf) - offset_of!(VenueGuard, guard_len) == 4 * GUARD_ELF_VENUES
        && size_of::<VenueGuard>() - offset_of!(VenueGuard, guard_elf)
            == GUARD_ELF_LEN * GUARD_ELF_VENUES
        && GUARD_ELF_FIRST as usize + GUARD_ELF_VENUES == crate::constants::VENUE_PROGRAMS
);

// ---- VenueExtras (6,512 B; 4,352 B before M2-SEC-2 added `stable` and `current_since`) ----
layout!(acct VenueExtras, 6_512, {
    version @ 8, bump @ 10, venue @ 11, _pad0 @ 12, sets @ 16, _reserved @ 6_448,
});
layout!(nested SeedPart, 34, { kind @ 0, len @ 1, bytes @ 2 });
layout!(nested ExtraEntry, 176, {
    kind @ 0, writable @ 1, n_seeds @ 2, _pad @ 3, key @ 8, seeds @ 40,
});
layout!(nested ExtrasSet, 712, { version @ 0, count @ 4, _pad @ 5, entries @ 8 });
layout!(nested IxExtras, 2_144, {
    current @ 0, previous @ 712, stable @ 1_424, current_since @ 2_136,
});

// ---- Market (3,104 B; ARCH §2.3.4) ----
layout!(acct Market, 3_104, {
    // header
    version @ 8, flags @ 10, bump @ 12, pool_vault_bump @ 13, market_vault_bump @ 14,
    trader_bump @ 15, trader_wsol_bump @ 16, venue @ 17, tier @ 18, decimals @ 19,
    ring_head @ 20, ring_count @ 21, ema_warm @ 22, ema_guard @ 23, created_at @ 24,
    // identity
    mint @ 32, token_program @ 64, creator @ 96,
    // own accounts
    pool_vault @ 128, market_vault @ 160, trader @ 192, trader_base_ata @ 224, trader_wsol @ 256,
    // pump detection keys
    bonding_curve @ 288, canonical_pool @ 320,
    // curve venue cache
    curve_base_ata @ 352, curve_quote_ata @ 384, sharing_config @ 416, pump_uva @ 448,
    pump_uva_quote_ata @ 480, cached_creator @ 512, cached_creator_vault @ 544,
    cached_creator_vault_ata @ 576,
    // pool venue cache
    pool_cache @ 608,
    // pool selection
    pool_seen_at @ 928, pool_d @ 936, fp_keys @ 952, fp_d @ 1_144, fp_t0 @ 1_160,
    fp_venue @ 1_168, _pad1 @ 1_169, _pad1a @ 1_170, _pad1b @ 1_172,
    // venue timing
    migration_seen_at @ 1_176, graduated_at @ 1_184, tier_computed_at @ 1_192,
    // route chains
    no_route_since @ 1_200, last_no_route_obs @ 1_208, pool_fail_since @ 1_216,
    last_pool_fail_obs @ 1_224,
    // lender pool (market part)
    tokens_free @ 1_232, borrowed_q @ 1_240, borrowed_q_by_tier @ 1_248, total_shares @ 1_280,
    acc @ 1_296, carry @ 1_312, acc_r @ 1_328, carry_r @ 1_344, dust @ 1_360,
    receivable @ 1_376, max_release @ 1_384, vest_bal @ 1_392, vest_last @ 1_400,
    vest_end @ 1_408, share_epoch @ 1_416, _pad2 @ 1_420,
    // rent
    rent_basis @ 1_424, rent_aggr @ 1_472, last_accrual_ts @ 1_520,
    // observation
    b_sec @ 1_528, b_lo @ 1_536, b_hi @ 1_552, b_llo @ 1_568, b_lhi @ 1_584, lb_sec @ 1_600,
    lb_lo @ 1_608, lb_hi @ 1_624, ema_price @ 1_640, ema_liq @ 1_656, liq_slow @ 1_672,
    ema_ts @ 1_688, slow_ts @ 1_696, ema_count @ 1_704, ring @ 1_712,
    // market-level params
    ema_params_version @ 2_800, kink_version @ 2_804, ema_clamp_bps @ 2_808,
    util_kink_bps @ 2_812, liq_slow_tau_s @ 2_816, liq_slow_dtmax_s @ 2_820,
    ema_dtmax_s @ 2_824, _pad3 @ 2_826,
    // OI
    open_positions @ 2_832, _pad4 @ 2_836, total_notional_open @ 2_840, max_q_hwm @ 2_848,
    min_open_fee_ppm @ 2_856, _pad5 @ 2_860,
    // derived keys (M5-SEC-6)
    trader_wsol_ata @ 2_864, pump_pool_authority @ 2_896,
    // misc (M7-SEC-3: `coverage_cleared_at` from `_reserved`; AIR-03: `lender_credit_rem` from
    // `_reserved[0..16]`; AIR-19: `min_open_hard_cap_bps` from `_reserved[16..24]`)
    coverage_cleared_at @ 2_928, lender_credit_rem @ 2_936, min_open_hard_cap_bps @ 2_952,
    _pad6 @ 2_954, _reserved @ 2_960,
});
layout!(nested RingEntry, 136, {
    ts @ 0, p @ 8, l @ 24, x @ 40, y @ 56, lo @ 72, hi @ 88, llo @ 104, lhi @ 120,
});

// ---- MarketVault (2,200 B; ARCH §2.3.6) ----
layout!(acct MarketVault, 2_200, {
    lender_sol @ 8, unmatched @ 16, protocol_fees @ 24, pi @ 32, lock_norm @ 48, b_min @ 64,
    pi_epoch @ 72, bump @ 76, _pad0 @ 77, cal_day @ 80, lock_cal @ 88, _reserved @ 2_136,
});

// ---- Borsh accounts (ARCH §2.3.5, §2.3.6) ----
const _: () = {
    assert!(ShortPosition::SPACE == 384);
    assert!(LenderPosition::SPACE == 241);
    assert!(ShareEpoch::SPACE == 143);
    // KI-65: `conv_rem` is the record's last field, rewritten in place by the lender
    // instructions (`engine::lender::store_records`).
    assert!(ShareEpoch::CONV_REM_OFFSET == 127);
    assert!(ShareEpoch::CONV_REM_OFFSET + 16 == ShareEpoch::SPACE);
    assert!(WalletExposure::SPACE == 81);
    assert!(OracleCache::SPACE == 45);
    assert!(GlobalConfig::SPACE == 1_448);
    assert!(VenueGuard::SPACE == 224);
    assert!(VenueExtras::SPACE == 6_512);
    assert!(Market::SPACE == 3_104);
    assert!(MarketVault::SPACE == 2_200);
};

#[cfg(test)]
mod tests {
    /// The zero-copy files: none may declare a raw `u128` / `i128` field (ARCH §2.3, A67).
    const ZERO_COPY_SOURCES: [(&str, &str); 5] = [
        ("config.rs", include_str!("config.rs")),
        ("venue_guard.rs", include_str!("venue_guard.rs")),
        ("venue_extras.rs", include_str!("venue_extras.rs")),
        ("market.rs", include_str!("market.rs")),
        ("vault.rs", include_str!("vault.rs")),
    ];

    /// `pub name: <type>` whose type mentions `u128` / `i128` (a field declaration; `pub fn`
    /// signatures and `let` bindings do not match).
    fn is_raw_128_field(line: &str) -> bool {
        let t = line.trim_start();
        let Some(rest) = t.strip_prefix("pub ") else {
            return false;
        };
        let Some((name, ty)) = rest.split_once(':') else {
            return false;
        };
        let name = name.trim();
        !name.is_empty()
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
            && (ty.contains("u128") || ty.contains("i128"))
    }

    #[test]
    fn raw_128_bit_fields() {
        assert!(is_raw_128_field("    pub total_shares: u128,"));
        assert!(is_raw_128_field("    pub j: [i128; 3],"));
        assert!(!is_raw_128_field("    pub total_shares: PodU128,"));
        assert!(!is_raw_128_field("    pub fn get(&self) -> u128 {"));
        for (file, src) in ZERO_COPY_SOURCES {
            for (n, line) in src.lines().enumerate() {
                assert!(
                    !is_raw_128_field(line),
                    "{file}:{}: raw 128-bit field in a zero-copy account: {line}",
                    n + 1
                );
            }
        }
    }
}
