//! T-CONFIG, host part (ARCHITECTURE §2.3.3, §8.1, §9 M0b; ECONOMICS §15): `params::validate`
//! equals `shortwin_sim.validate_params` — it accepts exactly the configurations the sim
//! accepts and reports exactly the sim's violations — on the defaults, every compiled bound
//! at its edges (the per-tier bounds for each tier), the per-tier SPEC CR minima, penalty
//! splits, every corner of `config_check.py`'s core box, its scan / targeted / stress
//! configurations and 3,000 seeded random configurations (`vectors/params/params.json`,
//! `gen_params.py`). The vectors move only parameters the sim models, so the program's gate
//! (`validate`, then `validate_econ`) accepts exactly what the sim accepts on all of them;
//! `params::validate_econ`'s rules (parameters the sim does not model) are unit-tested in
//! `src/params.rs`.

mod common;

use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use shortwin_math::params::{self, ParamSet, Rule, Violation};
use shortwin_math::tier::Tier;

#[derive(Deserialize)]
struct Vectors {
    defaults: BTreeMap<String, String>,
    cases: Vec<(String, String)>,
}

fn tier_of(s: &str) -> Tier {
    match s {
        "A" => Tier::A,
        "B" => Tier::B,
        "C" => Tier::C,
        "D" => Tier::D,
        _ => panic!("tier {s}"),
    }
}

/// `name` or `T.name` → (name, tier).
fn scoped(key: &str) -> (&str, Option<Tier>) {
    match key.split_once('.') {
        Some((t, name)) => (name, Some(tier_of(t))),
        None => (key, None),
    }
}

fn rule_named(name: &str, tier: Option<Tier>) -> Rule {
    Rule::ALL
        .into_iter()
        .find(|r| r.name() == name && r.per_tier() == tier.is_some())
        .unwrap_or_else(|| panic!("no rule {name} (tier {tier:?})"))
}

fn load() -> Vectors {
    common::load_vectors("params", "params.json")
}

#[test]
fn defaults_are_the_sims() {
    let v = load();
    assert!(v.defaults.len() > 90, "{} defaults", v.defaults.len());
    for (key, value) in &v.defaults {
        let (name, tier) = scoped(key);
        let want: u64 = value.parse().expect("u64");
        assert_eq!(
            ParamSet::DEFAULT.get(name, tier),
            Some(want),
            "default of {key}"
        );
    }
    assert_eq!(params::validate(&ParamSet::DEFAULT), Ok(()));
    assert_eq!(params::validate_econ(&ParamSet::DEFAULT), Ok(()));
}

#[test]
fn validate_equals_the_sim() {
    let v = load();
    assert!(v.cases.len() >= 3_000);
    let (mut accepted, mut rejected) = (0, 0);
    let mut seen = BTreeSet::new();
    for (overrides, want) in &v.cases {
        let mut ps = ParamSet::DEFAULT;
        for kv in overrides.split_whitespace() {
            let (key, value) = kv.split_once('=').expect("name=value");
            let (name, tier) = scoped(key);
            ps.set(name, tier, value.parse().expect("u64"))
                .unwrap_or_else(|e| panic!("{kv}: {e}"));
        }
        let want: BTreeSet<Violation> = want
            .split_whitespace()
            .map(|tok| {
                let (tier, name) = match tok.split_once(':') {
                    Some((t, name)) => (Some(tier_of(t)), name),
                    None => (None, tok),
                };
                Violation {
                    rule: rule_named(name, tier),
                    tier,
                }
            })
            .collect();
        let mut sim = Vec::new();
        params::check_sim(&ps, |x| sim.push(x));
        assert!(sim.iter().all(|x| x.rule.in_sim()), "[{overrides}]");
        let got: BTreeSet<Violation> = sim.iter().copied().collect();
        assert_eq!(got.len(), sim.len(), "[{overrides}] reported twice");
        assert_eq!(got, want, "[{overrides}]");
        // `validate` accepts exactly what the sim accepts (M0B-S-1) and otherwise reports
        // the first sim rule; `check` is `check_sim` followed by `check_econ`.
        assert_eq!(
            params::validate(&ps).is_ok(),
            want.is_empty(),
            "[{overrides}] {:?}",
            params::validate(&ps)
        );
        assert_eq!(params::validate(&ps).err(), sim.first().copied());
        // The vectors move only parameters the sim models: `validate_econ` accepts every
        // case, so the program's gate (`validate`, then `validate_econ`) accepts exactly
        // what the sim accepts (one source of truth, ARCH §2.3.3).
        let mut econ = Vec::new();
        params::check_econ(&ps, |x| econ.push(x));
        assert!(econ.is_empty(), "[{overrides}] {econ:?}");
        assert_eq!(params::validate_econ(&ps), Ok(()), "[{overrides}]");
        let mut all = Vec::new();
        params::check(&ps, |x| all.push(x));
        assert_eq!(all, sim);
        if want.is_empty() {
            accepted += 1;
        } else {
            rejected += 1;
        }
        seen.extend(want.iter().map(|x| x.rule));
    }
    assert!(
        accepted >= 1_000 && rejected >= 1_000,
        "{accepted} / {rejected}"
    );
    // Every rule of the sim is exercised, including the split's parts and every per-tier
    // bound (M1 doc sync).
    let sim_rules: BTreeSet<Rule> = Rule::ALL.into_iter().filter(|r| r.in_sim()).collect();
    assert_eq!(seen, sim_rules);
}

/// M0B-S-1 (review), closed by the M1 doc sync: the configurations the v0.4.1 sim accepted
/// although ECON §15 bounds the parameter are refused by the sim and by `validate` alike;
/// `validate_econ` has no say in them. Sim: `validate_params(replace(DEFAULT,
/// pen_lender_bps=3198, pen_protocol_bps=166, pen_keeper_bps=6636)) == ["pen_keeper_bps=6636
/// outside [0, 5000]"]`, and with tier A `cr_open_bps = 100_001`, `["tier A
/// cr_open_bps=100001 outside [0, 100000]"]`. A bound of a parameter the sim does not model
/// (`min_notional`) is `validate_econ`'s alone.
#[test]
fn validate_refuses_what_the_sim_refuses() {
    let mut ps = ParamSet::DEFAULT;
    ps.set("penalty_lender_bps", None, 3_198).expect("set");
    ps.set("penalty_protocol_bps", None, 166).expect("set");
    ps.set("penalty_keeper_bps", None, 6_636).expect("set");
    assert_eq!(
        params::validate(&ps),
        Err(Violation {
            rule: Rule::PenaltyKeeperBps,
            tier: None
        }),
        "penalty split"
    );
    assert_eq!(params::validate_econ(&ps), Ok(()));
    let mut ps = ParamSet::DEFAULT;
    ps.set("cr_open_bps", Some(Tier::A), 100_001).expect("set");
    assert_eq!(
        params::validate(&ps),
        Err(Violation {
            rule: Rule::TierCrOpenMax,
            tier: Some(Tier::A)
        }),
        "tier A cr_open_bps"
    );
    assert_eq!(params::validate_econ(&ps), Ok(()));
    let mut ps = ParamSet::DEFAULT;
    ps.set("min_notional", None, 9_999_999).expect("set");
    assert_eq!(params::validate(&ps), Ok(()), "min_notional");
    assert_eq!(
        params::validate_econ(&ps),
        Err(Violation {
            rule: Rule::MinNotional,
            tier: None
        })
    );
}
