//! `initialize_config`, `propose_config` / `apply_config` / `cancel_config`, `set_pause`,
//! `disable_venue` (ARCH §3.1; M1 exit criteria: timelock, bounds and pause tests; T-CONFIG's
//! on-chain part).

use anchor_lang::error::ErrorCode as AnchorErrorCode;
use anchor_lang::prelude::Pubkey;
use shortwin::PauseScope;
use shortwin::constants::{
    DEFAULT_TIMELOCK_S, MIN_TIMELOCK_S, VENUE_BIT_JUPITER, VENUE_BIT_PUMP_SWAP, VENUE_BITS_KNOWN,
};
use shortwin::errors::ShortwinError;
use shortwin::events::{
    ConfigApplied, ConfigCancelled, ConfigProposed, PauseSet, SCOPE_GLOBAL, SCOPE_MARKET,
    VenueDisabled,
};
use shortwin::state::{
    CONFIG_FLAG_PAUSED_OPENS, ConfigChange, EmaParamsArgs, MARKET_FLAG_PAUSED_OPENS, Params,
    TierParams, change_hash,
};
use shortwin_litesvm_tests::errors::{assert_anchor_err, assert_err, assert_failed, ok};
use shortwin_litesvm_tests::tokens::{self, TokenProgram};
use shortwin_litesvm_tests::{Env, START_TS, TxResult, events, ix, pda};
use shortwin_math::params::{ParamSet, Rule};
use shortwin_math::tier::Tier;
use solana_keypair::Keypair;
use solana_signer::Signer;

fn propose(env: &mut Env, change: ConfigChange) -> TxResult {
    let gov = env.governance.insecure_clone();
    env.send(&[ix::propose_config(&gov.pubkey(), change)], &[&gov])
}

fn apply(env: &mut Env) -> TxResult {
    let apply = env.apply_config_ix();
    env.send(&[apply], &[])
}

/// Proposes, waits out the timelock and applies.
fn propose_and_apply(env: &mut Env, change: ConfigChange) {
    ok(propose(env, change));
    env.warp(i64::from(env.config().timelock_s));
    ok(apply(env));
}

fn params_change(ps: &ParamSet) -> ConfigChange {
    ConfigChange::Params {
        params: Params::from_math(&ps.params),
        tiers: ps.tiers.map(|t| TierParams::from_math(&t)),
    }
}

// ---- initialize_config -------------------------------------------------------------------

#[test]
fn initialize_config_writes_everything() {
    let env = Env::initialized();
    let args = env.default_init_args();
    let c = env.config();
    assert_eq!(c.version, shortwin::constants::CONFIG_VERSION);
    assert_eq!(c.governance, args.governance);
    assert_eq!(c.pause_authority, args.pause_authority);
    assert_eq!(c.coverage_authority, Pubkey::default());
    assert_eq!(c.fee_recipient, args.fee_recipient);
    assert_eq!(c.pyth_sol_usd, args.pyth_sol_usd);
    assert_eq!(c.pyth_feed_id, args.pyth_feed_id);
    assert_eq!(c.timelock_s, DEFAULT_TIMELOCK_S);
    assert_eq!(c.venue_enabled, args.venue_enabled);
    assert_eq!(c.flags, 0);
    assert_eq!(c.venue_route_broken, 0);
    assert!(c.params == args.params);
    assert!(c.tiers == args.tiers);
    assert_eq!(c.ema_params.values(), args.ema);
    assert_eq!((c.ema_params.version, c.ema_params.force_at), (1, 0));
    assert_eq!(
        (c.kink.util_kink_bps, c.kink.version),
        (args.util_kink_bps, 1)
    );
    assert!(!c.pending.is_some());
    assert_eq!(c.param_set().ok(), Some(ParamSet::DEFAULT));

    let g = env.venue_guard();
    assert_eq!(g.upgraded, 0);
    assert_eq!(g.guard_slot, args.guard_slot);
    assert_eq!(g.flagged_slot, [0; 6]);

    let o = env.oracle_cache();
    assert_eq!((o.price, o.publish_time), (0, 0));
    assert_ne!(o.bump, 0);
}

#[test]
fn initialize_config_requires_the_upgrade_authority() {
    let mut env = Env::new();
    let args = env.default_init_args();
    let impostor = env.funded_keypair();
    let res = env.send(
        &[ix::initialize_config(&impostor.pubkey(), args)],
        &[&impostor],
    );
    assert_err(&res, ShortwinError::Unauthorized);

    // An immutable short.win (no upgrade authority) can never be initialized by anyone.
    env.set_own_upgrade_authority(None);
    let authority = env.upgrade_authority.insecure_clone();
    let args = env.default_init_args();
    let res = env.send(
        &[ix::initialize_config(&authority.pubkey(), args)],
        &[&authority],
    );
    assert_err(&res, ShortwinError::Unauthorized);
}

#[test]
fn initialize_config_runs_once() {
    let mut env = Env::initialized();
    let args = env.default_init_args();
    assert_failed(&env.initialize_config(args));
}

#[test]
fn initialize_config_checks_its_arguments() {
    let mut env = Env::new();
    let base = env.default_init_args();

    let mut a = base.clone();
    a.timelock_s = MIN_TIMELOCK_S - 1;
    assert_err(&env.initialize_config(a), ShortwinError::TimelockTooShort);

    let mut a = base.clone();
    a.governance = Pubkey::default();
    assert_err(&env.initialize_config(a), ShortwinError::InvalidParams);

    let mut a = base.clone();
    a.fee_recipient = Pubkey::default();
    assert_err(&env.initialize_config(a), ShortwinError::InvalidParams);

    let mut a = base.clone();
    a.venue_enabled = VENUE_BITS_KNOWN | (1 << 5);
    assert_err(&env.initialize_config(a), ShortwinError::InvalidParams);

    let mut a = base.clone();
    a.params.open_fee_bps = 101; // compiled bound [0, 100]
    assert_err(&env.initialize_config(a), ShortwinError::InvalidParams);

    let mut a = base.clone();
    a.util_kink_bps = 9_100; // relational: kink ≤ cap (9,000)
    assert_err(&env.initialize_config(a), ShortwinError::InvalidParams);

    let mut a = base.clone();
    a.ema.ema_dtmax_s = 3; // compiled bound [1, 2]
    assert_err(&env.initialize_config(a), ShortwinError::InvalidParams);

    // The minimum timelock itself is accepted.
    let mut a = base;
    a.timelock_s = MIN_TIMELOCK_S;
    ok(env.initialize_config(a));
    assert_eq!(env.config().timelock_s, MIN_TIMELOCK_S);
}

