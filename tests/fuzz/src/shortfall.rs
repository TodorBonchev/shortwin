//! The deficit tripwire (M10, FUZZ-M7-24H-3's follow-up; ECON §9.4): a bound on every lender's
//! negative SOL balance derived from the **exits' events and the observed receivable**, not from
//! the lender-pool accounting it checks.
//!
//! An exit (`close_short`, `liquidate`, `expire`, an executing `sol_settle`) resolves a position
//! the lenders were credited for continuously: the receivable falls by the position's credit and
//! the lenders' entitlement is trued up by `lender part − credited` (`pool::repay` / `pool::settle`:
//! a shortfall is a debit, shared pro rata to the shares at that instruction). A lender's
//! `pending` can only fall through such debits — accrual, vest releases and covers add, a claim
//! never takes it below 0 — so after any history
//!
//! `−pending ≤ Σ over landed exits of ⌈shortfall · shares/S⌉ (at the exit) + rounding`,
//!
//! where for each landed transaction with exits on a market
//!
//! - `credited` = the market's `receivable` after a **same-second `crank_price` on a clone of the
//!   pre-state** (the transaction's own accrual, which only its first market instruction makes)
//!   minus the `receivable` after the transaction: what the exits took off the receivable;
//! - each exit's lender part comes from its event: `Close.rent − Close.rent_protocol`;
//!   `Liquidate.rent` / `Expire.rent` less the position's locked `rent_protocol_bps` share (read
//!   on the pre-state, as `pool::repay` floors it); a settlement's principal
//!   `SolSettle.lender_amount − min(lump, lender_amount)` (`pool::settle`);
//! - a lender's `shares/S` is walked through the transaction: the pre-state's shares (current
//!   share epoch; a lender behind it counts as holding every share), then each instruction's
//!   `Deposit` / `Withdraw` event (`shares`, `s_after`) — so a JIT deposit bundled around an exit
//!   is charged at its shares at the exit — and after an `EpochRolled` every lender counts as
//!   holding every share.
//!
//! With one exit in the transaction its shortfall is `max(0, credited − lender part)`; with
//! several, the net `max(0, credited − Σ parts)` when no lender's fraction moves between them,
//! else each exit is charged `max(0, credited − its part)` (an upper bound: the other exits'
//! credits are not negative). [`debits`] computes the increments; the world keeps their sum per
//! `(market, lender)` since the lender's balance was last seen not negative ([`Ledger`]: a touch
//! on each exit transaction's pre-state restarts it) and `cover_negative` fails the run if a
//! lender's deficit exceeds it plus [`DEFICIT_DUST`].
//!
//! `credited` is the program's own `receivable`; it is held to an independent figure, the
//! resolved positions' `rent::credited_for` on their pre-state terms
//! (`World::check_credited`), so a receivable the program over-credits cannot widen the bound
//! with the deficit.
//!
//! Before M10 the wind-down funded deficits of any size with no check at all (22961b0);
//! FUZZ-M7-24H-3's reproducer (≈ 1,028 SOL since AIR-07's drain gate, a late depositor's 58.6 %
//! of the shortfall) sits at ≈ 100 % of its bound.

use std::collections::BTreeMap;

use anchor_lang::prelude::Pubkey;

/// What a lender's deficit may exceed its bound by: the entitlement's floor roundings (one
/// lamport per touch and per credit), the receivable's floored credits against the position's
/// rounded-up `credited_for`, and the pro-rata ceilings. 0.001 SOL: far above the observed
/// excess, far below any deficit a wrong debit would leave (`summary.json`'s
/// `wind_down_deficits` records the largest deficit and its share of the bound).
pub const DEFICIT_DUST: u64 = 1_000_000;

