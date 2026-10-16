//! The fuzzer's bounded CI smoke (`cargo test -p shortwin-fuzz --release --test smoke`): fixed
//! seeds on every profile with the invariant hooks and the fuzzer's oracles, the harness's own
//! self-tests (replay determinism of the action stream, `tmin` on an injected failure).
//!
//! `FUZZ_SMOKE_SEEDS` (default 3 per profile) and `FUZZ_SMOKE_ACTIONS` (default 200) scale it. A
//! debug build (a local `cargo test --workspace`) runs 1 × 60 per profile and skips the coverage
//! assertions: LiteSVM is about 20× slower there.
//! Needs the program builds, as `shortwin-litesvm-tests` does (`SHORTWIN_SO`,
//! `SHORTWIN_TEST_VENUE_SO`, `MOCK_CPAMM_SO`).

use shortwin_fuzz::action::{Action, Kind, Profile, stream};
use shortwin_fuzz::runner::{self, Crash, Opts};
use shortwin_fuzz::stats::Stats;
use shortwin_fuzz::world::{ABSENT_OWNERS, CREDITED_SKEW, DEFICIT_BOUND_PPM, INJECT, INJECTED};

fn env_num(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// The smoke seeds: the same on every machine (the action streams are a pure function of
/// them).
const SEED_BASE: u64 = 0x5eed_0000;

/// Whether this is the full smoke (a release build, as CI runs it).
const FULL: bool = !cfg!(debug_assertions);

fn smoke(profile: Profile) -> Stats {
    let seeds = env_num("FUZZ_SMOKE_SEEDS", if FULL { 3 } else { 1 });
    let actions = env_num("FUZZ_SMOKE_ACTIONS", if FULL { 200 } else { 60 }) as usize;
    let mut total = Stats::default();
    for i in 0..seeds {
        let seed = SEED_BASE + 97 * i + profile as u64;
        let o = runner::run(profile, seed, None, actions, Opts::default());
        if let Some(f) = &o.failure {
            let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("fuzz-smoke");
            let path = runner::write_crash(&dir, &o, false);
            panic!(
                "{} seed {seed} failed at step {} ({}):\n{}\ncrash file: {}",
                profile.name(),
                f.step,
                f.signature,
                f.message.lines().take(60).collect::<Vec<_>>().join("\n"),
                path.display()
            );
        }
        total.merge(&o.stats);
    }
    println!(
        "{}: {} runs, {} actions, {} transactions, {} hook checkpoints; opens {:?}, events {:?}",
        profile.name(),
        total.runs,
        total.executed(),
        total.txs,
        total.hook_checkpoints,
        total.ix.get("open_short"),
        total.events
    );
    assert!(total.executed() > 0);
    assert!(
        !FULL || total.ix.get("open_short").is_some_and(|v| v[0] > 0),
        "{}: no open landed in the smoke seeds",
        profile.name()
    );
    total
}

#[test]
fn smoke_mock() {
    let s = smoke(Profile::Mock);
    let exits = [
        "Liquidate",
        "Expire",
        "SolSettle(S1)",
        "SolSettle(S2)",
        "SolSettle(S3)",
    ]
    .iter()
    .map(|k| s.events.get(*k).copied().unwrap_or(0))
    .sum::<u64>();
    assert!(!FULL || exits > 0, "no keeper exit landed on the mock");
}

#[test]
fn smoke_mixed() {
    let s = smoke(Profile::Mixed);
    // O17's role (M7): the keeper's degraded-mode pauses and the adversarial signers.
    let landed = s.ix.get("set_coverage_pause").map_or(0, |v| v[0]);
    let refused = s.ix.get("set_coverage_pause").map_or(0, |v| v[1]);
    println!("mixed: set_coverage_pause landed {landed}, refused {refused}");
    assert!(
        !FULL || landed > 0,
        "no set_coverage_pause landed in the smoke seeds"
    );
}

/// The corpus (`corpus/*.json`: minimised reproducers of reported findings) replays: an open
/// finding either still fails with its recorded signature or passes (fixed: then mark the file
/// `"status": "fixed"`) — never with another failure; a finding marked fixed must pass, so its
/// reproducer is a regression. A finding that was an anomaly, not a failure, names its class in
/// `"anomaly"` (its `"signature"` is empty): fixed, the replay must also not report that class.
#[test]
fn corpus_replays() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus");
    let mut n = 0;
    for entry in std::fs::read_dir(&dir).expect("corpus dir") {
        let path = entry.expect("entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let c = runner::read_crash(&path).expect("corpus file");
        let raw: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read")).expect("json");
        let fixed = raw["status"].as_str() == Some("fixed");
        let o = runner::run(c.profile, c.seed, Some(&c.actions), 0, c.opts);
        if let Some(class) = raw["anomaly"].as_str() {
            let seen = o.stats.anomalies.get(class).map_or(0, |a| a.count);
            if fixed {
                assert_eq!(
                    seen,
                    0,
                    "{} is marked fixed but reports its anomaly `{class}` again:\n{}",
                    path.display(),
                    o.stats.anomalies[class]
                        .detail
                        .lines()
                        .take(30)
                        .collect::<Vec<_>>()
                        .join("\n")
                );
            } else {
                println!("{}: anomaly `{class}` × {seen}", path.display());
            }
        }
        match &o.failure {
            None => println!(
                "{}: passes ({})",
                path.display(),
                if fixed {
                    "fixed, regression"
                } else {
                    "fixed: mark it"
                }
            ),
            Some(f) if fixed => panic!(
                "{} is marked fixed but fails again ({}):\n{}",
                path.display(),
                f.signature,
                f.message.lines().take(30).collect::<Vec<_>>().join("\n")
            ),
            Some(f) => {
                assert_eq!(
                    f.signature,
                    c.signature,
                    "{} fails differently now:\n{}",
                    path.display(),
                    f.message.lines().take(30).collect::<Vec<_>>().join("\n")
                );
                println!("{}: still fails ({})", path.display(), f.signature);
            }
        }
        n += 1;
    }
    assert!(n > 0, "an empty corpus");
}

/// The crowd's instructions on the live curve that landed (`fuzz:crowd-<ix>-landed`) and were
/// refused, by pump instruction.
fn crowd(s: &Stats, ix: &str) -> (u64, u64) {
    let ev = |o: &str| {
        s.events
            .get(&format!("fuzz:crowd-{ix}-{o}"))
            .copied()
            .unwrap_or(0)
    };
    (ev("landed"), ev("refused"))
}

/// pump's v3 instructions the crowd trades since P7 (pump-final §12).
const CROWD_V3: [&str; 3] = ["buy_v3", "buy_exact_quote_in_v3", "sell_v3"];

#[test]
fn smoke_curve() {
    let s = smoke(Profile::Curve);
    // pump-final §12 (P7): the random smoke trades pump's v3 too (a sale may be refused there,
    // for want of real SOL reserves, as v2's are: `crowd_v3_trades_land` checks the refusals).
    for ix in CROWD_V3 {
        let (landed, refused) = crowd(&s, ix);
        println!("curve: crowd {ix} landed {landed}, refused {refused}");
        assert!(
            !FULL || landed > 0,
            "no crowd {ix} landed in the smoke seeds"
        );
    }
}

#[test]
fn smoke_pumpswap() {
    smoke(Profile::PumpSwap);
}

#[test]
fn smoke_amm_v4() {
    smoke(Profile::AmmV4);
}

#[test]
fn smoke_cpmm() {
    smoke(Profile::Cpmm);
}

/// M10 (G-G4): `fee_shock` reaches the pump venues. On the curve and PumpSwap profiles a shock to
/// level 5 — every tier's protocol fee in pump's or PumpSwap's `FeeConfig` at 1,000 bps, above
/// `venue_fee_hard_cap_bps` — held for 8 actions refuses the opens inside it `VenueFeeTooHigh`, and
/// `outage`'s fee branch runs (the keeper settling through it); every oracle holds and the
/// wind-down restores the configs. (Before M10 both returned "unsupported": the actions were
/// skipped.)
#[test]
fn fee_shocks_reach_the_pump_venues() {
    let acts: Vec<Action> = [
        "open 0 0 6000",
        "keeper_seconds 0 3 0",
        "fee_shock 0 12 7",
        "open 0 1 6000",
        "open 0 2 5000",
        "keeper_seconds 0 5 0",
        "close 0 0 0",
        "outage 0 0 1",
        "open 0 3 6000",
    ]
    .iter()
    .map(|a| a.parse().expect("action"))
    .collect();
    for profile in [Profile::Curve, Profile::PumpSwap] {
        let o = runner::run(profile, SEED_BASE + 7, Some(&acts), 0, Opts::default());
        if let Some(f) = &o.failure {
            panic!("{}: {}\n{}", profile.name(), f.signature, f.message);
        }
        let fee_shocks = o.stats.actions.get("fee_shock").copied().unwrap_or(0);
        let outages = o.stats.actions.get("outage").copied().unwrap_or(0);
        let refused = o
            .stats
            .errors
            .get("open_short:VenueFeeTooHigh")
            .copied()
            .unwrap_or(0);
        println!(
            "{}: fee_shock {fee_shocks}, outage {outages}, opens refused VenueFeeTooHigh {refused}",
            profile.name()
        );
        assert_eq!((fee_shocks, outages), (1, 1), "{}", profile.name());
        assert!(refused >= 2, "{}: {:?}", profile.name(), o.stats.errors);
    }
}

/// The action stream is a pure function of the profile and the seed, and a run of a given
/// list executes the same actions (the keys a run creates are random; the actions are not).
#[test]
fn a_seed_replays_its_actions() {
    let a = stream(Profile::Mock, 11, 80);
    assert_eq!(a, stream(Profile::Mock, 11, 80));
    let o1 = runner::run(Profile::Mock, 11, None, 80, Opts::default());
    let o2 = runner::run(Profile::Mock, 11, Some(&a), 0, Opts::default());
    assert!(o1.failure.is_none() && o2.failure.is_none());
    assert_eq!(o1.actions, o2.actions);
    assert_eq!(o1.stats.actions, o2.stats.actions);
    assert_eq!(o1.stats.skipped, o2.stats.skipped);
}

/// `tmin` shrinks a failing list to the actions the failure needs: an injected failure on the
/// first crank with a position open reduces to an open and a crank (plus what the open needs).
#[test]
fn tmin_minimises_an_injected_failure() {
    INJECT.with(|c| c.set(Some(Kind::Crank)));
    let mut failing = None;
    for seed in 0..20 {
        let opts = Opts {
            differential: false,
            avoid_known: true,
        };
        let o = runner::run(Profile::Mock, 900 + seed, None, 150, opts);
        if o.failure
            .as_ref()
            .is_some_and(|f| f.message.contains(INJECTED))
        {
            failing = Some(o);
            break;
        }
    }
    let o = failing.expect("an injected failure in 20 seeds");
    let before = o.actions.len();
    let c = Crash {
        profile: o.profile,
        seed: o.seed,
        opts: o.opts,
        actions: o.actions.clone(),
        signature: o.failure.as_ref().expect("failure").signature.clone(),
    };
    let m = runner::tmin(&c, 300, false);
    INJECT.with(|c| c.set(None));
    let f = m.failure.as_ref().expect("still failing");
    assert_eq!(f.signature, c.signature);
    println!(
        "tmin: {before} -> {} actions: {:?}",
        m.actions.len(),
        m.actions
    );
    assert!(
        m.actions.len() <= 4,
        "tmin left {} actions",
        m.actions.len()
    );
    assert!(m.actions.len() < before);
    assert_eq!(m.actions.last().map(|a| a.kind), Some(Kind::Crank));
}

/// M10: the deficit tripwire and the wind-down's second claim pass on FUZZ-M7-24H-3's reproducer
/// (a late depositor owing ≈ 1,028 SOL, its pro-rata share of a near-cap short's shortfall; since
/// AIR-07's drain gate the seeding lender leaves 1.98 ‰ of the claims unborrowed, else the late
/// deposit is refused `PoolDrained` and nothing is owed). The deficit sits at its bound — the late
/// depositor's share of the shortfall the exit's events and the receivable show — within the dust,
/// and the run fails the tripwire once the bound is scaled to 99 %: a debit 1 % above the lender's
/// share would be caught; a receivable over-credited by 1 SOL fails its cross-check against the
/// position's `credited_for`. The first pass's claim of the seeding lender finds `lender_sol` short
/// until the late depositor covers; the second pass pays it, and nothing is left owed.
#[test]
fn deficit_tripwire_on_fuzz_m7_24h_3() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("corpus/fuzz-m7-24h-3-amm-v4-wind-down-deficit-unfunded.json");
    let c = runner::read_crash(&path).expect("corpus file");
    let o = runner::run(c.profile, c.seed, Some(&c.actions), 0, c.opts);
    if let Some(f) = &o.failure {
        panic!("{}\n{}", f.signature, f.message);
    }
    let d = o.stats.deficits;
    let ev = |k: &str| o.stats.events.get(k).copied().unwrap_or(0);
    println!(
        "deficits {d:?}; second claims paid {}, nothing {}; unpaid {} lender_sol {}; events {:?}",
        ev("wind-down-second-claim-paid"),
        ev("wind-down-second-claim-nothing"),
        o.stats.wind_down_unpaid_max,
        o.stats.wind_down_lender_sol_max,
        o.stats.events
    );
    assert!(d.largest > 1_000_000_000_000, "{d:?}");
    let excess = i128::from(d.largest) - d.largest_bound as i128;
    assert!(
        excess <= shortwin_fuzz::shortfall::DEFICIT_DUST as i128,
        "{d:?}"
    );
    assert!(d.tightest_ppm >= 990_000, "the bound is loose: {d:?}");
    assert!(ev("shortfall-exits-short") > 0);
    assert!(ev("wind-down-second-claim-paid") > 0);
    assert!(o.stats.wind_down_unpaid_max <= shortwin_fuzz::world::CLAIM_DUST);

    DEFICIT_BOUND_PPM.with(|c| c.set(990_000));
    let o = runner::run(c.profile, c.seed, Some(&c.actions), 0, c.opts);
    DEFICIT_BOUND_PPM.with(|c| c.set(1_000_000));
    let f = o.failure.expect("the tripwire fires on a 99 % bound");
    assert!(f.message.contains("deficit tripwire"), "{}", f.message);

    // A receivable 1 SOL over its positions' credit would widen the bound with the deficit; the
    // independent `credited_for` catches it at the exit.
    CREDITED_SKEW.with(|c| c.set(1_000_000_000));
    let o = runner::run(c.profile, c.seed, Some(&c.actions), 0, c.opts);
    CREDITED_SKEW.with(|c| c.set(0));
    let f = o.failure.expect("the credited cross-check fires");
    assert!(f.message.contains("took"), "{}", f.message);
}

