#!/usr/bin/env python3
"""Vectors for shortwin_math::pool (ECONOMICS §9, §13; ARCHITECTURE §3.3, §8.1, §8.3 I8).

Drives the normative lender-pool model `docs/design/sim/pool_model.py` v0.4.2 (default
switches: option (a), owner decision O16) through a recording subclass of its `Pool` and
writes, next to itself:

  pool_fuzz_continuous_<cap>_<k>.json
                       `pool_model.fuzz` itself: seed 20260927, continuous rent at a 90 %
                       and a 100 % utilisation cap (the two option-(a) runs of ECON §9.6;
                       the option-(b) "release" run is the owner option O16 did not choose
                       and is not built), all 60,000 operations of each run (ARCH §8.1)
                       including time, `rent_protocol_bps` governance changes, full
                       withdrawals and the liveness wind-down every 2,000 operations
                       (I-P6). Split into FUZZ_PARTS files that continue one CRC chain; op
                       counts and epochs rolled are asserted to equal ECON §9.6. The
                       model's own `check()` runs after every operation, as in the model.
  pool_lock_fuzz_<k>.json
                       `pool_model.lock_fuzz(4000, seed)` for seeds 0..5 (R4A-2 / R4B-3,
                       the reviewers' lock-heavy mix): check() after every operation and,
                       every 25 operations, the untouched-state I-P4 sample (`k`, then `A`
                       for every lender). The sample totals are asserted to equal ECON §9.3
                       (954 states, 0 above T + dust).
  pool_stranded_<k>.json
                       the v0.4 rows of `pool_model.stranded_table()` (R3A-2, reviewer's
                       r3_stranded.py; `attacks/pool_r3.py`): 40 seeds x 3,000 operations
                       and the honest wind-down, one trace per seed.
  pool_ext_<group>_<k>.json
                       extended fuzz profiles with lazy lenders (no touch-all), so lenders
                       fall many share epochs behind and locks expire untouched: repeated
                       full wipes with post-wipe sweeps (V2-12, R4A-7), forced deep rebases,
                       runs and churn (C0-9, V2-3), time passing, governance changes, the
                       untouched-state I-P4 sample, dust amounts, u64-extreme amounts with
                       the on-chain overflow refusals, lumps; plus scripted traces for ARCH
                       §9 M0b's carried-over model notes (1) lock terms at the u128 ceiling,
                       (2) the v0.4 over-lock case (fixed in v0.4.1), (4) the last shares
                       leaving while dust is borrowed; and an empty lender that still holds
                       a lock term (its shares and lock floored to 0 in a rebase, its term
                       did not), which the next roll's record must scale (M0b review
                       M0B-A-1).
  pool_ext_orphans_<k>.json
                       KI-65 (v0.4.2, no rebase orphans): lazy lenders converting in random
                       orders through forced deep rebases, 0-token burns that leave dust
                       residues below the next rebase's unit, whole exits during a vest
                       (`exit`: pool_model.exit_all) and permissionless syncs of every lender
                       in a random order; plus the default-switch (v0.4.2) pools of
                       pool_model.orphan_vest_demo (every lender exits during a vest after a
                       rebase: the last exit takes S to 0 and is paid the vest) and
                       whale_order_demo (R4B-4 W1-W3: a JIT whale around a lump while a lender
                       holds an unconverted share, both sync orders of a rebase's rounding
                       unit, a whale deposit while S is a latent share).
  pool_ext_pot_<k>.json
                       KI-66 (the pot-only touch): lazy lenders, deep rebases and a third party's
                       sync_lender of random lenders (`h`, pool_model._touch_pot; some
                       repeated, then everyone claims), so lenders behind a rebase record move
                       their surplus to the pot without converting and convert later at their
                       own touch; 100 % and 90 % caps, normal and dust amounts (`pot_holds`:
                       the pot-only touches that held at a rebase record). KI-66 full (the
                       forward chain): windowed third-party syncs (`y U N 0`, the chain walked
                       through 1-8 records at a time) and the owners' partial syncs (`y U N 1`)
                       that stop inside a chain; ext_pot_fwd_open_100 puts residues at 999
                       modulo 1,000 before 1/1,000 rebases, so owners run a unit above their
                       chain (`fwd_open`: FWD_OPEN lender states after a line, of which with
                       lag 1, of which at fwd_epoch < share_epoch).
  pool_ext_topup_<k>.json
                       the rent top-up of finding M10-8C-1: extended profiles whose exits pay
                       rent above what accrued (`rpx`: a repay of the accrued rent plus an
                       R_min-like excess; `stx`: a settlement whose RENT = R_due exceeds the
                       accrued rent), so the top-up `max(0, min(principal, rent) − credited)`
                       is R_min-sized and not rounding-sized (`top_ups` counts the exits
                       with a positive top-up); and two scripted JIT bundles (a whale's
                       deposit, the exit, claim, withdrawal and claim in one second, around
                       an early repay and an early settlement) whose claims the model
                       asserts: the whale realises at most rounding, beyond its share of a
                       settlement's token value.
  pool_scenarios_<k>.json
                       the default-switch branches of pool_model's demos (ECON §9.3–§9.6),
                       the round-3 cases of attacks/pool_r3.py (jit_hold_table,
                       stuck_lock_table, wipe_tail_demo, zero_share_demo,
                       protocol_change_demo; stranded_table above), attacks/pool_edge.py
                       and the round-4 cases (stale_lock_demo, lock_tail_demo,
                       wipe_gate_demo, vest_exit_demo, restart_grief_table,
                       premium_jit_table), KI-66's pot_touch_demo and KI-66 full's
                       pot_chain_demo (its held-only comparison pool is not recorded), and
                       AIR-07's rebase_vest_demo (the settlement that leaves dust token
                       claims; since the residual fix the deposits before it are refused,
                       PoolDrained), AIR-08's empty_pool_gate_demo (a deposit after the last
                       exit waits for the sweep) and AIR-12's lock_era_demo (Π restarts with
                       every lock era); their pre-fix comparison pools are not recorded.

Line format (parsed by tests/pool_vectors.rs). Each trace is a list of lines; a line
holds one or more pool operations separated by ";" and ends with " #cccccccc", the
running CRC-32 (zlib) of every line's digest record so far. `now` is harness state that
starts at 0. Operations:

  d U AMOUNT        release + touch (one committed step), then pool::deposit
                    (rewards_required = false)                   -> shares minted
  w U SHARES        release + touch (one committed step), then pool::withdraw -> tokens
  a U               pool::touch, then pool::allowance            -> tokens
  A U               pool::allowance_view (pure)                  -> tokens
  c U               pool::claim                                  -> lamports paid
  v U LAMPORTS      pool::cover
  t U               pool::touch
  h U               pool::touch_pot (KI-66: a third party's sync_lender; behind a rebase record
                    the lender is accrued to its ends and, KI-66 full, along its forward chain
                    to the current epoch, its surplus moves to the pot, unconverted)
  y U N S           sync_lender(N) with the N records from where it starts (KI-66 full): S = 1,
                    the owner's (pool::convert from the lender's epoch, then pool::touch unless
                    still behind); S = 0, a third party's (pool::touch_pot from the lender's
                    epoch or, with a forward chain, from its fwd_epoch)
  l U               pool::lock_of (pure)                         -> shares
  k                 pool::cal_advance
  R                 pool::release (the committed vest release of an open refused by the
                    utilisation cap: the model releases before it refuses)
  b Q RELEASE_AT    pool::borrow (RELEASE_AT = deadline + the position's S3 tail)
  x DT LAMPORTS     now += DT, then pool::accrue(now, LAMPORTS) (LAMPORTS = the lender
                    rent credit ⌊(A·dI + rem)/(WAD·10^4)⌋ with the model's carried remainder
                    rem, AIR-03); refused as a whole, time included
  r Q RENT P CREDITED LUMP
                    pool::repay (RENT = rent paid, P = the position's rent_protocol_bps,
                    CREDITED = rent::credited_for, LUMP = lender lump that vests; the
                    lender rent above CREDITED vests with it, M10-8C-1)
  s Q SOL CREDITED LUMP RENT
                    pool::settle (LUMP = the part of SOL that vests; RENT = R_due, paid
                    first by the immediate rest, whose part above CREDITED vests too,
                    M10-8C-1)
  p LAMPORTS        pool::sweep (SweepSource::Trader)

U is an index into the trace's `users`; a lender is created (pool::Lender::new) the
first time a line names it. An operation may end in "=VALUE" (its expected result;
scenario traces) or "!CODE" (an expected error: overflow, underflow, invalid, divzero,
undefined, or a pool-rule refusal of MODEL_ERRORS, each its own MathError variant). A
failed operation changes nothing, except the committed first step of `d` and `w`
(exactly as the model releases and touches before it refuses). When `check_each_line`
is true the model's `check()` follows every line (Market::check_touch_all in
pool_harness.rs: cal_advance, the untouched-state allowances, touch everyone, the
invariants, touch + allowance for everyone).

Digest record of a line: "<results>|<state>\\n", where <results> joins each
operation's "=VALUE", "!CODE" or "" with "," and <state> is the canonical state:

  now,T,B,S,acc,carry,acc_r,carry_r,dust,receivable,max_release,vest_bal,vest_last,
  vest_end,epoch,wipe_live,wipe_swept,pi,pi_epoch,lock_norm,b_min,sol_vault,unmatched,
  protocol,inflow
  [;C<day>:<term>,...]                                   live calendar buckets, by day
  [;E<epoch>,acc_end,num,den_log1000,acc_r_end,s_old,carry,carry_r,conv_rem]   latest record
  [;<U>:shares,snap,pending,epoch,lock,lock_pi,lock_epoch,lock_term,lock_until,snap_r,
   real,claimed,fwd,fwd_epoch,fwd_shares,fwd_lag]         per lender, creation order

`lock_epoch` is the model's -1 ("no lock") written as 4294967295 (u32::MAX); `wipe_swept`
is the model's flag (the trader bit of Pool::wipe_swept). Rust types are the on-chain ones
of ARCH §2.3.4/§2.3.6; an operation whose model result does not fit them is refused
(expected "!overflow" / "!underflow") and the model state is rolled back, as a reverted
transaction would. The generator also asserts that every calendar bucket lies within the
program's 128-day ring (ECON §9.3).

`liveness` entries (fuzz traces): after line LINE, the model's wind_down_liveness (on a
copy): OPS in the vocabulary above (explicit results), the final canonical STATE, PAID
(sum of the claims), STRANDED (lender_sol left), BOUND (the model's dust bound) and
UNPAID (sum of positive pendings left).

`model_check_fails`: the lines after which pool_model's own check() (on a copy) fails,
for traces without check_each_line. The Rust invariant checker must fail on exactly those
lines and pass on every other line of every trace.
"""
import contextlib
import copy
import io
import json
import random
import runpy
import sys
import zlib
from pathlib import Path

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[5]
SIM = ROOT / "docs" / "design" / "sim"
sys.path.insert(0, str(SIM))
import pool_model as pm  # noqa: E402  (v0.4.2)

