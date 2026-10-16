#!/usr/bin/env python3
"""Mainnet vectors of pump's synthetic migration (pump-final §5, T-MATH-2; P1).

Since pump's 2026-10-08 redeploy a v3 buy past the curve's real token reserve `rT` completes the
curve and buys the rest from the would-be PumpSwap pool in one instruction. This extractor reads
eleven such completions observed on mainnet (the survey's sample, every one followed by its
`migrate_v2`) from raw `getTransaction` dumps and writes `synthetic.json` with the values pump
itself reported, so that `quotes_mainnet.rs` (`pump_synthetic_completions`) can check
`shortwin-math`'s `migrated_pool` (with `post_complete_quote_in`), `curve_synthetic`
(`B̂_v3`), `migrating_fv_price` and `migration_boost` against them:

* the completing buy (`buy_v3` or `buy_exact_quote_in_v3`, top level or inner): its arguments,
  the curve's base ATA before and after it (`pre/postTokenBalances`, owner = the curve);
* its `TradeEvent` (the curve part: `rT`, `n₁`, the fees, the post-trade curve) and its
  `PostCompleteBuyEvent` (the pool part: `pcB`, `pcQ`, the fees, the would-be pool before and
  after) — pump's `emit_cpi!` self-invocations, decoded at the IDL offsets (pump-public-docs'
  `pump.json`; `services/indexer/src/ingest/decode/venue.rs` decodes the same layouts);
* the migration: `CompletePumpAmmMigrationEvent` (`mint_amount`, `sol_amount`,
  `pool_migration_fee`, the pool), PumpSwap's `CreatePoolEvent` (the pool as created), and the
  WSOL balances after the transaction of the pool's quote vault (owner = the pool) and of its
  boost vault's ATA (owner = `PDA(pAMM, ["boost_vault", pool])`): `migrate_v2` moves the boost
  from the one to the other and books it as the pool's `virtual_quote_reserves`.

    python3 gen_synthetic.py --dumps <dir> [--fetch]

`--fetch` first stores every missing transaction of the sample in `<dir>` (read-only
`getTransaction` on mainnet RPC, `_solana.rpc`). Without `--dumps` it only prints a note and
exits 0, leaving `synthetic.json` as committed (CI's `vectors` job runs every generator without
arguments; the dumps are not committed, every case carries its signatures).
"""
import argparse
import json
import struct
import sys
import time
import urllib.error
from pathlib import Path

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
from _solana import (ANCHOR_EVENT_IX, PAMM, PUMP, WSOL, b58d, b58e, dump, find_pda,  # noqa: E402
                     instructions, load_tx, rpc)

HERE = Path(__file__).resolve().parent
S = str  # every integer is written as a decimal string (tests/vectors/README.md)

