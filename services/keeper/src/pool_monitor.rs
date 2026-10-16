//! Raydium pool selection off-chain (ARCHITECTURE §2.5 `None`, §2.6, §7.1 duty 5's Raydium fast
//! path and duty 12, the pool monitor; ECON §6.2): which pool a market should be on, and the
//! `propose_pool` / `execute_pool_switch` the program would accept now. The keeper loop
//! ([`crate::keeper`]) feeds these functions each tick and sends what they plan.
//!
//! **Discovery** ([`discover`]). Neither Raydium program derives a pool from its mint (R-RAY
//! §6.1), so the keeper finds every Raydium SOL pool of a covered mint with `getProgramAccounts`
//! byte filters (ARCH §7.1 "Inputs"): AMM v4 `coin_vault_mint` @400 / `pc_vault_mint` @432 and
//! CPMM `token_0_mint` @168 / `token_1_mint` @200, the mint on one side and WSOL on the other,
//! both orientations ([`discovery_queries`]: four calls per mint; the CPMM program orders its
//! mints, so one of its two calls matches nothing on mainnet, but a filter costs less than a
//! missed pool), each also filtered on the one account size the program verifies (`dataSize`
//! 752 for `AmmInfo`, 637 for `PoolState`: the node then compares bytes only on pool accounts,
//! and an account of another size is one `verify` would refuse anyway). Each pool account
//! gives its candidate ReadSet in the adapter's order
//! ([`CandidatePool::from_account`]). The calls can go to a separate endpoint
//! (`pool_select.discovery_rpc`).
//!
//! **Ranking** ([`rank`], M4-SEC-7). Anyone can create a CPMM pool for a mint (≈ 0.17 SOL), so
//! the discovered set is unbounded. Every discovered pool is scored once per discovery on what
//! its own account already says (a status and nonce `verify` would refuse, an unknown SOL side:
//! out) and one batched read of its WSOL vault and LP mint: its locked SOL `λ·R`, the SOL side
//! of its locked depth — unlocked liquidity scores nothing, so junk pools rank last however many
//! there are, and only burned LP could push a pool up. At most `rank_keys_per_tick` keys are read
//! per tick (a large set is ranked over several ticks, the previous ranking in force meanwhile;
//! no new discovery starts before it completes), and only the best `max_candidates` are
//! evaluated each tick.
//!
//! **Evaluation** ([`evaluate`]): the candidate's ReadSet accounts, fetched fresh, go through
//! **the program's own verifier** — `shortwin::engine::pool_select::verify_candidate`, i.e.
//! `venues::raydium_v4::verify` / `venues::raydium_cpmm::verify` with `eligible()` (owner,
//! length, status, nonce, the WSOL and token sides, vaults, config, LP mint; executable, fee ≤
//! `venue_fee_cap_bps`, and the floor on locked depth at the market's `P_ema` — the candidate's
//! spot on a market without a price: M4-SEC-1 / M4-SEC-3) — on host-side `AccountInfo`s, so a
//! candidate the keeper ranks is exactly one the program would verify, and its locked depth
//! `D = λ·√(X·Y)` is the program's number. A list-only market's D1 check is the program's
//! `classify_pump` on its detection set ([`classify`]). On a market with open positions the
//! evaluation also runs the program's **"switch only if all fit"** check
//! (`shortwin::engine::pool_select::carried_fit`, owner decision of 2026-09-29) on the market as
//! the next instruction sees it, with the cached oracle: whether every open position fits the
//! candidate's normal opening caps, which a collection window's candidate needs.
//!
//! **Plan** ([`plan`]), on the market as the next instruction sees it (the prologue projected
//! to the cluster clock, [`crate::decide::project`]: `pool_d` refreshed, the `pool_fail` chain
//! stepped):
//!
//! | Situation | Action |
//! |---|---|
//! | a collection window has run (`now ≥ fp_t0 + pool_fast_window_s`; a Raydium market's fast path, or a list-only market with positions, M4-SEC-4) | `execute_pool_switch` on the window's candidate (`fp_keys`) |
//! | otherwise | `propose_pool` of the deepest eligible candidate (not the incumbent, not the window's own) **iff `shortwin::engine::pool_select::decide` accepts it**: a list-only market adopts it (with positions open: opens or improves a window); a Raydium market with no open position adopts it at the 5 % margin inside the listing window, 25 % after; a fast path armed (`pool_fail` for `pool_fast_route_s`) opens a window with it; a running window takes it if it is deeper than `fp_d` and the chain that armed it holds (M4-SEC-2) — a window only with a candidate above the window floor (its locked SOL at `P_ema` alone, M4V-1; else nothing, `PoolTooThin`) and **whose opening caps every open position fits** (switch only if all fit: the deepest candidate that fits and that `decide` still takes for the window; none: nothing, `CarriedPositionsTooLarge` — the positions then exit on the failed pool through the S3 chain), and never a candidate whose own program the latch holds (the keeper ranks the others, M4V-4) |
//!
//! The decision is the program's own function on the program's own numbers, so the keeper
//! proposes at the first second the program accepts, and never otherwise (a market whose
//! incumbent is healthy and whose positions are open gets nothing: `PositionsOpen`). Every send
//! is still simulated first. Besides the action, a plan says whether the market is **pinned**
//! (R4B-10: a candidate that would be adopted at zero positions is blocked by open positions;
//! after `pinned_alert_s` the keeper raises `Alert::PoolPinned` so the Emergency multisig can
//! pause opens there and let the market drain) and whether the keeper must **keep the
//! `pool_fail` chain** alive itself (a failing incumbent on a market without positions, which
//! duty 5's no-route cadence does not observe, with a candidate deep enough for the fast path).
//!
//! **Cadence** ([`PoolTrack`]): discovery at the first sight of a market and every
//! `discovery_interval_s` after (every `fast_discovery_interval_s` while the incumbent fails or a
//! window runs; never while a ranking is still in progress); the best candidates' accounts
//! every `eval_interval_s`, and at once when the
//! incumbent starts or stops failing `pool_fail`, and every tick while the fast path is armed, a
//! window runs, or a pinned candidate meets a market whose last position just closed — so the
//! keeper proposes in the slot it sees `open_positions` reach 0 (ARCH §7.1 duty 12). Between
//! evaluations the last plan's `keep_chain` still observes a failing market without positions
//! on the no-route cadence.

use anchor_lang::prelude::Pubkey;
use shortwin::constants::{RAYDIUM_CPMM_PROGRAM_ID, RAYDIUM_V4_PROGRAM_ID, WSOL_MINT};
use shortwin::engine::pool_select::{self, FitAt, FitContext, Proposal, SelectParams, Situation};
use shortwin::errors::ShortwinError;
use shortwin::state::{Market, VenueKind, raydium_cache};
use shortwin::venues::detect::{self, PumpClass};
use shortwin_math::pool_select::{dominates, fast_path_qualifies, lambda_bps};
use shortwin_services_common::{AccountData, Chain, Memcmp};
use shortwin_venue_state::{mint, raydium_cpmm, raydium_v4};

use crate::config::PoolSelectConfig;
use crate::view::with_account_infos;

/// The custom error code of an Anchor error (`None` for a runtime error).
pub fn code_of(e: &anchor_lang::error::Error) -> Option<u32> {
    match e {
        anchor_lang::error::Error::AnchorError(a) => Some(a.error_code_number),
        anchor_lang::error::Error::ProgramError(_) => None,
    }
}