/// SQ-G8 (owner decision of 2026-10-02): `initialize_config` refuses any two of `governance`,
/// `pause_authority`, an installed `coverage_authority` and `fee_recipient` that are the same
/// key (`AuthoritiesNotDistinct`), every pair; an unset coverage key (`Pubkey::default()`) is
/// no key — the three others must still differ — and four distinct keys are accepted.
#[test]
fn initialize_config_refuses_shared_authorities() {
    let mut env = Env::new();
    let mut base = env.default_init_args();
    base.coverage_authority = env.coverage_authority.pubkey();
    for (i, j) in role_pairs() {
        let mut a = base.clone();
        let key = roles(&a)[i];
        set_role(&mut a, j, key);
        assert_err(
            &env.initialize_config(a),
            ShortwinError::AuthoritiesNotDistinct,
        );
    }
    let mut unset = base.clone();
    unset.coverage_authority = Pubkey::default();
    for (i, j) in role_pairs().filter(|&(i, j)| i != 2 && j != 2) {
        let mut a = unset.clone();
        let key = roles(&a)[i];
        set_role(&mut a, j, key);
        assert_err(
            &env.initialize_config(a),
            ShortwinError::AuthoritiesNotDistinct,
        );
    }
    ok(env.initialize_config(base));
    let c = env.config();
    assert_eq!(c.coverage_authority, env.coverage_authority.pubkey());
}

/// SQ-G8 at an `Authorities` change: `propose_config` refuses every pair early
/// (`AuthoritiesNotDistinct`, nothing pending), and `apply_config` checks again — a pending
/// change that shares a key (planted, as no proposal can leave one) is refused at apply and
/// stays pending; an unset coverage key is skipped at both (the other three must still
/// differ), and a rotation to four distinct keys applies.
#[test]
fn authorities_changes_refuse_shared_keys_at_propose_and_apply() {
    let mut env = Env::initialized();
    let gov = env.governance.pubkey();
    let base = ConfigChange::Authorities {
        governance: gov,
        pause_authority: env.pause_authority.pubkey(),
        coverage_authority: env.coverage_authority.pubkey(),
        fee_recipient: env.fee_recipient.pubkey(),
    };
    for (i, j) in role_pairs() {
        let mut change = base.clone();
        let key = change_roles(&change)[i];
        set_change_role(&mut change, j, key);
        let before = env.config().pending.hash();
        assert_err(
            &propose(&mut env, change.clone()),
            ShortwinError::AuthoritiesNotDistinct,
        );
        assert_eq!(env.config().pending.hash(), before, "roles {i} and {j}");

        plant_pending(&mut env, &change);
        let res = env.send(&[ix::apply_config_for(&change)], &[]);
        assert_err(&res, ShortwinError::AuthoritiesNotDistinct);
        assert!(
            env.config().pending.is_some(),
            "a refused apply keeps the change"
        );
        assert_eq!(env.config().governance, gov);
    }
    // Unset coverage: only the three others must differ.
    let mut unset = base.clone();
    set_change_role(&mut unset, 2, Pubkey::default());
    let mut shared = unset.clone();
    set_change_role(&mut shared, 3, env.pause_authority.pubkey());
    assert_err(
        &propose(&mut env, shared.clone()),
        ShortwinError::AuthoritiesNotDistinct,
    );
    ok(propose(&mut env, unset.clone()));
    // ... and at the application: refused and kept with two of the three sharing a key,
    // applied with the three distinct.
    plant_pending(&mut env, &shared);
    assert_err(
        &env.send(&[ix::apply_config_for(&shared)], &[]),
        ShortwinError::AuthoritiesNotDistinct,
    );
    assert!(
        env.config().pending.is_some(),
        "a refused apply keeps the change"
    );
    ok(propose(&mut env, unset.clone()));
    env.warp(i64::from(env.config().timelock_s));
    ok(env.send(&[ix::apply_config_for(&unset)], &[]));
    assert_eq!(env.config().coverage_authority, Pubkey::default());
    // Four distinct keys (the coverage key installed) apply.
    let fees = Pubkey::new_unique();
    let mut rotate = base;
    set_change_role(&mut rotate, 3, fees);
    ok(propose(&mut env, rotate.clone()));
    env.warp(i64::from(env.config().timelock_s));
    ok(env.send(&[ix::apply_config_for(&rotate)], &[]));
    let c = env.config();
    assert_eq!(
        (c.coverage_authority, c.fee_recipient),
        (env.coverage_authority.pubkey(), fees)
    );
}

/// The six pairs `(i, j)`, `i < j`, of the four roles of [`roles`].
fn role_pairs() -> impl Iterator<Item = (usize, usize)> {
    (0..4).flat_map(|i| ((i + 1)..4).map(move |j| (i, j)))
}

/// `[governance, pause_authority, coverage_authority, fee_recipient]` of `a`.
fn roles(a: &shortwin::state::InitConfigArgs) -> [Pubkey; 4] {
    [
        a.governance,
        a.pause_authority,
        a.coverage_authority,
        a.fee_recipient,
    ]
}

fn set_role(a: &mut shortwin::state::InitConfigArgs, role: usize, key: Pubkey) {
    match role {
        0 => a.governance = key,
        1 => a.pause_authority = key,
        2 => a.coverage_authority = key,
        _ => a.fee_recipient = key,
    }
}