/// M10 (FUZZ-M10-24H-A, KI-65): the wind-down's second claim pass leaves exactly ECON §9.4's
/// I-P6 residue — rounding — and the final-tree 24 h runs' reproducers pass. On CPMM the last
/// lender, its shares all withdrawn, stays owed more than `CLAIM_DUST` of the accumulators'
/// carry while none of its realised SOL is left and the pot is empty (`wind-down-owed-rounding`:
/// 2,149 lamports; since AIR-07's vest flush the recorded list left 943, so its action 45 now
/// sells). The rebase-orphan credit the oracle used to accept above the hooks' dust cap
/// (`wind-down-orphan-credit`) is gone with KI-65's cumulative conversion (ECON
/// §9.5): on the curve corpus file, and on run B's three largest cases — 19,131,900,419 lamports
/// left on the mock, 890,963,742 on CPMM, 552,350,825 on PumpSwap, each with `S = 1` orphan
/// share that the vest the leavers forfeited released to — every share has a holder, the last
/// exit takes `S` to 0 and is paid the vest (R3B-2), and `lender_sol` ends within the cap. The
/// three `one` replays run in the full (release) smoke only.
#[test]
fn wind_down_residue_on_fuzz_m10_24h_a() {
    use shortwin_fuzz::world::CLAIM_DUST;
    use shortwin_litesvm_tests::invariants::lender::DEFAULT_LIVENESS_DUST_CAP;
    let check = |what: &str, o: &runner::Outcome| {
        if let Some(f) = &o.failure {
            panic!("{what}: {}\n{}", f.signature, f.message);
        }
        let ev = |k: &str| o.stats.events.get(k).copied().unwrap_or(0);
        println!(
            "{what}: unpaid {} lender_sol {}; events {:?}",
            o.stats.wind_down_unpaid_max, o.stats.wind_down_lender_sol_max, o.stats.events
        );
        assert!(o.stats.wind_downs > 0, "{what}: no wind-down");
        assert!(
            o.stats.wind_down_lender_sol_max <= DEFAULT_LIVENESS_DUST_CAP,
            "{what}: lender_sol {} stranded",
            o.stats.wind_down_lender_sol_max
        );
        assert_eq!(ev("wind-down-orphan-credit"), 0, "{what}");
        ev("wind-down-owed-rounding")
    };
    let read = |file: &str| {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(file);
        runner::read_crash(&path).expect("corpus file")
    };
    let c = read("corpus/fuzz-m10-24h-a-cpmm-carry-owed.json");
    let o = runner::run(c.profile, c.seed, Some(&c.actions), 0, c.opts);
    assert!(
        check("cpmm-carry-owed", &o) > 0,
        "no `wind-down-owed-rounding`"
    );
    assert!(o.stats.wind_down_unpaid_max > CLAIM_DUST);
    let c = read("corpus/fuzz-m10-24h-a-curve-orphan-credit.json");
    let o = runner::run(c.profile, c.seed, Some(&c.actions), 0, c.opts);
    check("curve-orphan-credit (KI-65)", &o);
    if !FULL {
        return;
    }
    for (profile, seed, length) in [
        (Profile::Mock, 31_013_687, 398),
        (Profile::Cpmm, 31_005_995, 390),
        (Profile::PumpSwap, 31_016_959, 386),
    ] {
        let o = runner::run(profile, seed, None, length, Opts::default());
        check(
            &format!(
                "one --profile {} --seed {seed} --length {length} (KI-65)",
                profile.name()
            ),
            &o,
        );
    }
}

