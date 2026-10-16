//! `pool` against `pool_model.py` v0.4.2 (ARCH §8.1, §9 M0b): replays every trace of
//! `tests/vectors/pool/*.json` (written by `gen_pool.py`) and requires the state after
//! every line to equal the model's (running CRC-32 of the canonical state and results),
//! every explicit result and error, the checkpoints, the liveness wind-downs (I-P6) and
//! the final state, with no tolerance. The ECON §13 I-P1–I-P7 invariants (I-P4 also on the
//! untouched state) are asserted after every line (ARCH §8.3 I8), and must fail exactly
//! where `pool_model.check()` fails. The two option-(a) ECON §9.6 fuzz runs are replayed
//! whole (60,000 operations each), the lock-heavy mix of ECON §9.3, the 40-seed stranding
//! table of R3A-2, the extended profiles (KI-65's lazy conversion orders and KI-66's pot-only
//! touches among them) and every default-switch demo.

mod common;
#[path = "pool_harness.rs"]
mod pool_harness;

use common::Dec;
use pool_harness::{Market, crc32, diff_states};
use serde::Deserialize;

#[derive(Deserialize)]
struct File {
    generator: String,
    traces: Vec<Trace>,
}

#[derive(Deserialize)]
struct Trace {
    name: String,
    check_each_line: bool,
    users: Vec<String>,
    lump_vest_s: Dec<u32>,
    lock_grace_s: Dec<u32>,
    lines: Vec<String>,
    checkpoints: Vec<Checkpoint>,
    final_state: String,
    epochs_rolled: Dec<u32>,
    /// Lines after which `pool_model.Pool.check()` fails (traces without
    /// `check_each_line`); every other line must pass.
    #[serde(default)]
    model_check_fails: Vec<Dec<usize>>,
    /// The model's liveness wind-down (I-P6) after the given lines, on a copy.
    #[serde(default)]
    liveness: Vec<Liveness>,
    /// Split traces: part `k` of `parts`, starting at absolute line `first_line` of one
    /// CRC chain; checkpoint and liveness lines are absolute.
    #[serde(default)]
    part: Option<Dec<usize>>,
    #[serde(default)]
    parts: Option<Dec<usize>>,
    #[serde(default)]
    first_line: Option<Dec<usize>>,
    #[serde(default)]
    op_counts: std::collections::BTreeMap<String, Dec<u64>>,
    /// lock_fuzz traces: untouched-state I-P4 samples and how many were above T + dust.
    #[serde(default)]
    samples: Option<Dec<usize>>,
    #[serde(default)]
    above: Option<Dec<usize>>,
    /// Rent top-up profiles (M10-8C-1): exits whose top-up was positive.
    #[serde(default)]
    top_ups: Option<Dec<u64>>,
    /// Pot-only touch profiles (KI-66): the `h` (and `y U N 0`) operations that held at a
    /// rebase record or continued a forward chain.
    #[serde(default)]
    pot_holds: Option<Dec<u64>>,
    /// Forward-chain profiles (KI-66 full): `FWD_OPEN` lender states after a line, of which
    /// with lag 1, of which at `fwd_epoch < share_epoch` (`"open,lag1,at_fwd"`).
    #[serde(default)]
    fwd_open: Option<String>,
}

#[derive(Deserialize)]
struct Checkpoint {
    line: Dec<usize>,
    state: String,
}

#[derive(Deserialize)]
struct Liveness {
    line: Dec<usize>,
    ops: String,
    state: String,
    paid: Dec<u64>,
    stranded: Dec<u64>,
    bound: Dec<u128>,
    unpaid: Dec<u128>,
}

/// What one replay saw.
#[derive(Default)]
struct Stats {
    lines: usize,
    ops: usize,
    refused: usize,
    /// Untouched-state I-P4 samples (a `k` followed by `A` for every lender): count and
    /// how many summed above T + n.
    samples: usize,
    above: usize,
    liveness: usize,
    final_lender_sol: u64,
}

/// Splits `op args… [=VALUE|!CODE]`; returns (op, args, expected).
fn parse_op(op: &str) -> (&str, Vec<&str>, Option<&str>) {
    let mut words: Vec<&str> = op.split(' ').collect();
    let expected = match words.last() {
        Some(w) if w.starts_with('=') || w.starts_with('!') => words.pop(),
        _ => None,
    };
    (words[0], words[1..].to_vec(), expected)
}

