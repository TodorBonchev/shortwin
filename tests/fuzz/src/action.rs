//! Actions: a kind and three numbers (`m`, the market slot; `a`, `b`, kind-specific), decoded from
//! the seed stream with per-profile weights, and a one-line text form (`kind m a b`) for crash
//! files, the corpus, `replay` and `tmin`. An action is interpreted against the state it meets:
//! indices are taken modulo the live lists, so every subsequence of a recorded list is itself a
//! valid list (what delta debugging needs).

use std::fmt;
use std::str::FromStr;

use crate::rng::Rng;

/// Declares [`Kind`] with its text names.
macro_rules! kinds {
    ($($(#[$doc:meta])* $name:ident = $text:literal,)*) => {
        /// What an action does (ARCH §8.4's action list; module docs of [`crate::world`]).
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum Kind {
            $($(#[$doc])* $name,)*
        }

        impl Kind {
            /// Every kind.
            pub const ALL: &'static [Kind] = &[$(Kind::$name,)*];

            /// The text name.
            pub fn name(self) -> &'static str {
                match self {
                    $(Kind::$name => $text,)*
                }
            }

            /// The kind of a text name.
            pub fn from_name(s: &str) -> Option<Self> {
                match s {
                    $($text => Some(Kind::$name),)*
                    _ => None,
                }
            }
        }
    };
}

kinds! {
    // ---- trading --------------------------------------------------------------------------
    /// `open_short` by borrower `a`, size log-uniform in `[X/10⁶, X/20]` from `b`.
    Open = "open",
    /// The largest `q` an `open_short` accepts now (bisection on clones of the VM), opened.
    OpenNearCap = "open_near_cap",
    /// `close_short` of open position `a`.
    Close = "close",
    /// `add_margin` of `1 + b mod 10⁹` lamports on position `a`.
    AddMargin = "add_margin",
    // ---- lenders ----------------------------------------------------------------------------
    /// `deposit` by lender `a` of `b`‰ of its tokens (more minted or planted if short).
    Deposit = "deposit",
    /// `withdraw` by lender `a` of `b`‰ of its shares (all of them at `b ≥ 1000`).
    Withdraw = "withdraw",
    /// `claim_sol` ×`1 + b mod 4` by lender `a`, in one transaction (M10, V2-2: every claim
    /// after the first pays 0).
    ClaimSol = "claim_sol",
    /// `cover_deficit` by lender `a`.
    CoverDeficit = "cover_deficit",
    /// `sync_lender` of lender `a % n`, through the records it is behind on: signed by its owner
    /// when bit 16 of `a` is set, else sent by a third party (KI65-REV-SEC1; `a` is uniform, so
    /// the two choices are independent up to a bias of order `n/2³²`).
    SyncLender = "sync_lender",
    /// `sweep_trader`.
    SweepTrader = "sweep_trader",
    /// A venue outage held for 130 s … 2 h (`a`): the venue halted, its fee above the hard cap,
    /// or halted with the route flagged (`b`), the keeper cranking every 1–30 s and settling
    /// every position (the no-route chain, S3 after `s3_wait_s`, T-S3-BROKEN); then cleared.
    Outage = "outage",
    /// The Raydium fast path end to end: a deeper candidate planted, the incumbent halted past
    /// `pool_fast_route_s` with the keeper cranking, `propose_pool` (the window), the window
    /// waited out, `execute_pool_switch`, the incumbent restored.
    FastPath = "fast_path",
    /// A 1-lamport donation to the trader and a `sweep_trader` every minute, `1 + b mod 5` times.
    DustSweeps = "dust_sweeps",
    /// Every lender withdraws everything and claims (all lenders out), then a sweep; with
    /// `b` odd one lender keeps a dust share.
    LendersOut = "lenders_out",
    // ---- keeper and time ----------------------------------------------------------------------
    /// `1 + a mod 40` keeper seconds: each second a crank of every market, then `liquidate`,
    /// `expire` and `sol_settle` on every open position.
    KeeperSeconds = "keeper_seconds",
    /// One `crank_price` of market `m` (no warp).
    Crank = "crank",
    /// One keeper exit (`b mod 3`: liquidate, expire, sol_settle) on position `a` (no warp).
    KeeperExit = "keeper_exit",
    /// Keeper silence: `COVERAGE_GAP_S + 1 + a mod 900` seconds (41 s and more since the owner
    /// raised `COVERAGE_GAP_S` to 40, OWNER-M7-12) without an observation, then one crank
    /// (`CoverageGap`); with `b mod 3 ≠ 0` and the role installed, degraded mode (O17): the
    /// keeper's `set_coverage_pause(true)` on every market, cleared `1..=8` actions later.
    Silence = "silence",
    /// Warp by 1 s … 3 d (log-uniform from `a`), no crank.
    Warp = "warp",
    /// Warp to position `a`'s deadline `± b mod 8` seconds (or past its S3 tail), then keeper
    /// seconds.
    WarpToDeadline = "warp_to_deadline",
    // ---- prices and liquidity ------------------------------------------------------------------
    /// An honest random swap on the venue (either side, size from `b`; on the live curve one of
    /// pump's v2 or v3 trades, pump-final §12).
    Trade = "trade",
    /// A price shock ×`k` (up if `a` even, down otherwise; `k` from 1.05 to 10 from `b`), held,
    /// then `a mod 30` keeper seconds.
    Shock = "shock",
    /// A ramp to ×`k` over `2 + a mod 60` seconds of honest trades and keeper seconds.
    Ramp = "ramp",
    /// A third-party liquidity add (`a` even) or removal of `b`‱ of the pool, held.
    Liquidity = "liquidity",
    /// `[withdraw, crank, deposit]` (`a` even) or `[deposit, crank, withdraw]`, one
    /// transaction (the LP-wrapped crank).
    LpWrappedCrank = "lp_wrapped_crank",
    /// `[LP deposit, open_short, LP withdraw]` in one transaction.
    DepositWrappedOpen = "deposit_wrapped_open",
    /// `[LP withdraw, liquidate(a), LP deposit]` in one transaction.
    WithdrawWrappedExit = "withdraw_wrapped_exit",
    // ---- atomic bundles (ECON §12) ----------------------------------------------------------
    /// `[push ×k, crank, unwind]` in one transaction, `1 + a mod 20` seconds in a row.
    PumpCrankDump = "pump_crank_dump",
    /// A contrary low observation `[push ÷k, crank, unwind]` every second for `1 + a mod 20`
    /// seconds, then `sol_settle` on every position.
    ContraryLow = "contrary_low",
    /// `1 + a mod 6` attacker-only seconds (push up, down or flip from `a / 6`) before a
    /// settlement: each second only the attacker's bundle observes; then `sol_settle` and
    /// `liquidate` on every position.
    AttackerSeconds = "attacker_seconds",
    /// One push, then an opposite observation in every second that also holds the keeper's
    /// (`1 + a mod 30` seconds).
    PushThenFreeze = "push_then_freeze",
    /// `[front, exit, back]` in one transaction on position `a` (`b mod 4`: close, liquidate,
    /// expire, sol_settle; `b / 4` the front's direction and size).
    SandwichExit = "sandwich_exit",
    /// The same, held: front, exit and back in three transactions.
    HeldSandwich = "held_sandwich",
    /// `[dump or LP deposit, sol_settle(a), unwind]` in one transaction.
    DumpAroundSettle = "dump_around_settle",
    /// A JIT deposit around an exit: `[deposit, exit(a), withdraw]` by lender `b` — the exit a
    /// liquidation or an expiry that lands (with `b` even, at the position's deadline), the
    /// withdrawal the minted shares within the allowance; the JIT capture oracle (M10, R3B-2).
    JitDeposit = "jit_deposit",
    // ---- pauses, flags, venue admin ------------------------------------------------------------
    /// Sets the market's gates from `a`'s bits: opens paused (1), route broken (2), venue halted
    /// (4); they stay until the next `gates` action (I10's fuzz action).
    Gates = "gates",
    /// `set_pause(Global)` on (`a` odd) or off.
    GlobalPause = "global_pause",
    /// `disable_venue` of the market's venue bit, then (after the timelock) re-enabled by
    /// `propose_config(EnableVenues)`.
    DisableVenue = "disable_venue",
    /// The venue's own halt (mock `disabled`, AMM v4 status, CPMM status bits, PumpSwap
    /// `disable_flags`) set from `a` (0 clears).
    VenueHalt = "venue_halt",
    /// A venue fee shock (level `a`; 0 restores).
    FeeShock = "fee_shock",
    /// A venue-state layout change (mock `layout_version`) on (`a` odd) or off.
    LayoutShock = "layout_shock",
    /// A venue that over-delivers on exact-out swaps (mock `over_delivery` = `a mod 10⁶`).
    OverDelivery = "over_delivery",
    /// A venue interface change (mock `required_extras`): the per-mint PDA the published
    /// template derives (`a mod 3 = 0`, recoverable by [`Kind::ExtrasSet`] and a `current`,
    /// `previous` or `stable` selection), one or two fresh keys no template names
    /// (`a mod 3 = 1`: only the route flag and S3 recover the exits), or none.
    ExtrasShock = "extras_shock",
    /// A venue program upgrade (its ProgramData deploy slot changes); `flag_venue_upgraded`
    /// with `a` odd.
    UpgradeShock = "upgrade_shock",
    /// `revalidate_venue` of the market's venue programs at their live slots.
    Revalidate = "revalidate",
    /// `set_route_broken(Global, venue bit)` on (`a` odd) or off.
    RouteBrokenVenue = "route_broken_venue",
    /// A donation (`a mod 7`: trader, `trader_wsol` address, `ATA(trader, WSOL)`, boost ATA,
    /// tokens — to the pool vault, or on a pump venue with `a / 7` even to `trader_base_ata`
    /// (M10), or on a curve not yet graduated with `a / 7 ≡ 3 (mod 4)` to its `curve_base_ata`
    /// (pump-final §12: `(1 + b mod 1000)`‰ of 206.9e12 tokens, before or after a completion)
    /// —, market vault, a position) of `b`-scaled lamports or tokens.
    Donate = "donate",
    // ---- governance and oracle ---------------------------------------------------------------
    /// An adversarial in-bound config change (a parameter at a bound, the EMA parameters or
    /// the kink) proposed, the timelock waited out, applied.
    ConfigChange = "config_change",
    /// `crank_sol_usd` on a planted `PriceUpdateV2` (price moved by `b`, `a` seconds old).
    OracleCrank = "oracle_crank",
    // ---- pool selection (Raydium) ----------------------------------------------------------------
    /// Plants a candidate pool of the market's token (deeper or thinner from `a`, AMM v4 or
    /// CPMM from `b`).
    PlantCandidate = "plant_candidate",
    /// `propose_pool` of planted candidate `a`.
    ProposePool = "propose_pool",
    /// `execute_pool_switch` on the window's candidate.
    ExecuteSwitch = "execute_switch",
    // ---- the curve's graduation (M5 / M6) ------------------------------------------------------
    /// Another user completes the curve (pump-final §12): with `a` odd its `buy_v2` of exactly the
    /// remaining real tokens `rT`, the action before P7; with `a` even a `buy_v3(rT + δ)` that
    /// overshoots and completes the curve by pump's synthetic migration, `δ` = `(1 + b mod 500)`‰
    /// of the would-be pool `X_s = curve_base_ata − rT` (up to 50 %). A crash file of an earlier
    /// fuzzer names it `complete_curve` ([`LEGACY_COMPLETE_CURVE`]): parsed with `a` odd, so it
    /// replays the `buy_v2` of `rT` it recorded whatever its `a`.
    CrowdCompletes = "crowd_completes",
    /// pump `migrate_v2` by a third party.
    Migrate = "migrate",
    /// PumpSwap `disable_flags` bit 0 (`create_pool`, graduation halt) on (`a` odd) or off.
    GraduationHalt = "graduation_halt",
    /// PumpSwap's fee buckets (pump's 2026-10-02 redeploy, PUMP-2026-10-1): a third party's
    /// `buy_v2` (`a` even) or `sell_v2` of `1 + b mod 500`‱ of the pool's tokens, which keeps
    /// the protocol fee (less the buyback slice) and the creator fee in the quote vault, off the
    /// VQR; with `a mod 8 = 7` the permissionless `sweep_protocol_fee` and `sweep_creator_fee`
    /// instead (the buckets out, the price unchanged). On the curve profile once graduated.
    FeeBuckets = "fee_buckets",
    // ---- I14 -------------------------------------------------------------------------------------
    /// A short.win instruction (`a mod 5`: open, liquidate, expire, sol_settle, and at 4 —
    /// with a position open and `a / 5` even — close (M10), else crank) with one remaining
    /// account (`b`) omitted: must be refused with nothing changed.
    OmitAccount = "omit_account",
    /// The same with one remaining account replaced by a fresh key: must be refused too (an
    /// accepted replacement fails the run since M10; before, an anomaly).
    ReplaceAccount = "replace_account",
    // ---- M7: degraded mode, fees, the mint filter, extras sets ----------------------------------
    /// `set_coverage_pause` (O17's pause-opens-only `coverage_authority`): pause (`a` odd) or
    /// clear one market's `paused_coverage`, signed (`b mod 16`) by the role's key, Governance,
    /// the Emergency vault (★OWNER-M7-3 (b): its clear accepted, its pause refused) or a
    /// stranger, with the keeper's reason or a bad one; `b mod 16 = 15` rotates the role instead
    /// (Governance's timelocked `Authorities` change: a new key, or the default key, which makes
    /// the role inert). The outcome is predicted exactly, and a landed one must change nothing on
    /// the whole VM but bit 6 of that market's `flags` (I4's coverage clause).
    CoveragePause = "coverage_pause",
    /// `sweep_protocol_fees` of the market (anyone; to `config.fee_recipient`).
    SweepFees = "sweep_protocol_fees",
    /// The mint filter's tripwire: a freeze authority planted on the market's mint (held
    /// `1..=8` actions), with `flag_unsafe_mint` by anyone when `b mod 8 = 1` (one-way: opens
    /// and deposits stay refused); `flag_unsafe_mint` on a safe mint when `a mod 4 = 3`.
    MintShock = "mint_shock",
    /// The Emergency's `set_venue_extras` on the mock (`a mod 4`: the template the venue's
    /// interface change needs, a hostile fixed-key entry, an empty set, or both legs' template),
    /// on the sell leg, the buy leg or both (`b mod 3`). Opens and closes then pick `none`,
    /// `current`, `previous` or `stable` from their own numbers.
    ExtrasSet = "extras_set",
    /// A config change proposed and then cancelled (`cancel_config` by Governance or the
    /// Emergency vault, `a` odd), or left to expire past the apply window and then applied
    /// (`PendingChangeExpired`) when `b mod 4 = 3`.
    ConfigCancel = "config_cancel",
    // ---- M7 security review (M7-SEC-6) -----------------------------------------------------------
    /// `[set_coverage_pause(false), open_short, set_coverage_pause(true)]` signed by the role's key
    /// (`a` odd: the flag set first, the keeper's degraded mode): on a coverage-paused market the
    /// open must be refused `CoveragePaused` (the clear delay, M7-SEC-3); on one without the flag
    /// it meets its ordinary gates.
    CoverageBracket = "coverage_bracket",
    /// `[sale or buy of the venue within the open's spot–EMA guard, open_short, unwind or not]` in
    /// one transaction (M5-SEC-3, M7-SEC-4, M7-SEC-5): every term of the open is the I19 hook's.
    WrappedOpen = "wrapped_open",
    // ---- the owner's decisions of 2026-10-01 ------------------------------------------------------
    /// ★OWNER-M7-3 (b): the Emergency vault (`pause_authority`) clears one market's coverage pause
    /// — never sets one. `a` odd first sets the flag with the role's key (a hijacked key's pause,
    /// when the role is installed). `b mod 4`: 0 or 3, the clear alone (`b / 4 mod 8 = 7`: a bad
    /// reason, `InvalidParams`) — accepted with the role installed, unset or rotated, changing
    /// nothing on the whole VM but the flag and, for a set flag, `coverage_cleared_at` (I4's
    /// coverage clause) — then, after a clear of a set flag, an open in the same second must be
    /// refused `CoveragePaused` (the clear delay); 1, the Emergency's set: `Unauthorized`, or
    /// `CoverageAuthorityDisabled` while the role is unset; 2, `[Emergency clear, open_short]` in
    /// one transaction: refused `CoveragePaused` on a coverage-paused market, ordinary gates
    /// otherwise.
    EmergencyClear = "emergency_clear",
    // ---- M10: the adversarial suite (ARCH §8.2 F5) -------------------------------------------
    /// `[withdraw, sol_settle]` in one transaction (C0-9): lender `a` withdraws (the shares as
    /// [`Kind::Withdraw`] picks them from `b`) ahead of a settlement of position `b / 1100`; the
    /// withdrawal must stay within the pro-rata allowance the lender had before it.
    WithdrawThenSettle = "withdraw_then_settle",
    /// FUZZ-M7-24H-3's shape (M10, the deficit tripwire's reach): position `a` — opened near the
    /// cap first when none is open — left without a keeper for `1 + a mod 3` days, one crank (the
    /// rent receivable accrues to the lenders of then), lender `b` deposits `1 + b mod 3` times
    /// the pool's token claims (a late depositor, ECON §9.4); with `b` even the price is steered
    /// (ten cranked steps, up to three times) until the position's `B̂_live` lies in `(V − R_due,
    /// V − K_s − ert]`, where a liquidation pays less rent than it was credited; then `10 + a mod
    /// 50` keeper seconds.
    StaleReceivable = "stale_receivable",
}

/// The crowd's completion's name before P7 ([`Kind::CrowdCompletes`]), still parsed: always
/// pump's `buy_v2` of exactly `rT`, so it parses with `a` made odd and replays as recorded.
pub const LEGACY_COMPLETE_CURVE: &str = "complete_curve";

/// One action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Action {
    pub kind: Kind,
    /// The market slot (modulo the profile's markets).
    pub m: u8,
    pub a: u32,
    pub b: u32,
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} {} {}", self.kind.name(), self.m, self.a, self.b)
    }
}

