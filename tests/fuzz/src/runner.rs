//! Running, replaying and minimising action sequences.
//!
//! A **run** is a profile, a seed and an action list (drawn from the seed, or given): the world
//! is set up, every action applied, then the wind-down (the liveness oracle). A panic anywhere —
//! an invariant hook in `Env::send`, a fuzzer oracle ([`crate::world::ORACLE`]), the mock's
//! differential exit oracle — ends the run as a [`Failure`], caught per action so the counters
//! survive. A failure's **signature** (panic location and first message line, digits masked)
//! is what `tmin` keeps constant while it deletes actions (delta debugging).

use std::cell::RefCell;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::Once;
use std::time::Instant;

use serde_json::{Value, json};

use crate::action::{Action, Profile, stream};
use crate::setup;
use crate::stats::Stats;

thread_local! {
    static LAST_PANIC: RefCell<Option<String>> = const { RefCell::new(None) };
}

static HOOK: Once = Once::new();

/// Installs a panic hook that records the panic (location and message) for the runner instead
/// of printing it (`FUZZ_VERBOSE=1` prints too).
pub fn install_panic_hook() {
    HOOK.call_once(|| {
        let verbose = std::env::var_os("FUZZ_VERBOSE").is_some();
        let default = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            let loc = info
                .location()
                .map(|l| format!("{}:{}", l.file(), l.line()))
                .unwrap_or_default();
            let msg = if let Some(s) = info.payload().downcast_ref::<&str>() {
                (*s).to_string()
            } else if let Some(s) = info.payload().downcast_ref::<String>() {
                s.clone()
            } else {
                "<non-string panic>".to_string()
            };
            LAST_PANIC.with(|l| *l.borrow_mut() = Some(format!("{loc}\n{msg}")));
            if verbose {
                default(info);
            }
        }));
    });
}

fn take_panic() -> String {
    LAST_PANIC
        .with(|l| l.borrow_mut().take())
        .unwrap_or_else(|| "<panic without message>".into())
}

/// How a run interprets its actions (stored in crash files, so a replay and `tmin` run the
/// actions exactly as the original run did).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Opts {
    /// Keeper exits on an undisturbed mock market go through the differential oracle.
    pub differential: bool,
    /// Actions that would only re-trigger a known finding (README "Known findings") are skipped.
    pub avoid_known: bool,
}

impl Default for Opts {
    fn default() -> Self {
        Self {
            differential: true,
            avoid_known: true,
        }
    }
}

/// Where a failure happened.
pub const STEP_SETUP: usize = usize::MAX - 1;
pub const STEP_WIND_DOWN: usize = usize::MAX;

/// A failed run.
#[derive(Clone, Debug)]
pub struct Failure {
    /// The index of the failing action ([`STEP_SETUP`], [`STEP_WIND_DOWN`]).
    pub step: usize,
    /// `location\nmessage` (the message includes the transaction's logs for hook violations).
    pub message: String,
    pub signature: String,
}

/// A run's result.
pub struct Outcome {
    pub profile: Profile,
    pub seed: u64,
    pub opts: Opts,
    /// The actions of the run (for a failure: up to and including the failing one).
    pub actions: Vec<Action>,
    pub stats: Stats,
    pub failure: Option<Failure>,
    pub millis: u128,
}

/// The signature of a panic: its location and its message's first informative line with every
/// base58 key masked, then every run of digits (keys and amounts change between runs; the
/// check does not).
pub fn signature(message: &str) -> String {
    let mut lines = message.lines();
    let loc = lines.next().unwrap_or_default();
    let line: String = lines
        .find(|l| !l.trim().is_empty())
        .unwrap_or_default()
        .chars()
        .take(240)
        .collect();
    // Keys first (32–44 base58 characters), before the digit masking can shorten them.
    let is_b58 = |c: char| c.is_ascii_alphanumeric() && !matches!(c, '0' | 'O' | 'I' | 'l');
    let mut keyless = String::new();
    let mut word = String::new();
    let flush = |word: &mut String, out: &mut String| {
        if (32..=44).contains(&word.len()) {
            out.push_str("<key>");
        } else {
            out.push_str(word);
        }
        word.clear();
    };
    for c in line.chars() {
        if is_b58(c) {
            word.push(c);
        } else {
            flush(&mut word, &mut keyless);
            keyless.push(c);
        }
    }
    flush(&mut word, &mut keyless);
    // Every run of digits becomes one `#` (amounts vary in length between runs).
    let mut first = String::new();
    for c in keyless.chars() {
        if c.is_ascii_digit() {
            if !first.ends_with('#') {
                first.push('#');
            }
        } else {
            first.push(c);
        }
    }
    format!("{loc} | {first}")
}

/// The number of actions `run` draws for `seed` under `--actions max`: from a quarter of `max`
/// to all of it, `max/4 + splitmix64(seed ^ 0x1e57) % (3·max/4 + 1)` (so the wind-down's liveness
/// check meets states at every depth). `one` derives its length the same way, so `one --profile P
/// --seed S --actions M` replays exactly the run `run --actions M` made of that seed (M10; before,
/// `one` ran `M` actions and a reproducer had to recompute the length, FUZZ-M7-24H-3).
pub fn run_length(seed: u64, max: usize) -> usize {
    max / 4 + (crate::rng::splitmix64(seed ^ 0x1e57) % (max as u64 * 3 / 4 + 1)) as usize
}

