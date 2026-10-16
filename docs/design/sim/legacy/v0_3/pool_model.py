#!/usr/bin/env python3
"""
short.win -- lender-pool accounting reference model + property tests (v0.3).

Companion to docs/design/ECONOMICS.md section 9.  Standard library only.

    python3 pool_model.py            # invariant fuzzing + attack / edge-case demos
    python3 pool_model.py --ops 200000 --seed 7

Model (all integers; rounding always in favour of the pool):
  * token claims: shares are pro-rata claims on TC = T + B (unborrowed + borrowed
    tokens, INTERNAL counters, never the vault balance -> donations ignored);
    mint = floor(a * (S + VS) / (TC + VA)); burn value = floor(s * (TC + VA) / (S + VS)).
  * SOL claims: reward-per-share accumulator acc (WAD lamports per share, signed).
    Rent is credited continuously (lender 85 % share of the rent index growth x B)
    and booked as `receivable`; penalties / expiry fees / settlement SOL are
    credited when they happen; rent shortfalls are debited.
  * SOL settlement: B -= Q (token claims extinguished pro-rata via TC), vault SOL
    credited pro-rata via acc.

v0.2 changes (issue IDs of the 2026-09-27 design review):
  * C0-6  share epochs: when T + B == 0 (all claims settled in SOL) or shares per
          token exceed R_MAX, the epoch rolls: old shares' SOL is frozen at
          acc_end, shares convert at num/den (0/1 after a full wipe, 1/REBASE
          otherwise), acc restarts.  Lenders convert lazily in O(epochs behind).
  * C0-9  pro-rata withdrawal (SPEC §2.4): a lender may take only its pro-rata
          part of UNBORROWED inventory; the B-part of what it withdrew is locked
          in shares and unlocks only as borrowed tokens are resolved (returned or
          SOL-settled), tracked by a survival index PI.  Repeating withdrawals in a
          loop cannot drain T.
  * C0-13 claim_sol pays only realised SOL, pro-rata: pending * sol / (sol +
          receivable); a lender with pending < 0 cannot withdraw shares (it may
          `cover` the deficit in SOL first).
  * C0-12 per-lender solvency and share-ratio invariants are checked after every
          operation of the fuzz, and ghost checks enforce the withdrawal rules.

v0.3 changes (second review, 2026-09-27):
  * V2-2  claims have memory.  Realised lender SOL is allocated to shares when it
          arrives (accumulator accR -> per-lender `real`); a claim pays
          min(pending, real - claimed, sol_vault).  Repeating claim_sol cannot take
          another lender's realised SOL.  Continuous rent crediting still decides
          entitlement (pending), so a just-in-time depositor has pending ~ 0; the
          realised SOL it was allocated beyond its entitlement is handed back to
          the other shares on its next touch (surplus recycling), so nothing is
          stranded.  (The v0.2 rule pay = pending * sol / (sol + receivable) was
          memoryless: 100 calls took 151.5 of 153 realised SOL.)
  * V2-3  locks decay only on NET reductions of the borrowed inventory: Pi moves
          only when B_Q falls below its running minimum b_min since the first lock
          of the Pi epoch, so open/close churn of other positions cannot unlock a
          runner's locked shares while the position it is exposed to is still out.
  * V1-10 strict per-lender solvency: sum(max(pending, 0)) + unassigned <=
          sol_vault + receivable, plus sum(real - claimed) + unassigned <= sol_vault;
          the signed aggregate identity is kept as a separate check.
  * V2-12 a lender whose shares convert to 0 at an epoch roll jumps straight to
          the current epoch (no further ShareEpoch records needed); SOL credited
          while S == 0 after a full wipe tops up the latest (full-wipe) epoch record
          for the old epoch's lenders instead of going to the next depositor.
"""
from __future__ import annotations

import argparse
import random

WAD = 10**18
BPS = 10_000
VS = 1_000        # PROPOSED virtual shares (decimals offset 3)
VA = 1            # PROPOSED virtual token unit
LENDER_RENT_BPS = 8_500
R_MAX = 1_000_000   # PROPOSED: max shares per raw token unit before a rebase (initial ~VS/VA = 1,000)
REBASE = 1_000      # PROPOSED: rebase divides shares by 1,000
U128 = 2**128 - 1


