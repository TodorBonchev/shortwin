//! Pump detection (ARCH §2.5 `classify_pump`; D1; C0-15).
//!
//! A pump coin with a live bonding curve, a completed curve that is still migrating, or a valid
//! canonical PumpSwap pool trades on the pump venues, even if a deeper Raydium pool exists (D1
//! precedence, ARCH §2.5): `create_market` rejects a Raydium hint for such a mint
//! (`PumpVenuePrecedence`). A curve that completed with zeroed reserves and no canonical pool —
//! a coin that graduated to Raydium before PumpSwap existed, e.g. Fartcoin (ARCH `[V3]`) — is
//! `GraduatedElsewhere` and maps to its Raydium pool.
//!
//! ```text
//! classify_pump(bc, cp):   // both keys derived on-chain and key-checked by the caller (C0-15)
//!   if bc.owner == pump && bc.disc == BondingCurve:
//!       require !curve.is_mayhem_mode && curve.quote_mint == default   (ADOPTED Q6)
//!       require bc.data_len ≥ 115                                        (else CurveNeedsExtend)
//!       if !curve.complete:
//!           require !curve.is_cashback_coin    (else CurveCashbackCoin; pump-final §6.3, D1 (a))
//!           require curve.depth == 0           (else CurveQuoteNotSol)
//!                                          -> PumpCurve
//!       elif canonical_pool_valid(cp)      -> PumpSwap
//!       elif curve.virtual_token_reserves > 0 -> PumpMigrating
//!       else                               -> GraduatedElsewhere
//!   else                                   -> NotPump
//! canonical_pool_valid(cp) := cp.owner == pAMM && disc == Pool && pool.index == 0
//!     && pool.creator == PDA(pump, ["pool-authority", mint]) && pool.base_mint == mint
//!     && pool.quote_mint == WSOL && !pool.is_mayhem_mode
//! ```
//!
//! **An account at the canonical address that is not a valid pool (M5-SEC-5; A63).** The address
//! is pAMM's PDA of `["pool", 0, pool_authority(mint), mint, WSOL]`, so only pAMM can create an
//! account there, and any pool it creates there is this mint's canonical pool by construction.
//! [`canonical_pool`] therefore tells three states apart: [`CanonicalPool::Absent`] (the address
//! is System-owned: nothing there, or only lamports anyone can send), [`CanonicalPool::Valid`],
//! and [`CanonicalPool::Unreadable`] — any other account there (pAMM-owned but not parsing as a
//! valid canonical pool: a layout or discriminator change, a bool byte other than 0/1, fields that
//! a shifted layout no longer puts where they were; or an owner that could not have created it).
//! `Unreadable` is **never** read as "no canonical pool": it is a pump venue for D1
//! ([`PumpClass::PumpPoolUnreadable`]) and, on a curve market, the venue sync keeps the market
//! on its pump kind, not executable (`venues::pump_curve::sync`). Reading it as absent turned a
//! migrated curve into "graduated elsewhere" (`→ None`, open to a Raydium adoption against D1)
//! whenever pAMM changed its `Pool` layout (M5-SEC-5).
//!
//! **Scope.** The Raydium adoptions run it for the precedence check (the Raydium hints of
//! `create_market`, `propose_pool` and `execute_pool_switch` on a list-only market, M3/M4); since
//! M5 the `Auto` listing and `crank_price`'s re-detection of a `None` market also adopt a live or
//! migrating curve, and since M6 a valid canonical PumpSwap pool. A pump-owned account at the curve
//! address that carries the discriminator but does not parse cannot be classified, so it is
//! refused as `PumpVenuePrecedence` (a Raydium adoption needs the proof that no pump venue
//! applies); an account at the canonical-pool address that is not a valid pool is
//! [`PumpClass::PumpPoolUnreadable`], a pump venue, for the same reason (M5-SEC-5).
//!
//! **Cost.** The pool check derives `pool_authority(mint)` (`find_program_address`, 1,500 CU per
//! bump tried — a mint's creator can grind a low bump, M5-SEC-6); instructions that hold the
//! market pass its cached authority ([`pool_authority_of`], `Market::pump_pool_authority`).