/// Runs `profile` / `seed`: the given `actions`, or `n` drawn from the seed.
pub fn run(
    profile: Profile,
    seed: u64,
    actions: Option<&[Action]>,
    n: usize,
    opts: Opts,
) -> Outcome {
    install_panic_hook();
    let t0 = Instant::now();
    let list: Vec<Action> = match actions {
        Some(a) => a.to_vec(),
        None => stream(profile, seed, n),
    };
    let world = panic::catch_unwind(AssertUnwindSafe(|| setup::world(profile, seed, opts)));
    let mut world = match world {
        Ok(w) => w,
        Err(_) => {
            let message = take_panic();
            return Outcome {
                profile,
                seed,
                opts,
                actions: Vec::new(),
                stats: Stats::default(),
                failure: Some(Failure {
                    step: STEP_SETUP,
                    signature: signature(&message),
                    message,
                }),
                millis: t0.elapsed().as_millis(),
            };
        }
    };
    world.stats.runs = 1;
    let mut failure = None;
    let mut done = list.len();
    for (i, act) in list.iter().enumerate() {
        // `#i/n`: the action's index and the list's length (`one … --length n` replays a drawn
        // list; M10).
        world.stats.cursor = format!(
            "{} {} #{}/{} `{}`",
            profile.name(),
            seed,
            i,
            list.len(),
            act
        );
        let r = panic::catch_unwind(AssertUnwindSafe(|| {
            world.apply(act);
        }));
        if r.is_err() {
            let message = take_panic();
            failure = Some(Failure {
                step: i,
                signature: signature(&message),
                message,
            });
            done = i + 1;
            break;
        }
    }
    if failure.is_none() {
        world.stats.cursor = format!(
            "{} {} wind-down after {} actions",
            profile.name(),
            seed,
            list.len()
        );
        let r = panic::catch_unwind(AssertUnwindSafe(|| world.wind_down()));
        if r.is_err() {
            let message = take_panic();
            failure = Some(Failure {
                step: STEP_WIND_DOWN,
                signature: signature(&message),
                message,
            });
        }
    }
    let mut stats = std::mem::take(&mut world.stats);
    stats.hook_checkpoints = world.env.hook_checkpoints;
    // Dropping a world whose VM panicked mid-transaction is fine (LiteSVM owns plain data).
    drop(world);
    Outcome {
        profile,
        seed,
        opts,
        actions: list[..done].to_vec(),
        stats,
        failure,
        millis: t0.elapsed().as_millis(),
    }
}

/// A crash file's JSON.
pub fn crash_json(o: &Outcome, minimized: bool) -> Value {
    let f = o.failure.as_ref().expect("a failure");
    json!({
        "profile": o.profile.name(),
        "seed": o.seed,
        "step": match f.step {
            STEP_SETUP => json!("setup"),
            STEP_WIND_DOWN => json!("wind-down"),
            s => json!(s),
        },
        "signature": f.signature,
        "minimized": minimized,
        "differential": o.opts.differential,
        "avoid_known": o.opts.avoid_known,
        "actions": o.actions.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "message": f.message,
    })
}

/// Writes a crash file into `dir`; returns its path.
pub fn write_crash(dir: &Path, o: &Outcome, minimized: bool) -> PathBuf {
    std::fs::create_dir_all(dir).expect("crash dir");
    let f = o.failure.as_ref().expect("a failure");
    let h = crate::rng::splitmix64(
        f.signature
            .bytes()
            .fold(0u64, |a, b| a.wrapping_mul(131).wrapping_add(u64::from(b))),
    );
    let tag = if minimized { "min" } else { "crash" };
    let path = dir.join(format!(
        "{tag}-{}-{}-{:08x}.json",
        o.profile.name(),
        o.seed,
        h as u32
    ));
    let text = serde_json::to_string_pretty(&crash_json(o, minimized)).expect("json");
    std::fs::write(&path, text).expect("write crash file");
    path
}

/// A crash file read back: profile, seed, actions, signature.
pub struct Crash {
    pub profile: Profile,
    pub seed: u64,
    pub opts: Opts,
    pub actions: Vec<Action>,
    pub signature: String,
}

/// Reads a crash file (or a corpus file of the same shape).
pub fn read_crash(path: &Path) -> Result<Crash, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    let profile = v["profile"]
        .as_str()
        .and_then(Profile::from_name)
        .ok_or("crash file: profile")?;
    let seed = v["seed"].as_u64().ok_or("crash file: seed")?;
    let actions = v["actions"]
        .as_array()
        .ok_or("crash file: actions")?
        .iter()
        .map(|a| {
            a.as_str()
                .ok_or("action")
                .map_err(String::from)?
                .parse::<Action>()
        })
        .collect::<Result<Vec<_>, _>>()?;
    let signature = v["signature"].as_str().unwrap_or_default().to_string();
    let opts = Opts {
        differential: v["differential"].as_bool().unwrap_or(true),
        avoid_known: v["avoid_known"].as_bool().unwrap_or(false),
    };
    Ok(Crash {
        profile,
        seed,
        opts,
        actions,
        signature,
    })
}