/// KI-66's never-returns model (the KI-66 review), with absent owners forced on: three mock
/// seeds whose wind-down finds a lender held behind a rebase record (found by scanning seeds
/// 66,300,000 + 0…249 at `run`'s default 300 actions) and, in the full smoke, the KI-65
/// reproducers. The held lender is reached only by third-party syncs — the pot-only touch and
/// its forward chain (KI-66 full), 8 records at a time until the chain reaches the current
/// epoch, then one more that must move nothing (`pot-touch-not-idempotent`), each under the
/// KI-66 oracle — while the others exit and claim; the present lenders' unpaid stays within
/// what the absent lenders' own touches would still move to the pot after their chains (no
/// `wind-down-absent-owed-above-residual`), and once the absent owners return the wind-down's
/// I-P6 oracle passes.
#[test]
fn absent_owners_at_the_wind_down() {
    let check = |what: &str, o: &runner::Outcome| -> (u64, u64) {
        let ev = |k: &str| o.stats.events.get(k).copied().unwrap_or(0);
        println!(
            "{what}: absent {} held syncs {} (to the pot {}), chains started {} walked {}, \
             present owed {} (max {}); unpaid {} lender_sol {}",
            ev("wind-down-absent-owner"),
            ev("sync-lender-held"),
            ev("sync-lender-held-to-pot"),
            ev("pot-chain-hold"),
            ev("pot-chain-walk"),
            ev("wind-down-absent-present-owed"),
            o.stats.wind_down_absent_owed_max,
            o.stats.wind_down_unpaid_max,
            o.stats.wind_down_lender_sol_max
        );
        if let Some(f) = &o.failure {
            panic!("{what}: {}\n{}", f.signature, f.message);
        }
        assert!(
            o.stats.anomalies.is_empty(),
            "{what}: {:?}",
            o.stats.anomalies
        );
        (ev("wind-down-absent-owner"), ev("sync-lender-held-to-pot"))
    };
    ABSENT_OWNERS.with(|c| c.set(Some(true)));
    let seeds: &[(u64, usize)] = if FULL {
        &[(66_300_007, 265), (66_300_136, 249), (66_300_236, 92)]
    } else {
        &[(66_300_236, 92)]
    };
    for &(seed, length) in seeds {
        let o = runner::run(Profile::Mock, seed, None, length, Opts::default());
        let (absent, to_pot) = check(&format!("one --profile mock --seed {seed}"), &o);
        assert!(
            absent > 0 && to_pot > 0,
            "seed {seed}: no held lender's surplus moved"
        );
    }
    if FULL {
        for file in [
            "corpus/fuzz-m10-24h-a-curve-orphan-credit.json",
            "corpus/fuzz-m10-24h-a-cpmm-carry-owed.json",
        ] {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(file);
            let c = runner::read_crash(&path).expect("corpus file");
            check(
                file,
                &runner::run(c.profile, c.seed, Some(&c.actions), 0, c.opts),
            );
        }
        for (profile, seed, length) in [
            (Profile::Mock, 31_013_687, 398),
            (Profile::Cpmm, 31_005_995, 390),
            (Profile::PumpSwap, 31_016_959, 386),
        ] {
            let o = runner::run(profile, seed, None, length, Opts::default());
            check(
                &format!("one --profile {} --seed {seed}", profile.name()),
                &o,
            );
        }
    }
    ABSENT_OWNERS.with(|c| c.set(None));
}