SEED = 20260927
PM_OPS = 60_000          # the whole ECON §9.6 fuzz run (ARCH §8.1: 60,000 ops per rent mode)
FUZZ_PARTS = 3           # files per run, each under 1 MB (tests/vectors/README.md)
MAX_FILE_BYTES = 990_000
# ECON §9.6: share epochs rolled and op counts of the two option-(a) runs. Since AIR-07's residual
# fix a deposit into a drained pool is refused (PoolDrained): 26 of the 90 % run's 6,653 deposits
# and 234 of the 100 % run's 6,607, which moves every later count (before: 1020 and 1930 epochs).
ECON_9_6 = {
    9_000: (1021, {'accrue': 3395, 'adv': 3287, 'borrow': 2835, 'borrow!': 518, 'churn': 3168,
                   'churn!': 252, 'claim': 3378, 'claimN': 3416, 'cover': 3384, 'dep': 6627,
                   'dep!': 26, 'donate': 3329, 'gov': 3302, 'repay': 3246, 'settle': 3342,
                   'sweep': 3304, 'wd': 6315, 'wd!': 239, 'wdall': 2896, 'wdall!': 367,
                   'wipe': 3374}),
    10_000: (1941, {'accrue': 3344, 'adv': 3344, 'borrow': 3358, 'churn': 3467, 'claim': 3377,
                    'claimN': 3462, 'cover': 3396, 'dep': 5862, 'dep!': 745, 'donate': 3305,
                    'gov': 3303, 'repay': 3302, 'settle': 3325, 'sweep': 3264, 'wd': 5574,
                    'wd!': 966, 'wdall': 2848, 'wdall!': 458, 'wipe': 3300}),
}
# ECON §9.3: the lock-heavy mix, v0.4.1 row (sampled states, states above T + dust).
ECON_LOCK_FUZZ = (954, 0)
CHECKPOINT_EVERY = 1_000
U32, U64, U128 = 2**32 - 1, 2**64 - 1, 2**128 - 1
RANGES = {
    "u8": (0, 255), "u32": (0, U32), "u64": (0, U64), "u128": (0, U128),
    "i64": (-(2**63), 2**63 - 1), "i128": (-(2**127), 2**127 - 1),
    "lock_epoch": (-1, U32 - 1),
}
POOL_T = [("now", "i64"), ("T", "u64"), ("B", "u64"), ("S", "u128"), ("acc", "i128"),
          ("carry", "i128"), ("acc_r", "u128"), ("carry_r", "u128"), ("dust", "i128"),
          ("receivable", "u64"), ("max_release", "i64"), ("vest_bal", "u64"),
          ("vest_last", "i64"), ("vest_end", "i64"), ("epoch", "u32"), ("pi", "u128"),
          ("pi_epoch", "u32"), ("lock_norm", "u128"), ("b_min", "u64"), ("sol_vault", "u64"),
          ("unmatched", "u64"), ("protocol", "u64")]
LENDER_T = [("shares", "u128"), ("snap", "i128"), ("pending", "i64"), ("epoch", "u32"),
            ("lock", "u128"), ("lock_pi", "u128"), ("lock_epoch", "lock_epoch"),
            ("lock_term", "u128"), ("lock_until", "i64"), ("snap_r", "u128"), ("real", "u64"),
            ("claimed", "u64"), ("fwd", "u8"), ("fwd_epoch", "u32"), ("fwd_shares", "u128"),
            ("fwd_lag", "u8")]
RECORD_T = [(0, "i128"), (3, "u128"), (4, "u128"), (5, "i128"), (6, "u128"), (7, "u128")]
MODEL_ERRORS = {
    "deposit too small": "deposit_too_small",
    "share overflow": "overflow",
    "WipeSweepPending: sweep_trader / claim_venue_rewards first": "wipe_sweep_pending",
    "PoolDrained: no deposit while T <= (T + B) / 1,000 with tokens lent out": "pool_drained",
    "negative SOL balance: cover it first": "negative_pending",
    "not enough shares": "insufficient_shares",
    "insufficient unborrowed inventory": "insufficient_inventory",
    "above pro-rata share of unborrowed inventory": "above_allowance",
}
# Constructor switches whose non-default values are analysis-only (pool_model header):
# pools built with any of them are not recorded.
DEFAULT_SWITCHES = {"mode": "continuous", "prorata": True, "pi_decay": True, "lock_expiry": True,
                    "lock_calendar": True, "vest_weighted": True, "wipe_sweep_gate": True,
                    "exit_flush_bps": 0, "lock_floor": True, "cum_convert": True, "fwd_chain": True,
                    "vest_floor": True, "dust_flush": True, "empty_sweep_gate": True,
                    "pi_era_reset": True, "drain_gate": True}

BasePool = pm.Pool
REGISTRY: list = []
CONFIG = {"line_mode": "op", "explicit": True, "taint": False, "verify": False}


class Refused(ValueError):
    def __init__(self, code: str):
        super().__init__(code)
        self.code = code


def den_log(den: int) -> int:
    k = 0
    while den > 1:
        assert den % pm.REBASE == 0, den
        den //= pm.REBASE
        k += 1
    return k


def out_of_range(v: int, t: str):
    lo, hi = RANGES[t]
    if v > hi:
        return "overflow"
    if v < lo:
        return "underflow" if t.startswith("u") else "overflow"
    return None


def state_str(p) -> str:
    f = [p.now, p.T, p.B, p.S, p.acc, p.carry, p.acc_r, p.carry_r, p.dust, p.receivable,
         p.max_release, p.vest_bal, p.vest_last, p.vest_end, p.epoch, int(p.wipe_live),
         int(p.wipe_swept), p.pi, p.pi_epoch, p.lock_norm, p.b_min, p.sol_vault, p.unmatched,
         p.protocol, p.inflow]
    out = [",".join(map(str, f))]
    if p.lock_cal:
        out.append("C" + ",".join(f"{d}:{v}" for d, v in sorted(p.lock_cal.items())))
    if p.epoch:
        r = p.epochs[p.epoch - 1]
        out.append("E" + ",".join(map(str, [p.epoch - 1, r[0], r[1], den_log(r[2]), r[3], r[4], r[5], r[6], r[7]])))
    for who, L in p.lenders.items():
        vals = [L.shares, L.snap, L.pending, L.epoch, L.lock, L.lock_pi, L.lock_epoch % 2**32,
                L.lock_term, L.lock_until, L.snap_r, L.real, L.claimed, L.fwd, L.fwd_epoch, L.fwd_shares,
                L.fwd_lag]
        out.append(f"{p.rec_ids[who]}:" + ",".join(map(str, vals)))
    return ";".join(out)


def copy_lender(L):
    c = pm.Lender.__new__(pm.Lender)
    for s in pm.Lender.__slots__:
        setattr(c, s, getattr(L, s))
    return c


def model_copy(p, deep: bool = False):
    """A plain BasePool copy of the model state (no recorder). check() reads the positions
    only, so the light copy shares them; its touches write the conversion remainder of the
    records a lender converts through (KI-65), so it copies those."""
    q = object.__new__(BasePool)
    items = {k: v for k, v in p.__dict__.items() if not k.startswith("rec_")}
    if deep:
        q.__dict__.update(copy.deepcopy(items))
    else:
        q.__dict__.update(items)
        q.lenders = {w: copy_lender(L) for w, L in p.lenders.items()}
        q.lock_cal = dict(p.lock_cal)
        q.epochs = dict(p.epochs)
        for e in range(min((L.epoch for L in p.lenders.values()), default=p.epoch), p.epoch):
            q.epochs[e] = list(p.epochs[e])
    return q


def verify(p):
    """The model's check() (I-P1..I-P7, I-P4 also untouched) on a copy, so the lenders stay lazy."""
    BasePool.check(model_copy(p))


