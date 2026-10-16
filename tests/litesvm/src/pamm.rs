//! PumpSwap **markets** on the mainnet fork (M6; ARCH §2.4.1 PumpSwap row, §2.5 "On → PumpSwap",
//! §3.2, §3.12 PumpSwap, §5.4): the release short.win trading against the **dumped** PumpSwap
//! (with pump and pump-fees) programs and canonical pools of [`crate::fork`] — groups `pamm_state`
//! and `pamm_synthetic` (R-VEN Update 2026-09-30 H10: five pool roles at slot 451,784,607 and the
//! synthetic negative-VQR pool), and the graduated coins of `pump_state` — or a pool a real
//! `migrate_v2` just created.
//!
//! - [`Env::fork_pamm_coin`] loads a [`PammCoin`] (release short.win, the pump programs and
//!   statics, PumpSwap's statics, the role's accounts, mainnet's Rent, the `pamm_state` clock);
//!   [`Env::plant_negative_vqr`] replaces the `mature` pool with its synthetic negative-VQR copy.
//! - [`PammMarket`]: a market's keys and its venue's account lists in ARCH §3.12's order (ReadSet
//!   P0–P6 — the LP mint P6 since M6R-1 —, TradeExtras T0–T18), built from the fixture's or the
//!   live pool's keys and the
//!   harness's own derivations, never from the program's key functions; it implements
//!   [`VenueMarket`], so every builder of [`crate::engine`] works on it. [`PammMarket::graduated`]
//!   is a curve market's after its `→ PumpSwap` transition.
//! - Listing: [`create_pamm_market_ix`] (`create_market(Auto)` with the pAMM init set) and
//!   [`Env::pamm_market`]; the existence requirement's accounts ([`venue_account_ixs`]).
//! - PumpSwap's own instructions for the tests: [`pamm_trade_ix`] (`buy` / `sell` with R-VEN §2.2's
//!   exact account order), [`PammUser`] (a normal trader), [`pamm_init_uva_ix`],
//!   [`pamm_claim_cashback_ix`] (no signature of `user`, T-CASHBACK), [`pamm_liquidity_ix`]
//!   (`deposit` / `withdraw`, H11) and [`PammLp`] (a third-party liquidity provider, M6R-1),
//!   [`set_disable_flags`] (T-DISABLE), [`sweep_trader_pamm_ix`]; [`PammPool::locked_depth`]
//!   recomputes the pool's burned-LP depth `D` (M6R-1).
//!
//! Every instruction goes through [`Env::send`] (the invariant hooks, which see PumpSwap markets
//! through `invariants::venue_view`).

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::{AccountMeta, Instruction};
use litesvm::LiteSVM;
use shortwin::constants::{
    ASSOCIATED_TOKEN_PROGRAM_ID, PUMP_AMM_EVENT_AUTHORITY, PUMP_AMM_FEE_CONFIG,
    PUMP_AMM_GLOBAL_CONFIG, PUMP_AMM_GLOBAL_VOLUME_ACCUMULATOR, PUMP_AMM_PROGRAM_ID,
    PUMP_EVENT_AUTHORITY, PUMP_FEES_PROGRAM_ID, PUMP_PROGRAM_ID, TOKEN_2022_PROGRAM_ID,
    TOKEN_PROGRAM_ID, WSOL_MINT,
};
use shortwin::instructions::create_market::VENUE_HINT_AUTO;
use shortwin_venue_state::{pump_amm, token_account};
use solana_keypair::Keypair;
use solana_signer::Signer;

use crate::engine::VenueMarket;
use crate::fixtures::MarketKeys;
use crate::fork;
use crate::ix::{self, LenderKeys};
use crate::pump::{PumpMarket, wsol_ata};
use crate::tokens::TokenProgram;
use crate::{Env, TxResult, errors, pda, read};

/// PumpSwap `buy`'s discriminator (R-VEN §2.1).
pub const PAMM_BUY: [u8; 8] = [0x66, 0x06, 0x3d, 0x12, 0x01, 0xda, 0xeb, 0xea];
/// PumpSwap `sell`'s discriminator.
pub const PAMM_SELL: [u8; 8] = [0x33, 0xe6, 0x85, 0xa4, 0x01, 0x7f, 0x83, 0xad];
/// `init_user_volume_accumulator` (the same Anchor name on pump and PumpSwap).
pub const PAMM_INIT_UVA: [u8; 8] = [0x5e, 0x06, 0xca, 0x73, 0xff, 0x60, 0xe8, 0xb7];
/// `claim_cashback` (the same Anchor name on pump and PumpSwap).
pub const PAMM_CLAIM_CASHBACK: [u8; 8] = [0x25, 0x3a, 0x23, 0x7e, 0xbe, 0x35, 0xe4, 0xc5];
/// `extend_account` (the same Anchor name on pump and PumpSwap).
pub const EXTEND_ACCOUNT: [u8; 8] = [0xea, 0x66, 0xc2, 0xcb, 0x96, 0x48, 0x3e, 0xe5];
/// `create_pool` (pAMM IDL `pump_amm.json`, the PPD refresh of 2026-09-12 — the deployed ELF's).
pub const PAMM_CREATE_POOL: [u8; 8] = [233, 146, 209, 142, 207, 104, 64, 188];
/// `GlobalConfig.disable_flags` (u8 @56).
pub const GC_DISABLE_FLAGS: usize = 56;
/// PumpSwap's `DisabledBuy` (T-DISABLE, bit 3).
pub const PAMM_ERR_DISABLED_BUY: u32 = 6020;
/// PumpSwap's `DisabledSell` (bit 4).
pub const PAMM_ERR_DISABLED_SELL: u32 = 6021;
/// The compute-unit limit the harness requests for PumpSwap opens and closes (a PumpSwap trade is
/// 79–92 k CU on the ELF, R-VEN Update H5, on top of short.win's own).
pub const PAMM_TRADE_CU: u32 = 300_000;

fn m(key: Pubkey, writable: bool) -> AccountMeta {
    if writable {
        AccountMeta::new(key, false)
    } else {
        AccountMeta::new_readonly(key, false)
    }
}

fn pamm_pda(seeds: &[&[u8]]) -> Pubkey {
    Pubkey::find_program_address(seeds, &PUMP_AMM_PROGRAM_ID).0
}

/// `PDA(pAMM, ["user_volume_accumulator", user])`.
pub fn pamm_uva(user: &Pubkey) -> Pubkey {
    pamm_pda(&[b"user_volume_accumulator", user.as_ref()])
}

/// `PDA(pAMM, ["creator_vault", coin_creator])` (underscore).
pub fn coin_creator_vault_authority(coin_creator: &Pubkey) -> Pubkey {
    pamm_pda(&[b"creator_vault", coin_creator.as_ref()])
}

/// `PDA(pAMM, ["pool-v2", mint])`.
pub fn pool_v2(mint: &Pubkey) -> Pubkey {
    pamm_pda(&[b"pool-v2", mint.as_ref()])
}

/// `PDA(pAMM, ["boost_vault", pool])`.
pub fn boost_vault(pool: &Pubkey) -> Pubkey {
    pamm_pda(&[b"boost_vault", pool.as_ref()])
}

fn global_config(svm: &LiteSVM) -> Vec<u8> {
    svm.get_account(&PUMP_AMM_GLOBAL_CONFIG)
        .expect("PumpSwap GlobalConfig")
        .data
}

/// Protocol-fee recipient `i` (0–7) of the live `GlobalConfig`.
pub fn protocol_fee_recipient(svm: &LiteSVM, i: usize) -> Pubkey {
    let d = global_config(svm);
    let g = pump_amm::read_global_config(&pump_amm::PROGRAM, &d).expect("GlobalConfig");
    Pubkey::new_from_array(g.protocol_fee_recipients()[i])
}