def cdiv(a, b):
    return -((-a) // b)


class Lender:
    __slots__ = ("shares", "snap", "pending", "epoch", "lock", "lock_pi", "lock_epoch", "lock_term",
                 "snap_r", "real", "claimed", "withdrawn")

    def __init__(self, acc, acc_r, epoch):
        self.shares, self.snap, self.pending, self.epoch = 0, acc, 0, epoch
        self.lock, self.lock_pi, self.lock_epoch, self.lock_term = 0, WAD, -1, 0
        self.snap_r, self.real = acc_r, 0          # V2-2: realised SOL allocated to this lender (cumulative)
        self.claimed = 0                           # V2-2: SOL paid to this lender (cumulative; on-chain field)
        self.withdrawn = 0                         # ghost (tests only)


class Pool:
    def __init__(self, rent_mode: str = "continuous", util_cap_bps: int = 9_000, prorata: bool = True):
        assert rent_mode in ("continuous", "realized")
        self.mode = rent_mode
        self.util_cap = util_cap_bps
        self.prorata = prorata   # False reproduces v0.1 first-come-first-served withdrawals
        self.T = 0               # available tokens (internal)
        self.B = 0               # borrowed Q outstanding
        self.S = 0               # total shares (current epoch)
        self.acc = 0             # WAD lamports per share (signed): ENTITLEMENT (continuous rent + true-ups + lumps)
        self.carry = 0           # remainder of acc division (in lamports*WAD units)
        self.acc_r = 0           # V2-2: WAD lamports per share of REALISED lender SOL (only when SOL arrives)
        self.carry_r = 0
        self.inflow = 0          # ghost: cumulative lamports that reached sol_vault for lenders
        self.dust = 0            # lamports*WAD dropped at epoch rolls (stay in the vault, unowned)
        self.unassigned = 0      # SOL credited while S == 0 (goes to the next depositor set)
        self.sol_vault = 0       # realised lamports held for lenders
        self.receivable = 0      # rent credited to lenders, not yet paid in
        self.index = 0           # market rent index (lamports per raw token, WAD)
        self.epoch = 0           # share epoch (C0-6)
        self.epochs = {}         # e -> [acc_end, num, den, acc_r_end, s_old, carry, carry_r]
        self.wipe_live = False   # V2-12: latest record is a full wipe and no deposit since (S == 0)
        self.pi = WAD            # borrowed-inventory survival index (C0-9)
        self.pi_epoch = 0
        self.lock_norm = 0       # sum of lenders' lock terms (lock * WAD / lock_pi, ceil)
        self.b_min = 0           # V2-3: running minimum of B since the first lock of the current Pi epoch
        self.lenders = {}
        self.positions = {}      # pid -> [q, index_open]
        self.vault_token_balance = 0   # real SPL balance (can include donations)

    # ---- SOL accumulator --------------------------------------------------
    def _credit(self, lamports: int, realised: bool = False):
        """Entitlement credit (acc).  realised=True also allocates the SOL itself to
        shares (acc_r), i.e. the lamports reached sol_vault now (V2-2)."""
        if lamports == 0:
            return
        if self.S == 0:
            if self.wipe_live:                 # V2-12: top up the full-wipe record for the old lenders
                rec = self.epochs[self.epoch - 1]
                num = lamports * WAD + rec[5]
                d = num // rec[4]
                rec[5] = num - d * rec[4]
                rec[0] += d
                if realised:
                    num = lamports * WAD + rec[6]
                    d = num // rec[4]
                    rec[6] = num - d * rec[4]
                    rec[3] += d
                return
            self.unassigned += lamports         # genesis only: nobody holds shares yet
            return
        num = lamports * WAD + self.carry
        d = num // self.S                      # floor (toward -inf for debits)
        self.carry = num - d * self.S
        self.acc += d
        if realised:
            self._credit_r(lamports)

    def _credit_r(self, lamports: int):
        """Allocate realised lender SOL to the current shares (never negative)."""
        if lamports <= 0 or self.S == 0:
            return
        num = lamports * WAD + self.carry_r
        d = num // self.S
        self.carry_r = num - d * self.S
        self.acc_r += d

    def _touch(self, who, recycle: bool = True) -> Lender:
        L = self.lenders.get(who)
        if L is None:
            L = self.lenders[who] = Lender(self.acc, self.acc_r, self.epoch)
        while L.epoch < self.epoch:            # lazy epoch conversion (C0-6)
            acc_end, num, den, acc_r_end = self.epochs[L.epoch][:4]
            L.pending += L.shares * (acc_end - L.snap) // WAD
            L.real += L.shares * (acc_r_end - L.snap_r) // WAD
            if self.wipe_live and L.epoch == self.epoch - 1:
                # V2-12: the full-wipe record is still open for top-ups (no deposit since);
                # keep the old shares against it and convert once it is final.
                L.snap, L.snap_r = acc_end, acc_r_end
                return L
            L.shares = L.shares * num // den
            L.lock = L.lock * num // den
            L.snap = L.snap_r = 0
            L.epoch += 1
            if L.shares == 0 and L.lock == 0:  # V2-12: nothing left to convert -> jump
                L.epoch = self.epoch
                L.snap, L.snap_r = self.acc, self.acc_r
        L.pending += L.shares * (self.acc - L.snap) // WAD      # floor
        L.snap = self.acc
        L.real += L.shares * (self.acc_r - L.snap_r) // WAD
        L.snap_r = self.acc_r
        if recycle:                            # claim_sol keeps Market read-only: no recycle there
            self._recycle(L)
        return L

    def _recycle(self, L: Lender):
        """V2-2: realised SOL allocated to L beyond what L may ever claim (its
        entitlement, e.g. a just-in-time depositor) goes back to the other shares."""
        surplus = (L.real - L.claimed) - max(L.pending, 0)
        if surplus > 0 and self.S > L.shares:
            L.real -= surplus
            num = surplus * WAD + self.carry_r
            d = num // (self.S - L.shares)
            self.carry_r = num - d * (self.S - L.shares)
            self.acc_r += d
            L.snap_r = self.acc_r               # L does not take part in its own surplus

    # ---- C0-6 share epochs ----------------------------------------------------------
    def _maybe_roll(self):
        if self.S == 0:
            return
        if self.T + self.B == 0:
            self._roll(0, 1)
        elif self.S + VS > R_MAX * (self.T + self.B + VA):
            den = REBASE
            while self.S // den + VS > R_MAX * (self.T + self.B + VA):
                den *= REBASE                  # one roll, rebase factor 1,000^k
            self._roll(1, den)

    def _roll(self, num, den):
        self.epochs[self.epoch] = [self.acc, num, den, self.acc_r, self.S, 0, 0]
        self.epoch += 1
        self.S = self.S * num // den
        self.lock_norm = self.lock_norm * num // den
        self.dust += self.carry + self.carry_r
        self.carry = self.carry_r = 0
        self.acc = self.acc_r = 0
        self.wipe_live = self.S == 0          # V2-12

    # ---- C0-9 pro-rata withdrawal locks ----------------------------------------------
    def _resolve(self, q: int):
        """q borrowed tokens resolved (returned or SOL-settled).  V2-3: locks decay only
        when B falls below its running minimum since the first lock of the Pi epoch
        (a NET reduction of the inventory that existed when the locks were taken);
        churn of positions opened later cannot unlock anything.  Conservative: a lock
        may unlock later than the inventory it stands for."""
        if q >= self.B:
            self.pi_epoch += 1
            self.pi = WAD
            self.lock_norm = 0
            self.b_min = 0
            return
        bn = self.B - q
        if self.lock_norm > 0 and bn < self.b_min:
            self.pi = cdiv(self.pi * bn, self.b_min)
            self.b_min = bn

    def _lock_of(self, L: Lender) -> int:
        if L.lock_epoch != self.pi_epoch or L.lock == 0:
            return 0
        return min(cdiv(L.lock * self.pi, L.lock_pi), L.shares)

    def locked_total(self) -> int:
        return min(cdiv(self.lock_norm * self.pi, WAD), self.S)

    def free_allowance(self, who) -> int:
        """Tokens lender `who` may withdraw now (pro-rata of unborrowed inventory)."""
        L = self._touch(who)
        if L.epoch != self.epoch:               # parked on a live full-wipe record: no current shares
            return 0
        if not self.prorata:
            return min(L.shares * (self.T + self.B + VA) // (self.S + VS), self.T)
        u = L.shares - self._lock_of(L)
        su = max(self.S - self.locked_total(), u)
        if su == 0:
            return 0
        return min(u * self.T // su, L.shares * (self.T + self.B + VA) // (self.S + VS), self.T)

    # ---- lender actions -------------------------------------------------------
    def deposit(self, who, amount: int) -> int:
        L = self._touch(who)
        minted = amount * (self.S + VS) // (self.T + self.B + VA)
        if minted == 0:
            raise ValueError("deposit too small")
        if self.S + minted > U128:
            raise ValueError("share overflow")
        first = self.S == 0
        if first and self.wipe_live:
            self.wipe_live = False             # V2-12: the full-wipe record is final from now on
            L = self._touch(who)
        L.shares += minted
        self.S += minted
        self.T += amount
        self.vault_token_balance += amount
        if first and self.unassigned:
            u, self.unassigned = self.unassigned, 0
            self._credit(u, realised=True)     # genesis only
        return minted

    def cover(self, who, lamports: int):
        """Lender pays SOL to bring a negative pending balance back to >= 0 (C0-13)."""
        L = self._touch(who)
        self.sol_vault += lamports
        self.inflow += lamports
        L.pending += lamports
        L.real += lamports                     # the lender's own SOL: realised and claimable by it

    def withdraw(self, who, shares: int) -> int:
        L = self._touch(who)
        if L.pending < 0:
            raise ValueError("negative SOL balance: cover it first")      # C0-13
        if L.epoch != self.epoch or shares <= 0 or shares > L.shares:
            raise ValueError("not enough shares")
        amount = shares * (self.T + self.B + VA) // (self.S + VS)
        if amount > self.T:
            raise ValueError("insufficient unborrowed inventory")
        if self.prorata:
            allow = self.free_allowance(who)
            if amount > allow:
                raise ValueError("above pro-rata share of unborrowed inventory")
            # lock the borrowed-inventory part that corresponds to the withdrawn fraction
            cur = self._lock_of(L)
            u = L.shares - cur
            burn_max = cdiv(allow * (self.S + VS), self.T + self.B + VA) if allow else 0
            add = 0
            if self.B > 0 and burn_max > 0 and u > burn_max:   # nothing borrowed -> nothing to lock
                add = cdiv(min(shares, burn_max) * (u - burn_max), burn_max)
            new_lock = min(cur + add, L.shares - shares)
            if new_lock and self.lock_norm == 0:
                self.b_min = self.B                # V2-3: first lock of this Pi epoch
            if L.lock_epoch == self.pi_epoch:
                self.lock_norm -= min(L.lock_term, self.lock_norm)
            term = cdiv(new_lock * WAD, self.pi) if new_lock else 0
            self.lock_norm += term
            L.lock, L.lock_pi, L.lock_epoch, L.lock_term = new_lock, self.pi, self.pi_epoch, term
        L.shares -= shares
        L.withdrawn += amount
        self.S -= shares
        self.T -= amount
        self.vault_token_balance -= amount
        return amount

    def claim_sol(self, who) -> int:
        """C0-13 + V2-2: pays only realised SOL that was allocated to this lender and
        not yet claimed, never more than its entitlement: min(pending, real - claimed,
        sol_vault).  Memoryful: a second call in the same state pays 0."""
        L = self._touch(who, recycle=False)
        pay = min(L.pending, L.real - L.claimed, self.sol_vault)
        if pay <= 0:
            return 0
        L.pending -= pay
        L.claimed += pay
        self.sol_vault -= pay
        return pay

    def sweep_sol(self, lamports: int):
        """Trader sweep / venue cashback credited to lenders as realised SOL (A44)."""
        self.sol_vault += lamports
        self.inflow += lamports
        self._credit(lamports, realised=True)

    def donate_tokens(self, amount: int):
        """Direct SPL transfer to the vault: does NOT touch internal counters."""
        self.vault_token_balance += amount

    # ---- borrower side ----------------------------------------------------------
    def borrow(self, pid, q: int):
        if q <= 0 or q > self.T or (self.B + q) * BPS > self.util_cap * (self.T + self.B):
            raise ValueError("utilization cap")
        self.T -= q
        self.B += q
        self.vault_token_balance -= q
        self.positions[pid] = [q, self.index]

    def accrue(self, d_index: int):
        """Market rent index grows by d_index (lamports per raw token, WAD)."""
        self.index += d_index
        if self.mode == "continuous":
            credited = self.B * d_index * LENDER_RENT_BPS // (WAD * BPS)   # floor
            self.receivable += credited
            self._credit(credited)

    def _credited_for(self, q, i_open):
        # ceil: the per-position true-up never gives lenders more than was paid
        return cdiv(q * (self.index - i_open) * LENDER_RENT_BPS, WAD * BPS) if self.mode == "continuous" else 0

    def repay(self, pid, rent_paid: int, lender_lump: int):
        """Close / liquidation / expiry: Q tokens return, rent_paid lamports of rent
        (total, before the 85/15 split) and lender_lump (penalty share / expiry fee)."""
        q, i_open = self.positions.pop(pid)
        self._resolve(q)
        self.T += q
        self.B -= q
        self.vault_token_balance += q
        lender_rent = rent_paid - rent_paid * (BPS - LENDER_RENT_BPS) // BPS
        credited = self._credited_for(q, i_open)
        self.receivable -= min(credited, self.receivable)
        self.sol_vault += lender_rent + lender_lump
        self.inflow += lender_rent + lender_lump
        self._credit(lender_rent + lender_lump - credited)
        self._credit_r(lender_rent + lender_lump)

    def settle(self, pid, sol_in: int):
        """SOL settlement: token debt extinguished, sol_in lamports to lenders."""
        q, i_open = self.positions.pop(pid)
        self._resolve(q)
        self.B -= q
        credited = self._credited_for(q, i_open)
        self.receivable -= min(credited, self.receivable)
        self.sol_vault += sol_in
        self.inflow += sol_in
        self._credit(sol_in - credited)
        self._credit_r(sol_in)
        self._maybe_roll()

    # ---- invariants -----------------------------------------------------------------
    def check(self):
        for who in list(self.lenders):
            self._touch(who)
        Ls = self.lenders.values()
        cur = [L for L in Ls if L.epoch == self.epoch]      # lenders parked on a live wipe record hold no current shares
        tok_claims = sum(L.shares * (self.T + self.B + VA) // (self.S + VS) for L in cur)
        assert tok_claims <= self.T + self.B, ("token claims exceed assets", tok_claims, self.T + self.B)
        assert sum(L.shares for L in cur) <= self.S, ("shares", sum(L.shares for L in cur), self.S)
        # SOL (C0-12, V1-10, V2-2).
        # (a) claim side, STRICT per lender: every lender's unclaimed realised allocation is
        #     backed by sol_vault and nobody has been paid more than its own allocation, so a
        #     negative balance elsewhere can never fund (mask) an over-claim.
        unclaimed_real = sum(L.real - L.claimed for L in Ls)
        assert unclaimed_real + self.unassigned <= self.sol_vault, (
            "realised allocations exceed the vault", unclaimed_real, self.unassigned, self.sol_vault)
        assert all(L.claimed <= L.real for L in Ls), "claimed above realised allocation"
        assert sum(L.claimed for L in Ls) <= self.inflow
        # (b) entitlement side, signed aggregate identity.  The literal strict form
        #     sum(max(pending, 0)) <= sol_vault + receivable does NOT hold in general:
        #     rent shortfalls are debited pro-rata to CURRENT shares (disclosed, ECON §9.4),
        #     so a lender can go negative (it is then withdrawal-blocked until it covers);
        #     the excess over assets equals the sum of those negative balances.
        pos = sum(max(L.pending, 0) for L in Ls)
        neg = sum(max(-L.pending, 0) for L in Ls)
        assert pos - neg + self.unassigned <= self.sol_vault + self.receivable, (
            "SOL claims exceed assets (aggregate)", pos, neg, self.sol_vault, self.receivable)
        self.max_neg = max(getattr(self, "max_neg", 0), neg)
        self.max_strict_excess = max(getattr(self, "max_strict_excess", 0),
                                     pos + self.unassigned - self.sol_vault - self.receivable)
        # share-ratio bound (C0-6)
        assert self.S + VS <= R_MAX * (self.T + self.B + VA) or self.S == 0 or self.T + self.B == 0, (
            "share ratio", self.S, self.T, self.B)
        assert self.S <= U128
        assert self.vault_token_balance >= self.T
        assert self.B == sum(p[0] for p in self.positions.values())
        if self.prorata:
            # pro-rata allowances never promise more than the unborrowed inventory (+ rounding dust)
            assert sum(self.free_allowance(w) for w in self.lenders) <= self.T + len(self.lenders)


# ------------------------------------------------------------------------------------
def token_value(p: "Pool", L: "Lender") -> int:
    return L.shares * (p.T + p.B + VA) // (p.S + VS) if L.epoch == p.epoch else 0


def fuzz(ops: int, seed: int, mode: str, util_cap_bps: int = 9_000):
    rng = random.Random(seed)
    p = Pool(mode, util_cap_bps=util_cap_bps)
    users = [f"u{i}" for i in range(6)]
    pid = 0
    counts = {}
    for _ in range(ops):
        op = rng.choice(["dep", "dep", "wd", "wd", "borrow", "accrue", "repay", "settle", "claim", "donate",
                         "cover", "wipe", "claimN", "sweep", "churn"])
        try:
            if op == "dep":
                p.deposit(rng.choice(users), rng.choice([1, 7, 10**6, 10**9, 10**12, rng.randrange(1, 10**13)]))
            elif op == "wd":
                u = rng.choice(users)
                L = p.lenders.get(u)
                if L and L.shares:
                    before = (L.shares, L.pending)
                    allow = p.free_allowance(u)
                    s = rng.randrange(1, L.shares + 1) if rng.random() < 0.5 else \
                        max(1, allow * (p.S + VS) // (p.T + p.B + VA))
                    got = p.withdraw(u, min(s, L.shares))
                    assert before[1] >= 0, "withdrew with negative pending"          # ghost C0-13
                    assert got <= allow, ("withdrew above pro-rata allowance", got, allow)   # ghost C0-9
            elif op == "borrow" and p.T > 0:
                pid += 1
                p.borrow(pid, rng.randrange(1, p.T + 1))
            elif op == "accrue":
                p.accrue(rng.randrange(0, 10**15))
            elif op == "repay" and p.positions:
                k = rng.choice(list(p.positions))
                q, io = p.positions[k]
                owed = cdiv(q * (p.index - io), WAD)
                paid = owed if rng.random() < 0.8 else rng.randrange(0, owed + 1)   # shortfall sometimes
                p.repay(k, paid, rng.randrange(0, 10**9))
            elif op == "settle" and p.positions:
                k = rng.choice(list(p.positions))
                p.settle(k, rng.randrange(0, 10**12))
            elif op == "wipe" and p.T > 0 and p.S > 0:
                # run + full SOL settlement: borrow everything that the cap allows, settle it all
                pid += 1
                q = p.T * p.util_cap // BPS
                if q > 0 and (p.B + q) * BPS <= p.util_cap * (p.T + p.B):
                    p.borrow(pid, q)
                for k in list(p.positions):
                    p.settle(k, rng.randrange(0, 10**10))
            elif op == "claim":
                p.claim_sol(rng.choice(users))
            elif op == "claimN":                 # V2-2: the same lender claims N times in one transaction
                u = rng.choice(users)
                first = p.claim_sol(u)
                again = sum(p.claim_sol(u) for _ in range(rng.randrange(1, 20)))
                assert again == 0, ("repeated claim paid more", first, again)
            elif op == "sweep":                  # trader sweep / cashback (realised, share-based)
                p.sweep_sol(rng.randrange(1, 10**9))
            elif op == "churn" and p.T > 0 and p.B > 0:   # V2-3: someone else's open + close
                locks = {w: p._lock_of(p._touch(w)) for w in p.lenders}
                pid += 1
                c = rng.randrange(1, p.T + 1)
                p.borrow(pid, c)
                p.repay(pid, 0, 0)
                for w, lk in locks.items():
                    assert p._lock_of(p._touch(w)) >= lk, ("churn released a lock", w, lk)
            elif op == "cover":
                u = rng.choice(users)
                L = p.lenders.get(u)
                if L:
                    p._touch(u)
                    if L.pending < 0:
                        p.cover(u, -L.pending)
            elif op == "donate":
                p.donate_tokens(rng.randrange(1, 10**12))
            counts[op] = counts.get(op, 0) + 1
        except ValueError:
            counts[op + "!"] = counts.get(op + "!", 0) + 1
        p.check()
    return counts, p.epoch, getattr(p, "max_neg", 0), getattr(p, "max_strict_excess", 0)


def jit_demo():
    """Attacker deposits 9x the pool right before a 30-day position closes."""
    out = []
    for mode in ("realized", "continuous"):
        p = Pool(mode)
        p.deposit("honest", 1_000 * 10**6)
        p.borrow(1, 900 * 10**6)
        d_index = WAD * 5_000 * 30 * 86400 // (BPS * 31_536_000)
        for _ in range(30):
            p.accrue(d_index // 30)
        p.deposit("attacker", 9_000 * 10**6)
        q, io = p.positions[1]
        rent = cdiv(q * (p.index - io), WAD)
        p.repay(1, rent, 0)
        got_tokens = p.withdraw("attacker", p.lenders["attacker"].shares)
        got_sol = p.claim_sol("attacker")
        honest_sol = p.claim_sol("honest")
        out.append((mode, rent, got_sol, honest_sol, got_tokens))
    return out


def inflation_demo():
    """Classic ERC-4626 first-depositor attack: 1-unit deposit + large donation."""
    p = Pool()
    p.deposit("attacker", 1)
    p.donate_tokens(10**12)                 # ignored by internal accounting
    minted = p.deposit("victim", 10**9)
    back = p.withdraw("victim", minted)
    return minted, back


def run_demo():
    """C0-9: lender A sees an imminent S1 and runs; compare v0.1 FCFS with v0.2 pro-rata.
    Two equal lenders, 90 % utilisation, then the whole borrowed inventory is SOL-settled
    for 50 % of its token value (value 1 lamport per raw token)."""
    out = []
    for prorata in (False, True):
        p = Pool(prorata=prorata)
        p.deposit("A", 10**12)
        p.deposit("B", 10**12)
        p.borrow(1, 18 * 10**11)
        # A runs: repeated withdrawals in one transaction
        took = 0
        for _ in range(50):
            try:
                s = max(1, p.free_allowance("A") * (p.S + VS) // (p.T + p.B + VA))
                if s > p.lenders["A"].shares:
                    s = p.lenders["A"].shares
                took += p.withdraw("A", s)
            except ValueError:
                break
        p.settle(1, 9 * 10**11)             # S1: SOL worth 50 % of the tokens
        vals = {}
        for who in ("A", "B"):
            L = p._touch(who)
            tokens_left = token_value(p, L)
            got = (took if who == "A" else 0) + tokens_left + L.pending
            vals[who] = got
        out.append((prorata, took, vals["A"], vals["B"]))
    return out


def claim_loop_demo():
    """V2-2: lender A calls claim_sol N times in one transaction.  Two equal lenders,
    two positions (90 % utilisation), rent credited continuously, one position repays
    (half of the credited rent realised).  v0.2 rule: pay = pending*sol/(sol+recv)."""
    out = []
    for label, v2rule in (("v0.2 (memoryless)", True), ("v0.3 (realised allocation)", False)):
        for n in (1, 2, 5, 20, 100):
            p = Pool("continuous")
            p.deposit("A", 10**12)
            p.deposit("B", 10**12)
            p.borrow("pos1", 9 * 10**11)
            p.borrow("pos2", 9 * 10**11)
            p.accrue(200 * WAD // 1000)
            q, io = p.positions["pos1"]
            p.repay("pos1", cdiv(q * (p.index - io), WAD), 0)
            fair = p._touch("A").pending * p.sol_vault // (p.sol_vault + p.receivable)
            got = 0
            for _ in range(n):
                if v2rule:
                    L = p._touch("A")
                    den = p.sol_vault + p.receivable
                    pay = min(L.pending * p.sol_vault // den, p.sol_vault, L.pending) if L.pending > 0 else 0
                    L.pending -= pay
                    p.sol_vault -= pay
                    got += pay
                else:
                    got += p.claim_sol("A")
            out.append((label, n, fair, got, p.sol_vault))
    return out


def run_util_table():
    """V2-3: runner advantage by utilisation, without and with churn of other
    positions (open + close within the utilisation cap) between withdrawals.
    Two equal lenders; the inventory borrowed at the start is then SOL-settled at
    50 % of its token value.  Compares the v0.2 lock rule (decay on every
    resolution) with v0.3 (decay only below the running minimum of B)."""
    rows = []

    def v2_resolve(self, q):
        if q >= self.B:
            self.pi_epoch += 1
            self.pi = WAD
            self.lock_norm = 0
        else:
            self.pi = cdiv(self.pi * (self.B - q), self.B)

    for util in (90, 80, 60, 50, 30):
        cells = [f"{util}%"]
        for rule in ("v0.2", "v0.3"):
            for churn in (0, 30):
                p = Pool(prorata=True)
                if rule == "v0.2":
                    p._resolve = v2_resolve.__get__(p)
                p.deposit("A", 10**12)
                p.deposit("B", 10**12)
                big = util * 2 * 10**12 // 100
                p.borrow("big", big)
                took = 0

                def drain():
                    nonlocal took
                    for _ in range(50):
                        try:
                            a = p.free_allowance("A")
                            if a == 0:
                                break
                            s = min(max(1, a * (p.S + VS) // (p.T + p.B + VA)), p.lenders["A"].shares)
                            took += p.withdraw("A", s)
                        except ValueError:
                            break
                drain()
                for r in range(churn):
                    c = min(p.T, 9 * (p.T + p.B) // 10 - p.B)
                    if c <= 0:
                        break
                    p.borrow(f"c{r}", c)
                    p.repay(f"c{r}", 0, 0)
                    drain()
                p.settle("big", big // 2)
                vals = {}
                for w in ("A", "B"):
                    L = p._touch(w)
                    vals[w] = (took if w == "A" else 0) + token_value(p, L) + L.pending
                fair = (vals["A"] + vals["B"]) / 2
                cells.append(f"{100 * (vals['A'] - fair) / fair:+.1f}%")
        rows.append(cells)
    return rows


def wipe_demo():
    """C0-6: the critic's pool_edge (a) -- repeated full SOL settlements."""
    lines = []
    for label, cls_roll in (("v0.1 (no epochs)", False), ("v0.3 (share epochs)", True)):
        p = Pool(util_cap_bps=10_000)
        if not cls_roll:
            p._maybe_roll = lambda: None
        p.deposit("old", 10**14)
        p.borrow(1, 10**14)
        p.settle(1, 50 * 10**9)
        minted = p.deposit("new", 10**14)
        before = p.acc
        p.borrow(2, 5 * 10**13)
        p.accrue(WAD // 10**6)             # some rent on the new position
        p.repay(2, cdiv(5 * 10**13 * (WAD // 10**6), WAD), 10**12)    # + 1,000 SOL lump
        new_pending = p._touch("new").pending
        p.borrow(3, p.T)
        p.settle(3, 10**9)
        m2 = 10**14 * (p.S + VS) // (p.T + p.B + VA)
        lines.append((label, minted, new_pending, m2, m2 > U128, p.epoch))
    return lines


def wipe_tail_demo():
    """V2-12: (1) SOL credited while S == 0 after a full wipe (trader sweep, venue
    cashback) goes to the old epoch's lenders, not to the next (dust) depositor;
    (2) a lender whose shares converted to 0 skips all later epoch records."""
    p = Pool(util_cap_bps=10_000)
    p.deposit("old", 10**14)
    p.borrow(1, 10**14)
    p.settle(1, 50 * 10**9)                 # full wipe: T + B = 0
    p.sweep_sol(10**9)                      # 1 SOL of trader dust / cashback after the wipe
    p.deposit("dust", 1)                    # dust depositor arrives first
    p.sweep_sol(0)
    old = p._touch("old")
    dust = p._touch("dust")
    got_old = old.pending - 50 * 10**9
    # (2) five further wipes; the old lender (0 shares) is touched once afterwards
    for i in range(5):
        p.deposit(f"x{i}", 10**14)
        p.borrow(10 + i, p.T)
        p.settle(10 + i, 10**9)
        p.deposit(f"y{i}", 10**3)
    steps = p.epoch - old.epoch
    before = old.epoch
    p._touch("old")
    return got_old, dust.pending, steps, p.epoch - before, old.epoch == p.epoch


def escape_demo():
    """C0-13: the critic's pool_edge (b) -- early claimer then shortfall."""
    out = []
    for label, v2 in (("v0.1", False), ("v0.3", True)):
        p = Pool()
        if not v2:   # v0.1 behaviour: claims up to pending from any SOL, withdraw ignores pending
            def claim_v1(who, p=p):
                L = p._touch(who)
                pay = min(max(L.pending, 0), p.sol_vault)
                L.pending -= pay
                p.sol_vault -= pay
                return pay
            p.claim_sol = claim_v1
            p.prorata = False
        p.deposit("A", 10**12)
        p.deposit("B", 10**12)
        p.borrow(1, 10**12)
        d = WAD * 5_000 * 29 * 86400 // (BPS * 31_536_000)
        p.accrue(d)
        p.borrow(2, 5 * 10**11)
        p.settle(2, 10**12)
        a = p.claim_sol("A")
        q, io = p.positions[1]
        owed = cdiv(q * (p.index - io), WAD)
        p.repay(1, owed // 10, 0)           # 90 % rent shortfall
        LA = p._touch("A")
        try:
            if not v2:
                LA.pending, keep = LA.pending, LA.pending
                p.pending_check = False
                # v0.1 withdraw had no pending check
                amount = LA.shares * (p.T + p.B + VA) // (p.S + VS)
                p.S -= LA.shares
                p.T -= amount
                LA.shares = 0
                escaped = True
            else:
                p.withdraw("A", LA.shares)
                escaped = True
        except ValueError:
            escaped = False
        b = p.claim_sol("B")
        LB = p._touch("B")
        out.append((label, a, LA.pending, escaped, b, LB.pending, p.sol_vault, p.receivable))
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--ops", type=int, default=60_000)
    ap.add_argument("--seed", type=int, default=20260927)
    a = ap.parse_args()
    for mode, cap in (("continuous", 9_000), ("realized", 9_000), ("continuous", 10_000)):
        c, ep, mneg, mex = fuzz(a.ops, a.seed, mode, cap)
        print(f"fuzz[{mode}, util cap {cap/100:.0f}%]: {a.ops} ops, invariants held; share epochs rolled {ep}; "
              f"largest sum of negative pendings {mneg:,} lamports (entitlement-form excess {max(mex, 0):,}); "
              f"op counts {dict(sorted(c.items()))}")
    print("\nJIT rent capture (attacker deposits 9x pool just before a 30-day position closes):")
    print("| rent accounting | rent paid (lamports) | attacker SOL | honest lender SOL (claimable now) | attacker tokens back |")
    print("|---|---|---|---|---|")
    for mode, rent, att, hon, tok in jit_demo():
        print(f"| {mode} | {rent:,} | {att:,} ({100*att/rent:.1f}%) | {hon:,} ({100*hon/rent:.1f}%) | {tok:,} |")
    minted, back = inflation_demo()
    print(f"\nfirst-depositor attack: victim deposits 1e9, receives {minted:,} shares, redeems {back:,} tokens")
    print("\nC0-9 lender run before an S1 (two equal lenders, 90 % utilisation, S1 pays 50 % of token value):")
    print("| withdrawal rule | tokens A withdrew before S1 | A final value | B final value |")
    print("|---|---|---|---|")
    for prorata, took, va, vb in run_demo():
        print(f"| {'v0.3 pro-rata + lock' if prorata else 'v0.1 first come, first served'} | {took:,} | {va:,} | {vb:,} |")
    print("\nV2-2 lender A calls claim_sol N times in one transaction (A's fair realised share shown):")
    print("| claim rule | calls | A's fair realised share | A received | sol_vault left for B |")
    print("|---|---|---|---|---|")
    for label, n, fair, got, left in claim_loop_demo():
        print(f"| {label} | {n} | {fair:,} | {got:,} | {left:,} |")
    print("\nV2-3 runner advantage over fair value by utilisation (two equal lenders; the borrowed inventory is "
          "SOL-settled at 50 % of its value; churn = 30 rounds of others' open + close between A's withdrawals):")
    print("| utilisation | v0.2 lock rule, no churn | v0.2, churn | v0.3 lock rule, no churn | v0.3, churn |")
    print("|---|---|---|---|---|")
    for r in run_util_table():
        print("| " + " | ".join(r) + " |")
    print("\nC0-6 repeated full SOL settlements (critic pool_edge (a)):")
    print("| model | new deposit 1e14 mints | new lender pending after 1,000 SOL lump + rent (lamports) | next deposit after 2nd wipe would mint | > u128 | epochs |")
    print("|---|---|---|---|---|---|")
    for label, minted, npend, m2, over, ep in wipe_demo():
        print(f"| {label} | {minted:.3e} | {npend:,} | {m2:.3e} | {over} | {ep} |")
    got_old, got_dust, behind, _, jumped = wipe_tail_demo()
    print(f"\nV2-12 after a full wipe, a 1 SOL sweep: old epoch's lender +{got_old:,} lamports, "
          f"first (dust) depositor +{got_dust:,}; a lender with 0 shares {behind} epochs behind "
          f"caught up in one touch: {jumped}")
    print("\nC0-13 early claim then 90 % rent shortfall (critic pool_edge (b)):")
    print("| model | A claimed | A pending after shortfall | A could exit | B claims now | B pending left | sol_vault | receivable |")
    print("|---|---|---|---|---|---|---|---|")
    for label, a_, pa, esc, b_, pb, sv, rc in escape_demo():
        print(f"| {label} | {a_:,} | {pa:,} | {esc} | {b_:,} | {pb:,} | {sv:,} | {rc:,} |")


if __name__ == "__main__":
    main()