# (completing buy, its migrate_v2): the survey's 11 sampled synthetic completions
# (pump-final V5; `v3n_synth_completions.json` of the design stage).
SAMPLE = [
    ("2vVRr9egDUeiPnKtJDP4KHFjNCRcepseEpRnpvNSB5RFk8h6JjKcN72HvHUARiEPzRQSZkvfnZEBnwzZmFAyfa4j",
     "5hcXVV8qpXbEU5jpnNVnU3LSPvhcywyab5qtrAGLS9fd7Qb1o3JiLk4cjZ5RphvqrqG4zYtVZdSB4ETJaPwivfEb"),
    ("44mjNEZQJLQe9mGUuhCuXbRdgAP6AWP6rhQ1voAWD5JW4nkJyKy7HwvidFaL3RkNBCpeeyt7ALEPzEuRMe37k3cx",
     "3Jb5pHM3dGWjhRvR6TUbPx7jb6BBCnFsQF5j5FP1Mq5LtKd29acjdJWijq9RKEcw31wxLdKAvxQuabArQB4uHwft"),
    ("2xSy12ceiMbdEoTDuMZSBLqwQSWo73gtW6AdAGcEB7Uptt948f1eCPpyaSS1S9qsbTyErrghup2nntgrwAD1HHGi",
     "9QGWGfxqEP7rmg7XHX9GcwT48KS4CLQyQbhuqyuAZC4QH5m9GUVFLcvYTrfDaX9N5maXWxDEZTf8cwSLRZzWE3h"),
    ("3VJo7RJaLsAL6uwzuWdQ3JHwTVftvMxmBfMsfp8BfSaKexkM3cVrs7vHtwhuMEeRyShKRCGu9NoCeCMj5ieL6DcN",
     "48ALworj6tR5fiatsYgZYPT2REKxToFed9taZZQRcZnFxK1HEK3gDyWHMqLdbNdoWBhJmCosEoMaLPNU1LSAVcbW"),
    ("4r59WejW9sSkpEEEinG3Jb5ioMom1sUjS4Yq6syMrHBkZbGFVU3vHgASscpv9jHQJULSJa2iYJA9MwzdHuC5rSD7",
     "izqobHa8nzCmdkbfYPLxhRbnUgNxgats3ffKCVGPCcZwKCsz6NVLPMJ74VgrjsyVz6cMetrfVbk8SBxRHq9gRvK"),
    ("2j2ynAViBqBWbdzwnPMVyoLa98QBiMbvvuHudpTjaiKimnWWkMkqH1wmHsRWGdwzMN8dQk7eqypBEdeqqtaPG6QC",
     "3y1CMndMbY8q5y1uKZwF6DPQ3AjeRfYU3F58waQqLAFe5kx8vyFyPyh9EZQNV1cXc1oyAo9BtLBAWSbLVS32cKLn"),
    ("2Xebwuw7HeSFbAQrFNwnVLKwgy2unUM6Ayn8Kzz2swm5Gdaou3kQq3i8BZNA3tgPsH4hyCvB48vV72QhPN1cgmQN",
     "37KY99SDWkHYCYRiRirvzVwgApZPovmDYckBNZWiwR6LHWCthLvMNuK8vvu3SG1tNPFUEXE52RgwXSpxaDfad2ut"),
    ("2BDRzzxVLWA5SF2XthTv9j8j3jpjzxSKiMMxLgpx2T47Tr9ZtyLSWfPrqnPb9HTP3CcPkB3vBX6FNmy7Y5GWiZ7G",
     "5xUzXbampLZPWdhCXFeVxqUha8Ykdj19bUwXheLGQM2Vu2keKrCkS33X4D1wDHWDLwruubg7K9uCmb4eaVrvJepK"),
    ("28ZvRwwNmMw4mDvK4GTtX2PhAHQuvjzogN1mNXH2xtcWdrAntxZUTQcf1LVFFVCkaFhvrY65vxFrbpTq6kqY3S13",
     "29ZKgQNEntcwotj6WQCWBTmXpvT6M8o4HVPEkaeajeF4iZJNTWixvTLRQFNmRaTsEm8hXdx1KdZKkiZXSPVaLFeb"),
    ("5Q4NR6UgKJ4xzvd2ThEZmBgk9tjmAnxNqxa1MJS6TNxE5ao8xfnvnToHfPqQVW1xpkQiWpANrnQuUMjAs1i9gR7R",
     "45fvXKYpmjYvkndLNbiPL4RyhbrkTkrkd4oxdiiR5VJBfvAk39JgN1FtKVzSEhpdKVYXgsc5nnsdjscJ2f2XdVc4"),
    ("26dkNFB2kAS4GYdwDUwrZXHgHEQfw11oGEfxoERUCvWfvLmLxTV7JcSj4kbRqCMQCuYLpuZoCRqJNAsUtRD22xvG",
     "3rMbzcx1XWxYFXch1TEc5zDHc1p6wEMAsy8Cxxr1XRxLFLYfMHkSkrPYJQ9Ps2oGdLbnNbuK9KU7paQB9WThvM9S"),
]

TRADE_EVENT = bytes.fromhex("bddb7fd34ee661ee")
POST_COMPLETE_BUY_EVENT = bytes.fromhex("6fb06d8b316cd5fb")
COMPLETE_EVENT = bytes.fromhex("5f72619cd42e9808")
COMPLETE_MIGRATION_EVENT = bytes.fromhex("bde95db95c94ea94")
CREATE_POOL_EVENT = bytes.fromhex("b1310cd2a076a774")
BUY_V3 = bytes.fromhex("07051dc4f5176550")
BUY_EXACT_QUOTE_IN_V3 = bytes.fromhex("e1f7501ed5b38488")