/// Buyback-fee recipient `i` (0–7) of the live `GlobalConfig`.
pub fn buyback_fee_recipient(svm: &LiteSVM, i: usize) -> Pubkey {
    let d = global_config(svm);
    let g = pump_amm::read_global_config(&pump_amm::PROGRAM, &d).expect("GlobalConfig");
    Pubkey::new_from_array(g.buyback_fee_recipients()[i])
}

/// Writes `GlobalConfig.disable_flags` (what the admin's `disable` writes, T-DISABLE; a byte-level
/// plant of the venue's account, as the Raydium admin builders).
pub fn set_disable_flags(svm: &mut LiteSVM, flags: u8) {
    fork::patch(svm, &PUMP_AMM_GLOBAL_CONFIG, |d| {
        d[GC_DISABLE_FLAGS] = flags
    });
}

// ---- the coin creator (M6-SEC-4) ----------------------------------------------------------------

/// Metaplex Token Metadata (`metaqbxx…`), whose metadata account pAMM's `set_coin_creator` reads.
pub const METAPLEX_PROGRAM_ID: Pubkey =
    anchor_lang::pubkey!("metaqbxxUerdq28cj1RbAWkYQm3ybzjb6a8bt518x1s");

/// The Metaplex metadata account of the `legacy211` mint (`G8ZmGTba…pump`), as mainnet holds it
/// (`getAccountInfo` at slot 451,858,756; owner Metaplex, 607 bytes, 3,733,800 lamports; key 4
/// `MetadataV1`, update authority pump's `TSLvdd1p…`, one creator `4E9s4kuE…` with a 100 % share).
/// It is not part of `tests/fixtures/` (no fixture group dumps metadata); the M6-SEC-4 tests plant
/// it, as mainnet state, for `set_coin_creator` only.
pub const LEGACY211_METADATA_B64: &str = "BAbFwc5jjSVn0mRosF65UdGijcxuEjSCtcZ1FJdw5ivy4M9qQGdUj+3cqpXSV8syj2tINABGyXwAPny5ZOdUtO8gAAAAVGhlIFRyZW5jaGVzIFNhdmlvcgAAAAAAAAAAAAAAAAAKAAAAM1E2cG9kAAAAAMgAAABodHRwczovL2lwZnMuaW8vaXBmcy9RbWFHTFY5RFNVUkZ4QjFXa0hTUFdoUEM3YWVVOEFnQnc5OU02WmdoWGtZVXNkAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAQEAAAAv8aIH571lqZ/pNJ+7Y9KeoYWdow3mZ/ZguXP35RifxgBkAAAB/wECAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==";

/// The one creator in [`LEGACY211_METADATA_B64`]: what `set_coin_creator` gives the `legacy211`
/// pool.
pub const LEGACY211_METADATA_CREATOR: Pubkey =
    anchor_lang::pubkey!("4E9s4kuEXzppwNY6DRqLUJ8wPoZfbrdUhYXYjHVx5fJ5");

/// `PDA(Metaplex, ["metadata", Metaplex, mint])`.
pub fn metaplex_metadata(mint: &Pubkey) -> Pubkey {
    Pubkey::find_program_address(
        &[b"metadata", METAPLEX_PROGRAM_ID.as_ref(), mint.as_ref()],
        &METAPLEX_PROGRAM_ID,
    )
    .0
}

/// Plants [`LEGACY211_METADATA_B64`] at the `legacy211` mint's metadata address.
pub fn plant_legacy211_metadata(env: &mut Env) {
    let mint = PammCoin::Legacy211.mint();
    let data = fork::base64_decode(LEGACY211_METADATA_B64).expect("metadata base64");
    env.svm
        .set_account(
            metaplex_metadata(&mint),
            solana_account::Account {
                lamports: 3_733_800,
                data,
                owner: METAPLEX_PROGRAM_ID,
                executable: false,
                rent_epoch: 0,
            },
        )
        .expect("metadata");
}

/// pAMM `set_coin_creator` of `p` (PPD IDL: `[pool W, metadata, bonding_curve, event_authority,
/// program]`, **no signer**; "Sets Pool::coin_creator from Metaplex metadata creator or
/// BondingCurve::creator").
pub fn set_coin_creator_ix(p: &PammPool) -> Instruction {
    Instruction {
        program_id: PUMP_AMM_PROGRAM_ID,
        accounts: vec![
            m(p.pool, true),
            m(metaplex_metadata(&p.mint), false),
            m(pda::bonding_curve(&p.mint), false),
            m(PUMP_AMM_EVENT_AUTHORITY, false),
            m(PUMP_AMM_PROGRAM_ID, false),
        ],
        data: fork::sha256(b"global:set_coin_creator")[..8].to_vec(),
    }
}

// ---- pools -------------------------------------------------------------------------------------

/// The PumpSwap pool roles of the `pamm_state` dump (R-VEN Update 2026-09-30 H10).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PammCoin {
    /// Token-2022, 301 B, 65 s after its migration: the boost vault still holds 13.98 SOL.
    Fresh,
    /// e/acc (Token-2022, 301 B): boost spent, 7.02 % third-party LP (H11).
    Mature,
    /// A cashback coin (Token-2022, 301 B).
    Cashback,
    /// SPL Token, a 211-byte pool (no coin creator, VQR absent) and a 49-byte curve.
    Legacy211,
    /// SPL Token, a 261-byte pool never extended, a 256-byte curve.
    Legacy261,
}

impl PammCoin {
    pub const ALL: [Self; 5] = [
        Self::Fresh,
        Self::Mature,
        Self::Cashback,
        Self::Legacy211,
        Self::Legacy261,
    ];

    /// The manifest role.
    pub fn role(self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::Mature => "mature",
            Self::Cashback => "cashback",
            Self::Legacy211 => "legacy211",
            Self::Legacy261 => "legacy261",
        }
    }

    /// The role's mint.
    pub fn mint(self) -> Pubkey {
        fork::pamm_role_mint(self.role())
    }
}

/// A canonical PumpSwap pool's keys (read from the pool account).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PammPool {
    pub mint: Pubkey,
    /// The base mint's token program.
    pub base_program: Pubkey,
    pub pool: Pubkey,
    pub base_vault: Pubkey,
    pub quote_vault: Pubkey,
    pub lp_mint: Pubkey,
    pub boost_vault: Pubkey,
    pub boost_vault_ata: Pubkey,
}

impl PammPool {
    /// The canonical pool of `mint` as the VM holds it (panics if there is none).
    pub fn of(svm: &LiteSVM, mint: &Pubkey) -> Self {
        let pool = pda::canonical_pool(mint);
        let acc = svm.get_account(&pool).expect("canonical pool");
        let p = pump_amm::read_pool(&acc.owner.to_bytes(), &acc.data).expect("Pool");
        let base_program = svm.get_account(mint).expect("mint").owner;
        let bv = boost_vault(&pool);
        Self {
            mint: *mint,
            base_program,
            pool,
            base_vault: Pubkey::new_from_array(p.pool_base_token_account),
            quote_vault: Pubkey::new_from_array(p.pool_quote_token_account),
            lp_mint: Pubkey::new_from_array(p.lp_mint),
            boost_vault: bv,
            boost_vault_ata: wsol_ata(&bv),
        }
    }

    /// The pool at `pool` (any pAMM pool of `mint`, canonical or not; panics if it does not
    /// parse).
    pub fn at(svm: &LiteSVM, pool: &Pubkey, mint: &Pubkey) -> Self {
        let acc = svm.get_account(pool).expect("pool");
        let p = pump_amm::read_pool(&acc.owner.to_bytes(), &acc.data).expect("Pool");
        let base_program = svm.get_account(mint).expect("mint").owner;
        let bv = boost_vault(pool);
        Self {
            mint: *mint,
            base_program,
            pool: *pool,
            base_vault: Pubkey::new_from_array(p.pool_base_token_account),
            quote_vault: Pubkey::new_from_array(p.pool_quote_token_account),
            lp_mint: Pubkey::new_from_array(p.lp_mint),
            boost_vault: bv,
            boost_vault_ata: wsol_ata(&bv),
        }
    }

