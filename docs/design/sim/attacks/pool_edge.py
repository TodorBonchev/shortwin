"""C0-6 / C0-9 / C0-13: lender-pool edge cases, re-run on pool_model v0.4 (lumps vest over lump_vest_s).

(a) critic's share-ratio blow-up: run + full SOL settlement, new deposit, credit,
    second wipe.  v0.1: new deposit minted 9.0e30 shares, a 1,000 SOL credit gave
    the new lender 0 lamports, the next deposit would mint 8.1e44 > u128.
(b) critic's negative-pending escape: A claims realised+unrealised SOL, exits,
    rent shortfall.  v0.1: A pending -12.16 SOL, B could claim only 486.49 of 498.65.
(c) withdraw-then-settle run in one transaction (C0-9 test): the runner loops
    withdrawals before an S1.
"""
import os
import sys
HERE = os.path.dirname(os.path.abspath(__file__))
for cand in (os.path.join(HERE, ".."),):
    if os.path.exists(os.path.join(cand, "pool_model.py")):
        sys.path.insert(0, cand)
        break
from pool_model import *   # noqa: E402

print("--- (a) share/asset ratio after full SOL settlement (pump token: 1e15 raw supply) ---")
p = Pool()
p.deposit("old", 10**14)
p.borrow(1, 9 * 10**13)
allow = p.free_allowance("old")
p.withdraw("old", allow * (p.S + VS) // (p.T + p.B + VA))
print("before settle: T=%d B=%d S=%.3e epoch=%d" % (p.T, p.B, p.S, p.epoch))
p.settle(1, 50 * 10**9)
print("after settle : T=%d B=%d S=%.3e epoch=%d (old lender pending %d lamports)" % (
    p.T, p.B, p.S, p.epoch, p._touch("old").pending))
p.sweep_sol(0)                          # post-wipe sweep (nothing to sweep) before any deposit, v0.4.1 (R4A-7)
m = p.deposit("new", 10**14)
print("new deposit 1e14 raw -> minted %.3e shares; S=%.3e (u128 max %.3e)" % (m, p.S, U128))
before = p.acc
p.borrow(2, 5 * 10**13)
p.repay(2, 0, 1_000 * 10**9)            # 1,000 SOL lump credited to lenders (vests over lump_vest_s, v0.4)
p.advance(p.lump_vest_s + 1)
print("credit 1,000 SOL -> acc delta = %d WAD-units/share; new lender pending = %d lamports" % (
    p.acc - before, p._touch("new").pending))
p.borrow(3, 9 * 10**13)
allow = p.free_allowance("new")
p.withdraw("new", allow * (p.S + VS) // (p.T + p.B + VA))
p.settle(3, 10**9)
m2 = 10**14 * (p.S + VS) // (p.T + p.B + VA)
print("second wipe: next deposit would mint %.3e shares -> exceeds u128: %s (epochs rolled %d)" % (
    m2, m2 > U128, p.epoch))

print("\n--- (b) exited lender with a negative SOL balance ---")
p = Pool()
p.deposit("A", 10**12); p.deposit("B", 10**12)
p.borrow(1, 10**12)                     # 50% util
d = WAD * 5_000 * 29 * 86400 // (BPS * 31_536_000)
p.accrue(d)                             # 29 days of rent credited as receivable
p.borrow(2, 5 * 10**11)
p.settle(2, 10**12)                     # another position settles: realised SOL arrives
a = p.claim_sol("A")                    # v0.2: only A's pro-rata part of REALISED SOL
try:
    got = p.withdraw("A", p.lenders["A"].shares * p.T // (p.T + p.B) - 10)
except ValueError as e:
    got = 0
    print("A withdraw:", e)
q, io = p.positions[1][:2]
owed = cdiv(q * (p.index - io), WAD)
p.repay(1, owed // 10, 0)               # 90 % rent shortfall
LA, LB = p._touch("A"), p._touch("B")
print("A claimed %d, A pending now %d, A shares left %d, A tokens withdrawn %d" % (a, LA.pending, LA.shares, got))
try:
    p.withdraw("A", LA.shares)
    print("A exits with pending >= 0")
except ValueError as e:
    print("A exit blocked:", e)
b = p.claim_sol("B")
print("B pending before claim %d, B claims %d, sol_vault left %d, receivable %d" % (
    LB.pending + b, b, p.sol_vault, p.receivable))

print("\n--- (b2) a lender whose pending turns negative cannot withdraw until it covers ---")
p = Pool()
p.deposit("A", 10**12)
p.borrow(1, 9 * 10**11)
p.accrue(WAD // 1000)                   # rent credited while only A holds shares
p.deposit("C", 9 * 10**12)              # C joins late with 90 % of the shares
q, io = p.positions[1][:2]
p.repay(1, 0, 0)                        # the whole rent defaults: debit pro-rata to current shares
LC = p._touch("C")
print("C pending after shortfall: %d" % LC.pending)
try:
    p.withdraw("C", LC.shares // 2)
except ValueError as e:
    print("C withdraw blocked:", e)
p.cover("C", -LC.pending)
print("after cover: C withdraws %d tokens" % p.withdraw("C", LC.shares // 2))

print("\n--- (c) withdraw-then-settle run (two equal lenders, 90 % util, S1 pays 50 % of token value) ---")
for prorata, took, va, vb in run_demo():
    fair = (va + vb) // 2
    print("%-32s runner withdrew %d; runner %d (%+.1f%% vs fair), other %d (%+.1f%%)" % (
        "v0.4.1 pro-rata + lock" if prorata else "v0.1 first come, first served", took, va,
        100 * (va - fair) / fair, vb, 100 * (vb - fair) / fair))