/// `true` if `code` is `e`.
pub fn is(code: Option<u32>, e: ShortwinError) -> bool {
    code == Some(u32::from(e))
}

/// One `getProgramAccounts` query of the discovery.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiscoveryQuery {
    pub kind: VenueKind,
    pub program: Pubkey,
    /// The pool account's exact size (`dataSize`): AMM v4 `AmmInfo` 752 B, CPMM `PoolState`
    /// 637 B (ARCH §2.6 verification: `data_len ==` these).
    pub data_size: usize,
    pub filters: Vec<Memcmp>,
}

fn memcmp(offset: usize, key: &Pubkey) -> Memcmp {
    Memcmp {
        offset,
        bytes: key.to_bytes().to_vec(),
    }
}

/// The four queries that find every Raydium SOL pool of `mint` (module docs): AMM v4 with the
/// mint as coin and WSOL as pc, and the reverse; CPMM with the mint as token 0 and WSOL as
/// token 1, and the reverse.
pub fn discovery_queries(mint: &Pubkey) -> Vec<DiscoveryQuery> {
    let w = WSOL_MINT;
    let v4 = |a: &Pubkey, b: &Pubkey| DiscoveryQuery {
        kind: VenueKind::RaydiumV4,
        program: RAYDIUM_V4_PROGRAM_ID,
        data_size: raydium_v4::LEN,
        filters: vec![
            memcmp(raydium_v4::OFF_COIN_VAULT_MINT, a),
            memcmp(raydium_v4::OFF_PC_VAULT_MINT, b),
        ],
    };
    let cpmm = |a: &Pubkey, b: &Pubkey| DiscoveryQuery {
        kind: VenueKind::RaydiumCpmm,
        program: RAYDIUM_CPMM_PROGRAM_ID,
        data_size: raydium_cpmm::POOL_LEN,
        filters: vec![
            memcmp(raydium_cpmm::OFF_TOKEN_0_MINT, a),
            memcmp(raydium_cpmm::OFF_TOKEN_1_MINT, b),
        ],
    };
    vec![v4(mint, &w), v4(&w, mint), cpmm(mint, &w), cpmm(&w, mint)]
}

/// A discovered Raydium SOL pool of a mint, with its candidate ReadSet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CandidatePool {
    pub kind: VenueKind,
    pub pool: Pubkey,
    /// The ReadSet `propose_pool` takes for it, in the adapter's order: AMM v4 `[pool, coin
    /// vault, pc vault, LP mint]`, CPMM `[pool, amm_config, token 0 vault, token 1 vault, LP
    /// mint]`.
    pub read_keys: Vec<Pubkey>,
    /// Its WSOL vault (the default key when the pool account names no WSOL side).
    pub sol_vault: Pubkey,
    /// Its LP mint.
    pub lp_mint: Pubkey,
    /// The venue's internal LP counter (AMM v4 `lp_amount`, CPMM `lp_supply`).
    pub lp_counter: u64,
    /// The pool account's own fields leave it a chance with the verifier: a WSOL side, and on
    /// AMM v4 a status in {1, 6, 7} and nonce 254, on CPMM swaps not disabled (M4-SEC-7's
    /// pre-filter; a pool that fails it scores nothing).
    pub plausible: bool,
}

impl CandidatePool {
    /// The candidate of pool account `acc` at `pool` on `kind`'s program (`None` if it does
    /// not parse as that program's pool).
    pub fn from_account(kind: VenueKind, pool: Pubkey, acc: &AccountData) -> Option<Self> {
        let owner = acc.owner.to_bytes();
        let k = Pubkey::new_from_array;
        let (read_keys, sol_vault, lp_counter, plausible) = match kind {
            VenueKind::RaydiumV4 => {
                let i = raydium_v4::read(&owner, &acc.data).ok()?;
                let side = i.sol_is_coin();
                let vault = match side {
                    Some(true) => k(i.coin_vault),
                    Some(false) => k(i.pc_vault),
                    None => Pubkey::default(),
                };
                let status_ok = matches!(
                    i.status,
                    raydium_v4::STATUS_INITIALIZED
                        | raydium_v4::STATUS_SWAP_ONLY
                        | raydium_v4::STATUS_WAITING_TRADE
                );
                let nonce_ok =
                    i.nonce == u64::from(shortwin::constants::RAYDIUM_V4_AUTHORITY_NONCE);
                (
                    vec![pool, k(i.coin_vault), k(i.pc_vault), k(i.lp_mint)],
                    vault,
                    i.lp_amount,
                    side.is_some() && status_ok && nonce_ok,
                )
            }
            VenueKind::RaydiumCpmm => {
                let p = raydium_cpmm::read_pool(&owner, &acc.data).ok()?;
                let side = p.sol_is_token0();
                let vault = match side {
                    Some(true) => k(p.token_0_vault),
                    Some(false) => k(p.token_1_vault),
                    None => Pubkey::default(),
                };
                (
                    vec![
                        pool,
                        k(p.amm_config),
                        k(p.token_0_vault),
                        k(p.token_1_vault),
                        k(p.lp_mint),
                    ],
                    vault,
                    p.lp_supply,
                    side.is_some() && p.status & raydium_cpmm::STATUS_BIT_SWAP == 0,
                )
            }
            _ => return None,
        };
        let lp_mint = read_keys.last().copied().unwrap_or_default();
        Some(Self {
            kind,
            pool,
            read_keys,
            sol_vault,
            lp_mint,
            lp_counter,
            plausible,
        })
    }

    /// The accounts [`rank`] reads for it: its WSOL vault and its LP mint.
    pub fn rank_keys(&self) -> [Pubkey; 2] {
        [self.sol_vault, self.lp_mint]
    }
}

/// The ranking score of `c` from its WSOL vault and LP mint accounts ([`CandidatePool::rank_keys`],
/// in that order): its locked SOL `⌊λ_bps·R/10⁴⌋` (`R` the vault's balance; M4-SEC-7). 0 for an
/// implausible pool or accounts that do not read. Unlocked liquidity adds nothing to it.
pub fn score(c: &CandidatePool, accounts: &[Option<AccountData>]) -> u128 {
    if !c.plausible {
        return 0;
    }
    let (Some(Some(vault)), Some(Some(lp))) = (accounts.first(), accounts.get(1)) else {
        return 0;
    };
    let r = vault
        .data
        .get(64..72)
        .and_then(|b| b.try_into().ok())
        .map_or(0, u64::from_le_bytes);
    let Ok(m) = mint::read_base(&lp.owner.to_bytes(), &lp.data) else {
        return 0;
    };
    u128::from(lambda_bps(c.lp_counter, m.supply)) * u128::from(r) / 10_000
}

/// The best `k` candidates by score (highest first; among equals the lowest key), each with a
/// positive score (M4-SEC-7).
pub fn rank(mut scored: Vec<(u128, CandidatePool)>, k: usize) -> Vec<CandidatePool> {
    scored.retain(|(s, _)| *s > 0);
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.pool.cmp(&b.1.pool)));
    scored.into_iter().take(k).map(|(_, c)| c).collect()
}