    /// The pool as the reader sees it now.
    pub fn state(&self, svm: &LiteSVM) -> pump_amm::Pool {
        let acc = svm.get_account(&self.pool).expect("pool");
        pump_amm::read_pool(&acc.owner.to_bytes(), &acc.data).expect("Pool")
    }

    /// PumpSwap's pricing reserves now (vaults + VQR).
    pub fn reserves(&self, svm: &LiteSVM) -> Option<pump_amm::PoolReserves> {
        self.state(svm).reserves(
            fork::vault_amount(svm, &self.base_vault),
            fork::vault_amount(svm, &self.quote_vault),
        )
    }

    /// The boost vault's WSOL as the boost gate reads it (0 if the ATA is missing).
    pub fn boost_amount(&self, svm: &LiteSVM) -> u64 {
        let (owner, data) = svm
            .get_account(&self.boost_vault_ata)
            .map_or(([0u8; 32], Vec::new()), |a| (a.owner.to_bytes(), a.data));
        pump_amm::boost_vault_amount(&owner, &data, &self.boost_vault.to_bytes())
            .expect("boost vault ATA")
    }

    /// The pool's live coin creator.
    pub fn coin_creator(&self, svm: &LiteSVM) -> Pubkey {
        Pubkey::new_from_array(self.state(svm).coin_creator)
    }

    /// The base token program as the harness's enum.
    pub fn token_program(&self) -> TokenProgram {
        if self.base_program == TOKEN_2022_PROGRAM_ID {
            TokenProgram::Token2022
        } else {
            TokenProgram::Spl
        }
    }

    /// The LP mint's circulating supply (P6; Token-2022 on every canonical pool).
    pub fn lp_mint_supply(&self, svm: &LiteSVM) -> u64 {
        let acc = svm.get_account(&self.lp_mint).expect("LP mint");
        shortwin_venue_state::mint::read_base(&acc.owner.to_bytes(), &acc.data)
            .expect("LP mint")
            .supply
    }

    /// The pool's burned-LP share and locked depth (M6R-1, ECON #105), recomputed here: `λ_bps =
    /// ⌊(Pool.lp_supply − lp_mint.supply)·10⁴/lp_supply⌋` and `D = ⌊λ_bps·isqrt(X·(Y +
    /// VQR))/10⁴⌋` on the pricing reserves.
    pub fn locked_depth(&self, svm: &LiteSVM) -> (u128, u128) {
        let counter = u128::from(self.state(svm).lp_supply);
        let supply = u128::from(self.lp_mint_supply(svm));
        let lam = if counter == 0 || supply > counter {
            0
        } else {
            (counter - supply) * 10_000 / counter
        };
        let r = self.reserves(svm).expect("reserves");
        let root = (u128::from(r.base) * r.effective_quote).isqrt();
        (lam, lam * root / 10_000)
    }

    /// Spot `⌊(quote + VQR)·WAD/base⌋`.
    pub fn spot_wad(&self, svm: &LiteSVM) -> u128 {
        let r = self.reserves(svm).expect("reserves");
        r.effective_quote * shortwin_math::num::WAD / u128::from(r.base)
    }
}

// ---- PumpSwap's own instructions ---------------------------------------------------------------

/// PumpSwap's `init_user_volume_accumulator(payer, user)` (`user` need not sign; R-VEN Update H6).
pub fn pamm_init_uva_ix(payer: &Pubkey, user: &Pubkey) -> Instruction {
    Instruction {
        program_id: PUMP_AMM_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*payer, true),
            m(*user, false),
            m(pamm_uva(user), true),
            m(anchor_lang::system_program::ID, false),
            m(PUMP_AMM_EVENT_AUTHORITY, false),
            m(PUMP_AMM_PROGRAM_ID, false),
        ],
        data: PAMM_INIT_UVA.to_vec(),
    }
}

/// PumpSwap's `claim_cashback` for `user` into its WSOL account `user_wsol` (IDL: 9 accounts,
/// `user` writable and **not** a signer — T-CASHBACK: anyone may send it; the whole balance of
/// `ATA(UVA(user), WSOL)` moves to `user_wsol`, R-VEN Update H7).
pub fn pamm_claim_cashback_ix(user: &Pubkey, user_wsol: &Pubkey) -> Instruction {
    let u = pamm_uva(user);
    Instruction {
        program_id: PUMP_AMM_PROGRAM_ID,
        accounts: vec![
            m(*user, true),
            m(u, true),
            m(WSOL_MINT, false),
            m(TOKEN_PROGRAM_ID, false),
            m(wsol_ata(&u), true),
            m(*user_wsol, true),
            m(anchor_lang::system_program::ID, false),
            m(PUMP_AMM_EVENT_AUTHORITY, false),
            m(PUMP_AMM_PROGRAM_ID, false),
        ],
        data: PAMM_CLAIM_CASHBACK.to_vec(),
    }
}

/// pump's `extend_account` of `account` (`[account W, user S W, system, event_authority,
/// program]`): a curve under 115 bytes must be extended before a listing (`CurveNeedsExtend`).
pub fn pump_extend_account_ix(user: &Pubkey, account: &Pubkey) -> Instruction {
    Instruction {
        program_id: PUMP_PROGRAM_ID,
        accounts: vec![
            m(*account, true),
            AccountMeta::new(*user, true),
            m(anchor_lang::system_program::ID, false),
            m(PUMP_EVENT_AUTHORITY, false),
            m(PUMP_PROGRAM_ID, false),
        ],
        data: EXTEND_ACCOUNT.to_vec(),
    }
}

/// `PDA(pAMM, ["pool", index u16 LE, creator, mint, WSOL])`: the address of `creator`'s pool
/// `index` of `mint` (the canonical pool is index 0 by pump's pool authority).
pub fn pool_address(index: u16, creator: &Pubkey, mint: &Pubkey) -> Pubkey {
    pamm_pda(&[
        pump_amm::POOL_SEED,
        &index.to_le_bytes(),
        creator.as_ref(),
        mint.as_ref(),
        WSOL_MINT.as_ref(),
    ])
}

/// PumpSwap `create_pool(index, base_amount_in, quote_amount_in, coin_creator = default,
/// is_mayhem_mode = false, is_cashback_coin = false, creator_fee_bps = 0, can_edit_creator_fee =
/// false, is_holder_reward = false)` by `creator` (signer) from its base account `user_base` and
/// WSOL account `user_quote` (IDL: 18 accounts; the LP lands in `ATA(creator, LP mint)` under
/// Token-2022, the vaults are the pool's ATAs). Anyone may create a WSOL pool of any mint at any
/// index of their own: only index 0 by pump's pool authority is the canonical one (ARCH §2.5).
#[allow(clippy::too_many_arguments)]
pub fn pamm_create_pool_ix(
    creator: &Pubkey,
    mint: &Pubkey,
    base_program: &Pubkey,
    index: u16,
    user_base: &Pubkey,
    user_quote: &Pubkey,
    base_in: u64,
    quote_in: u64,
) -> Instruction {
    let pool = pool_address(index, creator, mint);
    let lp_mint = pamm_pda(&[pump_amm::LP_MINT_SEED, pool.as_ref()]);
    let mut data = PAMM_CREATE_POOL.to_vec();
    data.extend_from_slice(&index.to_le_bytes());
    data.extend_from_slice(&base_in.to_le_bytes());
    data.extend_from_slice(&quote_in.to_le_bytes());
    data.extend_from_slice(Pubkey::default().as_ref());
    data.push(0); // is_mayhem_mode
    data.push(0); // is_cashback_coin
    data.extend_from_slice(&0u64.to_le_bytes()); // creator_fee_bps
    data.push(0); // can_edit_creator_fee
    data.push(0); // is_holder_reward
    Instruction {
        program_id: PUMP_AMM_PROGRAM_ID,
        accounts: vec![
            m(pool, true),
            m(PUMP_AMM_GLOBAL_CONFIG, false),
            AccountMeta::new(*creator, true),
            m(*mint, false),
            m(WSOL_MINT, false),
            m(lp_mint, true),
            m(*user_base, true),
            m(*user_quote, true),
            m(pda::ata(creator, &lp_mint, &TOKEN_2022_PROGRAM_ID), true),
            m(pda::ata(&pool, mint, base_program), true),
            m(pda::ata(&pool, &WSOL_MINT, &TOKEN_PROGRAM_ID), true),
            m(anchor_lang::system_program::ID, false),
            m(TOKEN_2022_PROGRAM_ID, false),
            m(*base_program, false),
            m(TOKEN_PROGRAM_ID, false),
            m(ASSOCIATED_TOKEN_PROGRAM_ID, false),
            m(PUMP_AMM_EVENT_AUTHORITY, false),
            m(PUMP_AMM_PROGRAM_ID, false),
        ],
        data,
    }
}