/// FUZZ-M10-24H-D-B (KI-53 behind a rebase record): in the never-returns wind-down an absent
/// lender held at a rebase record owes a deficit — the record's own shortfall debit, pro rata to
/// the shares holding when the receivable the S3 settlement did not realise was debited — that
/// backs the present lenders' positive `pending`. Only its owner's `cover_deficit` pays that in,
/// so `World::absent_owners_out` bounds the present lenders' unpaid total by the absent
/// lenders' surplus moves plus their deficits, less what they are owed themselves beyond their
/// realised SOL (`wind-down-absent-deficit`); without the deficits the bound was the anomaly
/// `wind-down-absent-owed-above-residual` (owed 322,230,772 lamports, bound 1,002). When the
/// owners return they cover, and the second claim pass pays every lender. The bound stays tight
/// (3,111 lamports above the owed total here): a leak of 10,000 lamports beyond what the absent
/// owners' return brings (`ABSENT_OWED_LEAK`) fires it.
#[test]
fn absent_deficit_backs_the_present_lenders() {
    use shortwin_fuzz::world::{ABSENT_OWED_LEAK, CLAIM_DUST};
    const CLASS: &str = "wind-down-absent-owed-above-residual";
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("corpus/fuzz-m10-24h-d-b-curve-absent-deficit.json");
    let c = runner::read_crash(&path).expect("corpus file");
    let o = runner::run(c.profile, c.seed, Some(&c.actions), 0, c.opts);
    if let Some(f) = &o.failure {
        panic!("{}\n{}", f.signature, f.message);
    }
    let ev = |k: &str| o.stats.events.get(k).copied().unwrap_or(0);
    println!(
        "absent {} deficit markets {} present owed max {} (unpaid after the return {}); deficits \
         {:?}",
        ev("wind-down-absent-owner"),
        ev("wind-down-absent-deficit"),
        o.stats.wind_down_absent_owed_max,
        o.stats.wind_down_unpaid_max,
        o.stats.deficits
    );
    assert!(o.stats.anomalies.is_empty(), "{:?}", o.stats.anomalies);
    assert!(ev("wind-down-absent-deficit") > 0, "no absent deficit");
    assert!(
        o.stats.wind_down_absent_owed_max > 100_000_000,
        "the present lenders are not owed the deficit: {}",
        o.stats.wind_down_absent_owed_max
    );
    assert!(
        o.stats.deficits.checked > 0,
        "the returning owner's deficit was not checked"
    );
    assert!(o.stats.wind_down_unpaid_max <= CLAIM_DUST);

    ABSENT_OWED_LEAK.with(|l| l.set(10_000));
    let o = runner::run(c.profile, c.seed, Some(&c.actions), 0, c.opts);
    ABSENT_OWED_LEAK.with(|l| l.set(0));
    assert!(o.failure.is_none());
    assert!(
        o.stats.anomalies.contains_key(CLASS),
        "a 10,000-lamport leak passed the bound: {:?}",
        o.stats.anomalies
    );
}

