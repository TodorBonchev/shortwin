//! Shared setup of the keeper scenarios.

#![allow(dead_code)]

#[allow(unused_imports)]
pub use shortwin_keeper::ExitResult;
pub use shortwin_keeper::{Alert, ExitKind, KeeperConfig, TickReport};
pub use shortwin_keeper_litesvm::Harness;
pub use shortwin_litesvm_tests::engine::{Borrower, MarketSpec, MockMarket};
pub use shortwin_litesvm_tests::errors::ok;
pub use shortwin_litesvm_tests::{Env, mock};

/// Positions of this size fit every default cap (≈ 3 SOL of notional on the default pool).
pub const Q: u64 = 3_000_000_000_000;

/// `cfg` covering every market with open positions (`var_floor_lamports` 0). The fork scenarios'
/// positions are small — well below the global budget's 1-SOL value-at-risk floor (owner decision
/// A of 2026-10-01; ARCH §7.1 duty 8) — and what they test (synthetic curve exits, graduations,
/// wipes, exits and per-second coverage on the pump and PumpSwap venues) are the duties of a
/// covered market; below the floor a market gets the idle cadence only, which the `budget`
/// scenarios test.
pub fn covering_all(mut cfg: KeeperConfig) -> KeeperConfig {
    cfg.budget.var_floor_lamports = 0;
    cfg
}

/// A test-venue environment with `n` default mock markets and a keeper with `cfg`.
pub fn setup(n: usize, cfg: KeeperConfig) -> (Harness, Vec<MockMarket>) {
    let mut env = Env::test_venue();
    let markets = (0..n)
        .map(|_| env.mock_market(MarketSpec::default()))
        .collect();
    (Harness::new(env, cfg), markets)
}

/// Opens a position of `q` on `mm` for a new borrower (must succeed). A crank one second
/// before integrates the warm-up, so the keeper's first observation follows no gap (the
/// warm-up itself would otherwise count as a `CoverageGap` once a position is open).
pub fn open(h: &Harness, mm: &MockMarket, q: u64) -> (Borrower, u64) {
    h.with_env(|env| {
        env.crank(mm);
        env.warp(1);
        let mut b = env.borrower();
        let (res, nonce) = env.open(mm, &mut b, q);
        ok(res);
        (b, nonce)
    })
}

use anchor_lang::prelude::Pubkey;
use anchor_lang::solana_program::instruction::Instruction;
use shortwin_litesvm_tests::{errors, pda, read};
use solana_keypair::Keypair;
use solana_signer::Signer;
use solana_transaction::Transaction;

/// A System transfer of `lamports` from `from` (signer) to `to` (a donation to a PDA).
pub fn transfer_ix(from: &Pubkey, to: &Pubkey, lamports: u64) -> Instruction {
    let mut data = vec![2u8, 0, 0, 0];
    data.extend_from_slice(&lamports.to_le_bytes());
    Instruction {
        program_id: anchor_lang::system_program::ID,
        accounts: vec![
            anchor_lang::solana_program::instruction::AccountMeta::new(*from, true),
            anchor_lang::solana_program::instruction::AccountMeta::new(*to, false),
        ],
        data,
    }
}

/// An outside keeper that only simulates: the oracle of "would the program accept this
/// exit now" against which the keeper's timing is checked.
pub struct Oracle {
    pub kp: Keypair,
}

impl Oracle {
    /// A funded outside key.
    pub fn new(h: &Harness) -> Self {
        let kp = h.with_env(|env| env.funded_keypair());
        Self { kp }
    }

    /// Simulates `ix` (signed by this key) on the current state: `Ok` if the program accepts
    /// it, else the custom error code (if any).
    pub fn accepts(&self, h: &Harness, ix: Instruction) -> Result<(), Option<u32>> {
        h.with_env(|env| {
            let tx = Transaction::new_signed_with_payer(
                &[ix],
                Some(&self.kp.pubkey()),
                &[&self.kp],
                env.svm.latest_blockhash(),
            );
            match env.svm.simulate_transaction(tx) {
                Ok(_) => Ok(()),
                Err(f) => Err(errors::custom_code(&Err(f))),
            }
        })
    }
}

/// Whether the position `(borrower, nonce)` of `mm` is still open.
pub fn position_open(h: &Harness, mm: &MockMarket, borrower: &Pubkey, nonce: u64) -> bool {
    h.with_env(|env| {
        read::borsh::<shortwin::state::ShortPosition>(
            &env.svm,
            &pda::position(&mm.market(), borrower, nonce).0,
        )
        .is_some()
    })
}

/// The position account key.
pub fn position_key(mm: &MockMarket, borrower: &Pubkey, nonce: u64) -> Pubkey {
    pda::position(&mm.market(), borrower, nonce).0
}

/// The keeper's own evaluation of `position` on `market` now — its view, projection and decision
/// (`view::load`, `decide::project`, `decide::evaluate`) on the state its next tick reads (a tick's
/// report keeps the evaluations after its exits, which may have closed the position) — with the
/// projection and the venue state it read; `None` when the position is not open.
pub fn keeper_eval(
    h: &Harness,
    market: Pubkey,
    position: Pubkey,
) -> Option<(
    shortwin_keeper::decide::PositionEval,
    shortwin_keeper::decide::Projection,
    shortwin::venues::VenueState,
)> {
    use shortwin_keeper::config::CoverageConfig;
    use shortwin_keeper::{decide, view};
    let cv = shortwin_keeper_litesvm::block_on(view::load(h.chain(), &[market])).expect("view");
    let mv = cv.markets.first()?;
    let proj = decide::project(&cv.config, mv, cv.clock).expect("projection");
    let pv = mv.positions.iter().find(|p| p.key == position)?;
    let ev = decide::evaluate(mv, &proj, pv, &CoverageConfig::default()).expect("evaluation");
    Some((ev, proj, mv.state.clone()))
}

/// The exits of `kind` that landed in `r`, for `position`.
pub fn landed_for<'a>(
    r: &'a TickReport,
    kind: ExitKind,
    position: &'a Pubkey,
) -> impl Iterator<Item = &'a shortwin_services_common::TxOutcome> + 'a {
    r.exits
        .iter()
        .filter(move |e| e.kind == kind && e.position == *position)
        .filter_map(|e| e.result.landed())
}
