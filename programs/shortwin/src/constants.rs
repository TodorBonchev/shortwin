//! Compile-time constants (ARCH §2.1 `constants.rs`, §2.3.2, §2.3.3 scope C; C0-8).
//!
//! Venue program ids, their static PDAs and authorities, the venue ProgramData addresses and
//! the Pyth receiver are constants: **no config path can change them; only an audited program
//! upgrade can** (C0-8). `tests::static_addresses_rederive` re-derives every PDA below.
//!
//! The protocol constants (`EMA_TAU_S`, `RING_LEN`, `S3_OBS_GAP_S`, `COVERAGE_GAP_S`,
//! `P_ACC_MAX`, `P_OPEN_MAX`, the lock calendar, PumpSwap's sizing `λ_min`
//! `PUMPSWAP_LOCK_MIN_BPS`) and the compiled parameter bounds live in
//! `shortwin-math` (one table, shared with the keeper and the frontend) and are re-exported
//! here; the program never restates a bound.

use anchor_lang::prelude::*;

pub use shortwin_math::consts::{
    COVERAGE_GAP_S, EMA_TAU_S, LOCK_BUCKETS, LOCK_BUCKET_S, MAX_RELEASE_SPAN_S,
    PUMPSWAP_LOCK_MIN_BPS, P_ACC_MAX, P_OPEN_MAX, RING_LEN, S3_OBS_GAP_S,
};
/// The compiled hard bounds of every parameter (ARCH §2.3.3; ECON §15): `Rule::bounds()`.
/// `params::validate` / `params::validate_econ` enforce them with the relational checks.
pub use shortwin_math::params::Rule as ParamRule;
pub use shortwin_math::pool::{REBASE, R_MAX, VIRTUAL_ASSETS, VIRTUAL_SHARES};
pub use shortwin_math::tier::{AGE_A_S, AGE_C_S};

// ---- layout versions ---------------------------------------------------------------------

/// `GlobalConfig.version` (ARCH §2.3.2).
pub const CONFIG_VERSION: u16 = 1;
/// `VenueGuard.version`.
pub const VENUE_GUARD_VERSION: u16 = 1;
/// `VenueExtras.version`.
pub const VENUE_EXTRAS_VERSION: u16 = 2;
/// `Market.version` (ARCH §2.3.4).
pub const MARKET_VERSION: u16 = 1;

/// M7-SEC-3: seconds after `set_coverage_pause(false)` clears a set `paused_coverage` flag during
/// which `open_short` still refuses (`CoveragePaused`; `Market.coverage_cleared_at`). The
/// `coverage_authority` key then cannot bracket an open on a coverage-paused market,
/// `[set_coverage_pause(false), open_short, set_coverage_pause(true)]`, in one transaction or one
/// second: the flag stays cleared for at least this long before any open lands, which the
/// keeper's per-second reads see (and alert) and its next tick sets again — longer than the
/// keeper's read-to-landing loop (a `confirmed` read, a tick that waits for its confirmations,
/// the send). Its only cost to an honest unpause: opens resume this much later. It applies after
/// a clear by either signer — the coverage key or the Emergency vault (OWNER-M7-3, owner decision
/// of 2026-10-01). **PROPOSED**.
pub const COVERAGE_CLEAR_DELAY_S: i64 = 5;

// ---- governance --------------------------------------------------------------------------

/// Compiled floor of `GlobalConfig.timelock_s` (A8): 24 h.
pub const MIN_TIMELOCK_S: u32 = 86_400;
/// Default `timelock_s` (48 h, SPEC §11).
pub const DEFAULT_TIMELOCK_S: u32 = 172_800;
/// Compiled ceiling of `GlobalConfig.timelock_s`: 30 days. A unit slip (48 h in milliseconds)
/// or a hostile `Timelock` change could otherwise freeze every later change, key rotations
/// included, for years (M1-SEC-4; ARCH §2.3.2, §2.3.3).
pub const MAX_TIMELOCK_S: u32 = 2_592_000;
/// How long after `not_before` a pending change may still be applied: 14 days. Later,
/// `apply_config` refuses (`PendingChangeExpired`) and governance proposes it again, so a
/// forgotten proposal cannot be applied long after users stopped watching it (M1-SEC-9;
/// ARCH §2.3.2, §3.1).
pub const CONFIG_APPLY_WINDOW_S: i64 = 1_209_600;
/// How long an extras set must stand as `current` before the write that displaces it also
/// makes it the `stable` escape set of its instruction kind: 7 days (M2-SEC-2; ARCH §2.3.2
/// `VenueExtras`). Longer than Governance needs to re-publish a set (its 48 h Squads time
/// lock) or to rotate the Emergency key through a timelocked `Authorities` change (≥ 24 h,
/// 48 h by default, plus the Squads lock), so a compromised or twice-mistaken Emergency key
/// cannot push a working set out of every selectable slot before Governance can act.
/// **PROPOSED** (owner decision with O6): a compile-time constant, like the other timing bounds
/// of the Emergency powers.
pub const EXTRAS_STABLE_S: i64 = 604_800;