/// M10: `stale_receivable` (FUZZ-M7-24H-3's shape) reaches the deficit tripwire on both Raydium
/// programs: a near-cap short left unattended for days, a late depositor of three times the
/// pool, the price steered until `B̂_live` lies in `(V − R_due, V − K_s − ert]`, keeper seconds — a
/// liquidation realises a shortfall (`shortfall-exits-short`), the wind-down covers the late
/// depositor's deficit within its bound (each at ≥ 99.9 % of it: the bound is tight), and the
/// second claim pass leaves nothing owed. PumpSwap reaches the window too, but rarely (a probe:
/// its counts are printed, not asserted); on the mock and the curve it stays empty.
#[test]
fn stale_receivable_reaches_the_tripwire() {
    for profile in [Profile::AmmV4, Profile::Cpmm, Profile::PumpSwap] {
        let mut checked = 0;
        for k in 0..2u32 {
            let acts: Vec<Action> = [
                format!("stale_receivable 0 {} {}", k * 7 + 2, k * 3002 + 2),
                "keeper_seconds 0 5 0".into(),
            ]
            .iter()
            .map(|a| a.parse().expect("action"))
            .collect();
            let o = runner::run(
                profile,
                SEED_BASE + 0x31 + u64::from(k),
                Some(&acts),
                0,
                Opts::default(),
            );
            if let Some(f) = &o.failure {
                panic!("{}: {}\n{}", profile.name(), f.signature, f.message);
            }
            let ev = |k: &str| o.stats.events.get(k).copied().unwrap_or(0);
            let d = o.stats.deficits;
            println!(
                "{} +{k}: deficits {d:?}, in window {}, shortfall exits {}, unpaid {}",
                profile.name(),
                ev("stale-receivable-in-window"),
                ev("shortfall-exits-short"),
                o.stats.wind_down_unpaid_max
            );
            assert!(d.checked == 0 || d.tightest_ppm >= 999_000, "{d:?}");
            checked += d.checked;
        }
        assert!(
            !FULL || checked > 0 || profile == Profile::PumpSwap,
            "{}: no deficit reached",
            profile.name()
        );
    }
}