class RecPool(BasePool):
    """pool_model.Pool that logs its outermost calls in the pool-API vocabulary."""

    def __init__(self, *a, **kw):
        super().__init__(*a, **kw)
        self.rec_init()
        REGISTRY.append(self)

    def rec_init(self, **over):
        self.rec_switches = {k: getattr(self, k) for k in DEFAULT_SWITCHES}
        self.rec_depth = 0
        self.rec_ids = {}
        self.rec_ops = []
        self.rec_res = []
        self.rec_lines = []
        self.rec_checkpoints = []
        self.rec_liveness = []
        self.rec_crc = 0
        self.rec_line_mode = CONFIG["line_mode"]
        self.rec_explicit = CONFIG["explicit"]
        self.rec_taint = CONFIG["taint"]
        self.rec_verify = CONFIG["verify"]
        self.rec_check_fails = []
        self.rec_tainted = False
        self.rec_last_state = None
        for k, v in over.items():
            setattr(self, k, v)

    # ---- bookkeeping ---------------------------------------------------------------
    def rec_id(self, who) -> int:
        if who not in self.rec_ids:
            self.rec_ids[who] = len(self.rec_ids)
        return self.rec_ids[who]

    def rec_snapshot(self):
        scal = {k: v for k, v in self.__dict__.items()
                if k not in ("lenders", "epochs", "positions", "lock_cal") and not k.startswith("rec_")}
        lend = {w: [getattr(L, s) for s in pm.Lender.__slots__] for w, L in self.lenders.items()}
        n = len(self.epochs)
        last = list(self.epochs[n - 1]) if n else None
        # KI-65: a touch writes conv_rem into every record its lender converts through
        lo = min((L.epoch for L in self.lenders.values()), default=n)
        rems = {e: self.epochs[e][7] for e in range(lo, n)}
        return scal, lend, n, last, dict(self.positions), dict(self.lock_cal), rems

    def rec_restore(self, snap):
        scal, lend, n, last, pos, cal, rems = snap
        self.__dict__.update(scal)
        for w in [w for w in self.lenders if w not in lend]:
            del self.lenders[w]
        for w, vals in lend.items():
            L = self.lenders[w]
            for s, v in zip(pm.Lender.__slots__, vals):
                setattr(L, s, v)
        for e in range(n, len(self.epochs)):
            del self.epochs[e]
        if n:
            self.epochs[n - 1][:] = last
        for e, r in rems.items():
            self.epochs[e][7] = r
        self.positions.clear()
        self.positions.update(pos)
        self.lock_cal = dict(cal)

    def rec_violation(self):
        for name, t in POOL_T:
            v = out_of_range(getattr(self, name), t)
            if v:
                return v
        for L in self.lenders.values():
            for name, t in LENDER_T:
                v = out_of_range(getattr(L, name), t)
                if v:
                    return v
        if self.epochs:
            rec = self.epochs[self.epoch - 1]
            for i, t in RECORD_T:
                v = out_of_range(rec[i], t)
                if v:
                    return v
        for term in self.lock_cal.values():
            v = out_of_range(term, "u128")
            if v:
                return v
        return None

    def rec_ring_ok(self):
        """Every bucket lies within the 128-day ring the program keeps (ECON §9.3)."""
        if self.lock_cal:
            today = self.now // pm.LOCK_BUCKET_S
            assert max(self.lock_cal) <= today + pm.LOCK_BUCKETS, (max(self.lock_cal), today)

    def rec_log(self, token: str, result: str):
        explicit = self.rec_explicit or result.startswith("!")
        self.rec_ops.append(f"{token} {result}" if explicit and result else token)
        self.rec_res.append(result)
        if self.rec_line_mode == "op":
            self.rec_end_line()

    def rec_end_line(self):
        self.rec_ring_ok()
        st = state_str(self)
        self.rec_crc = zlib.crc32(f"{','.join(self.rec_res)}|{st}\n".encode(), self.rec_crc)
        ops = ";".join(self.rec_ops)
        self.rec_lines.append(f"{ops} #{self.rec_crc:08x}" if ops else f"#{self.rec_crc:08x}")
        if len(self.rec_lines) % CHECKPOINT_EVERY == 0:
            self.rec_checkpoints.append({"line": str(len(self.rec_lines) - 1), "state": st})
        self.rec_ops, self.rec_res = [], []
        self.rec_last_state = st
        if self.rec_verify:
            try:
                verify(self)
            except AssertionError:
                self.rec_check_fails.append(str(len(self.rec_lines) - 1))

    def rec_call(self, token, fn, who=None, pre=False, result=True):
        """Runs one outermost operation with the program's atomicity."""
        if self.rec_taint and self.rec_last_state is not None and state_str(self) != self.rec_last_state:
            self.rec_tainted = True                     # state changed outside the pool API
        if who is not None and who not in self.lenders:
            self.lenders[who] = pm.Lender(self.acc, self.acc_r, self.epoch)
        self.rec_depth += 1
        try:
            if pre:                                     # the committed release + touch of d / w
                snap = self.rec_snapshot()
                BasePool._release(self)
                BasePool._touch(self, who)
                v = self.rec_violation()
                if v:
                    self.rec_restore(snap)
                    raise Refused(v)
            snap = self.rec_snapshot()
            try:
                out = fn()
            except Refused:
                raise
            except ValueError as e:
                self.rec_restore(snap)
                raise Refused(MODEL_ERRORS[str(e)]) from None
            tok = token() if callable(token) else token
            v = self.rec_violation()
            if v:
                self.rec_restore(snap)
                raise Refused(v)
        except Refused as e:
            self.rec_depth -= 1
            tok = token() if callable(token) else token
            self.rec_log(tok, "!" + e.code)
            raise
        self.rec_depth -= 1
        self.rec_log(tok, f"={out}" if result and isinstance(out, int) and not isinstance(out, bool) else "")
        return out

    def who_of(self, L):
        return next(w for w, x in self.lenders.items() if x is L)

    # ---- the pool API ----------------------------------------------------------------
    def deposit(self, who, amount):
        if self.rec_depth:
            return BasePool.deposit(self, who, amount)
        assert 0 <= amount <= U64
        return self.rec_call(f"d {self.rec_id(who)} {amount}", lambda: BasePool.deposit(self, who, amount),
                             who, pre=True)

    def withdraw(self, who, shares):
        if self.rec_depth:
            return BasePool.withdraw(self, who, shares)
        assert 0 <= shares <= U128
        return self.rec_call(f"w {self.rec_id(who)} {shares}", lambda: BasePool.withdraw(self, who, shares),
                             who, pre=True)

    def free_allowance(self, who):
        if self.rec_depth:
            return BasePool.free_allowance(self, who)
        return self.rec_call(f"a {self.rec_id(who)}", lambda: BasePool.free_allowance(self, who), who)

    def allowance_view(self, who):
        if self.rec_depth:
            return BasePool.allowance_view(self, who)
        assert who in self.lenders, "allowance_view of an unknown lender creates nothing"
        return self.rec_call(f"A {self.rec_id(who)}", lambda: BasePool.allowance_view(self, who))

    def claim_sol(self, who):
        if self.rec_depth:
            return BasePool.claim_sol(self, who)
        return self.rec_call(f"c {self.rec_id(who)}", lambda: BasePool.claim_sol(self, who), who)

    def cover(self, who, lamports):
        if self.rec_depth:
            return BasePool.cover(self, who, lamports)
        assert 0 <= lamports <= U64
        return self.rec_call(f"v {self.rec_id(who)} {lamports}", lambda: BasePool.cover(self, who, lamports), who)

    def _touch(self, who, pot_only: bool = False):
        if self.rec_depth:
            return BasePool._touch(self, who, pot_only)
        assert not pot_only, "an outermost pot-only touch is _touch_pot (op h)"
        return self.rec_call(f"t {self.rec_id(who)}", lambda: BasePool._touch(self, who), who, result=False)

    def _touch_pot(self, who):
        if self.rec_depth:
            return BasePool._touch_pot(self, who)
        return self.rec_call(f"h {self.rec_id(who)}", lambda: BasePool._touch_pot(self, who), who, result=False)

    def sync_lender(self, who, n, signed):
        if self.rec_depth:
            return BasePool.sync_lender(self, who, n, signed)
        return self.rec_call(f"y {self.rec_id(who)} {n} {int(signed)}",
                             lambda: BasePool.sync_lender(self, who, n, signed), who, result=False)

    def _lock_of(self, L):
        if self.rec_depth:
            return BasePool._lock_of(self, L)
        return self.rec_call(f"l {self.rec_id(self.who_of(L))}", lambda: BasePool._lock_of(self, L))

    def _cal_advance(self):
        if self.rec_depth:
            return BasePool._cal_advance(self)
        return self.rec_call("k", lambda: BasePool._cal_advance(self), result=False)

    def locked_total(self):
        assert self.rec_depth, "an outermost locked_total() advances the calendar; not in the vocabulary"
        return BasePool.locked_total(self)

    def advance(self, dt: int, d_index: int = 0):
        if self.rec_depth:
            return BasePool.advance(self, dt, d_index)
        assert dt >= 0
        # The model's credit with its carried remainder (AIR-03, Pool.credit_rem).
        c = (self.A * d_index + self.credit_rem) // (pm.WAD * pm.BPS) \
            if d_index and self.mode == "continuous" else 0
        return self.rec_call(f"x {dt} {c}", lambda: BasePool.advance(self, dt, d_index))

    def borrow(self, pid, q, duration: int = 30 * 86400, tail: int = pm.S3_TAIL_S):
        if self.rec_depth:
            return BasePool.borrow(self, pid, q, duration, tail)
        release_at = self.now + duration + tail
        snap = self.rec_snapshot()
        self.rec_depth += 1
        try:
            BasePool.borrow(self, pid, q, duration, tail)
            ok = True
        except ValueError as e:
            assert str(e) == "utilization cap", e
            ok = False
        finally:
            self.rec_depth -= 1
        self.rec_restore(snap)
        if ok:
            return self.rec_call(f"b {q} {release_at}", lambda: BasePool.borrow(self, pid, q, duration, tail))
        # An open refused by the cap never reaches the pool, but the model released the vest
        # before refusing: the committed release.
        self.rec_call("R", lambda: BasePool._release(self))
        raise ValueError("utilization cap")

    def repay(self, pid, rent_paid, lender_lump):
        if self.rec_depth:
            return BasePool.repay(self, pid, rent_paid, lender_lump)
        q, io, p, _dl = self.positions[pid]
        credited = self._credited_for(q, io, p)
        assert 0 <= rent_paid <= U64 and 0 <= lender_lump <= U64
        return self.rec_call(f"r {q} {rent_paid} {p} {credited} {lender_lump}",
                             lambda: BasePool.repay(self, pid, rent_paid, lender_lump))

    def settle(self, pid, sol_in, lump=0, rent=None):
        if self.rec_depth:
            return BasePool.settle(self, pid, sol_in, lump, rent)
        q, io, p, _dl = self.positions[pid]
        credited = self._credited_for(q, io, p)
        if rent is None:
            # pool_model.settle's default: the position's accrued rent (R_due past min_rent_s).
            # Clamped to u64 (R_due is a u64 on-chain), passed explicitly so both sides agree.
            rent = min(pm.cdiv(q * (self.index - io), pm.WAD), U64)
        assert 0 <= sol_in <= U64 and 0 <= lump <= U64 and 0 <= rent <= U64
        return self.rec_call(f"s {q} {sol_in} {credited} {lump} {rent}",
                             lambda: BasePool.settle(self, pid, sol_in, lump, rent))

    def sweep_sol(self, lamports):
        if self.rec_depth:
            return BasePool.sweep_sol(self, lamports)
        assert 0 <= lamports <= U64
        return self.rec_call(f"p {lamports}", lambda: BasePool.sweep_sol(self, lamports))

    def check(self):
        if self.rec_depth:
            return BasePool.check(self)
        self.rec_depth += 1
        try:
            BasePool.check(self)
        finally:
            self.rec_depth -= 1
        assert self.rec_violation() is None, "check() left the on-chain ranges"
        if self.rec_line_mode == "check":
            self.rec_end_line()

    def wind_down_liveness(self):
        """I-P6 on a copy, recorded as a liveness block after the current line."""
        if self.rec_depth:
            return BasePool.wind_down_liveness(self)
        assert not self.rec_ops, "liveness runs between lines"
        expect = BasePool.wind_down_liveness(model_copy(self, deep=True))
        # The same steps as the model, on a recording copy (one op per line, explicit results).
        r = object.__new__(RecPool)
        r.__dict__.update(copy.deepcopy({k: v for k, v in self.__dict__.items() if not k.startswith("rec_")}))
        r.rec_init(rec_line_mode="op", rec_explicit=True, rec_taint=False, rec_verify=False,
                   rec_ids=dict(self.rec_ids))
        for k in list(r.positions):
            q, io, pr, dl = r.positions[k]
            r.repay(k, pm.cdiv(q * (r.index - io), pm.WAD), 0)
        r.advance(r.lump_vest_s + 1)
        for w in list(r.lenders):
            r._touch(w)
        before = r.sol_vault
        paid = 0
        for _ in range(2):
            for w in list(r.lenders):
                paid += r.claim_sol(w)
        stranded = r.sol_vault
        bound = r.n_touch + 2 * len(r.lenders) + r.epoch + 2
        assert (paid, before, stranded, bound) == expect, ((paid, before, stranded, bound), expect)
        unpaid = sum(max(L.pending, 0) for L in r.lenders.values())
        assert stranded <= bound or unpaid == 0
        ops = ";".join(line.rsplit(" #", 1)[0] for line in r.rec_lines)
        self.rec_liveness.append({"line": str(len(self.rec_lines) - 1), "ops": ops, "state": state_str(r),
                                  "paid": str(paid), "stranded": str(stranded), "bound": str(bound),
                                  "unpaid": str(unpaid)})
        return expect