/// [`roles`] of an `Authorities` change.
fn change_roles(c: &ConfigChange) -> [Pubkey; 4] {
    match c {
        ConfigChange::Authorities {
            governance,
            pause_authority,
            coverage_authority,
            fee_recipient,
        } => [
            *governance,
            *pause_authority,
            *coverage_authority,
            *fee_recipient,
        ],
        _ => panic!("not an Authorities change"),
    }
}

fn set_change_role(c: &mut ConfigChange, role: usize, key: Pubkey) {
    if let ConfigChange::Authorities {
        governance,
        pause_authority,
        coverage_authority,
        fee_recipient,
    } = c
    {
        match role {
            0 => *governance = key,
            1 => *pause_authority = key,
            2 => *coverage_authority = key,
            _ => *fee_recipient = key,
        }
    }
}

/// **TEST-ONLY**: stores `change` as the pending change, due now, by rewriting `GlobalConfig`
/// (bypassing `propose_config`'s checks) — the only way to reach `apply_config`'s own re-check
/// of a change no proposal can leave pending.
fn plant_pending(env: &mut Env, change: &ConfigChange) {
    let key = pda::config();
    let mut acc = env.svm.get_account(&key).expect("GlobalConfig");
    let size = core::mem::size_of::<shortwin::state::GlobalConfig>();
    let c: &mut shortwin::state::GlobalConfig =
        bytemuck::from_bytes_mut(&mut acc.data[8..8 + size]);
    c.pending.set(change, env.now()).expect("encodes");
    env.svm.set_account(key, acc).expect("set GlobalConfig");
}

// ---- propose / apply / cancel --------------------------------------------------------------

#[test]
fn timelock_gates_apply() {
    let mut env = Env::initialized();
    let change = ConfigChange::Kink {
        util_kink_bps: 7_000,
    };
    let meta = ok(propose(&mut env, change.clone()));
    let proposed = events::one::<ConfigProposed>(&meta);
    let not_before = START_TS + i64::from(DEFAULT_TIMELOCK_S);
    assert_eq!(proposed.not_before, not_before);
    assert_eq!(proposed.kind, change.kind());
    let (kind, payload) = change.encode().unwrap();
    assert_eq!(proposed.hash, change_hash(kind, &payload));
    assert_eq!(env.config().pending.not_before, not_before);

    assert_err(&apply(&mut env), ShortwinError::TimelockNotElapsed);
    env.warp(i64::from(DEFAULT_TIMELOCK_S) - 1);
    assert_err(&apply(&mut env), ShortwinError::TimelockNotElapsed);
    env.warp(1);
    let meta = ok(apply(&mut env));
    let applied = events::one::<ConfigApplied>(&meta);
    assert_eq!((applied.kind, applied.hash), (proposed.kind, proposed.hash));

    let c = env.config();
    assert_eq!((c.kink.util_kink_bps, c.kink.version), (7_000, 2));
    assert!(!c.pending.is_some());
    assert_err(&apply(&mut env), ShortwinError::NoPendingChange);
}

#[test]
fn the_compiled_24h_floor_holds_end_to_end() {
    let mut env = Env::initialized();
    // Governance lowers the timelock to the compiled minimum…
    propose_and_apply(
        &mut env,
        ConfigChange::Timelock {
            timelock_s: MIN_TIMELOCK_S,
        },
    );
    assert_eq!(env.config().timelock_s, MIN_TIMELOCK_S);
    // …but never below it,
    assert_err(
        &propose(
            &mut env,
            ConfigChange::Timelock {
                timelock_s: MIN_TIMELOCK_S - 1,
            },
        ),
        ShortwinError::TimelockTooShort,
    );
    // and the next change waits exactly 24 h.
    ok(propose(
        &mut env,
        ConfigChange::Kink {
            util_kink_bps: 7_500,
        },
    ));
    env.warp(i64::from(MIN_TIMELOCK_S) - 1);
    assert_err(&apply(&mut env), ShortwinError::TimelockNotElapsed);
    env.warp(1);
    ok(apply(&mut env));
}

#[test]
fn only_governance_proposes() {
    let mut env = Env::initialized();
    let change = ConfigChange::Kink {
        util_kink_bps: 7_000,
    };
    for signer in [
        env.pause_authority.insecure_clone(),
        env.upgrade_authority.insecure_clone(),
        env.funded_keypair(),
    ] {
        let res = env.send(
            &[ix::propose_config(&signer.pubkey(), change.clone())],
            &[&signer],
        );
        assert_err(&res, ShortwinError::Unauthorized);
    }
}

#[test]
fn proposals_are_validated_against_the_compiled_bounds() {
    let mut env = Env::initialized();
    // One representative per rule family: sim bounds, ECON-only bounds, a per-tier bound, the
    // per-tier SPEC CR minimum (v0.4.1) and relational checks.
    let cases: [(&str, Option<Tier>, u64); 7] = [
        (Rule::KappaLiqBps.name(), None, 3_001),
        (Rule::LiqPersistN.name(), None, 4),
        (Rule::MarketWarmupS.name(), None, 49),
        (Rule::LumpVestS.name(), None, 604_801),
        (Rule::TierMaxDurationS.name(), Some(Tier::D), 7_776_001),
        ("cr_liq_bps", Some(Tier::A), 12_499),
        ("delta_exp_bps", None, 250), // in [50, 300], but δ_liq (200) ≥ δ_exp fails
    ];
    for (name, tier, value) in cases {
        let mut ps = ParamSet::DEFAULT;
        assert!(ps.set(name, tier, value).is_ok(), "{name}");
        let res = propose(&mut env, params_change(&ps));
        assert_err(&res, ShortwinError::InvalidParams);
    }
    // Relational: the fee-scaled size cap may not exceed the OI cap.
    let mut ps = ParamSet::DEFAULT;
    ps.params.fee_size_cap_x100 = 500;
    ps.params.fee_oi_cap_x100 = 400;
    assert_err(
        &propose(&mut env, params_change(&ps)),
        ShortwinError::InvalidParams,
    );
    // Relational across variants: raising the kink above the current cap fails.
    assert_err(
        &propose(
            &mut env,
            ConfigChange::Kink {
                util_kink_bps: 9_100,
            },
        ),
        ShortwinError::InvalidParams,
    );
    // EMA parameters are bounded too.
    let mut ema = EmaParamsArgs::defaults();
    ema.ema_clamp_bps = 4_999;
    assert_err(
        &propose(&mut env, ConfigChange::EmaParams { ema }),
        ShortwinError::InvalidParams,
    );
    // An in-bounds proposal (both fee-scaled caps at their upper bounds) is accepted.
    let mut ps = ParamSet::DEFAULT;
    ps.params.fee_size_cap_x100 = 500;
    ps.params.fee_oi_cap_x100 = 1_000;
    ok(propose(&mut env, params_change(&ps)));
    assert_eq!(env.config().pending.kind, 1);
}