/// FUZZ-24D-A (run D): PumpSwap's cached expiry prediction does not outlive its action. The
/// reproducer halts PumpSwap, has a liquidation refused `NoRoute` (which caches the expiry's
/// `NoRoute`), and lifts the halt by a direct VM write (the undo, no transaction) before the
/// expiry: the expiry must be predicted afresh — refused `NotExpired` on the executable pool,
/// as the program does — and both decisions must have been checked.
#[test]
fn pswap_expire_cache_fuzz24d_a() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("corpus/fuzz-24d-a-pumpswap-expire-stale-prediction.json");
    let c = runner::read_crash(&path).expect("corpus file");
    let o = runner::run(c.profile, c.seed, Some(&c.actions), 0, c.opts);
    if let Some(f) = &o.failure {
        panic!("{}\n{}", f.signature, f.message);
    }
    let ev = |k: &str| o.stats.events.get(k).copied().unwrap_or(0);
    assert!(
        ev("liquidate:pswap-differential-refused:NoRoute") > 0,
        "{:?}",
        o.stats.events
    );
    assert!(
        ev("expire:pswap-differential-refused:NotExpired") > 0,
        "{:?}",
        o.stats.events
    );
}

/// The actions of a scripted curve run (pump-final §12's checks below).
fn script(lines: &[&str]) -> Vec<Action> {
    lines.iter().map(|a| a.parse().expect("action")).collect()
}

