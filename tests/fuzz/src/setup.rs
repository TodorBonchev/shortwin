//! Building a [`World`] for a profile from a seed. The setup draws its own choices (token program,
//! fee, fixture, oracle) from `Rng::new(seed ^ SETUP_SALT)`, so the action stream (from `seed`)
//! and the setup are independent: every subsequence of a run's actions replays on the same setup.

use anchor_lang::prelude::Pubkey;
use shortwin::constants::MIN_TIMELOCK_S;
use shortwin::state::InitConfigArgs;
use shortwin_litesvm_tests::Env;
use shortwin_litesvm_tests::engine::MarketSpec;
use shortwin_litesvm_tests::tokens::TokenProgram;
use solana_keypair::Keypair;
use solana_signer::Signer;

use crate::action::Profile;
use crate::liq;
use crate::rng::Rng;
use crate::stats::Stats;
use crate::venue::Venue;
use crate::venue::mock::MockVenue;
use crate::world::{Lender, Slot, World};

/// The setup generator's salt.
pub const SETUP_SALT: u64 = 0x5e75_eed0_000f_f1ce;

/// The salt of the M7 setup choices (drawn apart, so the earlier choices of a seed and the
/// corpus reproducers stay as they were).
pub const M7_SALT: u64 = 0x0c0f_e7a9_e000_0017;

/// The salt of the KI-66 wind-down choice ([`World::absent_owners`]), drawn apart as
/// [`M7_SALT`]'s.
pub const KI66_SALT: u64 = 0x0a6b_5e47_0000_0066;

/// `GlobalConfig.coverage_authority` in the account data (discriminator included; the
/// `state::config` layout, as `coverage_pause_it` writes it).
pub const CONFIG_COVERAGE_AUTHORITY_OFFSET: usize = 96;

/// **TEST-ONLY**: writes `key` into `GlobalConfig.coverage_authority` — the state an applied
/// `Authorities` change leaves (O17's role), without the timelock's warp at setup. Rotations
/// during a run take the real, timelocked path ([`crate::world::World`]'s `coverage_pause`).
pub fn plant_coverage_authority(env: &mut Env, key: Pubkey) {
    let cfg = shortwin_litesvm_tests::pda::config();
    let mut acc = env.svm.get_account(&cfg).expect("config");
    let at = CONFIG_COVERAGE_AUTHORITY_OFFSET;
    acc.data[at..at + 32].copy_from_slice(key.as_ref());
    env.svm.set_account(cfg, acc).expect("set config");
    assert_eq!(
        env.config().coverage_authority,
        key,
        "the planted coverage_authority"
    );
}

/// Borrowers per market.
pub const BORROWERS: usize = 4;
/// Lenders per market besides the seeding one.
pub const EXTRA_LENDERS: usize = 2;

/// The config choices of a run: the ECON §15 defaults but the compiled minimum timelock (24 h:
/// governance actions warp less), and — so that every cap binds somewhere — the per-market
/// notional limit at the 25 SOL launch guard (half the runs), 1,000 SOL or its 10⁵ SOL bound, and
/// the wallet cap at its default or at 10⁴ bps.
pub fn tweak_args(rng: &mut Rng) -> impl FnOnce(&mut InitConfigArgs) {
    let notional = [
        25_000_000_000u64,
        25_000_000_000,
        1_000_000_000_000,
        100_000_000_000_000,
    ][rng.below(4) as usize];
    let wallet = if rng.chance(1, 3) {
        Some(10_000u32)
    } else {
        None
    };
    move |a: &mut InitConfigArgs| {
        a.timelock_s = MIN_TIMELOCK_S;
        a.params.market_max_notional = notional;
        if let Some(w) = wallet {
            a.params.wallet_cap_bps = w;
        }
    }
}

/// The release build initialized with [`tweak_args`] (the fork profiles load their venues next).
pub fn release_env(rng: &mut Rng) -> Env {
    let mut env = Env::new();
    let mut args = env.default_init_args();
    tweak_args(rng)(&mut args);
    shortwin_litesvm_tests::errors::ok(env.initialize_config(args));
    env
}

/// The test-venue environment of the mock profiles ([`tweak_args`]), with a fresh oracle unless
/// the setup draws a missing one (every market Tier C).
pub fn mock_env(rng: &mut Rng) -> Env {
    let mut env = liq::env_with(tweak_args(rng));
    if rng.chance(3, 4) {
        liq::fresh_oracle(&mut env);
    }
    env
}