use anchor_lang::prelude::*;
use shortwin_venue_state::{pump, pump_amm};

use crate::constants::{PUMP_AMM_PROGRAM_ID, PUMP_PROGRAM_ID, WSOL_MINT};
use crate::errors::ShortwinError;
use crate::instructions::create_market::PUMP_POOL_AUTHORITY_SEED;

/// What `classify_pump` found (ARCH §2.5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PumpClass {
    /// No pump bonding curve for the mint.
    NotPump,
    /// A live curve.
    PumpCurve,
    /// A completed curve whose canonical pool does not exist yet (`vT > 0`).
    PumpMigrating,
    /// A completed curve with a valid canonical PumpSwap pool.
    PumpSwap,
    /// A completed curve whose canonical-pool address holds an account that is not a valid pool
    /// ([`CanonicalPool::Unreadable`]: a pAMM layout change, M5-SEC-5): the coin is on (or
    /// entering) PumpSwap, which short.win cannot read now. A pump venue for D1 (fail closed);
    /// the pump adapters treat it as not executable (A63).
    PumpPoolUnreadable,
    /// A completed curve with zeroed reserves and no canonical pool.
    GraduatedElsewhere,
}

impl PumpClass {
    /// The pump venues take precedence (D1): a Raydium listing is refused.
    pub const fn pump_venue(self) -> bool {
        matches!(
            self,
            Self::PumpCurve | Self::PumpMigrating | Self::PumpSwap | Self::PumpPoolUnreadable
        )
    }
}

/// What the account at a mint's canonical PumpSwap pool address holds (module docs; M5-SEC-5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CanonicalPool {
    /// No pool: the address is System-owned (nothing there, or lamports someone sent).
    Absent,
    /// A valid canonical pool (ARCH §2.5 `canonical_pool_valid`).
    Valid,
    /// Something only pAMM could have put there that is not a valid canonical pool (a layout or
    /// discriminator change, an invalid bool, fields a shifted layout moved), or an owner that
    /// cannot have created it. Never "no pool" (A63): not executable, and a pump venue for D1.
    Unreadable,
}

impl CanonicalPool {
    /// Anything but [`CanonicalPool::Absent`]: the coin has a canonical pool (readable or not).
    pub const fn exists(self) -> bool {
        !matches!(self, Self::Absent)
    }
}

/// `PDA(pump, ["pool-authority", mint])`, the canonical pool's `creator` (`find_program_address`:
/// listing and cold paths only; the market caches it, [`pool_authority_of`]).
pub fn pool_authority(mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(&[PUMP_POOL_AUTHORITY_SEED, mint.as_ref()], &PUMP_PROGRAM_ID).0
}

/// The market's cached pool authority (`Market::pump_pool_authority`, written at listing,
/// M5-SEC-6), derived only for a market that predates the cache.
pub fn pool_authority_of(market: &crate::state::Market) -> Pubkey {
    if market.pump_pool_authority == Pubkey::default() {
        pool_authority(&market.mint)
    } else {
        market.pump_pool_authority
    }
}

/// The state of the canonical pool account `cp` of `mint` (module docs), `authority` its
/// `pool_authority(mint)`. `cp` is already key-checked as the mint's canonical pool address.
pub fn canonical_pool(cp: &AccountInfo, mint: &Pubkey, authority: &Pubkey) -> CanonicalPool {
    canonical_pool_read(cp, mint, authority).0
}