/// A scripted curve run that must pass; its counters.
fn curve_script(what: &str, lines: &[&str]) -> Stats {
    let o = runner::run(
        Profile::Curve,
        SEED_BASE + 7,
        Some(&script(lines)),
        0,
        Opts::default(),
    );
    if let Some(f) = &o.failure {
        panic!("{what}: {}\n{}", f.signature, f.message);
    }
    println!("{what}: events {:?}", o.stats.events);
    o.stats
}

/// pump-final §12 (P7): the crowd's v3 trades land. On the live curve `trade` picks pump's
/// instruction from its size (`buy_v2`, `buy_v3` or `buy_exact_quote_in_v3`; `sell_v2` or
/// `sell_v3`) and `shock` from its factor (one push in two on v3). A scripted run of buys, small
/// sales and shocks must land every v3 instruction and have pump refuse none of them, so a crowd
/// reverted to v2, or v3 trades pump refuses (`buy_exact_quote_in_v3` with `min_tokens_out` 0,
/// `BuyZeroAmount`), fails here, not only in the counters of a long run.
#[test]
fn crowd_v3_trades_land() {
    let mut lines = Vec::new();
    for i in 0..9u32 {
        lines.push(format!("trade 0 0 {}", 7 + 13 * i));
    }
    for i in 0..6u32 {
        lines.push(format!("trade 0 1 {i}"));
    }
    for i in 0..4u32 {
        lines.push(format!("shock 0 0 {}", 11 + 17 * i));
        lines.push(format!("shock 0 1 {}", 11 + 17 * i));
    }
    let lines: Vec<&str> = lines.iter().map(String::as_str).collect();
    let s = curve_script("the crowd's v2 and v3 trades", &lines);
    for ix in CROWD_V3 {
        let (landed, refused) = crowd(&s, ix);
        assert!(landed > 0, "no crowd {ix} landed: {:?}", s.events);
        assert_eq!(refused, 0, "crowd {ix} refused: {:?}", s.events);
    }
    for ix in ["buy_v2", "sell_v2"] {
        assert!(crowd(&s, ix).0 > 0, "no crowd {ix} landed: {:?}", s.events);
    }
}