fn run_liveness(t: &Trace, m: &Market, b: &Liveness) {
    let mut w = m.clone();
    let mut paid = 0u64;
    for op in b.ops.split(';').filter(|s| !s.is_empty()) {
        let (name, args, expected) = parse_op(op);
        let got = w.apply(name, &args);
        if let Some(e) = expected {
            assert_eq!(
                got, e,
                "{}: liveness after line {} `{op}`",
                t.name, b.line.0
            );
        }
        if name == "c" {
            paid += got[1..].parse::<u64>().expect("claim result");
        }
    }
    let state = w.canonical();
    assert!(
        state == b.state,
        "{}: liveness wind-down after line {}\n{}",
        t.name,
        b.line.0,
        diff_states(&b.state, &state)
    );
    assert_eq!(paid, b.paid.0, "{}: liveness paid", t.name);
    assert_eq!(w.vault.lender_sol, b.stranded.0, "{}: stranded", t.name);
    // I-P6: sequential claims leave at most dust (the model's bound), unless every positive
    // entitlement was paid.
    assert!(
        u128::from(b.stranded.0) <= b.bound.0 || b.unpaid.0 == 0,
        "{}: I-P6 stranded {} > bound {}",
        t.name,
        b.stranded.0,
        b.bound.0
    );
}

/// Replays one whole trace.
fn replay(t: &Trace) -> Stats {
    assert!(
        t.first_line.as_ref().is_none_or(|d| d.0 == 0),
        "{}: split traces are merged first",
        t.name
    );
    let mut m = Market::with_params(t.lump_vest_s.0, t.lock_grace_s.0);
    let mut crc = 0u32;
    let mut cps = t.checkpoints.iter().peekable();
    let mut live = t.liveness.iter().peekable();
    let mut st = Stats::default();
    for (i, line) in t.lines.iter().enumerate() {
        let (body, digest) = match line.rsplit_once('#') {
            Some((b, d)) => (b.trim_end(), d),
            None => panic!("{}: line {i} has no digest: {line}", t.name),
        };
        let mut results = Vec::new();
        let mut sample: Option<u128> = None;
        for op in body.split(';').filter(|s| !s.is_empty()) {
            let (name, args, expected) = parse_op(op);
            let got = m.apply(name, &args);
            if let Some(e) = expected {
                assert_eq!(
                    got,
                    e,
                    "{}: line {i} `{op}`\nstate {}",
                    t.name,
                    m.canonical()
                );
            }
            match name {
                "k" => sample = Some(0),
                "A" => {
                    if let Some(s) = sample.as_mut() {
                        *s += got[1..].parse::<u128>().expect("allowance");
                    }
                }
                _ => {}
            }
            st.refused += usize::from(got.starts_with('!'));
            st.ops += 1;
            results.push(got);
        }
        if let Some(sum) = sample {
            st.samples += 1;
            let bound = u128::from(m.pool.tokens_free) + m.lenders.len() as u128;
            st.above += usize::from(sum > bound);
        }
        let check = if t.check_each_line {
            m.check_touch_all()
        } else {
            m.invariants()
        };
        let model_fails = t.model_check_fails.iter().any(|d| d.0 == i);
        match (check, model_fails) {
            (Ok(()), false) | (Err(_), true) => {}
            (Err(e), false) => panic!(
                "{}: invariant after line {i} `{line}`: {e}\nstate {}",
                t.name,
                m.canonical()
            ),
            (Ok(()), true) => panic!(
                "{}: pool_model.check() fails after line {i}, the Rust invariants pass",
                t.name
            ),
        }
        let state = m.canonical();
        crc = crc32(crc, format!("{}|{state}\n", results.join(",")).as_bytes());
        if format!("{crc:08x}") != digest {
            let near = t
                .checkpoints
                .iter()
                .find(|c| c.line.0 >= i)
                .map_or("", |c| c.state.as_str());
            panic!(
                "{}: state after line {i} `{line}` differs from pool_model (results {results:?})\n\
                 rust  {state}\n\
                 (run `python3 gen_pool.py` with a breakpoint, or compare with the next checkpoint:)\n{}",
                t.name,
                diff_states(near, &state)
            );
        }
        if let Some(cp) = cps.next_if(|c| c.line.0 == i) {
            assert!(
                cp.state == state,
                "{}: checkpoint {i}\n{}",
                t.name,
                diff_states(&cp.state, &state)
            );
        }
        while let Some(b) = live.next_if(|b| b.line.0 == i) {
            run_liveness(t, &m, b);
            st.liveness += 1;
        }
    }
    assert!(cps.next().is_none(), "{}: unused checkpoints", t.name);
    assert!(live.next().is_none(), "{}: unused liveness blocks", t.name);
    assert!(
        m.canonical() == t.final_state,
        "{}: final state\n{}",
        t.name,
        diff_states(&t.final_state, &m.canonical())
    );
    assert_eq!(
        m.pool.share_epoch, t.epochs_rolled.0,
        "{}: epochs rolled",
        t.name
    );
    assert!(m.lenders.len() <= t.users.len(), "{}: lenders", t.name);
    if let (Some(n), Some(a)) = (&t.samples, &t.above) {
        assert_eq!(
            (st.samples, st.above),
            (n.0, a.0),
            "{}: I-P4 samples",
            t.name
        );
    }
    st.lines = t.lines.len();
    st.final_lender_sol = m.vault.lender_sol;
    st
}