/// AIR-06 (ECON §5.7, #77): Governance cannot set `min_notional` below the floor at which every
/// tier keeps buyback room at the liquidation boundary (0.053 SOL, `params::MIN_NOTIONAL_FLOOR`):
/// the old compiled floor of 0.01 SOL — where a Tier A–C position had none, so `liquidate` and
/// `expire` refused `UseSolSettle` and every exit ended in S1 — and one lamport under the floor
/// are refused at `propose_config`; the floor itself is accepted.
#[test]
fn min_notional_below_the_buyback_room_floor_is_refused() {
    let mut env = Env::initialized();
    for v in [10_000_000, 52_999_999] {
        let mut ps = ParamSet::DEFAULT;
        ps.params.min_notional = v;
        assert_err(
            &propose(&mut env, params_change(&ps)),
            ShortwinError::InvalidParams,
        );
    }
    let mut ps = ParamSet::DEFAULT;
    ps.params.min_notional = 53_000_000;
    ok(propose(&mut env, params_change(&ps)));
    assert_eq!(env.config().pending.kind, 1);
}

/// The EMA fields of `ps` as `EmaParamsArgs`.
fn ema_args(ps: &ParamSet) -> EmaParamsArgs {
    EmaParamsArgs {
        ema_dtmax_s: ps.ema.dtmax_s,
        ema_clamp_bps: ps.ema.clamp_bps,
        liq_slow_tau_s: ps.ema.slow_tau_s,
        liq_slow_dtmax_s: ps.ema.slow_dtmax_s,
    }
}

/// The EMA parameters (the `EmaParams` variant) and the kink (the `Kink` variant); every
/// other rule is a `Params` field.
fn is_ema(name: &str) -> bool {
    matches!(
        name,
        "ema_dtmax_s" | "ema_clamp_bps" | "liq_slow_tau_s" | "liq_slow_dtmax_s"
    )
}

/// The one-variant proposal that moves `ps`'s `name` (T-CONFIG).
fn change_for(name: &str, ps: &ParamSet) -> ConfigChange {
    if is_ema(name) {
        ConfigChange::EmaParams { ema: ema_args(ps) }
    } else if name == "util_kink_bps" {
        ConfigChange::Kink {
            util_kink_bps: ps.params.util_kink_bps,
        }
    } else {
        params_change(ps)
    }
}

/// `initialize_config`'s arguments carrying the whole of `ps`.
fn init_args(env: &Env, ps: &ParamSet) -> shortwin::state::InitConfigArgs {
    let mut a = env.default_init_args();
    a.params = Params::from_math(&ps.params);
    a.tiers = ps.tiers.map(|t| TierParams::from_math(&t));
    a.ema = ema_args(ps);
    a.util_kink_bps = ps.params.util_kink_bps;
    a
}

/// `params::validate` then `params::validate_econ` (the program's gate, ARCH §2.3.3).
fn math_accepts(ps: &ParamSet) -> bool {
    shortwin_math::params::validate(ps).is_ok() && shortwin_math::params::validate_econ(ps).is_ok()
}

/// Every compiled bound rule pushed one past each end (each tier's value for a per-tier rule),
/// as the `ParamSet` it gives: `(rule name, tier, value, set)`.
fn bound_violations() -> Vec<(&'static str, Option<Tier>, u64, ParamSet)> {
    let mut out = Vec::new();
    for rule in Rule::ALL {
        let (lo, hi) = rule.bounds();
        if (lo, hi) == (0, u64::MAX) {
            continue; // relational only
        }
        let tiers: Vec<Option<Tier>> = if rule.per_tier() {
            [Tier::A, Tier::B, Tier::C, Tier::D].map(Some).to_vec()
        } else {
            vec![None]
        };
        for tier in tiers {
            for v in [lo.checked_sub(1), hi.checked_add(1)].into_iter().flatten() {
                let mut ps = ParamSet::DEFAULT;
                if ps.set(rule.name(), tier, v).is_ok() {
                    out.push((rule.name(), tier, v, ps));
                }
            }
        }
    }
    out
}

#[test]
fn every_compiled_bound_is_enforced_on_chain() {
    // T-CONFIG (on-chain part; M1-FID-7): each bound rule pushed one past each end — the EMA
    // rules through `EmaParams`, the kink through `Kink`, everything else through `Params`, for
    // every tier of a per-tier rule — is refused by propose_config, exactly when
    // `params::validate` / `validate_econ` refuse it; and the value at each end is accepted
    // whenever the math crate accepts it.
    let mut env = Env::initialized();
    let mut refused = 0;
    let mut kinds = std::collections::BTreeSet::new();
    for (name, tier, v, ps) in bound_violations() {
        assert!(!math_accepts(&ps), "{name} {tier:?} {v}");
        let change = change_for(name, &ps);
        kinds.insert(change.kind());
        let res = propose(&mut env, change);
        assert_err(&res, ShortwinError::InvalidParams);
        refused += 1;
    }
    assert!(refused >= 150, "only {refused} bound violations exercised");
    assert_eq!(kinds.len(), 3, "Params, EmaParams and Kink all exercised");
    // The edges themselves.
    let mut accepted = 0;
    for rule in Rule::ALL {
        let (lo, hi) = rule.bounds();
        if (lo, hi) == (0, u64::MAX) {
            continue;
        }
        let tier = rule.per_tier().then_some(Tier::B);
        for v in [lo, hi] {
            let mut ps = ParamSet::DEFAULT;
            if ps.set(rule.name(), tier, v).is_err() {
                continue;
            }
            let res = propose(&mut env, change_for(rule.name(), &ps));
            if math_accepts(&ps) {
                ok(res);
                accepted += 1;
            } else {
                assert_err(&res, ShortwinError::InvalidParams);
            }
        }
    }
    assert!(accepted > 100, "only {accepted} in-bound edges accepted");
}