impl FromStr for Action {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let mut it = s.split_whitespace();
        let name = it.next().ok_or("empty action")?;
        // `complete_curve`: the crowd's completion before P7 renamed it, always a `buy_v2` of
        // exactly `rT` (it read neither number) — `crowd_completes` with `a` odd.
        let legacy = name == LEGACY_COMPLETE_CURVE;
        let kind = Kind::from_name(name)
            .or_else(|| legacy.then_some(Kind::CrowdCompletes))
            .ok_or_else(|| format!("unknown action `{name}`"))?;
        let mut num = |what: &str| -> Result<u32, String> {
            it.next()
                .ok_or_else(|| format!("`{s}`: missing {what}"))?
                .parse::<u32>()
                .map_err(|e| format!("`{s}`: {what}: {e}"))
        };
        let m = u8::try_from(num("m")?).map_err(|e| e.to_string())?;
        let a = num("a")? | u32::from(legacy);
        let b = num("b")?;
        Ok(Self { kind, m, a, b })
    }
}

/// The fuzz profiles (ARCH §8.4; the survey's F2).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Profile {
    /// The test-venue build and one mock market (SPL or Token-2022), with the mock's knobs.
    Mock,
    /// The test-venue build and two mock markets (global actions hit both).
    Mixed,
    /// The pump fork: a live curve (fresh at a random progress, `near`, `legacy115`) and its
    /// graduation to PumpSwap.
    Curve,
    /// The PumpSwap fork: `fresh`, `mature` (third-party LP), `cashback` or `legacy261`.
    PumpSwap,
    /// The Raydium fork: Fartcoin on AMM v4 and planted candidate pools.
    AmmV4,
    /// The Raydium fork: USELESS or the Token-2022 pool on CPMM, an LP provider, planted
    /// candidates.
    Cpmm,
}