/// A third party's own PumpSwap pool of `mint` (M10 G-A2): a funded creator puts `base_in`
/// tokens (planted in its base account) and `quote_in` wrapped lamports into a real
/// `create_pool(index)` on the dumped pAMM ELF — a valid WSOL pool with real liquidity that is not
/// the mint's canonical pool (another address, another creator, `index ≠ 0` unless chosen). Returns
/// the pool and its creator.
pub fn create_third_party_pool(
    env: &mut Env,
    mint: &Pubkey,
    index: u16,
    base_in: u64,
    quote_in: u64,
) -> (PammPool, Keypair) {
    let base_program = env.svm.get_account(mint).expect("mint").owner;
    let kp = env.funded_keypair();
    let payer = env.payer.pubkey();
    errors::ok(env.send(
        &[fork::create_ata_idempotent_ix(
            &payer,
            &kp.pubkey(),
            mint,
            &base_program,
        )],
        &[],
    ));
    let user_base = pda::ata(&kp.pubkey(), mint, &base_program);
    fork::set_token_amount(&mut env.svm, &user_base, base_in);
    let user_quote = fork::fund_wsol(env, &kp.pubkey(), quote_in);
    let ix = pamm_create_pool_ix(
        &kp.pubkey(),
        mint,
        &base_program,
        index,
        &user_base,
        &user_quote,
        base_in,
        quote_in,
    );
    let signer = kp.insecure_clone();
    errors::ok(env.send(&[crate::pump::cu_limit_ix(400_000), ix], &[&signer]));
    let pool = pool_address(index, &kp.pubkey(), mint);
    (PammPool::at(&env.svm, &pool, mint), kp)
}

/// A PumpSwap trade direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PammTrade {
    /// `buy(base_amount_out, max_quote_amount_in, [false])`.
    Buy,
    /// `sell(base_amount_in, min_quote_amount_out)`.
    Sell,
}

/// PumpSwap `buy` / `sell` by `user` (R-VEN §2.2: 23 / 21 fixed accounts, then the cashback ATA
/// (buy) or the cashback ATA and the UVA (sell) on a cashback coin, the `pool-v2` marker when the
/// coin creator is set, the buyback recipient and its WSOL ATA); the first protocol and buyback
/// recipients of the live `GlobalConfig`.
#[allow(clippy::too_many_arguments)]
pub fn pamm_trade_ix(
    svm: &LiteSVM,
    p: &PammPool,
    kind: PammTrade,
    user: &Pubkey,
    user_base: &Pubkey,
    user_quote: &Pubkey,
    a: u64,
    b: u64,
) -> Instruction {
    let pool = p.state(svm);
    let protocol = protocol_fee_recipient(svm, 0);
    let buyback = buyback_fee_recipient(svm, 0);
    let coin_creator = Pubkey::new_from_array(pool.coin_creator);
    let va = coin_creator_vault_authority(&coin_creator);
    let u = pamm_uva(user);
    let mut acc = vec![
        m(p.pool, true),
        AccountMeta::new(*user, true),
        m(PUMP_AMM_GLOBAL_CONFIG, false),
        m(p.mint, false),
        m(WSOL_MINT, false),
        m(*user_base, true),
        m(*user_quote, true),
        m(p.base_vault, true),
        m(p.quote_vault, true),
        m(protocol, false),
        m(wsol_ata(&protocol), true),
        m(p.base_program, false),
        m(TOKEN_PROGRAM_ID, false),
        m(anchor_lang::system_program::ID, false),
        m(ASSOCIATED_TOKEN_PROGRAM_ID, false),
        m(PUMP_AMM_EVENT_AUTHORITY, false),
        m(PUMP_AMM_PROGRAM_ID, false),
        m(wsol_ata(&va), true),
        m(va, false),
    ];
    if kind == PammTrade::Buy {
        acc.push(m(PUMP_AMM_GLOBAL_VOLUME_ACCUMULATOR, false));
        acc.push(m(u, true));
    }
    acc.push(m(PUMP_AMM_FEE_CONFIG, false));
    acc.push(m(PUMP_FEES_PROGRAM_ID, false));
    if pool.is_cashback_coin {
        acc.push(m(wsol_ata(&u), true));
        if kind == PammTrade::Sell {
            acc.push(m(u, true));
        }
    }
    if coin_creator != Pubkey::default() {
        acc.push(m(pool_v2(&p.mint), false));
    }
    acc.push(m(buyback, false));
    acc.push(m(wsol_ata(&buyback), true));
    let mut data = match kind {
        PammTrade::Buy => PAMM_BUY.to_vec(),
        PammTrade::Sell => PAMM_SELL.to_vec(),
    };
    data.extend_from_slice(&a.to_le_bytes());
    data.extend_from_slice(&b.to_le_bytes());
    if kind == PammTrade::Buy {
        data.push(0);
    }
    Instruction {
        program_id: PUMP_AMM_PROGRAM_ID,
        accounts: acc,
        data,
    }
}

/// PumpSwap `deposit(lp_out, max_base_in, max_quote_in)` or `withdraw(lp_in, min_base_out,
/// min_quote_out)` by `user` (IDL: 15 accounts; the LP mint and account under Token-2022; R-VEN
/// Update H11: anyone can add liquidity to a canonical pool and take it out again).
#[allow(clippy::too_many_arguments)]
pub fn pamm_liquidity_ix(
    p: &PammPool,
    deposit: bool,
    user: &Pubkey,
    user_base: &Pubkey,
    user_quote: &Pubkey,
    user_lp: &Pubkey,
    a: u64,
    b: u64,
    c: u64,
) -> Instruction {
    let name: &[u8] = if deposit {
        b"global:deposit"
    } else {
        b"global:withdraw"
    };
    let mut data = fork::sha256(name)[..8].to_vec();
    for x in [a, b, c] {
        data.extend_from_slice(&x.to_le_bytes());
    }
    Instruction {
        program_id: PUMP_AMM_PROGRAM_ID,
        accounts: vec![
            m(p.pool, true),
            m(PUMP_AMM_GLOBAL_CONFIG, false),
            AccountMeta::new_readonly(*user, true),
            m(p.mint, false),
            m(WSOL_MINT, false),
            m(p.lp_mint, true),
            m(*user_base, true),
            m(*user_quote, true),
            m(*user_lp, true),
            m(p.base_vault, true),
            m(p.quote_vault, true),
            m(TOKEN_PROGRAM_ID, false),
            m(TOKEN_2022_PROGRAM_ID, false),
            m(PUMP_AMM_EVENT_AUTHORITY, false),
            m(PUMP_AMM_PROGRAM_ID, false),
        ],
        data,
    }
}