/// CPMM `open_time` clamp cut-off, 2024-06-01T00:00Z (ARCH §2.3.3, §4.7; R-RAY §7).
pub const T_CLAMP: i64 = 1_717_200_000;

// ---- `GlobalConfig.venue_enabled` / `venue_route_broken` bits (ARCH §2.3.2) --------------

/// Pump.fun bonding curve.
pub const VENUE_BIT_PUMP_CURVE: u32 = 1 << 0;
/// PumpSwap.
pub const VENUE_BIT_PUMP_SWAP: u32 = 1 << 1;
/// Raydium AMM v4.
pub const VENUE_BIT_RAYDIUM_V4: u32 = 1 << 2;
/// Raydium CPMM.
pub const VENUE_BIT_RAYDIUM_CPMM: u32 = 1 << 3;
/// Jupiter (v1.1).
pub const VENUE_BIT_JUPITER: u32 = 1 << 4;
/// TEST-ONLY: the mock venue (`VenueKind::Mock`, feature `test-venue`; A49).
#[cfg(feature = "test-venue")]
pub const VENUE_BIT_MOCK: u32 = 1 << 5;
/// Every bit `venue_enabled` may hold (the release set; a `test-venue` build adds
/// `VENUE_BIT_MOCK`).
#[cfg(not(feature = "test-venue"))]
pub const VENUE_BITS_KNOWN: u32 = VENUE_BITS_RELEASE;
/// Every bit `venue_enabled` may hold (the release set plus `VENUE_BIT_MOCK`).
#[cfg(feature = "test-venue")]
pub const VENUE_BITS_KNOWN: u32 = VENUE_BITS_RELEASE | VENUE_BIT_MOCK;
/// The venue bits of the release build.
pub const VENUE_BITS_RELEASE: u32 = VENUE_BIT_PUMP_CURVE
    | VENUE_BIT_PUMP_SWAP
    | VENUE_BIT_RAYDIUM_V4
    | VENUE_BIT_RAYDIUM_CPMM
    | VENUE_BIT_JUPITER;

/// Venue bits this build cannot let Governance enable (M5-SEC-4, a hard gate): a venue whose
/// markets this build can open positions on, but whose lifecycle it cannot finish.
/// `initialize_config` and `propose_config(EnableVenues)` refuse these bits (`InvalidParams`), so
/// `venue_enabled` cannot hold them and every open on such a venue fails `VenueDisabled`. The M5
/// build held the pump curve here: without the `→ PumpSwap` transition a curve that completed
/// and migrated (anyone can do both in one transaction, `migrate_v2` being permissionless) left
/// its market `PumpMigrating` for good — exits refused, the EMA frozen at the last curve price.
/// **M6 empties the set**: the transition (the ring rewrite to the new pool and the post-change
/// confirmation warm-up, `venues::pump_amm::graduate`) observes the pool and resumes exits on it.
/// The mechanism stays for a later unfinished venue.
pub const VENUE_BITS_UNFINISHED: u32 = 0;

/// Every bit Governance may enable in this build: the known bits less the unfinished ones.
pub const VENUE_BITS_ENABLEABLE: u32 = VENUE_BITS_KNOWN & !VENUE_BITS_UNFINISHED;

// ---- venue programs (VenueGuard indices, ARCH §2.3.2) ------------------------------------