fn load(file: &str) -> File {
    let f: File = common::load_vectors("pool", file);
    assert_eq!(f.generator, "gen_pool.py");
    f
}

/// Every `<stem>_<k>.json`, k = 1, 2, … until one is missing.
fn load_all(stem: &str) -> Vec<Trace> {
    let mut out = Vec::new();
    for k in 1.. {
        let name = format!("{stem}_{k}.json");
        if !common::vectors_dir().join("pool").join(&name).exists() {
            assert!(k > 1, "{stem}: no files");
            break;
        }
        out.extend(load(&name).traces);
    }
    out
}

fn replay_group(stem: &str, min_lines: usize) -> Vec<(String, Stats)> {
    let mut lines = 0;
    let mut out = Vec::new();
    for t in load_all(stem) {
        let st = replay(&t);
        println!(
            "{stem} {}: {} lines, {} pool operations, {} refused",
            t.name, st.lines, st.ops, st.refused
        );
        lines += st.lines;
        out.push((t.name, st));
    }
    assert!(lines >= min_lines, "{stem}: {lines} lines");
    out
}

/// Replays one ECON §9.6 fuzz run, `pool_fuzz_<stem>_<k>.json` for k = 1..=parts, as a
/// single trace: every part continues the previous part's state and CRC chain.
fn replay_fuzz_run(stem: &str, epochs: u32) {
    let mut merged: Option<Trace> = None;
    let mut k = 1;
    loop {
        let f = load(&format!("pool_fuzz_{stem}_{k}.json"));
        let [t]: [Trace; 1] = f.traces.try_into().unwrap_or_else(|_| panic!("{stem}_{k}"));
        assert_eq!(t.part.as_ref().map(|d| d.0), Some(k), "{stem}_{k}");
        let parts = t.parts.as_ref().map_or(0, |d| d.0);
        let first = t.first_line.as_ref().map_or(usize::MAX, |d| d.0);
        merged = Some(match merged {
            None => {
                assert_eq!(first, 0, "{stem}_{k}");
                t
            }
            Some(mut m) => {
                assert_eq!(first, m.lines.len(), "{stem}_{k}: not contiguous");
                m.lines.extend(t.lines);
                m.checkpoints.extend(t.checkpoints);
                m.liveness.extend(t.liveness);
                Trace {
                    final_state: t.final_state,
                    epochs_rolled: t.epochs_rolled,
                    op_counts: t.op_counts,
                    ..m
                }
            }
        });
        if k == parts {
            break;
        }
        k += 1;
    }
    let t = merged.unwrap_or_else(|| panic!("{stem}: no parts"));
    let st = replay(&t);
    println!(
        "{stem}: {} lines, {} pool operations, {} refused, {} liveness wind-downs",
        st.lines, st.ops, st.refused, st.liveness
    );
    // ARCH §8.1: the whole ECON §9.6 run (60,000 ops), liveness every 2,000 and at the end.
    assert_eq!(st.lines, 60_000, "{stem}");
    assert_eq!(st.liveness, 31, "{stem}: liveness wind-downs");
    let total: u64 = t.op_counts.values().map(|d| d.0).sum();
    assert_eq!(total, 60_000, "{stem}: op counts");
    assert_eq!(t.epochs_rolled.0, epochs, "{stem}: ECON §9.6 epochs rolled");
}

