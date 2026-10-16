//! `shortwin-fuzz`: the invariant fuzzer's command line (README.md).
//!
//! ```text
//! shortwin-fuzz run    [--profile P|all] [--duration 1h] [--runs N] [--jobs N] [--actions N]
//!                      [--seed S] [--out DIR] [--no-differential] [--include-known]
//!                      [--minimize]
//! shortwin-fuzz replay FILE
//! shortwin-fuzz tmin   FILE [--budget N]
//! shortwin-fuzz one    --profile P --seed S [--actions N | --length N]
//! ```
//!
//! `one` runs the action list `run` draws for that seed: `--actions` is `run`'s (the length is
//! derived from the seed, [`runner::run_length`]), `--length` an exact count instead.
//!
//! Needs the program builds (`anchor build --tools-version v1.57 --arch v3` and
//! `scripts/build-test-venue.sh`, or `SHORTWIN_SO`, `SHORTWIN_TEST_VENUE_SO`, `MOCK_CPAMM_SO`).
//! Exit status 1 if any run failed.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;
use shortwin_fuzz::action::Profile;
use shortwin_fuzz::runner::{self, Outcome, STEP_SETUP, STEP_WIND_DOWN};
use shortwin_fuzz::stats::Stats;

struct Args {
    cmd: String,
    file: Option<PathBuf>,
    profiles: Vec<Profile>,
    duration: Option<u64>,
    runs: Option<u64>,
    jobs: usize,
    actions: usize,
    seed: u64,
    out: PathBuf,
    differential: bool,
    avoid_known: bool,
    minimize: bool,
    budget: usize,
    /// `one --length N`: exactly `N` actions (else `run`'s per-seed length under `--actions`).
    length: Option<usize>,
}

fn usage() -> ! {
    eprintln!(
        "usage:\n  shortwin-fuzz run [--profile P|all] [--duration 1h] [--runs N] [--jobs N] \
         [--actions N] [--seed S] [--out DIR] [--no-differential] [--include-known] [--minimize]\n  \
         shortwin-fuzz replay FILE\n  shortwin-fuzz tmin FILE [--budget N]\n  \
         shortwin-fuzz one --profile P --seed S [--actions N | --length N]\nprofiles: {}",
        Profile::ALL.map(|p| p.name()).join(", ")
    );
    std::process::exit(2)
}

fn parse() -> Args {
    let mut it = std::env::args().skip(1);
    let cmd = it.next().unwrap_or_else(|| usage());
    let mut a = Args {
        cmd,
        file: None,
        profiles: Profile::ALL.to_vec(),
        duration: None,
        runs: None,
        jobs: std::thread::available_parallelism().map_or(1, |n| n.get()),
        actions: 300,
        seed: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |d| d.as_secs()),
        out: PathBuf::from("fuzz-out"),
        differential: true,
        avoid_known: true,
        minimize: false,
        budget: 400,
        length: None,
    };
    while let Some(arg) = it.next() {
        let mut val = || it.next().unwrap_or_else(|| usage());
        match arg.as_str() {
            "--profile" => {
                let v = val();
                a.profiles = if v == "all" {
                    Profile::ALL.to_vec()
                } else {
                    v.split(',')
                        .map(|p| Profile::from_name(p).unwrap_or_else(|| usage()))
                        .collect()
                };
            }
            "--duration" => {
                a.duration = Some(runner::parse_duration(&val()).unwrap_or_else(|e| {
                    eprintln!("{e}");
                    usage()
                }))
            }
            "--runs" => a.runs = Some(val().parse().unwrap_or_else(|_| usage())),
            "--jobs" => a.jobs = val().parse().unwrap_or_else(|_| usage()),
            "--actions" => a.actions = val().parse().unwrap_or_else(|_| usage()),
            "--seed" => a.seed = val().parse().unwrap_or_else(|_| usage()),
            "--out" => a.out = PathBuf::from(val()),
            "--budget" => a.budget = val().parse().unwrap_or_else(|_| usage()),
            "--length" => a.length = Some(val().parse().unwrap_or_else(|_| usage())),
            "--no-differential" => a.differential = false,
            "--include-known" => a.avoid_known = false,
            "--minimize" => a.minimize = true,
            s if !s.starts_with("--") && a.file.is_none() => a.file = Some(PathBuf::from(s)),
            _ => usage(),
        }
    }
    a
}