/// [`canonical_pool`] with the pool as read when it is `Valid` (the `→ PumpSwap` transition and a
/// PumpSwap listing write it into the market, M6).
pub fn canonical_pool_read(
    cp: &AccountInfo,
    mint: &Pubkey,
    authority: &Pubkey,
) -> (CanonicalPool, Option<pump_amm::Pool>) {
    if *cp.owner == anchor_lang::system_program::ID {
        return (CanonicalPool::Absent, None);
    }
    if *cp.owner != PUMP_AMM_PROGRAM_ID {
        return (CanonicalPool::Unreadable, None);
    }
    let Ok(data) = cp.try_borrow_data() else {
        return (CanonicalPool::Unreadable, None);
    };
    let Ok(pool) = pump_amm::read_pool(&cp.owner.to_bytes(), &data) else {
        return (CanonicalPool::Unreadable, None);
    };
    let valid = pool.index == pump_amm::CANONICAL_POOL_INDEX
        && pool.creator == authority.to_bytes()
        && pool.base_mint == mint.to_bytes()
        && pool.quote_mint == WSOL_MINT.to_bytes()
        && !pool.is_mayhem_mode;
    if valid {
        (CanonicalPool::Valid, Some(pool))
    } else {
        (CanonicalPool::Unreadable, None)
    }
}

/// `canonical_pool_valid(cp)` (module docs): `cp` is already key-checked as the mint's
/// canonical pool address.
pub fn canonical_pool_valid(cp: &AccountInfo, mint: &Pubkey, authority: &Pubkey) -> bool {
    canonical_pool(cp, mint, authority) == CanonicalPool::Valid
}

/// [`classify_pump_with`] with the pool authority derived (listing, the keeper).
pub fn classify_pump(bc: &AccountInfo, cp: &AccountInfo, mint: &Pubkey) -> Result<PumpClass> {
    classify_pump_with(bc, cp, mint, &pool_authority(mint))
}

