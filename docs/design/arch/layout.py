"""ARCH v0.4.1 account layouts (as built through M6; #99 max_q_hwm and #100 min_open_fee_ppm PROPOSED; M5-SEC-6 derived keys): explicit, no implicit padding, every field align <= 8 (PodU128/PodI128 = [u8;16]).

M6 and the owner's decisions of 2026-09-30 (ARCH changelog 56, ECON #105) change no layout: `pool_d` holds a
Raydium pool's locked depth and, on PumpSwap, the canonical pool's burned-LP depth `D` (refreshed by every
executable observation from the LP mint's supply; 0 at the transition); M6-SEC-1, M6-SEC-3 and M6R-4 add no field.

The AI review's fixes take reserved bytes, no size changes: Market.lender_credit_rem @2,936 (AIR-03) and
min_open_hard_cap_bps @2,952 (AIR-19), VenueGuard.guard_len @160 and guard_elf @176 (AIR-01)."""
import sys
SZ = {"u8":1,"u16":2,"u32":4,"u64":8,"i64":8,"Pubkey":32,"PodU128":16,"PodI128":16}
AL = {"u8":1,"u16":2,"u32":4,"u64":8,"i64":8,"Pubkey":1,"PodU128":1,"PodI128":1}
def arr(t,n): return (t,n)
def size_of(t):
    if isinstance(t, tuple):
        base, n = t
        return size_of(base)*n
    if t in SZ: return SZ[t]
    return STRUCTS[t][1]
def align_of(t):
    if isinstance(t, tuple): return align_of(t[0])
    if t in AL: return AL[t]
    return STRUCTS[t][2]
STRUCTS = {}
def layout(name, fields, disc=0, report=True):
    off = disc; rows = []; al = 1
    for grp, fname, t in fields:
        a = align_of(t); al = max(al, a)
        if off % a:
            raise SystemExit(f"{name}.{fname}: offset {off} not aligned to {a} -> add explicit padding")
        s = size_of(t)
        rows.append((grp, fname, t, off, s)); off += s
    if (off - disc) % al: raise SystemExit(f"{name}: size {off-disc} not a multiple of align {al}")
    STRUCTS[name] = (rows, off - disc, al)
    return rows, off

def tname(t):
    if isinstance(t, tuple): return f"[{tname(t[0])};{t[1]}]"
    return t

# v0.4.1 (R4B-2): the bucket extremes (lo, hi, llo, lhi) replace the clamped (obs, lobs)
ring = [("", "ts","i64"),("", "p","PodU128"),("", "l","PodU128"),("", "x","PodU128"),("", "y","PodU128"),
        ("", "lo","PodU128"),("", "hi","PodU128"),("", "llo","PodU128"),("", "lhi","PodU128")]
layout("RingEntry", ring)

M = []
def g(grp, *fs):
    for f,t in fs: M.append((grp,f,t))
g("header", ("version","u16"),("flags","u16"),("bump","u8"),("pool_vault_bump","u8"),("market_vault_bump","u8"),("trader_bump","u8"),
  ("trader_wsol_bump","u8"),("venue","u8"),("tier","u8"),("decimals","u8"),("ring_head","u8"),("ring_count","u8"),
  ("ema_warm","u8"),("ema_guard","u8"),   # M4R: the post-change warm-up countdown and reference guard, from _pad0 (size, offsets unchanged)
  ("created_at","i64"))
g("identity", ("mint","Pubkey"),("token_program","Pubkey"),("creator","Pubkey"))
g("own accounts", ("pool_vault","Pubkey"),("market_vault","Pubkey"),("trader","Pubkey"),("trader_base_ata","Pubkey"),("trader_wsol","Pubkey"))
g("pump detection keys", ("bonding_curve","Pubkey"),("canonical_pool","Pubkey"))
g("curve venue cache", ("curve_cache",arr("Pubkey",8)))
g("pool venue cache (union by venue)", ("pool_cache",arr("Pubkey",10)))
g("pool selection", ("pool_seen_at","i64"),("pool_d","PodU128"),("fp_keys",arr("Pubkey",6)),("fp_d","PodU128"),("fp_t0","i64"),("fp_venue","u8"),("_pad1","u8"),
  ("_pad1a",arr("u8",2)),("_pad1b",arr("u8",4)))   # M4R: M4-SEC-5's fp_lambda_bps retired to padding (size, offsets unchanged)
g("venue timing", ("migration_seen_at","i64"),("graduated_at","i64"),("tier_computed_at","i64"))
g("route chains", ("no_route_since","i64"),("last_no_route_obs","i64"),("pool_fail_since","i64"),("last_pool_fail_obs","i64"))
g("lender pool (Market part)", ("tokens_free","u64"),("borrowed_q","u64"),("borrowed_q_by_tier",arr("u64",4)),("total_shares","PodU128"),("acc","PodI128"),
  ("carry","PodI128"),("acc_r","PodU128"),("carry_r","PodU128"),("dust","PodI128"),("receivable","u64"),("max_release","i64"),
  ("vest_bal","u64"),("vest_last","i64"),("vest_end","i64"),("share_epoch","u32"),("_pad2",arr("u8",4)))