/// Venue programs the upgrade latch watches, in `VenueGuard` bit / slot order.
pub const VENUE_PROGRAMS: usize = 6;
/// `VenueGuard` index: pump (bonding curve).
pub const GUARD_PUMP: u8 = 0;
/// `VenueGuard` index: pump AMM (PumpSwap).
pub const GUARD_PUMP_AMM: u8 = 1;
/// `VenueGuard` index: pump fees.
pub const GUARD_PUMP_FEES: u8 = 2;
/// `VenueGuard` index: Raydium AMM v4.
pub const GUARD_RAYDIUM_V4: u8 = 3;
/// `VenueGuard` index: Raydium CPMM.
pub const GUARD_RAYDIUM_CPMM: u8 = 4;
/// `VenueGuard` index: Jupiter (v1.1).
pub const GUARD_JUPITER: u8 = 5;

/// `VenueGuard.upgraded` mask that blocks opens on a `PumpCurve` market: pump + pfee +
/// **pAMM** (a curve position exits on PumpSwap after graduation, and `B̂_str` prices the pAMM
/// pool; ARCH §3.1, R4B-7).
pub const GUARD_MASK_PUMP_CURVE: u32 =
    (1 << GUARD_PUMP) | (1 << GUARD_PUMP_FEES) | (1 << GUARD_PUMP_AMM);
/// Mask for a `PumpSwap` market: pAMM + pfee.
pub const GUARD_MASK_PUMP_SWAP: u32 = (1 << GUARD_PUMP_AMM) | (1 << GUARD_PUMP_FEES);
/// Mask for a `RaydiumV4` or `RaydiumCpmm` market: **both** Raydium programs (a fast path can
/// move the market between them with positions open).
pub const GUARD_MASK_RAYDIUM: u32 = (1 << GUARD_RAYDIUM_V4) | (1 << GUARD_RAYDIUM_CPMM);

/// Pump.fun bonding-curve program.
pub const PUMP_PROGRAM_ID: Pubkey = pubkey!("6EF8rrecthR5Dkzon8Nwu78hRvfCKubJ14M5uBEwF6P");
/// PumpSwap (pump AMM) program.
pub const PUMP_AMM_PROGRAM_ID: Pubkey = pubkey!("pAMMBay6oceH9fJKBRHGP5D4bD4sWpmSwMn52FMfXEA");
/// Pump fees program.
pub const PUMP_FEES_PROGRAM_ID: Pubkey = pubkey!("pfeeUxB6jkeY1Hxd7CsFCAjcbHA9rWtchMGdZ6VojVZ");
/// Raydium AMM v4 program.
pub const RAYDIUM_V4_PROGRAM_ID: Pubkey = pubkey!("675kPX9MHTjS2zt1qfr1NYHuzeLXfQM9H24wFSUt1Mp8");
/// Raydium CPMM program.
pub const RAYDIUM_CPMM_PROGRAM_ID: Pubkey = pubkey!("CPMMoo8L3F4NbTegBCKVNunggL7H1ZpdTHKxQB5qKP1C");
/// Jupiter v6 aggregator (v1.1).
pub const JUPITER_PROGRAM_ID: Pubkey = pubkey!("JUP6LkbZbjS1jKKwapdHNy74zcZ3tLUZoi5QNyVTaV4");

/// Venue program ids in `VenueGuard` order.
pub const VENUE_PROGRAM_IDS: [Pubkey; VENUE_PROGRAMS] = [
    PUMP_PROGRAM_ID,
    PUMP_AMM_PROGRAM_ID,
    PUMP_FEES_PROGRAM_ID,
    RAYDIUM_V4_PROGRAM_ID,
    RAYDIUM_CPMM_PROGRAM_ID,
    JUPITER_PROGRAM_ID,
];

/// ProgramData accounts of the venue programs, in `VenueGuard` order
/// (`PDA(BPFLoaderUpgradeable, [program_id])`; R-PUMP §1, R-VEN §7, R-RAY §1.1, R-JUP §1).
pub const VENUE_PROGRAM_DATA: [Pubkey; VENUE_PROGRAMS] = [
    pubkey!("B5MvUwXdiW1NMM6QFFD3ssPKBujD4zMohncbM73Z2BQu"),
    pubkey!("6naEzKeUuFh1Jeeu51NXQgr5qkXgXtc9WKNct4xynVJc"),
    pubkey!("75Uu23mqWBb8LM8vDppqC1mQAnCcBuLXhVaDezVMQLRw"),
    pubkey!("A7ZG7ByDi8DpzT9Ab7CiXhvgYTJQmaDPJkMDoPitaCQV"),
    pubkey!("DMawCQzbgNTmbzaESc7o6pvL1KAeetY8zA7jNpzntHhU"),
    pubkey!("4Ec7ZxZS6Sbdg5UGSLHbAnM7GQHp2eFd4KYWRexAipQT"),
];

