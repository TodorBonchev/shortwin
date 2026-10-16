//! Graduation duties (ARCHITECTURE §2.5 "On → PumpSwap", §5.1, §7.1 duty 6; ADOPTED Q14, Q16;
//! A14, A15; M5, M6; pump-final §7): what the keeper sends when a curve completes and when its
//! canonical PumpSwap pool appears.
//!
//! **How a curve completes now** (pump-final §1, §5.2; owner D1 (a), D2, D3). short.win's curve
//! adapter trades pump v3, which buys past the curve's remaining real tokens `rT` by pump's
//! synthetic migration: a liquidatable or expired position whose `Q` exceeds `rT` exits like any
//! other ([`crate::decide`]), its `liquidate` / `expire` buying `rT` on the curve and `Q − rT` from
//! the would-be pool in one `buy_v3`, priced `B̂_v3`, and the market moves to `PumpMigrating` in
//! that instruction. The 3-transaction Jito straddle bundle (`complete_curve` of the retired
//! `complete-curve` helper, `migrate_v2`, the transition) and its keeper fallback are gone; the
//! keeper is no longer the completer of last resort (ECON §10.1). A curve completed by anyone —
//! the keeper's own synthetic exit, a borrower's synthetic close, a third party's buy — goes
//! through the two duties below.
//!
//! **`migrate_v2` as soon as a curve completes** ([`migration_due`], [`migrate_v2_ix`];
//! `graduation.migrate`, on by default): a covered `PumpCurve` or `PumpMigrating` market whose
//! curve is complete with `vT > 0` and whose canonical PumpSwap pool does not exist yet — at the
//! start of the market's tick, and again right after a keeper exit of that tick completed the
//! curve. pump's `migrate_v2` is permissionless and idempotent (R-PUMP §7.4); it is sent
//! **top-level, alone with its compute budget** (A15: a CPI from short.win would reach stack height
//! 5; 51 trace entries), with its 27 IDL accounts plus the two boost accounts mainnet passes
//! (R-PUMP §3.6). After a synthetic completion it seeds the pool with the live `curve_base_ata`
//! and `rQ + pcQ − fee` (pump-final V3, §5.4): the same instruction.
//!
//! **The `→ PumpSwap` transition** ([`transition_due`], [`transition_ixs`]; M6): as soon as a
//! curve market's canonical pool is valid (the keeper's own `migrate_v2`, or anyone's), the keeper
//! sends one transaction: the accounts the pool's trades require that do not exist yet (ARCH
//! §3.12's existence requirement, [`crate::venue::pamm_account_keys`]: the trader's pAMM UVA —
//! PumpSwap `init_user_volume_accumulator(payer = keeper, user = trader)` —, the coin-creator
//! vault's WSOL ATA, the two recipients' WSOL ATAs, on a cashback coin the trader's cashback ATA;
//! `crank_price` creates nothing), then `crank_price` on the curve's ReadSet (the program's
//! prologue moves the market onto the pool: the ring rewritten to the pool's `lp_supply`, the
//! post-change warm-up `ema_warm = 8`, `ema_guard = 1`; this instruction records no observation),
//! then `crank_price` on the **PumpSwap** ReadSet: the pool's first observation, in the same
//! transaction — the first bucket of the warm-up is the keeper's own read of the pool, and the
//! warm-up ends one second earlier. No `extend_account`: a fresh pool (287 bytes since pump's
//! 2026-10-02 redeploy, 271 before) trades as it is (R-VEN Update G4), and the adapter reads it.
//! A market that is already `PumpSwap` (someone else cranked the transition, or a PumpSwap
//! listing) gets the missing accounts alone ([`pamm_account_ixs`]). The market's other positions
//! exit on the pool at the post-change warm-up's gate (`liquidate` and `expire` refuse
//! `EmaWarmingUp` until eight post-transition buckets are integrated: `T + 8` at the earliest).
//!
//! **A list-only market back on a pump venue, and the trader's accounts** ([`redetection_due`],
//! [`trader_account_ixs`]; ARCH §2.5 `None` row): a list-only market whose detection set shows a
//! live or migrating curve or a valid canonical pool (the program's own `classify_pump`) is
//! cranked, which moves it onto that venue; `crank_price` has no payer and creates no account, so
//! the keeper then creates the trader accounts the legs require — on the curve its base ATA and
//! pump UVA (T8 and T6 of the v3 list), on PumpSwap the pAMM accounts above.

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use anyhow::{Context, anyhow, bail};
use shortwin::constants::{
    ASSOCIATED_TOKEN_PROGRAM_ID, PUMP_AMM_EVENT_AUTHORITY, PUMP_AMM_GLOBAL_CONFIG,
    PUMP_AMM_PROGRAM_ID, PUMP_EVENT_AUTHORITY, PUMP_GLOBAL, PUMP_PROGRAM_ID, TOKEN_2022_PROGRAM_ID,
    TOKEN_PROGRAM_ID, WSOL_MINT,
};
use shortwin::state::{Market, VenueKind};
use shortwin::venues::detect::PumpClass;
use shortwin::venues::{pump_amm, pump_curve};
use shortwin_math::quote::sol_leg;
use shortwin_services_common::AccountData;
use shortwin_venue_state::{pump, pump_amm as pamm_state};