/// A third-party liquidity provider on a canonical PumpSwap pool (R-VEN Update H11: anyone can
/// deposit into a canonical pool and withdraw again; M6R-1's attacker, ECON #105): its base ATA,
/// a WSOL account and its LP ATA under Token-2022; deposits and withdrawals through pAMM's own
/// `deposit` / `withdraw` ([`pamm_liquidity_ix`]), which scale both vaults, VQR and `lp_supply`.
pub struct PammLp {
    pub kp: Keypair,
    pub base: Pubkey,
    pub wsol: Pubkey,
    pub lp: Pubkey,
}

impl PammLp {
    /// A new provider of `p` holding `base` tokens and `sol` wrapped lamports.
    pub fn new(env: &mut Env, p: &PammPool, base: u64, sol: u64) -> Self {
        let kp = env.funded_keypair();
        let payer = env.payer.pubkey();
        let ixs = [
            fork::create_ata_idempotent_ix(&payer, &kp.pubkey(), &p.mint, &p.base_program),
            fork::create_ata_idempotent_ix(
                &payer,
                &kp.pubkey(),
                &p.lp_mint,
                &TOKEN_2022_PROGRAM_ID,
            ),
        ];
        errors::ok(env.send(&ixs, &[]));
        let b = pda::ata(&kp.pubkey(), &p.mint, &p.base_program);
        if base > 0 {
            fork::set_token_amount(&mut env.svm, &b, base);
        }
        let w = fork::fund_wsol(env, &kp.pubkey(), sol);
        Self {
            base: b,
            wsol: w,
            lp: pda::ata(&kp.pubkey(), &p.lp_mint, &TOKEN_2022_PROGRAM_ID),
            kp,
        }
    }

    /// A provider funded for deposits of up to `k_bps/10⁴ ×` the pool as it is now (both vaults,
    /// with half as much again to spare: its later deposits may meet a pool that trades moved).
    pub fn funded_for(env: &mut Env, p: &PammPool, k_bps: u64) -> Self {
        let r = p.reserves(&env.svm).expect("reserves");
        let scale = |v: u64| {
            u64::try_from(u128::from(v) * u128::from(k_bps) * 150 / 1_000_000).expect("u64")
        };
        let (base, sol) = (scale(r.base), scale(r.real_quote));
        Self::new(env, p, base, sol.max(1))
    }

    /// The LP a deposit of `k_bps/10⁴ ×` the pool mints: `⌊Pool.lp_supply·k/10⁴⌋`.
    pub fn lp_for(svm: &LiteSVM, p: &PammPool, k_bps: u64) -> u64 {
        u64::try_from(u128::from(p.state(svm).lp_supply) * u128::from(k_bps) / 10_000).expect("u64")
    }

    /// `deposit(lp_out, u64::MAX, u64::MAX)`.
    pub fn deposit_ix(&self, p: &PammPool, lp_out: u64) -> Instruction {
        pamm_liquidity_ix(
            p,
            true,
            &self.kp.pubkey(),
            &self.base,
            &self.wsol,
            &self.lp,
            lp_out,
            u64::MAX,
            u64::MAX,
        )
    }

    /// `withdraw(lp_in, 0, 0)`.
    pub fn withdraw_ix(&self, p: &PammPool, lp_in: u64) -> Instruction {
        pamm_liquidity_ix(
            p,
            false,
            &self.kp.pubkey(),
            &self.base,
            &self.wsol,
            &self.lp,
            lp_in,
            0,
            0,
        )
    }

    /// The provider's LP balance.
    pub fn lp_balance(&self, svm: &LiteSVM) -> u64 {
        read::token_amount(svm, &self.lp).unwrap_or(0)
    }

    /// A deposit of `k_bps/10⁴ ×` the pool, sent alone; returns the LP minted.
    pub fn deposit(&self, env: &mut Env, p: &PammPool, k_bps: u64) -> u64 {
        let before = self.lp_balance(&env.svm);
        let lp = Self::lp_for(&env.svm, p, k_bps);
        let kp = self.kp.insecure_clone();
        errors::ok(env.send(&[self.deposit_ix(p, lp)], &[&kp]));
        self.lp_balance(&env.svm) - before
    }

    /// Every LP unit the provider holds withdrawn, sent alone.
    pub fn withdraw_all(&self, env: &mut Env, p: &PammPool) {
        let lp = self.lp_balance(&env.svm);
        let kp = self.kp.insecure_clone();
        errors::ok(env.send(&[self.withdraw_ix(p, lp)], &[&kp]));
    }
}

/// A normal PumpSwap trader: a funded keypair with its base ATA, a WSOL account and its pAMM UVA
/// (created through `Env::send` before any trade).
pub struct PammUser {
    pub kp: Keypair,
    pub base: Pubkey,
    pub wsol: Pubkey,
}

impl PammUser {
    /// A new trader of `p` holding `wsol` wrapped lamports.
    pub fn new(env: &mut Env, p: &PammPool, wsol: u64) -> Self {
        let kp = env.funded_keypair();
        let payer = env.payer.pubkey();
        let ixs = [
            fork::create_ata_idempotent_ix(&payer, &kp.pubkey(), &p.mint, &p.base_program),
            pamm_init_uva_ix(&payer, &kp.pubkey()),
        ];
        errors::ok(env.send(&ixs, &[]));
        let w = fork::fund_wsol(env, &kp.pubkey(), wsol);
        Self {
            base: pda::ata(&kp.pubkey(), &p.mint, &p.base_program),
            wsol: w,
            kp,
        }
    }

    /// `buy(base_out, max_in)`.
    pub fn buy(&self, env: &mut Env, p: &PammPool, base_out: u64, max_in: u64) -> TxResult {
        let ix = pamm_trade_ix(
            &env.svm,
            p,
            PammTrade::Buy,
            &self.kp.pubkey(),
            &self.base,
            &self.wsol,
            base_out,
            max_in,
        );
        let kp = self.kp.insecure_clone();
        env.send(&[ix], &[&kp])
    }

    /// `sell(base_in, min_out)` (plants the base balance first if short).
    pub fn sell(&self, env: &mut Env, p: &PammPool, base_in: u64, min_out: u64) -> TxResult {
        let have = read::token_amount(&env.svm, &self.base).unwrap_or(0);
        if have < base_in {
            fork::set_token_amount(&mut env.svm, &self.base, base_in);
        }
        let ix = pamm_trade_ix(
            &env.svm,
            p,
            PammTrade::Sell,
            &self.kp.pubkey(),
            &self.base,
            &self.wsol,
            base_in,
            min_out,
        );
        let kp = self.kp.insecure_clone();
        env.send(&[ix], &[&kp])
    }

    /// Buys until the pool's spot is `×num/den` its current one (constant product: the base
    /// reserve falls by `√(den/num)`).
    pub fn push_price(&self, env: &mut Env, p: &PammPool, num: u128, den: u128) -> TxResult {
        let r = p.reserves(&env.svm).expect("reserves");
        let base = u128::from(r.base);
        let target = (base * base * den / num).isqrt();
        let out = u64::try_from(base - target).expect("u64");
        self.buy(env, p, out, u64::MAX)
    }
}

// ---- markets -----------------------------------------------------------------------------------

/// A PumpSwap market on the fork: its keys, its pool, its lender and the recipients its trades
/// name.
pub struct PammMarket {
    pub keys: MarketKeys,
    pub pool: PammPool,
    /// `ATA(trader, mint)`.
    pub trader_base_ata: Pubkey,
    /// The protocol-fee and buyback recipients the trades name (from the live `GlobalConfig`).
    pub protocol_recipient: Pubkey,
    pub buyback_recipient: Pubkey,
    /// The coin creator the trades name (re-read with [`PammMarket::refresh_creator`]).
    pub coin_creator: Pubkey,
    /// The lender that seeded the market (and its token account).
    pub lender: Keypair,
    pub lender_account: Pubkey,
}