#[test]
fn every_compiled_bound_is_enforced_by_initialize_config() {
    // T-CONFIG through `initialize_config` (M1-FID-7): the same sweep; every one is refused,
    // then the defaults initialize.
    let mut env = Env::new();
    let mut refused = 0;
    for (name, tier, v, ps) in bound_violations() {
        let args = init_args(&env, &ps);
        let res = env.initialize_config(args);
        assert!(
            shortwin_litesvm_tests::errors::custom_code(&res)
                == Some(u32::from(ShortwinError::InvalidParams)),
            "{name} {tier:?} {v}: {:?}",
            res.as_ref().err().map(|f| &f.err)
        );
        refused += 1;
    }
    assert!(refused >= 150, "only {refused}");
    let args = env.default_init_args();
    ok(env.initialize_config(args));
}

/// The math crate's T-CONFIG vectors (`crates/shortwin-math/tests/vectors/params/params.json`:
/// every compiled edge, the config gate's box, scan, targeted and stress sets and 3,000 seeded
/// random configurations, generated from `shortwin_sim.validate_params`) as parameter sets.
fn vector_sets() -> Vec<ParamSet> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../crates/shortwin-math/tests/vectors/params/params.json");
    let text = std::fs::read_to_string(&path).unwrap();
    let v: serde_json::Value = serde_json::from_str(&text).unwrap();
    let tier = |s: &str| match s {
        "A" => Tier::A,
        "B" => Tier::B,
        "C" => Tier::C,
        _ => Tier::D,
    };
    v["cases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|case| {
            let mut ps = ParamSet::DEFAULT;
            for kv in case[0].as_str().unwrap().split_whitespace() {
                let (key, value) = kv.split_once('=').unwrap();
                let (name, t) = match key.split_once('.') {
                    Some((t, name)) => (name, Some(tier(t))),
                    None => (key, None),
                };
                ps.set(name, t, value.parse().unwrap()).unwrap();
            }
            ps
        })
        .collect()
}

#[test]
fn sampled_param_sets_are_accepted_exactly_when_the_math_crate_accepts() {
    // T-CONFIG (M1-FID-7): a sample of the math crate's vectors, in-bound and out, through
    // `initialize_config` (the whole set at once) and, where the set moves one variant's
    // fields only, through `propose_config`: the program accepts exactly what `params::validate`
    // + `validate_econ` accept.
    let sets = vector_sets();
    assert!(sets.len() > 3_000);
    let default = ParamSet::DEFAULT;
    let mut refused_env = Env::new();
    let mut proposer = Env::initialized();
    let (mut accepted, mut refused, mut proposed) = (0, 0, 0);
    for ps in sets.iter().step_by(17) {
        let want = math_accepts(ps);
        if want {
            let mut env = Env::new();
            let args = init_args(&env, ps);
            ok(env.initialize_config(args));
            assert!(env.config().param_set().unwrap() == *ps);
            accepted += 1;
        } else {
            let args = init_args(&refused_env, ps);
            assert_err(
                &refused_env.initialize_config(args),
                ShortwinError::InvalidParams,
            );
            refused += 1;
        }
        let ema_moved = ps.ema != default.ema;
        let kink_moved = ps.params.util_kink_bps != default.params.util_kink_bps;
        let mut rest = *ps;
        rest.ema = default.ema;
        rest.params.util_kink_bps = default.params.util_kink_bps;
        let rest_moved = rest != default;
        let change = match (rest_moved, ema_moved, kink_moved) {
            (true, false, false) => Some(params_change(ps)),
            (false, true, false) => Some(ConfigChange::EmaParams { ema: ema_args(ps) }),
            (false, false, true) => Some(ConfigChange::Kink {
                util_kink_bps: ps.params.util_kink_bps,
            }),
            _ => None,
        };
        if let Some(change) = change {
            let res = propose(&mut proposer, change);
            if want {
                ok(res);
            } else {
                assert_err(&res, ShortwinError::InvalidParams);
            }
            proposed += 1;
        }
    }
    assert!(accepted >= 50 && refused >= 50, "{accepted} / {refused}");
    assert!(proposed >= 100, "{proposed}");
}

#[test]
fn propose_replaces_and_cancel_clears() {
    let mut env = Env::initialized();
    ok(propose(
        &mut env,
        ConfigChange::Kink {
            util_kink_bps: 7_000,
        },
    ));
    let replacement = ConfigChange::EnableVenues {
        bits: VENUE_BIT_JUPITER,
    };
    ok(propose(&mut env, replacement.clone()));
    assert_eq!(env.config().pending.kind, replacement.kind());
    assert!(env.config().pending.change().ok() == Some(replacement));

    // Cancel: Governance or Emergency only.
    let stranger = env.funded_keypair();
    let res = env.send(&[ix::cancel_config(&stranger.pubkey())], &[&stranger]);
    assert_err(&res, ShortwinError::Unauthorized);

    let pause = env.pause_authority.insecure_clone();
    let meta = ok(env.send(&[ix::cancel_config(&pause.pubkey())], &[&pause]));
    let cancelled = events::one::<ConfigCancelled>(&meta);
    assert_eq!(cancelled.by, pause.pubkey());
    assert!(!env.config().pending.is_some());
    let res = env.send(&[ix::cancel_config(&pause.pubkey())], &[&pause]);
    assert_err(&res, ShortwinError::NoPendingChange);

    ok(propose(
        &mut env,
        ConfigChange::Kink {
            util_kink_bps: 7_000,
        },
    ));
    let gov = env.governance.insecure_clone();
    ok(env.send(&[ix::cancel_config(&gov.pubkey())], &[&gov]));
    env.warp(i64::from(DEFAULT_TIMELOCK_S));
    assert_err(&apply(&mut env), ShortwinError::NoPendingChange);
}