use crate::venue::{self, PammAccount};
use crate::view::MarketView;

/// pump `migrate_v2`'s discriminator (`idl/pump.json`, R-PUMP §3.6).
pub const PUMP_MIGRATE_V2: [u8; 8] = [0xbb, 0xcb, 0x12, 0x1f, 0xce, 0xed, 0xfe, 0x29];

/// The Rent sysvar `migrate_v2` names.
pub const SYSVAR_RENT: Pubkey =
    anchor_lang::prelude::pubkey!("SysvarRent111111111111111111111111111111111");

// ---- keys -----------------------------------------------------------------------------------

/// `ATA(owner, mint)` under `token_program`.
pub fn ata(owner: &Pubkey, mint: &Pubkey, token_program: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[owner.as_ref(), token_program.as_ref(), mint.as_ref()],
        &ASSOCIATED_TOKEN_PROGRAM_ID,
    )
    .0
}

/// The canonical PumpSwap pool `migrate_v2` creates for a SOL curve's mint and its accounts
/// (R-VEN §1.3, R-PUMP §3.6; the harness's `pump::NewPool` derives the same).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MigrationKeys {
    /// `PDA(pump, ["pool-authority", mint])`: the pool's creator.
    pub pool_authority: Pubkey,
    /// `PDA(pAMM, ["pool", 0u16, pool_authority, mint, WSOL])` (the market's `canonical_pool`).
    pub pool: Pubkey,
    /// `PDA(pAMM, ["pool_lp_mint", pool])`.
    pub lp_mint: Pubkey,
    /// `ATA(pool, mint)` and `ATA(pool, WSOL)`: the pool's vaults.
    pub base_vault: Pubkey,
    pub quote_vault: Pubkey,
    /// `PDA(pAMM, ["boost_vault", pool])` and its WSOL ATA (the two remaining accounts).
    pub boost_vault: Pubkey,
    pub boost_vault_ata: Pubkey,
}

impl MigrationKeys {
    /// The keys of `mint`'s migration (base token program `base_program`).
    pub fn of(mint: &Pubkey, base_program: &Pubkey) -> Self {
        let pool_authority = Pubkey::find_program_address(
            &[pump::POOL_AUTHORITY_SEED, mint.as_ref()],
            &PUMP_PROGRAM_ID,
        )
        .0;
        let pool = Pubkey::find_program_address(
            &[
                b"pool",
                &0u16.to_le_bytes(),
                pool_authority.as_ref(),
                mint.as_ref(),
                WSOL_MINT.as_ref(),
            ],
            &PUMP_AMM_PROGRAM_ID,
        )
        .0;
        let lp_mint =
            Pubkey::find_program_address(&[b"pool_lp_mint", pool.as_ref()], &PUMP_AMM_PROGRAM_ID).0;
        let boost_vault = Pubkey::find_program_address(
            &[pamm_state::BOOST_VAULT_SEED, pool.as_ref()],
            &PUMP_AMM_PROGRAM_ID,
        )
        .0;
        Self {
            pool_authority,
            pool,
            lp_mint,
            base_vault: ata(&pool, mint, base_program),
            quote_vault: ata(&pool, &WSOL_MINT, &TOKEN_PROGRAM_ID),
            boost_vault,
            boost_vault_ata: ata(&boost_vault, &WSOL_MINT, &TOKEN_PROGRAM_ID),
        }
    }
}