/// Deploy slots validated when v0.4.1 was written (ARCH §2.3.2 [V9]); `initialize_config`
/// takes the live values as arguments.
pub const VENUE_GUARD_SLOTS_2026_09_27: [u64; VENUE_PROGRAMS] = [
    449_734_335,
    446_462_733,
    446_465_969,
    445_763_204,
    445_763_504,
    449_280_566,
];

/// Deploy slots read on mainnet on 2026-10-02 (M10; the plan's synthesizer at slot 452,483,329
/// and the review at 452,485,424): Raydium CPMM was redeployed at 452,376,460 and Jupiter at
/// 451,957,263, the others are unchanged from [`VENUE_GUARD_SLOTS_2026_09_27`]. A test and
/// `scripts/dump-fixtures.py check --live` baseline only (the newest such constant); the program
/// never reads it (`initialize_config` takes the live values as arguments).
pub const VENUE_GUARD_SLOTS_2026_10_02: [u64; VENUE_PROGRAMS] = [
    449_734_335,
    446_462_733,
    446_465_969,
    445_763_204,
    452_376_460,
    451_957_263,
];

/// Deploy slots read on mainnet on 2026-10-03 (M10 pump revalidation, `scripts/dump-fixtures.py
/// check --live` at slot 452,963,256): pump, PumpSwap and pump-fees were redeployed on
/// 2026-10-02 at 452,654,932 / 452,654,882 / 452,655,002, the others are unchanged from
/// [`VENUE_GUARD_SLOTS_2026_10_02`]. A test and `check --live` baseline only (the newest such
/// constant); the program never reads it.
pub const VENUE_GUARD_SLOTS_2026_10_03: [u64; VENUE_PROGRAMS] = [
    452_654_932,
    452_654_882,
    452_655_002,
    445_763_204,
    452_376_460,
    451_957_263,
];

/// Deploy slots read on mainnet on 2026-10-13 (`check --live`): pump 456,494,698 (2026-10-13
/// 15:40 UTC; its 2026-10-08 build at 454,596,459 preceded it), PumpSwap 454,596,406
/// (2026-10-08), pump-fees 456,132,171 (2026-10-12; an unexamined 10-08 build at 454,596,501
/// preceded it), Raydium AMM v4 and CPMM unchanged, Jupiter 454,465,850. A test and
/// `check --live` baseline only (the program never reads it).
pub const VENUE_GUARD_SLOTS_2026_10_13: [u64; VENUE_PROGRAMS] = [
    456_494_698,
    454_596_406,
    456_132_171,
    445_763_204,
    452_376_460,
    454_465_850,
];

/// The ProgramData address of venue program `venue` (a `VenueGuard` index), or `None`.
pub const fn venue_program_data(venue: u8) -> Option<Pubkey> {
    let i = venue as usize;
    if i < VENUE_PROGRAMS {
        Some(VENUE_PROGRAM_DATA[i])
    } else {
        None
    }
}

// ---- static venue PDAs and authorities (ARCH §2.3.2; R-PUMP §3.1, R-VEN §1.1, R-RAY §1.1) --

/// pump `Global` = PDA(pump, ["global"]).
pub const PUMP_GLOBAL: Pubkey = pubkey!("4wTV1YmiEkRvAtNtsSGPtUrqRYQMe5SKy2uB4Jjaxnjf");
/// pump `event_authority` = PDA(pump, ["__event_authority"]).
pub const PUMP_EVENT_AUTHORITY: Pubkey = pubkey!("Ce6TQqeHC9p8KetsN6JsjHK7UTZk7nasjjnr7XxXp9F1");
/// pump global volume accumulator = PDA(pump, ["global_volume_accumulator"]).
pub const PUMP_GLOBAL_VOLUME_ACCUMULATOR: Pubkey =
    pubkey!("Hq2wp8uJ9jCPsYgNHex8RtqdvMPfVGoYwjvF1ATiwn2Y");