@contextlib.contextmanager
def recording(**cfg):
    saved = dict(CONFIG)
    CONFIG.update(cfg)
    REGISTRY.clear()
    pm.Pool = RecPool
    try:
        yield REGISTRY
    finally:
        pm.Pool = BasePool
        CONFIG.clear()
        CONFIG.update(saved)


def trace_json(p, name: str, note: str, **extra) -> dict:
    users = sorted(p.rec_ids, key=p.rec_ids.get)
    t = {
        "name": name,
        "note": note,
        "util_cap_bps": str(p.util_cap),
        "lump_vest_s": str(p.lump_vest_s),
        "lock_grace_s": str(p.lock_grace_s),
        "check_each_line": p.rec_line_mode == "check",
        "users": [str(u) for u in users],
        "lines": p.rec_lines,
        "checkpoints": p.rec_checkpoints,
        "final_state": state_str(p),
        "epochs_rolled": str(p.epoch),
    }
    if p.rec_verify:
        t["model_check_fails"] = p.rec_check_fails
    if p.rec_liveness:
        t["liveness"] = p.rec_liveness
    t.update(extra)
    return t


def split(full: dict, parts: int) -> list:
    """Cuts a trace into `parts` consecutive traces of one CRC chain (checkpoint, liveness
    and check-failure line numbers stay absolute)."""
    n = len(full["lines"])
    size = n // parts
    assert size * parts == n and size % CHECKPOINT_EVERY == 0
    out = []
    for k in range(parts):
        lo, hi = k * size, (k + 1) * size
        end = next(c for c in full["checkpoints"] if int(c["line"]) == hi - 1)["state"]
        assert hi < n or end == full["final_state"]
        part = dict(full, lines=full["lines"][lo:hi],
                    checkpoints=[c for c in full["checkpoints"] if lo <= int(c["line"]) < hi],
                    final_state=end, epochs_rolled=end.split(";")[0].split(",")[14],
                    part=str(k + 1), parts=str(parts), first_line=str(lo))
        if "liveness" in full:
            part["liveness"] = [b for b in full["liveness"] if lo <= int(b["line"]) < hi]
        out.append(part)
    return out


# ---- 1. pool_model's own fuzz ----------------------------------------------------------
def pm_fuzz_traces() -> list:
    parts = []
    for cap in (9_000, 10_000):
        with recording(line_mode="check", explicit=False) as reg:
            counts, epochs, _, _ = pm.fuzz(PM_OPS, SEED, "continuous", cap)
            (p,) = reg
        assert len(p.rec_lines) == PM_OPS
        assert (p.epoch, counts) == ECON_9_6[cap], (cap, p.epoch, counts)
        assert len(p.rec_liveness) == PM_OPS // 2_000 + 1
        full = trace_json(
            p, f"pm_fuzz_continuous_{cap // 100}",
            f"pool_model.fuzz({PM_OPS}, {SEED}, 'continuous', {cap}): the ECON §9.6 run, check() after "
            f"every operation, the liveness wind-down every 2,000 operations and at the end",
            seed=str(SEED), op_counts={k: str(v) for k, v in sorted(counts.items())})
        parts.extend(split(full, FUZZ_PARTS))
    return parts


# ---- 2. the reviewers' lock-heavy mix (R4A-2 / R4B-3) ------------------------------------
def lock_fuzz_traces() -> list:
    traces = []
    checks = bad = 0
    for seed in range(6):
        with recording(line_mode="check", explicit=False) as reg:
            c, b, w = pm.lock_fuzz(4000, seed)
            (p,) = reg
        checks += c
        bad += b
        traces.append(trace_json(p, f"lock_fuzz_{seed}", f"pool_model.lock_fuzz(4000, {seed}): check() after "
                                 f"every operation; every 25 operations the untouched-state I-P4 sample "
                                 f"(k, then A for every lender)", seed=str(seed), samples=str(c), above=str(b)))
    assert (checks, bad) == ECON_LOCK_FUZZ, (checks, bad)
    return traces


# ---- 3. R3A-2 stranded SOL (stranded_table, v0.4 rows) -----------------------------------
def stranded_traces():
    with recording(line_mode="op", explicit=False, verify=True) as reg:
        table = pm.stranded_table()
        pools = list(reg)
    assert len(pools) == 40
    row = table["v0.4 (unmatched pot)"]
    traces = [trace_json(p, f"stranded_{seed}", f"pool_model.stranded_table() v0.4 row, seed {seed} (R3A-2, "
                         f"reviewer's r3_stranded.py): 3,000 operations, then the honest wind-down",
                         seed=str(seed)) for seed, p in enumerate(pools)]
    assert sum(int(t["final_state"].split(";")[0].split(",")[21]) for t in traces) == row[1]
    return traces, row