fn w(k: Pubkey) -> AccountMeta {
    AccountMeta::new(k, false)
}

fn r(k: Pubkey) -> AccountMeta {
    AccountMeta::new_readonly(k, false)
}

/// The pump `Global` of a curve market's ReadSet accounts, parsed.
fn global_of(read: &[Option<AccountData>]) -> anyhow::Result<&AccountData> {
    read.get(pump_curve::C_GLOBAL)
        .and_then(Option::as_ref)
        .context("pump Global not read")
}

// ---- instructions ---------------------------------------------------------------------------

/// pump `migrate_v2` of `market`'s curve by `user` (anyone, R-PUMP §7.4): the 27 IDL accounts
/// (`idl/pump.json`) — `withdraw_authority` from the live `Global` (not a signer) — plus the boost
/// vault and its WSOL ATA; `read` is the market's ReadSet accounts.
pub fn migrate_v2_ix(
    user: &Pubkey,
    market: &Market,
    read: &[Option<AccountData>],
) -> anyhow::Result<Instruction> {
    let g = global_of(read)?;
    let global =
        pump::read_global(&g.owner.to_bytes(), &g.data).map_err(|e| anyhow!("Global: {e:?}"))?;
    let withdraw_authority = Pubkey::new_from_array(global.withdraw_authority());
    let mint = market.mint;
    let base = market.token_program;
    let k = MigrationKeys::of(&mint, &base);
    if k.pool != market.canonical_pool {
        bail!(
            "the derived canonical pool {} is not the market's {}",
            k.pool,
            market.canonical_pool
        );
    }
    let accounts = vec![
        r(PUMP_GLOBAL),
        w(withdraw_authority),
        r(mint),
        r(WSOL_MINT),
        w(market.bonding_curve),
        w(market.curve_base_ata),
        w(market.curve_quote_ata),
        AccountMeta::new(*user, true),
        r(anchor_lang::system_program::ID),
        r(PUMP_AMM_PROGRAM_ID),
        w(k.pool),
        w(k.pool_authority),
        w(ata(&k.pool_authority, &mint, &base)),
        w(ata(&k.pool_authority, &WSOL_MINT, &TOKEN_PROGRAM_ID)),
        r(PUMP_AMM_GLOBAL_CONFIG),
        w(k.lp_mint),
        w(ata(&k.pool_authority, &k.lp_mint, &TOKEN_2022_PROGRAM_ID)),
        w(k.base_vault),
        w(k.quote_vault),
        r(base),
        r(TOKEN_PROGRAM_ID),
        r(TOKEN_2022_PROGRAM_ID),
        r(ASSOCIATED_TOKEN_PROGRAM_ID),
        r(PUMP_AMM_EVENT_AUTHORITY),
        r(SYSVAR_RENT),
        r(PUMP_EVENT_AUTHORITY),
        r(PUMP_PROGRAM_ID),
        r(k.boost_vault),
        w(k.boost_vault_ata),
    ];
    Ok(Instruction {
        program_id: PUMP_PROGRAM_ID,
        accounts,
        data: PUMP_MIGRATE_V2.to_vec(),
    })
}

/// `create_associated_token_account_idempotent(payer, owner, mint)` under `token_program`.
pub fn create_ata_idempotent_ix(
    payer: &Pubkey,
    owner: &Pubkey,
    mint: &Pubkey,
    token_program: &Pubkey,
) -> Instruction {
    Instruction {
        program_id: ASSOCIATED_TOKEN_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*payer, true),
            w(ata(owner, mint, token_program)),
            r(*owner),
            r(*mint),
            r(anchor_lang::system_program::ID),
            r(*token_program),
        ],
        data: vec![1],
    }
}

/// pump `init_user_volume_accumulator(payer, user)` (R-PUMP §3.6; ARCH §3.2 "Effects"):
/// `[payer S W, user, user_volume_accumulator W, system, event_authority, program]`. Anyone may
/// create the UVA of any user (payer ≠ user); pump refuses a second creation.
pub fn pump_init_uva_ix(payer: &Pubkey, user: &Pubkey) -> Instruction {
    Instruction {
        program_id: PUMP_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*payer, true),
            r(*user),
            w(pump_curve::user_volume_accumulator(user)),
            r(anchor_lang::system_program::ID),
            r(PUMP_EVENT_AUTHORITY),
            r(PUMP_PROGRAM_ID),
        ],
        data: pump_curve::IX_INIT_USER_VOLUME_ACCUMULATOR.to_vec(),
    }
}