#[test]
fn apply_is_permissionless_and_sets_versions() {
    let mut env = Env::initialized();
    let mut ps = ParamSet::DEFAULT;
    ps.params.open_fee_bps = 30;
    propose_and_apply(&mut env, params_change(&ps));
    let c = env.config();
    assert_eq!(c.params.open_fee_bps, 30);
    assert_eq!(c.params_version, 2);

    let ema = EmaParamsArgs {
        ema_dtmax_s: 1,
        ema_clamp_bps: 8_000,
        liq_slow_tau_s: 1_800,
        liq_slow_dtmax_s: 30,
    };
    ok(propose(&mut env, ConfigChange::EmaParams { ema }));
    env.warp(i64::from(DEFAULT_TIMELOCK_S));
    let applied_at = env.now();
    ok(apply(&mut env));
    let c = env.config();
    assert_eq!(c.ema_params.values(), ema);
    assert_eq!(c.ema_params.version, 2);
    assert_eq!(
        c.ema_params.force_at,
        applied_at + i64::from(c.params.ema_force_delay_s)
    );
    assert_eq!(
        c.kink.version, 1,
        "a forced EMA adoption never advances the kink"
    );
}

#[test]
fn authorities_rotate() {
    let mut env = Env::initialized();
    let new_gov = env.funded_keypair();
    let new_pause = env.funded_keypair();
    let new_fees = Pubkey::new_unique();
    propose_and_apply(
        &mut env,
        ConfigChange::Authorities {
            governance: new_gov.pubkey(),
            pause_authority: new_pause.pubkey(),
            coverage_authority: Pubkey::default(),
            fee_recipient: new_fees,
        },
    );
    let c = env.config();
    assert_eq!(
        (c.governance, c.pause_authority, c.fee_recipient),
        (new_gov.pubkey(), new_pause.pubkey(), new_fees)
    );
    let change = ConfigChange::Kink {
        util_kink_bps: 7_000,
    };
    assert_err(
        &propose(&mut env, change.clone()),
        ShortwinError::Unauthorized,
    );
    ok(env.send(
        &[ix::propose_config(&new_gov.pubkey(), change)],
        &[&new_gov],
    ));
    // A default governance or Emergency key would freeze the config: refused.
    let res = env.send(
        &[ix::propose_config(
            &new_gov.pubkey(),
            ConfigChange::Authorities {
                governance: new_gov.pubkey(),
                pause_authority: Pubkey::default(),
                coverage_authority: Pubkey::default(),
                fee_recipient: new_fees,
            },
        )],
        &[&new_gov],
    );
    assert_err(&res, ShortwinError::InvalidParams);
}

#[test]
fn oracle_feed_changes_through_the_timelock() {
    let mut env = Env::initialized();
    let account = Pubkey::new_unique();
    propose_and_apply(
        &mut env,
        ConfigChange::OracleFeed {
            account,
            feed_id: [9; 32],
        },
    );
    let c = env.config();
    assert_eq!((c.pyth_sol_usd, c.pyth_feed_id), (account, [9; 32]));
    assert_err(
        &propose(
            &mut env,
            ConfigChange::OracleFeed {
                account: Pubkey::default(),
                feed_id: [9; 32],
            },
        ),
        ShortwinError::InvalidParams,
    );
}

// ---- set_pause ------------------------------------------------------------------------------

#[test]
fn global_pause_by_emergency_or_governance() {
    let mut env = Env::initialized();
    let pause = env.pause_authority.insecure_clone();
    let gov = env.governance.insecure_clone();

    let meta = ok(env.send(
        &[ix::set_pause(
            &pause.pubkey(),
            PauseScope::Global,
            true,
            None,
        )],
        &[&pause],
    ));
    let e = events::one::<PauseSet>(&meta);
    assert_eq!(
        (e.scope, e.paused, e.market),
        (SCOPE_GLOBAL, true, Pubkey::default())
    );
    assert!(env.config().paused_opens());
    assert_eq!(
        env.config().flags & CONFIG_FLAG_PAUSED_OPENS,
        CONFIG_FLAG_PAUSED_OPENS
    );

    ok(env.send(
        &[ix::set_pause(
            &gov.pubkey(),
            PauseScope::Global,
            false,
            None,
        )],
        &[&gov],
    ));
    assert!(!env.config().paused_opens());

    let stranger = env.funded_keypair();
    let res = env.send(
        &[ix::set_pause(
            &stranger.pubkey(),
            PauseScope::Global,
            true,
            None,
        )],
        &[&stranger],
    );
    assert_err(&res, ShortwinError::Unauthorized);
}

#[test]
fn market_pause_touches_only_that_market() {
    let mut env = Env::initialized();
    let (a, _) = env.install_market(TokenProgram::Spl, 6);
    let (b, _) = env.install_market(TokenProgram::Token2022, 9);
    let pause = env.pause_authority.insecure_clone();

    let meta = ok(env.send(
        &[ix::set_pause(
            &pause.pubkey(),
            PauseScope::Market,
            true,
            Some(a.market),
        )],
        &[&pause],
    ));
    let e = events::one::<PauseSet>(&meta);
    assert_eq!((e.scope, e.market), (SCOPE_MARKET, a.market));
    assert!(env.market(&a.market).has_flag(MARKET_FLAG_PAUSED_OPENS));
    assert!(!env.market(&b.market).has_flag(MARKET_FLAG_PAUSED_OPENS));
    assert!(!env.config().paused_opens());

    ok(env.send(
        &[ix::set_pause(
            &pause.pubkey(),
            PauseScope::Market,
            false,
            Some(a.market),
        )],
        &[&pause],
    ));
    assert!(!env.market(&a.market).has_flag(MARKET_FLAG_PAUSED_OPENS));

    // The scope must match the accounts.
    let res = env.send(
        &[ix::set_pause(
            &pause.pubkey(),
            PauseScope::Market,
            true,
            None,
        )],
        &[&pause],
    );
    assert_err(&res, ShortwinError::AccountMismatch);
    let res = env.send(
        &[ix::set_pause(
            &pause.pubkey(),
            PauseScope::Global,
            true,
            Some(a.market),
        )],
        &[&pause],
    );
    assert_err(&res, ShortwinError::AccountMismatch);

    // A non-market account in the market slot is refused by Anchor.
    let res = env.send(
        &[ix::set_pause(
            &pause.pubkey(),
            PauseScope::Market,
            true,
            Some(pda::config()),
        )],
        &[&pause],
    );
    assert_anchor_err(&res, AnchorErrorCode::AccountDiscriminatorMismatch);
}