/// `classify_pump(bc, cp)` for `mint` (module docs), `authority` = `pool_authority(mint)`. `bc`
/// and `cp` must already be key-checked against the mint's curve and canonical-pool PDAs.
///
/// Errors: `CurveMayhemUnsupported`, `CurveQuoteNotSol` (ADOPTED Q6; on a live curve also a
/// `depth` other than 0, pump's quote-chain depth — defence in depth: a SOL curve has depth 0),
/// `CurveNeedsExtend` (a curve shorter than 115 bytes), `PumpVenuePrecedence` (a pump-owned
/// curve account that does not parse), and `CurveCashbackCoin` for a **live** cashback curve
/// (pump-final §6.3, owner decision D1 (a)): pump's v3 trades refuse a cashback coin (6094) and
/// every curve leg trades v3. The refusal sits on the live branch only (pump-final C2), so a
/// complete cashback curve classifies as any other — `PumpMigrating` during the pause,
/// `PumpSwap` once its canonical pool exists — and the graduated coin lists on PumpSwap, where
/// `crank_price`'s re-detection and the pool selection keep working.
pub fn classify_pump_with(
    bc: &AccountInfo,
    cp: &AccountInfo,
    mint: &Pubkey,
    authority: &Pubkey,
) -> Result<PumpClass> {
    if *bc.owner != PUMP_PROGRAM_ID {
        return Ok(PumpClass::NotPump);
    }
    let data = bc.try_borrow_data()?;
    if data.get(..8) != Some(&pump::BONDING_CURVE_DISCRIMINATOR[..]) {
        return Ok(PumpClass::NotPump);
    }
    let curve = pump::read_curve(&bc.owner.to_bytes(), &data)
        .map_err(|_| error!(ShortwinError::PumpVenuePrecedence))?;
    require!(!curve.is_mayhem_mode, ShortwinError::CurveMayhemUnsupported);
    require!(curve.quote_mint == [0; 32], ShortwinError::CurveQuoteNotSol);
    require!(
        data.len() >= pump::CURVE_MIN_LEN,
        ShortwinError::CurveNeedsExtend
    );
    if !curve.complete {
        require!(!curve.is_cashback_coin, ShortwinError::CurveCashbackCoin);
        require!(curve.depth == 0, ShortwinError::CurveQuoteNotSol);
        return Ok(PumpClass::PumpCurve);
    }
    Ok(match canonical_pool(cp, mint, authority) {
        CanonicalPool::Valid => PumpClass::PumpSwap,
        CanonicalPool::Unreadable => PumpClass::PumpPoolUnreadable,
        CanonicalPool::Absent if curve.virtual_token_reserves > 0 => PumpClass::PumpMigrating,
        CanonicalPool::Absent => PumpClass::GraduatedElsewhere,
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::arithmetic_side_effects)]
mod tests {
    use super::*;

    fn curve(complete: bool, vt: u64, mayhem: bool, quote: [u8; 32], len: usize) -> Vec<u8> {
        let mut d = vec![0u8; len.max(pump::CURVE_MIN_LEN)];
        d[..8].copy_from_slice(&pump::BONDING_CURVE_DISCRIMINATOR);
        d[pump::OFF_VIRTUAL_TOKEN_RESERVES..pump::OFF_VIRTUAL_TOKEN_RESERVES + 8]
            .copy_from_slice(&vt.to_le_bytes());
        d[pump::OFF_COMPLETE] = u8::from(complete);
        d[pump::OFF_IS_MAYHEM_MODE] = u8::from(mayhem);
        d[pump::OFF_QUOTE_MINT..pump::OFF_QUOTE_MINT + 32].copy_from_slice(&quote);
        d.truncate(len);
        d
    }

    fn classify(bc_owner: Pubkey, bc: Vec<u8>) -> Result<PumpClass> {
        let mint = Pubkey::new_unique();
        let (bk, ck) = (Pubkey::new_unique(), Pubkey::new_unique());
        let (mut bl, mut cl) = (1u64, 0u64);
        let mut bd = bc;
        let mut cd: Vec<u8> = Vec::new();
        let system = Pubkey::default();
        let b = AccountInfo::new(&bk, false, false, &mut bl, &mut bd, &bc_owner, false);
        let c = AccountInfo::new(&ck, false, false, &mut cl, &mut cd, &system, false);
        classify_pump(&b, &c, &mint)
    }

    fn code(r: Result<PumpClass>) -> Option<u32> {
        match r {
            Err(anchor_lang::error::Error::AnchorError(e)) => Some(e.error_code_number),
            _ => None,
        }
    }

    #[test]
    fn classifies_curves() {
        let none = [0u8; 32];
        assert_eq!(
            classify(PUMP_PROGRAM_ID, curve(false, 1, false, none, 151)).unwrap(),
            PumpClass::PumpCurve
        );
        assert_eq!(
            classify(
                PUMP_PROGRAM_ID,
                curve(true, 279_900_000_000_000, false, none, 151)
            )
            .unwrap(),
            PumpClass::PumpMigrating
        );
        assert_eq!(
            classify(PUMP_PROGRAM_ID, curve(true, 0, false, none, 150)).unwrap(),
            PumpClass::GraduatedElsewhere
        );
        assert!(PumpClass::PumpCurve.pump_venue() && !PumpClass::GraduatedElsewhere.pump_venue());
        // Not pump's, or not a curve: not a pump coin.
        assert_eq!(
            classify(Pubkey::new_unique(), curve(false, 1, false, none, 151)).unwrap(),
            PumpClass::NotPump
        );
        let mut other = curve(false, 1, false, none, 151);
        other[0] ^= 1;
        assert_eq!(
            classify(PUMP_PROGRAM_ID, other).unwrap(),
            PumpClass::NotPump
        );
        // ADOPTED Q6, the length rule, an unparseable curve.
        assert_eq!(
            code(classify(PUMP_PROGRAM_ID, curve(true, 0, true, none, 151))),
            Some(u32::from(ShortwinError::CurveMayhemUnsupported))
        );
        assert_eq!(
            code(classify(
                PUMP_PROGRAM_ID,
                curve(true, 0, false, [7; 32], 151)
            )),
            Some(u32::from(ShortwinError::CurveQuoteNotSol))
        );
        assert_eq!(
            code(classify(PUMP_PROGRAM_ID, curve(true, 0, false, none, 114))),
            Some(u32::from(ShortwinError::CurveNeedsExtend))
        );
        let mut bad = curve(true, 0, false, none, 151);
        bad[pump::OFF_COMPLETE] = 2;
        assert_eq!(
            code(classify(PUMP_PROGRAM_ID, bad)),
            Some(u32::from(ShortwinError::PumpVenuePrecedence))
        );
    }

    /// pump-final §6.3 (owner D1 (a), C2): a **live** cashback curve is refused
    /// (`CurveCashbackCoin` = 6109: pump's v3 trades refuse it with 6094), a live curve of
    /// depth ≠ 0 is not a SOL curve, and both refusals sit on the live branch only — a complete
    /// cashback curve still classifies as `PumpMigrating` (no pool yet) or `PumpSwap` (its
    /// canonical pool is valid), so a graduated cashback coin lists on PumpSwap.
    #[test]
    fn a_live_cashback_curve_is_refused_and_a_graduated_one_is_not() {
        let none = [0u8; 32];
        let with = |complete: bool, vt: u64, cashback: bool, depth: u8| {
            let mut d = curve(complete, vt, false, none, pump::CURVE_LEN_2026_10_08);
            d[pump::OFF_IS_CASHBACK_COIN] = u8::from(cashback);
            d[pump::OFF_DEPTH] = depth;
            d
        };
        assert_eq!(
            code(classify(PUMP_PROGRAM_ID, with(false, 1, true, 0))),
            Some(u32::from(ShortwinError::CurveCashbackCoin))
        );
        assert_eq!(u32::from(ShortwinError::CurveCashbackCoin), 6_109);
        assert_eq!(
            code(classify(PUMP_PROGRAM_ID, with(false, 1, false, 1))),
            Some(u32::from(ShortwinError::CurveQuoteNotSol))
        );
        assert_eq!(
            classify(PUMP_PROGRAM_ID, with(false, 1, false, 0)).unwrap(),
            PumpClass::PumpCurve
        );
        // Complete and unmigrated: the pause, observed and priced as any migrating curve.
        assert_eq!(
            classify(PUMP_PROGRAM_ID, with(true, 279_900_000_000_000, true, 0)).unwrap(),
            PumpClass::PumpMigrating
        );
        // Complete with a valid canonical pool: the graduated cashback coin lists on PumpSwap.
        let mint = Pubkey::new_unique();
        let (bk, ck) = (Pubkey::new_unique(), Pubkey::new_unique());
        let (mut bl, mut cl) = (1u64, 1u64);
        let mut bd = with(true, 0, true, 0);
        let mut cd = pool(&mint);
        let pamm = PUMP_AMM_PROGRAM_ID;
        let b = AccountInfo::new(&bk, false, false, &mut bl, &mut bd, &PUMP_PROGRAM_ID, false);
        let c = AccountInfo::new(&ck, false, false, &mut cl, &mut cd, &pamm, false);
        assert_eq!(classify_pump(&b, &c, &mint).unwrap(), PumpClass::PumpSwap);
    }

    /// A valid canonical pool of `mint` (index 0, created by the pool authority, base `mint`,
    /// quote WSOL, mayhem off), 301 bytes.
    fn pool(mint: &Pubkey) -> Vec<u8> {
        let mut d = vec![0u8; 301];
        d[..8].copy_from_slice(&pump_amm::POOL_DISCRIMINATOR);
        d[pump_amm::OFF_CREATOR..pump_amm::OFF_CREATOR + 32]
            .copy_from_slice(pool_authority(mint).as_ref());
        d[pump_amm::OFF_BASE_MINT..pump_amm::OFF_BASE_MINT + 32].copy_from_slice(mint.as_ref());
        d[pump_amm::OFF_QUOTE_MINT..pump_amm::OFF_QUOTE_MINT + 32]
            .copy_from_slice(WSOL_MINT.as_ref());
        d
    }

    /// The canonical-pool state and the class of a migrated curve (`vT = 0`) and of a completed
    /// one still migrating (`vT > 0`) with `cp` = (`owner`, `lamports`, `data`).
    fn with_pool(owner: Pubkey, lamports: u64, data: Vec<u8>) -> (CanonicalPool, [PumpClass; 2]) {
        let mint = Pubkey::new_unique();
        let (ck, bk) = (Pubkey::new_unique(), Pubkey::new_unique());
        let (mut cl, mut cd) = (lamports, data);
        let c = AccountInfo::new(&ck, false, false, &mut cl, &mut cd, &owner, false);
        let state = canonical_pool(&c, &mint, &pool_authority(&mint));
        assert_eq!(
            canonical_pool_valid(&c, &mint, &pool_authority(&mint)),
            state == CanonicalPool::Valid
        );
        let classes = [0u64, 279_900_000_000_000].map(|vt| {
            let mut bl = 1u64;
            let mut bd = curve(true, vt, false, [0; 32], 151);
            let b = AccountInfo::new(&bk, false, false, &mut bl, &mut bd, &PUMP_PROGRAM_ID, false);
            let class = classify_pump(&b, &c, &mint).unwrap();
            assert_eq!(
                classify_pump_with(&b, &c, &mint, &pool_authority(&mint)).unwrap(),
                class
            );
            class
        });
        (state, classes)
    }

    /// M5-SEC-5: only a System-owned address is "no canonical pool"; anything else that is not a
    /// valid pool — a pAMM account that does not parse (a changed discriminator, a bool byte
    /// other than 0/1, too short to hold a bool), a pool whose fields no longer describe this
    /// mint's canonical SOL pool (a shifted layout), a mayhem pool, an owner that cannot have
    /// created it — is `Unreadable`: a pump venue for D1, never "graduated elsewhere".
    #[test]
    fn an_unreadable_canonical_pool_is_never_absent() {
        let system = anchor_lang::system_program::ID;
        let pamm = PUMP_AMM_PROGRAM_ID;
        // Absent: nothing there, or only lamports.
        for lamports in [0, 5_000] {
            let (state, classes) = with_pool(system, lamports, Vec::new());
            assert_eq!(state, CanonicalPool::Absent);
            assert!(!state.exists());
            assert_eq!(
                classes,
                [PumpClass::GraduatedElsewhere, PumpClass::PumpMigrating]
            );
        }
        // Valid.
        let mint_free = |f: &dyn Fn(&mut Vec<u8>, &Pubkey)| {
            let mint = Pubkey::new_unique();
            let mut d = pool(&mint);
            f(&mut d, &mint);
            let (ck, mut cl) = (Pubkey::new_unique(), 1u64);
            let c = AccountInfo::new(&ck, false, false, &mut cl, &mut d, &pamm, false);
            canonical_pool(&c, &mint, &pool_authority(&mint))
        };
        assert_eq!(mint_free(&|_, _| {}), CanonicalPool::Valid);
        // Unreadable: every way a pAMM account there can fail to be a valid canonical pool.
        type Change<'a> = (&'a str, &'a dyn Fn(&mut Vec<u8>, &Pubkey));
        let unreadable: [Change; 8] = [
            ("discriminator", &|d, _| d[0] ^= 0xff),
            ("mayhem bool 2", &|d, _| d[pump_amm::OFF_IS_MAYHEM_MODE] = 2),
            ("too short", &|d, _| d.truncate(8)),
            ("index 1", &|d, _| d[pump_amm::OFF_INDEX] = 1),
            ("creator", &|d, _| d[pump_amm::OFF_CREATOR] ^= 1),
            ("base mint", &|d, _| d[pump_amm::OFF_BASE_MINT] ^= 1),
            ("quote mint", &|d, _| d[pump_amm::OFF_QUOTE_MINT] ^= 1),
            ("mayhem", &|d, _| d[pump_amm::OFF_IS_MAYHEM_MODE] = 1),
        ];
        for (what, f) in unreadable {
            assert_eq!(mint_free(f), CanonicalPool::Unreadable, "{what}");
        }
        let (state, classes) = with_pool(pamm, 1, vec![7u8; 301]);
        assert_eq!(state, CanonicalPool::Unreadable);
        assert!(state.exists());
        assert_eq!(classes, [PumpClass::PumpPoolUnreadable; 2]);
        assert!(PumpClass::PumpPoolUnreadable.pump_venue());
        // An owner that cannot have created an account at pAMM's PDA: fail closed as well.
        let (state, _) = with_pool(Pubkey::new_unique(), 1, Vec::new());
        assert_eq!(state, CanonicalPool::Unreadable);
    }
}