/// The trader accounts a curve market's legs require (ARCH §3.12 PumpCurve; T8 and T6 of the v3
/// list, T18 and T16 of 1.0.0's): the
/// trader's base ATA and its pump UVA, in that order. `create_market` creates both; `crank_price`'s
/// `None →` pump re-detection creates neither (it has no payer), so the keeper does
/// ([`trader_account_ixs`]).
pub fn trader_account_keys(market: &Market) -> [Pubkey; 2] {
    [market.trader_base_ata, market.pump_uva]
}

/// The instructions that create the trader accounts of [`trader_account_keys`] `missing` marks,
/// paid by `payer`: `create_idempotent(trader, mint)` and pump `init_user_volume_accumulator(payer,
/// user = trader)`.
pub fn trader_account_ixs(payer: &Pubkey, market: &Market, missing: [bool; 2]) -> Vec<Instruction> {
    let mut v = Vec::new();
    if missing[0] {
        v.push(create_ata_idempotent_ix(
            payer,
            &market.trader,
            &market.mint,
            &market.token_program,
        ));
    }
    if missing[1] {
        v.push(pump_init_uva_ix(payer, &market.trader));
    }
    v
}

// ---- decisions ------------------------------------------------------------------------------

/// `migrate_v2` is due on `mv` (module docs): a curve market whose curve is complete with
/// `vT > 0` (not migrated) and whose canonical pool is not valid yet.
pub fn migration_due(mv: &MarketView) -> bool {
    matches!(mv.kind, VenueKind::PumpCurve | VenueKind::PumpMigrating)
        && mv
            .state
            .curve
            .as_ref()
            .is_some_and(|c| c.migrating() && !c.canonical_pool_valid())
}

/// A list-only market whose detection set shows a live or migrating pump curve or a valid
/// canonical PumpSwap pool (the program's `classify_pump` on this tick's accounts,
/// [`crate::pool_monitor::classify`]): `crank_price` moves it onto that venue (ARCH §2.5 `None`
/// row; M5, M6's `None → PumpSwap`), so the keeper cranks it.
pub fn redetection_due(mv: &MarketView) -> bool {
    mv.kind == VenueKind::None
        && matches!(
            crate::pool_monitor::classify(&mv.market, &mv.read_accounts),
            Ok(PumpClass::PumpCurve | PumpClass::PumpMigrating | PumpClass::PumpSwap)
        )
}

/// What buying out `mv`'s live curve would cost: `rT` and `buy_exact_out(rT)` on the live
/// `(vT, vQ)` at the live curve fees (ε₁ = 1, ≥ pump's own charge; ECON §3.1) — how far a buy is
/// from completing it (M7-SEC-8's hot-curve read classification, `crate::keeper`). `None` off a
/// live curve or when a fee input does not parse.
pub fn completion_quote(mv: &MarketView) -> Option<(u64, u64)> {
    let c = mv.state.curve.as_ref()?;
    if c.live.complete || c.live.rt == 0 {
        return None;
    }
    let fees = c.pricing.as_ref()?.fees;
    let rt = u64::try_from(c.live.rt).ok()?;
    let q = sol_leg::buy_exact_out(rt, c.live.vt, c.live.vq, true, &fees)?;
    Some((rt, q.cost))
}

/// The `→ PumpSwap` transition is due on `mv` (module docs; ARCH §7.1 duty 6 "on the canonical
/// pool's appearance"): a `PumpCurve` or `PumpMigrating` market whose canonical pool is valid.
/// Its next prologue moves it onto the pool; until then its exits refuse (`VenueMigrating`).
pub fn transition_due(mv: &MarketView) -> bool {
    matches!(mv.kind, VenueKind::PumpCurve | VenueKind::PumpMigrating)
        && mv
            .state
            .curve
            .as_ref()
            .is_some_and(shortwin::venues::CurveVenue::canonical_pool_valid)
}