fn step_name(step: usize) -> String {
    match step {
        STEP_SETUP => "setup".into(),
        STEP_WIND_DOWN => "wind-down".into(),
        s => format!("action #{s}"),
    }
}

fn print_failure(o: &Outcome) {
    let f = o.failure.as_ref().expect("failure");
    let head: String = f.message.lines().take(40).collect::<Vec<_>>().join("\n");
    eprintln!(
        "FAIL {} seed {} at {} ({} actions)\n  signature: {}\n{}\n",
        o.profile.name(),
        o.seed,
        step_name(f.step),
        o.actions.len(),
        f.signature,
        head
    );
}

fn top(map: &BTreeMap<String, u64>, n: usize) -> Vec<(String, u64)> {
    let mut v: Vec<(String, u64)> = map.iter().map(|(k, v)| (k.clone(), *v)).collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    v.truncate(n);
    v
}

fn report(s: &Stats, secs: f64, failures: &[(String, String)]) -> serde_json::Value {
    let actions = s.executed();
    println!("\n==== shortwin-fuzz summary ====");
    println!(
        "runs {}  actions {} ({:.1}/s)  skipped {}  transactions {} ({} failed)  hook checkpoints {}  wind-downs {}  wall {:.0}s",
        s.runs,
        actions,
        actions as f64 / secs.max(1.0),
        s.skipped.values().sum::<u64>(),
        s.txs,
        s.txs_failed,
        s.hook_checkpoints,
        s.wind_downs,
        secs
    );
    println!("\n-- actions by kind --");
    for (k, v) in top(&s.actions, 100) {
        let sk = s.skipped.get(&k).copied().unwrap_or(0);
        println!("  {k:<24} {v:>9}  (skipped {sk})");
    }
    println!("\n-- short.win instructions [landed, refused] --");
    for (k, v) in &s.ix {
        println!("  {k:<22} {:>9} {:>9}", v[0], v[1]);
    }
    println!("\n-- refusals (instruction:error), top 60 --");
    for (k, v) in top(&s.errors, 60) {
        println!("  {k:<60} {v:>8}");
    }
    println!("\n-- events and oracle outcomes --");
    for (k, v) in &s.events {
        println!("  {k:<48} {v:>9}");
    }
    // Coverage proxies (sBPF PC coverage is not measured): the share of short.win's
    // instructions that landed, of its events emitted, and of its error codes returned.
    let names: Vec<&str> = shortwin_fuzz::world::ERR_NAMES.to_vec();
    let codes_hit: std::collections::BTreeSet<&str> = s
        .errors
        .keys()
        .filter_map(|k| k.split(':').nth(1))
        .filter(|e| names.contains(e))
        .collect();
    let ix_landed = s.ix.values().filter(|v| v[0] > 0).count();
    let events_hit = s
        .events
        .keys()
        .filter(|k| k.chars().next().is_some_and(|c| c.is_ascii_uppercase()) && !k.contains(':'))
        .count();
    let codes_missed: Vec<&str> = names
        .iter()
        .copied()
        .filter(|n| !codes_hit.contains(n))
        .collect();
    println!(
        "\n-- coverage proxies --\n  instructions landed {}/{}  events {}+ (SolSettle by condition)  error codes returned {}/{}",
        ix_landed,
        shortwin_fuzz::stats::IX_COUNT,
        events_hit,
        codes_hit.len(),
        names.len()
    );
    println!("  codes never returned: {}", codes_missed.join(", "));
    let d = &s.deficits;
    let ev = |k: &str| s.events.get(k).copied().unwrap_or(0);
    println!(
        "\n-- the deficit tripwire and the wind-down's claims (M10) --\n  deficits checked {} in {} runs; \
         largest {} lamports (its bound {}, over the whole run {}), deficit/bound largest {:.4} \
         smallest {}\n  after the second claim pass: \
         largest unpaid pending {} lamports, largest lender_sol {} lamports\n  with absent \
         owners held behind a rebase record (KI-66): {} markets, {} lenders, {} held \
         third-party syncs; present lenders owed above dust {} times, at most {} lamports; \
         absent lenders left owing a deficit {} times\n  \
         forward chain (KI-66 full): {} started, {} walked further by a third party, owners \
         stopping inside one {} and settling one {}\n  KI-67: {} claims closed a position \
         ({} claim bundles refused after the close), {} deposits re-created one",
        d.checked,
        d.runs,
        d.largest,
        d.largest_bound,
        d.largest_total_bound,
        d.tightest_ppm as f64 / 1e6,
        d.loosest_ppm
            .map_or("-".into(), |p| format!("{:.4}", p as f64 / 1e6)),
        s.wind_down_unpaid_max,
        s.wind_down_lender_sol_max,
        s.events
            .get("wind-down-absent-market")
            .copied()
            .unwrap_or(0),
        s.events.get("wind-down-absent-owner").copied().unwrap_or(0),
        s.events.get("sync-lender-held").copied().unwrap_or(0),
        s.events
            .get("wind-down-absent-present-owed")
            .copied()
            .unwrap_or(0),
        s.wind_down_absent_owed_max,
        ev("wind-down-absent-deficit"),
        ev("pot-chain-hold"),
        ev("pot-chain-walk"),
        ev("owner-stops-in-chain"),
        ev("owner-settles-chain"),
        ev("claim-closes"),
        ev("claim-bundle-closes"),
        ev("deposit-recreates"),
    );
    println!("\n-- anomalies (not invariant violations; for review) --");
    if s.anomalies.is_empty() {
        println!("  none");
    }
    for (k, a) in &s.anomalies {
        println!("  {k}: {} × (first: {})", a.count, a.first);
    }
    println!("\n-- failures --");
    if failures.is_empty() {
        println!("  none");
    }
    for (sig, file) in failures {
        println!("  {sig}\n    {file}");
    }
    json!({
        "runs": s.runs,
        "actions": actions,
        "skipped": s.skipped.values().sum::<u64>(),
        "transactions": s.txs,
        "transactions_failed": s.txs_failed,
        "hook_checkpoints": s.hook_checkpoints,
        "wind_downs": s.wind_downs,
        "wall_seconds": secs,
        "actions_by_kind": s.actions,
        "instructions": s.ix.iter().map(|(k, v)| (k.clone(), json!({"landed": v[0], "refused": v[1]}))).collect::<serde_json::Map<_, _>>(),
        "refusals": s.errors,
        "events": s.events,
        "anomalies": s.anomalies.iter().map(|(k, a)| (k.clone(), json!({"count": a.count, "first": a.first, "detail": a.detail}))).collect::<serde_json::Map<_, _>>(),
        "failures": failures.iter().map(|(s, f)| json!({"signature": s, "file": f})).collect::<Vec<_>>(),
        "wind_down_deficits": {
            "checked": d.checked,
            "runs_with_deficit": d.runs,
            "largest_lamports": d.largest,
            "largest_bound_lamports": d.largest_bound.to_string(),
            "largest_cumulative_bound_lamports": d.largest_total_bound.to_string(),
            "largest_share_of_bound_ppm": d.tightest_ppm,
            "smallest_share_of_bound_ppm": d.loosest_ppm,
            "dust_lamports": shortwin_fuzz::shortfall::DEFICIT_DUST,
        },
        "wind_down_claims": {
            "largest_unpaid_pending_lamports": s.wind_down_unpaid_max,
            "largest_lender_sol_lamports": s.wind_down_lender_sol_max,
            "largest_absent_owed_lamports": s.wind_down_absent_owed_max,
            "absent_deficit_markets": s.events.get("wind-down-absent-deficit").copied().unwrap_or(0),
            "claim_dust_lamports": shortwin_fuzz::world::CLAIM_DUST,
        },
        "coverage": {
            "instructions_landed": ix_landed,
            "instructions_total": shortwin_fuzz::stats::IX_COUNT,
            "error_codes_returned": codes_hit.len(),
            "error_codes_total": names.len(),
            "error_codes_never_returned": codes_missed,
        },
    })
}