/// A landed transaction's exits on one market, as [`debits`] reads them.
#[derive(Clone, Debug, Default)]
pub struct TxExits {
    /// `receivable` (pre-state after a same-second crank) − `receivable` (post-state), ≥ 0.
    pub credited: u64,
    /// `(instruction index, lender part)` of every exit that resolved a position, in order.
    pub exits: Vec<(usize, u64)>,
    /// `(instruction index, lender, Δshares, S after)` of every `Deposit` / `Withdraw`, in order.
    pub moves: Vec<(usize, Pubkey, i128, u128)>,
    /// Instruction indices that emitted `EpochRolled`.
    pub rolls: Vec<usize>,
    /// The lenders' shares in the pre-state's share epoch (`None`: behind it, or unknown).
    pub shares: BTreeMap<Pubkey, Option<u128>>,
    /// `S` in the pre-state.
    pub total: u128,
}

/// `⌈v·num/den⌉` (0 when `den` is 0).
fn ceil_share(v: u128, num: u128, den: u128) -> u128 {
    if den == 0 {
        return 0;
    }
    let w = v.saturating_mul(num.min(den));
    w / den + u128::from(!w.is_multiple_of(den))
}

/// Each lender's share of the transaction's realised shortfall (module docs): the increment of
/// its deficit bound.
pub fn debits(tx: &TxExits) -> BTreeMap<Pubkey, u128> {
    let mut shares = tx.shares.clone();
    let mut total = tx.total;
    // The fraction of every lender at each exit: `Some((shares, S))`, `None` for every share.
    type Frac = Option<(u128, u128)>;
    let mut at: Vec<(u64, BTreeMap<Pubkey, Frac>)> = Vec::new();
    let last = tx
        .exits
        .iter()
        .map(|e| e.0)
        .chain(tx.moves.iter().map(|m| m.0))
        .chain(tx.rolls.iter().copied())
        .max();
    for ix in 0..=last.unwrap_or(0) {
        for (_, part) in tx.exits.iter().filter(|e| e.0 == ix) {
            let f = shares
                .iter()
                .map(|(k, s)| (*k, s.map(|s| (s, total))))
                .collect();
            at.push((*part, f));
        }
        for (_, who, d, s_after) in tx.moves.iter().filter(|m| m.0 == ix) {
            if let Some(Some(s)) = shares.get_mut(who) {
                *s = if *d >= 0 {
                    s.saturating_add(d.unsigned_abs())
                } else {
                    s.saturating_sub(d.unsigned_abs())
                };
            }
            total = *s_after;
        }
        if tx.rolls.contains(&ix) {
            for s in shares.values_mut() {
                *s = None;
            }
        }
    }
    let mut out: BTreeMap<Pubkey, u128> = BTreeMap::new();
    let charge = |out: &mut BTreeMap<Pubkey, u128>, f: &BTreeMap<Pubkey, Frac>, short: u128| {
        for (who, fr) in f {
            let d = match fr {
                Some((s, total)) => ceil_share(short, *s, *total),
                None => short,
            };
            *out.entry(*who).or_default() += d;
        }
    };
    let credited = u128::from(tx.credited);
    let steady = at.windows(2).all(|w| w[0].1 == w[1].1);
    if steady && let Some((_, f)) = at.first() {
        let parts: u128 = at.iter().map(|(p, _)| u128::from(*p)).sum();
        charge(&mut out, f, credited.saturating_sub(parts));
    } else {
        for (part, f) in &at {
            charge(&mut out, f, credited.saturating_sub(u128::from(*part)));
        }
    }
    out
}

/// Per-lender bounds of one run: `(market, lender)` → Σ [`debits`] since the lender's balance
/// was last seen not negative ([`Ledger::restart`]), and the same sum over the whole run.
///
/// The restart is sound because only a debit lowers `pending` below 0 (module docs): a deficit
/// `D = max(0, −pending)` that is 0 at some point grows by at most the debits charged since, so
/// the debits before it bound nothing left. The world restarts a lender's bound before each
/// transaction with an exit when a touch on the pre-state reads its `pending ≥ 0`.
#[derive(Clone, Debug, Default)]
pub struct Ledger {
    pub bound: BTreeMap<(Pubkey, Pubkey), u128>,
    pub total: BTreeMap<(Pubkey, Pubkey), u128>,
}

impl Ledger {
    /// Adds a transaction's increments on `market`.
    pub fn add(&mut self, market: Pubkey, inc: &BTreeMap<Pubkey, u128>) {
        for (who, d) in inc {
            let b = self.bound.entry((market, *who)).or_default();
            *b = b.saturating_add(*d);
            let t = self.total.entry((market, *who)).or_default();
            *t = t.saturating_add(*d);
        }
    }