/// The accounts `mv`'s PumpSwap trades require ([`venue::pamm_account_keys`]): on a PumpSwap
/// market from its live pool (P0: the coin creator, the cashback flag) and `GlobalConfig` (P3); on
/// a curve market about to graduate from the valid canonical pool if its ReadSet shows one, else
/// from the live curve (`migrate_v2` gives the pool the curve's creator and cashback flag), and
/// PumpSwap's `GlobalConfig` in the curve's ReadSet (C6).
pub fn pamm_accounts_of(mv: &MarketView) -> anyhow::Result<Vec<(Pubkey, PammAccount)>> {
    let m = &mv.market;
    let acc = |i: usize| mv.read_accounts.get(i).and_then(Option::as_ref);
    match mv.kind {
        VenueKind::PumpSwap => {
            let pool = venue::pamm_pool(&mv.read_accounts)?;
            let recipients = venue::pamm_recipients(m, acc(pump_amm::P_GLOBAL_CONFIG))?;
            Ok(venue::pamm_account_keys(
                m,
                &Pubkey::new_from_array(pool.coin_creator),
                recipients,
                pool.is_cashback_coin,
            ))
        }
        VenueKind::PumpCurve | VenueKind::PumpMigrating => {
            let recipients = venue::pamm_recipients(m, acc(pump_curve::C_PAMM_GLOBAL_CONFIG))?;
            let (creator, cashback) = match mv.state.curve.as_ref().and_then(|c| c.pool.as_ref()) {
                Some(pool) => (pool.coin_creator, pool.is_cashback_coin),
                None => {
                    let c = acc(pump_curve::C_CURVE).context("bonding curve not read")?;
                    let curve = pump::read_curve(&c.owner.to_bytes(), &c.data)
                        .map_err(|e| anyhow!("bonding curve: {e:?}"))?;
                    (curve.creator, curve.is_cashback_coin)
                }
            };
            Ok(venue::pamm_account_keys(
                m,
                &Pubkey::new_from_array(creator),
                recipients,
                cashback,
            ))
        }
        other => bail!("venue {other:?} has no PumpSwap accounts"),
    }
}

/// PumpSwap `init_user_volume_accumulator(payer, user)` (R-VEN Update H6: any payer, `user`
/// unsigned): `[payer S W, user, user_volume_accumulator W, system, event_authority, program]`.
pub fn pamm_init_uva_ix(payer: &Pubkey, user: &Pubkey) -> Instruction {
    Instruction {
        program_id: PUMP_AMM_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*payer, true),
            r(*user),
            w(pump_amm::user_volume_accumulator(user)),
            r(anchor_lang::system_program::ID),
            r(PUMP_AMM_EVENT_AUTHORITY),
            r(PUMP_AMM_PROGRAM_ID),
        ],
        data: pump_amm::IX_INIT_USER_VOLUME_ACCUMULATOR.to_vec(),
    }
}

/// The instructions that create `missing` (of [`pamm_accounts_of`]) for `market`, paid by `payer`.
pub fn pamm_account_ixs(
    payer: &Pubkey,
    market: &Market,
    missing: &[(Pubkey, PammAccount)],
) -> Vec<Instruction> {
    missing
        .iter()
        .map(|(_, what)| match what {
            PammAccount::Uva => pamm_init_uva_ix(payer, &market.trader),
            PammAccount::Ata {
                owner,
                mint,
                token_program,
            } => create_ata_idempotent_ix(payer, owner, mint, token_program),
        })
        .collect()
}

/// The PumpSwap ReadSet of a curve market's canonical pool before the transition wrote the
/// market's cache (`pump_amm::read_keys` on the cache the curve listing precomputed; the pool's
/// vaults are ATAs of the pool, `migrate_v2` creates them so — [`MigrationKeys`]; the LP mint P6 is
/// the pool's derived `pool_lp_mint`, M6R-1; the program key-checks every slot).
pub fn pamm_read_keys_of(market: &Market) -> [Pubkey; pump_amm::READ_LEN] {
    let k = MigrationKeys::of(&market.mint, &market.token_program);
    let mut keys = pump_amm::read_keys(market);
    for (slot, derived) in [
        (pump_amm::P_POOL, k.pool),
        (pump_amm::P_BASE_VAULT, k.base_vault),
        (pump_amm::P_QUOTE_VAULT, k.quote_vault),
        (pump_amm::P_BOOST_VAULT_ATA, k.boost_vault_ata),
    ] {
        if let Some(key) = keys.get_mut(slot)
            && *key == Pubkey::default()
        {
            *key = derived;
        }
    }
    keys
}