/// A random mock market spec.
pub fn mock_spec(rng: &mut Rng, program: TokenProgram) -> MarketSpec {
    let fee = [2_500, 2_500, 10_000, 500, 5_000][rng.below(5) as usize];
    let base_exp = rng.below(3);
    let mut spec = MarketSpec {
        program,
        fee_ppm: fee,
        ..MarketSpec::default()
    };
    // Pools of 1,000 SOL against 10¹⁵ raw tokens by default; thinner and deeper variants.
    match base_exp {
        0 => {}
        1 => {
            spec.base /= 10;
            spec.quote /= 10;
            spec.lend /= 10;
        }
        _ => {
            spec.base *= 4;
            spec.quote *= 4;
            spec.lend *= 4;
        }
    }
    if rng.chance(1, 4) {
        spec.lend *= 5;
    }
    spec
}

/// A market slot around `venue`: borrowers, the seeding lender and two more (funded with
/// `lend_each` tokens each).
pub fn slot(env: &mut Env, mut venue: Box<dyn Venue>, lend_each: u64) -> Slot {
    // 10⁶ SOL each: margins at the per-market notional bound (10⁵ SOL) never run a borrower dry.
    let borrowers = (0..BORROWERS)
        .map(|_| {
            let b = env.borrower();
            refill_faucet(env);
            env.fund(&b.key(), 1_000_000_000_000_000);
            b
        })
        .collect();
    let (kp, account) = venue.seed_lender();
    let mut lenders = vec![Lender { kp, account }];
    for _ in 0..EXTRA_LENDERS {
        let kp = env.funded_keypair();
        let account = venue.fund_tokens(env, &kp.pubkey(), lend_each);
        lenders.push(Lender { kp, account });
    }
    let spot0 = venue
        .reserves(env)
        .map(|(x, y)| u128::from(y) * shortwin_math::num::WAD / u128::from(x.max(1)));
    Slot {
        venue,
        borrowers,
        lenders,
        positions: Vec::new(),
        spot0,
    }
}

/// Refills LiteSVM's airdrop account (as the harness's own `ensure_airdrop` does).
pub fn refill_faucet(env: &mut Env) {
    let key = env.svm.airdrop_pubkey();
    env.svm
        .set_account(
            key,
            solana_account::Account {
                lamports: u64::MAX / 4,
                data: Vec::new(),
                owner: anchor_lang::system_program::ID,
                executable: false,
                rent_epoch: 0,
            },
        )
        .expect("refill the airdrop account");
}

/// The world of `profile` for `seed`.
pub fn world(profile: Profile, seed: u64, opts: crate::runner::Opts) -> World {
    let mut rng = Rng::new(seed ^ SETUP_SALT);
    let (env, slots) = match profile {
        Profile::Mock | Profile::Mixed => {
            let mut env = mock_env(&mut rng);
            let mut slots = Vec::new();
            for i in 0..profile.markets() {
                let program = if (rng.below(2) + i as u64).is_multiple_of(2) {
                    TokenProgram::Spl
                } else {
                    TokenProgram::Token2022
                };
                let spec = mock_spec(&mut rng, program);
                let v = MockVenue::new(&mut env, spec);
                let lend = spec.lend / 4;
                slots.push(slot(&mut env, Box::new(v), lend));
            }
            (env, slots)
        }
        Profile::Curve => crate::venue::curve::setup(&mut rng),
        Profile::PumpSwap => crate::venue::pswap::setup(&mut rng),
        Profile::AmmV4 | Profile::Cpmm => crate::venue::ray::setup(profile, &mut rng),
    };
    let mut env = env;
    let keeper = env.funded_keypair();
    let donor = env.funded_keypair();
    // O17's role: installed in three runs out of four; inert (the default key) otherwise.
    let mut m7 = Rng::new(seed ^ M7_SALT);
    let coverage = m7.chance(3, 4).then(|| {
        let kp = Keypair::new();
        plant_coverage_authority(&mut env, kp.pubkey());
        kp
    });
    let mut w = World {
        env,
        profile,
        slots,
        keeper,
        donor,
        stats: Stats::default(),
        differential: opts.differential,
        avoid_known: opts.avoid_known,
        disabled_bits: 0,
        step: 0,
        undo: Vec::new(),
        coverage,
        mint_saved: Vec::new(),
        ledger: Default::default(),
        pswap_expire: None,
        s_peaks: Vec::new(),
        had_position: Default::default(),
        absent_owners: crate::world::ABSENT_OWNERS
            .with(|c| c.get())
            .unwrap_or_else(|| Rng::new(seed ^ KI66_SALT).chance(1, 2)),
    };
    for si in 0..w.slots.len() {
        let key = w.slots[si].venue.vm().market();
        w.env.track_market(key);
        w.crank(si);
    }
    w
}