/// Every Raydium SOL pool of `mint` on `chain` ([`discovery_queries`]), deduplicated, sorted by
/// pool key.
pub async fn discover<C: Chain>(chain: &C, mint: &Pubkey) -> anyhow::Result<Vec<CandidatePool>> {
    let queries = discovery_queries(mint);
    // M7-SEC-8: the four `getProgramAccounts` calls in parallel (one round trip, not four).
    let results = futures::future::try_join_all(
        queries
            .iter()
            .map(|q| chain.program_accounts_sized(&q.program, q.data_size, &q.filters)),
    )
    .await?;
    let mut out: Vec<CandidatePool> = Vec::new();
    for (q, found) in queries.iter().zip(results) {
        for (key, acc) in found {
            if out.iter().any(|c| c.pool == key) {
                continue;
            }
            if let Some(c) = CandidatePool::from_account(q.kind, key, &acc) {
                out.push(c);
            }
        }
    }
    out.sort_by_key(|c| c.pool);
    Ok(out)
}

/// What the program's verifier says about a candidate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Verified and eligible (ARCH §2.6): its locked depth `D`, real SOL reserve `R` and one-way
    /// fee φ (ppm), as the program computes them, whether it passes the window floor of a
    /// collection window's candidate (its locked SOL at the market's `P_ema` alone reaches
    /// `pool_min_real_sol`; `pool_select::window_floor_ok`, M4V-1), and whether the market's open
    /// positions fit its opening caps (`pool_select::carried_fit`, switch only if all fit; `true`
    /// without an open position).
    Eligible {
        d: u128,
        real_sol: u64,
        fee_ppm: u32,
        window_floor: bool,
        fits: bool,
    },
    /// Refused (`code`: the program's custom error — `PoolIneligible`, `PoolTooThin`,
    /// `VenueAccountMismatch`, …).
    Refused { code: Option<u32>, detail: String },
}

/// A candidate after the program's verifier ([`evaluate`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Evaluated {
    pub kind: VenueKind,
    pub pool: Pubkey,
    pub read_keys: Vec<Pubkey>,
    pub verdict: Verdict,
}

impl Evaluated {
    /// `D` of an eligible candidate.
    pub fn d(&self) -> Option<u128> {
        match self.verdict {
            Verdict::Eligible { d, .. } => Some(d),
            Verdict::Refused { .. } => None,
        }
    }

    /// An eligible candidate that may also be a collection window's (M4V-1).
    pub fn window_floor(&self) -> bool {
        matches!(
            self.verdict,
            Verdict::Eligible {
                window_floor: true,
                ..
            }
        )
    }

    /// An eligible candidate whose opening caps the market's open positions fit (switch only if
    /// all fit).
    pub fn fits(&self) -> bool {
        matches!(self.verdict, Verdict::Eligible { fits: true, .. })
    }
}

/// Runs the program's verifier (`pool_select::verify_candidate`: the adapter's `verify` and
/// `eligible()`) on `cand`'s freshly fetched ReadSet `accounts` for `market`'s mint and token
/// program at `now` (module docs), then the program's window floor and "switch only if all fit"
/// (`pool_select::carried_fit` with `fit`: the parameters, the tier table, the cached oracle and
/// the market time the next instruction sees).
pub fn evaluate(
    market: &Market,
    fit: &FitContext,
    now: i64,
    cand: &CandidatePool,
    accounts: &[Option<AccountData>],
) -> Evaluated {
    let params = fit.params;
    let p_ref = pool_select::reference_price(market);
    let verdict = with_account_infos(&cand.read_keys, accounts, |infos| {
        pool_select::verify_candidate(
            cand.kind,
            infos,
            &market.mint,
            &market.token_program,
            params,
            now,
            p_ref,
        )
    });
    let verdict = match verdict {
        Ok(a) => Verdict::Eligible {
            d: a.state.locked_depth,
            real_sol: a.state.r,
            fee_ppm: a.state.fee_ppm,
            window_floor: pool_select::window_floor_ok(&a, params, p_ref),
            fits: pool_select::carried_fit(market, &a, fit, p_ref, FitAt::Proposal)
                .is_ok_and(|f| f.fits()),
        },
        Err(e) => Verdict::Refused {
            code: code_of(&e),
            detail: e.to_string(),
        },
    };
    Evaluated {
        kind: cand.kind,
        pool: cand.pool,
        read_keys: cand.read_keys.clone(),
        verdict,
    }
}

/// D1 on a list-only market (ARCH §2.5 "Precedence"): the program's `classify_pump` on its
/// detection set `[bonding_curve, canonical_pool]` (the fetched accounts, in that order). `Err`
/// with the program's code when it refuses (`CurveMayhemUnsupported`, `CurveQuoteNotSol`,
/// `CurveNeedsExtend`, …); a pump venue ([`PumpClass::pump_venue`]) refuses every Raydium
/// candidate (`PumpVenuePrecedence`).
pub fn classify(
    market: &Market,
    accounts: &[Option<AccountData>],
) -> Result<PumpClass, Option<u32>> {
    let keys = [market.bonding_curve, market.canonical_pool];
    with_account_infos(&keys, accounts, |infos| match infos {
        [bc, cp] => detect::classify_pump(bc, cp, &market.mint).map_err(|e| code_of(&e)),
        _ => Err(None),
    })
}

/// The incumbent pool of a Raydium market (the default key on any other venue).
pub fn incumbent_pool(market: &Market) -> Pubkey {
    match market.venue_kind() {
        Some(k) if k.is_raydium() => market
            .pool_cache
            .get(raydium_cache::RAY_POOL)
            .copied()
            .unwrap_or_default(),
        _ => Pubkey::default(),
    }
}

/// The collection window's candidate: its kind (`fp_venue`) and its ReadSet
/// `fp_keys[..read_len(fp_venue)]` (the CPMM observation in `fp_keys[5]` is not a ReadSet
/// slot). `None` without a window or with a corrupt kind.
pub fn window_candidate(market: &Market) -> Option<(VenueKind, Vec<Pubkey>)> {
    if market.fp_t0 == 0 {
        return None;
    }
    let kind = VenueKind::from_u8(market.fp_venue).filter(|k| k.is_raydium())?;
    let n = shortwin::venues::read_len(kind).ok()?;
    Some((kind, market.fp_keys.get(..n)?.to_vec()))
}

/// What the keeper sends for a market.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// `execute_pool_switch` on the window's candidate (its kind and ReadSet).
    Execute {
        kind: VenueKind,
        candidate: Vec<Pubkey>,
    },
    /// `propose_pool` of `pool` (`kind`, ReadSet `candidate`, locked depth `d`), which the
    /// program's `decide` answers with `expect`.
    Propose {
        kind: VenueKind,
        pool: Pubkey,
        candidate: Vec<Pubkey>,
        d: u128,
        expect: Proposal,
    },
}

impl Action {
    /// The kind of the candidate the action is about (its program's latch bit, M4-SEC-8).
    pub fn kind(&self) -> VenueKind {
        match self {
            Self::Execute { kind, .. } | Self::Propose { kind, .. } => *kind,
        }
    }

    /// The pool the action is about.
    pub fn pool(&self) -> Pubkey {
        match self {
            Self::Execute { candidate, .. } => candidate.first().copied().unwrap_or_default(),
            Self::Propose { pool, .. } => *pool,
        }
    }

    /// Whether this is part of the fast path (a window opened, improved or resolved).
    pub fn fast(&self) -> bool {
        matches!(
            self,
            Self::Execute { .. }
                | Self::Propose {
                    expect: Proposal::OpenWindow | Proposal::ImproveWindow,
                    ..
                }
        )
    }
}