// ---- disable_venue ----------------------------------------------------------------------------

#[test]
fn disable_is_immediate_and_enable_is_timelocked() {
    let mut env = Env::initialized();
    let pause = env.pause_authority.insecure_clone();
    let bit = VENUE_BIT_PUMP_SWAP.trailing_zeros() as u8;

    // Governance cannot use the Emergency power.
    let gov = env.governance.insecure_clone();
    let res = env.send(&[ix::disable_venue(&gov.pubkey(), bit)], &[&gov]);
    assert_err(&res, ShortwinError::Unauthorized);

    let meta = ok(env.send(&[ix::disable_venue(&pause.pubkey(), bit)], &[&pause]));
    let e = events::one::<VenueDisabled>(&meta);
    assert_eq!(e.bit, bit);
    assert_eq!(env.config().venue_enabled & VENUE_BIT_PUMP_SWAP, 0);
    assert_eq!(e.venue_enabled, env.config().venue_enabled);

    // Unknown bits are refused.
    for bad in [5u8, 31, 32, 255] {
        let res = env.send(&[ix::disable_venue(&pause.pubkey(), bad)], &[&pause]);
        assert_err(&res, ShortwinError::InvalidParams);
    }

    // Re-enabling goes through Governance and the timelock.
    ok(propose(
        &mut env,
        ConfigChange::EnableVenues {
            bits: VENUE_BIT_PUMP_SWAP,
        },
    ));
    assert_eq!(env.config().venue_enabled & VENUE_BIT_PUMP_SWAP, 0);
    env.warp(i64::from(DEFAULT_TIMELOCK_S));
    ok(apply(&mut env));
    assert_eq!(
        env.config().venue_enabled & VENUE_BIT_PUMP_SWAP,
        VENUE_BIT_PUMP_SWAP
    );
}

/// ★ AIR-24 (leads L-8). `disable_venue` cleared only the enabled bit and left a pending
/// `EnableVenues` change in place, which anyone could apply at `not_before`
/// (`[apply_config, open_short…]`): the Emergency's disable was undone unless it also cancelled the
/// change in time. Reproduced on the pre-fix build: the re-enable Governance proposed before a
/// second incident was applied after the Emergency disabled the venue again. Now `disable_venue`
/// cancels a pending `EnableVenues` change that would set the bit it clears (`ConfigCancelled`, by
/// the Emergency): the venue comes back only through a new proposal and its full timelock. A
/// pending change that does not set that bit stays pending and applies.
#[test]
fn air24_disable_venue_cancels_a_pending_enable_of_that_venue() {
    let mut env = Env::initialized();
    let pause = env.pause_authority.insecure_clone();
    let timelock = i64::from(DEFAULT_TIMELOCK_S);
    let bit = VENUE_BIT_PUMP_SWAP.trailing_zeros() as u8;
    let disable = |env: &mut Env| {
        let pause = env.pause_authority.insecure_clone();
        ok(env.send(&[ix::disable_venue(&pause.pubkey(), bit)], &[&pause]))
    };

    // An incident: the Emergency disables PumpSwap; Governance proposes re-enabling it.
    disable(&mut env);
    let enable = ConfigChange::EnableVenues {
        bits: VENUE_BIT_PUMP_SWAP | VENUE_BIT_JUPITER,
    };
    ok(propose(&mut env, enable.clone()));
    let pending = env.config().pending.hash();
    // A second incident before `not_before`: the Emergency disables PumpSwap again.
    env.warp(timelock / 2);
    let meta = disable(&mut env);
    // `not_before` passes: anyone's `apply_config` finds nothing to re-enable the venue with.
    env.warp(timelock);
    assert_err(&apply(&mut env), ShortwinError::NoPendingChange);
    assert_eq!(env.config().venue_enabled & VENUE_BIT_PUMP_SWAP, 0);
    let _: VenueDisabled = events::one(&meta);
    let cancelled: ConfigCancelled = events::one(&meta);
    assert_eq!(
        (cancelled.kind, cancelled.hash, cancelled.by),
        (enable.kind(), pending, pause.pubkey())
    );

    // A pending change that does not set the disabled bit is left alone.
    let other = ConfigChange::EnableVenues {
        bits: VENUE_BIT_JUPITER,
    };
    ok(propose(&mut env, other.clone()));
    let meta = disable(&mut env);
    assert!(events::of::<ConfigCancelled>(&meta).is_empty());
    assert!(env.config().pending.change().ok() == Some(other));
    env.warp(timelock);
    ok(apply(&mut env));
    let c = env.config();
    assert_eq!(c.venue_enabled & VENUE_BIT_JUPITER, VENUE_BIT_JUPITER);
    assert_eq!(c.venue_enabled & VENUE_BIT_PUMP_SWAP, 0);

    // The venue comes back through a new proposal and its full timelock.
    ok(propose(
        &mut env,
        ConfigChange::EnableVenues {
            bits: VENUE_BIT_PUMP_SWAP,
        },
    ));
    env.warp(timelock - 1);
    assert_err(&apply(&mut env), ShortwinError::TimelockNotElapsed);
    env.warp(1);
    ok(apply(&mut env));
    assert_eq!(
        env.config().venue_enabled & VENUE_BIT_PUMP_SWAP,
        VENUE_BIT_PUMP_SWAP
    );
}