/// The transition transaction's instructions for `mv`'s curve market (module docs): the
/// `missing` pool accounts, `crank_price` on the curve's ReadSet (`→ PumpSwap`), then
/// `crank_price` on the PumpSwap ReadSet (the pool's first observation): the keeper's own
/// transition after a `migrate_v2`, whoever migrated.
pub fn transition_ixs(
    payer: &Pubkey,
    mv: &MarketView,
    missing: &[(Pubkey, PammAccount)],
) -> Vec<Instruction> {
    let m = &mv.market;
    let mut v = pamm_account_ixs(payer, m, missing);
    v.push(crate::ixs::crank_price_with(
        &mv.key,
        &m.mint,
        &pump_curve::read_keys(m),
    ));
    v.push(crate::ixs::crank_price_with(
        &mv.key,
        &m.mint,
        &pamm_read_keys_of(m),
    ));
    v
}

// ---- records --------------------------------------------------------------------------------

/// One `migrate_v2` attempt of the keeper.
#[derive(Clone, Debug)]
pub struct MigrateRecord {
    pub market: Pubkey,
    pub result: crate::keeper::ExitResult,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `migrate_v2` names 29 accounts, `user` the only signer, `withdraw_authority` from the live
    /// `Global`, the canonical pool the market caches, and the discriminator
    /// `sha256("global:migrate_v2")[..8]` (the fork scenario checks the whole list against the
    /// harness's own derivation, `graduation::keeper_migrates_a_completed_curve`).
    #[test]
    fn migrate_v2_names_the_canonical_pool() {
        let mint = Pubkey::new_unique();
        let k = MigrationKeys::of(&mint, &TOKEN_2022_PROGRAM_ID);
        let mut m: Market = bytemuck::Zeroable::zeroed();
        m.mint = mint;
        m.token_program = TOKEN_2022_PROGRAM_ID;
        m.canonical_pool = k.pool;
        m.bonding_curve = Pubkey::new_unique();
        let mut g = vec![0u8; 1_087];
        g[..8].copy_from_slice(&[0xa7, 0xe8, 0xe8, 0xb1, 0xc8, 0x6c, 0x72, 0x7f]);
        let wa = Pubkey::new_unique();
        g[113..145].copy_from_slice(wa.as_ref());
        let mut read: Vec<Option<AccountData>> = vec![None; pump_curve::READ_LEN];
        read[pump_curve::C_GLOBAL] = Some(AccountData {
            lamports: 1,
            owner: PUMP_PROGRAM_ID,
            executable: false,
            data: g,
        });
        let user = Pubkey::new_unique();
        let ix = migrate_v2_ix(&user, &m, &read).unwrap();
        assert_eq!(ix.accounts.len(), 29);
        assert_eq!(ix.accounts[1].pubkey, wa);
        assert!(!ix.accounts[1].is_signer);
        assert_eq!(ix.accounts[7].pubkey, user);
        let signers: Vec<usize> = ix
            .accounts
            .iter()
            .enumerate()
            .filter(|(_, a)| a.is_signer)
            .map(|(i, _)| i)
            .collect();
        assert_eq!(signers, [7]);
        assert_eq!(ix.accounts[10].pubkey, k.pool);
        assert_eq!(ix.accounts[27].pubkey, k.boost_vault);
        let h = solana_sha256_hasher::hash(b"global:migrate_v2");
        assert_eq!(PUMP_MIGRATE_V2, h.to_bytes()[..8]);
        // A market whose cached pool is not the derived one is refused.
        m.canonical_pool = Pubkey::new_unique();
        assert!(migrate_v2_ix(&user, &m, &read).is_err());
    }