def u64(b, o):
    return struct.unpack_from("<Q", b, o)[0]


def trade_event(b):
    """`TradeEvent` through `ix_name` (body after the 16 tag bytes)."""
    o = 0
    out = dict(mint=b58e(b[o:o + 32]))
    o += 32
    out["sol_amount"], out["token_amount"] = u64(b, o), u64(b, o + 8)
    o += 16
    out["is_buy"] = b[o] == 1
    o += 1 + 32 + 8
    (out["virtual_sol_reserves"], out["virtual_token_reserves"], out["real_sol_reserves"],
     out["real_token_reserves"]) = struct.unpack_from("<4Q", b, o)
    o += 32 + 32
    out["fee_basis_points"], out["fee"] = u64(b, o), u64(b, o + 8)
    o += 16 + 32
    out["creator_fee_basis_points"], out["creator_fee"] = u64(b, o), u64(b, o + 8)
    o += 16 + 1 + 32
    n = struct.unpack_from("<I", b, o)[0]
    out["ix_name"] = b[o + 4:o + 4 + n].decode()
    o += 4 + n + 1 + 16
    out["buyback_fee_basis_points"], out["buyback_fee"] = u64(b, o), u64(b, o + 8)
    return out


def post_complete_buy_event(b):
    """`PostCompleteBuyEvent` (224 bytes after the tag, pump-final §3.6)."""
    names = ["base_out", "quote_in", "fee_basis_points", "fee", "creator_fee_basis_points",
             "creator_fee", "buyback_fee", "pool_base_reserves_before", "pool_quote_reserves_before",
             "pool_base_reserves_after", "pool_quote_reserves_after"]
    out = dict(user=b58e(b[0:32]), mint=b58e(b[32:64]), bonding_curve=b58e(b[64:96]),
               quote_mint=b58e(b[96:128]))
    out.update(zip(names, struct.unpack_from("<11Q", b, 136)))
    return out


def events(t, program):
    """`(tag, body)` of every `emit_cpi!` event of `program`, in execution order."""
    for prog, data, _ in instructions(t):
        if prog == program and data[:8] == ANCHOR_EVENT_IX:
            yield data[8:16], data[16:]


def curve_ata(t, curve, mint):
    """The curve's base ATA before and after the transaction (token balances owned by it)."""
    pre = post = None
    for side, key in (("preTokenBalances", "pre"), ("postTokenBalances", "post")):
        for tb in t["meta"].get(side) or []:
            if tb.get("owner") == curve and tb.get("mint") == mint:
                v = int(tb["uiTokenAmount"]["amount"])
                if key == "pre":
                    pre = v
                else:
                    post = v
    return pre, post