#[test]
fn admin_instructions_never_move_funds() {
    // I4 runs inside Env::send for every admin-signed instruction; drive every admin power
    // over a **funded** market (tokens in `pool_vault`, lender SOL and protocol fees in
    // `MarketVault`; M1-FID-5) and let it check, alone and bundled with a lender's own
    // withdrawal and claim (I4 is decided per instruction: an admin co-signing the
    // transaction does not make the lender's outflow "admin-signed").
    let mut env = Env::initialized();
    let authority = Keypair::new();
    let payer = env.payer.insecure_clone();
    let mint = tokens::create_mint(
        &mut env.svm,
        &payer,
        &authority.pubkey(),
        6,
        TokenProgram::Spl,
    );
    let creator = env.funded_keypair();
    ok(env.send(
        &[ix::create_market(
            &creator.pubkey(),
            &mint,
            &TokenProgram::Spl.id(),
            0,
            &pda::canonical_pool(&mint),
        )],
        &[&creator],
    ));
    let market = pda::market(&mint).0;
    let h = tokens::holder(
        &mut env.svm,
        &payer,
        &mint,
        &authority,
        10_000_000,
        TokenProgram::Spl,
    );
    let owner = h.owner.insecure_clone();
    let lk = ix::LenderKeys {
        owner: owner.pubkey(),
        mint,
        token_program: TokenProgram::Spl.id(),
        owner_token_account: h.token_account,
    };
    ok(env.send(&[ix::deposit(&lk, 10_000_000, 0)], &[&owner]));
    ok(env.send(
        &[ix::cover_deficit(&owner.pubkey(), &market, 2_000_000)],
        &[&owner],
    ));
    let v = env.market_vault(&market);
    assert!(v.lender_sol > 0 && v.protocol_fees > 0);

    let pause = env.pause_authority.insecure_clone();
    let gov = env.governance.insecure_clone();
    ok(env.send(
        &[
            ix::set_pause(&pause.pubkey(), PauseScope::Global, true, None),
            ix::set_pause(&gov.pubkey(), PauseScope::Market, true, Some(market)),
            ix::disable_venue(&pause.pubkey(), 0),
            ix::propose_config(
                &gov.pubkey(),
                ConfigChange::Kink {
                    util_kink_bps: 7_000,
                },
            ),
        ],
        &[&pause, &gov],
    ));
    ok(env.send(&[ix::cancel_config(&gov.pubkey())], &[&gov]));
    // Admin instructions bundled with the lender's own exit, in both orders.
    let shares = env.lender(&market, &owner.pubkey()).unwrap().shares;
    ok(env.send(
        &[
            ix::set_pause(&pause.pubkey(), PauseScope::Global, false, None),
            ix::withdraw(&lk, shares / 4, 0),
        ],
        &[&pause, &owner],
    ));
    ok(env.send(
        &[
            ix::withdraw(&lk, shares / 4, 0),
            ix::set_pause(&pause.pubkey(), PauseScope::Market, false, Some(market)),
            ix::claim_sol(&owner.pubkey(), &market),
        ],
        &[&pause, &owner],
    ));
    // The keeper's coverage key (O17), installed through the timelock: it pauses and clears
    // the funded market's opens, alone and bundled with the lender's exit; the general I4 clause
    // and its coverage clause (only bit 6 of the market's flags changes) run on each.
    env.enable_coverage_authority();
    let cov = env.coverage_authority.insecure_clone();
    ok(env.set_coverage_pause(&market, true));
    ok(env.send(
        &[
            ix::set_coverage_pause(
                &cov.pubkey(),
                &market,
                false,
                shortwin::events::COVERAGE_REASON_KEEPER,
            ),
            ix::withdraw(&lk, shares / 8, 0),
        ],
        &[&cov, &owner],
    ));
    ok(env.send(
        &[
            ix::claim_sol(&owner.pubkey(), &market),
            ix::set_coverage_pause(
                &cov.pubkey(),
                &market,
                true,
                shortwin::events::COVERAGE_REASON_KEEPER,
            ),
        ],
        &[&cov, &owner],
    ));
    // ★OWNER-M7-3 (b): the Emergency vault clears that pause (clear only), bundled with the
    // lender's exit — the general I4 clause (admin-signed) and the coverage clause run on it.
    ok(env.send(
        &[
            ix::withdraw(&lk, shares / 16, 0),
            ix::set_coverage_pause(
                &pause.pubkey(),
                &market,
                false,
                shortwin::events::COVERAGE_REASON_KEEPER,
            ),
        ],
        &[&pause, &owner],
    ));
    assert!(
        !env.market(&market)
            .has_flag(shortwin::state::MARKET_FLAG_PAUSED_COVERAGE)
    );
    // The fee sweep is the one allowed outflow (to the configured recipient, ≤ the fees).
    let recipient = env.fee_recipient.pubkey();
    ok(env.send(
        &[
            ix::set_pause(&pause.pubkey(), PauseScope::Global, true, None),
            ix::sweep_protocol_fees(&market, &recipient),
        ],
        &[&pause],
    ));
    assert_eq!(env.market_vault(&market).protocol_fees, 0);
    env.assert_invariants();
}

/// `initialize_config` fits one legacy transaction (1,232 bytes) with the upgrade authority as
/// its fee payer: 1,198 bytes today. One more account (33 bytes: key and index) would leave a
/// single byte, which is why `initialize_config` takes the fee recipient by key only while an
/// `Authorities` change validates the account (M1-SEC-5; the deployer's own initial choice is
/// fixable by that change, and the sweep's rent guard covers a fresh key). Guards any growth of
/// `InitConfigArgs`.
#[test]
fn initialize_config_fits_a_legacy_transaction() {
    let env = Env::new();
    let authority = env.upgrade_authority.insecure_clone();
    let ix = ix::initialize_config(&authority.pubkey(), env.default_init_args());
    let msg = solana_message::Message::new(&[ix], Some(&authority.pubkey()));
    let size = 1 + 64 * usize::from(msg.header.num_required_signatures) + msg.serialize().len();
    assert!(size <= 1_232, "{size} bytes");
    assert_eq!(size, 1_198);
}