impl Profile {
    /// Every profile.
    pub const ALL: [Profile; 6] = [
        Profile::Mock,
        Profile::Mixed,
        Profile::Curve,
        Profile::PumpSwap,
        Profile::AmmV4,
        Profile::Cpmm,
    ];

    /// The text name.
    pub fn name(self) -> &'static str {
        match self {
            Profile::Mock => "mock",
            Profile::Mixed => "mixed",
            Profile::Curve => "curve",
            Profile::PumpSwap => "pumpswap",
            Profile::AmmV4 => "amm-v4",
            Profile::Cpmm => "cpmm",
        }
    }

    /// The profile of a text name.
    pub fn from_name(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|p| p.name() == s)
    }

    /// Whether it runs on the test-venue build (else the release build).
    pub fn test_venue(self) -> bool {
        matches!(self, Profile::Mock | Profile::Mixed)
    }

    /// The weight of `kind` in this profile (0: never generated).
    pub fn weight(self, kind: Kind) -> u32 {
        use Kind::*;
        let mock = matches!(self, Profile::Mock | Profile::Mixed);
        let ray = matches!(self, Profile::AmmV4 | Profile::Cpmm);
        let lp = matches!(
            self,
            Profile::Mock | Profile::Mixed | Profile::Cpmm | Profile::PumpSwap | Profile::Curve
        );
        let base = match kind {
            Open => 14,
            OpenNearCap => 3,
            Close => 7,
            AddMargin => 3,
            Deposit => 4,
            Withdraw => 4,
            ClaimSol => 3,
            CoverDeficit => 1,
            SyncLender => 1,
            SweepTrader => 1,
            Outage => 1,
            DustSweeps => 1,
            LendersOut => 1,
            KeeperSeconds => 10,
            Crank => 4,
            KeeperExit => 6,
            Silence => 2,
            Warp => 3,
            WarpToDeadline => 2,
            Trade => 6,
            Shock => 5,
            Ramp => 2,
            PumpCrankDump => 3,
            ContraryLow => 2,
            AttackerSeconds => 2,
            PushThenFreeze => 1,
            SandwichExit => 3,
            HeldSandwich => 1,
            DumpAroundSettle => 2,
            JitDeposit => 1,
            Gates => 2,
            GlobalPause => 1,
            DisableVenue => 1,
            VenueHalt => 2,
            FeeShock => 1,
            UpgradeShock => 1,
            Revalidate => 1,
            RouteBrokenVenue => 1,
            Donate => 2,
            ConfigChange => 1,
            OracleCrank => 2,
            OmitAccount => 1,
            ReplaceAccount => 1,
            CoveragePause => 3,
            SweepFees => 1,
            MintShock => 1,
            ConfigCancel => 1,
            CoverageBracket => 1,
            WrappedOpen => 3,
            EmergencyClear => 2,
            WithdrawThenSettle => 1,
            StaleReceivable => 1,
            Liquidity | LpWrappedCrank | DepositWrappedOpen | WithdrawWrappedExit => {
                return if lp {
                    match kind {
                        Liquidity => 3,
                        LpWrappedCrank => 2,
                        _ => 1,
                    }
                } else {
                    0
                };
            }
            LayoutShock | OverDelivery | ExtrasShock => return if mock { 1 } else { 0 },
            ExtrasSet => return if mock { 2 } else { 0 },
            FastPath => return if ray { 1 } else { 0 },
            PlantCandidate | ProposePool | ExecuteSwitch => {
                return if ray {
                    match kind {
                        PlantCandidate => 1,
                        _ => 2,
                    }
                } else {
                    0
                };
            }
            CrowdCompletes | Migrate => return if self == Profile::Curve { 2 } else { 0 },
            GraduationHalt => {
                return if matches!(self, Profile::Curve | Profile::PumpSwap) {
                    1
                } else {
                    0
                };
            }
            FeeBuckets => {
                return match self {
                    Profile::PumpSwap => 3,
                    Profile::Curve => 1,
                    _ => 0,
                };
            }
        };
        // PumpSwap: M7-SEC-5's shape — a fee shock planting a tier boundary between the market
        // caps, with an `open_near_cap` inside its hold — three times as dense since M10's fuzz
        // follow-ups (the fee-scaled caps at the live fee, `scripts/mutants.sh`'s
        // `pump-caps-live-fee`, was killed by two runs in a hundred and survived a 3-minute fuzz
        // on a loaded machine).
        if self == Profile::PumpSwap {
            match kind {
                FeeShock => return 3,
                OpenNearCap => return 6,
                _ => {}
            }
        }
        // The Raydium and PumpSwap programs are upgraded through their own guard indices; the
        // mock has no venue program in the guard, so upgrade shocks there are no-ops.
        if mock && matches!(kind, UpgradeShock | Revalidate) {
            return 0;
        }
        base
    }

    /// The number of markets the profile sets up.
    pub fn markets(self) -> usize {
        match self {
            Profile::Mixed => 2,
            _ => 1,
        }
    }
}