/// Crash files written per signature (later occurrences are only counted).
const CRASH_FILES_PER_SIGNATURE: u64 = 5;

fn cmd_run(a: &Args) -> i32 {
    let opts = runner::Opts {
        differential: a.differential,
        avoid_known: a.avoid_known,
    };
    // Preflight: every selected profile sets up (the program builds, the fixtures).
    for p in &a.profiles {
        let o = runner::run(*p, a.seed, Some(&[]), 0, opts);
        if let Some(f) = o.failure.as_ref().filter(|f| f.step == STEP_SETUP) {
            eprintln!(
                "preflight: profile {} does not set up:\n{}",
                p.name(),
                f.message.lines().take(20).collect::<Vec<_>>().join("\n")
            );
            return 2;
        }
    }
    let t0 = Instant::now();
    let deadline = a.duration.map(|d| t0 + Duration::from_secs(d));
    let runs_cap = a
        .runs
        .unwrap_or(if deadline.is_none() { 1 } else { u64::MAX });
    let total = Arc::new(Mutex::new(Stats::default()));
    let failures: Arc<Mutex<Vec<(String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let seen: Arc<Mutex<BTreeMap<String, u64>>> = Arc::new(Mutex::new(BTreeMap::new()));
    let started = Arc::new(AtomicU64::new(0));
    let jobs = a.jobs.max(1);
    let live = Arc::new(AtomicUsize::new(jobs));
    std::thread::scope(|scope| {
        for job in 0..jobs {
            let total = Arc::clone(&total);
            let failures = Arc::clone(&failures);
            let started = Arc::clone(&started);
            let live = Arc::clone(&live);
            let seen = Arc::clone(&seen);
            let profiles = a.profiles.clone();
            let out = a.out.clone();
            let (actions, minimize, budget, base) = (a.actions, a.minimize, a.budget, a.seed);
            std::thread::Builder::new()
                .name(format!("fuzz-{job}"))
                .stack_size(64 << 20)
                .spawn_scoped(scope, move || {
                    let mut k = 0u64;
                    loop {
                        if deadline.is_some_and(|d| Instant::now() >= d) {
                            break;
                        }
                        let n = started.fetch_add(1, Ordering::Relaxed);
                        if n >= runs_cap {
                            break;
                        }
                        // One seed stream per job.
                        let seed = base
                            .wrapping_add(job as u64)
                            .wrapping_add(k.wrapping_mul(jobs as u64));
                        k += 1;
                        let profile = profiles[(shortwin_fuzz::rng::splitmix64(seed)
                            % profiles.len() as u64)
                            as usize];
                        // Run lengths vary (¼ to all of `--actions`), so the wind-down's
                        // liveness check meets states at every depth; `one` derives the same.
                        let n = runner::run_length(seed, actions);
                        let o = runner::run(profile, seed, None, n, opts);
                        total.lock().expect("stats").merge(&o.stats);
                        if let Some(fail) = &o.failure {
                            let sig = fail.signature.clone();
                            let count = {
                                let mut m = seen.lock().expect("seen");
                                let c = m.entry(sig.clone()).or_default();
                                *c += 1;
                                *c
                            };
                            if count > CRASH_FILES_PER_SIGNATURE {
                                continue;
                            }
                            print_failure(&o);
                            let mut path = runner::write_crash(&out, &o, false);
                            // Minimise the first failure of each signature only.
                            if minimize && count == 1 {
                                let c = runner::read_crash(&path).expect("crash file");
                                let m = runner::tmin(&c, budget, false);
                                path = runner::write_crash(&out, &m, true);
                                eprintln!(
                                    "minimised {} -> {} actions: {}",
                                    o.actions.len(),
                                    m.actions.len(),
                                    path.display()
                                );
                            }
                            failures
                                .lock()
                                .expect("failures")
                                .push((sig, path.display().to_string()));
                        }
                    }
                    live.fetch_sub(1, Ordering::Relaxed);
                })
                .expect("spawn");
        }
        // Progress every minute, until every worker has finished.
        let total = Arc::clone(&total);
        let failures = Arc::clone(&failures);
        let live = Arc::clone(&live);
        scope.spawn(move || {
            let mut last = Instant::now();
            while live.load(Ordering::Relaxed) > 0 {
                std::thread::sleep(Duration::from_millis(250));
                if last.elapsed() >= Duration::from_secs(60) {
                    last = Instant::now();
                    let s = total.lock().expect("stats").clone();
                    eprintln!(
                        "[{:>6.0}s] runs {} actions {} txs {} failures {} anomalies {}",
                        t0.elapsed().as_secs_f64(),
                        s.runs,
                        s.executed(),
                        s.txs,
                        failures.lock().expect("f").len(),
                        s.anomalies.values().map(|a| a.count).sum::<u64>()
                    );
                }
            }
        });
    });
    let s = total.lock().expect("stats").clone();
    let f = failures.lock().expect("f").clone();
    let counts = seen.lock().expect("seen").clone();
    let mut summary = report(&s, t0.elapsed().as_secs_f64(), &f);
    summary["failure_counts"] = json!(counts);
    for (sig, n) in &counts {
        println!("  {n} × {sig}");
    }
    std::fs::create_dir_all(&a.out).ok();
    let path = a.out.join("summary.json");
    std::fs::write(&path, serde_json::to_string_pretty(&summary).expect("json")).ok();
    println!("\nsummary: {}", path.display());
    i32::from(!f.is_empty())
}

fn cmd_replay(a: &Args) -> i32 {
    let path = a.file.clone().unwrap_or_else(|| usage());
    let c = runner::read_crash(&path).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(2)
    });
    let o = runner::run(c.profile, c.seed, Some(&c.actions), 0, c.opts);
    match &o.failure {
        Some(f) => {
            print_failure(&o);
            let same = f.signature == c.signature;
            println!(
                "replay: FAILS at {} — {} signature",
                step_name(f.step),
                if same { "same" } else { "a different" }
            );
            1
        }
        None => {
            println!(
                "replay: passes ({} actions, {} transactions)",
                o.stats.executed(),
                o.stats.txs
            );
            0
        }
    }
}