// Since AIR-07's residual fix a deposit into a drained pool is refused (26 of the 90 % run's
// deposits, 234 of the 100 % run's), which moves the epochs rolled from 1,020 and 1,930.
#[test]
fn pool_model_fuzz_continuous_90() {
    replay_fuzz_run("continuous_90", 1_021);
}

#[test]
fn pool_model_fuzz_continuous_100() {
    replay_fuzz_run("continuous_100", 1_941);
}

#[test]
fn lock_heavy_mix_untouched_ip4() {
    // ECON §9.3 (R4A-2 / R4B-3): 954 sampled states, none above T + dust (v0.4.1 row).
    let runs = replay_group("pool_lock_fuzz", 24_000);
    assert_eq!(runs.len(), 6);
    let samples: usize = runs.iter().map(|(_, s)| s.samples).sum();
    let above: usize = runs.iter().map(|(_, s)| s.above).sum();
    assert_eq!((samples, above), (954, 0));
}

#[test]
fn stranded_sol_after_an_honest_wind_down() {
    // ECON §9.4 / R3A-2 (reviewer's r3_stranded.py), v0.4 row: 40 seeds × 3,000 operations;
    // stranded 0.000015 SOL in total (15,044 lamports), worst seed 435 lamports, no seed above
    // 0.01 SOL. AIR-03's carried rent-credit remainder took it from 19,887 / 582 lamports
    // (0.000020 / 0.000001 SOL): an accrual's floored fraction no longer stays in `lender_sol`.
    // AIR-07's drained-pool gate, which refuses some of the table's deposits, moved it from
    // 15,058 / 440.
    let runs = replay_group("pool_stranded", 120_000);
    assert_eq!(runs.len(), 40);
    let stranded: Vec<u64> = runs.iter().map(|(_, s)| s.final_lender_sol).collect();
    let total: u64 = stranded.iter().sum();
    let worst = *stranded.iter().max().expect("seeds");
    assert_eq!((total, worst), (15_044, 435));
    assert_eq!(format!("{:.6}", total as f64 / 1e9), "0.000015");
    assert_eq!(stranded.iter().filter(|&&s| s > 10_000_000).count(), 0);
}

#[test]
fn ext_lazy_lenders() {
    replay_group("pool_ext_lazy", 14_000);
}

#[test]
fn ext_runs_and_churn() {
    replay_group("pool_ext_runs", 9_000);
}

#[test]
fn ext_dust_amounts() {
    replay_group("pool_ext_dust", 5_000);
}

/// The u64-extreme profiles and the scripted traces: ARCH §9 M0b's model notes (1), (2) and
/// (4), and M0B-A-1 (review): a lender whose shares and lock floored to 0 in a rebase while
/// its lock term did not reads the next roll's record, which scales the term (to 0), before
/// the V2-12 jump — the crate used to jump without the record and kept the term at 1.
#[test]
fn ext_u64_extreme_amounts_and_scripted_notes() {
    let runs = replay_group("pool_ext_extreme", 7_000);
    for name in [
        "ext_scripted_lock_terms",
        "ext_scripted_no_shares",
        "ext_scripted_overlock",
        "ext_scripted_empty_lock_term",
    ] {
        assert!(runs.iter().any(|(n, _)| n == name), "{name}");
    }
}

/// Finding M10-8C-1 (review of WP-6 batch 2): exits that pay rent above what accrued — a
/// repay of the accrued rent plus an `R_min`-like excess, a settlement whose `R_due` exceeds
/// it — so `pool::repay` / `pool::settle` vest an `R_min`-sized rent top-up, not only the
/// rounding-sized one of the other traces; and the scripted JIT bundles around an early
/// repay and an early settlement (the model asserts the whale realises at most rounding
/// beyond a settlement's token value; the replay pins every claim).
#[test]
fn ext_rent_top_up_vests() {
    let runs = replay_group("pool_ext_topup", 16_000);
    let traces = load_all("pool_ext_topup");
    let profiles: Vec<u64> = traces
        .iter()
        .filter_map(|t| t.top_ups.as_ref().map(|d| d.0))
        .collect();
    assert_eq!(profiles.len(), 5, "five profiles");
    assert!(profiles.iter().all(|&n| n >= 50), "{profiles:?}");
    assert!(runs.iter().any(|(n, _)| n == "ext_topup_scripted_jit"));
}