/// pump-fees `FeeConfig` of pump = PDA(pfee, ["fee_config", pump]).
pub const PUMP_FEE_CONFIG: Pubkey = pubkey!("8Wf5TiAheLUqBrKXeYg2JtAFFMWtKdG2BSFgqUcPVwTt");
/// PumpSwap `GlobalConfig` = PDA(pAMM, ["global_config"]).
pub const PUMP_AMM_GLOBAL_CONFIG: Pubkey = pubkey!("ADyA8hdefvWN2dbGGWFotbzWxrAvLW83WG6QCVXvJKqw");
/// PumpSwap `event_authority` = PDA(pAMM, ["__event_authority"]).
pub const PUMP_AMM_EVENT_AUTHORITY: Pubkey =
    pubkey!("GS4CU59F31iL7aR2Q8zVS8DRrcRnXX1yjQ66TqNVQnaR");
/// PumpSwap global volume accumulator = PDA(pAMM, ["global_volume_accumulator"]).
pub const PUMP_AMM_GLOBAL_VOLUME_ACCUMULATOR: Pubkey =
    pubkey!("C2aFPdENg4A2HQsmrd5rTw5TaYBX5Ku887cWjbFKtZpw");
/// pump-fees `FeeConfig` of PumpSwap = PDA(pfee, ["fee_config", pAMM]).
pub const PUMP_AMM_FEE_CONFIG: Pubkey = pubkey!("5PHirr8joyTMp9JMm6nW7hNDVyEYdkzDqazxPD7RaTjx");
/// Raydium AMM v4 vault / LP authority = `create_program_address(["amm authority", 254])`.
pub const RAYDIUM_V4_AUTHORITY: Pubkey = pubkey!("5Q544fKrFoe6tsEbD7S8EmxGTJYAKtTVhAW5Q5pge4j1");
/// The only AMM v4 authority nonce a market accepts (R-RAY §1.1).
pub const RAYDIUM_V4_AUTHORITY_NONCE: u8 = 254;
/// Raydium CPMM vault / LP authority = PDA(CPMM, ["vault_and_lp_mint_auth_seed"]).
pub const RAYDIUM_CPMM_AUTHORITY: Pubkey = pubkey!("GpMZbSM2GgvTKHJirzeGfMFoaZ8UR2X7F4v8vHTvxFbL");

// ---- tokens --------------------------------------------------------------------------------

/// SPL Token (p-token on mainnet).
pub const TOKEN_PROGRAM_ID: Pubkey = pubkey!("TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA");
/// Token-2022.
pub const TOKEN_2022_PROGRAM_ID: Pubkey = pubkey!("TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb");
/// Associated Token Account program.
pub const ASSOCIATED_TOKEN_PROGRAM_ID: Pubkey =
    pubkey!("ATokenGPvbdGVxr1b2hvZbsiqW5xWH25efTNsLJA8knL");
/// Wrapped SOL (SPL Token native mint); the only SOL side a venue may use (ARCH §2.6).
pub const WSOL_MINT: Pubkey = pubkey!("So11111111111111111111111111111111111111112");
/// Token-2022 native mint (never listable, never a SOL side; R-MINT §9.2).
pub const WSOL_2022_MINT: Pubkey = pubkey!("9pan9bMn5HatX4EJdBwg9VgCa7Uz5HL8N1m5D3NdXejP");
/// spl-memo v3/v4 (`MemoSq4g…`): `withdraw` CPIs it before a transfer into a Token-2022
/// account that requires incoming-transfer memos (ARCH §3.3; R-MINT §8.4; T-MEMO, U17).
pub const MEMO_PROGRAM_ID: Pubkey = pubkey!("MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr");

// ---- TEST-ONLY mock venue (feature `test-venue`; A49; tests/mock_cpamm/INTERFACE.md) -------