# ---- 4. extended fuzz --------------------------------------------------------------------
class Ext:
    """One extended-fuzz profile; the op mix and amount draws vary per profile."""

    def __init__(self, name, seed, n_ops, cap, users, ops, amounts, note, **pool_kw):
        self.name, self.seed, self.n_ops, self.cap = name, seed, n_ops, cap
        self.users = [f"u{i}" for i in range(users)]
        self.ops, self.amounts, self.note, self.pool_kw = ops, amounts, note, pool_kw

    def run(self) -> dict:
        rng = random.Random(self.seed)
        with recording(line_mode="manual", explicit=False, verify=True) as reg:
            p = pm.Pool("continuous", util_cap_bps=self.cap, **self.pool_kw)
        self.pid = 0
        self.p, self.rng = p, rng
        self.top_ups = 0
        self.holds = 0
        opens = [0, 0, 0]          # KI-66 full: FWD_OPEN states after a line; with lag 1; at fwd_epoch < epoch
        counts = {}
        for _ in range(self.n_ops):
            op = rng.choice(self.ops)
            try:
                getattr(self, "op_" + op)()
                counts[op] = counts.get(op, 0) + 1
            except ValueError:
                counts[op + "!"] = counts.get(op + "!", 0) + 1
            p.rec_end_line()
            for L in p.lenders.values():
                if L.fwd == pm.FWD_OPEN:
                    opens[0] += 1
                    opens[1] += L.fwd_lag == 1
                    opens[2] += L.epoch == L.fwd_epoch < p.epoch
        assert not p.rec_tainted
        extra = {}
        if {"rpx", "stx"} & set(self.ops):
            assert self.top_ups >= self.n_ops // 40, (self.name, self.top_ups)
            extra["top_ups"] = str(self.top_ups)
        if "pot" in self.ops or "syw" in self.ops:
            assert self.holds >= self.n_ops // (400 if "fwd" in self.name else 200), (self.name, self.holds)
            extra["pot_holds"] = str(self.holds)
        if "fwd" in self.name:
            extra["fwd_open"] = ",".join(map(str, opens))
        if self.name == "ext_pot_fwd_open_100":
            assert opens[0] >= 200 and opens[1] >= 50 and opens[2] >= 20, opens
        return trace_json(p, self.name, self.note, seed=str(self.seed),
                          op_counts={k: str(v) for k, v in sorted(counts.items())}, **extra)

    def amt(self, kind, *a):
        return self.amounts[kind](self.rng, self.p, *a)

    def who(self):
        return self.rng.choice(self.users)

    def op_dep(self):
        p = self.p
        if p.S == 0 and not p.wipe_swept and self.rng.random() < 0.6:
            # the depositor bundles the sweep a pool without shares waits for: after a full wipe
            # (R4A-7) and, since AIR-08, after the last lender's exit (else wipe_sweep_pending)
            p.sweep_sol(self.amt("sweep"))
        p.deposit(self.who(), self.amt("dep"))

    def op_wd(self):
        p, u = self.p, self.who()
        L = p.lenders.get(u)
        if not (L and L.shares):
            return
        allow = p.free_allowance(u)
        pend = L.pending
        if L.shares == 0:
            return
        r = self.rng.random()
        at = max(1, allow * (p.S + pm.VS) // (p.T + p.B + pm.VA))
        s = (self.rng.randrange(1, L.shares + 1) if r < 0.4 else at if r < 0.8
             else L.shares if r < 0.9 else 1)
        s = min(s, L.shares)
        if burn_takes_the_last_share(p, u, s):
            s -= 1
            if s == 0:
                return
        got = p.withdraw(u, s)
        assert pend >= 0 and got <= allow, ("ghost C0-9/C0-13", pend, got, allow)

    def op_run(self):
        """C0-9: one lender withdraws its whole allowance in a loop."""
        p, u = self.p, self.who()
        if u not in p.lenders:
            return
        for _ in range(self.rng.randrange(1, 25)):
            a = p.free_allowance(u)
            L = p.lenders[u]
            if a == 0 or L.shares == 0:
                return
            pend = L.pending
            s = min(max(1, a * (p.S + pm.VS) // (p.T + p.B + pm.VA)), L.shares)
            if burn_takes_the_last_share(p, u, s):
                s -= 1
                if s == 0:
                    return
            got = p.withdraw(u, s)
            assert pend >= 0 and got <= a

    def op_bor(self):
        p = self.p
        if p.T > 0:
            self.pid += 1
            p.borrow(self.pid, self.amt("bor"), self.rng.choice([3600, 86400, 7 * 86400, 30 * 86400]))

    def op_acc(self):
        self.p.advance(self.rng.randrange(0, 600), self.amt("dindex"))

    def op_adv(self):
        self.p.advance(self.rng.choice([60, 3600, 6 * 3600, 86400, 2 * 86400, 9 * 86400, 31 * 86400]))

    def op_gov(self):
        self.p.rent_protocol_bps = self.rng.choice([0, 1000, 1500, 3000, 5000])

    def op_rep(self):
        p = self.p
        if not p.positions:
            return
        k = self.rng.choice(list(p.positions))
        q, io, pr, dl = p.positions[k]
        owed = pm.cdiv(q * (p.index - io), pm.WAD)
        paid = owed if self.rng.random() < 0.8 else self.rng.randrange(0, owed + 1)
        p.repay(k, paid, self.amt("lump"))

    def op_set(self):
        p = self.p
        if p.positions:
            sol = self.amt("sol")
            p.settle(self.rng.choice(list(p.positions)), sol,
                     self.rng.randrange(0, sol + 1) if self.rng.random() < 0.4 else 0)

    def op_rpx(self):
        """M10-8C-1: an exit within min_rent_s pays R_due = R_min, above the rent accrued; the
        lender part above the credited rent vests with the lump."""
        p = self.p
        if not p.positions:
            return
        k = self.rng.choice(list(p.positions))
        q, io, pr, dl = p.positions[k]
        paid = min(pm.cdiv(q * (p.index - io), pm.WAD) + self.amt("rmin"), U64)
        top = max(paid - paid * pr // pm.BPS - p._credited_for(q, io, pr), 0)
        p.repay(k, paid, self.amt("lump") if self.rng.random() < 0.5 else 0)
        self.top_ups += top > 0

    def op_stx(self):
        """M10-8C-1: a settlement whose R_due (paid first by the immediate part) exceeds the
        rent accrued: an early settlement's R_min, or a settlement past min_rent_s, whose rent
        carries no protocol share."""
        p = self.p
        if not p.positions:
            return
        k = self.rng.choice(list(p.positions))
        q, io, pr, dl = p.positions[k]
        rent = min(pm.cdiv(q * (p.index - io), pm.WAD) + self.amt("rmin"), U64)
        sol = self.amt("sol")
        lump = self.rng.randrange(0, sol + 1) if self.rng.random() < 0.4 else 0
        top = max(min(sol - lump, rent) - p._credited_for(q, io, pr), 0)
        p.settle(k, sol, lump, rent)
        self.top_ups += top > 0

    def op_wipe(self):
        """Run + full SOL settlement: borrow what the cap allows, settle everything."""
        p = self.p
        if p.T > 0 and p.S > 0:
            q = p.T * p.util_cap // pm.BPS
            if q > 0 and (p.B + q) * pm.BPS <= p.util_cap * (p.T + p.B):
                self.pid += 1
                p.borrow(self.pid, q)
            for k in list(p.positions):
                p.settle(k, self.amt("sol"))

    def op_deep(self):
        """Settle all but a few raw units of the claims: a deep rebase (1,000^k, k > 1)."""
        p = self.p
        if p.T > 3 and p.S > 0:
            q = p.T - self.rng.randint(1, 3)
            if (p.B + q) * pm.BPS <= p.util_cap * (p.T + p.B):
                self.pid += 1
                p.borrow(self.pid, q)
            for k in list(p.positions):
                p.settle(k, self.amt("sol"))

    def op_clm(self):
        self.p.claim_sol(self.who())

    def op_clmN(self):
        """V2-2: repeated claims in one transaction pay 0 after the first."""
        u = self.who()
        self.p.claim_sol(u)
        again = sum(self.p.claim_sol(u) for _ in range(self.rng.randrange(1, 6)))
        assert again == 0

    def op_cov(self):
        p, u = self.p, self.who()
        if u not in p.lenders:
            return
        L = p._touch(u)
        if L.pending < 0:
            p.cover(u, -L.pending)
        elif self.rng.random() < 0.2:
            p.cover(u, self.amt("lump"))

    def op_swp(self):
        self.p.sweep_sol(self.amt("sweep"))

    def op_chn(self):
        """V2-3: another position opens and closes; no lock may shrink."""
        p = self.p
        if not (p.T > 0 and p.B > 0):
            return
        locks = {w: p._lock_of(p._touch(w)) for w in list(p.lenders)}
        self.pid += 1
        p.borrow(self.pid, self.rng.randrange(1, p.T + 1), 3600)
        p.repay(self.pid, 0, 0)
        for w, lk in locks.items():
            assert p._lock_of(p._touch(w)) >= lk, ("churn released a lock", w)

    def op_tch(self):
        self.p._touch(self.who())

    def op_view(self):
        """The untouched-state I-P4 sample (R4A-2 / R4B-3)."""
        p = self.p
        p._cal_advance()
        tot = sum(p.allowance_view(w) for w in list(p.lenders))
        assert tot <= p.T + len(p.lenders), ("I-P4 untouched", tot, p.T)

    def op_lock(self):
        p, u = self.p, self.who()
        if u in p.lenders:
            p._lock_of(p.lenders[u])

    def op_brn(self):
        """KI-65: a burn of fewer than 1,000 shares that pays 0 tokens, so the lender's shares
        end in a random residue below the next rebase's unit (dust conversions)."""
        p, u = self.p, self.who()
        if u not in p.lenders:
            return
        L = p._touch(u)
        zero = pm.cdiv(p.S + pm.VS, p.T + p.B + pm.VA) - 1     # the largest burn that pays 0 tokens
        s = last_shares_kept(p, min(self.rng.randrange(1, pm.REBASE), zero, L.shares))
        if s > 0 and L.epoch == p.epoch:
            p.withdraw(u, s)

    def op_exit(self):
        """KI-65: a lender leaves whole (pool_model.exit_all's burns), e.g. while a vest runs;
        while tokens are borrowed the burn that would take S to 0 keeps one share."""
        p, u = self.p, self.who()
        if u not in p.lenders:
            return
        for _ in range(64):
            L = p._touch(u)
            if not L.shares or L.epoch != p.epoch:
                break
            allow = p.free_allowance(u)
            zero = pm.cdiv(p.S + pm.VS, p.T + p.B + pm.VA) - 1
            fit = allow * (p.S + pm.VS) // (p.T + p.B + pm.VA)
            s = last_shares_kept(p, min(L.shares, max(fit, zero, 1)))
            if s == 0:
                break
            p.withdraw(u, s)
        p.claim_sol(u)

    def op_wdg(self):
        """op_wd, but never the burn that takes S to 0 while tokens are borrowed."""
        p, u = self.p, self.who()
        L = p.lenders.get(u)
        if not (L and L.shares):
            return
        allow = p.free_allowance(u)
        pend = L.pending
        if L.shares == 0:
            return
        r = self.rng.random()
        at = max(1, allow * (p.S + pm.VS) // (p.T + p.B + pm.VA))
        s = (self.rng.randrange(1, L.shares + 1) if r < 0.4 else at if r < 0.8
             else L.shares if r < 0.9 else 1)
        s = last_shares_kept(p, min(s, L.shares))
        if s == 0:
            return
        got = p.withdraw(u, s)
        assert pend >= 0 and got <= allow, ("ghost C0-9/C0-13", pend, got, allow)

    def op_ord(self):
        """KI-65: permissionless syncs of every lender in a random order (the conversion order
        decides who receives each record's rounding unit)."""
        for u in self.rng.sample(list(self.p.lenders), len(self.p.lenders)):
            self.p._touch(u)

    def pot(self, u):
        """The pot-only touch of `u`; counts the touches that hold at a rebase record."""
        p = self.p
        L = p.lenders.get(u)
        if L and L.epoch < p.epoch and p.epochs[L.epoch][1] and (L.shares or L.lock or L.lock_term):
            self.holds += 1
        p._touch_pot(u)

    def op_syw(self):
        """KI-66 full: a third party's sync_lender(n) with n records from the lender's epoch, or from
        its forward chain's epoch (the forward chain through the later records)."""
        p = self.p
        u = self.who()
        if u in p.lenders:
            L = p.lenders[u]
            if L.fwd or (L.epoch < p.epoch and p.epochs[L.epoch][1] and (L.shares or L.lock or L.lock_term)):
                self.holds += 1
            p.sync_lender(u, self.rng.randrange(1, 9), False)

    def op_sys(self):
        """KI-66 full: the owner's own sync_lender(n) with few records: a partial conversion that
        can stop inside a forward chain (FWD_OPEN)."""
        p = self.p
        u = self.who()
        if u in p.lenders:
            p.sync_lender(u, self.rng.choice([1, 1, 2, 3]), True)

    def op_own1(self):
        """KI-66 full: the owner's sync_lender(1) of a lender with a forward chain (or behind a rebase
        record), so its conversion stops inside the chain (FWD_OPEN); else a random lender's."""
        p = self.p
        chained = [u for u, L in p.lenders.items() if L.fwd]
        u = self.rng.choice(chained) if chained and self.rng.random() < 0.8 else self.who()
        if u in p.lenders:
            p.sync_lender(u, 1, True)

    def op_reb1(self):
        """KI-66 full: a settlement that leaves about S / (2 R_MAX) raw tokens: a rebase by 1/1,000 (k = 1),
        whose conv_rem the residues of op_rem9 can put near den - 1."""
        p = self.p
        keep = max(1, p.S // (2 * pm.R_MAX))
        if p.T > keep and p.S > 0 and p.B == 0:
            self.pid += 1
            p.borrow(self.pid, p.T - keep)
            p.settle(self.pid, self.amt("sol"))

    def op_rem9(self):
        """KI-66 full: a 0-token burn that leaves the lender's shares at 999 modulo 1,000, so the first
        conversion through the next rebase record leaves its conv_rem near den - 1 and the later
        conversions carry a unit (a forward chain's lag of 1)."""
        p, u = self.p, self.who()
        if u not in p.lenders:
            return
        L = p._touch(u)
        zero = pm.cdiv(p.S + pm.VS, p.T + p.B + pm.VA) - 1
        s = last_shares_kept(p, min((L.shares - 999) % pm.REBASE, zero, L.shares))
        if s > 0 and L.epoch == p.epoch:
            p.withdraw(u, s)

    def op_pot(self):
        """KI-66: a third party's sync_lender of a lender it does not own: the pot-only touch
        (behind a rebase record: accrued to the record's ends, surplus to the pot, unconverted)."""
        self.pot(self.who())

    def op_potall(self):
        """KI-66: pot-only touches of every lender in a random order (some twice: idempotent), then
        every lender claims in a random order (the deficit lenders draw on the pot)."""
        p, rng = self.p, self.rng
        for u in rng.sample(list(p.lenders), len(p.lenders)):
            self.pot(u)
            if rng.random() < 0.2:
                self.pot(u)
        for u in rng.sample(list(p.lenders), len(p.lenders)):
            p.claim_sol(u)


def last_shares_kept(p, shares: int) -> int:
    """`shares`, less the one share a withdrawal of all of S must keep while tokens are borrowed:
    the program refuses that burn (`LastSharesBorrowed`, ARCH §9 M0b note 4), pool_model does
    not model the rule, so the KI-65 profiles never ask for it (the trace replays through the
    program stay op for op)."""
    return shares - 1 if shares == p.S and p.B > 0 else shares


def burn_takes_the_last_share(p, who, shares: int) -> bool:
    """Whether `w who shares` would succeed and leave S = 0 while tokens are borrowed: the burn the
    program refuses (`LastSharesBorrowed`) and pool_model does not model (`last_shares_kept`). The
    random withdrawals (`op_wd`, `op_run`) then keep one share. The burn is tried exactly as
    `rec_call` runs it and rolled back, so nothing is recorded; a burn the model refuses anyway
    stays in the trace as a refusal."""
    if shares != p.S or p.B == 0:
        return False
    snap = p.rec_snapshot()
    p.rec_depth += 1
    try:
        BasePool._release(p)
        BasePool._touch(p, who)
        if p.rec_violation():
            return False
        BasePool.withdraw(p, who, shares)
        return not p.rec_violation() and p.S == 0 and p.B > 0
    except ValueError:
        return False
    finally:
        p.rec_depth -= 1
        p.rec_restore(snap)


def normal_amounts():
    return {
        "dep": lambda r, p: r.choice([1, 7, 10**6, 10**9, 10**12, r.randrange(1, 10**13)]),
        "bor": lambda r, p: r.randrange(1, p.T + 1),
        "dindex": lambda r, p: r.randrange(0, 10**15),
        "sol": lambda r, p: r.randrange(0, 10**12),
        "lump": lambda r, p: r.randrange(0, 10**9),
        "sweep": lambda r, p: r.choice([0, 1, r.randrange(1, 10**9)]),
        "rmin": lambda r, p: r.choice([1, r.randrange(1, 10**6), r.randrange(10**5, 10**9)]),
    }


def dust_amounts():
    return {
        "dep": lambda r, p: r.choice([1, 1, 2, 3, r.randrange(1, 40)]),
        "bor": lambda r, p: r.randrange(1, p.T + 1),
        "dindex": lambda r, p: r.choice([0, r.randrange(0, 10**21), r.randrange(0, 10**22)]),
        "sol": lambda r, p: r.randrange(0, 12),
        "lump": lambda r, p: r.randrange(0, 4),
        "sweep": lambda r, p: r.randrange(0, 4),
        "rmin": lambda r, p: r.randrange(0, 12),
    }


def extreme_amounts():
    def dep(r, p):
        return r.choice([U64, U64 - r.randrange(0, 10**6), 2**63, U64 // 3, r.randrange(2**60, 2**64),
                         r.randrange(1, 10**6), 1, U64 - p.T - p.B if p.T + p.B <= U64 else 1])

    def bor(r, p):
        return r.choice([p.T, max(1, p.T - r.randrange(0, 1000)), r.randrange(1, p.T + 1)])
    return {
        "dep": dep,
        "bor": bor,
        "dindex": lambda r, p: r.randrange(0, 10**9),
        "sol": lambda r, p: r.choice([0, 1, r.randrange(0, 10**14), 10**14]),
        "lump": lambda r, p: r.randrange(0, 10**12),
        "sweep": lambda r, p: r.choice([0, r.randrange(0, 10**13)]),
        "rmin": lambda r, p: r.choice([1, r.randrange(0, 10**12), 10**13]),
    }


LAZY = (["dep"] * 3 + ["wd"] * 3 + ["run", "bor", "bor", "acc", "acc", "adv", "rep", "rep", "set", "set",
        "clm", "clm", "clmN", "cov", "swp", "swp", "wipe", "deep", "chn", "tch", "gov", "view", "lock"])
TOPUP = LAZY + ["bor"] * 6 + ["rpx"] * 4 + ["stx"] * 4 + ["dep", "clm", "wd"]
ORPHANS = (["dep"] * 3 + ["brn"] * 3 + ["deep"] * 3 + ["exit"] * 2 + ["tch"] * 2 +
           ["wdg", "bor", "acc", "adv", "rep", "set", "clm", "clmN", "cov", "swp", "swp", "wipe", "ord", "view"])
FWD = (["dep"] * 3 + ["deep"] * 3 + ["pot"] * 3 + ["syw"] * 4 + ["sys"] * 3 + ["bor"] * 2 + ["acc"] * 2 +
       ["rep"] * 2 + ["adv", "set", "clm", "clm", "swp", "wd", "brn", "exit", "cov", "view", "wipe", "potall"])
FWD_DEEP = FWD + ["deep"] * 6 + ["sys"] * 3 + ["syw"] * 3 + ["bor"] * 2 + ["acc"] * 2
FWD_OPEN = (["dep"] * 3 + ["rem9"] * 3 + ["deep"] * 2 + ["reb1"] * 4 + ["syw"] * 6 + ["own1"] * 6 +
            ["bor"] * 2 + ["acc"] * 2 + ["rep", "adv", "set", "swp", "clm", "cov"])
POT = (["dep"] * 3 + ["deep"] * 3 + ["pot"] * 6 + ["potall"] * 2 + ["bor"] * 2 + ["acc"] * 2 + ["rep"] * 2 +
       ["adv", "set", "clm", "swp", "wd", "brn", "exit", "cov", "view", "wipe"])
RUNS = (["dep"] * 3 + ["run"] * 3 + ["wd", "bor", "bor", "acc", "adv", "rep", "set", "set", "clm", "clmN",
        "cov", "swp", "wipe", "chn", "chn", "chn", "tch", "view"])


def ext_profiles() -> list:
    return [
        Ext("ext_lazy_continuous_100", 1, 5_000, 10_000, 8, LAZY, normal_amounts(),
            "lazy lenders (no touch-all), util cap 100 %: full wipes, lenders parked on a live wipe "
            "record, post-wipe sweeps (V2-12, R4A-7), deep rebases, many epochs behind, expiring locks"),
        Ext("ext_lazy_continuous_90", 2, 5_000, 9_000, 8, LAZY, normal_amounts(),
            "lazy lenders, util cap 90 %: rebases, locks expiring untouched, churn, time, governance"),
        Ext("ext_lazy_minima_90", 8, 4_000, 9_000, 6, LAZY, normal_amounts(),
            "lazy lenders, lump_vest_s and lock_grace_s at their compiled minima (3,600 s)",
            lump_vest_s=3_600, lock_grace_s=3_600),
        Ext("ext_runs_continuous_100", 3, 5_000, 10_000, 3, RUNS, normal_amounts(),
            "three lenders, runner loops at the allowance with churn in between (C0-9, V2-3), wipes"),
        Ext("ext_runs_continuous_90", 4, 4_000, 9_000, 3, RUNS, normal_amounts(),
            "three lenders, runner loops with churn, 90 % cap"),
        Ext("ext_dust_continuous_100", 5, 5_000, 10_000, 6, LAZY, dust_amounts(),
            "dust amounts: 1..40 raw tokens, 0..11 lamports: virtual offsets, carries, rounding"),
        Ext("ext_extreme_continuous_100", 6, 4_000, 10_000, 5, LAZY, extreme_amounts(),
            "u64-extreme token amounts: T + B at the u64 ceiling (deposits refused with overflow), "
            "huge share counts, deep rebases"),
        Ext("ext_extreme_continuous_90", 7, 3_000, 9_000, 5, LAZY, extreme_amounts(),
            "u64-extreme token amounts, 90 % cap, lumps up to 1,000 SOL"),
        Ext("ext_topup_continuous_90", 9, 4_000, 9_000, 6, TOPUP, normal_amounts(),
            "M10-8C-1: exits paying rent above what accrued (rpx, stx: R_min-like excesses up to "
            "1 SOL) among the lazy-lender mix, 90 % cap"),
        Ext("ext_topup_continuous_100", 10, 4_000, 10_000, 6, TOPUP, normal_amounts(),
            "M10-8C-1: exits paying rent above what accrued, 100 % cap: full wipes flush the top-up"),
        Ext("ext_topup_minima_90", 11, 3_000, 9_000, 4, TOPUP, normal_amounts(),
            "M10-8C-1: exits paying rent above what accrued, lump_vest_s at its compiled minimum "
            "(3,600 s)", lump_vest_s=3_600, lock_grace_s=3_600),
        Ext("ext_topup_dust_100", 12, 3_000, 10_000, 6, TOPUP, dust_amounts(),
            "M10-8C-1: rent excesses of 0..11 lamports: top-ups at the rounding scale"),
        Ext("ext_topup_extreme_100", 13, 2_000, 10_000, 5, TOPUP, extreme_amounts(),
            "M10-8C-1: u64-extreme token amounts, rent excesses up to 10,000 SOL"),
        Ext("ext_orphans_continuous_100", 14, 4_000, 10_000, 6, ORPHANS, normal_amounts(),
            "KI-65: lazy lenders through forced deep rebases, converting in random orders; 0-token burns "
            "leave dust residues below the next rebase's unit; whole exits while lumps vest"),
        Ext("ext_orphans_dust_100", 15, 4_000, 10_000, 8, ORPHANS, dust_amounts(),
            "KI-65: dust amounts (1..40 raw tokens): most holdings are below one new share at a rebase, "
            "so the records' rounding units decide who holds S; whole exits while lumps vest"),
        Ext("ext_pot_continuous_100", 16, 4_000, 10_000, 6, POT, normal_amounts(),
            "KI-66: lazy lenders and third-party syncs (pot-only touches: a lender behind a rebase record "
            "is accrued to its ends and its surplus moves to the pot, unconverted), some repeated, then "
            "claims; late joiners and early leavers around deep rebases, rent paid across them"),
        # Seeds 82 and 66 (were 17 and 20): since AIR-07's residual fix a deposit into a drained
        # pool is refused, which left the old seeds 8 and 5 pot-only holds (22 and 17 before).
        # Seed 66 (23 holds) also reaches withdrawals the model refuses with negative_pending
        # while the program's lender is not short (lender_it traces.rs, `sol_gaps`).
        Ext("ext_pot_dust_100", 82, 4_000, 10_000, 8, POT, dust_amounts(),
            "KI-66: pot-only touches with dust amounts (1..40 raw tokens, 0..11 lamports)"),
        Ext("ext_pot_continuous_90", 18, 4_000, 9_000, 6, POT, normal_amounts(),
            "KI-66: pot-only touches, 90 % cap: positions stay open across rebases"),
        Ext("ext_pot_fwd_100", 19, 4_000, 10_000, 6, FWD, normal_amounts(),
            "KI-66 full: windowed third-party syncs (the forward chain through later records) and the "
            "owners' partial conversions inside a chain (FWD_OPEN)"),
        Ext("ext_pot_fwd_deep_100", 21, 4_000, 10_000, 4, FWD_DEEP, normal_amounts(),
            "KI-66 full: four lenders, frequent deep rebases: chains through several records, owners "
            "stopping inside them"),
        Ext("ext_pot_fwd_dust_100", 66, 4_000, 10_000, 8, FWD, dust_amounts(),
            "KI-66 full: the forward chain with dust amounts (most chains floor to 0 shares)"),
        Ext("ext_pot_fwd_open_100", 22, 4_000, 10_000, 6, FWD_OPEN, normal_amounts(),
            "KI-66 full: owners stopping inside forward chains (sync_lender(1) of a chained lender: "
            "FWD_OPEN), residues at 999 modulo 1,000 before rebases so conv_rem sits near den - 1 and "
            "the owner's shares run a unit above the chain (lag 1), chains walked to the current epoch "
            "before the next roll (epoch == fwd_epoch < share_epoch)"),
    ]


def scripted_lock_overflow() -> dict:
    """ARCH §9 M0b carried-over note (1). A lock exists, the borrowed inventory falls to one
    raw unit (a net reduction: Pi = ceil(WAD * 1 / b_min) = 1), then it is borrowed again. A
    new lock's term floor(lock * WAD / Pi) no longer fits u128, so that withdrawal is refused
    (Overflow); a small one still fits; everything clears when B_Q reaches 0."""
    with recording(line_mode="op", explicit=True, verify=True) as reg:
        p = pm.Pool("continuous", util_cap_bps=10_000)
    big = 2**62
    for who in ("a", "b", "c"):
        p.deposit(who, big)
    p.borrow(1, 2 * big - 1)
    p.borrow(2, 1)
    p.withdraw("a", max(1, p.free_allowance("a") * (p.S + pm.VS) // (p.T + p.B + pm.VA)))   # b_min = B
    p.repay(1, 0, 0)                                  # B_Q: 2^63 -> 1, Pi -> 1
    p.borrow(3, p.T // 2)                             # the inventory is out again, Pi stays 1

    def at_allowance(who):
        return max(1, p.free_allowance(who) * (p.S + pm.VS) // (p.T + p.B + pm.VA))

    for who in ("b", "c"):
        try:
            p.withdraw(who, at_allowance(who))        # lock term > u128: refused
        except ValueError:
            pass
    p.withdraw("c", 1)                                # a one-share withdrawal takes no lock
    p.withdraw("c", at_allowance("c") // 10**20)      # a small lock still fits
    p._lock_of(p._touch("c"))
    p.repay(2, 0, 0)
    p.settle(3, 10**9)                                # B_Q -> 0: the Pi epoch rolls, Pi = WAD
    p.withdraw("b", at_allowance("b"))
    p.claim_sol("a")
    return trace_json(p, "ext_scripted_lock_terms", "scripted, ARCH §9 M0b note (1): lock terms at the u128 "
                      "ceiling while Pi is 1; an overflowing term refuses the withdrawal (C0-9, V2-3)")


def scripted_no_shares() -> dict:
    """ARCH §9 M0b carried-over note (4). The last lender can burn all its shares while one
    borrowed raw unit is out (its claim on that unit floors to 0). With S = 0 and B_Q > 0,
    continuous rent reaches the protocol sink before any SOL arrived: it takes lender_sol
    that backs the leaver's realised allocation (the model's check() fails on those lines,
    I-P2 claim side), and the position's true-up then debits the protocol by more than it
    received, which the program's u64 protocol_fees refuses (the close is refused until a
    deposit brings S back above 0)."""
    with recording(line_mode="op", explicit=True, verify=True) as reg:
        p = pm.Pool("continuous", util_cap_bps=9_000)

    def exit_all(who):
        p.free_allowance(who)
        p.withdraw(who, p.lenders[who].shares)

    p.deposit("a", 3)
    p.sweep_sol(1_000)                               # a lump that vests to a
    p.withdraw("a", 1234)
    p.borrow(1, 1)
    exit_all("a")                                    # S = 0 while B_Q = 1; a is paid the vest
    p.accrue(12 * pm.WAD)                            # 10 lamports of entitlement -> protocol, from a's SOL
    p.claim_sol("a")                                 # a is short
    try:
        p.repay(1, 0, 0)                             # credited 11 > the protocol's 10: refused
    except ValueError:
        pass
    p.sweep_sol(0)                                   # AIR-08: a deposit after the last exit waits for it
    p.deposit("b", 10**6)                            # S > 0 again (no wipe record)
    p.repay(1, 0, 0)                                 # the shortfall debits b
    p._touch("b")
    p.cover("b", -p.lenders["b"].pending)
    p.claim_sol("a")
    return trace_json(p, "ext_scripted_no_shares", "scripted, ARCH §9 M0b note (4): S = 0 while B_Q > 0 "
                      "after a dust exit; rent credited to the protocol sink takes lender SOL (pool_model."
                      "check() fails on the listed lines) and the close's true-up is refused while S = 0")


def scripted_overlock() -> dict:
    """ARCH §9 M0b carried-over note (2), fixed in v0.4.1 (found by M0's tests/pool_props.rs):
    borrowed tokens come back but B_Q never falls below b_min, so V2-3 keeps the locks on
    almost every share. v0.4's ceil'd locked exceeded the sum of the lenders' locks and the
    allowances summed to 1.79 T; with floored terms and aggregate the sum stays <= T."""
    with recording(line_mode="op", explicit=True, verify=True) as reg:
        p = pm.Pool("continuous", util_cap_bps=9_000)

    def at(who):
        return min(max(1, p.free_allowance(who) * (p.S + pm.VS) // (p.T + p.B + pm.VA)), p.lenders[who].shares)

    p.deposit("a", 7_814_436_457)
    p.borrow(1, 266_932_504)
    p.borrow(2, 267_199)
    p.free_allowance("a")
    p.withdraw("a", 1)                               # first lock: b_min = B_Q
    p.repay(1, 0, 0)                                 # net reduction: Pi falls to ~1e15
    p.deposit("b", 11_938_382_295)
    p.borrow(3, 267_199)
    p.withdraw("b", at("b"))
    p.withdraw("a", at("a"))
    p.repay(3, 0, 0)                                 # B_Q back to b_min: no decay (V2-3)
    p.free_allowance("a")
    p.free_allowance("b")
    for w in ("a", "b"):
        p.allowance_view(w)
    p.withdraw("a", at("a"))
    p.free_allowance("b")
    p.withdraw("b", at("b"))
    return trace_json(p, "ext_scripted_overlock", "scripted, ARCH §9 M0b note (2): locks kept on almost every "
                      "share after the borrowed tokens returned (V2-3); v0.4.1's floored terms keep the "
                      "allowances within T")


def scripted_empty_lock_term() -> dict:
    """M0b review M0B-A-1 (the reviewers' minimal model trace). Pi decays below WAD (a net
    reduction of B_Q below b_min), so a lock's term floor(lock * WAD / Pi) exceeds the lock;
    a 1/10^6 rebase then floors the lender's shares and lock to 0 but leaves its term at 1,
    and the lender jumps (V2-12). A full wipe rolls again: the model's _touch reads that
    record and scales the term (1 * 0 // 1 = 0) before jumping, so the lender's next touch
    needs the record even though it has no shares and no lock."""
    with recording(line_mode="op", explicit=True, verify=True) as reg:
        p = pm.Pool("continuous", util_cap_bps=10_000)

    def at_allowance(who):
        return p.free_allowance(who) * (p.S + pm.VS) // (p.T + p.B + pm.VA)

    p.deposit("Y", 2_000_000)
    p.deposit("X", 1_000)
    p.borrow(1, 1_500_000)
    p.borrow(2, 400_000)
    p.withdraw("X", at_allowance("X"))
    p.repay(2, 0, 0)                                 # B_Q < b_min: Pi decays to 0.789 WAD
    p.withdraw("X", at_allowance("X"))               # term = lock * WAD / Pi > lock
    p.borrow(3, p.T - 1)
    p.settle(3, 0)
    p.settle(1, 0)                                   # T + B = 1: a 1/10^6 rebase
    p._touch("X")                                    # X: shares 0, lock 0, lock_term 1 (jumps)
    assert (p.lenders["X"].shares, p.lenders["X"].lock, p.lenders["X"].lock_term) == (0, 0, 1)
    p.borrow(5, 1)
    p.settle(5, 0)                                   # a full wipe
    p.sweep_sol(0)
    p.deposit("Y", 1_000)                            # closes the wipe record
    p._touch("X")                                    # the model scales the term: 1 * 0 // 1 = 0
    assert p.lenders["X"].lock_term == 0
    return trace_json(p, "ext_scripted_empty_lock_term", "scripted, M0b review M0B-A-1: an empty lender "
                      "(shares and lock floored to 0 by a rebase, lock term 1) reads the next roll's record, "
                      "which scales its term to 0, before the V2-12 jump")


def scripted_topup_jit() -> dict:
    """Finding M10-8C-1 (review of WP-6 batch 2): a whale's [deposit 9x, exit, claim,
    withdraw, claim] in one second around an exit a minute after the open. A repay pays
    R_min = 171,233 lamports (lender part 145,549 at a 15 % protocol share) against about
    6,800 credited; a settlement pays R_due = R_min first out of its immediate part, beyond
    the token value of the extinguished debt. The top-up vests: the whale realises 0 of
    it (at most 2 lamports of rounding on the repay; its share of the token value on the
    settlement); it is paid to the shares that hold through lump_vest_s."""
    with recording(line_mode="op", explicit=True, verify=True) as reg:
        p = pm.Pool("continuous", util_cap_bps=9_000)
    r_min = 171_233
    p.rent_protocol_bps = 1_500
    p.deposit("h", 10**12)
    p.borrow(1, 10**11, 3600)
    p.borrow(2, 10**11, 3600)
    p.advance(60, 8 * 10**10)                        # a minute of rent: ~6,800 lamports credited

    def jit(who, exit_, top, token_part):
        s_before = p.S
        minted = p.deposit(who, 9 * 10**12)
        exit_()
        paid = p.claim_sol(who)
        a = p.free_allowance(who)
        shares = min(max(1, a * (p.S + pm.VS) // (p.T + p.B + pm.VA)), p.lenders[who].shares)
        p.withdraw(who, shares)
        paid += p.claim_sol(who)
        bound = token_part * minted // (s_before + minted) + 2
        assert top > 100_000 and paid <= bound, (who, paid, bound, top)
        return paid

    q, io, pr, _ = p.positions[1]
    credited = p._credited_for(q, io, pr)
    lender_rent = r_min - r_min * pr // pm.BPS
    jit("w", lambda: p.repay(1, r_min, 10**6), lender_rent - credited, 0)
    q, io, pr, _ = p.positions[2]
    credited = p._credited_for(q, io, pr)
    sol, lump = 5 * 10**9, 10**7
    jit("x", lambda: p.settle(2, sol, lump, r_min), r_min - credited, sol - lump - r_min)
    p.advance(p.lump_vest_s + 1)
    for who in ("h", "w", "x"):
        p.claim_sol(who)
    return trace_json(p, "ext_topup_scripted_jit", "scripted, finding M10-8C-1: a 9x whale's one-second "
                      "[deposit, exit, claim, withdraw, claim] around an early repay (R_min above the credited "
                      "rent) and an early settlement (R_due = R_min paid first): the top-up vests, the whale "
                      "realises none of it at once; the shares holding through lump_vest_s are paid it")


def orphan_demo_traces() -> list:
    """KI-65: the default-switch (v0.4.2) pools of pool_model's orphan demos, every result
    explicit, so the replay pins each claim (the v0.4.1 comparison pools are not recorded)."""
    traces = []
    for demo, args in (("whale_order_demo", ()), ("orphan_vest_demo", (24,))):
        with recording(line_mode="op", explicit=True, taint=True, verify=True) as reg:
            ret = getattr(pm, demo)(*args)
            pools = list(reg)
        keep = [p for p in pools if kept(p)]
        assert keep, demo
        for i, p in enumerate(keep):
            traces.append(trace_json(p, f"ext_orphans_{demo}_{i}", f"pool_model.{demo}{args or '()'} v0.4.2, "
                                     f"default-switch pool #{i}; demo returned {ret!r}"))
    return traces


# ---- 5. demos ------------------------------------------------------------------------------
DEMOS = ["jit_demo", "jit_hold_table", "inflation_demo", "run_demo", "claim_loop_demo", "run_util_table",
         "wipe_demo", "wipe_tail_demo", "zero_share_demo", "protocol_change_demo", "escape_demo",
         "stuck_lock_table", "stale_lock_demo", "lock_tail_demo", "wipe_gate_demo", "vest_exit_demo",
         "restart_grief_table", "premium_jit_table", "pot_touch_demo", "pot_chain_demo", "rebase_vest_demo",
         "empty_pool_gate_demo", "lock_era_demo"]
PATCHED = ("_resolve", "_maybe_roll", "claim_sol", "_touch", "withdraw", "deposit", "_credit")


def kept(p) -> bool:
    """Default switches only, no method patched on the instance, no state changed outside
    the pool API (the v0.1/v0.2 variants and the analysis rows are not recorded)."""
    if p.rec_switches != DEFAULT_SWITCHES or p.prorata is not True:
        return False
    if any(k in p.__dict__ for k in PATCHED):
        return False
    if p.rec_last_state is not None and state_str(p) != p.rec_last_state:
        p.rec_tainted = True
    return not p.rec_tainted and bool(p.rec_lines)


def scenario_traces() -> list:
    traces = []
    for demo in DEMOS:
        with recording(line_mode="op", explicit=True, taint=True, verify=True) as reg:
            ret = getattr(pm, demo)()
            pools = list(reg)
        keep = [p for p in pools if kept(p)]
        assert keep, demo
        for i, p in enumerate(keep):
            traces.append(trace_json(p, f"{demo}_{i}", f"pool_model.{demo}() v0.4.2, default-switch variant #{i}; "
                                     f"demo returned {ret!r}"))
    with recording(line_mode="op", explicit=True, taint=True, verify=True) as reg:
        with contextlib.redirect_stdout(io.StringIO()):
            runpy.run_path(str(SIM / "attacks" / "pool_edge.py"), run_name="__main__")
        pools = list(reg)
    keep = [p for p in pools if kept(p)]
    assert keep
    for i, p in enumerate(keep):
        traces.append(trace_json(p, f"pool_edge_{i}", f"attacks/pool_edge.py v0.4.2, pool #{i}"))
    return traces


def file_text(traces: list, source: str) -> str:
    out = {
        "generator": Path(__file__).name,
        "source": source,
        "format": "see the docstring of gen_pool.py",
        "traces": traces,
    }
    return json.dumps(out, indent=1, sort_keys=True) + "\n"


def write(name: str, traces: list, source: str):
    text = file_text(traces, source)
    assert len(text) <= MAX_FILE_BYTES, (name, len(text))
    Path(__file__).with_name(name).write_text(text)


def pack(traces: list, stem: str, source: str) -> int:
    """Writes traces into as few files as fit MAX_FILE_BYTES: <stem>_<k>.json."""
    groups, cur = [], []
    for t in traces:
        if cur and len(file_text(cur + [t], source)) > MAX_FILE_BYTES:
            groups.append(cur)
            cur = []
        cur.append(t)
    if cur:
        groups.append(cur)
    for k, g in enumerate(groups, 1):
        write(f"{stem}_{k}.json", g, source)
    return len(groups)


def main() -> None:
    src = "docs/design/sim/pool_model.py v0.4.2"
    for old in Path(__file__).parent.glob("pool_*.json"):
        old.unlink()
    for t in pm_fuzz_traces():
        write(t["name"].replace("pm_fuzz", "pool_fuzz") + f"_{t['part']}.json", [t],
              f"{src}: fuzz() driven through a recording Pool (part {t['part']} of {t['parts']})")
    pack(lock_fuzz_traces(), "pool_lock_fuzz", f"{src}: lock_fuzz() (R4A-2 / R4B-3) driven through a recording Pool")
    stranded, row = stranded_traces()
    pack(stranded, "pool_stranded", f"{src}: stranded_table() v0.4 rows (R3A-2; attacks/pool_r3.py); "
         f"table row (inflow, stranded, worst, above 0.01 SOL) = {row}")
    ext = {e.name: e.run() for e in ext_profiles()}
    ext["ext_scripted_lock_terms"] = scripted_lock_overflow()
    ext["ext_scripted_no_shares"] = scripted_no_shares()
    ext["ext_scripted_overlock"] = scripted_overlock()
    ext["ext_scripted_empty_lock_term"] = scripted_empty_lock_term()
    ext["ext_topup_scripted_jit"] = scripted_topup_jit()
    for t in orphan_demo_traces():
        ext[t["name"]] = t
    for group in ("lazy", "runs", "dust", "extreme", "topup", "orphans", "pot"):
        traces = [t for n, t in ext.items() if n.startswith(f"ext_{group}")
                  or (group == "extreme" and n.startswith("ext_scripted"))]
        pack(traces, f"pool_ext_{group}", f"{src}: Pool driven by gen_pool.py's extended fuzz profiles")
    pack(scenario_traces(), "pool_scenarios", f"{src}: the default-switch demos of pool_model (ECON §9.3–§9.6, "
         f"round-3 and round-4 cases) and attacks/pool_edge.py")


if __name__ == "__main__":
    main()