impl PammMarket {
    /// The PumpSwap market of `mint` on the VM's canonical pool.
    pub fn new(svm: &LiteSVM, mint: &Pubkey, lender: Keypair, lender_account: Pubkey) -> Self {
        let pool = PammPool::of(svm, mint);
        let market = pda::market(mint).0;
        let keys = MarketKeys {
            mint: *mint,
            token_program: pool.base_program,
            market,
            market_vault: pda::market_vault(&market).0,
            pool_vault: pda::pool_vault(&market).0,
            trader: pda::trader(&market).0,
        };
        Self {
            trader_base_ata: pda::ata(&keys.trader, mint, &pool.base_program),
            protocol_recipient: protocol_fee_recipient(svm, 0),
            buyback_recipient: buyback_fee_recipient(svm, 0),
            coin_creator: pool.coin_creator(svm),
            pool,
            keys,
            lender,
            lender_account,
        }
    }

    /// A curve market's after its `→ PumpSwap` transition: the same market, lender and keys on
    /// the canonical pool `migrate_v2` created.
    pub fn graduated(svm: &LiteSVM, pm: &PumpMarket) -> Self {
        Self::new(
            svm,
            &pm.keys.mint,
            pm.lender.insecure_clone(),
            pm.lender_account,
        )
    }

    /// The market.
    pub fn market(&self) -> Pubkey {
        self.keys.market
    }

    /// The trader PDA.
    pub fn trader(&self) -> Pubkey {
        self.keys.trader
    }

    /// The trader's pAMM UVA (T14).
    pub fn trader_uva(&self) -> Pubkey {
        pamm_uva(&self.keys.trader)
    }

    /// `ATA(trader's pAMM UVA, WSOL)` (T18): the trader's cashback.
    pub fn cashback_ata(&self) -> Pubkey {
        wsol_ata(&self.trader_uva())
    }

    /// The coin-creator vault authority and its WSOL ATA the trades name (T12, T11).
    pub fn creator_vault(&self) -> (Pubkey, Pubkey) {
        let a = coin_creator_vault_authority(&self.coin_creator);
        (a, wsol_ata(&a))
    }

    /// Re-reads the pool's coin creator (after an admin change).
    pub fn refresh_creator(&mut self, svm: &LiteSVM) {
        self.coin_creator = self.pool.coin_creator(svm);
    }

    /// Chooses other recipients (protocol recipient `i`, buyback recipient `j`).
    pub fn use_recipients(&mut self, svm: &LiteSVM, i: usize, j: usize) {
        self.protocol_recipient = protocol_fee_recipient(svm, i);
        self.buyback_recipient = buyback_fee_recipient(svm, j);
    }

    /// The lender's keys for the lender builders.
    pub fn lender_keys(&self) -> LenderKeys {
        LenderKeys {
            owner: self.lender.pubkey(),
            mint: self.keys.mint,
            token_program: self.keys.token_program,
            owner_token_account: self.lender_account,
        }
    }

    /// The TradeExtras T0–T18 as a list of keys (ARCH §3.12 order).
    pub fn trade_keys(&self) -> Vec<Pubkey> {
        self.trade_extras().iter().map(|a| a.pubkey).collect()
    }
}

impl VenueMarket for PammMarket {
    fn keys(&self) -> &MarketKeys {
        &self.keys
    }

    /// P0 the pool (W), P1 its base vault (W), P2 its WSOL vault (W), P3 `GlobalConfig`, P4
    /// `FeeConfig`, P5 the boost vault's WSOL ATA, P6 the pool's LP mint (M6R-1: its supply gives
    /// the burned LP's locked depth).
    fn read_set(&self, writable: bool) -> Vec<AccountMeta> {
        vec![
            m(self.pool.pool, writable),
            m(self.pool.base_vault, writable),
            m(self.pool.quote_vault, writable),
            m(PUMP_AMM_GLOBAL_CONFIG, false),
            m(PUMP_AMM_FEE_CONFIG, false),
            m(self.pool.boost_vault_ata, false),
            m(self.pool.lp_mint, false),
        ]
    }

    /// T0–T18 (ARCH §3.12).
    fn trade_extras(&self) -> Vec<AccountMeta> {
        let (auth, auth_ata) = self.creator_vault();
        let uva = self.trader_uva();
        vec![
            m(PUMP_AMM_PROGRAM_ID, false),
            m(PUMP_AMM_EVENT_AUTHORITY, false),
            m(PUMP_FEES_PROGRAM_ID, false),
            m(WSOL_MINT, false),
            m(TOKEN_PROGRAM_ID, false),
            m(ASSOCIATED_TOKEN_PROGRAM_ID, false),
            m(anchor_lang::system_program::ID, false),
            m(self.trader_base_ata, true),
            m(pda::trader_wsol(&self.keys.market).0, true),
            m(self.protocol_recipient, false),
            m(wsol_ata(&self.protocol_recipient), true),
            m(auth_ata, true),
            m(auth, false),
            m(PUMP_AMM_GLOBAL_VOLUME_ACCUMULATOR, false),
            m(uva, true),
            m(pool_v2(&self.keys.mint), false),
            m(self.buyback_recipient, false),
            m(wsol_ata(&self.buyback_recipient), true),
            m(wsol_ata(&uva), true),
        ]
    }

    fn cu_limit(&self) -> Option<u32> {
        Some(PAMM_TRADE_CU)
    }
}

/// The pAMM init set of a PumpSwap listing (after the detection set): `[pAMM program, the
/// trader's pAMM UVA (W), pAMM event authority]`.
pub fn pamm_init_set(market: &Pubkey) -> Vec<AccountMeta> {
    let trader = pda::trader(market).0;
    vec![
        m(PUMP_AMM_PROGRAM_ID, false),
        m(pamm_uva(&trader), true),
        m(PUMP_AMM_EVENT_AUTHORITY, false),
    ]
}

/// `create_market(Auto, u64::MAX)` of `mint` with the pAMM init set.
pub fn create_pamm_market_ix(
    creator: &Pubkey,
    mint: &Pubkey,
    token_program: &Pubkey,
) -> Instruction {
    let ix = ix::create_market(
        creator,
        mint,
        token_program,
        VENUE_HINT_AUTO,
        &pda::canonical_pool(mint),
    );
    let market = pda::market(mint).0;
    ix::with_remaining(ix, &pamm_init_set(&market))
}

/// The accounts ARCH §3.12's existence requirement names that a market's trades need and that may
/// not exist yet, created by `payer` (idempotent, any payer): the trader's pAMM UVA (T14), the
/// coin-creator vault's WSOL ATA (T11), the trader's cashback ATA (T18, cashback coins) and the
/// two recipients' WSOL ATAs (T10, T17).
pub fn venue_account_ixs(svm: &LiteSVM, payer: &Pubkey, pm: &PammMarket) -> Vec<Instruction> {
    let spl = TOKEN_PROGRAM_ID;
    let (auth, _) = pm.creator_vault();
    let mut ixs = Vec::new();
    if svm
        .get_account(&pm.trader_uva())
        .is_none_or(|a| a.data.is_empty())
    {
        ixs.push(pamm_init_uva_ix(payer, &pm.trader()));
    }
    ixs.push(fork::create_ata_idempotent_ix(
        payer, &auth, &WSOL_MINT, &spl,
    ));
    ixs.push(fork::create_ata_idempotent_ix(
        payer,
        &pm.protocol_recipient,
        &WSOL_MINT,
        &spl,
    ));
    ixs.push(fork::create_ata_idempotent_ix(
        payer,
        &pm.buyback_recipient,
        &WSOL_MINT,
        &spl,
    ));
    if pm.pool.state(svm).is_cashback_coin {
        ixs.push(fork::create_ata_idempotent_ix(
            payer,
            &pm.trader_uva(),
            &WSOL_MINT,
            &spl,
        ));
    }
    ixs
}