/// KI-65 (v0.4.2, no rebase orphans): lazy lenders converting in random orders through
/// forced deep rebases, dust residues below a rebase's unit, whole exits while lumps vest —
/// the canonical state pins the latest record's `conv_rem` and the touch-all invariants
/// after every line require `Σ shares = S` on the converted copy (I-P1); and the v0.4.2
/// pools of `orphan_vest_demo` (the last exit takes `S` to 0 and is paid the vest) and
/// `whale_order_demo` (R4B-4 W1–W3), every claim pinned.
#[test]
fn ext_orphans_lazy_conversion_order() {
    let runs = replay_group("pool_ext_orphans", 8_000);
    for name in [
        "ext_orphans_continuous_100",
        "ext_orphans_dust_100",
        "ext_orphans_whale_order_demo_0",
        "ext_orphans_orphan_vest_demo_0",
    ] {
        assert!(runs.iter().any(|(n, _)| n == name), "{name}");
    }
    // Every lender left during the vest: nothing is stranded beyond rounding.
    for (n, st) in &runs {
        if n.starts_with("ext_orphans_orphan_vest_demo") {
            assert!(st.final_lender_sol <= 3, "{n}: {}", st.final_lender_sol);
        }
    }
}

/// KI-66, the pot-only touch (`h`, `pool::touch_pot`, a third party's `sync_lender`): lazy
/// lenders around deep rebases are touched by third parties (some twice) and claim; a lender
/// behind a rebase record is accrued to the record's ends and, KI-66 full, along its forward
/// chain to the current epoch, and moves its surplus to the pot without converting (the
/// canonical state pins its shares, epoch, snapshots and chain, the record's `conv_rem` and the
/// pot), and converts later at its own touch; windowed syncs (`y`) walk the chain a few records
/// at a time and the owners' partial syncs stop inside it (`FWD_OPEN`, lag 0 or 1, at an epoch
/// before `fwd_epoch` and at `fwd_epoch` before the current one); plus `pot_touch_demo` and
/// `pot_chain_demo` in `pool_scenarios` (T-POT's late joiner behind a rebase record, and a
/// rebase later: A paid in full without C).
#[test]
fn ext_pot_only_touch() {
    let runs = replay_group("pool_ext_pot", 10_000);
    let traces = load_all("pool_ext_pot");
    let holds: Vec<u64> = traces
        .iter()
        .filter_map(|t| t.pot_holds.as_ref().map(|d| d.0))
        .collect();
    assert_eq!(holds.len(), 7, "seven profiles");
    assert!(holds.iter().all(|&n| n >= 10), "{holds:?}");
    assert!(
        traces
            .iter()
            .all(|t| t.lines.iter().any(|l| l.starts_with("h ")
                || l.contains(";h ")
                || l.contains(" 0 #")
                || l.contains("y "))),
        "every profile sends pot-only touches"
    );
    let open: Vec<Vec<u64>> = traces
        .iter()
        .filter_map(|t| t.fwd_open.as_ref())
        .map(|f| f.split(',').map(|x| x.parse().expect("count")).collect())
        .collect();
    assert_eq!(open.len(), 4, "four forward-chain profiles");
    let tuned = open
        .iter()
        .max_by_key(|c| c[0])
        .expect("ext_pot_fwd_open_100");
    assert!(
        tuned[0] >= 200 && tuned[1] >= 50 && tuned[2] >= 20,
        "{open:?}"
    );
    assert_eq!(runs.len(), 7);
}

#[test]
fn econ_demos_round_3_and_round_4_cases() {
    let runs = replay_group("pool_scenarios", 5_000);
    for demo in [
        "jit_demo",
        "jit_hold_table",
        "inflation_demo",
        "run_demo",
        "claim_loop_demo",
        "run_util_table",
        "wipe_demo",
        "wipe_tail_demo",
        "zero_share_demo",
        "protocol_change_demo",
        "escape_demo",
        "stuck_lock_table",
        "stale_lock_demo",
        "lock_tail_demo",
        "wipe_gate_demo",
        "vest_exit_demo",
        "restart_grief_table",
        "premium_jit_table",
        "pot_touch_demo",
        "pot_chain_demo",
        "rebase_vest_demo",
        "empty_pool_gate_demo",
        "lock_era_demo",
        "pool_edge",
    ] {
        assert!(
            runs.iter().any(|(n, _)| n.starts_with(demo)),
            "{demo} missing"
        );
    }
}
