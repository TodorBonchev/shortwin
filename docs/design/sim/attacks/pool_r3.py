"""Round-3 lender-pool findings, re-run on pool_model v0.4 against the frozen v0.3 model
(adapted from the reviewers' r3/xdoc/sim/r3_stranded.py, r3/R3B/sim/jit_lump.py, stuck_lock.py,
wipe_dust.py; R3A-10 and R3B-9 probes).  The functions live in ../pool_model.py."""
import os
import sys
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.join(HERE, ".."))
import pool_model as pm   # noqa: E402

if __name__ == "__main__":
    print("R3A-2 stranded SOL after an honest wind-down (40 seeds x 3,000 ops):")
    for label, (inflow, stranded, worst, above) in pm.stranded_table().items():
        print(f"  {label}: inflow {inflow/1e9:,.1f} SOL, stranded {stranded/1e9:.6f} SOL, worst seed {worst/1e9:.6f}, "
              f"seeds > 0.01 SOL: {above}")
    w, hs, tot = pm.lump_legacy_row()
    print(f"R3B-2 v0.3: whale 9x captures {w/1e9:.4f} SOL ({100*w/tot:.1f} % of lender income), honest {hs/1e9:.4f}")
    for label, h, w, hs, tot in pm.jit_hold_table():
        print(f"R3B-2 {label}, whale holds {h} h: whale {w/1e9:.4f} SOL ({100*w/tot:.1f} %), honest {hs/1e9:.4f}")
    for scen, label, cells in pm.stuck_lock_table():
        print(f"R3B-1 {scen} | {label} | 24 h / 7 d / 30 d / 32 d: {' / '.join(cells)}")
    print("R3B-2 post-wipe (v0.4):", pm.wipe_tail_demo())
    for row in pm.zero_share_demo():
        print("R3A-10", row)
    for row in pm.protocol_change_demo():
        print("R3B-9", row)