/// `sweep_trader()` of a PumpSwap market: the token leg `[trader_base_ata, pool_vault, mint, token
/// program, the trader's pump UVA, ATA(the trader's pAMM UVA, WSOL)]` (M6), then the latest
/// `ShareEpoch` record while a full-wipe record is live.
pub fn sweep_trader_pamm_ix(env: &Env, pm: &PammMarket) -> Instruction {
    let base = crate::engine::sweep_trader_ix_for(env, &pm.market());
    let mk = env.market(&pm.market());
    let live = mk.total_shares.get() == 0 && mk.has_flag(shortwin::state::MARKET_FLAG_WIPE_LIVE);
    let (head, rest) = base
        .accounts
        .split_at(base.accounts.len() - usize::from(live));
    let mut accounts = head.to_vec();
    accounts.extend([
        m(pm.trader_base_ata, true),
        m(pm.keys.pool_vault, true),
        m(pm.keys.mint, false),
        m(pm.keys.token_program, false),
        m(fork::pump_user_volume_accumulator(&pm.trader()), false),
        m(pm.cashback_ata(), false),
    ]);
    accounts.extend_from_slice(rest);
    Instruction {
        program_id: base.program_id,
        accounts,
        data: base.data,
    }
}

// ---- the 2026-10-02 redeploy's fee buckets (M10 pump revalidation) -----------------------------
//
// The PumpSwap deployed on 2026-10-02 (`fork::PumpDeploy::Oct2026`) has the v2 trades of
// `pump-rust-client` 0.2.0's IDL (`pump_amm.json`; R-VEN Update 2026-09-30 H2): `buy_v2` /
// `sell_v2` pay the buyback slice in the trade and keep the rest of the protocol fee and the
// creator fee **in the quote vault**, counted in `Pool.protocol_fees` @271 and `creator_fees` @279
// and subtracted from `virtual_quote_reserves`; `sweep_protocol_fee` / `sweep_creator_fee` pay
// them out. `shortwin-venue-state` reads them since PUMP-2026-10-1 (`pump_amm::Pool::
// protocol_fees` / `creator_fees`, `PoolReserves::payable_quote`); the constants below are its.

/// PumpSwap `buy_v2` (`b817ee61…`).
pub const PAMM_BUY_V2: [u8; 8] = [0xb8, 0x17, 0xee, 0x61, 0x67, 0xc5, 0xd3, 0x3d];
/// PumpSwap `sell_v2` (`5df6823c…`).
pub const PAMM_SELL_V2: [u8; 8] = [0x5d, 0xf6, 0x82, 0x3c, 0xe7, 0xe9, 0x40, 0xb2];
/// `sweep_protocol_fee` (`0830be07…`; the same Anchor name on pump and PumpSwap).
pub const SWEEP_PROTOCOL_FEE: [u8; 8] = [0x08, 0x30, 0xbe, 0x07, 0xb6, 0x44, 0xb7, 0xe5];
/// `sweep_creator_fee` (`20f6bf34…`).
pub const SWEEP_CREATOR_FEE: [u8; 8] = [0x20, 0xf6, 0xbf, 0x34, 0x08, 0xc9, 0x49, 0xba];
/// `Pool.protocol_fees` (u64), the un-swept protocol fee bucket.
pub const POOL_OFF_PROTOCOL_FEES: usize = pump_amm::OFF_PROTOCOL_FEES;
/// `Pool.creator_fees` (u64), the un-swept creator fee bucket.
pub const POOL_OFF_CREATOR_FEES: usize = pump_amm::OFF_CREATOR_FEES;
/// What the redeploy's `create_pool` allocates and its trades grow a shorter pool to.
pub const POOL_LEN_2026_10: usize = pump_amm::POOL_LEN_2026_10;
/// PumpSwap's `InsufficientRealQuoteReserves`: a sale paying out more than `vault − buckets`.
pub const PAMM_ERR_INSUFFICIENT_REAL_QUOTE: u32 = 6063;

/// The pool's fee buckets `(protocol_fees, creator_fees)` (0 on a pool shorter than 287 bytes).
pub fn pool_buckets(svm: &LiteSVM, pool: &Pubkey) -> (u64, u64) {
    let d = svm.get_account(pool).expect("pool").data;
    let at = |o: usize| {
        d.get(o..o + 8)
            .map_or(0, |b| u64::from_le_bytes(b.try_into().unwrap()))
    };
    (at(POOL_OFF_PROTOCOL_FEES), at(POOL_OFF_CREATOR_FEES))
}

/// A byte-level plant of what un-swept v2 trades leave on a 287- or 301-byte pool (the scenario
/// builders' idiom): `protocol` and `creator` lamports of the quote vault counted in the buckets
/// and taken off `virtual_quote_reserves` (the vault itself is unchanged, so the price falls by
/// the buckets, exactly as pump's `effective = vault + VQR` with `VQR = boost − buckets`).
pub fn plant_buckets(svm: &mut LiteSVM, p: &PammPool, protocol: u64, creator: u64) {
    fork::patch(svm, &p.pool, |d| {
        assert!(d.len() >= POOL_LEN_2026_10, "a pool of at least 287 bytes");
        let vqr = i128::from_le_bytes(d[245..261].try_into().unwrap());
        let vqr = vqr - i128::from(protocol) - i128::from(creator);
        d[245..261].copy_from_slice(&vqr.to_le_bytes());
        d[POOL_OFF_PROTOCOL_FEES..POOL_OFF_PROTOCOL_FEES + 8]
            .copy_from_slice(&protocol.to_le_bytes());
        d[POOL_OFF_CREATOR_FEES..POOL_OFF_CREATOR_FEES + 8].copy_from_slice(&creator.to_le_bytes());
    });
}

/// PumpSwap `buy_v2(base_amount_out, max_quote_amount_in)` or `sell_v2(base_amount_in,
/// min_quote_amount_out)` by `user` (IDL `pump_amm.json` of `pump-rust-client` 0.2.0: 17
/// accounts — the pool, `user`, `GlobalConfig`, both mints, both user and pool token accounts,
/// both token programs, System, the user's pAMM UVA, the pAMM `FeeConfig`, the first buyback
/// recipient's WSOL ATA, the event authority, the program; no protocol or creator fee accounts
/// and no pump-fees CPI). Canonical pump pools only; cashback coins and mayhem pools stay on v1.
#[allow(clippy::too_many_arguments)]
pub fn pamm_trade_v2_ix(
    svm: &LiteSVM,
    p: &PammPool,
    kind: PammTrade,
    user: &Pubkey,
    user_base: &Pubkey,
    user_quote: &Pubkey,
    a: u64,
    b: u64,
) -> Instruction {
    let buyback = buyback_fee_recipient(svm, 0);
    let accounts = vec![
        m(p.pool, true),
        AccountMeta::new(*user, true),
        m(PUMP_AMM_GLOBAL_CONFIG, false),
        m(p.mint, false),
        m(WSOL_MINT, false),
        m(*user_base, true),
        m(*user_quote, true),
        m(p.base_vault, true),
        m(p.quote_vault, true),
        m(p.base_program, false),
        m(TOKEN_PROGRAM_ID, false),
        m(anchor_lang::system_program::ID, false),
        m(pamm_uva(user), true),
        m(PUMP_AMM_FEE_CONFIG, false),
        m(wsol_ata(&buyback), true),
        m(PUMP_AMM_EVENT_AUTHORITY, false),
        m(PUMP_AMM_PROGRAM_ID, false),
    ];
    let mut data = match kind {
        PammTrade::Buy => PAMM_BUY_V2.to_vec(),
        PammTrade::Sell => PAMM_SELL_V2.to_vec(),
    };
    data.extend_from_slice(&a.to_le_bytes());
    data.extend_from_slice(&b.to_le_bytes());
    Instruction {
        program_id: PUMP_AMM_PROGRAM_ID,
        accounts,
        data,
    }
}