    /// `lender`'s balance on `market` is not negative now: its bound restarts at 0.
    pub fn restart(&mut self, market: Pubkey, lender: &Pubkey) {
        self.bound.remove(&(market, *lender));
    }

    /// The bound of `lender` on `market` (0 if no exit charged it since its last restart).
    pub fn of(&self, market: &Pubkey, lender: &Pubkey) -> u128 {
        self.bound.get(&(*market, *lender)).copied().unwrap_or(0)
    }

    /// The run's cumulative bound of `lender` on `market` (no restarts).
    pub fn total_of(&self, market: &Pubkey, lender: &Pubkey) -> u128 {
        self.total.get(&(*market, *lender)).copied().unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(b: u8) -> Pubkey {
        Pubkey::new_from_array([b; 32])
    }

    fn tx(credited: u64, exits: &[(usize, u64)], shares: &[(u8, Option<u128>)]) -> TxExits {
        TxExits {
            credited,
            exits: exits.to_vec(),
            shares: shares.iter().map(|(k, s)| (key(*k), *s)).collect(),
            total: shares.iter().filter_map(|s| s.1).sum(),
            ..TxExits::default()
        }
    }

    /// One exit paying less than it was credited: each lender bears its share, rounded up; one
    /// paying more charges nobody.
    #[test]
    fn one_exit_is_shared_pro_rata() {
        let d = debits(&tx(1_000, &[(1, 400)], &[(1, Some(41)), (2, Some(59))]));
        assert_eq!(d[&key(1)], 246); // ⌈600·41/100⌉
        assert_eq!(d[&key(2)], 354);
        let d = debits(&tx(1_000, &[(1, 1_500)], &[(1, Some(41)), (2, Some(59))]));
        assert_eq!((d[&key(1)], d[&key(2)]), (0, 0));
    }

    /// FUZZ-M7-24H-3's shape: a deposit bundled before the exit is charged at its shares at the
    /// exit, not at the pre-state's none; a withdrawal after it does not undo the charge.
    #[test]
    fn a_jit_deposit_is_charged_at_the_exit() {
        let mut t = tx(1_000, &[(1, 0)], &[(1, Some(100)), (2, Some(0))]);
        t.moves = vec![(0, key(2), 300, 400), (2, key(2), -300, 100)];
        let d = debits(&t);
        assert_eq!((d[&key(1)], d[&key(2)]), (250, 750));
    }

    /// Several exits under steady fractions net out; with a fraction moving between them each
    /// exit is charged on its own, and a lender behind the epoch counts every share.
    #[test]
    fn several_exits_and_unknown_shares() {
        let d = debits(&tx(
            1_000,
            &[(0, 300), (1, 900)],
            &[(1, Some(1)), (2, Some(1))],
        ));
        assert_eq!((d[&key(1)], d[&key(2)]), (0, 0));
        let mut t = tx(1_000, &[(0, 300), (2, 900)], &[(1, Some(1)), (2, Some(1))]);
        t.moves = vec![(1, key(2), 2, 4)];
        let d = debits(&t);
        assert_eq!(d[&key(1)], 350 + 25); // ⌈700·1/2⌉ + ⌈100·1/4⌉
        assert_eq!(d[&key(2)], 350 + 75);
        let d = debits(&tx(1_000, &[(0, 0)], &[(1, Some(5)), (2, None)]));
        assert_eq!((d[&key(1)], d[&key(2)]), (1_000, 1_000));
    }

    /// A restart drops the debits before it from the bound, not from the run's total.
    #[test]
    fn a_restart_drops_the_earlier_debits() {
        let (m, a) = (key(9), key(1));
        let mut l = Ledger::default();
        l.add(m, &[(a, 600)].into_iter().collect());
        l.restart(m, &a);
        l.add(m, &[(a, 40)].into_iter().collect());
        assert_eq!((l.of(&m, &a), l.total_of(&m, &a)), (40, 640));
        assert_eq!(l.of(&m, &key(2)), 0);
    }
}