/// Delta debugging (ddmin) over `c`'s actions: the smallest list found that still fails with
/// the same signature. `budget` bounds the number of runs.
pub fn tmin(c: &Crash, budget: usize, log: bool) -> Outcome {
    let fails = |acts: &[Action]| -> Option<Outcome> {
        let o = run(c.profile, c.seed, Some(acts), 0, c.opts);
        match &o.failure {
            Some(f) if f.signature == c.signature => Some(o),
            _ => None,
        }
    };
    let mut best = fails(&c.actions).unwrap_or_else(|| {
        panic!(
            "the crash does not reproduce with its signature `{}` (replay it to see what it does now)",
            c.signature
        )
    });
    let mut cur = best.actions.clone();
    let mut n = 2usize;
    let mut runs = 1usize;
    while cur.len() >= 2 && runs < budget {
        let chunk = cur.len().div_ceil(n);
        let mut reduced = false;
        let mut i = 0;
        while i < cur.len() && runs < budget {
            let mut cand = cur[..i].to_vec();
            cand.extend_from_slice(&cur[(i + chunk).min(cur.len())..]);
            runs += 1;
            if let Some(o) = fails(&cand) {
                if log {
                    eprintln!("tmin: {} -> {} actions", cur.len(), o.actions.len());
                }
                cur = o.actions.clone();
                best = o;
                n = (n - 1).max(2);
                reduced = true;
                break;
            }
            i += chunk;
        }
        if !reduced {
            if n >= cur.len() {
                break;
            }
            n = (n * 2).min(cur.len());
        }
    }
    // A last pass: drop single actions one by one.
    let mut i = 0;
    while i < cur.len() && runs < budget {
        let mut cand = cur.clone();
        cand.remove(i);
        runs += 1;
        if let Some(o) = fails(&cand) {
            cur = o.actions.clone();
            best = o;
        } else {
            i += 1;
        }
    }
    best
}

/// A duration like `90s`, `10m`, `5h30m`, `24h`, `1d` (bare numbers are seconds).
pub fn parse_duration(s: &str) -> Result<u64, String> {
    let mut total = 0u64;
    let mut num = String::new();
    for c in s.chars() {
        if c.is_ascii_digit() {
            num.push(c);
            continue;
        }
        let v: u64 = num.parse().map_err(|_| format!("bad duration `{s}`"))?;
        num.clear();
        total += v * match c {
            's' => 1,
            'm' => 60,
            'h' => 3_600,
            'd' => 86_400,
            _ => return Err(format!("bad duration unit in `{s}`")),
        };
    }
    if !num.is_empty() {
        total += num
            .parse::<u64>()
            .map_err(|_| format!("bad duration `{s}`"))?;
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The derived lengths stay where FUZZ-M7-24H-3's reproducer recorded them (`--actions 400`,
    /// amm-v4 seed 20333958: 280 actions), within `[max/4, max]`.
    #[test]
    fn run_lengths() {
        assert_eq!(run_length(20_333_958, 400), 280);
        for seed in 0..1_000 {
            let n = run_length(seed, 300);
            assert!((75..=300).contains(&n), "{seed}: {n}");
        }
        assert_eq!(run_length(7, 0), 0);
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("90s").unwrap(), 90);
        assert_eq!(parse_duration("5h30m").unwrap(), 19_800);
        assert_eq!(parse_duration("24h").unwrap(), 86_400);
        assert_eq!(parse_duration("15").unwrap(), 15);
        assert!(parse_duration("3x").is_err());
    }

    #[test]
    fn signatures_mask_keys_inside_words() {
        let a = signature(
            "tests/litesvm/src/env.rs:314\nI7 violated: market 2rL3hRBjHEnL8JdZcz98GiuhUYmEZ7CpfrSoBkecQdGg: trader_wsol holds 2800000001 lamports",
        );
        let b = signature(
            "tests/litesvm/src/env.rs:314\nI7 violated: market 734mkw25Fy72iU1omyWyjVrYGvZvqmZyCFQJsL1EG3Ha: trader_wsol holds 36000001 lamports",
        );
        assert_eq!(a, b);
        assert!(
            a.contains("market <key>: trader_wsol holds # lamports"),
            "{a}"
        );
    }

    #[test]
    fn signatures_mask_numbers_and_keys() {
        let a = signature(
            "src/world.rs:10\nI13: charge 123 outside [5, 9] for 9xQeWvG816bUx9EPjHmaT23yvVM2ZWbrrpZb9PusVFin",
        );
        let b = signature(
            "src/world.rs:10\nI13: charge 456 outside [7, 8] for 5yvyAddWb8RRGh3pPiFS5eV33eZYeHVmAAfwAFmqVpKK",
        );
        assert_eq!(a, b);
    }
}