    /// The PumpSwap ReadSet tx3's second crank names before the transition wrote the cache: the
    /// curve listing's precomputed cache (`pump_amm::precompute`) — the same keys `migrate_v2`
    /// creates ([`MigrationKeys`]) — and, on a market whose cache is empty, the derivation itself.
    #[test]
    fn the_pool_read_set_before_the_transition_is_the_migrations() {
        use shortwin::constants::{PUMP_AMM_FEE_CONFIG, PUMP_AMM_GLOBAL_CONFIG};
        for program in [TOKEN_PROGRAM_ID, TOKEN_2022_PROGRAM_ID] {
            let mint = Pubkey::new_unique();
            let k = MigrationKeys::of(&mint, &program);
            let mut m: Market = bytemuck::Zeroable::zeroed();
            m.mint = mint;
            m.token_program = program;
            m.canonical_pool = k.pool;
            m.trader = Pubkey::new_unique();
            let want = [
                k.pool,
                k.base_vault,
                k.quote_vault,
                PUMP_AMM_GLOBAL_CONFIG,
                PUMP_AMM_FEE_CONFIG,
                k.boost_vault_ata,
                // M6R-1: the LP mint `create_pool` derives for the pool.
                pump_amm::lp_mint(&k.pool),
            ];
            assert_eq!(pamm_read_keys_of(&m), want, "empty cache: derived");
            pump_amm::precompute(&mut m, &Pubkey::new_unique()).unwrap();
            assert_eq!(pump_amm::read_keys(&m), want, "the listing's precompute");
            assert_eq!(pamm_read_keys_of(&m), want);
        }
    }

    /// tx3 / the transition transaction: the missing accounts (PumpSwap's UVA init with the keeper
    /// paying and the trader unsigned; `create_idempotent` of each WSOL ATA), then `crank_price`
    /// on the curve's ReadSet, then on the pool's; nothing signs but the payer.
    #[test]
    fn the_transition_creates_the_missing_accounts_then_cranks_twice() {
        let mint = Pubkey::new_unique();
        let k = MigrationKeys::of(&mint, &TOKEN_PROGRAM_ID);
        let mut m: Market = bytemuck::Zeroable::zeroed();
        m.mint = mint;
        m.token_program = TOKEN_PROGRAM_ID;
        m.canonical_pool = k.pool;
        m.trader = Pubkey::new_unique();
        m.bonding_curve = Pubkey::new_unique();
        m.curve_base_ata = Pubkey::new_unique();
        let owner = Pubkey::new_unique();
        let mv = MarketView {
            key: Pubkey::new_unique(),
            market: Box::new(m),
            vault: Box::new(shortwin_math::pool::Vault::new()),
            kind: VenueKind::PumpCurve,
            read_keys: pump_curve::read_keys(&m).to_vec(),
            read_accounts: vec![None; pump_curve::READ_LEN],
            state: shortwin::venues::VenueState::not_executable(VenueKind::PumpCurve),
            positions: Vec::new(),
            extras: None,
            supply: None,
        };
        let payer = Pubkey::new_unique();
        let uva = pump_amm::user_volume_accumulator(&m.trader);
        let missing = [
            (uva, PammAccount::Uva),
            (
                pump_curve::wsol_ata(&owner),
                PammAccount::Ata {
                    owner,
                    mint: WSOL_MINT,
                    token_program: TOKEN_PROGRAM_ID,
                },
            ),
        ];
        let v = transition_ixs(&payer, &mv, &missing);
        assert_eq!(v.len(), 4);
        assert_eq!(v[0].program_id, PUMP_AMM_PROGRAM_ID);
        assert_eq!(v[0].data, pump_amm::IX_INIT_USER_VOLUME_ACCUMULATOR);
        assert_eq!(v[0].accounts[1].pubkey, m.trader);
        assert!(!v[0].accounts[1].is_signer);
        assert_eq!(v[0].accounts[2].pubkey, uva);
        assert_eq!(v[1].program_id, ASSOCIATED_TOKEN_PROGRAM_ID);
        assert_eq!(v[1].accounts[1].pubkey, pump_curve::wsol_ata(&owner));
        let tail = |ix: &Instruction| {
            ix.accounts[ix.accounts.len() - 7..]
                .iter()
                .map(|a| a.pubkey)
                .collect::<Vec<_>>()
        };
        assert_eq!(v[2].program_id, shortwin::ID);
        assert_eq!(tail(&v[2]), pump_curve::read_keys(&m).to_vec());
        assert_eq!(v[3].program_id, shortwin::ID);
        let pool_keys = pamm_read_keys_of(&m);
        assert_eq!(
            v[3].accounts[v[3].accounts.len() - pump_amm::READ_LEN..]
                .iter()
                .map(|a| a.pubkey)
                .collect::<Vec<_>>(),
            pool_keys.to_vec()
        );
        for ix in &v {
            assert!(
                ix.accounts
                    .iter()
                    .all(|a| !a.is_signer || a.pubkey == payer)
            );
        }
        // Nothing missing: the two cranks alone.
        assert_eq!(transition_ixs(&payer, &mv, &[]).len(), 2);
    }
}