/// The mock constant-product venue `tests/mock_cpamm` (never deployed).
#[cfg(feature = "test-venue")]
pub const MOCK_CPAMM_PROGRAM_ID: Pubkey = pubkey!("BPAZFPkmVtYEWEZHCzFUns7oybh1VWfAvmu3ofM2mQi7");
/// The mock's global vault authority `PDA(mock, ["authority"])`.
#[cfg(feature = "test-venue")]
pub const MOCK_CPAMM_AUTHORITY: Pubkey = pubkey!("FchvTASEWCcGETHi6QMLW7v64Jzn66W8Z3yqZjY9iqLA");
/// Marker string a `test-venue` build logs when it adopts a mock pool: CI's A49 check requires
/// it (and the mock id) **absent** from the release `shortwin.so` and present in the
/// test-venue build.
#[cfg(feature = "test-venue")]
pub const TEST_VENUE_MARKER: &str = "shortwin:test-venue:mock-cpamm";

// ---- oracle (ARCH §4.7, A10, A29; R-VEN §5) ------------------------------------------------

/// Pyth Solana receiver: owner of every `PriceUpdateV2`.
pub const PYTH_RECEIVER_ID: Pubkey = pubkey!("rec5EKMGg6MxZYaMdyBfgwp4d5rB9T1VQH5pJv5LtFJ");
/// Sponsored shard-0 SOL/USD `PriceUpdateV2` (the `initialize_config` default).
pub const PYTH_SOL_USD_ACCOUNT: Pubkey = pubkey!("7UVimffxr9ow1uXYxsr4LHAcV58mLzhmwaeKvJ1pjLiE");
/// SOL/USD feed id `ef0d8b6f…b56d`.
pub const PYTH_SOL_USD_FEED_ID: [u8; 32] = [
    0xef, 0x0d, 0x8b, 0x6f, 0xda, 0x2c, 0xeb, 0xa4, 0x1d, 0xa1, 0x5d, 0x40, 0x95, 0xd1, 0xda, 0x39,
    0x2a, 0x0d, 0x2f, 0x8e, 0xd0, 0xc6, 0xc7, 0xbc, 0x0f, 0x4c, 0xfa, 0xc8, 0xc2, 0x80, 0xb5, 0x6d,
];

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_lang::solana_program::bpf_loader_upgradeable;

    fn pda(program: &Pubkey, seeds: &[&[u8]]) -> Pubkey {
        Pubkey::find_program_address(seeds, program).0
    }

    /// ARCH §2.3.2: every static address re-derived from its program and seeds (C0-8).
    #[test]
    fn static_addresses_rederive() {
        assert_eq!(pda(&PUMP_PROGRAM_ID, &[b"global"]), PUMP_GLOBAL);
        assert_eq!(
            pda(&PUMP_PROGRAM_ID, &[b"__event_authority"]),
            PUMP_EVENT_AUTHORITY
        );
        assert_eq!(
            pda(&PUMP_PROGRAM_ID, &[b"global_volume_accumulator"]),
            PUMP_GLOBAL_VOLUME_ACCUMULATOR
        );
        assert_eq!(
            pda(
                &PUMP_FEES_PROGRAM_ID,
                &[b"fee_config", PUMP_PROGRAM_ID.as_ref()]
            ),
            PUMP_FEE_CONFIG
        );
        assert_eq!(
            pda(&PUMP_AMM_PROGRAM_ID, &[b"global_config"]),
            PUMP_AMM_GLOBAL_CONFIG
        );
        assert_eq!(
            pda(&PUMP_AMM_PROGRAM_ID, &[b"__event_authority"]),
            PUMP_AMM_EVENT_AUTHORITY
        );
        assert_eq!(
            pda(&PUMP_AMM_PROGRAM_ID, &[b"global_volume_accumulator"]),
            PUMP_AMM_GLOBAL_VOLUME_ACCUMULATOR
        );
        assert_eq!(
            pda(
                &PUMP_FEES_PROGRAM_ID,
                &[b"fee_config", PUMP_AMM_PROGRAM_ID.as_ref()]
            ),
            PUMP_AMM_FEE_CONFIG
        );
        assert_eq!(
            Pubkey::create_program_address(
                &[b"amm authority", &[RAYDIUM_V4_AUTHORITY_NONCE]],
                &RAYDIUM_V4_PROGRAM_ID
            )
            .ok(),
            Some(RAYDIUM_V4_AUTHORITY)
        );
        assert_eq!(
            pda(&RAYDIUM_CPMM_PROGRAM_ID, &[b"vault_and_lp_mint_auth_seed"]),
            RAYDIUM_CPMM_AUTHORITY
        );
        for (id, data) in VENUE_PROGRAM_IDS.iter().zip(VENUE_PROGRAM_DATA.iter()) {
            assert_eq!(pda(&bpf_loader_upgradeable::ID, &[id.as_ref()]), *data);
        }
    }

    #[cfg(feature = "test-venue")]
    #[test]
    fn mock_venue_constants_rederive() {
        assert_eq!(
            MOCK_CPAMM_PROGRAM_ID.to_bytes(),
            shortwin_venue_state::mock_cpamm::PROGRAM
        );
        assert_eq!(
            pda(&MOCK_CPAMM_PROGRAM_ID, &[b"authority"]),
            MOCK_CPAMM_AUTHORITY
        );
    }

    #[test]
    fn venue_state_constants_agree() {
        assert_eq!(
            shortwin_venue_state::program_data::LOADER_UPGRADEABLE,
            bpf_loader_upgradeable::ID.to_bytes()
        );
        assert_eq!(
            shortwin_venue_state::pyth::RECEIVER,
            PYTH_RECEIVER_ID.to_bytes()
        );
        // The venue readers' program ids and authorities (M3: Raydium, pump detection).
        use shortwin_venue_state::{mint, pump, pump_amm, raydium_cpmm, raydium_v4};
        assert_eq!(raydium_v4::PROGRAM, RAYDIUM_V4_PROGRAM_ID.to_bytes());
        assert_eq!(raydium_v4::AUTHORITY, RAYDIUM_V4_AUTHORITY.to_bytes());
        assert_eq!(
            raydium_v4::AUTHORITY_NONCE,
            u64::from(RAYDIUM_V4_AUTHORITY_NONCE)
        );
        assert_eq!(raydium_cpmm::PROGRAM, RAYDIUM_CPMM_PROGRAM_ID.to_bytes());
        assert_eq!(raydium_cpmm::AUTHORITY, RAYDIUM_CPMM_AUTHORITY.to_bytes());
        assert_eq!(raydium_cpmm::T_CLAMP, T_CLAMP);
        assert_eq!(pump::PROGRAM, PUMP_PROGRAM_ID.to_bytes());
        assert_eq!(pump_amm::PROGRAM, PUMP_AMM_PROGRAM_ID.to_bytes());
        assert_eq!(mint::NATIVE_MINT, WSOL_MINT.to_bytes());
        assert_eq!(mint::TOKEN_PROGRAM, TOKEN_PROGRAM_ID.to_bytes());
        assert_eq!(mint::TOKEN_2022_PROGRAM, TOKEN_2022_PROGRAM_ID.to_bytes());
    }

    #[test]
    fn guard_indices_and_masks() {
        assert_eq!(
            venue_program_data(GUARD_JUPITER),
            Some(VENUE_PROGRAM_DATA[5])
        );
        assert_eq!(venue_program_data(6), None);
        assert_eq!(VENUE_PROGRAMS, 6);
        assert_eq!(GUARD_MASK_PUMP_CURVE, 0b00111);
        assert_eq!(GUARD_MASK_PUMP_SWAP, 0b00110);
        assert_eq!(GUARD_MASK_RAYDIUM, 0b11000);
        assert_eq!(VENUE_BITS_RELEASE, 0b11111);
        #[cfg(not(feature = "test-venue"))]
        assert_eq!(VENUE_BITS_KNOWN, 0b11111);
        #[cfg(feature = "test-venue")]
        assert_eq!(VENUE_BITS_KNOWN, 0b111111);
        // M5-SEC-4's gate, emptied by M6 (the `→ PumpSwap` transition): every known bit is
        // enableable, the pump curve included.
        assert_eq!(VENUE_BITS_UNFINISHED, 0);
        assert_eq!(VENUE_BITS_ENABLEABLE, VENUE_BITS_KNOWN);
        assert_ne!(VENUE_BITS_ENABLEABLE & VENUE_BIT_PUMP_CURVE, 0);
        assert_eq!(T_CLAMP, 1_717_200_000);
        const { assert!(MIN_TIMELOCK_S <= DEFAULT_TIMELOCK_S) };
        const { assert!(DEFAULT_TIMELOCK_S <= MAX_TIMELOCK_S) };
        const { assert!(CONFIG_APPLY_WINDOW_S > 0) };
    }
}