fn cmd_tmin(a: &Args) -> i32 {
    let path = a.file.clone().unwrap_or_else(|| usage());
    let c = runner::read_crash(&path).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(2)
    });
    let before = c.actions.len();
    let m = runner::tmin(&c, a.budget, true);
    let out = path.parent().map_or_else(|| a.out.clone(), PathBuf::from);
    let p = runner::write_crash(&out, &m, true);
    println!(
        "tmin: {before} -> {} actions: {}",
        m.actions.len(),
        p.display()
    );
    for act in &m.actions {
        println!("  {act}");
    }
    0
}

fn cmd_one(a: &Args) -> i32 {
    let profile = *a.profiles.first().unwrap_or_else(|| usage());
    let opts = runner::Opts {
        differential: a.differential,
        avoid_known: a.avoid_known,
    };
    // The length `run --actions N` draws for this seed (an anomaly's or a crash's cursor names it
    // as `#i/n`), unless `--length` gives it.
    let n = a
        .length
        .unwrap_or_else(|| runner::run_length(a.seed, a.actions));
    println!(
        "one: {} seed {} — {n} actions{}",
        profile.name(),
        a.seed,
        if a.length.is_some() {
            String::new()
        } else {
            format!(" (`run --actions {}`'s length for this seed)", a.actions)
        }
    );
    let o = runner::run(profile, a.seed, None, n, opts);
    let f: Vec<(String, String)> = if let Some(fail) = &o.failure {
        print_failure(&o);
        let p = runner::write_crash(&a.out, &o, false);
        vec![(fail.signature.clone(), p.display().to_string())]
    } else {
        Vec::new()
    };
    let summary = report(&o.stats, o.millis as f64 / 1000.0, &f);
    std::fs::create_dir_all(&a.out).ok();
    let path = a.out.join("summary.json");
    std::fs::write(&path, serde_json::to_string_pretty(&summary).expect("json")).ok();
    i32::from(!f.is_empty())
}

fn main() {
    let a = parse();
    let code = match a.cmd.as_str() {
        "run" => cmd_run(&a),
        "replay" => cmd_replay(&a),
        "tmin" => cmd_tmin(&a),
        "one" => cmd_one(&a),
        _ => usage(),
    };
    std::process::exit(code);
}