/// A market's pool-selection plan this tick ([`plan`]).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PoolPlan {
    pub action: Option<Action>,
    /// The deepest eligible candidate other than the incumbent and the window's own: its pool
    /// and `D`.
    pub best: Option<(Pubkey, u128)>,
    /// Why nothing is proposed: the program's refusal of the best candidate (`PositionsOpen`,
    /// `PoolNotDominant`, …; `PumpVenuePrecedence` and the curve codes on a pump coin's
    /// list-only market; `VenueUpgraded` while the latch holds a Raydium upgrade).
    pub blocked: Option<u32>,
    /// R4B-10: the best candidate would be adopted at zero positions (≥ `pool_switch_margin_bps`
    /// more locked depth), but positions are open and the incumbent does not fail.
    pub pinned: bool,
    /// The keeper must observe the market on the no-route cadence to keep its `pool_fail` chain
    /// continuous: the incumbent fails, no position is open (so duty 5 does not observe it),
    /// the fast path is not armed yet, and a candidate is deep enough for it.
    pub keep_chain: bool,
}

/// The plan for `market` (as the next instruction sees it: the projected market and its
/// [`Situation`]) given its evaluated candidates (module docs).
pub fn plan(
    s: &Situation,
    p: &SelectParams,
    market: &Market,
    candidates: &[Evaluated],
) -> PoolPlan {
    let raydium = s.venue.is_raydium();
    if (raydium || s.venue == VenueKind::None)
        && s.fp_t0 != 0
        && pool_select::window_ends(s.fp_t0, p.fast_window_s).is_ok_and(|end| s.now >= end)
        && let Some((kind, candidate)) = window_candidate(market)
    {
        return PoolPlan {
            action: Some(Action::Execute { kind, candidate }),
            ..PoolPlan::default()
        };
    }
    let incumbent = incumbent_pool(market);
    let window_pool = (s.fp_t0 != 0).then(|| market.fp_keys[0]);
    let mut ranked: Vec<(u128, &Evaluated)> = candidates
        .iter()
        .filter(|c| c.pool != incumbent && Some(c.pool) != window_pool)
        .filter_map(|c| c.d().map(|d| (d, c)))
        .collect();
    // The deepest first; among equals the lowest key (deterministic).
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.pool.cmp(&b.1.pool)));
    let mut out = PoolPlan::default();
    let Some(&(d, c)) = ranked.first() else {
        return out;
    };
    out.best = Some((c.pool, d));
    // M4V-1: a window's candidate also needs the window floor, which is monotone in `D` at the
    // market's one `P_ema`: if the deepest candidate misses it, every candidate does.
    let window_floor = c.window_floor();
    match pool_select::decide(s, d, p) {
        Ok(Proposal::OpenWindow | Proposal::ImproveWindow) if !window_floor => {
            out.blocked = Some(u32::from(ShortwinError::PoolTooThin));
        }
        // Switch only if all fit: the window takes the deepest candidate whose opening caps every
        // open position fits (not monotone in `D`: the fee, the reserve and the tier count too)
        // and that `decide` still takes for the window; with none, the positions exit on the
        // failed pool through the S3 chain.
        Ok(Proposal::OpenWindow | Proposal::ImproveWindow) if !c.fits() => {
            let fitting = ranked.iter().find_map(|&(d, c)| {
                if !(c.fits() && c.window_floor()) {
                    return None;
                }
                match pool_select::decide(s, d, p) {
                    Ok(expect @ (Proposal::OpenWindow | Proposal::ImproveWindow)) => {
                        Some((d, c, expect))
                    }
                    _ => None,
                }
            });
            match fitting {
                Some((d, c, expect)) => {
                    out.action = Some(Action::Propose {
                        kind: c.kind,
                        pool: c.pool,
                        candidate: c.read_keys.clone(),
                        d,
                        expect,
                    });
                }
                None => out.blocked = Some(u32::from(ShortwinError::CarriedPositionsTooLarge)),
            }
        }
        Ok(expect) => {
            out.action = Some(Action::Propose {
                kind: c.kind,
                pool: c.pool,
                candidate: c.read_keys.clone(),
                d,
                expect,
            });
        }
        Err(e) => {
            let code = code_of(&e);
            out.blocked = code;
            out.pinned = raydium
                && is(code, ShortwinError::PositionsOpen)
                && dominates(d, s.pool_d, u64::from(p.switch_margin_bps));
            out.keep_chain = raydium
                && s.pool_fail
                && s.open_positions == 0
                && s.fp_t0 == 0
                && !pool_select::fast_path_armed(s, p)
                && fast_path_qualifies(d, s.pool_d, u64::from(p.fast_min_bps))
                && window_floor;
        }
    }
    out
}

/// Evaluate the candidates now (module docs, "Cadence"): the fast path is armed, a window
/// runs, a pinned candidate meets a market whose positions are all closed, or the incumbent
/// just started (or stopped) failing `pool_fail`.
pub fn urgent(s: &Situation, p: &SelectParams, track: &PoolTrack) -> bool {
    s.fp_t0 != 0
        || pool_select::fast_path_armed(s, p)
        || (s.open_positions == 0 && track.pinned.is_some())
        || track.last_pool_fail.is_some_and(|f| f != s.pool_fail)
}

/// A market's pool, locked depth and collection window as last seen (someone else's changes
/// are reported).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Seen {
    pub pool: Pubkey,
    pub pool_d: u128,
    pub fp_t0: i64,
    /// The window's candidate pool (`fp_keys[0]`).
    pub fp_pool: Pubkey,
}

impl Seen {
    /// What `market` shows.
    pub fn of(market: &Market) -> Self {
        Self {
            pool: incumbent_pool(market),
            pool_d: market.pool_d.get(),
            fp_t0: market.fp_t0,
            fp_pool: market.fp_keys[0],
        }
    }
}

/// Per-market pool-selection state (module docs, "Cadence").
#[derive(Clone, Debug, Default)]
pub struct PoolTrack {
    /// The best of the mint's Raydium SOL pools as last ranked (at most `max_candidates`,
    /// M4-SEC-7): the ones evaluated each tick.
    pub candidates: Vec<CandidatePool>,
    /// Discovered pools not ranked yet (a ranking in progress, `rank_keys_per_tick` keys a tick).
    pub rank_queue: Vec<CandidatePool>,
    /// The scores of the ranking in progress.
    pub rank_scores: Vec<(u128, CandidatePool)>,
    /// How many pools the last complete discovery found (all of them, before the ranking).
    pub discovered: usize,
    pub last_discovery: Option<i64>,
    pub last_eval: Option<i64>,
    /// The last evaluation of the candidates.
    pub evaluated: Vec<Evaluated>,
    /// `pool_fail` at the last evaluation (its change makes the next one urgent).
    pub last_pool_fail: Option<bool>,
    /// The last plan's `keep_chain`: observe the market on the no-route cadence until the next
    /// evaluation says otherwise.
    pub keep_chain: bool,
    /// R4B-10: the pinned candidate and the second it was first seen pinned.
    pub pinned: Option<(Pubkey, i64)>,
    /// `Alert::PoolPinned` raised for the current pinning.
    pub pinned_alerted: bool,
    /// The last action attempted: the second and its pool (at most one per second each).
    pub tried: Option<(i64, Pubkey)>,
    /// No proposal before this second (after a refusal).
    pub backoff_until: i64,
    /// The market as last seen.
    pub seen: Option<Seen>,
    /// Refusals already alerted (pool, code), until an action lands.
    pub refusals_alerted: Vec<(Pubkey, Option<u32>)>,
}