/// pump-final §12 (P7): the `migrating_fv` oracle on an S3 after a synthetic completion. A
/// position opens, the crowd overshoots `rT` with a `buy_v3` (`crowd_completes` with `a` even: δ
/// = 25 % of the would-be pool), PumpSwap's `create_pool` is halted, and 20.69e12 tokens are
/// donated to `curve_base_ata` after the completion (`donate` with `a = 25`). The wind-down leaves
/// the market `PumpMigrating` and settles the position S3 after the pause: its FV, both box
/// corners and the lenders' amount are §5.4's on `(X₁, Y₁)` = (the live `curve_base_ata`,
/// donation included, `rQ − fee + pcQ`) with `P_ema` ρ-scaled, recomputed host-side
/// (`migrating::check_s3`). The same run with every expected FV one lamport higher
/// (`migrating::FV_SKEW`) fails the oracle.
#[test]
fn migrating_fv_after_a_synthetic_completion_and_a_donation() {
    let lines = [
        "open 0 0 6000",
        "keeper_seconds 0 3 0",
        "crowd_completes 0 0 249",
        "graduation_halt 0 1 0",
        "donate 0 25 99",
        "keeper_seconds 0 5 0",
    ];
    let s = curve_script("S3 after a pool part", &lines);
    let ev = |k: &str| s.events.get(k).copied().unwrap_or(0);
    assert_eq!(ev("fuzz:crowd-completes-v3-overshoot-landed"), 1);
    assert_eq!(ev("donate-curve-base-ata-after-completion"), 1);
    assert!(ev("SolSettle(S3)") >= 1, "{:?}", s.events);
    assert!(ev("fuzz:migrating-fv-pool-part") >= 1, "{:?}", s.events);

    shortwin_fuzz::migrating::FV_SKEW.with(|c| c.set(1));
    let o = runner::run(
        Profile::Curve,
        SEED_BASE + 7,
        Some(&script(&lines)),
        0,
        Opts::default(),
    );
    shortwin_fuzz::migrating::FV_SKEW.with(|c| c.set(0));
    let f = o
        .failure
        .expect("a 1-lamport FV difference fails the oracle");
    assert!(f.message.contains("migrating_fv"), "{}", f.message);

    // Without a pool part (`buy_v2` of exactly `rT`) the donation is ignored: FV on 206.9e12.
    let s = curve_script(
        "S3 after an exact completion",
        &[
            "open 0 0 6000",
            "keeper_seconds 0 3 0",
            "crowd_completes 0 1 0",
            "donate 0 25 99",
            "keeper_seconds 0 5 0",
        ],
    );
    let ev = |k: &str| s.events.get(k).copied().unwrap_or(0);
    assert_eq!(ev("fuzz:crowd-completes-v2-exact-landed"), 1);
    assert!(ev("fuzz:migrating-fv-standard") >= 1, "{:?}", s.events);
}

/// pump-final §12 (P7): the `migrating_fv` oracle's second half. 103.45e12 tokens donated to
/// `curve_base_ata` before the crowd's `buy_v3` overshoot (pump prices its pool part on the live
/// balance), then `migrate_v2`: the pool it creates is `(X₁, Y₁)` — `X₁` the live
/// `curve_base_ata`, donations included, `Y₁ = rQ − fee + pcQ` — as §5.4 priced the pause
/// (`migrating::check_created_pool`); the market then graduates.
#[test]
fn migrate_v2_creates_the_priced_pool_after_a_donation() {
    let s = curve_script(
        "migrate after a donation and a pool part",
        &[
            "open 0 0 6000",
            "keeper_seconds 0 3 0",
            "donate 0 25 499",
            "crowd_completes 0 0 99",
            "keeper_seconds 0 3 0",
            "migrate 0 0 0",
            "keeper_seconds 0 20 0",
        ],
    );
    let ev = |k: &str| s.events.get(k).copied().unwrap_or(0);
    assert_eq!(ev("donate-curve-base-ata"), 1);
    assert_eq!(ev("fuzz:crowd-completes-v3-overshoot-landed"), 1);
    assert_eq!(
        ev("fuzz:migrate-pool-checked:pool-part"),
        1,
        "{:?}",
        s.events
    );
    assert_eq!(ev("fuzz:graduated"), 1);
}

/// FUZZ-M7D-1's reproducer on pump's v3 (P7): two `shock`s buy 98 % of `rT` around an open, so
/// `Q > rT`; in the wind-down the keeper's liquidation of the position completes the curve by
/// pump's synthetic migration in its own instruction (the wind-down completes no curve of its
/// own any more): the curve differential oracle checks it, the completion included.
#[test]
fn fuzz_m7d_1_closes_by_synthetic_migration() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("corpus/fuzz-m7d-1-curve-straddle-wind-down.json");
    let c = runner::read_crash(&path).expect("corpus file");
    let o = runner::run(c.profile, c.seed, Some(&c.actions), 0, c.opts);
    if let Some(f) = &o.failure {
        panic!("{}\n{}", f.signature, f.message);
    }
    let ev = |k: &str| o.stats.events.get(k).copied().unwrap_or(0);
    println!("FUZZ-M7D-1: events {:?}", o.stats.events);
    assert!(
        ev("fuzz:exit-completes-curve-synthetic") >= 1,
        "{:?}",
        o.stats.events
    );
    assert!(
        ev("liquidate:curve-differential-completes-synthetic") >= 1,
        "{:?}",
        o.stats.events
    );
}