/// `sweep_protocol_fee()` (`protocol`) or `sweep_creator_fee()` of `p` to `recipient` (a listed
/// protocol fee recipient, or the coin creator's vault authority), paid by `payer` (12 accounts;
/// the recipient's WSOL ATA is created idempotently at the payer's cost).
pub fn pamm_sweep_ix(
    p: &PammPool,
    protocol: bool,
    payer: &Pubkey,
    recipient: &Pubkey,
) -> Instruction {
    Instruction {
        program_id: PUMP_AMM_PROGRAM_ID,
        accounts: vec![
            AccountMeta::new(*payer, true),
            m(PUMP_AMM_GLOBAL_CONFIG, false),
            m(p.pool, true),
            m(WSOL_MINT, false),
            m(TOKEN_PROGRAM_ID, false),
            m(p.quote_vault, true),
            m(*recipient, false),
            m(wsol_ata(recipient), true),
            m(anchor_lang::system_program::ID, false),
            m(ASSOCIATED_TOKEN_PROGRAM_ID, false),
            m(PUMP_AMM_EVENT_AUTHORITY, false),
            m(PUMP_AMM_PROGRAM_ID, false),
        ],
        data: if protocol {
            SWEEP_PROTOCOL_FEE.to_vec()
        } else {
            SWEEP_CREATOR_FEE.to_vec()
        },
    }
}

impl PammUser {
    /// `buy_v2(base_out, max_in)` or `sell_v2(base_in, min_out)` (a sale plants the base balance
    /// first if short).
    pub fn trade_v2(
        &self,
        env: &mut Env,
        p: &PammPool,
        kind: PammTrade,
        a: u64,
        b: u64,
    ) -> TxResult {
        if kind == PammTrade::Sell {
            let have = read::token_amount(&env.svm, &self.base).unwrap_or(0);
            if have < a {
                fork::set_token_amount(&mut env.svm, &self.base, a);
            }
        }
        let ix = pamm_trade_v2_ix(
            &env.svm,
            p,
            kind,
            &self.kp.pubkey(),
            &self.base,
            &self.wsol,
            a,
            b,
        );
        let kp = self.kp.insecure_clone();
        env.send(&[ix], &[&kp])
    }
}

impl Env {
    /// Loads PumpSwap's statics of the `pamm_state` dump (`GlobalConfig`, `FeeConfig`, the global
    /// volume accumulator, WSOL, the 16 recipient WSOL ATAs; its Rent sysvar is
    /// [`Env::set_pump_rent`]'s, byte-identical).
    pub fn load_pamm_statics(&mut self) {
        let names: Vec<String> = fork::all_pamm_fixtures()
            .filter(|f| f.group == "pamm_state" && f.role.is_none())
            .filter(|f| !f.name.ends_with("sysvar_rent"))
            .map(|f| f.name.clone())
            .collect();
        for name in names {
            fork::load(&mut self.svm, &name);
        }
    }

    /// Loads every `pamm_state` account of `coin`'s role (pool, mint, vaults, LP mint, coin-creator
    /// vault and ATA, boost vault ATA, pool authority, the migrated curve). Returns the pool.
    pub fn load_pamm_coin(&mut self, coin: PammCoin) -> PammPool {
        let names: Vec<String> = fork::all_pamm_fixtures()
            .filter(|f| f.group == "pamm_state" && f.role.as_deref() == Some(coin.role()))
            .map(|f| f.name.clone())
            .collect();
        for name in names {
            fork::load(&mut self.svm, &name);
        }
        PammPool::of(&self.svm, &coin.mint())
    }

    /// The **synthetic** negative-VQR pool (`pamm_mature_pool_negative_vqr`: the `mature` pool with
    /// VQR = −⌊vault/2⌋) in place of the `mature` pool (same address).
    pub fn plant_negative_vqr(&mut self) {
        fork::load(&mut self.svm, "pamm_mature_pool_negative_vqr");
    }

    /// Sets the clock to the `pamm_state` snapshot's slot and block time.
    pub fn set_pamm_clock(&mut self) {
        let s = fork::pamm_snapshot();
        self.set_clock(s.block_time, s.slot);
    }

    /// [`Env::initialized`], the pump programs and statics, PumpSwap's statics, `coin`'s accounts,
    /// mainnet's Rent and the `pamm_state` clock.
    pub fn fork_pamm_coin(coin: PammCoin) -> (Self, PammPool) {
        Self::fork_pamm_coin_with(coin, |_| {})
    }

    /// [`Env::fork_pamm_coin`] with the config's initial arguments changed by `f` first (within
    /// the compiled bounds, e.g. the per-market notional limit).
    pub fn fork_pamm_coin_with(
        coin: PammCoin,
        f: impl FnOnce(&mut shortwin::state::InitConfigArgs),
    ) -> (Self, PammPool) {
        let mut env = Self::new();
        let mut args = env.default_init_args();
        f(&mut args);
        errors::ok(env.initialize_config(args));
        env.load_pump_programs();
        env.load_pump_statics();
        env.load_pamm_statics();
        let p = env.load_pamm_coin(coin);
        env.set_pump_rent();
        env.set_pamm_clock();
        (env, p)
    }

    /// Lists `mint` with `create_market(Auto)` and the pAMM init set, as a new funded creator.
    pub fn list_pamm(&mut self, mint: &Pubkey, token_program: &Pubkey) -> TxResult {
        let creator = self.funded_keypair();
        let ix = create_pamm_market_ix(&creator.pubkey(), mint, token_program);
        self.send(&[ix], &[&creator])
    }

    /// The [`PammMarket`] of an already listed `mint`: a lender deposits `lend` (planted in its
    /// account first), the existence requirement's accounts are created, and `warmup` seconds pass
    /// with a crank each second (the price start and its warm-up, M6).
    pub fn pamm_market_listed(&mut self, mint: &Pubkey, lend: u64, warmup: i64) -> PammMarket {
        let program = if self.svm.get_account(mint).expect("mint").owner == TOKEN_2022_PROGRAM_ID {
            TokenProgram::Token2022
        } else {
            TokenProgram::Spl
        };
        let lender = self.funded_keypair();
        let lender_account = fork::fund_token(self, &lender.pubkey(), mint, program, lend);
        let pm = PammMarket::new(&self.svm, mint, lender, lender_account);
        if lend > 0 {
            let lender = pm.lender.insecure_clone();
            errors::ok(self.send(&[ix::deposit(&pm.lender_keys(), lend, 0)], &[&lender]));
        }
        let payer = self.payer.pubkey();
        let ixs = venue_account_ixs(&self.svm, &payer, &pm);
        errors::ok(self.send(&ixs, &[]));
        for _ in 0..warmup {
            self.warp(1);
            errors::ok(self.send(&[crate::engine::crank_price_ix(&pm)], &[]));
        }
        pm
    }

    /// A PumpSwap market on `coin` on a fresh fork: listed, `lend` tokens lent, 60 s of cranks
    /// (the listing warm-up `market_warmup_s` and the price's post-change warm-up).
    pub fn pamm_market(coin: PammCoin, lend: u64) -> (Self, PammMarket) {
        let (mut env, p) = Self::fork_pamm_coin(coin);
        let payer = env.payer.pubkey();
        if env
            .svm
            .get_account(&pda::bonding_curve(&p.mint))
            .is_some_and(|a| a.data.len() < shortwin_venue_state::pump::CURVE_MIN_LEN)
        {
            // A 2024-layout curve: the client prepends pump's `extend_account` (ARCH §3.2 check 3).
            let kp = env.payer.insecure_clone();
            errors::ok(env.send(
                &[pump_extend_account_ix(&payer, &pda::bonding_curve(&p.mint))],
                &[&kp],
            ));
        }
        errors::ok(env.list_pamm(&p.mint, &p.base_program));
        let pm = env.pamm_market_listed(&p.mint, lend, 60);
        (env, pm)
    }
}

/// The token-account state of `key` (`None` if it is not one).
pub fn token_state(svm: &LiteSVM, key: &Pubkey) -> Option<token_account::TokenAccount> {
    let acc = svm.get_account(key)?;
    token_account::read(&acc.owner.to_bytes(), &acc.data).ok()
}