impl PoolTrack {
    /// A discovery is due: no ranking in progress, and never run or older than the interval
    /// (the fast one while the incumbent fails or a window runs).
    pub fn discovery_due(&self, now: i64, fast: bool, cfg: &PoolSelectConfig) -> bool {
        let every = if fast {
            cfg.fast_discovery_interval_s
        } else {
            cfg.discovery_interval_s
        };
        self.rank_queue.is_empty()
            && self
                .last_discovery
                .is_none_or(|t| now < t || now.saturating_sub(t) >= every)
    }

    /// Starts ranking a discovery's pools (M4-SEC-7): the implausible ones score 0 at once.
    pub fn start_ranking(&mut self, found: Vec<CandidatePool>) {
        self.discovered = found.len();
        self.rank_scores.clear();
        let (plausible, rest): (Vec<_>, Vec<_>) = found.into_iter().partition(|c| c.plausible);
        self.rank_scores.extend(rest.into_iter().map(|c| (0, c)));
        self.rank_queue = plausible;
    }

    /// The next pools to rank within `budget_keys` keys (at least one pool).
    pub fn next_rank_batch(&mut self, budget_keys: usize) -> Vec<CandidatePool> {
        let n = (budget_keys / 2).max(1).min(self.rank_queue.len());
        self.rank_queue.drain(..n).collect()
    }

    /// Records a ranked batch; once the queue is empty the best `k` become the candidates
    /// (returns `true` then).
    pub fn finish_rank_batch(&mut self, scored: Vec<(u128, CandidatePool)>, k: usize) -> bool {
        self.rank_scores.extend(scored);
        if !self.rank_queue.is_empty() {
            return false;
        }
        self.candidates = rank(std::mem::take(&mut self.rank_scores), k);
        true
    }

    /// An evaluation is due: urgent ([`urgent`]), never run, or older than `eval_interval_s`.
    pub fn eval_due(&self, now: i64, urgent: bool, cfg: &PoolSelectConfig) -> bool {
        urgent
            || self
                .last_eval
                .is_none_or(|t| now < t || now.saturating_sub(t) >= cfg.eval_interval_s)
    }