def case(t, m):
    sig = t["transaction"]["signatures"][0]
    ev = [(tag, body) for tag, body in events(t, PUMP)]
    trades = [trade_event(b) for tag, b in ev if tag == TRADE_EVENT]
    posts = [post_complete_buy_event(b) for tag, b in ev if tag == POST_COMPLETE_BUY_EVENT]
    assert len(trades) == 1 and len(posts) == 1, sig
    assert [tag for tag, _ in ev if tag in (TRADE_EVENT, COMPLETE_EVENT, POST_COMPLETE_BUY_EVENT)] == [
        TRADE_EVENT, COMPLETE_EVENT, POST_COMPLETE_BUY_EVENT], sig
    tr, pc = trades[0], posts[0]
    buys = [(data[:8], data) for prog, data, _ in instructions(t)
            if prog == PUMP and data[:8] in (BUY_V3, BUY_EXACT_QUOTE_IN_V3)]
    assert len(buys) == 1, sig
    disc, data = buys[0]
    a_pre, a_post = curve_ata(t, pc["bonding_curve"], pc["mint"])
    assert a_pre is not None and a_post is not None, sig
    mig_sig = m["transaction"]["signatures"][0]
    mev = [(tag, body) for tag, body in events(m, PUMP) if tag == COMPLETE_MIGRATION_EVENT]
    assert len(mev) == 1, mig_sig
    mb = mev[0][1]
    assert b58e(mb[32:64]) == pc["mint"], mig_sig
    cp = [body for tag, body in events(m, PAMM) if tag == CREATE_POOL_EVENT]
    assert len(cp) == 1, mig_sig
    # CreatePoolEvent: timestamp i64, index u16, creator, base_mint, quote_mint, 2 decimals, then
    # base_amount_in, quote_amount_in, pool_base_amount, pool_quote_amount.
    o = 8 + 2 + 32 * 3 + 2
    base_in, quote_in, pool_base, pool_quote = struct.unpack_from("<4Q", cp[0], o)
    pool = b58e(mb[128:160])
    boost_vault = find_pda([b"boost_vault", b58d(pool)], PAMM)
    wsol_after = {tb.get("owner"): int(tb["uiTokenAmount"]["amount"])
                  for tb in m["meta"].get("postTokenBalances") or [] if tb.get("mint") == WSOL}
    assert pool in wsol_after and boost_vault in wsol_after, mig_sig
    return dict(
        sig=sig, mig_sig=mig_sig, mint=pc["mint"], curve=pc["bonding_curve"],
        instruction="buy_v3" if disc == BUY_V3 else "buy_exact_quote_in_v3",
        arg0=S(u64(data, 8)), arg1=S(u64(data, 16)),
        curve_base_ata_before=S(a_pre), curve_base_ata_after=S(a_post),
        curve_part=dict(ix_name=tr["ix_name"], sol_amount=S(tr["sol_amount"]), token_amount=S(tr["token_amount"]),
                   vt_after=S(tr["virtual_token_reserves"]), vq_after=S(tr["virtual_sol_reserves"]),
                   rt_after=S(tr["real_token_reserves"]), rq_after=S(tr["real_sol_reserves"]),
                   fee_bps=S(tr["fee_basis_points"]), fee=S(tr["fee"]),
                   creator_fee_bps=S(tr["creator_fee_basis_points"]), creator_fee=S(tr["creator_fee"]),
                   buyback_fee=S(tr["buyback_fee"])),
        pool_part={k: S(v) if isinstance(v, int) else v for k, v in pc.items()
                   if k not in ("user", "mint", "bonding_curve")},
        migration=dict(mint_amount=S(u64(mb, 64)), sol_amount=S(u64(mb, 72)),
                       pool_migration_fee=S(u64(mb, 80)), pool_base_amount=S(pool_base),
                       pool_quote_amount=S(pool_quote), base_amount_in=S(base_in),
                       quote_amount_in=S(quote_in), pool=pool,
                       pool_quote_vault_after=S(wsol_after[pool]),
                       boost_vault_ata_after=S(wsol_after[boost_vault])),
    )


def load(dumps, sig):
    t = load_tx(dumps / f"{sig}.json")
    assert t is not None, sig
    return t


def fetch(dumps, sigs):
    for sig in sigs:
        if (dumps / f"{sig}.json").exists():
            continue
        for attempt in range(6):
            try:
                # Some of the sample are v1 transactions (refused under version 0).
                r = rpc("getTransaction", [sig, {"encoding": "json", "maxSupportedTransactionVersion": 1,
                                                 "commitment": "finalized"}])
                break
            except urllib.error.HTTPError as e:  # 429: the public endpoint's rate limit
                if e.code != 429 or attempt == 5:
                    raise
                time.sleep(5 * (attempt + 1))
        assert r is not None, sig
        (dumps / f"{sig}.json").write_text(json.dumps(r))
        time.sleep(2)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dumps", type=Path)
    ap.add_argument("--fetch", action="store_true")
    a = ap.parse_args()
    if a.dumps is None:
        print("gen_synthetic.py: no --dumps given; synthetic.json left unchanged "
              "(extract with --dumps <dir> [--fetch])")
        return
    a.dumps.mkdir(parents=True, exist_ok=True)
    if a.fetch:
        fetch(a.dumps, [s for pair in SAMPLE for s in pair])
    cases = [case(load(a.dumps, c), load(a.dumps, m)) for c, m in SAMPLE]
    out = dict(
        generator="gen_synthetic.py",
        source="mainnet getTransaction dumps of pump synthetic completions and their migrate_v2 "
               "(pump-final V5; pump's 2026-10-08 build)",
        synthetic_completions=cases,
    )
    dump(out, HERE / "synthetic.json")
    print("synthetic_completions", len(cases))


if __name__ == "__main__":
    main()