g("rent", ("rent_basis",arr("PodU128",3)),("rent_aggr",arr("PodU128",3)),("last_accrual_ts","i64"))
g("observation", ("b_sec","i64"),("b_lo","PodU128"),("b_hi","PodU128"),("b_llo","PodU128"),("b_lhi","PodU128"),("lb_sec","i64"),("lb_lo","PodU128"),("lb_hi","PodU128"),
  ("ema_price","PodU128"),("ema_liq","PodU128"),("liq_slow","PodU128"),("ema_ts","i64"),("slow_ts","i64"),("ema_count","u64"),("ring",arr("RingEntry",8)))
g("market-level params", ("ema_params_version","u32"),("kink_version","u32"),("ema_clamp_bps","u32"),("util_kink_bps","u32"),("liq_slow_tau_s","u32"),("liq_slow_dtmax_s","u32"),
  ("ema_dtmax_s","u16"),("_pad3",arr("u8",6)))
g("OI", ("open_positions","u32"),("_pad4",arr("u8",4)),("total_notional_open","u64"),
  ("max_q_hwm","u64"),   # #99: high-water mark of q since open_positions last returned to 0, from _reserved (size unchanged)
  ("min_open_fee_ppm","u32"),("_pad5",arr("u8",4)))   # #100 (FIT-2): low-water mark of the open fee, from _reserved
g("derived keys", ("trader_wsol_ata","Pubkey"),   # M5-SEC-6: ATA(trader, WSOL) under SPL Token, derived once at listing, from _reserved
  ("pump_pool_authority","Pubkey"))                # M5-SEC-6: PDA(pump, ["pool-authority", mint]), the canonical pool's creator, likewise
g("misc", ("coverage_cleared_at","i64"),   # M7-SEC-3: the second set_coverage_pause(false) last cleared a set flag; opens wait COVERAGE_CLEAR_DELAY_S after it, from _reserved
  ("lender_credit_rem","PodU128"),         # AIR-03: the lender rent credit's remainder carried between accruals, from _reserved[0..16]
  ("min_open_hard_cap_bps","u16"),         # AIR-19: the lowest venue_fee_hard_cap_bps an open position locked (the S3 chain's cap), from _reserved[16..18]
  ("_pad6",arr("u8",6)),
  ("_reserved",arr("u8",144)))
rows, end = layout("Market", M, disc=8)

# v0.4.1 (R4A-2 / R4B-3): MarketVault becomes zero-copy and carries the lock expiry calendar
MV = [("","lender_sol","u64"),("","unmatched","u64"),("","protocol_fees","u64"),("","pi","PodU128"),("","lock_norm","PodU128"),
      ("","b_min","u64"),("","pi_epoch","u32"),("","bump","u8"),("","_pad0",arr("u8",3)),("","cal_day","i64"),
      ("","lock_cal",arr("PodU128",128)),("","_reserved",arr("u8",64))]
_, mv_end = layout("MarketVault", MV, disc=8)

# AIR-01: guard_len / guard_elf (the ELF fingerprints of pump-fees, AMM v4, CPMM and Jupiter) take the 64 reserved bytes
VG = [("","version","u16"),("","bump","u8"),("","_pad0","u8"),("","upgraded","u32"),("","guard_slot",arr("u64",6)),("","flagged_slot",arr("u64",6)),("","flagged_at",arr("i64",6)),
      ("","guard_len",arr("u32",4)),("","guard_elf",arr(arr("u8",12),4))]
_, vg_end = layout("VenueGuard", VG, disc=8)

def dump(name, rows, total):
    print(f"## {name}: {total} bytes incl. 8-byte discriminator")
    cur = None
    for grp, f, t, off, s in rows:
        print(f"{grp:32s} {f:24s} {tname(t):18s} @{off:5d} {s:4d}")
if __name__ == "__main__":
    print("RingEntry", STRUCTS["RingEntry"][1], "bytes")
    dump("Market", rows, end)
    # group summary
    from collections import OrderedDict
    gs = OrderedDict()
    for grp, f, t, off, s in rows:
        gs.setdefault(grp, [off, 0]); gs[grp][1] += s
    print("\n## Market groups")
    for k,(o,s) in gs.items(): print(f"{k:34s} @{o:5d} {s:5d} B")
    print("VenueGuard", vg_end)
    print("MarketVault", mv_end)
    # rent-exempt minimums: (len + 128) * 5080 (V5)
    for n, L in [("Market", end), ("VenueGuard", vg_end), ("MarketVault", mv_end)]:
        print(n, "rent", (L+128)*5080)