    /// Records this tick's pinned state; returns `Some(since)` exactly once per pinning, when it
    /// has lasted `pinned_alert_s` (R4B-10).
    pub fn note_pinned(
        &mut self,
        pinned: Option<Pubkey>,
        now: i64,
        cfg: &PoolSelectConfig,
    ) -> Option<i64> {
        match pinned {
            None => {
                self.pinned = None;
                self.pinned_alerted = false;
                None
            }
            Some(pool) => {
                let since = match self.pinned {
                    Some((p, since)) if p == pool => since,
                    _ => {
                        self.pinned = Some((pool, now));
                        self.pinned_alerted = false;
                        now
                    }
                };
                if !self.pinned_alerted && now.saturating_sub(since) >= cfg.pinned_alert_s {
                    self.pinned_alerted = true;
                    Some(since)
                } else {
                    None
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytemuck::Zeroable;
    use shortwin::events::{POOL_KIND_LISTING, POOL_KIND_LISTING_WINDOW, POOL_KIND_ZERO_POSITION};

    const P: SelectParams = SelectParams {
        list_window_s: 3_600,
        list_margin_bps: 500,
        switch_margin_bps: 2_500,
        fast_route_s: 120,
        fast_min_bps: 2_500,
        fast_window_s: 60,
    };

    fn eligible(pool: Pubkey, d: u128) -> Evaluated {
        Evaluated {
            kind: VenueKind::RaydiumCpmm,
            pool,
            read_keys: vec![pool, Pubkey::new_unique()],
            verdict: Verdict::Eligible {
                d,
                real_sol: 25_000_000_000,
                fee_ppm: 2_500,
                window_floor: true,
                fits: true,
            },
        }
    }

    /// An eligible candidate whose opening caps the market's open positions do not fit (switch
    /// only if all fit).
    fn too_small(pool: Pubkey, d: u128) -> Evaluated {
        let mut e = eligible(pool, d);
        if let Verdict::Eligible { fits, .. } = &mut e.verdict {
            *fits = false;
        }
        e
    }

    /// An eligible candidate under the window floor (M4V-1: its locked SOL alone is below
    /// `pool_min_real_sol`).
    fn under_window_floor(pool: Pubkey, d: u128) -> Evaluated {
        let mut e = eligible(pool, d);
        if let Verdict::Eligible { window_floor, .. } = &mut e.verdict {
            *window_floor = false;
        }
        e
    }

    fn refused(pool: Pubkey, e: ShortwinError) -> Evaluated {
        Evaluated {
            kind: VenueKind::RaydiumV4,
            pool,
            read_keys: vec![pool],
            verdict: Verdict::Refused {
                code: Some(u32::from(e)),
                detail: String::new(),
            },
        }
    }

    /// A healthy AMM v4 market on `inc`, listed at 1,000, now 10,000, `pool_d` 10,000.
    fn market(inc: Pubkey) -> (Market, Situation) {
        let mut m = Market::zeroed();
        m.venue = VenueKind::RaydiumV4 as u8;
        m.pool_cache[raydium_cache::RAY_POOL] = inc;
        m.pool_d.set(10_000);
        m.pool_seen_at = 1_000;
        let s = Situation::of(&m, VenueKind::RaydiumV4, 10_000, false);
        (m, s)
    }

    fn code(e: ShortwinError) -> Option<u32> {
        Some(u32::from(e))
    }

    fn proposed(plan: &PoolPlan) -> Option<(Pubkey, Proposal)> {
        match &plan.action {
            Some(Action::Propose { pool, expect, .. }) => Some((*pool, *expect)),
            _ => None,
        }
    }

    #[test]
    fn the_queries_filter_both_programs_on_both_sides() {
        let mint = Pubkey::new_unique();
        let q = discovery_queries(&mint);
        assert_eq!(q.len(), 4);
        type Row = (VenueKind, Pubkey, usize, Vec<u8>, usize, Vec<u8>);
        let offsets: Vec<Row> = q
            .iter()
            .map(|q| {
                (
                    q.kind,
                    q.program,
                    q.filters[0].offset,
                    q.filters[0].bytes.clone(),
                    q.filters[1].offset,
                    q.filters[1].bytes.clone(),
                )
            })
            .collect();
        let (m, w) = (mint.to_bytes().to_vec(), WSOL_MINT.to_bytes().to_vec());
        const V4: VenueKind = VenueKind::RaydiumV4;
        const CPMM: VenueKind = VenueKind::RaydiumCpmm;
        assert_eq!(
            offsets,
            vec![
                (V4, RAYDIUM_V4_PROGRAM_ID, 400, m.clone(), 432, w.clone()),
                (V4, RAYDIUM_V4_PROGRAM_ID, 400, w.clone(), 432, m.clone()),
                (
                    CPMM,
                    RAYDIUM_CPMM_PROGRAM_ID,
                    168,
                    m.clone(),
                    200,
                    w.clone()
                ),
                (CPMM, RAYDIUM_CPMM_PROGRAM_ID, 168, w, 200, m),
            ]
        );
        // The one account size each program's verifier accepts (ARCH §2.6).
        let sizes: Vec<usize> = q.iter().map(|q| q.data_size).collect();
        assert_eq!(sizes, vec![752, 752, 637, 637]);
    }

    #[test]
    fn a_pool_account_gives_the_adapters_read_set() {
        let pool = Pubkey::new_unique();
        let keys: Vec<Pubkey> = (0..5).map(|_| Pubkey::new_unique()).collect();
        // AMM v4: coin vault @336, pc vault @368, LP mint @464; exactly 752 bytes.
        let mut d = vec![0u8; raydium_v4::LEN];
        for (off, k) in [
            (raydium_v4::OFF_COIN_VAULT, keys[0]),
            (raydium_v4::OFF_PC_VAULT, keys[1]),
            (raydium_v4::OFF_LP_MINT, keys[2]),
        ] {
            d[off..off + 32].copy_from_slice(k.as_ref());
        }
        let acc = |owner: Pubkey, data: Vec<u8>| AccountData {
            lamports: 1,
            owner,
            executable: false,
            data,
        };
        let c = CandidatePool::from_account(
            VenueKind::RaydiumV4,
            pool,
            &acc(RAYDIUM_V4_PROGRAM_ID, d.clone()),
        )
        .unwrap();
        assert_eq!(c.read_keys, vec![pool, keys[0], keys[1], keys[2]]);
        // Another owner, another length or the wrong kind: not a candidate.
        assert!(
            CandidatePool::from_account(
                VenueKind::RaydiumV4,
                pool,
                &acc(Pubkey::new_unique(), d.clone())
            )
            .is_none()
        );
        let mut long = d.clone();
        long.push(0);
        assert!(
            CandidatePool::from_account(
                VenueKind::RaydiumV4,
                pool,
                &acc(RAYDIUM_V4_PROGRAM_ID, long)
            )
            .is_none()
        );
        assert!(
            CandidatePool::from_account(VenueKind::PumpSwap, pool, &acc(RAYDIUM_V4_PROGRAM_ID, d))
                .is_none()
        );
        // CPMM: config @8, vaults @72 / @104, LP mint @136, behind its discriminator.
        let mut d = vec![0u8; raydium_cpmm::POOL_LEN];
        d[..8].copy_from_slice(&raydium_cpmm::POOL_DISCRIMINATOR);
        for (off, k) in [
            (raydium_cpmm::OFF_AMM_CONFIG, keys[0]),
            (raydium_cpmm::OFF_TOKEN_0_VAULT, keys[1]),
            (raydium_cpmm::OFF_TOKEN_1_VAULT, keys[2]),
            (raydium_cpmm::OFF_LP_MINT, keys[3]),
        ] {
            d[off..off + 32].copy_from_slice(k.as_ref());
        }
        let c = CandidatePool::from_account(
            VenueKind::RaydiumCpmm,
            pool,
            &acc(RAYDIUM_CPMM_PROGRAM_ID, d),
        )
        .unwrap();
        assert_eq!(c.read_keys, vec![pool, keys[0], keys[1], keys[2], keys[3]]);
    }

    /// The program's verifier on host accounts: a missing ReadSet is refused with the adapter's
    /// own code, never ranked.
    #[test]
    fn evaluation_is_the_programs_verifier() {
        let mut m = Market::zeroed();
        m.mint = Pubkey::new_unique();
        m.token_program = shortwin::constants::TOKEN_PROGRAM_ID;
        let pool = Pubkey::new_unique();
        let lp_mint = Pubkey::new_unique();
        let cand = CandidatePool {
            kind: VenueKind::RaydiumV4,
            pool,
            read_keys: vec![pool, Pubkey::new_unique(), Pubkey::new_unique(), lp_mint],
            sol_vault: Pubkey::new_unique(),
            lp_mint,
            lp_counter: 0,
            plausible: true,
        };
        let params: shortwin::state::Params = Zeroable::zeroed();
        let tiers = shortwin::state::TierParams::defaults();
        let fit = FitContext {
            params: &params,
            tiers: &tiers,
            oracle: Default::default(),
            now: 1,
        };
        let e = evaluate(&m, &fit, 1, &cand, &[]);
        assert_eq!(e.d(), None);
        assert!(
            matches!(&e.verdict, Verdict::Refused { code, .. } if is(*code, ShortwinError::PoolIneligible)),
            "{e:?}"
        );
        // D1 on a market whose curve and canonical pool do not exist: not a pump coin.
        assert_eq!(classify(&m, &[None, None]), Ok(PumpClass::NotPump));
    }

    #[test]
    fn a_list_only_market_adopts_the_deepest_eligible_candidate() {
        let mut m = Market::zeroed();
        m.venue = VenueKind::None as u8;
        let s = Situation::of(&m, VenueKind::None, 10_000, false);
        let (a, b, c) = (
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
        );
        let cands = [
            eligible(a, 5),
            refused(b, ShortwinError::PoolTooThin),
            eligible(c, 7),
        ];
        let pl = plan(&s, &P, &m, &cands);
        assert_eq!(proposed(&pl), Some((c, Proposal::Adopt(POOL_KIND_LISTING))));
        assert_eq!(pl.best, Some((c, 7)));
        // Nothing eligible: nothing to do.
        let pl = plan(&s, &P, &m, &cands[1..2]);
        assert_eq!(pl, PoolPlan::default());
        // M4-SEC-4: with positions open (M5's curve that graduated elsewhere) the deepest
        // candidate opens a collection window, a deeper one improves it, and the keeper
        // executes it once it has run.
        m.open_positions = 2;
        let s = Situation::of(&m, VenueKind::None, 10_000, false);
        let pl = plan(&s, &P, &m, &cands);
        assert_eq!(proposed(&pl), Some((c, Proposal::OpenWindow)));
        m.fp_t0 = 10_000;
        m.fp_d.set(5);
        m.fp_keys[0] = a;
        m.fp_venue = VenueKind::RaydiumCpmm as u8;
        let s = Situation::of(&m, VenueKind::None, 10_030, false);
        let pl = plan(&s, &P, &m, &cands);
        assert_eq!(proposed(&pl), Some((c, Proposal::ImproveWindow)));
        let s = Situation::of(&m, VenueKind::None, 10_060, false);
        let pl = plan(&s, &P, &m, &cands);
        assert!(
            matches!(&pl.action, Some(Action::Execute { kind: VenueKind::RaydiumCpmm, candidate }) if candidate[0] == a),
            "{pl:?}"
        );
    }

    /// M4-SEC-7: the ranking keeps the best `k` pools by locked SOL; pools the pool account
    /// already rules out, or whose vault and LP mint do not read, score nothing.
    #[test]
    fn the_ranking_keeps_the_best_pools_by_locked_sol() {
        let pool = |plausible: bool| CandidatePool {
            kind: VenueKind::RaydiumCpmm,
            pool: Pubkey::new_unique(),
            read_keys: vec![],
            sol_vault: Pubkey::new_unique(),
            lp_mint: Pubkey::new_unique(),
            lp_counter: 1_000,
            plausible,
        };
        let vault = |amount: u64| {
            let mut data = vec![0u8; 165];
            data[64..72].copy_from_slice(&amount.to_le_bytes());
            Some(AccountData {
                lamports: 1,
                owner: shortwin::constants::TOKEN_PROGRAM_ID,
                executable: false,
                data,
            })
        };
        let lp = |supply: u64| {
            let mut data = vec![0u8; 82];
            data[36..44].copy_from_slice(&supply.to_le_bytes());
            data[44] = 9;
            data[45] = 1;
            Some(AccountData {
                lamports: 1,
                owner: shortwin::constants::TOKEN_PROGRAM_ID,
                executable: false,
                data,
            })
        };
        let (a, b, c, d) = (pool(true), pool(true), pool(true), pool(false));
        // a: 100 SOL at λ 50 % (50 locked); b: 1,000 SOL at λ 1 % (10); c: unlocked (0).
        assert_eq!(score(&a, &[vault(100), lp(500)]), 50);
        assert_eq!(score(&b, &[vault(1_000), lp(990)]), 10);
        assert_eq!(score(&c, &[vault(1_000_000), lp(1_000)]), 0);
        assert_eq!(score(&d, &[vault(100), lp(0)]), 0, "implausible");
        assert_eq!(score(&a, &[None, lp(500)]), 0);
        let ranked = rank(vec![(10, b.clone()), (0, c), (50, a.clone()), (0, d)], 8);
        assert_eq!(ranked, vec![a.clone(), b]);
        assert_eq!(rank(vec![(10, pool(true)), (50, a.clone())], 1), vec![a]);
        // A discovery is ranked over ticks within the key budget; no new discovery meanwhile.
        let cfg = PoolSelectConfig::default();
        let mut t = PoolTrack::default();
        let found: Vec<CandidatePool> = (0..250).map(|i| pool(i % 5 != 0)).collect();
        t.start_ranking(found);
        assert_eq!((t.discovered, t.rank_queue.len()), (250, 200));
        assert!(!t.discovery_due(0, false, &cfg));
        let mut ticks = 0;
        while !t.rank_queue.is_empty() {
            let batch = t.next_rank_batch(cfg.rank_keys_per_tick);
            assert!(batch.len() * 2 <= cfg.rank_keys_per_tick);
            let scored = batch.into_iter().map(|c| (1, c)).collect();
            t.finish_rank_batch(scored, cfg.max_candidates);
            ticks += 1;
        }
        assert_eq!((ticks, t.candidates.len()), (2, cfg.max_candidates));
        assert!(t.discovery_due(0, false, &cfg));
    }

    #[test]
    fn zero_position_margins_and_the_listing_window() {
        let inc = Pubkey::new_unique();
        let (m, after) = market(inc);
        let cand = Pubkey::new_unique();
        // The incumbent itself is never a candidate, however deep it reads.
        let pl = plan(
            &after,
            &P,
            &m,
            &[eligible(inc, 1_000_000), eligible(cand, 12_499)],
        );
        assert_eq!(pl.action, None);
        assert_eq!(pl.blocked, code(ShortwinError::PoolNotDominant));
        assert!(!pl.pinned && !pl.keep_chain);
        let pl = plan(&after, &P, &m, &[eligible(cand, 12_500)]);
        assert_eq!(
            proposed(&pl),
            Some((cand, Proposal::Adopt(POOL_KIND_ZERO_POSITION)))
        );
        // Inside the listing window: 5 %.
        let inside = Situation {
            pool_seen_at: 9_000,
            ..after
        };
        let pl = plan(&inside, &P, &m, &[eligible(cand, 10_500)]);
        assert_eq!(
            proposed(&pl),
            Some((cand, Proposal::Adopt(POOL_KIND_LISTING_WINDOW)))
        );
        assert_eq!(
            plan(&inside, &P, &m, &[eligible(cand, 10_499)]).action,
            None
        );
    }

    /// Open positions and a healthy incumbent: nothing is proposed, however deep the candidate;
    /// one that would be adopted at zero positions pins the market (R4B-10).
    #[test]
    fn open_positions_and_a_healthy_incumbent_propose_nothing() {
        let inc = Pubkey::new_unique();
        let (m, s) = market(inc);
        let s = Situation {
            open_positions: 1,
            ..s
        };
        let cand = Pubkey::new_unique();
        let pl = plan(&s, &P, &m, &[eligible(cand, 1_000_000)]);
        assert_eq!(pl.action, None);
        assert_eq!(pl.blocked, code(ShortwinError::PositionsOpen));
        assert!(pl.pinned);
        // Below the switch margin: blocked, not pinned.
        let pl = plan(&s, &P, &m, &[eligible(cand, 12_499)]);
        assert_eq!(pl.blocked, code(ShortwinError::PositionsOpen));
        assert!(!pl.pinned);
    }

    #[test]
    fn the_fast_path_opens_improves_and_executes_the_window() {
        let inc = Pubkey::new_unique();
        let (mut m, s) = market(inc);
        let failing = |since: i64| Situation {
            open_positions: 3,
            pool_fail: true,
            pool_fail_since: since,
            ..s
        };
        let (shallow, deep, deeper) = (
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
        );
        let cands = [eligible(shallow, 3_000), eligible(deep, 6_000)];
        // 119 s: not armed, nothing; the chain is duty 5's (positions are open).
        let pl = plan(&failing(10_000 - 119), &P, &m, &cands);
        assert_eq!(pl.action, None);
        assert!(!pl.keep_chain);
        // 120 s: the deepest opens the window.
        let pl = plan(&failing(10_000 - 120), &P, &m, &cands);
        assert_eq!(proposed(&pl), Some((deep, Proposal::OpenWindow)));
        assert!(pl.action.as_ref().unwrap().fast());
        // A window on `shallow` (someone else's): the deepest improves it.
        m.fp_t0 = 9_990;
        m.fp_d.set(3_000);
        m.fp_venue = VenueKind::RaydiumCpmm as u8;
        m.fp_keys[0] = shallow;
        let win = Situation {
            fp_t0: 9_990,
            fp_d: 3_000,
            ..failing(9_000)
        };
        let pl = plan(&win, &P, &m, &cands);
        assert_eq!(proposed(&pl), Some((deep, Proposal::ImproveWindow)));
        // The window's own pool is never proposed again; nothing deeper: nothing.
        m.fp_keys[0] = deep;
        let win = Situation { fp_d: 6_000, ..win };
        assert_eq!(plan(&win, &P, &m, &cands).action, None);
        let pl = plan(
            &win,
            &P,
            &m,
            &[eligible(deeper, 6_001), eligible(deep, 9_999)],
        );
        assert_eq!(proposed(&pl), Some((deeper, Proposal::ImproveWindow)));
        // After the window: execute on the recorded keys (the CPMM ReadSet: five of six).
        for (i, k) in m.fp_keys.iter_mut().enumerate().skip(1) {
            *k = Pubkey::new_from_array([i as u8; 32]);
        }
        let due = Situation {
            now: 9_990 + 60,
            ..win
        };
        let pl = plan(&due, &P, &m, &cands);
        assert_eq!(
            pl.action,
            Some(Action::Execute {
                kind: VenueKind::RaydiumCpmm,
                candidate: m.fp_keys[..5].to_vec()
            })
        );
        assert_eq!(pl.action.as_ref().unwrap().pool(), deep);
    }

    /// M4V-1: a candidate under the window floor never opens or improves a window (the program
    /// refuses `PoolTooThin`), and the keeper does not keep a chain for it; at zero positions it
    /// is still adopted by the zero-position rule, whose floor is the listing one.
    #[test]
    fn a_candidate_under_the_window_floor_opens_no_window() {
        let inc = Pubkey::new_unique();
        let (m, s) = market(inc);
        let cand = Pubkey::new_unique();
        let armed = Situation {
            open_positions: 3,
            pool_fail: true,
            pool_fail_since: 10_000 - 120,
            ..s
        };
        let pl = plan(&armed, &P, &m, &[under_window_floor(cand, 6_000)]);
        assert_eq!(pl.action, None);
        assert_eq!(pl.blocked, code(ShortwinError::PoolTooThin));
        let failing = Situation {
            open_positions: 0,
            pool_fail: true,
            pool_fail_since: 10_000 - 10,
            ..s
        };
        assert!(!plan(&failing, &P, &m, &[under_window_floor(cand, 6_000)]).keep_chain);
        assert!(plan(&failing, &P, &m, &[eligible(cand, 6_000)]).keep_chain);
        let pl = plan(&s, &P, &m, &[under_window_floor(cand, 12_500)]);
        assert_eq!(
            proposed(&pl),
            Some((
                cand,
                Proposal::Adopt(shortwin::events::POOL_KIND_ZERO_POSITION)
            ))
        );
    }

    /// Switch only if all fit: a window takes only a candidate whose opening caps every open
    /// position fits — the deepest such one that the window still takes; none: nothing
    /// (`CarriedPositionsTooLarge`), the positions exit on the failed pool through the S3 chain.
    /// Zero-position adoptions do not ask.
    #[test]
    fn a_window_takes_only_a_candidate_the_carried_positions_fit() {
        let inc = Pubkey::new_unique();
        let (mut m, s) = market(inc);
        let armed = Situation {
            open_positions: 3,
            pool_fail: true,
            pool_fail_since: 10_000 - 120,
            ..s
        };
        let (big, mid, small, tiny) = (
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
            Pubkey::new_unique(),
        );
        // The deepest does not fit: the next deepest that fits opens the window.
        let cands = [
            too_small(big, 9_000),
            eligible(mid, 6_000),
            eligible(small, 3_000),
            eligible(tiny, 2_000),
        ];
        let pl = plan(&armed, &P, &m, &cands);
        assert_eq!(proposed(&pl), Some((mid, Proposal::OpenWindow)));
        assert_eq!(pl.best, Some((big, 9_000)));
        // Only candidates below the fast-path minimum fit: nothing (the window would refuse them).
        let pl = plan(
            &armed,
            &P,
            &m,
            &[too_small(big, 9_000), eligible(tiny, 2_000)],
        );
        assert_eq!(pl.action, None);
        assert_eq!(pl.blocked, code(ShortwinError::CarriedPositionsTooLarge));
        // None fits.
        let none = [too_small(big, 9_000), too_small(mid, 6_000)];
        let pl = plan(&armed, &P, &m, &none);
        assert_eq!(pl.action, None);
        assert_eq!(pl.blocked, code(ShortwinError::CarriedPositionsTooLarge));
        // A running window on `small`: a fitting deeper candidate improves it, one that is not
        // deeper than the window's does not.
        m.fp_t0 = 9_990;
        m.fp_d.set(3_000);
        m.fp_venue = VenueKind::RaydiumCpmm as u8;
        m.fp_keys[0] = small;
        let win = Situation {
            fp_t0: 9_990,
            fp_d: 3_000,
            pool_fail_since: 9_000,
            ..armed
        };
        let pl = plan(&win, &P, &m, &cands);
        assert_eq!(proposed(&pl), Some((mid, Proposal::ImproveWindow)));
        let pl = plan(
            &win,
            &P,
            &m,
            &[too_small(big, 9_000), eligible(tiny, 2_000)],
        );
        assert_eq!(pl.blocked, code(ShortwinError::CarriedPositionsTooLarge));
        // No position open: a dominant candidate is adopted whatever `fits` says (it is `true`
        // there anyway: nothing to carry).
        let (m0, s0) = market(inc);
        let pl = plan(&s0, &P, &m0, &[too_small(big, 12_500)]);
        assert_eq!(
            proposed(&pl),
            Some((big, Proposal::Adopt(POOL_KIND_ZERO_POSITION)))
        );
    }

    /// A failing incumbent on a market without positions: duty 5 does not observe it, so the
    /// keeper keeps the chain itself while a candidate is deep enough for the fast path.
    #[test]
    fn a_failing_market_without_positions_keeps_its_chain() {
        let inc = Pubkey::new_unique();
        let (m, s) = market(inc);
        let failing = Situation {
            pool_fail: true,
            pool_fail_since: 10_000 - 30,
            ..s
        };
        let cand = Pubkey::new_unique();
        let pl = plan(&failing, &P, &m, &[eligible(cand, 4_000)]);
        assert!(pl.keep_chain && pl.action.is_none());
        // Too thin for the fast path: no chain to keep.
        assert!(!plan(&failing, &P, &m, &[eligible(cand, 2_499)]).keep_chain);
        // Dominant: adopted at once (zero positions), no chain needed.
        let pl = plan(&failing, &P, &m, &[eligible(cand, 12_500)]);
        assert_eq!(
            proposed(&pl),
            Some((cand, Proposal::Adopt(POOL_KIND_ZERO_POSITION)))
        );
        // Armed: the window.
        let armed = Situation {
            pool_fail_since: 10_000 - 120,
            ..failing
        };
        let pl = plan(&armed, &P, &m, &[eligible(cand, 4_000)]);
        assert_eq!(proposed(&pl), Some((cand, Proposal::OpenWindow)));
        assert!(!pl.keep_chain);
    }

    #[test]
    fn cadence_and_pinning() {
        let cfg = PoolSelectConfig {
            discovery_interval_s: 300,
            fast_discovery_interval_s: 10,
            eval_interval_s: 30,
            pinned_alert_s: 100,
            ..PoolSelectConfig::default()
        };
        let mut t = PoolTrack::default();
        assert!(t.discovery_due(0, false, &cfg) && t.eval_due(0, false, &cfg));
        t.last_discovery = Some(1_000);
        t.last_eval = Some(1_000);
        assert!(!t.discovery_due(1_299, false, &cfg) && t.discovery_due(1_300, false, &cfg));
        assert!(!t.discovery_due(1_009, true, &cfg) && t.discovery_due(1_010, true, &cfg));
        assert!(!t.eval_due(1_029, false, &cfg) && t.eval_due(1_029, true, &cfg));
        assert!(t.eval_due(1_030, false, &cfg));
        // Pinned for 100 s: one alert, then none; a new candidate restarts the clock.
        let (a, b) = (Pubkey::new_unique(), Pubkey::new_unique());
        assert_eq!(t.note_pinned(Some(a), 1_000, &cfg), None);
        assert_eq!(t.note_pinned(Some(a), 1_099, &cfg), None);
        assert_eq!(t.note_pinned(Some(a), 1_100, &cfg), Some(1_000));
        assert_eq!(t.note_pinned(Some(a), 1_500, &cfg), None);
        assert_eq!(t.note_pinned(Some(b), 1_600, &cfg), None);
        assert_eq!(t.pinned, Some((b, 1_600)));
        assert_eq!(t.note_pinned(None, 1_700, &cfg), None);
        assert_eq!((t.pinned, t.pinned_alerted), (None, false));
        // A pinned candidate meeting zero positions is urgent.
        let (_, s) = market(Pubkey::new_unique());
        assert!(!urgent(&s, &P, &t));
        t.pinned = Some((a, 1));
        assert!(urgent(&s, &P, &t));
        assert!(!urgent(
            &Situation {
                open_positions: 1,
                ..s
            },
            &P,
            &t
        ));
        // The incumbent starts failing, or recovers: evaluate at once.
        t.pinned = None;
        t.last_pool_fail = Some(false);
        assert!(!urgent(&s, &P, &t));
        let failing = Situation {
            pool_fail: true,
            pool_fail_since: s.now,
            ..s
        };
        assert!(urgent(&failing, &P, &t));
        t.last_pool_fail = Some(true);
        assert!(!urgent(&failing, &P, &t) && urgent(&s, &P, &t));
    }
}