/// Draws one action for `profile` from `rng` (weights of [`Profile::weight`]).
pub fn draw(profile: Profile, rng: &mut Rng) -> Action {
    let total: u64 = Kind::ALL
        .iter()
        .map(|k| u64::from(profile.weight(*k)))
        .sum();
    let mut x = rng.below(total);
    let mut kind = Kind::Open;
    for k in Kind::ALL {
        let w = u64::from(profile.weight(*k));
        if x < w {
            kind = *k;
            break;
        }
        x -= w;
    }
    Action {
        kind,
        m: (rng.below(profile.markets() as u64)) as u8,
        a: rng.next_u32(),
        b: rng.next_u32(),
    }
}

/// `n` actions for `profile` from `seed` (the action stream is a pure function of both).
pub fn stream(profile: Profile, seed: u64, n: usize) -> Vec<Action> {
    let mut rng = Rng::new(seed);
    (0..n).map(|_| draw(profile, &mut rng)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_round_trip() {
        for p in Profile::ALL {
            for a in stream(p, 42, 200) {
                let s = a.to_string();
                assert_eq!(s.parse::<Action>().unwrap(), a, "{s}");
                assert!(p.weight(a.kind) > 0);
            }
        }
        assert!("nope 0 0 0".parse::<Action>().is_err());
        // The crowd's completion kept its old name (P7 renamed it), and its old action: the
        // `buy_v2` of `rT` (`a` odd) whatever the recorded `a`.
        for (old, new) in [
            ("complete_curve 0 1 2", "crowd_completes 0 1 2"),
            (
                "complete_curve 0 1918102270 7",
                "crowd_completes 0 1918102271 7",
            ),
            ("complete_curve 1 0 0", "crowd_completes 1 1 0"),
        ] {
            assert_eq!(
                old.parse::<Action>().unwrap(),
                new.parse::<Action>().unwrap(),
                "{old}"
            );
        }
        assert_eq!(
            "crowd_completes 0 2 0".parse::<Action>().unwrap().a,
            2,
            "the new name keeps a"
        );
        for k in Kind::ALL {
            assert_eq!(Kind::from_name(k.name()), Some(*k));
        }
    }
}
